//! NativeScript Linux runtime written against Node-API, for any engine that
//! provides napi_* plus the jsr host functions (js_create_runtime, ...), or as
//! an addon inside an existing Node-API host (`engine` off).

#[cfg(not(feature = "engine"))]
pub mod addon;
mod app_bundle;
#[cfg(feature = "engine")]
pub mod cli;
#[cfg(feature = "compat")]
pub mod compat;
#[cfg(feature = "module_runner")]
pub mod esm;
pub mod event_loop;
pub mod ffi;
pub mod globals;
pub mod interop;
pub mod js;
pub mod marshal;
pub mod modules;
pub mod natives;
pub mod state;
pub mod structs;
pub mod sys;

pub use nativescript_metadata::{MetaFile, MetaSet};

use js::{Js, JsResult};
use std::ptr::null_mut;
use sys::*;

/// Runs a classic script, returning its completion value.
pub fn run_script(js: Js, source: &str, name: &str) -> JsResult<napi_value> {
    let mut result = null_mut();
    #[cfg(feature = "engine")]
    {
        let file = std::ffi::CString::new(name).unwrap_or_default();
        js::ok(unsafe { js_execute_script(js.0, js.string(source), file.as_ptr(), &mut result) })?;
    }
    #[cfg(not(feature = "engine"))]
    {
        let _ = name;
        js::ok(unsafe { napi_run_script(js.0, js.string(source), &mut result) })?;
    }
    Ok(result)
}

#[cfg(feature = "engine")]
mod engine_runtime {
use super::*;
use js::Thrown;
use state::State;
use std::path::Path;
use std::sync::Arc;

const PRELUDES: [(&str, &str); 3] = [
    ("nativescript:tslib", include_str!("../../core/js/tslib.js")),
    (
        "nativescript:prelude",
        include_str!("../../core/js/prelude.js"),
    ),
    ("nativescript:web", include_str!("../../core/js/web.js")),
];

/// Engine features some shims lack.
const POLYFILLS: &str = r#"
if (typeof queueMicrotask !== 'function') {
  Object.defineProperty(globalThis, 'queueMicrotask', {
    value: (f) => { Promise.resolve().then(f); }, writable: true, configurable: true,
  });
}
"#;

pub struct Runtime {
    state: Box<State>,
    runtime: napi_runtime,
    env: napi_env,
    scope: napi_handle_scope,
}

impl Runtime {
    /// `engine` names the engine in `process.versions`.
    pub fn new(metadata: Arc<MetaSet>, engine: &str) -> Result<Self, String> {
        let mut runtime = null_mut();
        let mut env = null_mut();
        unsafe {
            if js_create_runtime(&mut runtime) != 0 || js_create_napi_env(&mut env, runtime) != 0 {
                return Err("failed to create the JS engine".into());
            }
        }
        let state = Box::new(State::new(metadata, env));
        state::set_current(&*state);
        let mut scope = null_mut();
        unsafe { napi_open_handle_scope(env, &mut scope) };
        let rt = Runtime {
            state,
            runtime,
            env,
            scope,
        };
        rt.install(engine)
            .map_err(|_| rt.take_error("failed to initialize the runtime"))?;
        event_loop::install_livesync_signal();
        Ok(rt)
    }

    fn install(&self, engine: &str) -> JsResult<()> {
        let st = &*self.state;
        let js = st.js();
        let global = js.global();
        run_script(js, POLYFILLS, "nativescript:polyfills")?;
        interop::install(st, global);
        event_loop::install(st, global)?;
        // no referrer: resolved from the app folder (see resolve::default_base)
        js.set(
            global,
            "require",
            modules::require_function(js, Path::new("")),
        );
        globals::install(st)?;
        let argv: Vec<String> = std::env::args().collect();
        let natives = natives::create(js, &argv, engine);
        for (name, code) in PRELUDES {
            let init = run_script(js, code, name)?;
            js.call(init, js.undefined(), &[natives])?;
        }
        Ok(())
    }

    /// Prints and clears the pending exception.
    fn take_error(&self, fallback: &str) -> String {
        let js = self.state.js();
        match js.take_exception() {
            Some(e) => {
                let text = js.describe(e);
                eprintln!("Uncaught {text}");
                self.state.uncaught.set(self.state.uncaught.get() + 1);
                text
            }
            None => fallback.to_owned(),
        }
    }

    pub fn env(&self) -> napi_env {
        self.env
    }

    pub fn metadata(&self) -> &MetaSet {
        &self.state.meta
    }

    /// Node style `process.argv`.
    pub fn set_argv(&self, argv: &[String]) {
        let js = self.state.js();
        if let Ok(process) = js.get(js.global(), "process") {
            let items: Vec<napi_value> = argv.iter().map(|a| js.string(a)).collect();
            js.set(process, "argv", js.array(&items));
        }
    }

    /// Runs a classic script. Uncaught exceptions are printed and returned as `Err`.
    pub fn run_script(&self, source: &str, name: &str) -> Result<(), String> {
        self.scoped(|st| {
            run_script(st.js(), source, name)
                .map(|_| ())
                .map_err(|Thrown| None)
        })
    }

    /// Runs a file, or an app directory, as the entry module.
    pub fn run_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = nativescript_core::resolve::entry_point(path.as_ref())?;
        self.scoped(|st| modules::run_entry(st, &path))
    }

    /// Evaluates `source` and returns its string form (tests).
    pub fn eval_to_string(&self, source: &str) -> Result<String, String> {
        let mut out = String::new();
        self.scoped(|st| {
            let v = run_script(st.js(), source, "[eval]").map_err(|Thrown| None)?;
            out = st.js().to_string(v);
            Ok(())
        })?;
        Ok(out)
    }

    fn scoped(&self, f: impl FnOnce(&State) -> modules::Load<()>) -> Result<(), String> {
        let st = &*self.state;
        let js = st.js();
        // includes errors thrown by C -> JS callbacks
        let before = st.uncaught.get();
        let mut scope = null_mut();
        unsafe { napi_open_handle_scope(js.0, &mut scope) };
        let result = match f(st) {
            Ok(()) => Ok(()),
            Err(Some(e)) if !js.is_exception_pending() => {
                eprintln!("Uncaught Error: {e}");
                st.uncaught.set(st.uncaught.get() + 1);
                Err(e)
            }
            Err(_) => Err(self.take_error("script failed")),
        };
        event_loop::checkpoint(st);
        unsafe { napi_close_handle_scope(js.0, scope) };
        match (result, st.uncaught.get() - before) {
            (Ok(()), 0) => Ok(()),
            (Ok(()), n) => Err(format!("{n} uncaught error(s)")),
            (r, _) => r,
        }
    }

    /// Runs timers and callbacks until nothing keeps the loop alive.
    pub fn run_event_loop(&self) -> Result<(), String> {
        let before = self.state.uncaught.get();
        event_loop::run(&self.state);
        if self.state.uncaught.get() != before {
            return Err("uncaught exception".into());
        }
        Ok(())
    }

    pub fn uncaught(&self) -> u32 {
        self.state.uncaught.get()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Node runs cleanup hooks in reverse order
        let hooks = std::mem::take(&mut *self.state.cleanup_hooks.borrow_mut());
        for (hook, arg) in hooks.into_iter().rev() {
            unsafe { hook(arg as *mut std::ffi::c_void) };
        }
        // refs must go before the env
        self.state.timers.borrow_mut().clear();
        unsafe {
            napi_close_handle_scope(self.env, self.scope);
            js_free_napi_env(self.env);
            js_free_runtime(self.runtime);
        }
        state::set_current(std::ptr::null());
    }
}

}
#[cfg(feature = "engine")]
pub use engine_runtime::Runtime;
