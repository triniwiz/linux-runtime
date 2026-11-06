//! Event loop on GLib's main context (timers also fire inside GTK main loops).
//! Microtasks never run in the middle of a C call; C callbacks from other
//! threads run on the JS thread.

use crate::state::{state, State};
use crate::{report_exception, throw_error};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Condvar, Mutex};

use nativescript_core::glib::*;

pub struct Timer {
    function: v8::Global<v8::Function>,
    args: Vec<v8::Global<v8::Value>>,
    repeat: bool,
    /// GSource*, we hold a reference
    source: usize,
}

impl Drop for Timer {
    fn drop(&mut self) {
        unsafe { g_source_unref(self.source as gpointer) };
    }
}

pub use nativescript_core::glib::MainContext;

fn attach(source: gpointer, priority: i32, func: SourceFunc, data: gpointer, st: &State) {
    unsafe {
        g_source_set_priority(source, priority);
        g_source_set_callback(source, func, data, None);
        g_source_attach(source, st.main_context.context as gpointer);
    }
}

thread_local! {
    /// Runtime of the JS thread, for GLib callbacks that only get a gpointer.
    static CURRENT: RefCell<Option<Rc<State>>> = const { RefCell::new(None) };
}

pub fn set_current(st: Option<Rc<State>>) {
    CURRENT.with(|c| *c.borrow_mut() = st);
}

fn current() -> Option<Rc<State>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Runs `f` with a fresh scope in the runtime's context, then reports
/// exceptions, runs microtasks and reports unhandled rejections.
pub fn with_js(st: &State, f: impl FnOnce(&mut v8::PinScope<'_, '_>)) {
    let isolate = st.isolate.get();
    let Some(context) = st.context.borrow().clone() else {
        return;
    };
    let isolate = unsafe { &mut *isolate };
    v8::scope!(let scope, isolate);
    let context = v8::Local::new(scope, &context);
    let scope = &mut v8::ContextScope::new(scope, context);
    v8::tc_scope!(let tc, scope);
    f(tc);
    if let Some(exception) = tc.exception() {
        let message = tc.message();
        report_exception(tc, exception, message);
        st.uncaught.set(st.uncaught.get() + 1);
        tc.reset();
    }
    checkpoint(tc, st);
}

/// Runs pending microtasks and reports unhandled rejections.
pub fn checkpoint(scope: &mut v8::PinScope<'_, '_>, st: &State) {
    st.microtasks_scheduled.set(false);
    scope.perform_microtask_checkpoint();
    crate::napi::drain_finalizers(st);
    let rejections = std::mem::take(&mut *st.rejections.borrow_mut());
    for (_, value) in rejections {
        let value = v8::Local::new(scope, &value);
        let mut text = value.to_rust_string_lossy(scope);
        if let Ok(object) = v8::Local::<v8::Object>::try_from(value) {
            let key = v8::String::new(scope, "stack").unwrap();
            if let Some(stack) = object.get(scope, key.into()).filter(|s| s.is_string()) {
                text = stack.to_rust_string_lossy(scope);
            }
        }
        eprintln!("Uncaught (in promise) {text}");
        st.uncaught.set(st.uncaught.get() + 1);
    }
}

pub extern "C" fn promise_reject_callback(message: v8::PromiseRejectMessage) {
    v8::callback_scope!(unsafe scope, &message);
    let st = state(scope);
    let promise = message.get_promise();
    match message.get_event() {
        v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
            let value = message.get_value().unwrap_or_else(|| v8::undefined(scope).into());
            let entry = (v8::Global::new(scope, promise), v8::Global::new(scope, value));
            st.rejections.borrow_mut().push(entry);
        }
        v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
            let mut rejections = st.rejections.borrow_mut();
            rejections.retain(|(p, _)| *p != promise);
        }
        _ => {}
    }
}

/// Makes sure microtasks queued during a C -> JS callback run soon, from the
/// main loop rather than in the middle of the C call.
pub fn schedule_microtasks(st: &State) {
    if st.microtasks_scheduled.replace(true) {
        return;
    }
    unsafe extern "C" fn run(_: gpointer) -> gboolean {
        if let Some(st) = current() {
            with_js(&st, |_| {});
        }
        0
    }
    unsafe {
        // attach to the context dispatching this callback (a nested
        // g_main_loop_run may iterate another context than ours)
        let current = g_main_current_source();
        let context = if current.is_null() { std::ptr::null_mut() } else { g_source_get_context(current) };
        let context = if context.is_null() { st.main_context.context as gpointer } else { context };
        let source = g_idle_source_new();
        g_source_set_priority(source, G_PRIORITY_DEFAULT);
        g_source_set_callback(source, run, std::ptr::null_mut(), None);
        g_source_attach(source, context);
        g_source_unref(source);
    }
}

/// Whether the loop has work keeping it alive.
pub fn alive(st: &State) -> bool {
    !st.timers.borrow().is_empty() || st.loop_refs.get() > 0 || st.microtasks_scheduled.get()
}

/// Runs the GLib main context until no timers / references are left.
pub fn run(st: &State) {
    let context = st.main_context.context as gpointer;
    with_js(st, |_| {});
    while alive(st) {
        unsafe { g_main_context_iteration(context, 1) };
    }
}

unsafe extern "C" fn timer_fired(data: gpointer) -> gboolean {
    let id = data as usize as u32;
    let Some(st) = current() else { return 0 };
    let (function, args, repeat) = {
        let timers = st.timers.borrow();
        let Some(timer) = timers.get(&id) else { return 0 };
        (timer.function.clone(), timer.args.clone(), timer.repeat)
    };
    if !repeat {
        let timer = st.timers.borrow_mut().remove(&id);
        drop(timer);
    }
    with_js(&st, |scope| {
        let function = v8::Local::new(scope, &function);
        let args: Vec<v8::Local<v8::Value>> = args.iter().map(|a| v8::Local::new(scope, a)).collect();
        let recv = v8::undefined(scope).into();
        function.call(scope, recv, &args);
    });
    // clearInterval() from the callback removes the timer
    (repeat && st.timers.borrow().contains_key(&id)) as gboolean
}

fn add_timer<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, mut rv: v8::ReturnValue<'s, v8::Value>, repeat: bool) {
    let st = state(scope);
    let Ok(function) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        return throw_error(scope, "the callback must be a function");
    };
    let delay = args.get(1).number_value(scope).filter(|d| d.is_finite()).unwrap_or(0.0).max(0.0);
    let id = st.next_timer.get().wrapping_add(1).max(1);
    st.next_timer.set(id);
    let extra = (2..args.length()).map(|i| v8::Global::new(scope, args.get(i))).collect();
    let interval = (delay as u32).max(if repeat { 1 } else { 0 });
    let source = unsafe { g_timeout_source_new(interval) };
    attach(source, G_PRIORITY_DEFAULT, timer_fired, id as usize as gpointer, &st);
    st.timers.borrow_mut().insert(
        id,
        Timer {
            function: v8::Global::new(scope, function),
            args: extra,
            repeat,
            source: source as usize,
        },
    );
    rv.set(v8::Integer::new_from_unsigned(scope, id).into());
}

fn set_timeout<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue<'s, v8::Value>) {
    add_timer(scope, args, rv, false)
}

fn set_interval<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, rv: v8::ReturnValue<'s, v8::Value>) {
    add_timer(scope, args, rv, true)
}

fn clear_timer<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue<'s, v8::Value>) {
    let st = state(scope);
    let Some(id) = args.get(0).uint32_value(scope) else { return };
    let timer = st.timers.borrow_mut().remove(&id);
    if let Some(timer) = timer {
        unsafe { g_source_destroy(timer.source as gpointer) };
    }
}

fn queue_microtask<'s>(scope: &mut v8::PinScope<'s, '_>, args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue<'s, v8::Value>) {
    let Ok(function) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        return throw_error(scope, "queueMicrotask expects a function");
    };
    scope.enqueue_microtask(function);
}

/// `nativescript.ref()` / `unref()`: keep the loop alive (e.g. while waiting
/// for callbacks from other threads).
fn loop_ref<'s>(scope: &mut v8::PinScope<'s, '_>, _args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue<'s, v8::Value>) {
    let st = state(scope);
    st.loop_refs.set(st.loop_refs.get() + 1);
}

fn loop_unref<'s>(scope: &mut v8::PinScope<'s, '_>, _args: v8::FunctionCallbackArguments<'s>, _rv: v8::ReturnValue<'s, v8::Value>) {
    let st = state(scope);
    st.loop_refs.set((st.loop_refs.get() - 1).max(0));
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, global: v8::Local<'s, v8::Object>) {
    fn set<'s>(scope: &mut v8::PinScope<'s, '_>, target: v8::Local<'s, v8::Object>, name: &str, f: impl v8::MapFnTo<v8::FunctionCallback>) {
        let f = v8::Function::new(scope, f).unwrap();
        let key = v8::String::new(scope, name).unwrap();
        f.set_name(key);
        target.set(scope, key.into(), f.into());
    }
    set(scope, global, "setTimeout", set_timeout);
    set(scope, global, "setInterval", set_interval);
    set(scope, global, "clearTimeout", clear_timer);
    set(scope, global, "clearInterval", clear_timer);
    set(scope, global, "queueMicrotask", queue_microtask);
    let ns = v8::Object::new(scope);
    set(scope, ns, "ref", loop_ref);
    set(scope, ns, "unref", loop_unref);
    let key = v8::String::new(scope, "nativescript").unwrap();
    global.set(scope, key.into(), ns.into());
}

/// A closure invocation posted from a foreign thread.
struct Job {
    run: Mutex<Option<Box<dyn FnOnce()>>>,
    done: Mutex<bool>,
    cv: Condvar,
}

/// Runs `f` on the JS thread and waits for it. The JS thread must be
/// iterating the default main context (our loop, g_main_loop_run, GTK...).
pub fn run_on_js_thread(context: usize, f: Box<dyn FnOnce() + '_>) {
    // SAFETY: the job lives on this stack frame until the JS thread signals
    // completion, so borrowed data outlives the call.
    let f: Box<dyn FnOnce()> = unsafe { std::mem::transmute(f) };
    let job = Job {
        run: Mutex::new(Some(f)),
        done: Mutex::new(false),
        cv: Condvar::new(),
    };
    unsafe extern "C" fn dispatch(data: gpointer) -> gboolean {
        let job = &*(data as *const Job);
        if let Some(f) = job.run.lock().unwrap().take() {
            f();
        }
        *job.done.lock().unwrap() = true;
        job.cv.notify_all();
        0
    }
    // not g_main_context_invoke: it runs inline on this thread when the context is free
    queue(context, dispatch, &job as *const Job as gpointer);
    let mut done = job.done.lock().unwrap();
    while !*done {
        done = job.cv.wait(done).unwrap();
    }
}

type Completion = Box<dyn for<'s, 'i> FnOnce(&mut v8::PinScope<'s, 'i>, &State) + Send>;

/// Keeps the loop alive until a `post` with `release` runs.
pub fn begin_pending(st: &State) {
    st.loop_refs.set(st.loop_refs.get() + 1);
}

pub fn end_pending(st: &State) {
    st.loop_refs.set((st.loop_refs.get() - 1).max(0));
}

/// Runs `f` with this thread's runtime state, if any.
pub fn with_current(f: impl FnOnce(&State)) {
    if let Some(st) = current() {
        f(&st);
    }
}

/// Runs `f` on the JS thread owning `context`, from any thread.
pub fn post(context: usize, release: bool, f: Completion) {
    struct Job {
        release: bool,
        f: Completion,
    }
    unsafe extern "C" fn dispatch(data: gpointer) -> gboolean {
        let job = Box::from_raw(data as *mut Job);
        if let Some(st) = current() {
            if job.release {
                st.loop_refs.set((st.loop_refs.get() - 1).max(0));
            }
            let f = job.f;
            with_js(&st, |scope| f(scope, &st));
        }
        0
    }
    let data = Box::into_raw(Box::new(Job { release, f })) as gpointer;
    queue(context, dispatch, data);
}


/// Runs `f` on the thread owning `context` without entering JS.
pub fn post_state(context: usize, f: Box<dyn FnOnce(&State) + Send>) {
    unsafe extern "C" fn dispatch(data: gpointer) -> gboolean {
        let f = Box::from_raw(data as *mut Box<dyn FnOnce(&State) + Send>);
        if let Some(st) = current() {
            f(&st);
        }
        0
    }
    let data = Box::into_raw(Box::new(f)) as gpointer;
    queue(context, dispatch, data);
}
