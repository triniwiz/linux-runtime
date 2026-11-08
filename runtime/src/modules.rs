//! ES modules and CommonJS.

use crate::state::{state, State};
use crate::throw_error;
use nativescript_core::resolve::*;
use std::collections::HashMap;

fn source<'s>(scope: &mut v8::PinScope<'s, '_>, path: &Path) -> Load<v8::Local<'s, v8::String>> {
    if is_url(path) {
        let text = crate::net::fetch_text(&path.to_string_lossy()).map(strip_shebang)?;
        return v8::String::new(scope, &text).ok_or(Some("module too large".to_string()));
    }
    crate::app_bundle::source_string(scope, path).map_err(Some)
}
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct ModuleMap {
    esm: HashMap<PathBuf, v8::Global<v8::Module>>,
    /// module identity hash -> path
    paths: HashMap<i32, PathBuf>,
    resolutions: HashMap<(i32, String), PathBuf>,
    synthetic_exports: HashMap<i32, Vec<(String, v8::Global<v8::Value>)>>,
    /// CommonJS `module` objects
    cjs: HashMap<PathBuf, v8::Global<v8::Object>>,
    /// `ns:module` (dev host API for tooling)
    builtin: Option<v8::Global<v8::Object>>,
}

fn str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

/// `Err(None)` means a JS exception is pending.
type Load<T> = Result<T, Option<String>>;

fn origin<'s>(scope: &mut v8::PinScope<'s, '_>, path: &Path, module: bool) -> v8::ScriptOrigin<'s> {
    let name = str(scope, &path.to_string_lossy());
    v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        0,
        None,
        false,
        false,
        module,
        None,
    )
}

/// Loads (but doesn't evaluate) the module graph rooted at `path`.
pub fn load<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Load<v8::Local<'s, v8::Module>> {
    if !is_volatile(path) {
        if let Some(m) = st.modules.borrow().esm.get(path) {
            return Ok(v8::Local::new(scope, m));
        }
    }
    let module = if is_builtin(path) {
        let exports = builtin(scope, st, path)?;
        let mut pairs = vec![("default".to_owned(), exports.into())];
        if let Some(keys) = exports.get_own_property_names(scope, Default::default()) {
            for i in 0..keys.length() {
                let k = keys.get_index(scope, i).unwrap();
                let v = exports.get(scope, k).unwrap();
                pairs.push((k.to_rust_string_lossy(scope), v));
            }
        }
        synthetic(scope, st, path, pairs)
    } else {
        match format_of(path) {
            Format::Esm => {
                let source = source(scope, path)?;
                let origin = origin(scope, path, true);
                let mut source = v8::script_compiler::Source::new(source, Some(&origin));
                v8::script_compiler::compile_module(scope, &mut source).ok_or(None)?
            }
            Format::Json => {
                let text = source(scope, path)?;
                let value = v8::json::parse(scope, text).ok_or(None)?;
                synthetic(scope, st, path, vec![("default".into(), value)])
            }
            Format::CommonJs => {
                let exports = require_cjs(scope, st, path)?;
                let mut pairs = vec![("default".to_owned(), exports)];
                if let Some(obj) = exports.to_object(scope).filter(|_| exports.is_object()) {
                    if let Some(keys) = obj.get_own_property_names(scope, Default::default()) {
                        for i in 0..keys.length() {
                            let k = keys.get_index(scope, i).unwrap();
                            let name = k.to_rust_string_lossy(scope);
                            if name != "default" {
                                let v = obj.get(scope, k).unwrap();
                                pairs.push((name, v));
                            }
                        }
                    }
                }
                synthetic(scope, st, path, pairs)
            }
            Format::Addon => return Err(Some("native addons can't be imported yet".into())),
        }
    };
    let hash = module.get_identity_hash().get();
    {
        let mut map = st.modules.borrow_mut();
        map.esm
            .insert(path.to_path_buf(), v8::Global::new(scope, module));
        map.paths.insert(hash, path.to_path_buf());
    }
    let requests = module.get_module_requests();
    let base = dir_of(path);
    for i in 0..requests.length() {
        let request = requests.get(scope, i).unwrap();
        let request = v8::Local::<v8::ModuleRequest>::try_from(request)
            .map_err(|_| Some("bad module request".to_string()))?;
        let spec = request.get_specifier().to_rust_string_lossy(scope);
        let resolved = resolve(&spec, &base, Kind::Import)?;
        load(scope, st, &resolved)?;
        st.modules
            .borrow_mut()
            .resolutions
            .insert((hash, spec), resolved);
    }
    Ok(module)
}

fn synthetic<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
    exports: Vec<(String, v8::Local<'s, v8::Value>)>,
) -> v8::Local<'s, v8::Module> {
    let names: Vec<_> = exports.iter().map(|(n, _)| str(scope, n)).collect();
    let name = str(scope, &path.to_string_lossy());
    let module = v8::Module::create_synthetic_module(scope, name, &names, synthetic_steps);
    let values = exports
        .into_iter()
        .map(|(n, v)| (n, v8::Global::new(scope, v)))
        .collect();
    st.modules
        .borrow_mut()
        .synthetic_exports
        .insert(module.get_identity_hash().get(), values);
    module
}

fn synthetic_steps<'s>(
    context: v8::Local<'s, v8::Context>,
    module: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Value>> {
    v8::callback_scope!(unsafe scope, context);
    let st = state(scope);
    let exports = st
        .modules
        .borrow_mut()
        .synthetic_exports
        .remove(&module.get_identity_hash().get())?;
    for (name, value) in exports {
        let name = str(scope, &name);
        let value = v8::Local::new(scope, &value);
        module.set_synthetic_module_export(scope, name, value)?;
    }
    Some(v8::undefined(scope).into())
}

fn resolve_callback<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let st = state(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let key = (referrer.get_identity_hash().get(), spec);
    let map = st.modules.borrow();
    let found = map
        .resolutions
        .get(&key)
        .and_then(|p| map.esm.get(p))
        .map(|m| v8::Local::new(scope, m));
    if found.is_none() {
        drop(map);
        throw_error(scope, &format!("Cannot resolve '{}'", key.1));
    }
    found
}

/// Loads, links and evaluates; returns the evaluation promise.
pub fn evaluate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Load<(v8::Local<'s, v8::Module>, v8::Local<'s, v8::Value>)> {
    let module = load(scope, st, path)?;
    if module.get_status() == v8::ModuleStatus::Uninstantiated {
        module
            .instantiate_module(scope, resolve_callback)
            .ok_or(None)?;
    }
    let result = module.evaluate(scope).ok_or(None)?;
    Ok((module, result))
}

/// Runs `path` as the main ES module.
pub fn run_main<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, path: &Path) -> Load<()> {
    let (_, result) = evaluate(scope, st, path)?;
    if let Ok(promise) = v8::Local::<v8::Promise>::try_from(result) {
        if promise.state() == v8::PromiseState::Rejected {
            promise.mark_as_handled();
            let e = promise.result(scope);
            scope.throw_exception(e);
            return Err(None);
        }
    }
    Ok(())
}

pub fn dynamic_import<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    resource: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let st = state(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let referrer = resource.to_rust_string_lossy(scope);
    let referrer = if referrer.starts_with("http") {
        PathBuf::from(referrer)
    } else {
        file_url_to_path(&referrer)
    };
    let base = if referrer.is_absolute() || is_url(&referrer) {
        dir_of(&referrer)
    } else {
        PathBuf::new()
    };

    v8::tc_scope!(let tc, scope);
    let outcome = resolve(&spec, &base, Kind::Import)
        .map_err(Some)
        .and_then(|path| evaluate(tc, &st, &path));
    match outcome {
        Ok((module, result)) => {
            let namespace = module.get_module_namespace();
            match v8::Local::<v8::Promise>::try_from(result) {
                Ok(p) if p.state() == v8::PromiseState::Rejected => {
                    // the import() promise carries the rejection
                    p.mark_as_handled();
                    let e = p.result(tc);
                    resolver.reject(tc, e);
                }
                Ok(p) if p.state() == v8::PromiseState::Pending => {
                    // top-level await: resolve with the namespace once evaluated
                    let then = v8::Function::builder(
                        |_: &mut v8::PinScope,
                         args: v8::FunctionCallbackArguments,
                         mut rv: v8::ReturnValue| rv.set(args.data()),
                    )
                    .data(namespace)
                    .build(tc)?;
                    let chained = p.then(tc, then)?;
                    resolver.resolve(tc, chained.into());
                }
                _ => {
                    resolver.resolve(tc, namespace);
                }
            }
        }
        Err(message) => {
            let error = match (tc.exception(), message) {
                (Some(e), _) => e,
                (None, Some(m)) => {
                    let m = str(tc, &m);
                    v8::Exception::error(tc, m)
                }
                (None, None) => v8::undefined(tc).into(),
            };
            tc.reset();
            resolver.reject(tc, error);
        }
    }
    Some(promise)
}

pub unsafe extern "C" fn import_meta(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    v8::callback_scope!(unsafe scope, context);
    let st = state(scope);
    let Some(path) = st
        .modules
        .borrow()
        .paths
        .get(&module.get_identity_hash().get())
        .cloned()
    else {
        return;
    };
    let dir = dir_of(&path);
    let url = if is_url(&path) {
        path.to_string_lossy().into_owned()
    } else {
        path_to_file_url(&path)
    };
    for (k, v) in [
        ("url", url),
        ("filename", path.to_string_lossy().into_owned()),
        ("dirname", dir.to_string_lossy().into_owned()),
    ] {
        let k = str(scope, k);
        let v = str(scope, &v);
        meta.set(scope, k.into(), v.into());
    }
    let data = str(scope, &dir.to_string_lossy());
    if let Some(f) = v8::Function::builder(meta_resolve)
        .data(data.into())
        .build(scope)
    {
        let k = str(scope, "resolve");
        meta.set(scope, k.into(), f.into());
    }
}

fn meta_resolve<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let base = PathBuf::from(args.data().to_rust_string_lossy(scope));
    let spec = args.get(0).to_rust_string_lossy(scope);
    match resolve(&spec, &base, Kind::Import) {
        Ok(p) => rv.set(str(scope, &path_to_file_url(&p)).into()),
        Err(e) => throw_error(scope, &e),
    }
}

/// `module.exports` of the CommonJS module at `path`, loading it once.
pub fn require_cjs<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Load<v8::Local<'s, v8::Value>> {
    let key = str(scope, "exports");
    if let Some(module) = st.modules.borrow().cjs.get(path) {
        let module = v8::Local::new(scope, module);
        return Ok(module.get(scope, key.into()).unwrap());
    }
    let module = v8::Object::new(scope);
    let exports = v8::Object::new(scope);
    module.set(scope, key.into(), exports.into());
    let filename = str(scope, &path.to_string_lossy());
    let dirname = str(scope, &dir_of(path).to_string_lossy());
    for (k, v) in [("id", filename), ("filename", filename), ("path", dirname)] {
        let k = str(scope, k);
        module.set(scope, k.into(), v.into());
    }
    // cached before running so cycles see the partial exports
    st.modules
        .borrow_mut()
        .cjs
        .insert(path.to_path_buf(), v8::Global::new(scope, module));

    let outcome = (|| -> Load<()> {
        let source = source(scope, path)?;
        let origin = origin(scope, path, false);
        let mut source = v8::script_compiler::Source::new(source, Some(&origin));
        let params =
            ["exports", "require", "module", "__filename", "__dirname"].map(|p| str(scope, p));
        let function = v8::script_compiler::compile_function(
            scope,
            &mut source,
            &params,
            &[],
            v8::script_compiler::CompileOptions::NoCompileOptions,
            v8::script_compiler::NoCacheReason::NoReason,
        )
        .ok_or(None)?;
        let require = require_function(scope, &dir_of(path)).ok_or(None)?;
        function
            .call(
                scope,
                exports.into(),
                &[
                    exports.into(),
                    require.into(),
                    module.into(),
                    filename.into(),
                    dirname.into(),
                ],
            )
            .ok_or(None)?;
        Ok(())
    })();
    if outcome.is_err() {
        st.modules.borrow_mut().cjs.remove(path);
    }
    outcome?;
    let k = str(scope, "loaded");
    let t = v8::Boolean::new(scope, true);
    module.set(scope, k.into(), t.into());
    Ok(module.get(scope, key.into()).unwrap())
}

/// Builtin module object (`ns:module`).
fn builtin<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Load<v8::Local<'s, v8::Object>> {
    let map = st.modules.borrow();
    match (path.to_string_lossy().as_ref(), map.builtin.as_ref()) {
        ("ns:module", Some(o)) => Ok(v8::Local::new(scope, o)),
        (name, _) => Err(Some(format!("Cannot find module '{name}'"))),
    }
}

fn require_any<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Load<v8::Local<'s, v8::Value>> {
    if is_builtin(path) {
        return builtin(scope, st, path).map(Into::into);
    }
    match format_of(path) {
        Format::CommonJs => require_cjs(scope, st, path),
        Format::Json => {
            let text = source(scope, path)?;
            v8::json::parse(scope, text).ok_or(None)
        }
        // require(esm) works for modules without top-level await
        Format::Esm => {
            let (module, result) = evaluate(scope, st, path)?;
            if let Ok(p) = v8::Local::<v8::Promise>::try_from(result) {
                match p.state() {
                    v8::PromiseState::Rejected => {
                        p.mark_as_handled();
                        let e = p.result(scope);
                        scope.throw_exception(e);
                        return Err(None);
                    }
                    v8::PromiseState::Pending => {
                        return Err(Some(format!(
                            "require() of {} which uses top-level await, use import()",
                            path.display()
                        )))
                    }
                    _ => {}
                }
            }
            Ok(module.get_module_namespace())
        }
        Format::Addon => crate::napi::load_addon(scope, st, path),
    }
}

fn require_callback<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let st = state(scope);
    let base = PathBuf::from(args.data().to_rust_string_lossy(scope));
    let spec = args.get(0).to_rust_string_lossy(scope);
    let outcome = resolve(&spec, &base, Kind::Require)
        .map_err(Some)
        .and_then(|p| require_any(scope, &st, &p));
    match outcome {
        Ok(v) => rv.set(v),
        Err(Some(e)) => throw_error(scope, &e),
        Err(None) => {}
    }
}

fn require_resolve<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let base = PathBuf::from(args.data().to_rust_string_lossy(scope));
    let spec = args.get(0).to_rust_string_lossy(scope);
    match resolve(&spec, &base, Kind::Require) {
        Ok(p) => rv.set(str(scope, &p.to_string_lossy()).into()),
        Err(e) => throw_error(scope, &e),
    }
}

/// `require` resolving relative to `dir`.
pub fn require_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    dir: &Path,
) -> Option<v8::Local<'s, v8::Function>> {
    let data = str(scope, &dir.to_string_lossy());
    let require = v8::Function::builder(require_callback)
        .data(data.into())
        .build(scope)?;
    let resolve = v8::Function::builder(require_resolve)
        .data(data.into())
        .build(scope)?;
    let key = str(scope, "resolve");
    require.set(scope, key.into(), resolve.into());
    Some(require)
}

/// Runs the entry file as an ES module or a CommonJS module.
pub fn run_entry<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, path: &Path) -> Load<()> {
    match format_of(path) {
        Format::Esm => run_main(scope, st, path),
        Format::CommonJs => require_cjs(scope, st, path).map(|_| ()),
        Format::Json | Format::Addon => Err(Some(format!("{} isn't a script", path.display()))),
    }
}

/// Drops `path` from the module caches so the next import/require reloads it.
pub fn invalidate(st: &State, path: &Path) {
    let mut map = st.modules.borrow_mut();
    if let Some(m) = map.esm.remove(path) {
        map.paths.retain(|_, p| p != path);
        drop(m);
    }
    map.cjs.remove(path);
    map.resolutions.retain(|_, p| p != path);
}

pub fn clear(st: &State) {
    let mut map = st.modules.borrow_mut();
    map.esm.clear();
    map.paths.clear();
    map.resolutions.clear();
    map.cjs.clear();
}

/// Files of every loaded module (for watchers).
pub fn loaded_files(st: &State) -> Vec<PathBuf> {
    let map = st.modules.borrow();
    map.esm
        .keys()
        .chain(map.cjs.keys())
        .filter(|p| !is_url(p))
        .cloned()
        .collect()
}

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

/// hmrResolve(specifier, parentPath?) -> absolute path
fn hmr_resolve<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let spec = args.get(0).to_rust_string_lossy(scope);
    let parent = if args.get(1).is_string() {
        args.get(1).to_rust_string_lossy(scope)
    } else {
        String::new()
    };
    let base = if parent.is_empty() {
        PathBuf::new()
    } else {
        dir_of(Path::new(&parent))
    };
    match resolve(&spec, &base, Kind::Import) {
        Ok(p) => rv.set(str(scope, &p.to_string_lossy()).into()),
        Err(e) => throw_error(scope, &e),
    }
}

fn hmr_invalidate<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let path = args.get(0).to_rust_string_lossy(scope);
    invalidate(&st, Path::new(&path));
}

fn hmr_clear<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, _rv: Rv<'s>) {
    clear(&state(scope));
}

/// hmrLoad(path) -> exports / namespace, (re)loading after invalidation
fn hmr_load<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let path = PathBuf::from(args.get(0).to_rust_string_lossy(scope));
    match require_any(scope, &st, &path) {
        Ok(v) => rv.set(v),
        Err(Some(e)) => throw_error(scope, &e),
        Err(None) => {}
    }
}

fn read_text<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let path = args.get(0).to_rust_string_lossy(scope);
    match nativescript_core::resolve::read_text(Path::new(&path)) {
        Ok(text) => rv.set(str(scope, &text).into()),
        Err(e) => throw_error(scope, &e),
    }
}

/// copyFile(source, destination), creating the destination directory
fn copy_file<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let src = args.get(0).to_rust_string_lossy(scope);
    let dst = PathBuf::from(args.get(1).to_rust_string_lossy(scope));
    let result = dst
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .map_err(|e| e.to_string())
        .and_then(|_| read_bytes(Path::new(&src)))
        .and_then(|bytes| std::fs::write(&dst, &*bytes).map_err(|e| e.to_string()));
    if let Err(e) = result {
        throw_error(scope, &format!("copy {src} -> {}: {e}", dst.display()));
    }
}

// ns:module: dev host API (see @nativescript/vite browser-runtime-contract.ts)

/// Path or registry key of a module URL (`file:` URLs become paths).
fn module_key(url: &str) -> PathBuf {
    if url.starts_with("file:") {
        file_url_to_path(url)
    } else {
        canonical(PathBuf::from(url))
    }
}

fn dev_configure<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let json = args.get(0).to_rust_string_lossy(scope);
    match serde_json::from_str(&json) {
        Ok(config) => configure_loader(&config),
        Err(e) => throw_error(scope, &format!("configureLoader: {e}")),
    }
}

fn dev_invalidate<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let Ok(urls) = v8::Local::<v8::Array>::try_from(args.get(0)) else {
        return;
    };
    for i in 0..urls.length() {
        let url = urls
            .get_index(scope, i)
            .unwrap()
            .to_rust_string_lossy(scope);
        invalidate(&st, &module_key(&url));
    }
}

fn dev_loaded<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let urls: Vec<String> = {
        let map = st.modules.borrow();
        map.esm
            .keys()
            .chain(map.cjs.keys())
            .filter(|p| !is_builtin(p))
            .map(|p| {
                if is_url(p) {
                    p.to_string_lossy().into_owned()
                } else {
                    path_to_file_url(p)
                }
            })
            .collect()
    };
    let items: Vec<v8::Local<v8::Value>> = urls.iter().map(|u| str(scope, u).into()).collect();
    rv.set(v8::Array::new_with_elements(scope, &items).into());
}

/// createRequire(fileOrUrl): `require` relative to that module
fn dev_create_require<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let file = module_key(&args.get(0).to_rust_string_lossy(scope));
    if let Some(f) = require_function(scope, &dir_of(&file)) {
        rv.set(f.into());
    }
}

fn dev_set_boot_complete<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    crate::net::set_dev_boot_complete(args.get(0).boolean_value(scope));
}

fn dev_set_builtin<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    if let Ok(o) = v8::Local::<v8::Object>::try_from(args.get(0)) {
        state(scope).modules.borrow_mut().builtin = Some(v8::Global::new(scope, o));
    }
}

pub fn install_hmr<'s>(scope: &mut v8::PinScope<'s, '_>, natives: v8::Local<'s, v8::Object>) {
    macro_rules! set {
        ($name:literal, $f:expr) => {{
            let f = v8::Function::new(scope, $f).unwrap();
            let k = str(scope, $name);
            natives.set(scope, k.into(), f.into());
        }};
    }
    set!("hmrResolve", hmr_resolve);
    set!("hmrInvalidate", hmr_invalidate);
    set!("hmrClear", hmr_clear);
    set!("hmrLoad", hmr_load);
    set!("readText", read_text);
    set!("copyFile", copy_file);
    set!("devConfigure", dev_configure);
    set!("devInvalidate", dev_invalidate);
    set!("devLoaded", dev_loaded);
    set!("devCreateRequire", dev_create_require);
    set!("devSetBuiltin", dev_set_builtin);
    set!("devSetBootComplete", dev_set_boot_complete);
}
