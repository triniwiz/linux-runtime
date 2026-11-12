//! GObject classes implemented by a JS engine: GType registration, class and interface
//! initialization, the property / finalize / toggle-notify handlers, and GValues.
//! Vtable slots hold C function pointers the engine made for the class's methods.

use crate::glib::gpointer;
use std::ffi::{c_char, CStr};
use std::sync::OnceLock;

pub type GType = usize;

#[repr(C)]
#[derive(Default)]
pub struct GValue {
    pub g_type: GType,
    pub data: [u64; 2],
}

#[repr(C)]
struct GTypeQuery {
    type_: GType,
    type_name: *const c_char,
    class_size: u32,
    instance_size: u32,
}

#[repr(C)]
struct GTypeInfo {
    class_size: u16,
    base_init: usize,
    base_finalize: usize,
    class_init: Option<unsafe extern "C" fn(gpointer, gpointer)>,
    class_finalize: usize,
    class_data: gpointer,
    instance_size: u16,
    n_preallocs: u16,
    instance_init: usize,
    value_table: usize,
}

#[repr(C)]
struct GInterfaceInfo {
    interface_init: Option<unsafe extern "C" fn(gpointer, gpointer)>,
    interface_finalize: usize,
    interface_data: gpointer,
}

type ObjectFn = unsafe extern "C" fn(gpointer);
type PropertyFn = unsafe extern "C" fn(gpointer, u32, *mut GValue, gpointer);

#[repr(C)]
struct GObjectClass {
    g_type: GType,
    construct_properties: gpointer,
    constructor: usize,
    set_property: Option<PropertyFn>,
    get_property: Option<PropertyFn>,
    dispose: Option<ObjectFn>,
    finalize: Option<ObjectFn>,
}

#[repr(C)]
struct GParamSpec {
    g_type_instance: gpointer,
    name: *const c_char,
    flags: u32,
    value_type: GType,
    owner_type: GType,
}

#[repr(C)]
struct GSignalQuery {
    signal_id: u32,
    signal_name: *const c_char,
    itype: GType,
    signal_flags: u32,
    return_type: GType,
    n_params: u32,
    param_types: *const GType,
}

pub type ToggleNotify = unsafe extern "C" fn(gpointer, gpointer, i32);

#[link(name = "gobject-2.0")]
extern "C" {
    fn g_type_query(type_: GType, query: *mut GTypeQuery);
    fn g_type_register_static(parent: GType, name: *const c_char, info: *const GTypeInfo, flags: u32) -> GType;
    fn g_type_add_interface_static(type_: GType, iface: GType, info: *const GInterfaceInfo);
    fn g_type_set_qdata(type_: GType, quark: u32, data: gpointer);
    fn g_type_get_qdata(type_: GType, quark: u32) -> gpointer;
    fn g_type_parent(type_: GType) -> GType;
    fn g_type_name(type_: GType) -> *const c_char;
    fn g_type_test_flags(type_: GType, flags: u32) -> i32;
    fn g_type_fundamental(type_: GType) -> GType;
    fn g_type_from_name(name: *const c_char) -> GType;
    fn g_type_class_ref(type_: GType) -> gpointer;
    fn g_type_class_unref(class: gpointer);
    fn g_object_class_install_property(class: gpointer, id: u32, pspec: gpointer);
    fn g_object_class_override_property(class: gpointer, id: u32, name: *const c_char);
    fn g_object_class_find_property(class: gpointer, name: *const c_char) -> gpointer;
    fn g_signal_newv(
        name: *const c_char,
        itype: GType,
        flags: u32,
        class_closure: gpointer,
        accumulator: gpointer,
        accu_data: gpointer,
        c_marshaller: gpointer,
        return_type: GType,
        n_params: u32,
        param_types: *const GType,
    ) -> u32;
    fn g_signal_parse_name(detailed: *const c_char, itype: GType, id: *mut u32, detail: *mut u32, force: i32) -> i32;
    fn g_signal_query(id: u32, query: *mut GSignalQuery);
    fn g_signal_emitv(params: *const GValue, id: u32, detail: u32, ret: *mut GValue);
    pub fn g_object_new_with_properties(
        type_: GType,
        n: u32,
        names: *const *const c_char,
        values: *const GValue,
    ) -> gpointer;
    pub fn g_object_is_floating(object: gpointer) -> i32;
    pub fn g_object_ref_sink(object: gpointer) -> gpointer;
    pub fn g_object_unref(object: gpointer);
    pub fn g_object_add_toggle_ref(object: gpointer, notify: ToggleNotify, data: gpointer);
    pub fn g_object_remove_toggle_ref(object: gpointer, notify: ToggleNotify, data: gpointer);
    fn g_value_init(value: *mut GValue, type_: GType) -> *mut GValue;
    fn g_value_unset(value: *mut GValue);
    fn g_value_set_boolean(v: *mut GValue, x: i32);
    fn g_value_set_schar(v: *mut GValue, x: i8);
    fn g_value_set_uchar(v: *mut GValue, x: u8);
    fn g_value_set_int(v: *mut GValue, x: i32);
    fn g_value_set_uint(v: *mut GValue, x: u32);
    fn g_value_set_long(v: *mut GValue, x: i64);
    fn g_value_set_ulong(v: *mut GValue, x: u64);
    fn g_value_set_int64(v: *mut GValue, x: i64);
    fn g_value_set_uint64(v: *mut GValue, x: u64);
    fn g_value_set_float(v: *mut GValue, x: f32);
    fn g_value_set_double(v: *mut GValue, x: f64);
    fn g_value_set_string(v: *mut GValue, x: *const c_char);
    fn g_value_set_enum(v: *mut GValue, x: i32);
    fn g_value_set_flags(v: *mut GValue, x: u32);
    fn g_value_set_object(v: *mut GValue, x: gpointer);
    fn g_value_set_boxed(v: *mut GValue, x: gpointer);
    fn g_value_set_pointer(v: *mut GValue, x: gpointer);
    fn g_value_set_variant(v: *mut GValue, x: gpointer);
    fn g_value_set_param(v: *mut GValue, x: gpointer);
    fn g_value_get_boolean(v: *const GValue) -> i32;
    fn g_value_get_schar(v: *const GValue) -> i8;
    fn g_value_get_uchar(v: *const GValue) -> u8;
    fn g_value_get_int(v: *const GValue) -> i32;
    fn g_value_get_uint(v: *const GValue) -> u32;
    fn g_value_get_long(v: *const GValue) -> i64;
    fn g_value_get_ulong(v: *const GValue) -> u64;
    fn g_value_get_int64(v: *const GValue) -> i64;
    fn g_value_get_uint64(v: *const GValue) -> u64;
    fn g_value_get_float(v: *const GValue) -> f32;
    fn g_value_get_double(v: *const GValue) -> f64;
    fn g_value_get_string(v: *const GValue) -> *const c_char;
    fn g_value_get_enum(v: *const GValue) -> i32;
    fn g_value_get_flags(v: *const GValue) -> u32;
    fn g_value_get_object(v: *const GValue) -> gpointer;
    fn g_value_get_boxed(v: *const GValue) -> gpointer;
    fn g_value_get_pointer(v: *const GValue) -> gpointer;
    fn g_value_get_variant(v: *const GValue) -> gpointer;
    fn g_value_get_param(v: *const GValue) -> gpointer;
}

#[link(name = "glib-2.0")]
extern "C" {
    fn g_quark_from_static_string(s: *const c_char) -> u32;
    fn glib_check_version(major: u32, minor: u32, micro: u32) -> *const c_char;
}

// fundamental type n is n << 2
pub const G_TYPE_INTERFACE: GType = 2 << 2;
pub const G_TYPE_CHAR: GType = 3 << 2;
pub const G_TYPE_UCHAR: GType = 4 << 2;
pub const G_TYPE_BOOLEAN: GType = 5 << 2;
pub const G_TYPE_INT: GType = 6 << 2;
pub const G_TYPE_UINT: GType = 7 << 2;
pub const G_TYPE_LONG: GType = 8 << 2;
pub const G_TYPE_ULONG: GType = 9 << 2;
pub const G_TYPE_INT64: GType = 10 << 2;
pub const G_TYPE_UINT64: GType = 11 << 2;
pub const G_TYPE_ENUM: GType = 12 << 2;
pub const G_TYPE_FLAGS: GType = 13 << 2;
pub const G_TYPE_FLOAT: GType = 14 << 2;
pub const G_TYPE_DOUBLE: GType = 15 << 2;
pub const G_TYPE_STRING: GType = 16 << 2;
pub const G_TYPE_POINTER: GType = 17 << 2;
pub const G_TYPE_BOXED: GType = 18 << 2;
pub const G_TYPE_PARAM: GType = 19 << 2;
pub const G_TYPE_OBJECT: GType = 20 << 2;
pub const G_TYPE_VARIANT: GType = 21 << 2;
pub const G_TYPE_NONE: GType = 1 << 2;

/// Callbacks into the engine that defined a class, made on any thread.
pub trait Host: Sync {
    fn get_property(&self, class: u32, object: usize, index: usize, value: *mut GValue);
    fn set_property(&self, class: u32, object: usize, index: usize, value: *const GValue);
    /// The GObject is finalized: its JS object goes.
    fn finalize(&self, object: usize);
    fn toggle(&self, object: usize, is_last: bool);
}

pub struct Slot {
    pub offset: usize,
    pub code: usize,
}

pub enum Property {
    /// a GParamSpec made by the class
    Install(usize),
    /// a parent class's or interface's property, by name
    Override(std::ffi::CString),
}

pub struct Signal {
    pub name: std::ffi::CString,
    pub flags: u32,
    pub return_type: GType,
    pub params: Vec<GType>,
}

pub struct Interface {
    pub gtype: GType,
    pub slots: Vec<Slot>,
}

pub struct ClassDef {
    pub host: &'static dyn Host,
    /// the engine's index of the class
    pub class: u32,
    pub vfuncs: Vec<Slot>,
    pub interfaces: Vec<Interface>,
    pub properties: Vec<Property>,
    pub signals: Vec<Signal>,
    gtype: std::cell::Cell<GType>,
    /// GObjectClass.finalize of the nearest native ancestor
    parent_finalize: std::cell::Cell<Option<ObjectFn>>,
    /// a JS class's parent is a native class: its finalize is ours
    root: bool,
}

// read on other threads only after class_init wrote it, under GLib's type lock
unsafe impl Sync for ClassDef {}
unsafe impl Send for ClassDef {}

impl ClassDef {
    pub fn new(host: &'static dyn Host, class: u32, root: bool) -> Self {
        ClassDef {
            host,
            class,
            vfuncs: Vec::new(),
            interfaces: Vec::new(),
            properties: Vec::new(),
            signals: Vec::new(),
            gtype: std::cell::Cell::new(0),
            parent_finalize: std::cell::Cell::new(None),
            root,
        }
    }

    pub fn gtype(&self) -> GType {
        self.gtype.get()
    }
}

fn quark() -> u32 {
    static QUARK: OnceLock<u32> = OnceLock::new();
    *QUARK.get_or_init(|| unsafe { g_quark_from_static_string(c"nativescript-js-class".as_ptr()) })
}

/// The JS class definition of `gtype` (not of its ancestors).
pub fn class_def(gtype: GType) -> Option<&'static ClassDef> {
    let def = unsafe { g_type_get_qdata(gtype, quark()) } as *const ClassDef;
    (!def.is_null()).then(|| unsafe { &*def })
}

/// The JS class definition of an instance's class or its nearest JS ancestor.
pub fn instance_class(object: usize) -> Option<&'static ClassDef> {
    let mut gtype = instance_type(object);
    while gtype != 0 {
        if let Some(def) = class_def(gtype) {
            return Some(def);
        }
        gtype = unsafe { g_type_parent(gtype) };
    }
    None
}

/// G_TYPE_FROM_INSTANCE
#[inline]
pub fn instance_type(object: usize) -> GType {
    unsafe { **(object as *const *const GType) }
}

pub fn type_query(gtype: GType) -> (u32, u32) {
    let mut q = GTypeQuery {
        type_: 0,
        type_name: std::ptr::null(),
        class_size: 0,
        instance_size: 0,
    };
    unsafe { g_type_query(gtype, &mut q) };
    (q.class_size, q.instance_size)
}

/// G_TYPE_FLAG_FINAL
fn is_final(gtype: GType) -> bool {
    // GLib before 2.70 has no final types, and reports flags it doesn't know as set
    static FINAL_TYPES: OnceLock<bool> = OnceLock::new();
    *FINAL_TYPES.get_or_init(|| unsafe { glib_check_version(2, 70, 0) }.is_null())
        && unsafe { g_type_test_flags(gtype, 1 << 6) } != 0
}

/// Registers `name` deriving from `parent`, implemented by `def` (kept for the
/// process's lifetime, as GTypes are).
pub fn register(parent: GType, name: &CStr, def: ClassDef) -> Result<GType, String> {
    let parent_name = || unsafe { CStr::from_ptr(g_type_name(parent)) }.to_string_lossy().into_owned();
    if is_final(parent) {
        return Err(format!("{} is final and can't be subclassed", parent_name()));
    }
    let (class_size, instance_size) = type_query(parent);
    if class_size == 0 {
        return Err(format!("{} can't be subclassed", parent_name()));
    }
    let def: &'static ClassDef = Box::leak(Box::new(def));
    let info = GTypeInfo {
        class_size: class_size as u16,
        base_init: 0,
        base_finalize: 0,
        class_init: Some(class_init),
        class_finalize: 0,
        class_data: def as *const ClassDef as gpointer,
        instance_size: instance_size as u16,
        n_preallocs: 0,
        instance_init: 0,
        value_table: 0,
    };
    let gtype = unsafe { g_type_register_static(parent, name.as_ptr(), &info, 0) };
    if gtype == 0 {
        return Err(format!("GLib refused to register {}", name.to_string_lossy()));
    }
    def.gtype.set(gtype);
    unsafe { g_type_set_qdata(gtype, quark(), def as *const ClassDef as gpointer) };
    for (i, iface) in def.interfaces.iter().enumerate() {
        let data: &'static (usize, usize) = Box::leak(Box::new((def as *const ClassDef as usize, i)));
        let info = GInterfaceInfo {
            interface_init: Some(interface_init),
            interface_finalize: 0,
            interface_data: data as *const (usize, usize) as gpointer,
        };
        unsafe { g_type_add_interface_static(gtype, iface.gtype, &info) };
    }
    Ok(gtype)
}

pub fn type_from_name(name: &CStr) -> GType {
    unsafe { g_type_from_name(name.as_ptr()) }
}

unsafe fn write_slots(memory: gpointer, slots: &[Slot]) {
    for slot in slots {
        unsafe { ((memory as usize + slot.offset) as *mut usize).write_unaligned(slot.code) };
    }
}

unsafe extern "C" fn class_init(klass: gpointer, data: gpointer) {
    let def = unsafe { &*(data as *const ClassDef) };
    let object_class = unsafe { &mut *(klass as *mut GObjectClass) };
    if def.root {
        def.parent_finalize.set(object_class.finalize);
        object_class.finalize = Some(finalize);
    }
    if !def.properties.is_empty() {
        // GObject checks the handlers are set before properties are installed
        object_class.set_property = Some(set_property);
        object_class.get_property = Some(get_property);
        for (i, property) in def.properties.iter().enumerate() {
            let id = i as u32 + 1;
            match property {
                Property::Install(pspec) => unsafe {
                    g_object_class_install_property(klass, id, *pspec as gpointer)
                },
                Property::Override(name) => unsafe {
                    g_object_class_override_property(klass, id, name.as_ptr())
                },
            }
        }
    }
    for signal in &def.signals {
        unsafe {
            g_signal_newv(
                signal.name.as_ptr(),
                def.gtype(),
                signal.flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                signal.return_type,
                signal.params.len() as u32,
                signal.params.as_ptr(),
            )
        };
    }
    unsafe { write_slots(klass, &def.vfuncs) };
}

unsafe extern "C" fn interface_init(vtable: gpointer, data: gpointer) {
    let &(def, i) = unsafe { &*(data as *const (usize, usize)) };
    let def = unsafe { &*(def as *const ClassDef) };
    unsafe { write_slots(vtable, &def.interfaces[i].slots) };
}

/// The JS class whose property `id` is `pspec`: the class that installed it, or for an
/// overridden property (GObject passes the interface's or parent's pspec) the class
/// in the instance's chain that overrode it.
unsafe fn owner(object: gpointer, id: u32, pspec: gpointer) -> Option<&'static ClassDef> {
    let pspec = unsafe { &*(pspec as *const GParamSpec) };
    if let Some(def) = class_def(pspec.owner_type) {
        return Some(def);
    }
    let name = unsafe { CStr::from_ptr(pspec.name) };
    let mut gtype = instance_type(object as usize);
    while gtype != 0 {
        if let Some(def) = class_def(gtype) {
            if let Some(Property::Override(n)) = def.properties.get(id as usize - 1) {
                if n.as_c_str() == name {
                    return Some(def);
                }
            }
        }
        gtype = unsafe { g_type_parent(gtype) };
    }
    None
}

unsafe extern "C" fn set_property(object: gpointer, id: u32, value: *mut GValue, pspec: gpointer) {
    if let Some(def) = unsafe { owner(object, id, pspec) } {
        def.host.set_property(def.class, object as usize, id as usize - 1, value);
    }
}

unsafe extern "C" fn get_property(object: gpointer, id: u32, value: *mut GValue, pspec: gpointer) {
    if let Some(def) = unsafe { owner(object, id, pspec) } {
        def.host.get_property(def.class, object as usize, id as usize - 1, value);
    }
}

unsafe extern "C" fn finalize(object: gpointer) {
    // the root JS class of the instance's class chain installed this, and holds the
    // native ancestor's finalize
    let mut gtype = instance_type(object as usize);
    let mut root = None;
    while gtype != 0 {
        if let Some(def) = class_def(gtype) {
            if def.root {
                root = Some(def);
                break;
            }
        }
        gtype = unsafe { g_type_parent(gtype) };
    }
    let Some(def) = root else { return };
    def.host.finalize(object as usize);
    if let Some(parent) = def.parent_finalize.get() {
        unsafe { parent(object) };
    }
}

/// Toggle-notify of JS objects' references: `data` is the instance's ClassDef.
pub unsafe extern "C" fn toggle_notify(data: gpointer, object: gpointer, is_last: i32) {
    let def = unsafe { &*(data as *const ClassDef) };
    def.host.toggle(object as usize, is_last != 0);
}

pub fn param_value_type(pspec: usize) -> GType {
    unsafe { (*(pspec as *const GParamSpec)).value_type }
}

/// GParamSpec of the property `name` (canonical form) of `gtype`'s class.
pub fn find_property(gtype: GType, name: &CStr) -> Option<usize> {
    unsafe {
        let klass = g_type_class_ref(gtype);
        let pspec = g_object_class_find_property(klass, name.as_ptr());
        g_type_class_unref(klass);
        (!pspec.is_null()).then_some(pspec as usize)
    }
}

pub struct SignalInfo {
    pub id: u32,
    pub detail: u32,
    pub return_type: GType,
    pub params: Vec<GType>,
}

pub fn signal_info(gtype: GType, detailed: &CStr) -> Option<SignalInfo> {
    let (mut id, mut detail) = (0u32, 0u32);
    if unsafe { g_signal_parse_name(detailed.as_ptr(), gtype, &mut id, &mut detail, 1) } == 0 {
        return None;
    }
    let mut q = GSignalQuery {
        signal_id: 0,
        signal_name: std::ptr::null(),
        itype: 0,
        signal_flags: 0,
        return_type: 0,
        n_params: 0,
        param_types: std::ptr::null(),
    };
    unsafe { g_signal_query(id, &mut q) };
    // G_SIGNAL_TYPE_STATIC_SCOPE is the low bit
    let params = (0..q.n_params as usize)
        .map(|i| unsafe { *q.param_types.add(i) } & !1)
        .collect();
    Some(SignalInfo { id, detail, return_type: q.return_type & !1, params })
}

/// Emits with `values[0]` the instance; `ret` initialized to the return type, if any.
pub fn emit(values: &[GValue], signal: &SignalInfo, ret: Option<&mut GValue>) {
    let ret = ret.map_or(std::ptr::null_mut(), |r| r as *mut GValue);
    unsafe { g_signal_emitv(values.as_ptr(), signal.id, signal.detail, ret) };
}

/// GValues initialized to `types`, unset when dropped.
pub struct Values(pub Vec<GValue>);

impl Values {
    pub fn with_types(types: &[GType]) -> Self {
        let mut v: Vec<GValue> = types.iter().map(|_| GValue::default()).collect();
        for (value, &t) in v.iter_mut().zip(types) {
            unsafe { g_value_init(value, t) };
        }
        Values(v)
    }
}

impl Drop for Values {
    fn drop(&mut self) {
        for v in &mut self.0 {
            if v.g_type != 0 {
                unsafe { g_value_unset(v) };
            }
        }
    }
}

pub fn value_init(value: &mut GValue, gtype: GType) {
    unsafe { g_value_init(value, gtype) };
}

/// A GValue's contents, independent of the engine.
pub enum Val<'a> {
    None,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(Option<&'a CStr>),
    /// a GObject (or an object implementing an interface)
    Object(usize),
    /// boxed, pointer, variant and param values
    Pointer(usize),
}

pub fn fundamental(gtype: GType) -> GType {
    if gtype <= G_TYPE_VARIANT {
        gtype
    } else {
        unsafe { g_type_fundamental(gtype) }
    }
}

/// # Safety
/// `value` is an initialized GValue.
pub unsafe fn read<'a>(value: *const GValue) -> Val<'a> {
    unsafe {
        match fundamental((*value).g_type) {
            G_TYPE_BOOLEAN => Val::Bool(g_value_get_boolean(value) != 0),
            G_TYPE_CHAR => Val::I64(g_value_get_schar(value) as i64),
            G_TYPE_UCHAR => Val::I64(g_value_get_uchar(value) as i64),
            G_TYPE_INT => Val::I64(g_value_get_int(value) as i64),
            G_TYPE_UINT => Val::I64(g_value_get_uint(value) as i64),
            G_TYPE_LONG => Val::I64(g_value_get_long(value)),
            G_TYPE_ULONG => Val::U64(g_value_get_ulong(value)),
            G_TYPE_INT64 => Val::I64(g_value_get_int64(value)),
            G_TYPE_UINT64 => Val::U64(g_value_get_uint64(value)),
            G_TYPE_FLOAT => Val::F64(g_value_get_float(value) as f64),
            G_TYPE_DOUBLE => Val::F64(g_value_get_double(value)),
            G_TYPE_ENUM => Val::I64(g_value_get_enum(value) as i64),
            G_TYPE_FLAGS => Val::I64(g_value_get_flags(value) as i64),
            G_TYPE_STRING => {
                let s = g_value_get_string(value);
                Val::Str((!s.is_null()).then(|| CStr::from_ptr(s)))
            }
            G_TYPE_OBJECT | G_TYPE_INTERFACE => Val::Object(g_value_get_object(value) as usize),
            G_TYPE_BOXED => Val::Pointer(g_value_get_boxed(value) as usize),
            G_TYPE_POINTER => Val::Pointer(g_value_get_pointer(value) as usize),
            G_TYPE_VARIANT => Val::Pointer(g_value_get_variant(value) as usize),
            G_TYPE_PARAM => Val::Pointer(g_value_get_param(value) as usize),
            _ => Val::None,
        }
    }
}

/// The JS value of a GValue's type, as the engine converted it.
pub enum In<'a> {
    Bool(bool),
    Number(f64),
    BigInt(i64),
    BigUint(u64),
    Str(Option<&'a CStr>),
    /// an address: a GObject, Pointer, struct view or NULL
    Address(usize),
}

impl In<'_> {
    fn int(&self) -> i64 {
        match *self {
            In::Bool(b) => b as i64,
            In::Number(n) => n as i64,
            In::BigInt(n) => n,
            In::BigUint(n) => n as i64,
            In::Address(a) => a as i64,
            In::Str(_) => 0,
        }
    }
    fn float(&self) -> f64 {
        match *self {
            In::Number(n) => n,
            In::BigInt(n) => n as f64,
            In::BigUint(n) => n as f64,
            ref other => other.int() as f64,
        }
    }
    fn address(&self) -> usize {
        match *self {
            In::Address(a) => a,
            _ => 0,
        }
    }
}

/// Sets an initialized GValue; `Err` names the unsupported type.
///
/// # Safety
/// `value` is an initialized GValue.
pub unsafe fn write(value: *mut GValue, v: In) -> Result<(), String> {
    unsafe {
        let gtype = (*value).g_type;
        match fundamental(gtype) {
            G_TYPE_BOOLEAN => g_value_set_boolean(value, match v {
                In::Bool(b) => b as i32,
                ref other => (other.float() != 0.0) as i32,
            }),
            G_TYPE_CHAR => g_value_set_schar(value, v.int() as i8),
            G_TYPE_UCHAR => g_value_set_uchar(value, v.int() as u8),
            G_TYPE_INT => g_value_set_int(value, v.int() as i32),
            G_TYPE_UINT => g_value_set_uint(value, v.int() as u32),
            G_TYPE_LONG => g_value_set_long(value, v.int()),
            G_TYPE_ULONG => g_value_set_ulong(value, match v {
                In::BigUint(n) => n,
                ref other => other.int() as u64,
            }),
            G_TYPE_INT64 => g_value_set_int64(value, v.int()),
            G_TYPE_UINT64 => g_value_set_uint64(value, match v {
                In::BigUint(n) => n,
                ref other => other.int() as u64,
            }),
            G_TYPE_FLOAT => g_value_set_float(value, v.float() as f32),
            G_TYPE_DOUBLE => g_value_set_double(value, v.float()),
            G_TYPE_ENUM => g_value_set_enum(value, v.int() as i32),
            G_TYPE_FLAGS => g_value_set_flags(value, v.int() as u32),
            G_TYPE_STRING => match v {
                In::Str(s) => g_value_set_string(value, s.map_or(std::ptr::null(), |s| s.as_ptr())),
                _ => g_value_set_string(value, std::ptr::null()),
            },
            G_TYPE_OBJECT | G_TYPE_INTERFACE => g_value_set_object(value, v.address() as gpointer),
            G_TYPE_BOXED => g_value_set_boxed(value, v.address() as gpointer),
            G_TYPE_POINTER => g_value_set_pointer(value, v.address() as gpointer),
            G_TYPE_VARIANT => g_value_set_variant(value, v.address() as gpointer),
            G_TYPE_PARAM => g_value_set_param(value, v.address() as gpointer),
            _ => return Err(format!("values of GType {gtype} are not supported")),
        }
    }
    Ok(())
}

/// What a JS value converts to for a GValue of `gtype`.
pub enum Want {
    Bool,
    Number,
    /// 64-bit: a Number or a BigInt
    Int64,
    UInt64,
    Str,
    Address,
}

pub fn want(gtype: GType) -> Option<Want> {
    Some(match fundamental(gtype) {
        G_TYPE_BOOLEAN => Want::Bool,
        G_TYPE_CHAR | G_TYPE_UCHAR | G_TYPE_INT | G_TYPE_UINT | G_TYPE_ENUM | G_TYPE_FLAGS
        | G_TYPE_FLOAT | G_TYPE_DOUBLE => Want::Number,
        G_TYPE_LONG | G_TYPE_INT64 => Want::Int64,
        G_TYPE_ULONG | G_TYPE_UINT64 => Want::UInt64,
        G_TYPE_STRING => Want::Str,
        G_TYPE_OBJECT | G_TYPE_INTERFACE | G_TYPE_BOXED | G_TYPE_POINTER | G_TYPE_VARIANT
        | G_TYPE_PARAM => Want::Address,
        _ => return None,
    })
}

/// `itemCount` / `item_count` -> `item-count`, GObject's canonical property names.
pub fn canonical_name(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let chars: Vec<char> = key.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            out.push('-');
            continue;
        }
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                out.push('-');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Creates an instance with construct properties.
pub fn new_object(gtype: GType, names: &[std::ffi::CString], values: &Values) -> usize {
    let ptrs: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
    unsafe {
        g_object_new_with_properties(gtype, ptrs.len() as u32, ptrs.as_ptr(), values.0.as_ptr()) as usize
    }
}
