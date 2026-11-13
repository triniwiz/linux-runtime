//! GObject classes implemented in JS (core/js/gobject.js describes them, this registers
//! and runs them). An instance's JS object is a view of its instance struct with the
//! class's prototype; it holds a toggle reference, so it is kept alive while native code
//! references the GObject and only weakly held otherwise.

use crate::handles::{self, Kind};
use crate::interop::{self, keys};
use crate::state::{state, State};
use crate::{marshal, report_exception, structs, throw_error};
use nativescript_core::gobject::{self as gobj, ClassDef, GType, GValue, In, Val, Values, Want};
use nativescript_metadata::Type;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::ffi::CString;
use std::rc::Rc;

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

pub struct Prop {
    /// the JS property: an accessor of the class, or of gobject.js reading `slot`
    key: v8::Global<v8::Value>,
    /// where gobject.js's accessors keep the value (a Symbol)
    slot: v8::Global<v8::Value>,
    /// read and written through `key` (an accessor of the class, or an override)
    custom: bool,
}

pub struct Class {
    cls: v8::Global<v8::Function>,
    proto: v8::Global<v8::Object>,
    /// a view with the class's prototype, cloned for each instance
    template: Option<v8::Global<v8::Object>>,
    /// instance struct of the nearest native ancestor
    base: Type,
    gtype: GType,
    def: &'static ClassDef,
    props: Vec<Prop>,
}

struct Entry {
    weak: Rc<WeakCell>,
    strong: Option<v8::Global<v8::Object>>,
    def: &'static ClassDef,
}

// V8's weak handles, without rusty_v8's per-handle finalizer map and boxed closure:
// these are the C bindings its `Weak` uses.
extern "C" {
    fn v8__Global__NewWeak(
        isolate: *mut std::ffi::c_void,
        data: *const v8::Data,
        parameter: *const std::ffi::c_void,
        callback: unsafe extern "C" fn(*const std::ffi::c_void),
    ) -> *const v8::Data;
    fn v8__Global__Reset(data: *const v8::Data);
    fn v8__WeakCallbackInfo__GetParameter(info: *const std::ffi::c_void) -> *mut std::ffi::c_void;
}

/// A weak handle to a JS object. V8 holds a reference to the cell (the callback's
/// parameter) until it collects the object or the handle is reset.
struct WeakCell {
    slot: Cell<*const v8::Data>,
    address: usize,
}

thread_local! {
    /// cells of JS objects collected since the last drain
    static COLLECTED: RefCell<Vec<Rc<WeakCell>>> = const { RefCell::new(Vec::new()) };
}

/// Runs during GC: no JS, no borrows of the runtime's state.
unsafe extern "C" fn weak_callback(info: *const std::ffi::c_void) {
    let cell = unsafe { Rc::from_raw(v8__WeakCallbackInfo__GetParameter(info) as *const WeakCell) };
    unsafe { v8__Global__Reset(cell.slot.replace(std::ptr::null())) };
    COLLECTED.with(|c| c.borrow_mut().push(cell));
}

impl WeakCell {
    fn new(isolate: &mut v8::Isolate, object: v8::Local<v8::Object>, address: usize) -> Rc<Self> {
        let cell = Rc::new(WeakCell { slot: Cell::new(std::ptr::null()), address });
        let raw = unsafe { std::mem::transmute::<v8::UnsafeRawIsolatePtr, *mut std::ffi::c_void>(isolate.as_raw_isolate_ptr()) };
        let parameter = Rc::into_raw(cell.clone()) as *const std::ffi::c_void;
        let data = &*object as *const v8::Object as *const v8::Data;
        cell.slot.set(unsafe { v8__Global__NewWeak(raw, data, parameter, weak_callback) });
        cell
    }

    fn get<'s>(&self) -> Option<v8::Local<'s, v8::Object>> {
        let slot = self.slot.get();
        // SAFETY: a Local is a pointer to a handle slot
        (!slot.is_null()).then(|| unsafe { std::mem::transmute::<*const v8::Data, v8::Local<'s, v8::Object>>(slot) })
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        let slot = self.weak.slot.replace(std::ptr::null());
        if !slot.is_null() {
            unsafe {
                v8__Global__Reset(slot);
                // V8's reference: the callback won't run
                Rc::decrement_strong_count(Rc::as_ptr(&self.weak));
            }
        }
    }
}

#[derive(Default)]
pub struct GObjects {
    classes: RefCell<Vec<Class>>,
    instances: RefCell<HashMap<usize, Entry, nativescript_core::native::FxBuildHasher>>,
    /// `new` calls inside g_object_new: (GType, JS object once a vfunc needed it)
    constructing: RefCell<Vec<(GType, Option<v8::Global<v8::Object>>)>>,
    /// JS objects of native instances, while their class's constructor runs
    adopting: RefCell<Vec<v8::Global<v8::Object>>>,
    /// instances in dispose: tracking them again would resurrect them
    disposing: RefCell<Vec<usize>>,
    signals: RefCell<HashMap<(GType, String), Rc<gobj::SignalInfo>>>,
    host: OnceCell<&'static Host>,
    /// toggle-reference data of native classes' instances
    native: OnceCell<&'static ClassDef>,
    wrappers: RefCell<HashMap<GType, Wrapper, nativescript_core::native::FxBuildHasher>>,
    protos: RefCell<HashMap<GType, v8::Global<v8::Object>, nativescript_core::native::FxBuildHasher>>,
    /// struct name -> the GObject type or interface it names, 0 for other structs
    pointees: RefCell<HashMap<String, GType, nativescript_core::native::FxBuildHasher>>,
    /// struct name -> its GType, found through `*_get_type` when not registered yet
    named: RefCell<HashMap<String, GType, nativescript_core::native::FxBuildHasher>>,
}

enum Wrapper {
    NotObject,
    JsClass,
    /// an interop.Pointer with the class's prototype, cloned per instance
    Native(v8::Global<v8::Object>),
}

pub struct Host {
    state: *const State,
    thread: std::thread::ThreadId,
    context: usize,
    /// cleared when the runtime goes: its GObjects may outlive it
    alive: std::sync::atomic::AtomicBool,
}

// the state is only used on its thread: other threads hop to it
unsafe impl Sync for Host {}

impl Host {
    /// `f` on the JS thread, without a V8 scope
    fn on_thread(&self, f: impl FnOnce(&State)) {
        if !self.alive.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let st = unsafe { &*self.state };
        if std::thread::current().id() == self.thread {
            f(st);
        } else {
            crate::event_loop::run_on_js_thread(self.context, Box::new(|| f(st)));
        }
    }

    fn run(&self, f: impl FnOnce(&mut v8::PinScope<'_, '_>, &State)) {
        if !self.alive.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let st = unsafe { &*self.state };
        let call = || with_scope(st, |scope| f(scope, st));
        if std::thread::current().id() == self.thread {
            call();
        } else {
            crate::event_loop::run_on_js_thread(self.context, Box::new(call));
        }
    }
}

/// A scope in the runtime's context, entered from C.
fn with_scope(st: &State, f: impl FnOnce(&mut v8::PinScope<'_, '_>)) {
    let Some(context) = st.context_local() else {
        return;
    };
    let isolate = unsafe { &mut *st.isolate.get() };
    v8::scope!(let _handles, isolate);
    v8::callback_scope!(unsafe scope, context);
    v8::tc_scope!(let tc, scope);
    f(tc);
    if let Some(exception) = tc.exception() {
        let message = tc.message();
        report_exception(tc, exception, message);
        st.uncaught.set(st.uncaught.get() + 1);
    }
}

impl gobj::Host for Host {
    fn get_property(&self, class: u32, object: usize, index: usize, value: *mut GValue) {
        self.run(|scope, st| {
            let Some(obj) = instance(scope, st, object, false) else { return };
            let classes = st.gobjects.classes.borrow();
            let prop = &classes[class as usize].props[index];
            let key = v8::Local::new(scope, if prop.custom { &prop.key } else { &prop.slot });
            drop(classes);
            let Some(v) = obj.get(scope, key) else { return };
            if !v.is_undefined() {
                if let Err(e) = unsafe { set_value(scope, value, v) } {
                    throw_error(scope, &e);
                }
            }
        });
    }

    fn set_property(&self, class: u32, object: usize, index: usize, value: *const GValue) {
        self.run(|scope, st| {
            let Some(obj) = instance(scope, st, object, false) else { return };
            let v = unsafe { to_js(scope, st, gobj::read(value)) };
            let classes = st.gobjects.classes.borrow();
            let prop = &classes[class as usize].props[index];
            let key = v8::Local::new(scope, if prop.custom { &prop.key } else { &prop.slot });
            drop(classes);
            obj.set(scope, key, v);
        });
    }

    fn finalize(&self, object: usize) {
        self.on_thread(|st| {
            let entry = st.gobjects.instances.borrow_mut().remove(&object);
            drop(entry);
        });
    }

    fn toggle(&self, object: usize, is_last: bool) {
        self.on_thread(|st| {
            let mut instances = st.gobjects.instances.borrow_mut();
            let Some(entry) = instances.get_mut(&object) else { return };
            entry.strong = if is_last {
                None
            } else {
                let isolate = unsafe { &mut *st.isolate.get() };
                entry.weak.get().map(|o| v8::Global::new(isolate, o))
            };
        });
    }
}

fn host(st: &State) -> &'static Host {
    st.gobjects.host.get_or_init(|| {
        Box::leak(Box::new(Host {
            state: st,
            thread: st.thread,
            context: st.main_context.context,
            alive: std::sync::atomic::AtomicBool::new(true),
        }))
    })
}

fn native_def(st: &State) -> &'static ClassDef {
    st.gobjects.native.get_or_init(|| Box::leak(Box::new(ClassDef::new(host(st), u32::MAX, false))))
}

/// The struct a `Struct *` type points to.
#[inline]
pub fn pointee(ty: &Type) -> Option<&str> {
    match ty {
        Type::Pointer(inner) => match &**inner {
            Type::Struct(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

/// The GObject type or interface a struct name stands for (`GtkWidget`), else 0. Only
/// asked with an instance at hand, so the type is registered.
pub fn pointee_gtype(st: &State, name: &str) -> GType {
    if let Some(&gtype) = st.gobjects.pointees.borrow().get(name) {
        return gtype;
    }
    let bare = name.strip_prefix('_').unwrap_or(name);
    let gtype = CString::new(bare).map_or(0, |c| gobj::type_from_name(&c));
    let gtype = match gobj::fundamental(gtype) {
        gobj::G_TYPE_OBJECT | gobj::G_TYPE_INTERFACE => gtype,
        _ => 0,
    };
    st.gobjects.pointees.borrow_mut().insert(name.to_owned(), gtype);
    gtype
}

/// A `Struct *` value: the JS object of the GObject when the struct is a GObject type.
pub fn read_pointer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    address: usize,
) -> Option<v8::Local<'s, v8::Object>> {
    let name = pointee(ty)?;
    if pointee_gtype(st, name) == 0 {
        return None;
    }
    wrap(scope, st, address, false)
}

/// The JS object of the GObject at `address`: the instance of its JS class, or an
/// interop.Pointer with its native class's prototype, one per GObject while JS holds it.
/// `owned`: the caller's reference passes to the JS object. `None` when it isn't a GObject.
pub fn wrap<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    address: usize,
    owned: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    let release = |owned: bool| {
        if owned {
            unsafe { gobj::g_object_unref(address as _) };
        }
    };
    if let Some(o) = live(scope, st, address) {
        release(owned);
        return Some(o);
    }
    let template = match wrapper(scope, st, gobj::instance_type(address)) {
        Some(Ok(t)) => t,
        Some(Err(())) => {
            let o = instance(scope, st, address, false);
            release(owned);
            return o;
        }
        None => return None,
    };
    let object = handles::clone(scope, template)?;
    handles::set_address(object, address);
    // without ownership annotations, take a reference of our own: a floating one (a new
    // widget) becomes it, a borrowed object stays referenced by whoever else holds it
    let floating = unsafe { gobj::g_object_is_floating(address as _) } != 0;
    if floating {
        unsafe { gobj::g_object_ref_sink(address as _) };
    }
    track(scope, st, address, object, native_def(st));
    release(owned || floating);
    Some(object)
}

/// How instances of `gtype` read in JS: `Ok(template)` for native classes, `Err` for JS
/// classes of this runtime, `None` for types that aren't GObjects.
fn wrapper<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    gtype: GType,
) -> Option<Result<v8::Local<'s, v8::Object>, ()>> {
    let made = |scope: &mut v8::PinScope<'s, '_>, w: &Wrapper| match w {
        Wrapper::NotObject => None,
        Wrapper::JsClass => Some(Err(())),
        Wrapper::Native(t) => Some(Ok(v8::Local::new(scope, t))),
    };
    if let Some(w) = st.gobjects.wrappers.borrow().get(&gtype) {
        return made(scope, w);
    }
    let ours = host(st) as *const Host as *const u8;
    let mut js = false;
    let mut t = gtype;
    while t != 0 && !js {
        js = gobj::class_def(t).is_some_and(|d| std::ptr::eq(d.host as *const dyn gobj::Host as *const u8, ours));
        t = gobj::type_parent(t);
    }
    let w = if !gobj::is_a(gtype, gobj::G_TYPE_OBJECT) {
        Wrapper::NotObject
    } else if js {
        Wrapper::JsClass
    } else {
        let proto = class_proto(scope, st, gtype)?;
        let template = interop::new_pointer(scope, st, 0, None);
        template.set_prototype(scope, proto.into());
        Wrapper::Native(v8::Global::new(scope, template))
    };
    let result = made(scope, &w);
    st.gobjects.wrappers.borrow_mut().insert(gtype, w);
    result
}

/// The prototype of a native class's instances: its struct constructor's (`GtkLabel.prototype`)
/// when the metadata has one, inheriting the parent class's, and interop.Pointer's at the root.
pub fn class_proto<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, gtype: GType) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(p) = st.gobjects.protos.borrow().get(&gtype) {
        return Some(v8::Local::new(scope, p));
    }
    let parent = match gobj::type_parent(gtype) {
        0 => interop::new_pointer(scope, st, 0, None).get_prototype(scope)?.to_object(scope)?,
        p => class_proto(scope, st, p)?,
    };
    let proto = constructor_proto(scope, st, gtype).unwrap_or_else(|| v8::Object::new(scope));
    let object_proto = v8::Object::new(scope).get_prototype(scope)?;
    if proto.get_prototype(scope).is_some_and(|p| p == object_proto) {
        proto.set_prototype(scope, parent.into());
    }
    st.gobjects.protos.borrow_mut().insert(gtype, v8::Global::new(scope, proto));
    Some(proto)
}

fn constructor_proto<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, gtype: GType) -> Option<v8::Local<'s, v8::Object>> {
    let name = gobj::type_name(gtype).to_str().ok()?;
    let global = scope.get_current_context().global(scope);
    for name in [name.to_owned(), format!("_{name}")] {
        let key = v8::String::new(scope, &name)?;
        let Some(ctor) = global.get(scope, key.into()).filter(|v| v.is_function()) else {
            continue;
        };
        if !matches!(interop::type_from_value(scope, st, ctor), Some(Type::Struct(_))) {
            continue;
        }
        let key = v8::String::new(scope, "prototype")?;
        return ctor.to_object(scope)?.get(scope, key.into())?.to_object(scope);
    }
    None
}

/// The GType of the struct `name`, registering it through its `*_get_type` function; 0
/// for structs that aren't GTypes.
fn gtype_named(scope: &mut v8::PinScope<'_, '_>, st: &State, name: &str) -> GType {
    if let Some(&gtype) = st.gobjects.named.borrow().get(name) {
        return gtype;
    }
    let bare = name.strip_prefix('_').unwrap_or(name);
    let mut gtype = CString::new(bare).map_or(0, |c| gobj::type_from_name(&c));
    if gtype == 0 {
        if let Some(n) = v8::String::new(scope, bare) {
            if let Ok(Some(v)) = interop::gobject_hook(scope, "gtype", &[n.into()]) {
                gtype = gtype_of(scope, v);
            }
        }
    }
    st.gobjects.named.borrow_mut().insert(name.to_owned(), gtype);
    gtype
}

/// `new GtkLabel(props)`: a new instance of a native GObject class (`Ok(None)` for other
/// structs).
pub fn construct_native<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    props: v8::Local<'s, v8::Value>,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    let Type::Struct(name) = ty else { return Ok(None) };
    let gtype = gtype_named(scope, st, name);
    if gtype == 0 || !gobj::is_a(gtype, gobj::G_TYPE_OBJECT) {
        return Ok(None);
    }
    if gobj::is_abstract(gtype) {
        return Err(format!("{name} is abstract: extend it, or create one with a C constructor"));
    }
    let address = create(scope, gtype, props)?;
    if address == 0 {
        return Err(format!("failed to create a {name}"));
    }
    let owned = gobj::new_is_owned(gtype, address);
    wrap(scope, st, address, owned).map(|o| Some(o.into())).ok_or_else(|| format!("failed to create a {name}"))
}

fn tag<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>) -> Option<u32> {
    let key = keys::gobject_class(scope);
    let v = o.get_private(scope, key)?;
    if v.is_undefined() {
        return None;
    }
    v.uint32_value(scope)
}

/// The JS object of a GObject created by a JS class (made when needed), else None.
pub fn instance<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    address: usize,
    dispose: bool,
) -> Option<v8::Local<'s, v8::Object>> {
    if let Some(o) = live(scope, st, address) {
        return Some(o);
    }
    let def = gobj::instance_class(address)?;
    if !std::ptr::eq(def.host as *const dyn gobj::Host as *const u8, host(st) as *const Host as *const u8) {
        return None;
    }
    let object = bare(scope, st, def.class, address)?;
    if dispose || st.gobjects.disposing.borrow().contains(&address) {
        return Some(object);
    }
    let gtype = gobj::instance_type(address);
    {
        let mut frames = st.gobjects.constructing.borrow_mut();
        if let Some(frame) = frames.last_mut().filter(|f| f.0 == gtype && f.1.is_none()) {
            frame.1 = Some(v8::Global::new(scope, object));
            drop(frames);
            track(scope, st, address, object, def);
            return Some(object);
        }
    }
    // created by native code (GtkBuilder, g_object_new): run the class's constructor
    track(scope, st, address, object, def);
    let cls = v8::Local::new(scope, &st.gobjects.classes.borrow()[def.class as usize].cls);
    st.gobjects.adopting.borrow_mut().push(v8::Global::new(scope, object));
    v8::tc_scope!(let tc, scope);
    if cls.new_instance(tc, &[]).is_none() {
        if let Some(exception) = tc.exception() {
            let message = tc.message();
            report_exception(tc, exception, message);
            st.uncaught.set(st.uncaught.get() + 1);
        }
    }
    let mut adopting = st.gobjects.adopting.borrow_mut();
    if let Some(i) = adopting.iter().position(|g| v8::Local::new(tc, g) == object) {
        adopting.remove(i);
    }
    Some(object)
}

/// Handle slot of the JS object of a GObject, for returns without a scope.
#[inline]
pub fn live_slot(st: &State, address: usize) -> Option<*const v8::Object> {
    let instances = st.gobjects.instances.borrow();
    let entry = instances.get(&address)?;
    let slot = match &entry.strong {
        // SAFETY: the Global belongs to this isolate
        Some(g) => unsafe { v8::Handle::get_unchecked(g) as *const v8::Object },
        None => entry.weak.slot.get() as *const v8::Object,
    };
    (!slot.is_null()).then_some(slot)
}

fn live<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, address: usize) -> Option<v8::Local<'s, v8::Object>> {
    let instances = st.gobjects.instances.borrow();
    let entry = instances.get(&address)?;
    match &entry.strong {
        Some(g) => Some(v8::Local::new(scope, g)),
        None => entry.weak.get().map(|o| v8::Local::new(scope, o)),
    }
}

fn bare<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, class: u32, address: usize) -> Option<v8::Local<'s, v8::Object>> {
    let template = st.gobjects.classes.borrow()[class as usize].template.as_ref().map(|t| v8::Local::new(scope, t));
    if let Some(object) = template.and_then(|t| handles::clone(scope, t)) {
        handles::set_address(object, address);
        return Some(object);
    }
    let (base, proto) = {
        let classes = st.gobjects.classes.borrow();
        let c = &classes[class as usize];
        (c.base.clone(), v8::Local::new(scope, &c.proto))
    };
    let object = structs::view(scope, st, &base, address, None).ok()?.to_object(scope)?;
    object.set_prototype(scope, proto.into());
    Some(object)
}

fn track<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    address: usize,
    object: v8::Local<'s, v8::Object>,
    def: &'static ClassDef,
) {
    let isolate = unsafe { &mut *st.isolate.get() };
    let weak = WeakCell::new(isolate, object, address);
    let strong = Some(v8::Global::new(scope, object));
    let previous = st.gobjects.instances.borrow_mut().insert(address, Entry { weak, strong, def });
    if previous.is_none() {
        let data = def as *const ClassDef as *mut std::ffi::c_void;
        unsafe { gobj::g_object_add_toggle_ref(address as _, gobj::toggle_notify, data) };
    }
}

/// The runtime goes: its classes stop calling into it, and its JS objects let go.
pub fn shutdown(st: &State) {
    if let Some(host) = st.gobjects.host.get() {
        host.alive.store(false, std::sync::atomic::Ordering::Release);
    }
    st.gobjects.instances.borrow_mut().clear();
    COLLECTED.with(|c| c.borrow_mut().clear());
}

pub fn enter_dispose(st: &State, address: usize) {
    st.gobjects.disposing.borrow_mut().push(address);
}

pub fn leave_dispose(st: &State, address: usize) {
    let mut disposing = st.gobjects.disposing.borrow_mut();
    if let Some(i) = disposing.iter().rposition(|&a| a == address) {
        disposing.remove(i);
    }
}

/// Drops the toggle references of collected JS objects (from the event loop: it can
/// finalize the GObject, calling into JS).
pub fn drain(st: &State) {
    loop {
        let collected = COLLECTED.with(|c| std::mem::take(&mut *c.borrow_mut()));
        if collected.is_empty() {
            return;
        }
        for cell in collected {
            let address = cell.address;
            let entry = {
                let mut instances = st.gobjects.instances.borrow_mut();
                // a newer JS object may have replaced the collected one
                match instances.get(&address) {
                    Some(e) if Rc::ptr_eq(&e.weak, &cell) && e.strong.is_none() => instances.remove(&address),
                    _ => None,
                }
            };
            if let Some(entry) = entry {
                let data = entry.def as *const ClassDef as *mut std::ffi::c_void;
                drop(entry);
                unsafe { gobj::g_object_remove_toggle_ref(address as _, gobj::toggle_notify, data) };
            }
        }
    }
}

/// `new X()` / `super()` of a JS GObject class `target`: `Ok(None)` when it isn't one.
pub fn construct<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    target: v8::Local<'s, v8::Object>,
    props: v8::Local<'s, v8::Value>,
) -> Result<Option<v8::Local<'s, v8::Value>>, String> {
    let class = match tag(scope, target) {
        Some(c) => c,
        None => {
            // first construction: gobject.js describes the class and registers it
            match crate::interop::gobject_hook(scope, "register", &[target.into()]) {
                Ok(Some(v)) if v.is_true() => {}
                Ok(_) => return Ok(None),
                Err(()) => return Err(String::new()),
            }
            tag(scope, target).ok_or("the class wasn't registered")?
        }
    };
    let (gtype, def, proto) = {
        let classes = st.gobjects.classes.borrow();
        let c = &classes[class as usize];
        (c.gtype, c.def, v8::Local::new(scope, &c.proto))
    };
    // the class's constructor, running for a GObject native code created
    let adopted = st.gobjects.adopting.borrow().last().map(|g| v8::Local::new(scope, g));
    if let Some(adopted) = adopted {
        if adopted.get_prototype(scope).is_some_and(|p| p == proto) {
            st.gobjects.adopting.borrow_mut().pop();
            return Ok(Some(adopted.into()));
        }
    }
    st.gobjects.constructing.borrow_mut().push((gtype, None));
    let created = create(scope, gtype, props);
    let frame = st.gobjects.constructing.borrow_mut().pop();
    let address = created?;
    let object = match frame.and_then(|f| f.1) {
        Some(g) => v8::Local::new(scope, &g),
        None => {
            let object = bare(scope, st, class, address).ok_or("failed to create the JS object")?;
            track(scope, st, address, object, def);
            object
        }
    };
    if gobj::new_is_owned(gtype, address) {
        unsafe {
            if gobj::g_object_is_floating(address as _) != 0 {
                gobj::g_object_ref_sink(address as _);
            }
            // the toggle reference keeps it alive from here
            gobj::g_object_unref(address as _);
        }
    }
    Ok(Some(object.into()))
}

fn is_plain_object<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<v8::Local<'s, v8::Object>> {
    if !v.is_object() || v.is_function() || v.is_array() || handles::of(v).is_some() {
        return None;
    }
    let o = v.to_object(scope)?;
    let proto = o.get_prototype(scope)?;
    let object_proto = v8::Object::new(scope).get_prototype(scope)?;
    (proto.is_null() || proto == object_proto).then_some(o)
}

fn create<'s>(scope: &mut v8::PinScope<'s, '_>, gtype: GType, props: v8::Local<'s, v8::Value>) -> Result<usize, String> {
    let Some(props) = is_plain_object(scope, props) else {
        return Ok(gobj::new_object(gtype, &[], &Values(Vec::new())));
    };
    let keys = props
        .get_own_property_names(scope, Default::default())
        .ok_or("failed to read the properties")?;
    let mut names = Vec::with_capacity(keys.length() as usize);
    let mut types = Vec::with_capacity(names.capacity());
    let mut items = Vec::with_capacity(names.capacity());
    for i in 0..keys.length() {
        let key = keys.get_index(scope, i).ok_or("failed to read the properties")?;
        let name = key.to_rust_string_lossy(scope);
        let canonical = CString::new(gobj::canonical_name(&name)).map_err(|e| e.to_string())?;
        let pspec = gobj::find_property(gtype, &canonical).ok_or_else(|| format!("the class has no property '{name}'"))?;
        types.push(gobj::param_value_type(pspec));
        names.push(canonical);
        items.push(props.get(scope, key).ok_or("failed to read the properties")?);
    }
    let mut values = Values::with_types(&types);
    for (value, item) in values.0.iter_mut().zip(items) {
        unsafe { set_value(scope, value, item)? };
    }
    Ok(gobj::new_object(gtype, &names, &values))
}

/// Sets an initialized GValue from a JS value.
///
/// # Safety
/// `value` is an initialized GValue.
pub unsafe fn set_value<'s>(scope: &mut v8::PinScope<'s, '_>, value: *mut GValue, v: v8::Local<'s, v8::Value>) -> Result<(), String> {
    let gtype = unsafe { (*value).g_type };
    let want = gobj::want(gtype).ok_or_else(|| format!("values of GType {gtype} are not supported"))?;
    let text;
    let input = match want {
        Want::Bool => In::Bool(v.boolean_value(scope)),
        Want::Number => In::Number(v.number_value(scope).unwrap_or(0.0)),
        Want::Int64 | Want::UInt64 => match v8::Local::<v8::BigInt>::try_from(v) {
            Ok(b) if matches!(want, Want::UInt64) => In::BigUint(b.u64_value().0),
            Ok(b) => In::BigInt(b.i64_value().0),
            Err(_) => In::Number(v.number_value(scope).unwrap_or(0.0)),
        },
        Want::Str => {
            if v.is_null_or_undefined() {
                In::Str(None)
            } else {
                text = CString::new(v.to_rust_string_lossy(scope).replace('\0', "")).unwrap();
                In::Str(Some(&text))
            }
        }
        Want::Address => In::Address(address_of(scope, v)?),
    };
    unsafe { gobj::write(value, input) }
}

fn address_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Result<usize, String> {
    if v.is_null_or_undefined() {
        return Ok(0);
    }
    match handles::of(v) {
        Some((o, Kind::Pointer | Kind::Struct)) => Ok(handles::address(o)),
        Some((_, Kind::Reference)) => interop::reference_storage(scope, v).ok_or_else(|| "an empty Reference".into()),
        None => Err("expected a GObject, Pointer or null".into()),
    }
}

pub fn to_js<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, v: Val) -> v8::Local<'s, v8::Value> {
    match v {
        Val::None => v8::undefined(scope).into(),
        Val::Bool(b) => v8::Boolean::new(scope, b).into(),
        Val::I64(n) => marshal::number_or_bigint(scope, n),
        Val::U64(n) if n <= (1u64 << 53) => v8::Number::new(scope, n as f64).into(),
        Val::U64(n) => v8::BigInt::new_from_u64(scope, n).into(),
        Val::F64(n) => v8::Number::new(scope, n).into(),
        Val::Str(None) | Val::Object(0) | Val::Pointer(0) => v8::null(scope).into(),
        Val::Str(Some(s)) => v8::String::new(scope, &s.to_string_lossy()).map_or_else(|| v8::null(scope).into(), Into::into),
        Val::Object(a) => match wrap(scope, st, a, false) {
            Some(o) => o.into(),
            None => interop::new_pointer(scope, st, a, None).into(),
        },
        Val::Pointer(a) => interop::new_pointer(scope, st, a, None).into(),
    }
}

fn str_arg<'s>(scope: &mut v8::PinScope<'s, '_>, o: v8::Local<'s, v8::Object>, key: &str) -> Option<v8::Local<'s, v8::Value>> {
    let k = v8::String::new(scope, key)?;
    o.get(scope, k.into())
}

fn gtype_of<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> GType {
    match v8::Local::<v8::BigInt>::try_from(v) {
        Ok(b) => b.u64_value().0 as GType,
        Err(_) => v.number_value(scope).unwrap_or(0.0) as GType,
    }
}

fn array<'s>(scope: &mut v8::PinScope<'s, '_>, v: Option<v8::Local<'s, v8::Value>>) -> Vec<v8::Local<'s, v8::Value>> {
    let Some(a) = v.and_then(|v| v8::Local::<v8::Array>::try_from(v).ok()) else {
        return Vec::new();
    };
    (0..a.length()).filter_map(|i| a.get_index(scope, i)).collect()
}

/// Vtable slots for `[[method, structConstructor, field], ...]`: C functions calling
/// the instance's method.
fn slots<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    proto: v8::Local<'s, v8::Object>,
    list: Vec<v8::Local<'s, v8::Value>>,
) -> Result<Vec<gobj::Slot>, String> {
    let mut out = Vec::new();
    for item in list {
        let item = array(scope, Some(item));
        let [method, record, field] = [item.first(), item.get(1), item.get(2)].map(|v| v.copied());
        let (Some(method), Some(record), Some(field)) = (method, record, field) else {
            return Err("invalid vtable slot".into());
        };
        let ty = interop::type_from_value(scope, st, record).ok_or("not a struct constructor")?;
        let layout = st.record(&ty)?;
        let field = field.to_rust_string_lossy(scope);
        let f = layout.fields.iter().find(|f| f.name == field).ok_or_else(|| format!("no field {field}"))?;
        let Type::FunctionPointer(sig) = &f.ty else {
            return Err(format!("{field} is not a function pointer"));
        };
        let name = method.to_rust_string_lossy(scope);
        let function = proto
            .get(scope, method)
            .and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())
            .ok_or_else(|| format!("{name} is not a method"))?;
        let code = crate::ffi::method_closure(scope, st, function, sig, name == "dispose")?;
        out.push(gobj::Slot { offset: f.offset, code });
    }
    Ok(out)
}

/// `natives.gobjectRegister(cls, description)`: registers the GType of a JS class.
fn register<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    match register_class(scope, &st, args.get(0), args.get(1)) {
        Ok(gtype) => rv.set(v8::Number::new(scope, gtype as f64).into()),
        Err(e) => throw_error(scope, &e),
    }
}

fn register_class<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    cls: v8::Local<'s, v8::Value>,
    desc: v8::Local<'s, v8::Value>,
) -> Result<GType, String> {
    let cls = v8::Local::<v8::Function>::try_from(cls).map_err(|_| "expected a class")?;
    let desc = desc.to_object(scope).ok_or("expected a class description")?;
    let parent = str_arg(scope, desc, "parent").map(|v| gtype_of(scope, v)).unwrap_or(0);
    let name = str_arg(scope, desc, "name").map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
    let base = str_arg(scope, desc, "base").ok_or("no base")?;
    let base = interop::type_from_value(scope, st, base).ok_or("the base isn't a struct constructor")?;
    let root = str_arg(scope, desc, "root").is_some_and(|v| v.is_true());
    if root {
        class_proto(scope, st, parent).ok_or("the parent class has no prototype")?;
    }
    let class = st.gobjects.classes.borrow().len() as u32;
    let key = v8::String::new(scope, "prototype").unwrap();
    let proto = cls.get(scope, key.into()).and_then(|p| p.to_object(scope)).ok_or("the class has no prototype")?;
    let mut def = ClassDef::new(host(st), class, root);
    let list = str_arg(scope, desc, "vfuncs");
    let list = array(scope, list);
    def.vfuncs = slots(scope, st, proto, list)?;
    let list = str_arg(scope, desc, "interfaces");
    for iface in array(scope, list) {
        let item = array(scope, Some(iface));
        let gtype = item.first().map(|v| gtype_of(scope, *v)).unwrap_or(0);
        let list = array(scope, item.get(1).copied());
        def.interfaces.push(gobj::Interface { gtype, slots: slots(scope, st, proto, list)? });
    }
    let mut props = Vec::new();
    let list = str_arg(scope, desc, "properties");
    for p in array(scope, list) {
        let item = array(scope, Some(p));
        let get = |i: usize| item.get(i).copied();
        let (Some(key), Some(slot)) = (get(0), get(1)) else {
            return Err("invalid property".into());
        };
        let custom = get(2).is_some_and(|v| v.is_true());
        let pspec = get(3).and_then(|v| handles::is(v, Kind::Pointer)).map(handles::address);
        def.properties.push(match pspec {
            Some(pspec) => gobj::Property::Install(pspec),
            None => {
                let name = get(4).map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
                gobj::Property::Override(CString::new(name).map_err(|e| e.to_string())?)
            }
        });
        props.push(Prop { key: v8::Global::new(scope, key), slot: v8::Global::new(scope, slot), custom });
    }
    let list = str_arg(scope, desc, "signals");
    for s in array(scope, list) {
        let item = array(scope, Some(s));
        let name = item.first().map(|v| v.to_rust_string_lossy(scope)).unwrap_or_default();
        let flags = item.get(1).and_then(|v| v.uint32_value(scope)).unwrap_or(2);
        let return_type = item.get(2).map(|v| gtype_of(scope, *v)).unwrap_or(gobj::G_TYPE_NONE);
        let params = array(scope, item.get(3).copied()).into_iter().map(|v| gtype_of(scope, v)).collect();
        def.signals.push(gobj::Signal { name: CString::new(name).map_err(|e| e.to_string())?, flags, return_type, params });
    }
    let cname = CString::new(name.clone()).map_err(|e| e.to_string())?;
    let gtype = gobj::register(parent, &cname, def)?;
    let def = gobj::class_def(gtype).ok_or("registration failed")?;
    let template = structs::view(scope, st, &base, 0, None)
        .ok()
        .and_then(|v| v.to_object(scope))
        .filter(|o| o.set_prototype(scope, proto.into()) == Some(true))
        .and_then(|o| handles::clone(scope, o).map(|_| o))
        .map(|o| v8::Global::new(scope, o));
    st.gobjects.classes.borrow_mut().push(Class {
        cls: v8::Global::new(scope, cls),
        proto: v8::Global::new(scope, proto),
        template,
        base,
        gtype,
        def,
        props,
    });
    let key = keys::gobject_class(scope);
    let index = v8::Integer::new_from_unsigned(scope, class);
    cls.set_private(scope, key, index.into());
    Ok(gtype)
}

/// `natives.gobjectEmit(instance, signal, ...args)`: emits with the signal's GValue types.
fn emit<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    match emit_signal(scope, &st, &args) {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

fn emit_signal<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, args: &Args<'s>) -> Result<v8::Local<'s, v8::Value>, String> {
    let address = address_of(scope, args.get(0))?;
    if address == 0 {
        return Err("emit needs an instance".into());
    }
    let gtype = gobj::instance_type(address);
    let name = args.get(1).to_rust_string_lossy(scope);
    let key = (gtype, name);
    let cached = st.gobjects.signals.borrow().get(&key).cloned();
    let signal = match cached {
        Some(s) => s,
        None => {
            let c = CString::new(key.1.clone()).map_err(|e| e.to_string())?;
            let s = Rc::new(gobj::signal_info(gtype, &c).ok_or_else(|| format!("unknown signal '{}'", key.1))?);
            st.gobjects.signals.borrow_mut().insert(key, s.clone());
            s
        }
    };
    let mut types = Vec::with_capacity(signal.params.len() + 1);
    types.push(gtype);
    types.extend_from_slice(&signal.params);
    let mut values = Values::with_types(&types);
    unsafe { gobj::write(&mut values.0[0], In::Address(address))? };
    for (i, value) in values.0[1..].iter_mut().enumerate() {
        unsafe { set_value(scope, value, args.get(i as i32 + 2))? };
    }
    if signal.return_type == gobj::G_TYPE_NONE {
        gobj::emit(&values.0, &signal, None);
        return Ok(v8::undefined(scope).into());
    }
    let mut ret = Values::with_types(&[signal.return_type]);
    gobj::emit(&values.0, &signal, Some(&mut ret.0[0]));
    Ok(unsafe { to_js(scope, st, gobj::read(&ret.0[0])) })
}

/// `natives.gobjectSlot(structConstructor, field)`: the field is a function pointer.
fn is_slot<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let field = args.get(1).to_rust_string_lossy(scope);
    let slot = interop::type_from_value(scope, &st, args.get(0))
        .and_then(|ty| st.record(&ty).ok())
        .is_some_and(|r| r.fields.iter().any(|f| f.name == field && matches!(f.ty, Type::FunctionPointer(_))));
    rv.set(v8::Boolean::new(scope, slot).into());
}

/// `natives.gobjectFields(structConstructor)`: the prototype of the struct's views, with
/// its field accessors.
fn fields<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let proto = interop::type_from_value(scope, &st, args.get(0))
        .and_then(|ty| structs::view(scope, &st, &ty, 0, None).ok())
        .and_then(|v| v.to_object(scope))
        .and_then(|o| o.get_prototype(scope));
    match proto {
        Some(p) => rv.set(p),
        None => throw_error(scope, "expected a struct constructor"),
    }
}

/// `natives.gobjectPointerType()`: an interop type for `GObject *` values, read as their
/// JS objects.
fn pointer_type<'s>(scope: &mut v8::PinScope<'s, '_>, _args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let descriptor = v8::Object::new(scope);
    let key = v8::String::new(scope, "name").unwrap();
    let name = v8::String::new(scope, "GObject*").unwrap();
    descriptor.set(scope, key.into(), name.into());
    interop::tag_type(scope, &st, descriptor, Type::Pointer(Box::new(Type::Struct("GObject".into()))));
    rv.set(descriptor.into());
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, natives: v8::Local<'s, v8::Object>) {
    let f = v8::Function::new(scope, fields).unwrap();
    let k = v8::String::new(scope, "gobjectFields").unwrap();
    natives.set(scope, k.into(), f.into());
    let f = v8::Function::new(scope, pointer_type).unwrap();
    let k = v8::String::new(scope, "gobjectPointerType").unwrap();
    natives.set(scope, k.into(), f.into());
    let f = v8::Function::new(scope, is_slot).unwrap();
    let k = v8::String::new(scope, "gobjectSlot").unwrap();
    natives.set(scope, k.into(), f.into());
    let f = v8::Function::new(scope, register).unwrap();
    let k = v8::String::new(scope, "gobjectRegister").unwrap();
    natives.set(scope, k.into(), f.into());
    let f = v8::Function::new(scope, emit).unwrap();
    let k = v8::String::new(scope, "gobjectEmit").unwrap();
    natives.set(scope, k.into(), f.into());
}
