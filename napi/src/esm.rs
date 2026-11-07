//! ES modules for engines whose Node-API shim exposes no module loader (all of
//! them). As in the Windows runtime, each module is transformed with oxc's
//! `ModuleRunnerTransform` (Vite's ssrTransform) into an async function whose
//! imports, exports, `import.meta` and `import()` go through a JS module runner
//! (`RUNNER`) keeping the registry. Exports are getters hoisted above the
//! imports, so live bindings hold through cycles; top-level await works since
//! every body is async.
//!
//! Resolution is the shared Node style resolver. Not supported: HTTP modules,
//! `ns:` builtins, `import()` from CommonJS.

use crate::js::*;
use crate::modules::{load_addon, require_cjs, Load};
use crate::state::{state, State};
use crate::sys::*;
use nativescript_core::resolve::{
    dir_of, format_of, path_to_file_url, read_source, resolve, Format, Kind,
};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

/// The runner's module body parameters, in the order `RUNNER` passes them.
const WRAPPER_HEAD: &str = "(async function (__vite_ssr_exports__, __vite_ssr_import_meta__, \
__vite_ssr_import__, __vite_ssr_dynamic_import__, __vite_ssr_exportAll__, __vite_ssr_exportName__) \
{\"use strict\";";
const WRAPPER_TAIL: &str = "\n})";

pub struct Transformed {
    pub code: String,
    pub deps: Vec<String>,
}

fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset.min(source.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rfind('\n').map_or(before.len(), |i| before.len() - i - 1) + 1;
    (line, col)
}

/// The runner's async wrapper for a module. `Err` is a SyntaxError message.
pub fn transform(source: &str, key: &str) -> Result<Transformed, String> {
    use oxc::allocator::Allocator;
    use oxc::codegen::Codegen;
    use oxc::parser::Parser;
    use oxc::semantic::SemanticBuilder;
    use oxc::span::SourceType;
    use oxc_transformer_plugins::ModuleRunnerTransform;

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        let at = error
            .labels
            .as_ref()
            .first()
            .map(|label| line_col(source, label.offset() as usize));
        return Err(match at {
            Some((line, col)) => format!("{} ({key}:{line}:{col})", error.message),
            None => format!("{} ({key})", error.message),
        });
    }
    let mut program = parsed.program;
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let (deps, _dynamic) =
        ModuleRunnerTransform::new().transform(&allocator, &mut program, scoping);
    let body = Codegen::new().build(&program).code;
    let mut deps: Vec<String> = deps.into_iter().collect();
    deps.sort();
    Ok(Transformed {
        code: format!("{WRAPPER_HEAD}{body}{WRAPPER_TAIL}"),
        deps,
    })
}

thread_local! {
    /// `__nsEsmRunner`, installed on first use
    static RUNNER: Cell<napi_ref> = const { Cell::new(null_mut()) };
}

fn fail<T>(js: Js, e: Option<String>) -> Load<T> {
    if let Some(e) = &e {
        js.throw(e);
    }
    Err(None)
}

fn throw_syntax_error(js: Js, message: &str) {
    let error = js
        .get(js.global(), "SyntaxError")
        .and_then(|ctor| {
            let mut v = null_mut();
            let arg = js.string(message);
            ok(unsafe { napi_new_instance(js.0, ctor, 1, &arg, &mut v) }).map(|_| v)
        });
    match error {
        Ok(e) => unsafe {
            napi_throw(js.0, e);
        },
        Err(_) => js.throw(message),
    }
}

/// `__nsEsmLoad(key) → { kind, fn, deps } | { kind, value }`
fn load(st: &State, key: &str) -> Load<napi_value> {
    let js = st.js();
    let path = Path::new(key);
    let out = js.object();
    let kind = match format_of(path) {
        Format::Esm => {
            let source = read_source(path)?;
            let t = match transform(&source, key) {
                Ok(t) => t,
                Err(e) => {
                    throw_syntax_error(js, &e);
                    return Err(None);
                }
            };
            let function = crate::run_script(js, &t.code, key).map_err(|_| None)?;
            let deps: Vec<napi_value> = t.deps.iter().map(|d| js.string(d)).collect();
            js.set(out, "fn", function);
            js.set(out, "deps", js.array(&deps));
            "esm"
        }
        Format::CommonJs => {
            js.set(out, "value", require_cjs(st, path)?);
            "cjs"
        }
        Format::Addon => {
            js.set(out, "value", load_addon(st, path)?);
            "cjs"
        }
        Format::Json => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{key}: {e}"))?;
            let json = js.get(js.global(), "JSON").map_err(|_| None)?;
            let parse = js.get(json, "parse").map_err(|_| None)?;
            let value = js.call(parse, json, &[js.string(&text)]).map_err(|_| None)?;
            js.set(out, "value", value);
            "json"
        }
    };
    js.set(out, "kind", js.string(kind));
    Ok(out)
}

/// Resolves `spec` from the module `referrer` (a key), or the working directory.
fn resolve_key(spec: &str, referrer: Option<&str>) -> Result<PathBuf, String> {
    let base = match referrer {
        Some(r) => dir_of(Path::new(r)),
        None => std::env::current_dir().unwrap_or_default(),
    };
    resolve(spec, &base, Kind::Import)
}

fn opt_string(js: Js, v: napi_value) -> Option<String> {
    matches!(js.type_of(v), STRING).then(|| js.to_string(v))
}

unsafe extern "C" fn native_load(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    let key = st.js().to_string(a[0]);
    load(st, &key).or_else(|e| fail(st.js(), e)).unwrap_or(null_mut())
}

unsafe extern "C" fn native_resolve(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<2>(env, info);
    let spec = js.to_string(a[0]);
    match resolve_key(&spec, opt_string(js, a[1]).as_deref()) {
        Ok(p) => js.string(&p.to_string_lossy()),
        Err(e) => {
            js.throw(&e);
            null_mut()
        }
    }
}

unsafe extern "C" fn native_file_url(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let (a, ..) = cb_args::<1>(env, info);
    js.string(&path_to_file_url(Path::new(&js.to_string(a[0]))))
}

/// Reports an error from a module graph as uncaught.
unsafe extern "C" fn native_report(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    napi_throw(env, a[0]);
    crate::event_loop::report_pending(st);
    null_mut()
}

fn runner(st: &State) -> Load<napi_value> {
    let js = st.js();
    let r = RUNNER.with(Cell::get);
    if !r.is_null() {
        return Ok(js.deref(r));
    }
    let init = crate::run_script(js, RUNNER_JS, "nativescript:module-runner").map_err(|_| None)?;
    let natives = js.object();
    type Cb = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;
    for (name, cb) in [
        ("load", native_load as Cb),
        ("resolve", native_resolve),
        ("fileUrl", native_file_url),
        ("report", native_report),
    ] {
        js.set(natives, name, js.function(name, Some(cb), null_mut()));
    }
    let runner = js.call(init, js.undefined(), &[natives]).map_err(|_| None)?;
    RUNNER.with(|c| c.set(js.reference(runner, 1)));
    Ok(runner)
}

/// Runs the entry module; errors are reported as uncaught when the graph settles.
pub fn run_entry(st: &State, path: &Path) -> Load<()> {
    let js = st.js();
    let runner = runner(st)?;
    let run = js.get(runner, "runEntry").map_err(|_| None)?;
    js.call(run, runner, &[js.string(&path.to_string_lossy())])
        .map_err(|_| None)?;
    Ok(())
}

/// `require()` of an ES module: evaluates it now, draining jobs, unless it
/// awaits something outside the graph.
pub fn require(st: &State, path: &Path) -> Load<napi_value> {
    let js = st.js();
    let runner = runner(st)?;
    let import = js.get(runner, "requireModule").map_err(|_| None)?;
    let record = js
        .call(import, runner, &[js.string(&path.to_string_lossy())])
        .map_err(|_| None)?;
    let state_of = |r| js.get(r, "state").map(|s| js.to_string(s)).unwrap_or_default();
    if state_of(record) == "pending" {
        unsafe { js_execute_pending_jobs(js.0) };
    }
    match state_of(record).as_str() {
        "evaluated" => js.get(record, "exports").map_err(|_| None),
        "failed" => {
            let e = js.get(record, "error").map_err(|_| None)?;
            unsafe { napi_throw(js.0, e) };
            Err(None)
        }
        _ => Err(Some(format!(
            "require() of {} which uses top-level await, use import()",
            path.display()
        ))),
    }
}

/// The module runner, evaluated as a classic script on every engine (ES5-ish
/// syntax, nothing the transform would have to lower).
const RUNNER_JS: &str = r#"(function (natives) {
  'use strict';
  var load = natives.load, resolve = natives.resolve, fileUrl = natives.fileUrl, report = natives.report;
  // key -> { key, exports, promise, state, error, stack }; `stack` is the static
  // import chain a module was first reached through, which is how a cycle shows
  var registry = new Map();

  function namespace() {
    var ns = Object.create(null);
    Object.defineProperty(ns, Symbol.toStringTag, { value: 'Module' });
    return ns;
  }

  function importMeta(key) {
    var sep = key.lastIndexOf('/');
    return {
      url: fileUrl(key),
      filename: key,
      dirname: sep < 0 ? '' : key.slice(0, sep) || '/',
      resolve: function (spec) { return fileUrl(resolve(String(spec), key)); }
    };
  }

  function defineExport(exports, name, getter) {
    Object.defineProperty(exports, name, { enumerable: true, configurable: true, get: getter });
  }

  // `export * from`: every name but `default` and the module's own
  function exportAll(exports, source) {
    for (var name in source) {
      if (name !== 'default' && !(name in exports)) {
        defineExport(exports, name, (function (n) { return function () { return source[n]; }; })(name));
      }
    }
  }

  function settle(record, promise) {
    record.promise = promise.then(function () {
      record.state = 'evaluated';
      return record.exports;
    }, function (e) {
      record.state = 'failed';
      record.error = e;
      throw e;
    });
    // rejections surface through importers / the entry, not as unhandled
    record.promise.then(null, function () {});
  }

  // Loads and starts evaluating `key`. A load failure throws and registers nothing.
  function instantiate(key, parentStack) {
    var loaded = load(key);
    var record = { key: key, exports: namespace(), promise: null, state: 'pending', error: undefined,
      stack: parentStack.concat([key]) };
    var exports = record.exports;
    if (loaded.kind !== 'esm') {
      var value = loaded.value;
      defineExport(exports, 'default', function () { return value; });
      if (loaded.kind === 'cjs' && value !== null && (typeof value === 'object' || typeof value === 'function')) {
        Object.keys(value).forEach(function (name) {
          if (name !== 'default') defineExport(exports, name, function () { return value[name]; });
        });
      }
      record.state = 'evaluated';
      record.promise = Promise.resolve(exports);
      registry.set(key, record);
      return record;
    }
    registry.set(key, record);
    var body;
    try {
      body = loaded.fn.call(
        undefined,
        exports,
        importMeta(key),
        function (s) { return importFrom(s, record); },
        function (s) { return dynamicImport(s, key); },
        function (source) { exportAll(exports, source); },
        function (name, getter) { defineExport(exports, name, getter); }
      );
    } catch (e) {
      body = Promise.reject(e);
    }
    settle(record, body);
    return record;
  }

  // Static import from `importer`'s body.
  function importFrom(spec, importer) {
    var key = resolve(spec, importer.key);
    var record = registry.get(key);
    if (record) {
      if (record.state === 'failed') return Promise.reject(record.error);
      // in a cycle, take the partial exports: their getters fill in once the body runs
      if (record.state === 'evaluated' || importer.stack.indexOf(key) >= 0) return record.exports;
      return record.promise;
    }
    try {
      return instantiate(key, importer.stack).promise;
    } catch (e) {
      return Promise.reject(e);
    }
  }

  // `import()`: the namespace once the graph (with any top-level await) has evaluated
  function dynamicImport(spec, referrer) {
    try {
      var key = resolve(String(spec), referrer);
      return (registry.get(key) || instantiate(key, [])).promise;
    } catch (e) {
      return Promise.reject(e);
    }
  }

  return {
    runEntry: function (key) {
      var promise;
      try {
        promise = (registry.get(key) || instantiate(key, [])).promise;
      } catch (e) {
        promise = Promise.reject(e);
      }
      return promise.then(null, function (e) { report(e); });
    },
    requireModule: function (key) {
      return registry.get(key) || instantiate(key, []);
    },
    importModule: dynamicImport
  };
})"#;

#[cfg(test)]
mod tests {
    use super::transform;

    #[test]
    fn exports_are_getters_hoisted_above_imports() {
        let out = transform(
            "import d, { a } from './dep.mjs';\nexport let n = a;\nexport default d;\nexport * from './star.mjs';",
            "/app/main.mjs",
        )
        .unwrap();
        assert_eq!(out.deps, vec!["./dep.mjs".to_string(), "./star.mjs".to_string()]);
        let getter = out.code.find("Object.defineProperty(__vite_ssr_exports__, \"n\"").unwrap();
        let import = out.code.find("await __vite_ssr_import__(\"./dep.mjs\"").unwrap();
        assert!(getter < import, "{}", out.code);
    }

    #[test]
    fn syntax_errors_name_the_module_and_location() {
        let err = transform("export const ok = 1;\nexport const = 2;", "/app/bad.mjs")
            .err()
            .unwrap();
        assert!(err.contains("(/app/bad.mjs:2:"), "{err}");
    }
}
