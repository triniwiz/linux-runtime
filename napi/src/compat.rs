//! node_api.h functions the engine shims don't provide: async work,
//! threadsafe functions, cleanup hooks, module registration and newer APIs.

#![allow(non_camel_case_types)]

use crate::event_loop::{hold, post};
use crate::js::Js;
use crate::state::current;
use crate::sys::*;
use std::collections::VecDeque;
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Condvar, Mutex, OnceLock};

const OK: napi_status = 0;
const INVALID_ARG: napi_status = 1;
const FUNCTION_EXPECTED: napi_status = 5;
const GENERIC_FAILURE: napi_status = 9;
const CANCELLED: napi_status = 11;
const QUEUE_FULL: napi_status = 15;
const CLOSING: napi_status = 16;

macro_rules! arg {
    ($($p:expr),+) => { $( if $p.is_null() { return INVALID_ARG; } )+ };
}

fn main_context() -> usize {
    current().map(|st| st.main_context.context).unwrap_or(0)
}

fn release_loop() {
    if let Some(st) = current() {
        st.loop_refs.set((st.loop_refs.get() - 1).max(0));
    }
}

#[repr(C)]
pub struct napi_module {
    nm_version: i32,
    nm_flags: u32,
    nm_filename: *const c_char,
    nm_register_func: Option<crate::modules::AddonInit>,
    nm_modname: *const c_char,
    nm_priv: *mut c_void,
    reserved: [*mut c_void; 4],
}

#[no_mangle]
pub unsafe extern "C" fn napi_module_register(module: *mut napi_module) {
    if let Some(f) = module.as_ref().and_then(|m| m.nm_register_func) {
        crate::modules::PENDING_MODULE.with(|m| m.set(Some(f)));
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_version(env: napi_env, result: *mut u32) -> napi_status {
    arg!(env, result);
    *result = 9;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_fatal_error(
    location: *const c_char,
    location_len: usize,
    message: *const c_char,
    message_len: usize,
) -> ! {
    let text = |p: *const c_char, len: usize| -> String {
        if p.is_null() {
            return String::new();
        }
        let len = if len == usize::MAX {
            libc::strlen(p)
        } else {
            len
        };
        String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, len)).into_owned()
    };
    eprintln!(
        "FATAL ERROR: {} {}",
        text(location, location_len),
        text(message, message_len)
    );
    std::process::abort();
}

#[no_mangle]
pub unsafe extern "C" fn napi_fatal_exception(env: napi_env, err: napi_value) -> napi_status {
    arg!(env);
    eprintln!("Uncaught {}", Js(env).describe(err));
    if let Some(st) = current() {
        st.uncaught.set(st.uncaught.get() + 1);
    }
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_async_init(
    env: napi_env,
    _resource: napi_value,
    _name: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    arg!(env, result);
    *result = 1 as *mut c_void;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_async_destroy(env: napi_env, _context: *mut c_void) -> napi_status {
    arg!(env);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_make_callback(
    env: napi_env,
    _context: *mut c_void,
    recv: napi_value,
    func: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    napi_call_function(env, recv, func, argc, argv, result)
}

#[no_mangle]
pub unsafe extern "C" fn napi_open_callback_scope(
    env: napi_env,
    _resource: napi_value,
    _context: *mut c_void,
    result: *mut *mut c_void,
) -> napi_status {
    arg!(env, result);
    *result = 1 as *mut c_void;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_close_callback_scope(
    env: napi_env,
    _scope: *mut c_void,
) -> napi_status {
    arg!(env);
    OK
}

type Job = Box<dyn FnOnce() + Send>;

/// Worker pool shared by async work.
fn pool() -> &'static Mutex<Sender<Job>> {
    static POOL: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = channel::<Job>();
        let rx = std::sync::Arc::new(Mutex::new(rx));
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 16);
        for i in 0..n {
            let rx = rx.clone();
            std::thread::Builder::new()
                .name(format!("napi-worker-{i}"))
                .spawn(move || loop {
                    let job = rx.lock().unwrap().recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => return,
                    }
                })
                .expect("napi worker thread");
        }
        Mutex::new(tx)
    })
}

const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const RUNNING: u8 = 2;
const CANCELLED_STATE: u8 = 3;

pub struct AsyncWork {
    env: napi_env,
    main_context: usize,
    execute: unsafe extern "C" fn(napi_env, *mut c_void),
    complete: Option<unsafe extern "C" fn(napi_env, napi_status, *mut c_void)>,
    data: *mut c_void,
    state: AtomicU8,
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_async_work(
    env: napi_env,
    _resource: napi_value,
    _name: napi_value,
    execute: Option<unsafe extern "C" fn(napi_env, *mut c_void)>,
    complete: Option<unsafe extern "C" fn(napi_env, napi_status, *mut c_void)>,
    data: *mut c_void,
    result: *mut *mut AsyncWork,
) -> napi_status {
    arg!(env, result);
    let Some(execute) = execute else {
        return INVALID_ARG;
    };
    let work = AsyncWork {
        env,
        main_context: main_context(),
        execute,
        complete,
        data,
        state: AtomicU8::new(IDLE),
    };
    *result = Box::into_raw(Box::new(work));
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_delete_async_work(
    env: napi_env,
    work: *mut AsyncWork,
) -> napi_status {
    arg!(env, work);
    drop(Box::from_raw(work));
    OK
}

fn complete_on_js(context: usize, work: usize, status: napi_status) {
    post(
        context,
        true,
        Box::new(move |_| {
            // complete() usually deletes the work: copy what we need first
            let (env, complete, data) = {
                let w = unsafe { &*(work as *const AsyncWork) };
                (w.env, w.complete, w.data)
            };
            if let Some(complete) = complete {
                unsafe { complete(env, status, data) };
            }
        }),
    );
}

#[no_mangle]
pub unsafe extern "C" fn napi_queue_async_work(env: napi_env, work: *mut AsyncWork) -> napi_status {
    arg!(env, work);
    if (*work).state.swap(QUEUED, Ordering::SeqCst) == QUEUED {
        return GENERIC_FAILURE;
    }
    if let Some(st) = current() {
        hold(st);
    }
    let context = (*work).main_context;
    let work = work as usize;
    let job: Job = Box::new(move || {
        let w = unsafe { &*(work as *const AsyncWork) };
        if w.state
            .compare_exchange(QUEUED, RUNNING, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return; // cancelled, completion already posted
        }
        unsafe { (w.execute)(w.env, w.data) };
        w.state.store(IDLE, Ordering::SeqCst);
        complete_on_js(context, work, OK);
    });
    let _ = pool().lock().unwrap().send(job);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_cancel_async_work(
    env: napi_env,
    work: *mut AsyncWork,
) -> napi_status {
    arg!(env, work);
    if (*work)
        .state
        .compare_exchange(QUEUED, CANCELLED_STATE, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return GENERIC_FAILURE;
    }
    complete_on_js((*work).main_context, work as usize, CANCELLED);
    OK
}

pub struct Tsfn {
    env: usize,
    main_context: usize,
    /// napi_ref, JS thread only
    func: usize,
    context: usize,
    call_js: Option<unsafe extern "C" fn(napi_env, napi_value, *mut c_void, *mut c_void)>,
    finalize_cb: napi_finalize,
    finalize_data: usize,
    queue: Mutex<VecDeque<usize>>,
    space: Condvar,
    max_queue: usize,
    threads: Mutex<usize>,
    closing: AtomicBool,
    scheduled: AtomicBool,
    /// keeps the loop alive, JS thread only
    refd: AtomicBool,
}

impl Tsfn {
    fn schedule(&self) {
        if self.scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let ptr = self as *const Tsfn as usize;
        post(
            self.main_context,
            false,
            Box::new(move |_| unsafe { dispatch(ptr) }),
        );
    }
}

unsafe fn dispatch(ptr: usize) {
    let t = &*(ptr as *const Tsfn);
    t.scheduled.store(false, Ordering::SeqCst);
    let items: Vec<usize> = t.queue.lock().unwrap().drain(..).collect();
    t.space.notify_all();
    let env = t.env as napi_env;
    let js = Js(env);
    let func = if t.func == 0 {
        std::ptr::null_mut()
    } else {
        js.deref(t.func as napi_ref)
    };
    for data in items {
        match t.call_js {
            Some(cb) => cb(env, func, t.context as *mut c_void, data as *mut c_void),
            None if !func.is_null() => {
                let _ = js.call(func, js.undefined(), &[]);
            }
            None => {}
        }
        if js.is_exception_pending() {
            return;
        }
    }
    if t.closing.load(Ordering::SeqCst)
        && *t.threads.lock().unwrap() == 0
        && t.queue.lock().unwrap().is_empty()
    {
        finalize(ptr);
    }
}

unsafe fn finalize(ptr: usize) {
    let t = Box::from_raw(ptr as *mut Tsfn);
    let env = t.env as napi_env;
    if let Some(cb) = t.finalize_cb {
        cb(
            env,
            t.finalize_data as *mut c_void,
            t.context as *mut c_void,
        );
    }
    Js(env).delete_ref(t.func as napi_ref);
    if t.refd.load(Ordering::Relaxed) {
        release_loop();
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_threadsafe_function(
    env: napi_env,
    func: napi_value,
    _resource: napi_value,
    _name: napi_value,
    max_queue_size: usize,
    initial_thread_count: usize,
    finalize_data: *mut c_void,
    finalize_cb: napi_finalize,
    context: *mut c_void,
    call_js: Option<unsafe extern "C" fn(napi_env, napi_value, *mut c_void, *mut c_void)>,
    result: *mut *const Tsfn,
) -> napi_status {
    arg!(env, result);
    if initial_thread_count == 0 {
        return INVALID_ARG;
    }
    let js = Js(env);
    let func = if func.is_null() {
        if call_js.is_none() {
            return INVALID_ARG;
        }
        0
    } else if js.type_of(func) != crate::js::FUNCTION {
        return FUNCTION_EXPECTED;
    } else {
        js.reference(func, 1) as usize
    };
    let t = Box::new(Tsfn {
        env: env as usize,
        main_context: main_context(),
        func,
        context: context as usize,
        call_js,
        finalize_cb,
        finalize_data: finalize_data as usize,
        queue: Mutex::new(VecDeque::new()),
        space: Condvar::new(),
        max_queue: max_queue_size,
        threads: Mutex::new(initial_thread_count),
        closing: AtomicBool::new(false),
        scheduled: AtomicBool::new(false),
        refd: AtomicBool::new(true),
    });
    if let Some(st) = current() {
        hold(st);
    }
    *result = Box::into_raw(t);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_threadsafe_function_context(
    func: *const Tsfn,
    result: *mut *mut c_void,
) -> napi_status {
    arg!(func, result);
    *result = (*func).context as *mut c_void;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_call_threadsafe_function(
    func: *const Tsfn,
    data: *mut c_void,
    blocking: i32,
) -> napi_status {
    let Some(t) = func.as_ref() else {
        return INVALID_ARG;
    };
    let mut queue = t.queue.lock().unwrap();
    while t.max_queue > 0 && queue.len() >= t.max_queue {
        if t.closing.load(Ordering::SeqCst) {
            return CLOSING;
        }
        if blocking == 0 {
            return QUEUE_FULL;
        }
        queue = t.space.wait(queue).unwrap();
    }
    if t.closing.load(Ordering::SeqCst) {
        return CLOSING;
    }
    queue.push_back(data as usize);
    drop(queue);
    t.schedule();
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_acquire_threadsafe_function(func: *const Tsfn) -> napi_status {
    let Some(t) = func.as_ref() else {
        return INVALID_ARG;
    };
    if t.closing.load(Ordering::SeqCst) {
        return CLOSING;
    }
    *t.threads.lock().unwrap() += 1;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_release_threadsafe_function(
    func: *const Tsfn,
    mode: i32,
) -> napi_status {
    let Some(t) = func.as_ref() else {
        return INVALID_ARG;
    };
    {
        let mut threads = t.threads.lock().unwrap();
        if *threads == 0 {
            return INVALID_ARG;
        }
        *threads -= 1;
        if *threads == 0 || mode == 1 {
            t.closing.store(true, Ordering::SeqCst);
            if mode == 1 {
                t.queue.lock().unwrap().clear();
            }
        }
    }
    t.space.notify_all();
    if t.closing.load(Ordering::SeqCst) {
        t.schedule();
    }
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_unref_threadsafe_function(
    env: napi_env,
    func: *const Tsfn,
) -> napi_status {
    arg!(env, func);
    if (*func).refd.swap(false, Ordering::Relaxed) {
        release_loop();
    }
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_ref_threadsafe_function(
    env: napi_env,
    func: *const Tsfn,
) -> napi_status {
    arg!(env, func);
    if !(*func).refd.swap(true, Ordering::Relaxed) {
        if let Some(st) = current() {
            hold(st);
        }
    }
    OK
}

#[repr(C)]
pub struct napi_node_version {
    major: u32,
    minor: u32,
    patch: u32,
    release: *const c_char,
}
struct SyncVersion(napi_node_version);
unsafe impl Sync for SyncVersion {}
static NODE_VERSION: SyncVersion = SyncVersion(napi_node_version {
    major: 22,
    minor: 0,
    patch: 0,
    release: c"node".as_ptr(),
});

#[no_mangle]
pub unsafe extern "C" fn napi_get_node_version(
    env: napi_env,
    version: *mut *const napi_node_version,
) -> napi_status {
    arg!(env, version);
    *version = &NODE_VERSION.0;
    OK
}

/// No libuv here: a stable dummy pointer for addons that only check it.
#[no_mangle]
pub unsafe extern "C" fn napi_get_uv_event_loop(
    env: napi_env,
    result: *mut *mut c_void,
) -> napi_status {
    static DUMMY: [u64; 64] = [0; 64];
    arg!(env, result);
    *result = DUMMY.as_ptr() as *mut c_void;
    OK
}

pub type CleanupHook = unsafe extern "C" fn(*mut c_void);

#[no_mangle]
pub unsafe extern "C" fn napi_add_env_cleanup_hook(
    env: napi_env,
    fun: Option<CleanupHook>,
    arg: *mut c_void,
) -> napi_status {
    arg!(env);
    let (Some(fun), Some(st)) = (fun, current()) else {
        return INVALID_ARG;
    };
    st.cleanup_hooks.borrow_mut().push((fun, arg as usize));
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_remove_env_cleanup_hook(
    env: napi_env,
    fun: Option<CleanupHook>,
    arg: *mut c_void,
) -> napi_status {
    arg!(env);
    let (Some(fun), Some(st)) = (fun, current()) else {
        return INVALID_ARG;
    };
    st.cleanup_hooks
        .borrow_mut()
        .retain(|(f, a)| !(*f as usize == fun as usize && *a == arg as usize));
    OK
}

pub struct AsyncCleanupHook {
    hook: unsafe extern "C" fn(*mut AsyncCleanupHook, *mut c_void),
    arg: *mut c_void,
}

unsafe extern "C" fn run_async_hook(data: *mut c_void) {
    let h = data as *mut AsyncCleanupHook;
    ((*h).hook)(h, (*h).arg);
}

#[no_mangle]
pub unsafe extern "C" fn napi_add_async_cleanup_hook(
    env: napi_env,
    hook: Option<unsafe extern "C" fn(*mut AsyncCleanupHook, *mut c_void)>,
    arg: *mut c_void,
    remove_handle: *mut *mut AsyncCleanupHook,
) -> napi_status {
    arg!(env);
    let (Some(hook), Some(st)) = (hook, current()) else {
        return INVALID_ARG;
    };
    let h = Box::into_raw(Box::new(AsyncCleanupHook { hook, arg }));
    st.cleanup_hooks
        .borrow_mut()
        .push((run_async_hook, h as usize));
    if !remove_handle.is_null() {
        *remove_handle = h;
    }
    OK
}

/// The handle stays valid until teardown; removing just skips the hook.
#[no_mangle]
pub unsafe extern "C" fn napi_remove_async_cleanup_hook(
    handle: *mut AsyncCleanupHook,
) -> napi_status {
    arg!(handle);
    unsafe extern "C" fn noop(_: *mut AsyncCleanupHook, _: *mut c_void) {}
    (*handle).hook = noop;
    OK
}

/// File URL of the addon being loaded (all addons share one env).
#[no_mangle]
pub unsafe extern "C" fn node_api_get_module_file_name(
    env: napi_env,
    result: *mut *const c_char,
) -> napi_status {
    arg!(env, result);
    let Some(st) = current() else {
        return GENERIC_FAILURE;
    };
    *result = st.module_file.borrow().as_ptr();
    OK
}

/// Runs `finalize_cb` from the event loop instead of the GC.
#[no_mangle]
pub unsafe extern "C" fn node_api_post_finalizer(
    env: napi_env,
    finalize_cb: napi_finalize,
    data: *mut c_void,
    hint: *mut c_void,
) -> napi_status {
    arg!(env);
    let Some(cb) = finalize_cb else {
        return INVALID_ARG;
    };
    let (env, data, hint) = (env as usize, data as usize, hint as usize);
    post(
        main_context(),
        false,
        Box::new(move |_| unsafe { cb(env as napi_env, data as *mut c_void, hint as *mut c_void) }),
    );
    OK
}

/// Strings are copied (`copied` = true): the caller keeps ownership.
#[no_mangle]
pub unsafe extern "C" fn node_api_create_external_string_latin1(
    env: napi_env,
    str: *mut c_char,
    length: usize,
    _finalize_cb: napi_finalize,
    _hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    if !copied.is_null() {
        *copied = true;
    }
    napi_create_string_latin1(env, str, length, result)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_external_string_utf16(
    env: napi_env,
    str: *mut u16,
    length: usize,
    _finalize_cb: napi_finalize,
    _hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    if !copied.is_null() {
        *copied = true;
    }
    napi_create_string_utf16(env, str, length, result)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_property_key_utf16(
    env: napi_env,
    str: *const u16,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    napi_create_string_utf16(env, str, length, result)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_syntax_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
) -> napi_status {
    arg!(env, msg, result);
    let js = Js(env);
    let Ok(ctor) = js.get(js.global(), "SyntaxError") else {
        return GENERIC_FAILURE;
    };
    let Ok(error) = js.new_instance(ctor, &[msg]) else {
        return GENERIC_FAILURE;
    };
    if !code.is_null() {
        js.set(error, "code", code);
    }
    *result = error;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn node_api_throw_syntax_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    arg!(env, msg);
    let js = Js(env);
    let text = |p: *const c_char| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
    let code = if code.is_null() {
        std::ptr::null_mut()
    } else {
        js.string(&text(code))
    };
    let mut error = std::ptr::null_mut();
    let status = node_api_create_syntax_error(env, code, js.string(&text(msg)), &mut error);
    if status != OK {
        return status;
    }
    napi_throw(env, error)
}

/// Referenced so the linker keeps every export.
pub struct Symbols(pub &'static [*const ()]);
unsafe impl Sync for Symbols {}
#[used]
pub static SYMBOLS: Symbols = Symbols(&[
    napi_module_register as *const (),
    napi_get_version as *const (),
    napi_fatal_error as *const (),
    napi_fatal_exception as *const (),
    napi_async_init as *const (),
    napi_async_destroy as *const (),
    napi_make_callback as *const (),
    napi_open_callback_scope as *const (),
    napi_close_callback_scope as *const (),
    napi_create_async_work as *const (),
    napi_delete_async_work as *const (),
    napi_queue_async_work as *const (),
    napi_cancel_async_work as *const (),
    napi_create_threadsafe_function as *const (),
    napi_get_threadsafe_function_context as *const (),
    napi_call_threadsafe_function as *const (),
    napi_acquire_threadsafe_function as *const (),
    napi_release_threadsafe_function as *const (),
    napi_unref_threadsafe_function as *const (),
    napi_ref_threadsafe_function as *const (),
    napi_get_node_version as *const (),
    napi_get_uv_event_loop as *const (),
    napi_add_env_cleanup_hook as *const (),
    napi_remove_env_cleanup_hook as *const (),
    napi_add_async_cleanup_hook as *const (),
    napi_remove_async_cleanup_hook as *const (),
    node_api_get_module_file_name as *const (),
    node_api_post_finalizer as *const (),
    node_api_create_external_string_latin1 as *const (),
    node_api_create_external_string_utf16 as *const (),
    node_api_create_property_key_utf16 as *const (),
    node_api_create_syntax_error as *const (),
    node_api_throw_syntax_error as *const (),
]);

/// Called by engine binaries so the exports above are linked in.
pub fn keep() {
    std::hint::black_box(&SYMBOLS);
}
