//! requestAnimationFrame on the window's frame clock; workers follow the main thread's frames.

use crate::event_loop::{current, post_state, with_js};
use crate::state::{state, State};
use nativescript_core::glib::*;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

/// Refresh interval in microseconds, from the frame clock.
static REFRESH_US: AtomicI64 = AtomicI64::new(16_667);
/// Whether the main thread's tick callback is installed.
static MAIN_TICKING: AtomicBool = AtomicBool::new(false);
/// GMainContexts of workers waiting for a frame.
static WORKERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[derive(Default)]
pub struct Frames {
    handler: RefCell<Option<v8::Global<v8::Function>>>,
    requested: Cell<bool>,
    /// GtkWidget* and id of our tick callback
    tick: Cell<(usize, u32)>,
    /// GSource* of the timer, we hold a reference
    timer: Cell<usize>,
    /// when the timer's frame is due
    due_us: Cell<i64>,
    last_us: Cell<i64>,
}

#[repr(C)]
struct GList {
    data: gpointer,
    next: *mut GList,
    prev: *mut GList,
}

type TickFn = unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean;
type DestroyFn = unsafe extern "C" fn(gpointer);

extern "C" {
    fn g_get_monotonic_time() -> i64;
    fn g_list_free(list: *mut GList);
}

struct Gtk {
    list_toplevels: unsafe extern "C" fn() -> *mut GList,
    get_mapped: unsafe extern "C" fn(gpointer) -> gboolean,
    add_tick_callback: unsafe extern "C" fn(gpointer, TickFn, gpointer, Option<DestroyFn>) -> u32,
    remove_tick_callback: unsafe extern "C" fn(gpointer, u32),
    frame_time: unsafe extern "C" fn(gpointer) -> i64,
    refresh_info: unsafe extern "C" fn(gpointer, i64, *mut i64, *mut i64),
}

/// GTK 3 or 4, once the app has loaded it.
fn gtk() -> Option<&'static Gtk> {
    static GTK: OnceLock<Gtk> = OnceLock::new();
    if let Some(gtk) = GTK.get() {
        return Some(gtk);
    }
    let sym = |name: &[u8]| unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) };
    let symbols = [
        sym(b"gtk_window_list_toplevels\0"),
        sym(b"gtk_widget_get_mapped\0"),
        sym(b"gtk_widget_add_tick_callback\0"),
        sym(b"gtk_widget_remove_tick_callback\0"),
        sym(b"gdk_frame_clock_get_frame_time\0"),
        sym(b"gdk_frame_clock_get_refresh_info\0"),
    ];
    if symbols.iter().any(|s| s.is_null()) {
        return None;
    }
    let gtk = unsafe {
        Gtk {
            list_toplevels: std::mem::transmute::<*mut c_void, _>(symbols[0]),
            get_mapped: std::mem::transmute::<*mut c_void, _>(symbols[1]),
            add_tick_callback: std::mem::transmute::<*mut c_void, _>(symbols[2]),
            remove_tick_callback: std::mem::transmute::<*mut c_void, _>(symbols[3]),
            frame_time: std::mem::transmute::<*mut c_void, _>(symbols[4]),
            refresh_info: std::mem::transmute::<*mut c_void, _>(symbols[5]),
        }
    };
    Some(GTK.get_or_init(|| gtk))
}

fn mapped_window(gtk: &Gtk) -> Option<gpointer> {
    unsafe {
        let list = (gtk.list_toplevels)();
        let mut found = None;
        let mut item = list;
        while !item.is_null() {
            if (gtk.get_mapped)((*item).data) != 0 {
                found = Some((*item).data);
                break;
            }
            item = (*item).next;
        }
        g_list_free(list);
        found
    }
}

/// Monotonic microseconds to performance.now() milliseconds.
fn to_now_ms(us: i64) -> f64 {
    static ORIGIN_US: OnceLock<i64> = OnceLock::new();
    let origin = *ORIGIN_US.get_or_init(|| unsafe {
        g_get_monotonic_time() - crate::natives::start().elapsed().as_micros() as i64
    });
    (us - origin) as f64 / 1000.0
}

fn is_worker(st: &State) -> bool {
    st.parent.borrow().is_some()
}

fn context_of(st: &State) -> usize {
    st.main_context.context
}

/// Main thread: ticks on a mapped window. Returns false without one.
fn start_ticking(st: &State) -> bool {
    if st.frames.tick.get().0 != 0 {
        return true;
    }
    let Some(gtk) = gtk() else { return false };
    let Some(window) = mapped_window(gtk) else {
        return false;
    };
    let id =
        unsafe { (gtk.add_tick_callback)(window, tick, std::ptr::null_mut(), Some(tick_removed)) };
    if id == 0 {
        return false;
    }
    st.frames.tick.set((window as usize, id));
    MAIN_TICKING.store(true, Ordering::SeqCst);
    true
}

fn start_timer(st: &State, delay_us: i64) {
    let frames = &st.frames;
    if frames.timer.get() != 0 {
        return;
    }
    let now = unsafe { g_get_monotonic_time() };
    let last = frames.last_us.get();
    // fixed cadence, so late wakeups don't drift
    let due = if last == 0 {
        now
    } else {
        (last + delay_us).max(now)
    };
    frames.due_us.set(due);
    unsafe {
        let source = g_timeout_source_new(((due - now + 999) / 1000) as u32);
        g_source_set_priority(source, G_PRIORITY_DEFAULT);
        g_source_set_callback(source, timer_fired, std::ptr::null_mut(), None);
        g_source_attach(source, context_of(st) as gpointer);
        frames.timer.set(source as usize);
    }
}

fn stop_timer(st: &State) {
    let timer = st.frames.timer.replace(0);
    if timer != 0 {
        unsafe {
            g_source_destroy(timer as gpointer);
            g_source_unref(timer as gpointer);
        }
    }
}

fn schedule(st: &State) {
    let refresh = REFRESH_US.load(Ordering::Relaxed);
    if !is_worker(st) {
        if !start_ticking(st) {
            start_timer(st, refresh);
        }
        return;
    }
    let context = context_of(st);
    {
        let mut workers = WORKERS.lock().unwrap();
        if !workers.contains(&context) {
            workers.push(context);
        }
    }
    let main_ticking = MAIN_TICKING.load(Ordering::SeqCst);
    if !main_ticking {
        let main = unsafe { g_main_context_default() } as usize;
        post_state(
            main,
            Box::new(|st| {
                if !is_worker(st) {
                    start_ticking(st);
                }
            }),
        );
    }
    // in case the main thread can't hand us one in time
    start_timer(
        st,
        if main_ticking {
            refresh + refresh / 2
        } else {
            refresh
        },
    );
}

fn dispatch(st: &State, time_us: i64) {
    let frames = &st.frames;
    frames.last_us.set(time_us);
    if !frames.requested.replace(false) {
        return;
    }
    let Some(handler) = frames.handler.borrow().clone() else {
        return;
    };
    let time = to_now_ms(time_us);
    with_js(st, |scope| {
        let handler = v8::Local::new(scope, handler);
        let time = v8::Number::new(scope, time).into();
        let recv = v8::undefined(scope).into();
        handler.call(scope, recv, &[time]);
    });
}

/// Sends this frame to the waiting workers. Returns whether there were any.
fn hand_to_workers(time_us: i64) -> bool {
    let mut workers = WORKERS.lock().unwrap();
    let any = !workers.is_empty();
    // under the lock: workers unregister before their context goes away
    for context in workers.drain(..) {
        post_state(
            context,
            Box::new(move |st| {
                stop_timer(st);
                dispatch(st, time_us);
            }),
        );
    }
    any
}

unsafe extern "C" fn tick(_widget: gpointer, clock: gpointer, _data: gpointer) -> gboolean {
    let (Some(st), Some(gtk)) = (current(), gtk()) else {
        return 0;
    };
    let time_us = (gtk.frame_time)(clock);
    let mut refresh = 0;
    let mut presentation = 0;
    (gtk.refresh_info)(clock, time_us, &mut refresh, &mut presentation);
    if refresh > 0 {
        REFRESH_US.store(refresh, Ordering::Relaxed);
    }
    let handed = hand_to_workers(time_us);
    if st.frames.requested.get() {
        dispatch(&st, time_us);
    } else {
        st.frames.last_us.set(time_us);
    }
    // workers re-request from their own thread
    (st.frames.requested.get() || handed) as gboolean
}

unsafe extern "C" fn tick_removed(_data: gpointer) {
    MAIN_TICKING.store(false, Ordering::SeqCst);
    let Some(st) = current() else { return };
    st.frames.tick.set((0, 0));
    // the window went away with a frame pending
    if st.frames.requested.get() {
        schedule(&st);
    } else if !WORKERS.lock().unwrap().is_empty() {
        start_ticking(&st);
    }
}

unsafe extern "C" fn timer_fired(_data: gpointer) -> gboolean {
    let Some(st) = current() else { return 0 };
    let source = st.frames.timer.replace(0);
    if is_worker(&st) {
        let context = context_of(&st);
        WORKERS.lock().unwrap().retain(|c| *c != context);
    }
    dispatch(&st, st.frames.due_us.get());
    if source != 0 {
        g_source_unref(source as gpointer);
    }
    if st.frames.requested.get() {
        schedule(&st);
    }
    0
}

/// Whether a frame is pending (keeps the loop alive).
pub fn pending(st: &State) -> bool {
    st.frames.requested.get()
}

/// Drops the pending frame and its sources.
pub fn cancel(st: &State) {
    let frames = &st.frames;
    frames.requested.set(false);
    stop_timer(st);
    if is_worker(st) {
        let context = context_of(st);
        WORKERS.lock().unwrap().retain(|c| *c != context);
    }
    let (widget, id) = frames.tick.replace((0, 0));
    if let (true, Some(gtk)) = (widget != 0, gtk()) {
        unsafe { (gtk.remove_tick_callback)(widget as gpointer, id) };
    }
}

fn request_frame<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    if !st.frames.requested.replace(true) {
        schedule(&st);
    }
}

fn set_frame_handler<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    if let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) {
        *st.frames.handler.borrow_mut() = Some(v8::Global::new(scope, f));
    }
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, natives: v8::Local<'s, v8::Object>) {
    for (name, f) in [
        ("requestFrame", v8::Function::new(scope, request_frame)),
        (
            "setFrameHandler",
            v8::Function::new(scope, set_frame_handler),
        ),
    ] {
        let key = v8::String::new(scope, name).unwrap();
        natives.set(scope, key.into(), f.unwrap().into());
    }
}
