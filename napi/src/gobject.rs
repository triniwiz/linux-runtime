//! GObject classes implemented in JS on the Node-API engines (see runtime/src/gobject.rs
//! for the V8 runtime). An instance's JS object has the class's prototype and the
//! struct handle of its instance struct; one reference to it is strong while native
//! code also references the GObject (a toggle reference) and weak otherwise.

use crate::js::*;
use crate::state::{attach, handle, state, Handle, Kind, State};
use crate::sys::*;
use crate::{ffi, interop};
use nativescript_core::gobject::{self as gobj, ClassDef, GType, GValue, In, Val, Values, Want};
use nativescript_metadata::Type;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::ptr::null_mut;
use std::rc::Rc;

// Node-API only promises references to objects: property keys (strings, Symbols)
// live in a per-class array, [key, slot] at 2 * index.

struct Class {
    proto: napi_ref,
    cls: napi_ref,
    /// type index of the nearest native ancestor's instance struct
    base: u32,
    gtype: GType,
    def: &'static ClassDef,
    /// per property: read and written through its key (an accessor of the class, or
    /// an override) rather than gobject.js's slot
    custom: Vec<bool>,
    keys: napi_ref,
}

/// Identifies one JS object of a GObject: its handle's finalizer queues it.
pub struct Token {
    address: usize,
}

struct Entry {
    object: napi_ref,
    strong: bool,
    token: Rc<Token>,
    def: &'static ClassDef,
}

thread_local! {
    /// tokens of JS objects collected since the last drain
    static COLLECTED: RefCell<Vec<Rc<Token>>> = const { RefCell::new(Vec::new()) };
}

/// From the struct handle's finalizer, possibly during GC.
pub fn collected(token: Rc<Token>) {
    COLLECTED.with(|c| c.borrow_mut().push(token));
}

#[derive(Default)]
pub struct GObjects {
    classes: RefCell<Vec<Class>>,
    instances: RefCell<HashMap<usize, Entry, nativescript_core::native::FxBuildHasher>>,
    constructing: RefCell<Vec<(GType, Option<napi_ref>)>>,
    adopting: RefCell<Vec<napi_ref>>,
    disposing: RefCell<Vec<usize>>,
    signals: RefCell<HashMap<(GType, String), Rc<gobj::SignalInfo>>>,
    host: OnceCell<&'static Host>,
    /// own property of registered classes holding their index
    key: OnceCell<napi_ref>,
    /// Object.create
    create: OnceCell<napi_ref>,
    /// Object.setPrototypeOf
    set_prototype: OnceCell<napi_ref>,
    /// toggle-reference data of native classes' instances
    native: OnceCell<&'static ClassDef>,
    wrappers: RefCell<HashMap<GType, Wrapper, nativescript_core::native::FxBuildHasher>>,
    protos: RefCell<HashMap<GType, napi_ref, nativescript_core::native::FxBuildHasher>>,
    /// struct name -> the GObject type or interface it names, 0 for other structs
    pointees: RefCell<HashMap<String, GType, nativescript_core::native::FxBuildHasher>>,
    /// struct name -> its GType, found through `*_get_type` when not registered yet
    named: RefCell<HashMap<String, GType, nativescript_core::native::FxBuildHasher>>,
}

#[derive(Clone, Copy)]
enum Wrapper {
    NotObject,
    JsClass,
    /// instances are interop.Pointers with the class's prototype
    Native(napi_ref),
}

pub struct Host {
    state: *const State,
    thread: std::thread::ThreadId,
    context: usize,
    alive: std::sync::atomic::AtomicBool,
}

unsafe impl Sync for Host {}

impl Host {
    fn on_thread(&self, f: impl FnOnce(&State) + Send) {
        if !self.alive.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let st = unsafe { &*self.state };
        if std::thread::current().id() == self.thread {
            return f(st);
        }
        let st_addr = self.state as usize;
        let done = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let signal = done.clone();
        let f: Box<dyn FnOnce() + Send + '_> = Box::new(move || {
            f(unsafe { &*(st_addr as *const State) });
            *signal.0.lock().unwrap() = true;
            signal.1.notify_all();
        });
        // SAFETY: this thread waits for it below
        let f: Box<dyn FnOnce() + Send> = unsafe { std::mem::transmute(f) };
        nativescript_core::glib::invoke(self.context, f);
        let mut finished = done.0.lock().unwrap();
        while !*finished {
            finished = done.1.wait(finished).unwrap();
        }
    }

    /// `f` in a handle scope, reporting what it throws
    fn run(&self, f: impl FnOnce(Js, &State) + Send) {
        self.on_thread(|st| {
            let js = st.js();
            let mut scope = null_mut();
            unsafe { napi_open_handle_scope(js.0, &mut scope) };
            f(js, st);
            crate::event_loop::report_pending(st);
            unsafe { napi_close_handle_scope(js.0, scope) };
        });
    }
}

impl gobj::Host for Host {
    fn get_property(&self, class: u32, object: usize, index: usize, value: *mut GValue) {
        let value = value as usize;
        self.run(move |js, st| {
            let Some(obj) = instance(st, object, false) else { return };
            let key = prop_key(st, class, index);
            let Some(v) = get(js, obj, key) else { return };
            if js.type_of(v) != UNDEFINED {
                if let Err(e) = unsafe { set_value(st, value as *mut GValue, v) } {
                    js.throw(&e);
                }
            }
        });
    }

    fn set_property(&self, class: u32, object: usize, index: usize, value: *const GValue) {
        let value = value as usize;
        self.run(move |js, st| {
            let Some(obj) = instance(st, object, false) else { return };
            let v = to_js(st, unsafe { gobj::read(value as *const GValue) });
            let key = prop_key(st, class, index);
            unsafe { napi_set_property(js.0, obj, key, v) };
        });
    }

    fn finalize(&self, object: usize) {
        self.on_thread(move |st| {
            let entry = st.gobjects.instances.borrow_mut().remove(&object);
            if let Some(e) = entry {
                st.js().delete_ref(e.object);
            }
        });
    }

    fn toggle(&self, object: usize, is_last: bool) {
        self.on_thread(move |st| {
            let js = st.js();
            let mut instances = st.gobjects.instances.borrow_mut();
            let Some(entry) = instances.get_mut(&object) else { return };
            if is_last == entry.strong {
                let mut count = 0;
                unsafe {
                    if is_last {
                        napi_reference_unref(js.0, entry.object, &mut count);
                    } else {
                        napi_reference_ref(js.0, entry.object, &mut count);
                    }
                }
                entry.strong = !is_last;
            }
        });
    }
}

fn prop_key(st: &State, class: u32, index: usize) -> napi_value {
    let js = st.js();
    let (keys, custom) = {
        let classes = st.gobjects.classes.borrow();
        let c = &classes[class as usize];
        (js.deref(c.keys), c.custom[index])
    };
    let mut key = null_mut();
    unsafe { napi_get_element(js.0, keys, (2 * index + !custom as usize) as u32, &mut key) };
    key
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

fn object_fn(st: &State, cell: &OnceCell<napi_ref>, name: &str) -> napi_value {
    let js = st.js();
    let r = *cell.get_or_init(|| {
        let object = js.get(js.global(), "Object").unwrap_or(null_mut());
        js.reference(js.get(object, name).unwrap_or(null_mut()), 1)
    });
    js.deref(r)
}

fn set_prototype(st: &State, object: napi_value, proto: napi_value) {
    let js = st.js();
    let f = object_fn(st, &st.gobjects.set_prototype, "setPrototypeOf");
    let _ = js.call(f, js.undefined(), &[object, proto]);
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
pub fn read_pointer(st: &State, ty: &Type, address: usize) -> Option<napi_value> {
    let name = pointee(ty)?;
    if pointee_gtype(st, name) == 0 {
        return None;
    }
    wrap(st, address, false)
}

/// The JS object of the GObject at `address`: the instance of its JS class, or an
/// interop.Pointer with its native class's prototype, one per GObject while JS holds it.
/// `owned`: the caller's reference passes to the JS object. `None` when it isn't a GObject.
pub fn wrap(st: &State, address: usize, owned: bool) -> Option<napi_value> {
    let release = |owned: bool| {
        if owned {
            unsafe { gobj::g_object_unref(address as _) };
        }
    };
    if let Some(o) = live(st, address) {
        release(owned);
        return Some(o);
    }
    let proto = match wrapper(st, gobj::instance_type(address)) {
        Wrapper::NotObject => return None,
        Wrapper::JsClass => {
            let o = instance(st, address, false);
            release(owned);
            return o;
        }
        Wrapper::Native(proto) => proto,
    };
    // an interop.Pointer instance keeps its handle where engines find it fastest (V8's
    // napi_define_class instances: an internal field rather than a private property)
    let object = interop::new_pointer(st, address, None);
    let h = crate::state::handle_of(st.js(), object, Kind::Pointer)?;
    let token = Rc::new(Token { address });
    h.token = Some(token.clone());
    set_prototype(st, object, st.js().deref(proto));
    // without ownership annotations, take a reference of our own: a floating one (a new
    // widget) becomes it, a borrowed object stays referenced by whoever else holds it
    let floating = unsafe { gobj::g_object_is_floating(address as _) } != 0;
    if floating {
        unsafe { gobj::g_object_ref_sink(address as _) };
    }
    track(st, address, object, token, native_def(st));
    release(owned || floating);
    Some(object)
}

fn wrapper(st: &State, gtype: GType) -> Wrapper {
    if let Some(&w) = st.gobjects.wrappers.borrow().get(&gtype) {
        return w;
    }
    let ours = host(st) as *const Host as *const u8;
    let mut js_class = false;
    let mut t = gtype;
    while t != 0 && !js_class {
        js_class = gobj::class_def(t).is_some_and(|d| std::ptr::eq(d.host as *const dyn gobj::Host as *const u8, ours));
        t = gobj::type_parent(t);
    }
    let w = if !gobj::is_a(gtype, gobj::G_TYPE_OBJECT) {
        Wrapper::NotObject
    } else if js_class {
        Wrapper::JsClass
    } else {
        match class_proto(st, gtype) {
            Some(_) => Wrapper::Native(st.gobjects.protos.borrow()[&gtype]),
            None => Wrapper::NotObject,
        }
    };
    st.gobjects.wrappers.borrow_mut().insert(gtype, w);
    w
}

/// The prototype of a native class's instances: its struct constructor's (`GtkLabel.prototype`)
/// when the metadata has one, inheriting the parent class's, and interop.Pointer's at the root.
fn class_proto(st: &State, gtype: GType) -> Option<napi_value> {
    let js = st.js();
    if let Some(&p) = st.gobjects.protos.borrow().get(&gtype) {
        return Some(js.deref(p));
    }
    let parent = match gobj::type_parent(gtype) {
        0 => js.get(js.deref(st.pointer_ctor.get()), "prototype").ok()?,
        p => class_proto(st, p)?,
    };
    let proto = constructor_proto(st, gtype).unwrap_or_else(|| js.object());
    let mut current = null_mut();
    unsafe { napi_get_prototype(js.0, proto, &mut current) };
    let mut object_proto = null_mut();
    unsafe { napi_get_prototype(js.0, js.object(), &mut object_proto) };
    if js.strict_equals(current, object_proto) {
        set_prototype(st, proto, parent);
    }
    st.gobjects.protos.borrow_mut().insert(gtype, js.reference(proto, 1));
    let _ = interop::gobject_hook(js, "populate", &[proto, js.number(gtype as f64)]);
    Some(proto)
}

fn constructor_proto(st: &State, gtype: GType) -> Option<napi_value> {
    let js = st.js();
    let name = gobj::type_name(gtype).to_str().ok()?;
    for name in [name.to_owned(), format!("_{name}")] {
        let Some(ctor) = js.get(js.global(), &name).ok().filter(|v| js.type_of(*v) == FUNCTION) else {
            continue;
        };
        if !matches!(interop::type_from_value(st, ctor), Some(Type::Struct(_))) {
            continue;
        }
        return js.get(ctor, "prototype").ok();
    }
    None
}

/// The GType of the struct `name`, registering it through its `*_get_type` function; 0
/// for structs that aren't GTypes.
fn gtype_named(st: &State, name: &str) -> GType {
    if let Some(&gtype) = st.gobjects.named.borrow().get(name) {
        return gtype;
    }
    let js = st.js();
    let bare = name.strip_prefix('_').unwrap_or(name);
    let mut gtype = CString::new(bare).map_or(0, |c| gobj::type_from_name(&c));
    if gtype == 0 {
        if let Ok(Some(v)) = interop::gobject_hook(js, "gtype", &[js.string(bare)]) {
            gtype = gtype_of(js, v);
        }
    }
    st.gobjects.named.borrow_mut().insert(name.to_owned(), gtype);
    gtype
}

/// `new GtkLabel(props)`: a new instance of a native GObject class (`Ok(None)` for other
/// structs).
pub fn construct_native(st: &State, ty: &Type, props: napi_value) -> Result<Option<napi_value>, String> {
    let Type::Struct(name) = ty else { return Ok(None) };
    let gtype = gtype_named(st, name);
    if gtype == 0 || !gobj::is_a(gtype, gobj::G_TYPE_OBJECT) {
        return Ok(None);
    }
    if gobj::is_abstract(gtype) {
        return Err(format!("{name} is abstract: extend it, or create one with a C constructor"));
    }
    let address = create(st, gtype, props)?;
    if address == 0 {
        return Err(format!("failed to create a {name}"));
    }
    let owned = gobj::new_is_owned(gtype, address);
    wrap(st, address, owned).map(Some).ok_or_else(|| format!("failed to create a {name}"))
}

fn get(js: Js, object: napi_value, key: napi_value) -> Option<napi_value> {
    let mut v = null_mut();
    (unsafe { napi_get_property(js.0, object, key, &mut v) } == 0).then_some(v)
}

fn class_key(st: &State) -> napi_value {
    let js = st.js();
    let r = *st.gobjects.key.get_or_init(|| {
        let mut symbol = null_mut();
        unsafe { napi_create_symbol(js.0, js.string("gobject-class"), &mut symbol) };
        js.reference(js.array(&[symbol]), 1)
    });
    let mut symbol = null_mut();
    unsafe { napi_get_element(js.0, js.deref(r), 0, &mut symbol) };
    symbol
}

fn tag(st: &State, cls: napi_value) -> Option<u32> {
    let js = st.js();
    let key = class_key(st);
    let mut has = false;
    unsafe { napi_has_own_property(js.0, cls, key, &mut has) };
    if !has {
        return None;
    }
    get(js, cls, key).and_then(|v| js.f64_of(v).ok()).map(|n| n as u32)
}

/// The JS object of a GObject created by a JS class (made when needed), else None.
pub fn instance(st: &State, address: usize, dispose: bool) -> Option<napi_value> {
    if let Some(o) = live(st, address) {
        return Some(o);
    }
    let def = gobj::instance_class(address)?;
    if !std::ptr::eq(def.host as *const dyn gobj::Host as *const u8, host(st) as *const Host as *const u8) {
        return None;
    }
    let js = st.js();
    if dispose || st.gobjects.disposing.borrow().contains(&address) {
        return bare(st, def.class, address, None);
    }
    let gtype = gobj::instance_type(address);
    let token = Rc::new(Token { address });
    let object = bare(st, def.class, address, Some(token.clone()))?;
    {
        let mut frames = st.gobjects.constructing.borrow_mut();
        if let Some(frame) = frames.last_mut().filter(|f| f.0 == gtype && f.1.is_none()) {
            frame.1 = Some(js.reference(object, 1));
            drop(frames);
            track(st, address, object, token, def);
            return Some(object);
        }
    }
    // created by native code (GtkBuilder, g_object_new): run the class's constructor
    track(st, address, object, token, def);
    let cls = js.deref(st.gobjects.classes.borrow()[def.class as usize].cls);
    st.gobjects.adopting.borrow_mut().push(js.reference(object, 1));
    if js.new_instance(cls, &[]).is_err() {
        crate::event_loop::report_pending(st);
    }
    let mut adopting = st.gobjects.adopting.borrow_mut();
    if let Some(i) = adopting.iter().position(|r| js.strict_equals(js.deref(*r), object)) {
        js.delete_ref(adopting.remove(i));
    }
    Some(object)
}

fn live(st: &State, address: usize) -> Option<napi_value> {
    let instances = st.gobjects.instances.borrow();
    let entry = instances.get(&address)?;
    let v = st.js().deref(entry.object);
    (!v.is_null()).then_some(v)
}

fn bare(st: &State, class: u32, address: usize, token: Option<Rc<Token>>) -> Option<napi_value> {
    let js = st.js();
    let (proto, base) = {
        let classes = st.gobjects.classes.borrow();
        let c = &classes[class as usize];
        (js.deref(c.proto), c.base)
    };
    let create = object_fn(st, &st.gobjects.create, "create");
    let object = js.call(create, js.undefined(), &[proto]).ok()?;
    let h = attach(js, object, Handle::new(Kind::Struct, address, Some(base), null_mut()));
    unsafe { (*h).token = token };
    Some(object)
}

fn track(st: &State, address: usize, object: napi_value, token: Rc<Token>, def: &'static ClassDef) {
    let js = st.js();
    let entry = Entry { object: js.reference(object, 1), strong: true, token, def };
    let previous = st.gobjects.instances.borrow_mut().insert(address, entry);
    match previous {
        Some(p) => js.delete_ref(p.object),
        None => {
            let data = def as *const ClassDef as *mut c_void;
            unsafe { gobj::g_object_add_toggle_ref(address as _, gobj::toggle_notify, data) };
        }
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
        for token in collected {
            let address = token.address;
            let entry = {
                let mut instances = st.gobjects.instances.borrow_mut();
                // a newer JS object may have replaced the collected one
                match instances.get(&address) {
                    Some(e) if Rc::ptr_eq(&e.token, &token) && !e.strong => instances.remove(&address),
                    _ => None,
                }
            };
            if let Some(entry) = entry {
                st.js().delete_ref(entry.object);
                let data = entry.def as *const ClassDef as *mut c_void;
                unsafe { gobj::g_object_remove_toggle_ref(address as _, gobj::toggle_notify, data) };
            }
        }
    }
}

/// The runtime goes: its classes stop calling into it, and its JS objects let go.
pub fn shutdown(st: &State) {
    if let Some(host) = st.gobjects.host.get() {
        host.alive.store(false, std::sync::atomic::Ordering::Release);
    }
    let js = st.js();
    for (_, e) in st.gobjects.instances.borrow_mut().drain() {
        js.delete_ref(e.object);
    }
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

/// `new X()` / `super()` of a JS GObject class `target`: `Ok(None)` when it isn't one,
/// `Err("")` when a JS exception is pending.
pub fn construct(st: &State, target: napi_value, props: napi_value) -> Result<Option<napi_value>, String> {
    let js = st.js();
    let class = match tag(st, target) {
        Some(c) => c,
        None => {
            match crate::interop::gobject_hook(js, "register", &[target]) {
                Ok(Some(v)) if js.type_of(v) == BOOLEAN && js.bool_of(v) => {}
                Ok(_) => return Ok(None),
                Err(()) => return Err(String::new()),
            }
            tag(st, target).ok_or("the class wasn't registered")?
        }
    };
    let (gtype, def, proto) = {
        let classes = st.gobjects.classes.borrow();
        let c = &classes[class as usize];
        (c.gtype, c.def, js.deref(c.proto))
    };
    // the class's constructor, running for a GObject native code created
    let adopted = st.gobjects.adopting.borrow().last().map(|r| js.deref(*r));
    if let Some(adopted) = adopted {
        let mut p = null_mut();
        unsafe { napi_get_prototype(js.0, adopted, &mut p) };
        if js.strict_equals(p, proto) {
            if let Some(r) = st.gobjects.adopting.borrow_mut().pop() {
                js.delete_ref(r);
            }
            return Ok(Some(adopted));
        }
    }
    st.gobjects.constructing.borrow_mut().push((gtype, None));
    let created = create(st, gtype, props);
    let frame = st.gobjects.constructing.borrow_mut().pop();
    let address = created?;
    let object = match frame.and_then(|f| f.1) {
        Some(r) => {
            let o = js.deref(r);
            js.delete_ref(r);
            o
        }
        None => {
            let token = Rc::new(Token { address });
            let object = bare(st, class, address, Some(token.clone())).ok_or("failed to create the JS object")?;
            track(st, address, object, token, def);
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
    Ok(Some(object))
}

fn is_plain_object(js: Js, v: napi_value) -> bool {
    if js.type_of(v) != OBJECT || handle(js, v).is_some() {
        return false;
    }
    let mut is_array = false;
    unsafe { napi_is_array(js.0, v, &mut is_array) };
    if is_array {
        return false;
    }
    let mut proto = null_mut();
    unsafe { napi_get_prototype(js.0, v, &mut proto) };
    if js.type_of(proto) == NULL {
        return true;
    }
    let mut object_proto = null_mut();
    unsafe { napi_get_prototype(js.0, js.object(), &mut object_proto) };
    js.strict_equals(proto, object_proto)
}

fn elements(js: Js, v: napi_value) -> Vec<napi_value> {
    let mut is_array = false;
    unsafe { napi_is_array(js.0, v, &mut is_array) };
    if !is_array {
        return Vec::new();
    }
    let mut n = 0;
    unsafe { napi_get_array_length(js.0, v, &mut n) };
    (0..n)
        .map(|i| {
            let mut e = null_mut();
            unsafe { napi_get_element(js.0, v, i, &mut e) };
            e
        })
        .collect()
}

fn create(st: &State, gtype: GType, props: napi_value) -> Result<usize, String> {
    let js = st.js();
    if !is_plain_object(js, props) {
        return Ok(gobj::new_object(gtype, &[], &Values(Vec::new())));
    }
    let mut keys = null_mut();
    unsafe { napi_get_property_names(js.0, props, &mut keys) };
    let keys = elements(js, keys);
    let mut names = Vec::with_capacity(keys.len());
    let mut types = Vec::with_capacity(keys.len());
    let mut items = Vec::with_capacity(keys.len());
    for key in keys {
        let name = js.to_string(key);
        let canonical = CString::new(gobj::canonical_name(&name)).map_err(|e| e.to_string())?;
        let pspec = gobj::find_property(gtype, &canonical).ok_or_else(|| format!("the class has no property '{name}'"))?;
        types.push(gobj::param_value_type(pspec));
        names.push(canonical);
        items.push(get(js, props, key).ok_or("failed to read the properties")?);
    }
    let mut values = Values::with_types(&types);
    for (value, item) in values.0.iter_mut().zip(items) {
        unsafe { set_value(st, value, item)? };
    }
    Ok(gobj::new_object(gtype, &names, &values))
}

/// # Safety
/// `value` is an initialized GValue.
pub unsafe fn set_value(st: &State, value: *mut GValue, v: napi_value) -> Result<(), String> {
    let js = st.js();
    let gtype = unsafe { (*value).g_type };
    let want = gobj::want(gtype).ok_or_else(|| format!("values of GType {gtype} are not supported"))?;
    let text;
    let input = match want {
        Want::Bool => In::Bool(js.bool_of(v)),
        Want::Number => In::Number(js.f64_of(v).unwrap_or(0.0)),
        Want::Int64 | Want::UInt64 => match js.type_of(v) {
            BIGINT if matches!(want, Want::UInt64) => In::BigUint(js.bigint_u64_of(v).unwrap_or(0)),
            BIGINT => In::BigInt(js.bigint_i64(v).unwrap_or(0)),
            _ => In::Number(js.f64_of(v).unwrap_or(0.0)),
        },
        Want::Str => match js.type_of(v) {
            UNDEFINED | NULL => In::Str(None),
            _ => {
                text = CString::new(js.to_string(v).replace('\0', "")).unwrap();
                In::Str(Some(&text))
            }
        },
        Want::Address => In::Address(address_of(js, v)?),
    };
    unsafe { gobj::write(value, input) }
}

fn address_of(js: Js, v: napi_value) -> Result<usize, String> {
    match js.type_of(v) {
        UNDEFINED | NULL => Ok(0),
        _ => match handle(js, v) {
            Some(h) if matches!(h.kind, Kind::Pointer | Kind::Struct | Kind::Reference) => Ok(h.address),
            _ => Err("expected a GObject, Pointer or null".into()),
        },
    }
}

pub fn to_js(st: &State, v: Val) -> napi_value {
    let js = st.js();
    match v {
        Val::None => js.undefined(),
        Val::Bool(b) => js.boolean(b),
        Val::I64(n) => js.i64(n),
        Val::U64(n) => js.u64(n),
        Val::F64(n) => js.number(n),
        Val::Str(None) | Val::Object(0) | Val::Pointer(0) => js.null(),
        Val::Str(Some(s)) => js.string(&s.to_string_lossy()),
        Val::Object(a) => wrap(st, a, false).unwrap_or_else(|| interop::new_pointer(st, a, None)),
        Val::Pointer(a) => interop::new_pointer(st, a, None),
    }
}

fn gtype_of(js: Js, v: napi_value) -> GType {
    match js.type_of(v) {
        BIGINT => js.bigint_u64_of(v).unwrap_or(0) as GType,
        _ => js.f64_of(v).unwrap_or(0.0) as GType,
    }
}

fn slots(st: &State, proto: napi_value, list: Vec<napi_value>) -> Result<Vec<gobj::Slot>, String> {
    let js = st.js();
    let mut out = Vec::new();
    for item in list {
        let item = elements(js, item);
        let (Some(&method), Some(&record), Some(&field)) = (item.first(), item.get(1), item.get(2)) else {
            return Err("invalid vtable slot".into());
        };
        let ty = interop::type_from_value(st, record).ok_or("not a struct constructor")?;
        let layout = st.record(&ty)?;
        let field = js.to_string(field);
        let f = layout.fields.iter().find(|f| f.name == field).ok_or_else(|| format!("no field {field}"))?;
        let Type::FunctionPointer(sig) = &f.ty else {
            return Err(format!("{field} is not a function pointer"));
        };
        let name = js.to_string(method);
        let function = get(js, proto, method).filter(|f| js.type_of(*f) == FUNCTION).ok_or_else(|| format!("{name} is not a method"))?;
        let code = ffi::method_closure(st, function, sig, name == "dispose")?;
        out.push(gobj::Slot { offset: f.offset, code });
    }
    Ok(out)
}

fn register_class(st: &State, cls: napi_value, desc: napi_value) -> Result<GType, String> {
    let js = st.js();
    if js.type_of(cls) != FUNCTION || js.type_of(desc) != OBJECT {
        return Err("expected a class and its description".into());
    }
    let field = |name: &str| js.get(desc, name).unwrap_or(null_mut());
    let parent = gtype_of(js, field("parent"));
    let name = js.to_string(field("name"));
    let base = interop::type_from_value(st, field("base")).ok_or("the base isn't a struct constructor")?;
    let base = st.register_type(&base);
    let root = js.bool_of(field("root"));
    if root {
        class_proto(st, parent).ok_or("the parent class has no prototype")?;
    }
    let proto = js.get(cls, "prototype").map_err(|_| "the class has no prototype")?;
    let class = st.gobjects.classes.borrow().len() as u32;
    let mut def = ClassDef::new(host(st), class, root);
    def.vfuncs = slots(st, proto, elements(js, field("vfuncs")))?;
    for iface in elements(js, field("interfaces")) {
        let item = elements(js, iface);
        let gtype = item.first().map(|v| gtype_of(js, *v)).unwrap_or(0);
        let list = item.get(1).map(|v| elements(js, *v)).unwrap_or_default();
        def.interfaces.push(gobj::Interface { gtype, slots: slots(st, proto, list)? });
    }
    let mut custom_list = Vec::new();
    let mut keys = Vec::new();
    for p in elements(js, field("properties")) {
        let item = elements(js, p);
        let (Some(&key), Some(&slot)) = (item.first(), item.get(1)) else {
            return Err("invalid property".into());
        };
        let custom = item.get(2).is_some_and(|v| js.bool_of(*v));
        let pspec = item.get(3).and_then(|v| crate::state::handle_of(js, *v, Kind::Pointer)).map(|h| h.address);
        def.properties.push(match pspec {
            Some(pspec) => gobj::Property::Install(pspec),
            None => {
                let name = item.get(4).map(|v| js.to_string(*v)).unwrap_or_default();
                gobj::Property::Override(CString::new(name).map_err(|e| e.to_string())?)
            }
        });
        custom_list.push(custom);
        keys.extend([key, slot]);
    }
    for s in elements(js, field("signals")) {
        let item = elements(js, s);
        let name = item.first().map(|v| js.to_string(*v)).unwrap_or_default();
        let flags = item.get(1).and_then(|v| js.f64_of(*v).ok()).unwrap_or(2.0) as u32;
        let return_type = item.get(2).map(|v| gtype_of(js, *v)).unwrap_or(gobj::G_TYPE_NONE);
        let params = item.get(3).map(|v| elements(js, *v)).unwrap_or_default().into_iter().map(|v| gtype_of(js, v)).collect();
        def.signals.push(gobj::Signal { name: CString::new(name).map_err(|e| e.to_string())?, flags, return_type, params });
    }
    let cname = CString::new(name.clone()).map_err(|e| e.to_string())?;
    let gtype = gobj::register(parent, &cname, def)?;
    let def = gobj::class_def(gtype).ok_or("registration failed")?;
    st.gobjects.classes.borrow_mut().push(Class {
        proto: js.reference(proto, 1),
        cls: js.reference(cls, 1),
        base,
        gtype,
        def,
        custom: custom_list,
        keys: js.reference(js.array(&keys), 1),
    });
    let key = class_key(st);
    let desc = napi_property_descriptor {
        utf8name: std::ptr::null(),
        name: key,
        method: None,
        getter: None,
        setter: None,
        value: js.uint(class),
        attributes: 0,
        data: null_mut(),
    };
    unsafe { napi_define_properties(js.0, cls, 1, &desc) };
    Ok(gtype)
}

unsafe extern "C" fn register(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = cb_args::<2>(env, info);
    match register_class(st, argv[0], argv[1]) {
        Ok(gtype) => st.js().number(gtype as f64),
        Err(e) => {
            st.js().throw(&e);
            null_mut()
        }
    }
}

unsafe extern "C" fn emit(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, argc, _, _) = cb_args::<16>(env, info);
    match emit_signal(st, &argv[..argc.min(16)]) {
        Ok(v) => v,
        Err(e) => {
            st.js().throw(&e);
            null_mut()
        }
    }
}

fn emit_signal(st: &State, args: &[napi_value]) -> Result<napi_value, String> {
    let js = st.js();
    let address = address_of(js, *args.first().ok_or("emit needs an instance")?)?;
    if address == 0 {
        return Err("emit needs an instance".into());
    }
    let gtype = gobj::instance_type(address);
    let name = js.to_string(*args.get(1).ok_or("emit needs a signal name")?);
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
        let arg = args.get(i + 2).copied().unwrap_or_else(|| js.undefined());
        unsafe { set_value(st, value, arg)? };
    }
    if signal.return_type == gobj::G_TYPE_NONE {
        gobj::emit(&values.0, &signal, None);
        return Ok(js.undefined());
    }
    let mut ret = Values::with_types(&[signal.return_type]);
    gobj::emit(&values.0, &signal, Some(&mut ret.0[0]));
    Ok(to_js(st, unsafe { gobj::read(&ret.0[0]) }))
}

unsafe extern "C" fn is_slot(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = cb_args::<2>(env, info);
    let js = st.js();
    let field = js.to_string(argv[1]);
    let slot = interop::type_from_value(st, argv[0])
        .and_then(|ty| st.record(&ty).ok())
        .is_some_and(|r| r.fields.iter().any(|f| f.name == field && matches!(f.ty, Type::FunctionPointer(_))));
    js.boolean(slot)
}

struct HandlerNotify {
    host: &'static Host,
    closure: usize,
}

/// GClosure finalize notifier of a signal handler: disconnected, or its instance finalized.
unsafe extern "C" fn handler_finalized(data: *mut c_void, _closure: *mut c_void) {
    let notify = unsafe { Box::from_raw(data as *mut HandlerNotify) };
    let closure = notify.closure;
    notify.host.on_thread(move |st| ffi::release_handler(st, closure));
}

/// `natives.gobjectHandler(function, { returns, args })`: [callback, notify, notify data]
/// for a GClosure of its own, freed with it.
unsafe extern "C" fn handler(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<2>(env, info);
    if js.type_of(argv[0]) != FUNCTION {
        js.throw("expected a function");
        return null_mut();
    }
    let made = interop::signature_from_spec(st, argv[1]).and_then(|signature| ffi::handler_closure(st, argv[0], &signature));
    let (code, closure) = match made {
        Ok(m) => m,
        Err(e) => {
            js.throw(&e);
            return null_mut();
        }
    };
    let notify = Box::into_raw(Box::new(HandlerNotify { host: host(st), closure }));
    let items = [code, handler_finalized as *const () as usize, notify as usize].map(|a| interop::new_pointer(st, a, None));
    js.array(&items)
}

/// `natives.gobjectMethods(typeName)`: [[method, function], ...]
unsafe extern "C" fn methods(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<1>(env, info);
    let name = js.to_string(argv[0]);
    let list = gobj::function_prefix(&st.meta, &name).map(|p| gobj::methods(st, &p, &name)).unwrap_or_default();
    let items: Vec<napi_value> = list
        .iter()
        .map(|m| js.array(&[js.string(&m.name), js.string(&m.function)]))
        .collect();
    js.array(&items)
}

/// `natives.gobjectMethod(functionName)`: the C function as a method, its receiver the
/// first argument.
unsafe extern "C" fn method(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<1>(env, info);
    let name = js.to_string(argv[0]);
    let function = st
        .meta
        .find(&name)
        .filter(|m| m.kind() == nativescript_metadata::MetaKind::Function)
        .and_then(|m| ffi::function_for(st, st.meta.id(&m)).ok());
    match function {
        Some(f) => ffi::js_method(st, &name, &f),
        None => js.undefined(),
    }
}

/// `natives.gobjectProperties(gtype)`: [[camelCaseName, pspec], ...] the type installs.
unsafe extern "C" fn properties(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<1>(env, info);
    let items: Vec<napi_value> = gobj::own_properties(gtype_of(js, argv[0]))
        .into_iter()
        .map(|(name, pspec)| js.array(&[js.string(&gobj::camel_name(&name)), interop::new_pointer(st, pspec, None)]))
        .collect();
    js.array(&items)
}

/// `natives.gobjectInterfaces(gtype)`: the interfaces it adds to its parent's.
unsafe extern "C" fn interfaces(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<1>(env, info);
    let items: Vec<napi_value> = gobj::own_interfaces(gtype_of(js, argv[0])).into_iter().map(|t| js.number(t as f64)).collect();
    js.array(&items)
}

fn object_and_pspec(st: &State, object: napi_value, pspec: napi_value) -> Option<(usize, usize)> {
    let js = st.js();
    let object = address_of(js, object).ok().filter(|a| *a != 0)?;
    let pspec = crate::state::handle_of(js, pspec, Kind::Pointer)?.address;
    Some((object, pspec))
}

/// `natives.gobjectGet(object, pspec)`
unsafe extern "C" fn get_prop(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = cb_args::<2>(env, info);
    let Some((object, pspec)) = object_and_pspec(st, argv[0], argv[1]) else {
        st.js().throw("expected a GObject and a GParamSpec");
        return null_mut();
    };
    let value = gobj::read_property(object, pspec);
    to_js(st, unsafe { gobj::read(&value.0[0]) })
}

/// `natives.gobjectSet(object, pspec, value)`
unsafe extern "C" fn set_prop(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let (argv, _, _, _) = cb_args::<3>(env, info);
    let Some((object, pspec)) = object_and_pspec(st, argv[0], argv[1]) else {
        js.throw("expected a GObject and a GParamSpec");
        return null_mut();
    };
    let mut value = gobj::property_value(pspec);
    match unsafe { set_value(st, &mut value.0[0], argv[2]) } {
        Ok(()) => gobj::write_property(object, pspec, &value.0[0]),
        Err(e) => js.throw(&e),
    }
    js.undefined()
}

/// `natives.gobjectFields(structConstructor)`: the prototype of the struct's views, with
/// its field accessors.
unsafe extern "C" fn fields(env: napi_env, info: napi_callback_info) -> napi_value {
    let st = state(env);
    let (argv, _, _, _) = cb_args::<1>(env, info);
    let js = st.js();
    let view = interop::type_from_value(st, argv[0]).and_then(|ty| crate::structs::view(st, &ty, 0, None).ok());
    match view {
        Some(v) => {
            let mut proto = null_mut();
            unsafe { napi_get_prototype(js.0, v, &mut proto) };
            proto
        }
        None => {
            js.throw("expected a struct constructor");
            null_mut()
        }
    }
}

/// `natives.gobjectPointerType()`: an interop type for `GObject *` values, read as their
/// JS objects.
unsafe extern "C" fn pointer_type(env: napi_env, _info: napi_callback_info) -> napi_value {
    let st = state(env);
    let js = st.js();
    let descriptor = js.object();
    unsafe { napi_set_named_property(js.0, descriptor, c"name".as_ptr(), js.string("GObject*")) };
    interop::tag_type(st, descriptor, &Type::Pointer(Box::new(Type::Struct("GObject".into()))));
    descriptor
}

pub const NATIVES: [(&str, unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value); 12] = [
    ("gobjectHandler", handler),
    ("gobjectMethods", methods),
    ("gobjectMethod", method),
    ("gobjectProperties", properties),
    ("gobjectInterfaces", interfaces),
    ("gobjectGet", get_prop),
    ("gobjectSet", set_prop),
    ("gobjectRegister", register),
    ("gobjectEmit", emit),
    ("gobjectSlot", is_slot),
    ("gobjectFields", fields),
    ("gobjectPointerType", pointer_type),
];
