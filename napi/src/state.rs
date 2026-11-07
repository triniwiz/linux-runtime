//! Per JS thread state, reachable from napi callbacks through the env's instance data.

use crate::ffi::{CFunction, ClosureData};
use crate::js::Js;
use crate::sys::*;
use nativescript_core::glib::MainContext;
use nativescript_core::Native;
use nativescript_metadata::{MetaId, MetaSet};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::hash::BuildHasherDefault;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub struct State {
    pub native: Native,
    pub env: napi_env,
    pub main_context: MainContext,
    pub thread: std::thread::ThreadId,
    pub functions: RefCell<HashMap<MetaId, Rc<CFunction>>>,
    pub closures: RefCell<Vec<Box<ClosureData>>>,
    /// constructors: Pointer, Reference
    pub pointer_ctor: Cell<napi_ref>,
    pub reference_ctor: Cell<napi_ref>,
    /// struct view classes by type index
    pub struct_classes: RefCell<HashMap<u32, napi_ref>>,
    pub timers: RefCell<HashMap<u32, crate::event_loop::Timer>>,
    pub next_timer: Cell<u32>,
    pub loop_refs: Cell<i32>,
    pub jobs_scheduled: Cell<bool>,
    pub uncaught: Cell<u32>,
    /// CommonJS `module` objects and addon exports
    pub modules: RefCell<HashMap<PathBuf, napi_ref>>,
    pub flush_rejections: Cell<napi_ref>,
    /// live interop handles
    pub handles: RefCell<PtrSet>,
    /// napi env cleanup hooks (fn, arg)
    pub cleanup_hooks: RefCell<Vec<(unsafe extern "C" fn(*mut c_void), usize)>>,
    /// file URL of the addon being loaded
    pub module_file: RefCell<std::ffi::CString>,
    /// where resolved metadata globals are memoized (null: the global object)
    pub globals_target: Cell<napi_ref>,
}

impl std::ops::Deref for State {
    type Target = Native;
    fn deref(&self) -> &Native {
        &self.native
    }
}

impl State {
    pub fn new(meta: Arc<MetaSet>, env: napi_env) -> Self {
        State {
            native: Native::new(meta),
            env,
            main_context: MainContext::for_current_thread(),
            thread: std::thread::current().id(),
            functions: Default::default(),
            closures: Default::default(),
            pointer_ctor: Cell::new(std::ptr::null_mut()),
            reference_ctor: Cell::new(std::ptr::null_mut()),
            struct_classes: Default::default(),
            timers: Default::default(),
            next_timer: Cell::new(0),
            loop_refs: Cell::new(0),
            jobs_scheduled: Cell::new(false),
            uncaught: Cell::new(0),
            modules: Default::default(),
            flush_rejections: Cell::new(std::ptr::null_mut()),
            handles: Default::default(),
            cleanup_hooks: Default::default(),
            module_file: Default::default(),
            globals_target: Cell::new(std::ptr::null_mut()),
        }
    }

    #[inline]
    pub fn js(&self) -> Js {
        Js(self.env)
    }
}

thread_local! {
    static CURRENT: Cell<*const State> = const { Cell::new(std::ptr::null()) };
}

/// Not instance data: addons share the env and may set their own.
pub fn set_current(st: *const State) {
    CURRENT.with(|c| c.set(st));
}

/// Runtime of this JS thread (one per thread).
#[inline]
pub fn state<'a>(_env: napi_env) -> &'a State {
    unsafe { &*CURRENT.with(|c| c.get()) }
}

pub fn current<'a>() -> Option<&'a State> {
    let p = CURRENT.with(|c| c.get());
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Native state behind interop objects (napi_wrap).
pub struct Handle {
    pub kind: Kind,
    pub address: usize,
    pub type_index: Option<u32>,
    /// keeps the memory alive
    pub owner: napi_ref,
    /// Reference value before it has storage
    pub pending: napi_ref,
    /// strings stored in struct fields / references
    pub keep: Vec<napi_ref>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Pointer,
    Reference,
    Struct,
    /// interop.types.* and struct constructors
    Type,
    /// JS wrapper of a C function (address usable as a function pointer)
    Function,
}

/// Hasher for pointer keys.
#[derive(Default)]
pub struct PtrHasher(u64);

impl std::hash::Hasher for PtrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64 >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub type PtrSet = HashSet<usize, BuildHasherDefault<PtrHasher>>;

unsafe extern "C" fn finalize_handle(env: napi_env, data: *mut c_void, _: *mut c_void) {
    if let Some(st) = current() {
        st.handles.borrow_mut().remove(&(data as usize));
    }
    let h = Box::from_raw(data as *mut Handle);
    let js = Js(env);
    js.delete_ref(h.owner);
    js.delete_ref(h.pending);
    for r in h.keep {
        js.delete_ref(r);
    }
}

/// Wraps `object`. Not type tagged: tags are slow on some engines, `handles` tells ours apart.
pub fn attach(js: Js, object: napi_value, handle: Handle) -> *mut Handle {
    let ptr = Box::into_raw(Box::new(handle));
    unsafe {
        napi_wrap(
            js.0,
            object,
            ptr as *mut c_void,
            Some(finalize_handle),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if let Some(st) = current() {
        st.handles.borrow_mut().insert(ptr as usize);
    }
    ptr
}

/// Our handle behind `v`, if it's an interop object.
#[inline]
pub fn handle<'a>(js: Js, v: napi_value) -> Option<&'a mut Handle> {
    if !matches!(js.type_of(v), crate::js::OBJECT | crate::js::FUNCTION) {
        return None;
    }
    let mut data: *mut c_void = std::ptr::null_mut();
    unsafe { napi_unwrap(js.0, v, &mut data) };
    if data.is_null() || !current()?.handles.borrow().contains(&(data as usize)) {
        return None;
    }
    Some(unsafe { &mut *(data as *mut Handle) })
}

pub fn handle_of<'a>(js: Js, v: napi_value, kind: Kind) -> Option<&'a mut Handle> {
    handle(js, v).filter(|h| h.kind == kind)
}

impl Handle {
    pub fn new(kind: Kind, address: usize, type_index: Option<u32>, owner: napi_ref) -> Self {
        Handle {
            kind,
            address,
            type_index,
            owner,
            pending: std::ptr::null_mut(),
            keep: Vec::new(),
        }
    }
}
