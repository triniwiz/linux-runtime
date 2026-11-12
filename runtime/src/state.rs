//! Per-isolate runtime state, stored in the isolate slot as `Rc<State>`.

use crate::ffi::{CFunction, ClosureData};
use nativescript_core::Native;
use nativescript_metadata::{MetaId, MetaSet};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub struct State {
    pub native: Native,
    pub isolate: Cell<*mut v8::Isolate>,
    pub context: RefCell<Option<v8::Global<v8::Context>>>,
    /// handle slot of `context`
    context_slot: Cell<*const v8::Context>,
    pub thread: std::thread::ThreadId,
    pub functions: RefCell<HashMap<MetaId, Rc<CFunction>>>,
    struct_arena: RefCell<Option<(v8::Global<v8::ArrayBuffer>, usize, usize)>>,
    /// libffi closures handed to C, kept alive for the lifetime of the runtime
    pub closures: RefCell<Vec<Box<ClosureData>>>,
    /// bumped when closures are released (invalidates cached closure lookups)
    pub closure_generation: Cell<u64>,
    pub keys: crate::interop::keys::Keys,
    pub pointers: RefCell<Option<crate::handles::Factory>>,
    /// struct views, keyed by type index
    pub struct_views: RefCell<HashMap<u32, Rc<crate::handles::Factory>>>,
    /// accessor data of struct view fields
    pub fields: RefCell<Vec<Box<crate::structs::FieldAccess>>>,
    pub main_context: crate::event_loop::MainContext,
    pub modules: RefCell<crate::modules::ModuleMap>,
    pub next_request: Cell<u32>,
    pub requests: RefCell<HashMap<u32, v8::Global<v8::PromiseResolver>>>,
    pub websockets: RefCell<HashMap<u32, std::sync::mpsc::Sender<crate::net::WsCommand>>>,
    pub ws_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    pub workers: RefCell<HashMap<u32, std::sync::Arc<crate::workers::Link>>>,
    pub worker_dispatch: RefCell<Option<v8::Global<v8::Function>>>,
    pub napi_envs: RefCell<Vec<Box<crate::napi::Env>>>,
    /// interop.adopt: owners of adopted pointers by id, and the (id, free function,
    /// address) of collected ones, freed at the next checkpoint
    pub adopted: RefCell<HashMap<u64, v8::Weak<v8::Object>>>,
    pub adopted_collected: Rc<RefCell<Vec<(u64, usize, usize)>>>,
    pub next_adopted: Cell<u64>,
    pub inspector: RefCell<Option<crate::inspector::Inspector>>,
    pub napi_addons: RefCell<HashMap<std::path::PathBuf, v8::Global<v8::Value>>>,
    /// set in worker runtimes
    pub parent: RefCell<Option<crate::workers::Parent>>,
    pub timers: RefCell<HashMap<u32, crate::event_loop::Timer>>,
    pub frames: crate::frames::Frames,
    pub next_timer: Cell<u32>,
    pub loop_refs: Cell<i32>,
    pub microtasks_scheduled: Cell<bool>,
    pub rejections: RefCell<Vec<(v8::Global<v8::Promise>, v8::Global<v8::Value>)>>,
    /// uncaught errors reported from the event loop
    pub uncaught: Cell<u32>,
    /// Set while a resolved global is being defined on the global object:
    /// defining a property consults the interceptor again, which must not recurse.
    pub memoizing: Cell<bool>,
    pub gobjects: crate::gobject::GObjects,
    /// V8 value layout, when the probe at startup matched
    tagged: std::cell::OnceCell<crate::tagged::Layout>,
}

impl State {
    pub fn new(meta: std::sync::Arc<MetaSet>) -> Self {
        State {
            native: Native::new(meta),
            isolate: Cell::new(std::ptr::null_mut()),
            context: RefCell::new(None),
            context_slot: Cell::new(std::ptr::null()),
            thread: std::thread::current().id(),
            functions: Default::default(),
            struct_arena: Default::default(),
            closures: Default::default(),
            closure_generation: Cell::new(0),
            keys: Default::default(),
            pointers: Default::default(),
            struct_views: Default::default(),
            fields: Default::default(),
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
            adopted: Default::default(),
            adopted_collected: Default::default(),
            next_adopted: Cell::new(0),
            inspector: Default::default(),
            napi_addons: Default::default(),
            timers: Default::default(),
            frames: Default::default(),
            next_timer: Cell::new(0),
            loop_refs: Cell::new(0),
            microtasks_scheduled: Cell::new(false),
            rejections: Default::default(),
            uncaught: Cell::new(0),
            memoizing: Cell::new(false),
            gobjects: Default::default(),
            tagged: Default::default(),
        }
    }

    pub fn set_context(&self, context: v8::Global<v8::Context>) {
        // SAFETY: the Global belongs to this isolate and lives in `self.context`
        let slot = unsafe { v8::Handle::get_unchecked(&context) as *const v8::Context };
        *self.context.borrow_mut() = Some(context);
        self.context_slot.set(slot);
    }

    /// The context, read through its Global's slot (no new handle).
    #[inline]
    pub fn context_local<'s>(&self) -> Option<v8::Local<'s, v8::Context>> {
        let slot = self.context_slot.get();
        // SAFETY: a Local is a pointer to a handle slot
        (!slot.is_null()).then(|| unsafe {
            std::mem::transmute::<*const v8::Context, v8::Local<'s, v8::Context>>(slot)
        })
    }

    #[inline(always)]
    pub fn tagged(&self) -> Option<&crate::tagged::Layout> {
        self.tagged.get()
    }

    /// Probes the V8 value layout once `interop` is installed.
    pub fn probe_layout(&self, scope: &mut v8::PinScope) {
        let pointers = self.pointers.borrow();
        let Some(factory) = pointers.as_ref() else {
            return;
        };
        let layout =
            crate::tagged::Layout::probe(scope, factory.prototype_slot(), |scope, addr| {
                factory.make(scope, addr, None)
            });
        if let Some(layout) = layout {
            factory.set_raw_fields(true);
            let _ = self.tagged.set(layout);
        }
    }

    /// Registers a type and returns its index (used to tag JS objects).
    /// GC managed memory for small struct copies, bump allocated from a shared
    /// ArrayBuffer (the returned owner keeps it alive).
    pub fn struct_memory<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        size: usize,
    ) -> (v8::Local<'s, v8::ArrayBuffer>, usize) {
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
