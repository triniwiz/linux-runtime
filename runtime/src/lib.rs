//! NativeScript runtime for Linux: V8 (rusty_v8) with C APIs exposed lazily
//! from the metadata generator's output.

mod event_loop;
mod ffi;
mod globals;
mod handles;
mod inspector;
mod interop;
mod marshal;
mod modules;
mod napi;
mod natives;
mod net;
mod state;
mod structs;
mod workers;

pub use nativescript_metadata::MetaFile;

use state::State;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Once;

static V8_INIT: Once = Once::new();

pub struct Runtime {
    // field order matters: handles must be dropped before the isolate
    context: v8::Global<v8::Context>,
    state: Rc<State>,
    isolate: Box<v8::OwnedIsolate>,
}

impl Runtime {
    pub fn new(metadata: MetaFile) -> Result<Self, String> {
        Self::build(Arc::new(metadata), None)
    }

    /// Runtime sharing already loaded metadata (e.g. with other threads).
    pub fn with_metadata(metadata: Arc<MetaFile>) -> Result<Self, String> {
        Self::build(metadata, None)
    }

    pub(crate) fn new_worker(metadata: Arc<MetaFile>, parent: workers::Parent) -> Result<Self, String> {
        Self::build(metadata, Some(parent))
    }

    fn build(metadata: Arc<MetaFile>, parent: Option<workers::Parent>) -> Result<Self, String> {
        V8_INIT.call_once(|| {
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
        });

        // Boxed so the isolate address stays stable for libffi closures
        let mut isolate = Box::new(v8::Isolate::new(v8::CreateParams::default()));
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 50);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(event_loop::promise_reject_callback);
        isolate.set_host_import_module_dynamically_callback(modules::dynamic_import);
        isolate.set_host_initialize_import_meta_object_callback(modules::import_meta);
        napi::keep_symbols();
        let state = Rc::new(State::new(metadata));
        *state.parent.borrow_mut() = parent;
        state.isolate.set(&mut **isolate as *mut v8::Isolate);
        isolate.set_slot(state.clone());

        let context = {
            v8::scope!(let scope, &mut **isolate);
            let template = v8::ObjectTemplate::new(scope);
            globals::install_interceptor(&template);
            let context = v8::Context::new(
                scope,
                v8::ContextOptions {
                    global_template: Some(template),
                    ..Default::default()
                },
            );
            let scope = &mut v8::ContextScope::new(scope, context);
            let global = context.global(scope);
            interop::install(scope, &state, global);
            event_loop::install(scope, global);
            let cwd = std::env::current_dir().unwrap_or_default();
            if let Some(require) = modules::require_function(scope, &cwd) {
                let key = v8::String::new(scope, "require").unwrap();
                global.set(scope, key.into(), require.into());
            }
            v8::tc_scope!(let tc, scope);
            if let Err(e) = globals::install(tc, &state) {
                if let Some(exception) = tc.exception() {
                    let message = tc.message();
                    report_exception(tc, exception, message);
                }
                return Err(e);
            }
            v8::Global::new(tc, context)
        };
        *state.context.borrow_mut() = Some(context.clone());
        event_loop::set_current(Some(state.clone()));

        Ok(Runtime { context, state, isolate })
    }

    pub fn metadata(&self) -> &MetaFile {
        &self.state.meta
    }

    /// Runs a classic script. Uncaught exceptions are printed and returned as `Err`.
    pub fn run_script(&mut self, source: &str, name: &str) -> Result<(), String> {
        v8::scope!(let scope, &mut **self.isolate);
        let context = v8::Local::new(scope, &self.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        v8::tc_scope!(let tc, scope);

        let source = v8::String::new(tc, source).ok_or("script too large")?;
        let resource = v8::String::new(tc, name).unwrap();
        let origin = v8::ScriptOrigin::new(tc, resource.into(), 0, 0, false, 0, None, false, false, false, None);
        let ran = v8::Script::compile(tc, source, Some(&origin)).and_then(|script| script.run(tc));
        if let Some(exception) = tc.exception() {
            let message = tc.message();
            let text = report_exception(tc, exception, message);
            return Err(text);
        }
        if ran.is_some() {
            let before = self.state.uncaught.get();
            event_loop::checkpoint(tc, &self.state);
            if self.state.uncaught.get() != before {
                return Err("unhandled promise rejection".into());
            }
        }
        Ok(())
    }

    /// Starts the DevTools inspector on `port`. With `wait`, blocks until a
    /// frontend attaches and pauses on the first statement.
    pub fn start_inspector(&mut self, port: u16, wait: bool, title: &str) -> Result<(), String> {
        inspector::start(&self.state, port, title)?;
        if wait {
            inspector::wait_for_debugger(&self.state);
        }
        Ok(())
    }

    /// Files of the modules loaded so far (to watch for changes).
    pub fn loaded_files(&self) -> Vec<std::path::PathBuf> {
        modules::loaded_files(&self.state)
    }

    /// Sets `process.argv`.
    pub fn set_argv(&mut self, argv: &[String]) {
        v8::scope!(let scope, &mut **self.isolate);
        let context = v8::Local::new(scope, &self.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        let items: Vec<v8::Local<v8::Value>> = argv.iter().map(|a| v8::String::new(scope, a).unwrap().into()).collect();
        let array = v8::Array::new_with_elements(scope, &items);
        let global = context.global(scope);
        let key = v8::String::new(scope, "process").unwrap();
        if let Some(process) = global.get(scope, key.into()).and_then(|p| p.to_object(scope)) {
            let key = v8::String::new(scope, "argv").unwrap();
            process.set(scope, key.into(), array.into());
        }
    }

    /// Evaluates `source` and returns the result as a string.
    pub fn eval_to_string(&mut self, source: &str) -> Result<String, String> {
        v8::scope!(let scope, &mut **self.isolate);
        let context = v8::Local::new(scope, &self.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        v8::tc_scope!(let tc, scope);
        let source = v8::String::new(tc, source).ok_or("script too large")?;
        let value = v8::Script::compile(tc, source, None).and_then(|s| s.run(tc));
        match value {
            Some(v) => Ok(v.to_rust_string_lossy(tc)),
            None => {
                let e = tc.exception().map(|e| e.to_rust_string_lossy(tc)).unwrap_or_default();
                Err(e)
            }
        }
    }

    /// Runs timers and other pending work until nothing keeps the loop alive.
    /// Returns an error if uncaught errors were reported meanwhile.
    pub fn run_event_loop(&mut self) -> Result<(), String> {
        let before = self.state.uncaught.get();
        event_loop::run(&self.state);
        match self.state.uncaught.get() - before {
            0 => Ok(()),
            n => Err(format!("{n} uncaught error(s)")),
        }
    }

    /// Runs a file as an ES module or CommonJS module (by extension,
    /// package.json "type" or syntax).
    pub fn run_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
        v8::scope!(let scope, &mut **self.isolate);
        let context = v8::Local::new(scope, &self.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        v8::tc_scope!(let tc, scope);
        let outcome = modules::run_entry(tc, &self.state, &path);
        if let Some(exception) = tc.exception() {
            let message = tc.message();
            return Err(report_exception(tc, exception, message));
        }
        if let Err(Some(e)) = outcome {
            eprintln!("Error: {e}");
            return Err(e);
        }
        let before = self.state.uncaught.get();
        event_loop::checkpoint(tc, &self.state);
        if self.state.uncaught.get() != before {
            return Err("unhandled promise rejection".into());
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        workers::shutdown(&self.state);
        self.state.inspector.borrow_mut().take();
        if !self.state.napi_envs.borrow().is_empty() {
            event_loop::with_js(&self.state, |_| napi::teardown(&self.state));
        }
        event_loop::set_current(None);
        self.state.timers.borrow_mut().clear();
        self.state.rejections.borrow_mut().clear();
    }
}

pub(crate) fn throw_error(scope: &mut v8::PinScope, message: &str) {
    let message = v8::String::new(scope, message).unwrap();
    let error = v8::Exception::error(scope, message);
    scope.throw_exception(error);
}

/// Prints an uncaught exception with its stack (or location) to stderr.
pub(crate) fn report_exception<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    exception: v8::Local<'s, v8::Value>,
    message: Option<v8::Local<'s, v8::Message>>,
) -> String {
    let mut text = String::new();
    if let Ok(object) = v8::Local::<v8::Object>::try_from(exception) {
        let key = v8::String::new(scope, "stack").unwrap();
        if let Some(stack) = object.get(scope, key.into()).filter(|s| s.is_string()) {
            text = stack.to_rust_string_lossy(scope);
        }
    }
    if text.is_empty() {
        text = exception.to_rust_string_lossy(scope);
        if let Some(message) = message {
            let resource = message
                .get_script_resource_name(scope)
                .map(|r| r.to_rust_string_lossy(scope))
                .unwrap_or_default();
            let line = message.get_line_number(scope).unwrap_or(0);
            text = format!("{text}\n    at {resource}:{line}");
        }
    }
    eprintln!("Uncaught {text}");
    text
}
