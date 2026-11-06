//! GLib main loop primitives shared by the engines.

use std::ffi::c_void;

#[allow(non_camel_case_types)]
pub type gboolean = i32;
#[allow(non_camel_case_types)]
pub type gpointer = *mut c_void;
pub type SourceFunc = unsafe extern "C" fn(gpointer) -> gboolean;

pub const G_PRIORITY_DEFAULT: i32 = 0;

extern "C" {
    pub fn g_main_context_default() -> gpointer;
    pub fn g_main_context_new() -> gpointer;
    pub fn g_main_context_ref(context: gpointer) -> gpointer;
    pub fn g_main_context_unref(context: gpointer);
    pub fn g_main_context_get_thread_default() -> gpointer;
    pub fn g_main_context_push_thread_default(context: gpointer);
    pub fn g_main_context_pop_thread_default(context: gpointer);
    pub fn g_main_context_iteration(context: gpointer, may_block: gboolean) -> gboolean;
    pub fn g_main_context_invoke_full(context: gpointer, priority: i32, func: SourceFunc, data: gpointer, notify: Option<unsafe extern "C" fn(gpointer)>);
    pub fn g_timeout_source_new(interval: u32) -> gpointer;
    pub fn g_idle_source_new() -> gpointer;
    pub fn g_source_set_callback(source: gpointer, func: SourceFunc, data: gpointer, notify: Option<unsafe extern "C" fn(gpointer)>);
    pub fn g_source_set_priority(source: gpointer, priority: i32);
    pub fn g_source_attach(source: gpointer, context: gpointer) -> u32;
    pub fn g_source_destroy(source: gpointer);
    pub fn g_source_unref(source: gpointer);
    pub fn g_main_current_source() -> gpointer;
    pub fn g_source_get_context(source: gpointer) -> gpointer;
    pub fn g_unix_signal_add_full(priority: i32, signum: i32, handler: SourceFunc, data: gpointer, notify: Option<unsafe extern "C" fn(gpointer)>) -> u32;
}

/// Runs `f` on the default main context each time `signum` arrives (e.g. SIGUSR1 for livesync).
pub fn on_signal(signum: i32, f: fn()) {
    unsafe extern "C" fn run(data: gpointer) -> gboolean {
        let f: fn() = std::mem::transmute(data);
        f();
        1
    }
    unsafe { g_unix_signal_add_full(G_PRIORITY_DEFAULT, signum, run, f as gpointer, None) };
}

/// The GMainContext JS runs on for this thread: the process default on the
/// main thread (GTK needs it there), a new thread-default context elsewhere.
pub struct MainContext {
    pub context: usize,
    pushed: bool,
}

impl MainContext {
    pub fn for_current_thread() -> Self {
        unsafe {
            let is_main = libc::getpid() == libc::gettid();
            let existing = g_main_context_get_thread_default();
            if !existing.is_null() {
                return MainContext { context: g_main_context_ref(existing) as usize, pushed: false };
            }
            if is_main {
                return MainContext { context: g_main_context_ref(g_main_context_default()) as usize, pushed: false };
            }
            let context = g_main_context_new();
            g_main_context_push_thread_default(context);
            MainContext { context: context as usize, pushed: true }
        }
    }
}

impl Drop for MainContext {
    fn drop(&mut self) {
        unsafe {
            if self.pushed {
                g_main_context_pop_thread_default(self.context as gpointer);
            }
            g_main_context_unref(self.context as gpointer);
        }
    }
}

/// Always deferred, unlike g_main_context_invoke which runs inline when the
/// caller can acquire the context.
pub fn queue(context: usize, func: SourceFunc, data: gpointer) {
    unsafe {
        let source = g_idle_source_new();
        g_source_set_priority(source, G_PRIORITY_DEFAULT);
        g_source_set_callback(source, func, data, None);
        g_source_attach(source, context as gpointer);
        g_source_unref(source);
    }
}

/// Runs `f` on the thread owning `context` (from any thread).
pub fn invoke(context: usize, f: Box<dyn FnOnce() + Send>) {
    unsafe extern "C" fn run(data: gpointer) -> gboolean {
        let f = Box::from_raw(data as *mut Box<dyn FnOnce() + Send>);
        f();
        0
    }
    queue(context, run, Box::into_raw(Box::new(f)) as gpointer);
}
