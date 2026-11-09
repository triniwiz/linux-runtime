//! V8 foreground tasks as GLib sources on the isolate's main context. Tasks posted from V8's
//! worker threads (GC finalization, the memory reducer, compile results) wake the loop instead
//! of waiting for the next JS call.

use crate::event_loop::{current, with_js};
use nativescript_core::glib::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Registry {
    /// isolate -> GMainContext of its thread
    contexts: HashMap<usize, usize>,
    /// posted before the isolate was registered
    early: HashMap<usize, Vec<(Box<Pending>, f64)>>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Default::default);

struct Pending {
    isolate: usize,
    task: Option<v8::Task>,
    nestable: bool,
}

pub struct Platform;

impl v8::PlatformImpl for Platform {
    fn post_task(&self, isolate: *mut c_void, task: v8::Task) {
        post(isolate, task, 0.0, true);
    }

    fn post_non_nestable_task(&self, isolate: *mut c_void, task: v8::Task) {
        post(isolate, task, 0.0, false);
    }

    fn post_delayed_task(&self, isolate: *mut c_void, task: v8::Task, delay: f64) {
        post(isolate, task, delay, true);
    }

    fn post_non_nestable_delayed_task(&self, isolate: *mut c_void, task: v8::Task, delay: f64) {
        post(isolate, task, delay, false);
    }
}

fn post(isolate: *mut c_void, task: v8::Task, delay: f64, nestable: bool) {
    let isolate = isolate as usize;
    let pending = Box::new(Pending {
        isolate,
        task: Some(task),
        nestable,
    });
    let mut registry = REGISTRY.lock().unwrap();
    match registry.contexts.get(&isolate) {
        Some(&context) => attach(context, pending, delay),
        None => registry
            .early
            .entry(isolate)
            .or_default()
            .push((pending, delay)),
    }
}

/// The C++ isolate address V8 passes to the platform.
fn key(isolate: &v8::Isolate) -> usize {
    let raw = unsafe { isolate.as_raw_isolate_ptr() };
    unsafe { std::mem::transmute::<v8::UnsafeRawIsolatePtr, *mut c_void>(raw) as usize }
}

/// Routes `isolate`'s tasks to `context` (kept alive) until the registration drops.
pub struct Registration {
    isolate: usize,
    context: usize,
}

pub fn register(isolate: &v8::Isolate, context: usize) -> Registration {
    let isolate = key(isolate);
    unsafe { g_main_context_ref(context as gpointer) };
    let mut registry = REGISTRY.lock().unwrap();
    registry.contexts.insert(isolate, context);
    for (pending, delay) in registry.early.remove(&isolate).unwrap_or_default() {
        attach(context, pending, delay);
    }
    Registration { isolate, context }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let early = {
            let mut registry = REGISTRY.lock().unwrap();
            registry.contexts.remove(&self.isolate);
            registry.early.remove(&self.isolate)
        };
        drop(early); // outside the lock
                     // destroys the sources still holding its tasks
        unsafe { g_main_context_unref(self.context as gpointer) };
    }
}

fn attach(context: usize, pending: Box<Pending>, delay: f64) {
    unsafe {
        let source = if delay > 0.0 {
            g_timeout_source_new((delay * 1000.0).ceil().min(u32::MAX as f64) as u32)
        } else {
            g_idle_source_new()
        };
        g_source_set_priority(source, G_PRIORITY_DEFAULT);
        g_source_set_callback(source, run, Box::into_raw(pending) as gpointer, Some(free));
        g_source_attach(source, context as gpointer);
        g_source_unref(source);
    }
}

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// non-nestable tasks that came up while another task ran
    static DEFERRED: RefCell<Vec<Box<Pending>>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "C" fn run(data: gpointer) -> gboolean {
    let pending = &mut *(data as *mut Pending);
    // a task of a disposed isolate is dropped by `free`
    let Some(st) = current().filter(|st| key(&*st.isolate.get()) == pending.isolate) else {
        return 0;
    };
    let Some(task) = pending.task.take() else {
        return 0;
    };
    if !pending.nestable && RUNNING.get() {
        DEFERRED.with_borrow_mut(|d| {
            d.push(Box::new(Pending {
                isolate: pending.isolate,
                task: Some(task),
                nestable: false,
            }))
        });
        return 0;
    }
    let outer = RUNNING.replace(true);
    with_js(&st, |_| task.run());
    RUNNING.set(outer);
    if outer {
        return 0;
    }
    for pending in DEFERRED.take() {
        attach(st.main_context.context, pending, 0.0);
    }
    0
}

unsafe extern "C" fn free(data: gpointer) {
    drop(Box::from_raw(data as *mut Pending));
}
