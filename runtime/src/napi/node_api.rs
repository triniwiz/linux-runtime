//! node_api.h: buffers, async work, threadsafe functions, hooks.

use super::js_api::external_arraybuffer;
use super::*;
use crate::event_loop::{self, post};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Condvar, Mutex, OnceLock};

macro_rules! env {
    ($e:expr) => {{
        if $e.is_null() {
            return INVALID_ARG;
        }
        unsafe { &mut *$e }
    }};
}
macro_rules! arg {
    ($env:ident, $($p:expr),+) => { $( if $p.is_null() { return $env.status(INVALID_ARG); } )+ };
}
macro_rules! scope {
    ($env:ident, $s:ident) => {
        let __ctx = $env.context();
        v8::callback_scope!(unsafe $s, __ctx);
    };
}

#[repr(C)]
pub struct napi_module {
    nm_version: i32,
    nm_flags: u32,
    nm_filename: *const c_char,
    nm_register_func: Option<napi_addon_register_func>,
    nm_modname: *const c_char,
    nm_priv: *mut c_void,
    reserved: [*mut c_void; 4],
}

#[no_mangle]
pub unsafe extern "C" fn napi_module_register(module: *mut napi_module) {
    if let Some(f) = module.as_ref().and_then(|m| m.nm_register_func) {
        PENDING_MODULE.with(|m| *m.borrow_mut() = Some(f));
    }
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
pub unsafe extern "C" fn napi_async_init(
    env: napi_env,
    _resource: napi_value,
    _name: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    *result = 1 as *mut c_void;
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_async_destroy(env: napi_env, _context: *mut c_void) -> napi_status {
    env!(env).status(OK)
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
    super::js_api::napi_call_function(env, recv, func, argc, argv, result)
}

#[no_mangle]
pub unsafe extern "C" fn napi_open_callback_scope(
    env: napi_env,
    _resource: napi_value,
    _context: *mut c_void,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    *result = 1 as *mut c_void;
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_close_callback_scope(
    env: napi_env,
    _scope: *mut c_void,
) -> napi_status {
    env!(env).status(OK)
}

// ---------- buffers (Uint8Array views, zero copy for external data) ----------

fn buffer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    ab: v8::Local<'s, v8::ArrayBuffer>,
    len: usize,
) -> Option<v8::Local<'s, v8::Uint8Array>> {
    let view = v8::Uint8Array::new(scope, ab, 0, len)?;
    // Node Buffer semantics when a Buffer class exists
    let global = scope.get_current_context().global(scope);
    let key = v8::String::new(scope, "Buffer")?;
    if let Some(proto) = global
        .get(scope, key.into())
        .filter(|b| b.is_function())
        .and_then(|b| b.to_object(scope))
        .and_then(|b| {
            let k = v8::String::new(scope, "prototype")?;
            b.get(scope, k.into())
        })
    {
        view.set_prototype(scope, proto);
    }
    Some(view)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer(
    env: napi_env,
    length: usize,
    data: *mut *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let ab = v8::ArrayBuffer::new(s, length);
    let Some(b) = buffer(s, ab, length) else {
        return env.status(GENERIC_FAILURE);
    };
    if !data.is_null() {
        *data = ab
            .data()
            .map(|p| p.as_ptr())
            .unwrap_or(std::ptr::null_mut());
    }
    *result = napi(b);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external_buffer(
    env: napi_env,
    length: usize,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let ab = external_arraybuffer(env, s, data, length, finalize_cb, hint);
    let Some(b) = buffer(s, ab, length) else {
        return env.status(GENERIC_FAILURE);
    };
    *result = napi(b);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer_copy(
    env: napi_env,
    length: usize,
    data: *const c_void,
    result_data: *mut *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    if length > 0 {
        arg!(env, data);
    }
    scope!(env, s);
    let ab = v8::ArrayBuffer::new(s, length);
    let dst = ab
        .data()
        .map(|p| p.as_ptr())
        .unwrap_or(std::ptr::null_mut());
    if length > 0 {
        std::ptr::copy_nonoverlapping(data as *const u8, dst as *mut u8, length);
    }
    let Some(b) = buffer(s, ab, length) else {
        return env.status(GENERIC_FAILURE);
    };
    if !result_data.is_null() {
        *result_data = dst;
    }
    *result = napi(b);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_buffer(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(v) = local(value) else {
        return env.status(INVALID_ARG);
    };
    *result = v.is_array_buffer_view();
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_buffer_info(
    env: napi_env,
    value: napi_value,
    data: *mut *mut c_void,
    length: *mut usize,
) -> napi_status {
    let env = env!(env);
    let Some(view) = local(value).and_then(|v| v8::Local::<v8::ArrayBufferView>::try_from(v).ok())
    else {
        return env.status(INVALID_ARG);
    };
    if !data.is_null() {
        *data = view.data();
    }
    if !length.is_null() {
        *length = view.byte_length();
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_buffer_from_arraybuffer(
    env: napi_env,
    arraybuffer: napi_value,
    byte_offset: usize,
    byte_length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(ab) = local(arraybuffer).and_then(|v| v8::Local::<v8::ArrayBuffer>::try_from(v).ok())
    else {
        return env.status(ARRAYBUFFER_EXPECTED);
    };
    if byte_offset + byte_length > ab.byte_length() {
        return env.status(INVALID_ARG);
    }
    scope!(env, s);
    let Some(view) = v8::Uint8Array::new(s, ab, byte_offset, byte_length) else {
        return env.status(GENERIC_FAILURE);
    };
    *result = napi(view);
    env.status(OK)
}

// ---------- async work ----------

type Job = Box<dyn FnOnce() + Send>;

/// Fixed size worker pool shared by async work.
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
    let e = env!(env);
    arg!(e, result);
    let Some(execute) = execute else {
        return e.status(INVALID_ARG);
    };
    *result = Box::into_raw(Box::new(AsyncWork {
        env,
        execute,
        complete,
        data,
        state: AtomicU8::new(IDLE),
    }));
    e.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_delete_async_work(
    env: napi_env,
    work: *mut AsyncWork,
) -> napi_status {
    let env = env!(env);
    arg!(env, work);
    drop(Box::from_raw(work));
    env.status(OK)
}

fn complete_on_js(context: usize, work: usize, status: napi_status) {
    post(
        context,
        true,
        Box::new(move |scope, _| {
            // complete() usually deletes the work: copy what we need first
            let (env, complete, data) = {
                let w = unsafe { &*(work as *const AsyncWork) };
                (w.env, w.complete, w.data)
            };
            if let Some(complete) = complete {
                unsafe { complete(env, status, data) };
            }
            let env = unsafe { &mut *env };
            if let Some(e) = env.exception.take() {
                let e = v8::Local::new(scope, e);
                scope.throw_exception(e);
            }
        }),
    );
}

#[no_mangle]
pub unsafe extern "C" fn napi_queue_async_work(env: napi_env, work: *mut AsyncWork) -> napi_status {
    let env = env!(env);
    arg!(env, work);
    if (*work).state.swap(QUEUED, Ordering::SeqCst) == QUEUED {
        return env.status(GENERIC_FAILURE);
    }
    event_loop::with_current(event_loop::begin_pending);
    let context = env.main_context;
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
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_cancel_async_work(
    env: napi_env,
    work: *mut AsyncWork,
) -> napi_status {
    let env = env!(env);
    arg!(env, work);
    if (*work)
        .state
        .compare_exchange(QUEUED, CANCELLED_STATE, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return env.status(GENERIC_FAILURE);
    }
    complete_on_js(env.main_context, work as usize, CANCELLED);
    env.status(OK)
}

// ---------- threadsafe functions ----------

/// JS-thread-only data inside a cross thread object.
struct JsOnly<T>(T);
unsafe impl<T> Send for JsOnly<T> {}
unsafe impl<T> Sync for JsOnly<T> {}

pub struct Tsfn {
    env: usize,
    main_context: usize,
    func: JsOnly<std::cell::RefCell<Option<v8::Global<v8::Function>>>>,
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
    refd: JsOnly<std::cell::Cell<bool>>,
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
            Box::new(move |scope, _| unsafe { dispatch(scope, ptr) }),
        );
    }
}

unsafe fn dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, ptr: usize) {
    let t = &*(ptr as *const Tsfn);
    t.scheduled.store(false, Ordering::SeqCst);
    let items: Vec<usize> = t.queue.lock().unwrap().drain(..).collect();
    t.space.notify_all();
    let env = t.env as napi_env;
    let func = t.func.0.borrow().as_ref().map(|g| v8::Local::new(scope, g));
    for data in items {
        if t.closing.load(Ordering::SeqCst)
            && *t.threads.lock().unwrap() == 0
            && t.call_js.is_none()
        {
            break;
        }
        match t.call_js {
            Some(cb) => cb(
                env,
                func.map(napi).unwrap_or(std::ptr::null_mut()),
                t.context as *mut c_void,
                data as *mut c_void,
            ),
            None => {
                if let Some(f) = func {
                    let recv = v8::undefined(scope).into();
                    f.call(scope, recv, &[]);
                }
            }
        }
        if let Some(e) = (*env).exception.take() {
            let e = v8::Local::new(scope, e);
            scope.throw_exception(e);
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
    if let Some(cb) = t.finalize_cb {
        cb(
            t.env as napi_env,
            t.finalize_data as *mut c_void,
            t.context as *mut c_void,
        );
    }
    if t.refd.0.get() {
        event_loop::with_current(event_loop::end_pending);
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
    let e = env!(env);
    arg!(e, result);
    if initial_thread_count == 0 {
        return e.status(INVALID_ARG);
    }
    let func = match local(func) {
        None if call_js.is_none() => return e.status(INVALID_ARG),
        None => None,
        Some(f) => match v8::Local::<v8::Function>::try_from(f) {
            Ok(f) => Some(v8::Global::new(e.isolate(), f)),
            Err(_) => return e.status(FUNCTION_EXPECTED),
        },
    };
    let t = Box::new(Tsfn {
        env: env as usize,
        main_context: e.main_context,
        func: JsOnly(std::cell::RefCell::new(func)),
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
        refd: JsOnly(std::cell::Cell::new(true)),
    });
    event_loop::with_current(event_loop::begin_pending);
    *result = Box::into_raw(t);
    e.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_threadsafe_function_context(
    func: *const Tsfn,
    result: *mut *mut c_void,
) -> napi_status {
    if func.is_null() || result.is_null() {
        return INVALID_ARG;
    }
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
    let env = env!(env);
    arg!(env, func);
    if (*func).refd.0.replace(false) {
        event_loop::with_current(event_loop::end_pending);
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_ref_threadsafe_function(
    env: napi_env,
    func: *const Tsfn,
) -> napi_status {
    let env = env!(env);
    arg!(env, func);
    if !(*func).refd.0.replace(true) {
        event_loop::with_current(event_loop::begin_pending);
    }
    env.status(OK)
}

// ---------- misc ----------

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
    let env = env!(env);
    arg!(env, version);
    *version = &NODE_VERSION.0;
    env.status(OK)
}

/// No libuv here: a stable dummy pointer for addons that only check it.
#[no_mangle]
pub unsafe extern "C" fn napi_get_uv_event_loop(
    env: napi_env,
    result: *mut *mut c_void,
) -> napi_status {
    static DUMMY: [u64; 64] = [0; 64];
    let env = env!(env);
    arg!(env, result);
    *result = DUMMY.as_ptr() as *mut c_void;
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_fatal_exception(env: napi_env, err: napi_value) -> napi_status {
    let env = env!(env);
    scope!(env, s);
    let text = local(err)
        .map(|e| e.to_rust_string_lossy(s))
        .unwrap_or_default();
    eprintln!("Uncaught {text}");
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_add_env_cleanup_hook(
    env: napi_env,
    fun: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
) -> napi_status {
    let env = env!(env);
    let Some(fun) = fun else {
        return env.status(INVALID_ARG);
    };
    env.cleanup_hooks.push((fun, arg));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_remove_env_cleanup_hook(
    env: napi_env,
    fun: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: *mut c_void,
) -> napi_status {
    let env = env!(env);
    let Some(fun) = fun else {
        return env.status(INVALID_ARG);
    };
    env.cleanup_hooks
        .retain(|(f, a)| !(*f as usize == fun as usize && *a == arg));
    env.status(OK)
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
    let env = env!(env);
    let Some(hook) = hook else {
        return env.status(INVALID_ARG);
    };
    let h = Box::into_raw(Box::new(AsyncCleanupHook { hook, arg }));
    env.cleanup_hooks.push((run_async_hook, h as *mut c_void));
    if !remove_handle.is_null() {
        *remove_handle = h;
    }
    env.status(OK)
}

/// The handle stays valid until teardown; removing just skips the hook.
#[no_mangle]
pub unsafe extern "C" fn napi_remove_async_cleanup_hook(
    handle: *mut AsyncCleanupHook,
) -> napi_status {
    if handle.is_null() {
        return INVALID_ARG;
    }
    unsafe extern "C" fn noop(_: *mut AsyncCleanupHook, _: *mut c_void) {}
    (*handle).hook = noop;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn node_api_get_module_file_name(
    env: napi_env,
    result: *mut *const c_char,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    *result = env.filename.as_ptr();
    env.status(OK)
}

/// Referenced so the linker keeps every export.
pub struct Symbols(pub &'static [*const ()]);
unsafe impl Sync for Symbols {}
#[used]
pub static SYMBOLS: Symbols = Symbols(&[
    napi_module_register as *const (),
    napi_fatal_error as *const (),
    napi_async_init as *const (),
    napi_async_destroy as *const (),
    napi_make_callback as *const (),
    napi_open_callback_scope as *const (),
    napi_close_callback_scope as *const (),
    napi_create_buffer as *const (),
    napi_create_external_buffer as *const (),
    napi_create_buffer_copy as *const (),
    napi_is_buffer as *const (),
    napi_get_buffer_info as *const (),
    node_api_create_buffer_from_arraybuffer as *const (),
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
    napi_fatal_exception as *const (),
    napi_add_env_cleanup_hook as *const (),
    napi_remove_env_cleanup_hook as *const (),
    napi_add_async_cleanup_hook as *const (),
    napi_remove_async_cleanup_hook as *const (),
    node_api_get_module_file_name as *const (),
]);
