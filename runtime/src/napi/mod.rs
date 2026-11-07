//! Node-API implemented on rusty_v8 (native addons, `.node` files).

#![allow(non_camel_case_types, clippy::missing_safety_doc)]

mod js_api;
mod node_api;

use crate::state::State;
use std::cell::RefCell;
use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;

pub type napi_status = i32;
pub type napi_env = *mut Env;
pub type napi_value = *mut v8::Value;
pub type napi_callback_info = *const CallbackInfo;
pub type napi_callback = Option<unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value>;
pub type napi_finalize = Option<unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void)>;
pub type napi_addon_register_func = unsafe extern "C" fn(napi_env, napi_value) -> napi_value;

pub const OK: napi_status = 0;
pub const INVALID_ARG: napi_status = 1;
pub const OBJECT_EXPECTED: napi_status = 2;
pub const STRING_EXPECTED: napi_status = 3;
pub const NAME_EXPECTED: napi_status = 4;
pub const FUNCTION_EXPECTED: napi_status = 5;
pub const NUMBER_EXPECTED: napi_status = 6;
pub const BOOLEAN_EXPECTED: napi_status = 7;
pub const ARRAY_EXPECTED: napi_status = 8;
pub const GENERIC_FAILURE: napi_status = 9;
pub const PENDING_EXCEPTION: napi_status = 10;
pub const CANCELLED: napi_status = 11;
pub const ESCAPE_CALLED_TWICE: napi_status = 12;
pub const QUEUE_FULL: napi_status = 15;
pub const CLOSING: napi_status = 16;
pub const BIGINT_EXPECTED: napi_status = 17;
pub const DATE_EXPECTED: napi_status = 18;
pub const ARRAYBUFFER_EXPECTED: napi_status = 19;
pub const DETACHABLE_ARRAYBUFFER_EXPECTED: napi_status = 20;

const MESSAGES: [&str; 24] = [
    "",
    "Invalid argument",
    "An object was expected",
    "A string was expected",
    "A string or symbol was expected",
    "A function was expected",
    "A number was expected",
    "A boolean was expected",
    "An array was expected",
    "Unknown failure",
    "An exception is pending",
    "The async work item was cancelled",
    "napi_escape_handle already called on scope",
    "Invalid handle scope usage",
    "Invalid callback scope usage",
    "Thread-safe function queue is full",
    "Thread-safe function handle is closing",
    "A bigint was expected",
    "A date was expected",
    "An arraybuffer was expected",
    "A detachable arraybuffer was expected",
    "Main thread would deadlock",
    "External buffers are not allowed",
    "Cannot run JavaScript",
];

#[repr(C)]
pub struct napi_extended_error_info {
    pub error_message: *const c_char,
    pub engine_reserved: *mut c_void,
    pub engine_error_code: u32,
    pub error_code: napi_status,
}

/// A finalizer queued by GC, run later from a safe point.
pub(crate) struct PendingFinalize {
    pub cb: unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void),
    pub data: *mut c_void,
    pub hint: *mut c_void,
}

pub struct Env {
    pub(crate) isolate: *mut v8::Isolate,
    /// leaked Global slot, usable as a Local
    context: NonNull<v8::Context>,
    pub(crate) main_context: usize,
    last_error: napi_extended_error_info,
    messages: Vec<CString>,
    pub(crate) exception: Option<v8::Global<v8::Value>>,
    pub(crate) instance_data: Option<(*mut c_void, napi_finalize, *mut c_void)>,
    pub(crate) cleanup_hooks: Vec<(unsafe extern "C" fn(*mut c_void), *mut c_void)>,
    pub(crate) finalizers: Rc<RefCell<Vec<PendingFinalize>>>,
    pub(crate) filename: CString,
    pub(crate) callbacks: Vec<Box<CallbackData>>,
}

impl Env {
    #[inline]
    pub(crate) fn context<'s>(&self) -> v8::Local<'s, v8::Context> {
        unsafe {
            std::mem::transmute::<NonNull<v8::Context>, v8::Local<'s, v8::Context>>(self.context)
        }
    }

    #[inline]
    pub(crate) fn isolate(&self) -> &mut v8::Isolate {
        unsafe { &mut *self.isolate }
    }

    pub(crate) fn status(&mut self, status: napi_status) -> napi_status {
        self.last_error.error_code = status;
        status
    }

    /// Queues `cb` for when `value` is garbage collected.
    pub(crate) fn finalize_on_gc(
        &mut self,
        value: v8::Local<v8::Value>,
        cb: napi_finalize,
        data: *mut c_void,
        hint: *mut c_void,
    ) {
        let Some(cb) = cb else { return };
        let queue = self.finalizers.clone();
        let weak = v8::Weak::with_finalizer(
            self.isolate(),
            value,
            Box::new(move |_| queue.borrow_mut().push(PendingFinalize { cb, data, hint })),
        );
        // the Weak must stay alive until it fires
        std::mem::forget(weak);
    }
}

/// `napi_callback` plus its data, the V8 function data points here.
pub struct CallbackData {
    pub env: napi_env,
    pub cb: unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value,
    pub data: *mut c_void,
}

pub struct CallbackInfo {
    pub args: *const c_void,
    pub data: *mut c_void,
}

#[inline]
pub(crate) fn napi<'s, T>(v: v8::Local<'s, T>) -> napi_value
where
    v8::Local<'s, T>: Into<v8::Local<'s, v8::Value>>,
{
    let v: v8::Local<v8::Value> = v.into();
    unsafe { std::mem::transmute::<v8::Local<v8::Value>, napi_value>(v) }
}

#[inline]
pub(crate) unsafe fn local<'s>(v: napi_value) -> Option<v8::Local<'s, v8::Value>> {
    std::mem::transmute::<napi_value, Option<v8::Local<'s, v8::Value>>>(v)
}

/// JS entry point for napi callbacks.
pub(crate) fn trampoline<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::FunctionCallbackArguments<'s>,
    mut rv: v8::ReturnValue<'s, v8::Value>,
) {
    let Ok(ext) = v8::Local::<v8::External>::try_from(args.data()) else {
        return;
    };
    let data = unsafe { &*(ext.value() as *const CallbackData) };
    let info = CallbackInfo {
        args: &args as *const _ as *const c_void,
        data: data.data,
    };
    let result = unsafe { (data.cb)(data.env, &info) };
    let env = unsafe { &mut *data.env };
    if let Some(e) = env.exception.take() {
        let e = v8::Local::new(scope, e);
        scope.throw_exception(e);
        return;
    }
    if let Some(v) = unsafe { local(result) } {
        rv.set(v);
    }
}

pub(crate) fn args_of<'a>(info: napi_callback_info) -> &'a v8::FunctionCallbackArguments<'a> {
    unsafe { &*((*info).args as *const v8::FunctionCallbackArguments) }
}

type ExternalFinalizer = (usize, usize, usize, usize);

/// Finalizers of external buffers, whose deleters may run on any thread.
fn external_finalizers() -> &'static std::sync::Mutex<Vec<ExternalFinalizer>> {
    static Q: std::sync::OnceLock<std::sync::Mutex<Vec<ExternalFinalizer>>> =
        std::sync::OnceLock::new();
    Q.get_or_init(Default::default)
}

pub(crate) fn queue_external_finalizer(
    env: usize,
    cb: unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void),
    data: *mut c_void,
    hint: *mut c_void,
) {
    external_finalizers()
        .lock()
        .unwrap()
        .push((env, cb as usize, data as usize, hint as usize));
}

/// Runs finalizers queued by GC (outside GC, where napi calls are allowed).
pub fn drain_finalizers(st: &State) {
    let envs: Vec<napi_env> = st
        .napi_envs
        .borrow()
        .iter()
        .map(|e| &**e as *const Env as napi_env)
        .collect();
    if envs.is_empty() {
        return;
    }
    let mine: Vec<ExternalFinalizer> = {
        let mut q = external_finalizers().lock().unwrap();
        let (mine, rest) = q.drain(..).partition(|f| envs.contains(&(f.0 as napi_env)));
        *q = rest;
        mine
    };
    for (env, cb, data, hint) in mine {
        let cb: unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void) =
            unsafe { std::mem::transmute(cb) };
        unsafe { cb(env as napi_env, data as *mut c_void, hint as *mut c_void) };
    }
    for env in envs {
        loop {
            let pending = std::mem::take(&mut *unsafe { &*env }.finalizers.borrow_mut());
            if pending.is_empty() {
                break;
            }
            for f in pending {
                unsafe { (f.cb)(env, f.data, f.hint) };
            }
        }
    }
}

thread_local! {
    /// set by napi_module_register while an addon is dlopen()ed
    pub(crate) static PENDING_MODULE: RefCell<Option<napi_addon_register_func>> = const { RefCell::new(None) };
}

/// Loads a `.node` addon and returns its exports.
pub fn load_addon<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    path: &Path,
) -> Result<v8::Local<'s, v8::Value>, Option<String>> {
    if let Some(exports) = st.napi_addons.borrow().get(path) {
        return Ok(v8::Local::new(scope, exports));
    }
    let cpath =
        CString::new(path.to_string_lossy().as_bytes()).map_err(|_| "invalid path".to_string())?;
    PENDING_MODULE.with(|m| m.borrow_mut().take());
    let handle = unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(Some(format!(
            "{}: {}",
            path.display(),
            nativescript_core::dlerror()
        )));
    }
    let sym = |name: &str| {
        let name = CString::new(name).unwrap();
        unsafe { libc::dlsym(handle, name.as_ptr()) }
    };
    let init: napi_addon_register_func = match sym("napi_register_module_v1") {
        p if !p.is_null() => unsafe {
            std::mem::transmute::<*mut c_void, napi_addon_register_func>(p)
        },
        _ => PENDING_MODULE
            .with(|m| m.borrow_mut().take())
            .ok_or_else(|| format!("{}: not a Node-API module", path.display()))?,
    };
    let _version = match sym("node_api_module_get_api_version_v1") {
        p if !p.is_null() => unsafe {
            std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(p)()
        },
        _ => 8,
    };

    let context = scope.get_current_context();
    let context = v8::Global::new(scope, context).into_raw();
    let mut env = Box::new(Env {
        isolate: st.isolate.get(),
        context,
        main_context: st.main_context.context,
        last_error: napi_extended_error_info {
            error_message: std::ptr::null(),
            engine_reserved: std::ptr::null_mut(),
            engine_error_code: 0,
            error_code: OK,
        },
        messages: MESSAGES.iter().map(|m| CString::new(*m).unwrap()).collect(),
        exception: None,
        instance_data: None,
        cleanup_hooks: Vec::new(),
        finalizers: Rc::new(RefCell::new(Vec::new())),
        filename: CString::new(nativescript_core::resolve::path_to_file_url(path))
            .unwrap_or_default(),
        callbacks: Vec::new(),
    });
    let env_ptr: napi_env = &mut *env;
    st.napi_envs.borrow_mut().push(env);

    let exports = v8::Object::new(scope);
    let result = unsafe { init(env_ptr, napi(exports)) };
    let env = unsafe { &mut *env_ptr };
    if let Some(e) = env.exception.take() {
        let e = v8::Local::new(scope, e);
        scope.throw_exception(e);
        return Err(None);
    }
    let exports = unsafe { local(result) }.unwrap_or(exports.into());
    st.napi_addons
        .borrow_mut()
        .insert(PathBuf::from(path), v8::Global::new(scope, exports));
    Ok(exports)
}

/// Runs cleanup hooks and instance data finalizers (runtime teardown).
pub fn teardown(st: &State) {
    let envs: Vec<Box<Env>> = std::mem::take(&mut *st.napi_envs.borrow_mut());
    for mut env in envs {
        let ptr: napi_env = &mut *env;
        for (hook, arg) in std::mem::take(&mut env.cleanup_hooks).into_iter().rev() {
            unsafe { hook(arg) };
        }
        if let Some((data, Some(cb), hint)) = env.instance_data.take() {
            unsafe { cb(ptr, data, hint) };
        }
    }
}

/// Keeps every exported napi symbol in the binary (see build.rs, -rdynamic).
pub fn keep_symbols() -> usize {
    std::hint::black_box(js_api::SYMBOLS.0.len() + node_api::SYMBOLS.0.len())
}
