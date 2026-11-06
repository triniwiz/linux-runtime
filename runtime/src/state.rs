//! Per-isolate runtime state, stored in the isolate slot as `Rc<State>`.

use crate::ffi::{CFunction, ClosureData};
use nativescript_core::Native;
use nativescript_metadata::{MetaFile, Offset};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub struct State {
    pub native: Native,
    pub isolate: Cell<*mut v8::Isolate>,
    pub context: RefCell<Option<v8::Global<v8::Context>>>,
    pub thread: std::thread::ThreadId,
    pub functions: RefCell<HashMap<Offset, Rc<CFunction>>>,
    struct_arena: RefCell<Option<(v8::Global<v8::ArrayBuffer>, usize, usize)>>,
    /// libffi closures handed to C, kept alive for the lifetime of the runtime
    pub closures: RefCell<Vec<Box<ClosureData>>>,
    pub pointer_template: RefCell<Option<v8::Global<v8::ObjectTemplate>>>,
    /// struct view instance templates, keyed by type index
    pub struct_templates: RefCell<HashMap<u32, v8::Global<v8::ObjectTemplate>>>,
    pub main_context: crate::event_loop::MainContext,
    pub modules: RefCell<crate::modules::ModuleMap>,
    pub next_request: Cell<u32>,
    pub requests: RefCell<HashMap<u32, v8::Global<v8::PromiseResolver>>>,
    pub websockets: RefCell<HashMap<u32, std::sync::mpsc::Sender<crate::net::WsCommand>>>,
    pub ws_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    pub workers: RefCell<HashMap<u32, std::sync::Arc<crate::workers::Link>>>,
    pub worker_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    pub napi_envs: RefCell<Vec<Box<crate::napi::Env>>>,
    pub inspector: RefCell<Option<crate::inspector::Inspector>>,
    pub napi_addons: RefCell<HashMap<std::path::PathBuf, v8::Global<v8::Value>>>,
    /// set in worker runtimes
    pub parent: RefCell<Option<crate::workers::Parent>>,
    pub timers: RefCell<HashMap<u32, crate::event_loop::Timer>>,
    pub next_timer: Cell<u32>,
    pub loop_refs: Cell<i32>,
    pub microtasks_scheduled: Cell<bool>,
    pub rejections: RefCell<Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>>,
    /// uncaught errors reported from the event loop
    pub uncaught: Cell<u32>,
    /// Set while a resolved global is being defined on the global object:
    /// defining a property consults the interceptor again, which must not recurse.
    pub memoizing: Cell<bool>,
}

impl State {
    pub fn new(meta: std::sync::Arc<MetaFile>) -> Self {
        State {
            native: Native::new(meta),
            isolate: Cell::new(std::ptr::null_mut()),
            context: RefCell::new(None),
            thread: std::thread::current().id(),
            functions: Default::default(),
            struct_arena: Default::default(),
            closures: Default::default(),
            pointer_template: Default::default(),
            struct_templates: Default::default(),
            main_context: crate::event_loop::MainContext::for_current_thread(),
            modules: Default::default(),
            next_request: Cell::new(0),
            requests: Default::default(),
            websockets: Default::default(),
            ws_dispatch: Default::default(),
            workers: Default::default(),
            worker_dispatch: Default::default(),
            parent: Default::default(),
            napi_envs: Default::default(),
            inspector: Default::default(),
            napi_addons: Default::default(),
            timers: Default::default(),
            next_timer: Cell::new(0),
            loop_refs: Cell::new(0),
            microtasks_scheduled: Cell::new(false),
            rejections: Default::default(),
            uncaught: Cell::new(0),
            memoizing: Cell::new(false),
        }
    }

    /// Registers a type and returns its index (used to tag JS objects).
    /// GC managed memory for small struct copies, bump allocated from a shared
    /// ArrayBuffer (the returned owner keeps it alive).
    pub fn struct_memory<'s>(&self, scope: &mut v8::PinScope<'s, '_>, size: usize) -> (v8::Local<'s, v8::ArrayBuffer>, usize) {
        const ARENA: usize = 64 * 1024;
        let size = size.max(1).div_ceil(16) * 16;
        if size > ARENA / 8 {
            let buffer = v8::ArrayBuffer::new(scope, size);
            let addr = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
            return (buffer, addr);
        }
        let mut arena = self.struct_arena.borrow_mut();
        if let Some((buffer, base, used)) = arena.as_mut() {
            if *used + size <= ARENA {
                let addr = *base + *used;
                *used += size;
                return (v8::Local::new(scope, &*buffer), addr);
            }
        }
        let buffer = v8::ArrayBuffer::new(scope, ARENA);
        let base = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
        *arena = Some((v8::Global::new(scope, buffer), base, size));
        (buffer, base)
    }
}

/// Fetches the runtime state from the isolate.
pub fn state(scope: &v8::PinScope) -> Rc<State> {
    scope
        .get_slot::<Rc<State>>()
        .expect("nativescript runtime state not installed")
        .clone()
}

impl std::ops::Deref for State {
    type Target = Native;
    fn deref(&self) -> &Native {
        &self.native
    }
}
