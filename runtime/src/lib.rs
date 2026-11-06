//! NativeScript runtime for Linux: V8 (rusty_v8) with C APIs exposed lazily
//! from the metadata generator's output.

mod ffi;
mod globals;
mod interop;
mod longdouble;
mod marshal;
mod state;
mod structs;

pub use nativescript_metadata::MetaFile;

use state::State;
use std::path::Path;
use std::rc::Rc;
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
        V8_INIT.call_once(|| {
            let platform = v8::new_default_platform(0, false).make_shared();
            v8::V8::initialize_platform(platform);
            v8::V8::initialize();
        });

        // Boxed so the isolate address stays stable for libffi closures
        let mut isolate = Box::new(v8::Isolate::new(v8::CreateParams::default()));
        isolate.set_capture_stack_trace_for_uncaught_exceptions(true, 50);
        let state = Rc::new(State::new(metadata));
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
            v8::tc_scope!(let tc, scope);
            if let Err(e) = globals::install(tc) {
                if let Some(exception) = tc.exception() {
                    let message = tc.message();
                    report_exception(tc, exception, message);
                }
                return Err(e);
            }
            v8::Global::new(tc, context)
        };
        *state.context.borrow_mut() = Some(context.clone());

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
        if ran.is_some() {
            tc.perform_microtask_checkpoint();
        }
        if let Some(exception) = tc.exception() {
            let message = tc.message();
            let text = report_exception(tc, exception, message);
            return Err(text);
        }
        Ok(())
    }

    pub fn run_file(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let source = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // allow `#!/usr/bin/env ns-linux` scripts
        let source = if source.starts_with("#!") {
            format!("//{source}")
        } else {
            source
        };
        let name = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.run_script(&source, &name.to_string_lossy())
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
