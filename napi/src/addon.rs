//! The C interop as a Node-API addon.
//! `init(metadataPaths)` returns an object resolving C declarations by name, plus `interop`,
//! `pumpGLib` and `runGLib`; the host keeps its own event loop, timers and microtasks.

use crate::js::*;
use crate::state::{self, State};
use crate::sys::*;
use crate::{globals, interop};
use nativescript_core::glib::{g_main_context_iteration, gpointer};
use nativescript_metadata::{MetaFile, MetaSet};
use std::cell::Cell;
use std::ptr::null_mut;
use std::sync::Arc;

/// Unknown string keys resolve from metadata; resolved values are memoized on `target`.
const PROXY: &str = r#"(function (target, has, get) {
  return new Proxy(target, {
    has: (t, k) => Reflect.has(t, k) || (typeof k === 'string' && has(k)),
    get: (t, k, r) => (typeof k !== 'string' || Reflect.has(t, k)) ? Reflect.get(t, k, r) : get(k),
  });
})"#;

thread_local! {
    /// the module object of this thread's runtime
    static MODULE: Cell<napi_ref> = const { Cell::new(null_mut()) };
}

/// Body of `napi_register_module_v1`: exports `init`.
///
/// # Safety
/// `env` and `exports` come from the host's module registration.
pub unsafe fn register(env: napi_env, exports: napi_value) -> napi_value {
    let js = Js(env);
    js.set(exports, "init", js.function("init", Some(init), null_mut()));
    exports
}

fn strings(js: Js, array: napi_value) -> Vec<String> {
    let mut len = 0;
    unsafe { napi_get_array_length(js.0, array, &mut len) };
    (0..len)
        .filter_map(|i| {
            let mut v = null_mut();
            unsafe { napi_get_element(js.0, array, i, &mut v) };
            js.rust_string(v).ok()
        })
        .collect()
}

/// `init(metadataPaths)`: the module object, created once per thread.
unsafe extern "C" fn init(env: napi_env, info: napi_callback_info) -> napi_value {
    let js = Js(env);
    let existing = MODULE.with(Cell::get);
    if !existing.is_null() {
        return js.deref(existing);
    }
    let (a, ..) = cb_args::<1>(env, info);
    let mut files = Vec::new();
    for path in strings(js, a[0]) {
        match MetaFile::open(&path) {
            Ok(f) => files.push(f),
            Err(e) => {
                js.throw(&format!("{path}: {e}"));
                return null_mut();
            }
        }
    }
    if files.is_empty() {
        js.throw("init() needs at least one metadata file");
        return null_mut();
    }
    // lives as long as the thread's env
    let st: &'static State = Box::leak(Box::new(State::new(Arc::new(MetaSet::new(files)), env)));
    state::set_current(st);

    let target = js.object();
    st.globals_target.set(js.reference(target, 1));
    interop::install(st, target);
    for (name, cb) in [
        ("pumpGLib", pump_glib as unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value),
        ("runGLib", glib_loop::run),
        ("stopGLib", glib_loop::stop),
    ] {
        js.set(target, name, js.function(name, Some(cb), null_mut()));
    }
    let Ok(factory) = crate::run_script(js, PROXY, "nativescript:addon") else {
        return null_mut();
    };
    let has = js.function("has", Some(globals::lookup_has), null_mut());
    let get = js.function("get", Some(globals::lookup_get), null_mut());
    let Ok(module) = js.call(factory, js.undefined(), &[target, has, get]) else {
        return null_mut();
    };
    MODULE.with(|m| m.set(js.reference(module, 1)));
    module
}

/// `pumpGLib()`: dispatches what's ready on this thread's GLib main context without blocking.
/// True if anything ran.
unsafe extern "C" fn pump_glib(env: napi_env, _info: napi_callback_info) -> napi_value {
    let st = state::state(env);
    let context = st.main_context.context as gpointer;
    let mut ran = false;
    // bounded: a source that keeps itself ready can't starve the host
    for _ in 0..1000 {
        if g_main_context_iteration(context, 0) == 0 {
            break;
        }
        ran = true;
    }
    st.js().boolean(ran)
}

/// GLib's main context driven by the host's libuv loop: GLib's fds become uv polls and its next
/// timeout a uv timer, so sources dispatch as soon as they're ready (frame clock, input, timeouts)
/// and an idle app doesn't wake up.
mod glib_loop {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::{c_int, c_void, CStr};
    use std::sync::OnceLock;

    type Handle = *mut c_void;
    type Cb = unsafe extern "C" fn(Handle);
    type PollCb = unsafe extern "C" fn(Handle, c_int, c_int);

    const UV_CHECK: c_int = 2;
    const UV_POLL: c_int = 8;
    const UV_PREPARE: c_int = 9;
    const UV_TIMER: c_int = 13;
    const UV_READABLE: c_int = 1;
    const UV_WRITABLE: c_int = 2;

    const G_IO_IN: u16 = 1;
    const G_IO_OUT: u16 = 4;
    const G_IO_PRI: u16 = 2;
    const G_IO_ERR: u16 = 8;
    const G_IO_HUP: u16 = 16;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct GPollFd {
        fd: c_int,
        events: u16,
        revents: u16,
    }

    extern "C" {
        fn g_main_context_acquire(context: gpointer) -> c_int;
        fn g_main_context_release(context: gpointer);
        fn g_main_context_prepare(context: gpointer, priority: *mut c_int) -> c_int;
        fn g_main_context_query(
            context: gpointer,
            max_priority: c_int,
            timeout: *mut c_int,
            fds: *mut GPollFd,
            n_fds: c_int,
        ) -> c_int;
        fn g_main_context_check(context: gpointer, max_priority: c_int, fds: *mut GPollFd, n: c_int)
            -> c_int;
        fn g_main_context_dispatch(context: gpointer);
        fn napi_get_uv_event_loop(env: napi_env, uv_loop: *mut *mut c_void) -> napi_status;
        fn napi_async_init(
            env: napi_env,
            resource: napi_value,
            name: napi_value,
            result: *mut *mut c_void,
        ) -> napi_status;
        fn napi_open_callback_scope(
            env: napi_env,
            resource: napi_value,
            context: *mut c_void,
            result: *mut *mut c_void,
        ) -> napi_status;
        fn napi_close_callback_scope(env: napi_env, scope: *mut c_void) -> napi_status;
        fn napi_async_destroy(env: napi_env, context: *mut c_void) -> napi_status;
    }

    /// libuv as exported by the host, looked up so hosts without it can still load the addon.
    struct Uv {
        handle_size: unsafe extern "C" fn(c_int) -> usize,
        prepare_init: unsafe extern "C" fn(*mut c_void, Handle) -> c_int,
        prepare_start: unsafe extern "C" fn(Handle, Cb) -> c_int,
        check_init: unsafe extern "C" fn(*mut c_void, Handle) -> c_int,
        check_start: unsafe extern "C" fn(Handle, Cb) -> c_int,
        timer_init: unsafe extern "C" fn(*mut c_void, Handle) -> c_int,
        timer_start: unsafe extern "C" fn(Handle, Cb, u64, u64) -> c_int,
        timer_stop: unsafe extern "C" fn(Handle) -> c_int,
        poll_init: unsafe extern "C" fn(*mut c_void, Handle, c_int) -> c_int,
        poll_start: unsafe extern "C" fn(Handle, c_int, PollCb) -> c_int,
        close: unsafe extern "C" fn(Handle, Option<Cb>),
        unref: unsafe extern "C" fn(Handle),
    }

    fn uv() -> Option<&'static Uv> {
        static UV: OnceLock<Option<Uv>> = OnceLock::new();
        UV.get_or_init(|| {
            fn sym<T>(name: &CStr) -> Option<T> {
                let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
                (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
            }
            Some(Uv {
                handle_size: sym(c"uv_handle_size")?,
                prepare_init: sym(c"uv_prepare_init")?,
                prepare_start: sym(c"uv_prepare_start")?,
                check_init: sym(c"uv_check_init")?,
                check_start: sym(c"uv_check_start")?,
                timer_init: sym(c"uv_timer_init")?,
                timer_start: sym(c"uv_timer_start")?,
                timer_stop: sym(c"uv_timer_stop")?,
                poll_init: sym(c"uv_poll_init")?,
                poll_start: sym(c"uv_poll_start")?,
                close: sym(c"uv_close")?,
                unref: sym(c"uv_unref")?,
            })
        })
        .as_ref()
    }

    struct Pump {
        env: napi_env,
        context: gpointer,
        uv_loop: *mut c_void,
        prepare: Handle,
        check: Handle,
        timer: Handle,
        /// fd -> uv poll handle and the uv events it watches
        polls: HashMap<c_int, (Handle, c_int)>,
        /// GLib's poll set for this iteration, revents filled from the uv polls
        fds: Vec<GPollFd>,
        n_fds: usize,
        max_priority: c_int,
        resource: napi_ref,
        async_context: *mut c_void,
    }

    thread_local! {
        static PUMP: Cell<*mut Pump> = const { Cell::new(null_mut()) };
    }

    fn handle(uv: &Uv, kind: c_int, pump: *mut Pump) -> Handle {
        let size = unsafe { (uv.handle_size)(kind) };
        let h = unsafe { libc::calloc(1, size) };
        // uv_handle_t starts with `void* data`
        unsafe { *(h as *mut *mut Pump) = pump };
        h
    }

    unsafe extern "C" fn free_handle(h: Handle) {
        libc::free(h);
    }

    /// Asks GLib what to wait for and mirrors it into uv polls and the timer.
    unsafe extern "C" fn prepare(h: Handle) {
        let p = &mut **(h as *mut *mut Pump);
        let Some(uv) = uv() else { return };
        let ready = g_main_context_prepare(p.context, &mut p.max_priority) != 0;
        let mut timeout: c_int = -1;
        loop {
            let n = g_main_context_query(
                p.context,
                p.max_priority,
                &mut timeout,
                p.fds.as_mut_ptr(),
                p.fds.len() as c_int,
            ) as usize;
            if n <= p.fds.len() {
                p.n_fds = n;
                break;
            }
            p.fds.resize(n, GPollFd { fd: -1, events: 0, revents: 0 });
        }
        let mut wanted: HashMap<c_int, c_int> = HashMap::new();
        for f in &mut p.fds[..p.n_fds] {
            f.revents = 0;
            let mut events = 0;
            if f.events & (G_IO_IN | G_IO_PRI | G_IO_HUP | G_IO_ERR) != 0 {
                events |= UV_READABLE;
            }
            if f.events & G_IO_OUT != 0 {
                events |= UV_WRITABLE;
            }
            *wanted.entry(f.fd).or_insert(0) |= events;
        }
        p.polls.retain(|fd, (poll, _)| {
            let keep = wanted.contains_key(fd);
            if !keep {
                (uv.close)(*poll, Some(free_handle));
            }
            keep
        });
        let pump = p as *mut Pump;
        for (fd, events) in wanted {
            let entry = p.polls.entry(fd).or_insert_with(|| {
                let poll = handle(uv, UV_POLL, pump);
                (uv.poll_init)(p.uv_loop, poll, fd);
                (uv.unref)(poll);
                (poll, 0)
            });
            if entry.1 != events {
                (uv.poll_start)(entry.0, events, polled);
                entry.1 = events;
            }
        }
        if ready {
            timeout = 0;
        }
        (uv.timer_stop)(p.timer);
        if timeout >= 0 {
            (uv.timer_start)(p.timer, woken, timeout as u64, 0);
        }
    }

    unsafe extern "C" fn woken(_: Handle) {}

    unsafe extern "C" fn polled(h: Handle, status: c_int, events: c_int) {
        let p = &mut **(h as *mut *mut Pump);
        let Some((&fd, _)) = p.polls.iter().find(|(_, (poll, _))| *poll == h) else {
            return;
        };
        for f in p.fds[..p.n_fds].iter_mut().filter(|f| f.fd == fd) {
            if status < 0 {
                f.revents |= G_IO_ERR;
            }
            if events & UV_READABLE != 0 {
                f.revents |= f.events & (G_IO_IN | G_IO_PRI | G_IO_HUP | G_IO_ERR);
            }
            if events & UV_WRITABLE != 0 {
                f.revents |= f.events & G_IO_OUT;
            }
        }
    }

    /// After uv's poll phase: dispatch what GLib finds ready, as a host callback (microtasks
    /// and process.nextTick run after it).
    unsafe extern "C" fn check(h: Handle) {
        let p = &mut **(h as *mut *mut Pump);
        if g_main_context_check(p.context, p.max_priority, p.fds.as_mut_ptr(), p.n_fds as c_int) == 0 {
            return;
        }
        let js = Js(p.env);
        let mut scope = null_mut();
        napi_open_handle_scope(p.env, &mut scope);
        let resource = js.deref(p.resource);
        let mut callback_scope = null_mut();
        napi_open_callback_scope(p.env, resource, p.async_context, &mut callback_scope);
        g_main_context_dispatch(p.context);
        napi_close_callback_scope(p.env, callback_scope);
        napi_close_handle_scope(p.env, scope);
    }

    /// `runGLib()`: drives this thread's GLib main context from the host's event loop and keeps
    /// the process alive until `stopGLib()`. False when the host has no libuv loop.
    pub unsafe extern "C" fn run(env: napi_env, _info: napi_callback_info) -> napi_value {
        let st = state::state(env);
        let js = st.js();
        if !PUMP.with(Cell::get).is_null() {
            return js.boolean(true);
        }
        let mut uv_loop = null_mut();
        let Some(uv) = uv().filter(|_| napi_get_uv_event_loop(env, &mut uv_loop) == 0) else {
            return js.boolean(false);
        };
        let context = st.main_context.context as gpointer;
        g_main_context_acquire(context);
        let resource = js.object();
        let mut async_context = null_mut();
        napi_async_init(env, resource, js.string("GLib"), &mut async_context);
        let p = Box::into_raw(Box::new(Pump {
            env,
            context,
            uv_loop,
            prepare: null_mut(),
            check: null_mut(),
            timer: null_mut(),
            polls: HashMap::new(),
            fds: vec![GPollFd { fd: -1, events: 0, revents: 0 }; 16],
            n_fds: 0,
            max_priority: 0,
            resource: js.reference(resource, 1),
            async_context,
        }));
        let pump = &mut *p;
        pump.prepare = handle(uv, UV_PREPARE, p);
        pump.check = handle(uv, UV_CHECK, p);
        pump.timer = handle(uv, UV_TIMER, p);
        (uv.prepare_init)(uv_loop, pump.prepare);
        (uv.check_init)(uv_loop, pump.check);
        (uv.timer_init)(uv_loop, pump.timer);
        (uv.unref)(pump.check);
        (uv.unref)(pump.timer);
        // the prepare handle stays referenced: it keeps the process alive
        (uv.prepare_start)(pump.prepare, prepare);
        (uv.check_start)(pump.check, check);
        PUMP.with(|c| c.set(p));
        js.boolean(true)
    }

    /// `stopGLib()`: hands GLib's main context back; the process can exit again.
    pub unsafe extern "C" fn stop(env: napi_env, _info: napi_callback_info) -> napi_value {
        let js = Js(env);
        let p = PUMP.with(|c| c.replace(null_mut()));
        if p.is_null() {
            return js.undefined();
        }
        let Some(uv) = uv() else { return js.undefined() };
        let pump = Box::from_raw(p);
        for h in [pump.prepare, pump.check, pump.timer]
            .into_iter()
            .chain(pump.polls.values().map(|(h, _)| *h))
        {
            (uv.close)(h, Some(free_handle));
        }
        g_main_context_release(pump.context);
        napi_async_destroy(env, pump.async_context);
        js.delete_ref(pump.resource);
        js.undefined()
    }
}
