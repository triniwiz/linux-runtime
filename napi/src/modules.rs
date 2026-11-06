//! CommonJS, JSON and `.node` addons on the shared Node style resolver.

use crate::js::*;
use crate::state::{state, State};
use crate::sys::*;
use nativescript_core::resolve::{dir_of, format_of, read_source, resolve, Format, Kind};
use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

/// `Err(None)`: a JS exception is pending.
pub type Load<T> = Result<T, Option<String>>;

pub type AddonInit = unsafe extern "C" fn(napi_env, napi_value) -> napi_value;

thread_local! {
    /// set by napi_module_register while an addon is dlopen()ed
    pub static PENDING_MODULE: std::cell::Cell<Option<AddonInit>> = const { std::cell::Cell::new(None) };
}

fn exports_of(js: Js, module: napi_value) -> Load<napi_value> {
    js.get(module, "exports").map_err(|_| None)
}

/// `module.exports` of the CommonJS module at `path`, loading it once.
pub fn require_cjs(st: &State, path: &Path) -> Load<napi_value> {
    let js = st.js();
    if let Some(r) = st.modules.borrow().get(path) {
        return exports_of(js, js.deref(*r));
    }
    let module = js.object();
    let exports = js.object();
    let filename = js.string(&path.to_string_lossy());
    let dirname = js.string(&dir_of(path).to_string_lossy());
    js.set(module, "exports", exports);
    js.set(module, "id", filename);
    js.set(module, "filename", filename);
    js.set(module, "path", dirname);
    // cached before running so cycles see the partial exports
    st.modules.borrow_mut().insert(path.to_path_buf(), js.reference(module, 1));

    let outcome = (|| -> Load<()> {
        let source = read_source(path)?;
        let wrapped = format!("(function (exports, require, module, __filename, __dirname) {{{source}\n}})");
        let function = crate::run_script(js, &wrapped, &path.to_string_lossy()).map_err(|_| None)?;
        let require = require_function(js, &dir_of(path));
        js.call(function, exports, &[exports, require, module, filename, dirname]).map_err(|_| None)?;
        Ok(())
    })();
    if outcome.is_err() {
        if let Some(r) = st.modules.borrow_mut().remove(path) {
            js.delete_ref(r);
        }
    }
    outcome?;
    js.set(module, "loaded", js.boolean(true));
    exports_of(js, module)
}

fn parse_json(js: Js, text: &str) -> Load<napi_value> {
    let json = js.get(js.global(), "JSON").map_err(|_| None)?;
    let parse = js.get(json, "parse").map_err(|_| None)?;
    js.call(parse, json, &[js.string(text)]).map_err(|_| None)
}

/// Loads a `.node` addon into this env and returns its exports.
pub fn load_addon(st: &State, path: &Path) -> Load<napi_value> {
    let js = st.js();
    if let Some(r) = st.modules.borrow().get(path) {
        return Ok(js.deref(*r));
    }
    let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "invalid path".to_string())?;
    PENDING_MODULE.with(|m| m.take());
    let handle = unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(Some(format!("{}: {}", path.display(), nativescript_core::dlerror())));
    }
    let init = unsafe { libc::dlsym(handle, c"napi_register_module_v1".as_ptr()) };
    let init: AddonInit = if init.is_null() {
        PENDING_MODULE
            .with(|m| m.take())
            .ok_or_else(|| format!("{}: not a Node-API module", path.display()))?
    } else {
        unsafe { std::mem::transmute::<*mut c_void, AddonInit>(init) }
    };
    *st.module_file.borrow_mut() = CString::new(nativescript_core::resolve::path_to_file_url(path)).unwrap_or_default();
    let exports = js.object();
    let result = unsafe { init(js.0, exports) };
    if js.is_exception_pending() {
        return Err(None);
    }
    let exports = if result.is_null() { exports } else { result };
    st.modules.borrow_mut().insert(path.to_path_buf(), js.reference(exports, 1));
    Ok(exports)
}

fn require_any(st: &State, path: &Path) -> Load<napi_value> {
    match format_of(path) {
        Format::CommonJs => require_cjs(st, path),
        Format::Json => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            parse_json(st.js(), &text)
        }
        Format::Addon => load_addon(st, path),
        Format::Esm => Err(Some(format!("{}: ES modules aren't supported on this engine yet", path.display()))),
    }
}

unsafe fn base_and_spec(env: napi_env, info: napi_callback_info) -> (PathBuf, String) {
    let (a, _, _, data) = cb_args::<1>(env, info);
    let base = &*(data as *const PathBuf);
    (base.clone(), Js(env).to_string(a[0]))
}

unsafe extern "C" fn require_callback(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (base, spec) = base_and_spec(env, info);
    match resolve(&spec, &base, Kind::Require).map_err(Some).and_then(|p| require_any(st, &p)) {
        Ok(v) => v,
        Err(Some(e)) => {
            st.js().throw(&e);
            null_mut()
        }
        Err(None) => null_mut(),
    }
}

unsafe extern "C" fn require_resolve(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (base, spec) = base_and_spec(env, info);
    match resolve(&spec, &base, Kind::Require) {
        Ok(p) => js.string(&p.to_string_lossy()),
        Err(e) => {
            js.throw(&e);
            null_mut()
        }
    }
}

unsafe extern "C" fn drop_base(_: napi_env, data: *mut c_void, _: *mut c_void) {
    drop(Box::from_raw(data as *mut PathBuf));
}

/// `require` resolving relative to `dir`.
pub fn require_function(js: Js, dir: &Path) -> napi_value {
    let base = Box::into_raw(Box::new(dir.to_path_buf())) as *mut c_void;
    let require = js.function("require", Some(require_callback), base);
    js.set(require, "resolve", js.function("resolve", Some(require_resolve), base));
    unsafe { napi_add_finalizer(js.0, require, base, Some(drop_base), null_mut(), null_mut()) };
    require
}

/// Runs the entry file as a CommonJS module.
pub fn run_entry(st: &State, path: &Path) -> Load<()> {
    match format_of(path) {
        Format::CommonJs | Format::Esm => require_any(st, path).map(|_| ()),
        _ => Err(Some(format!("{} isn't a script", path.display()))),
    }
}
