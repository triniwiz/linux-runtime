//! Event loop on GLib's main context, as in the V8 runtime.

use crate::js::*;
use crate::state::{current, state, State};
use crate::sys::*;
use nativescript_core::glib::*;
use std::ptr::null_mut;

pub struct Timer {
    env: napi_env,
    function: napi_ref,
    args: Vec<napi_ref>,
    repeat: bool,
    /// GSource*, we hold a reference
    source: usize,
}

impl Drop for Timer {
    fn drop(&mut self) {
        let js = Js(self.env);
        js.delete_ref(self.function);
        for a in &self.args {
            js.delete_ref(*a);
        }
        unsafe { g_source_unref(self.source as gpointer) };
    }
}

/// Reports the pending exception as uncaught.
pub fn report_pending(st: &State) {
    let js = st.js();
    if let Some(e) = js.take_exception() {
        // a Node-API host reports it (process 'uncaughtException')
        #[cfg(not(feature = "engine"))]
        unsafe {
            napi_fatal_exception(js.0, e);
        }
        #[cfg(feature = "engine")]
        {
            // @nativescript/core's hook
            if let Ok(hook) = js.get(js.global(), "__onUncaughtError") {
                if js.type_of(hook) == FUNCTION {
                    let _ = js.call(hook, js.undefined(), &[e]);
                    js.clear_exception();
                }
            }
            eprintln!("Uncaught {}", js.describe(e));
        }
        st.uncaught.set(st.uncaught.get() + 1);
    }
}

/// Runs `f` in a handle scope, then reports exceptions and runs pending jobs.
pub fn with_js(st: &State, f: impl FnOnce(Js)) {
    let js = st.js();
    let mut scope = null_mut();
    unsafe { napi_open_handle_scope(js.0, &mut scope) };
    f(js);
    report_pending(st);
    checkpoint(st);
    unsafe { napi_close_handle_scope(js.0, scope) };
}

/// Runs pending jobs (microtasks) and reports unhandled rejections.
pub fn checkpoint(st: &State) {
    let js = st.js();
    st.jobs_scheduled.set(false);
    crate::state::drain_adopted(st);
    // a Node-API host drains its own microtasks
    #[cfg(not(feature = "engine"))]
    let _ = js;
    // a job that throws stops the queue: report and continue
    #[cfg(feature = "engine")]
    while unsafe { js_execute_pending_jobs(js.0) } != 0 {
        if !js.is_exception_pending() {
            break;
        }
        report_pending(st);
    }
    let flush = st.flush_rejections.get();
    if !flush.is_null() {
        let _ = js.call(js.deref(flush), js.undefined(), &[]);
        report_pending(st);
    }
}

/// Runs jobs queued by a C -> JS callback from the main loop, not mid C call.
pub fn schedule_jobs(st: &State) {
    if st.jobs_scheduled.replace(true) {
        return;
    }
    unsafe extern "C" fn run(_: gpointer) -> gboolean {
        if let Some(st) = current() {
            with_js(st, |_| {});
        }
        0
    }
    unsafe {
        // the context dispatching this callback (a nested loop may iterate another one)
        let source = g_main_current_source();
        let context = if source.is_null() {
            null_mut()
        } else {
            g_source_get_context(source)
        };
        let context = if context.is_null() {
            st.main_context.context
        } else {
            context as usize
        };
        queue(context, run, null_mut());
    }
}

pub fn alive(st: &State) -> bool {
    !st.timers.borrow().is_empty() || st.loop_refs.get() > 0 || st.jobs_scheduled.get()
}

/// Runs the GLib main context until no timers / references are left.
pub fn run(st: &State) {
    with_js(st, |_| {});
    while alive(st) {
        unsafe { g_main_context_iteration(st.main_context.context as gpointer, 1) };
    }
}

unsafe extern "C" fn timer_fired(data: gpointer) -> gboolean {
    let id = data as usize as u32;
    let Some(st) = current() else { return 0 };
    let (function, args, repeat) = {
        let timers = st.timers.borrow();
        let Some(t) = timers.get(&id) else { return 0 };
        (t.function, t.args.clone(), t.repeat)
    };
    // a one-shot timer's refs must outlive the call
    let one_shot = if repeat {
        None
    } else {
        st.timers.borrow_mut().remove(&id)
    };
    with_js(st, |js| {
        let argv: Vec<napi_value> = args.iter().map(|a| js.deref(*a)).collect();
        let _ = js.call(js.deref(function), js.undefined(), &argv);
    });
    drop(one_shot);
    // clearInterval() from the callback removes the timer
    (repeat && st.timers.borrow().contains_key(&id)) as gboolean
}

unsafe fn add_timer(env: napi_env, info: napi_callback_info, repeat: bool) -> napi_value {
    let st = state(env);
    let js = st.js();
    let mut argc = 0usize;
    napi_get_cb_info(env, info, &mut argc, null_mut(), null_mut(), null_mut());
    let mut argv = vec![null_mut(); argc.max(2)];
    let undefined = js.undefined();
    argv.iter_mut().for_each(|a| *a = undefined);
    napi_get_cb_info(
        env,
        info,
        &mut argc,
        argv.as_mut_ptr(),
        null_mut(),
        null_mut(),
    );
    if js.type_of(argv[0]) != FUNCTION {
        js.throw_type("the callback must be a function");
        return null_mut();
    }
    let delay = js
        .f64_of(argv[1])
        .ok()
        .filter(|d| d.is_finite())
        .unwrap_or(0.0)
        .max(0.0);
    let id = st.next_timer.get().wrapping_add(1).max(1);
    st.next_timer.set(id);
    let interval = (delay as u32).max(repeat as u32);
    let source = g_timeout_source_new(interval);
    g_source_set_priority(source, G_PRIORITY_DEFAULT);
    g_source_set_callback(source, timer_fired, id as usize as gpointer, None);
    g_source_attach(source, st.main_context.context as gpointer);
    let timer = Timer {
        env,
        function: js.reference(argv[0], 1),
        args: argv.iter().skip(2).map(|a| js.reference(*a, 1)).collect(),
        repeat,
        source: source as usize,
    };
    st.timers.borrow_mut().insert(id, timer);
    js.uint(id)
}

unsafe extern "C" fn set_timeout(env: napi_env, info: napi_callback_info) -> napi_value {
    add_timer(env, info, false)
}

unsafe extern "C" fn set_interval(env: napi_env, info: napi_callback_info) -> napi_value {
    add_timer(env, info, true)
}

unsafe extern "C" fn clear_timer(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    let id = st.js().f64_of(a[0]).unwrap_or(0.0) as u32;
    let timer = st.timers.borrow_mut().remove(&id);
    if let Some(timer) = timer {
        g_source_destroy(timer.source as gpointer);
    }
    null_mut()
}

unsafe extern "C" fn loop_ref(env: napi_env, _: napi_callback_info) -> napi_value {
    let st = state(env);
    st.loop_refs.set(st.loop_refs.get() + 1);
    null_mut()
}

unsafe extern "C" fn loop_unref(env: napi_env, _: napi_callback_info) -> napi_value {
    let st = state(env);
    st.loop_refs.set((st.loop_refs.get() - 1).max(0));
    null_mut()
}

unsafe extern "C" fn report_rejection(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (a, ..) = cb_args::<1>(env, info);
    eprintln!("Uncaught (in promise) {}", st.js().describe(a[0]));
    st.uncaught.set(st.uncaught.get() + 1);
    null_mut()
}

/// Rejection tracking through the engine's `onUnhandledPromiseRejectionTracker` hook.
const REJECTIONS: &str = r#"(function (report) {
  const pending = new Map();
  Object.defineProperty(globalThis, 'onUnhandledPromiseRejectionTracker', {
    value(promise, reason, handled) {
      if (handled) pending.delete(promise);
      else pending.set(promise, reason);
    },
  });
  return function flush() {
    if (pending.size === 0) return;
    const reasons = [...pending.values()];
    pending.clear();
    for (const r of reasons) report(r);
  };
})"#;

pub fn install(st: &State, global: napi_value) -> JsResult<()> {
    let js = st.js();
    type Cb = unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value;
    for (name, cb) in [
        ("setTimeout", set_timeout as Cb),
        ("setInterval", set_interval),
        ("clearTimeout", clear_timer),
        ("clearInterval", clear_timer),
    ] {
        js.set(global, name, js.function(name, Some(cb), null_mut()));
    }
    let ns = js.object();
    js.set(ns, "ref", js.function("ref", Some(loop_ref), null_mut()));
    js.set(
        ns,
        "unref",
        js.function("unref", Some(loop_unref), null_mut()),
    );
    js.set(global, "nativescript", ns);

    let init = crate::run_script(js, REJECTIONS, "nativescript:rejections")?;
    let report = js.function("report", Some(report_rejection), null_mut());
    let flush = js.call(init, js.undefined(), &[report])?;
    st.flush_rejections.set(js.reference(flush, 1));
    Ok(())
}

/// Runs `f` on the JS thread owning `context` (from any thread), then
/// drains jobs. `release` drops a loop reference taken with `loop_ref`.
pub fn post(context: usize, release: bool, f: Box<dyn FnOnce(&State) + Send>) {
    struct Job {
        release: bool,
        f: Box<dyn FnOnce(&State) + Send>,
    }
    unsafe extern "C" fn dispatch(data: gpointer) -> gboolean {
        let job = Box::from_raw(data as *mut Job);
        if let Some(st) = current() {
            if job.release {
                st.loop_refs.set((st.loop_refs.get() - 1).max(0));
            }
            let f = job.f;
            with_js(st, |_| f(st));
        }
        0
    }
    queue(
        context,
        dispatch,
        Box::into_raw(Box::new(Job { release, f })) as gpointer,
    );
}

/// Keeps the loop alive until a matching `post(.., release = true, ..)`.
pub fn hold(st: &State) {
    st.loop_refs.set(st.loop_refs.get() + 1);
}

/// SIGUSR1 calls `global.__onLiveSync()` (the CLI signals after syncing files).
pub fn install_livesync_signal() {
    use nativescript_core::nsbundle::{fuses, Fuse};
    // release builds: no LiveSync hook (any same-user process could reset the UI)
    if !fuses::get().is_on(Fuse::EnvOverrides) {
        return;
    }
    fn fire() {
        if let Some(st) = current() {
            with_js(st, |js| {
                if let Ok(f) = js.get(js.global(), "__onLiveSync") {
                    if js.type_of(f) == FUNCTION {
                        let _ = js.call(f, js.undefined(), &[]);
                    }
                }
            });
        }
    }
    nativescript_core::glib::on_signal(libc::SIGUSR1, fire);
}
