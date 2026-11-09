//! Live struct / union views: field accessors read and write the C memory;
//! an owner (ArrayBuffer or parent view) keeps it alive.

use crate::fast::{self, Strings};
use crate::handles::{self, Kind};
use crate::interop::keys;
use crate::marshal::{self, CallCx, Mem};
use crate::state::{state, State};
use crate::tagged::{self, info, Layout};
use crate::throw_error;
use nativescript_core::call::ValueKind;
use nativescript_metadata::Type;
use std::cell::Cell;
use std::ffi::c_void;
use v8::fast_api::{CFunction as FastFunction, FastApiCallbackOptions, Type as CType};

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

/// Address of the memory behind a struct view.
#[inline]
pub fn address<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
) -> Option<usize> {
    handles::is(v, Kind::Struct).map(handles::address)
}

fn this_view<'s>(
    _scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    this: v8::Local<'s, v8::Object>,
) -> Option<(usize, Type)> {
    let o = handles::is(this.into(), Kind::Struct)?;
    Some((handles::address(o), st.type_at(handles::type_index(o)?)?))
}

/// Accessor data of a struct field (owned by `State::fields`).
pub struct FieldAccess {
    state: *const State,
    type_index: u32,
    index: usize,
    offset: usize,
    /// scalar kinds are read / written in place; `Other` takes the full conversion
    get: ValueKind,
    set: ValueKind,
    /// handle slot of the view class's prototype instance
    proto_slot: Cell<*const usize>,
}

fn scalar(kind: ValueKind) -> bool {
    matches!(
        kind,
        ValueKind::Bool
            | ValueKind::I8
            | ValueKind::U8
            | ValueKind::I16
            | ValueKind::U16
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::I64
            | ValueKind::U64
            | ValueKind::F32
            | ValueKind::F64
    )
}

impl FieldAccess {
    /// Field address in `this`, if it's a view of this field's record.
    #[inline(always)]
    fn address(&self, layout: Option<&Layout>, this: v8::Local<v8::Value>) -> Option<usize> {
        let base = match layout {
            Some(l) => l.instance_address(this, self.proto_slot.get())?,
            None => {
                let o = handles::is(this, Kind::Struct)?;
                (handles::type_index(o)? == self.type_index).then(|| handles::address(o))?
            }
        };
        Some(base + self.offset)
    }
}

#[inline(always)]
unsafe fn load(kind: ValueKind, p: *const u8) -> u128 {
    unsafe {
        match kind {
            ValueKind::Bool | ValueKind::I8 | ValueKind::U8 => *p as u128,
            ValueKind::I16 | ValueKind::U16 => (p as *const u16).read_unaligned() as u128,
            ValueKind::I32 | ValueKind::U32 | ValueKind::F32 => {
                (p as *const u32).read_unaligned() as u128
            }
            _ => (p as *const u64).read_unaligned() as u128,
        }
    }
}

#[inline(always)]
unsafe fn store(kind: ValueKind, p: *mut u8, bits: u64) {
    unsafe {
        match kind {
            ValueKind::Bool | ValueKind::I8 | ValueKind::U8 => *p = bits as u8,
            ValueKind::I16 | ValueKind::U16 => (p as *mut u16).write_unaligned(bits as u16),
            ValueKind::I32 | ValueKind::U32 | ValueKind::F32 => {
                (p as *mut u32).write_unaligned(bits as u32)
            }
            _ => (p as *mut u64).write_unaligned(bits),
        }
    }
}

/// Address and type index of the struct view `this`.
#[inline]
fn this_record(this: v8::Local<v8::Object>) -> Option<(usize, u32)> {
    let o = handles::is(this.into(), Kind::Struct)?;
    Some((handles::address(o), handles::type_index(o)?))
}

/// The field's struct view `this`, or an error to throw.
fn checked_view(f: &FieldAccess, this: v8::Local<v8::Object>) -> Result<usize, &'static str> {
    match this_record(this) {
        Some((addr, index)) if index == f.type_index => Ok(addr),
        Some(_) => Err("not a view of this struct"),
        None => Err("not a struct"),
    }
}

fn field_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    f: &FieldAccess,
    this: v8::Local<'s, v8::Object>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let st = unsafe { &*f.state };
    let addr = checked_view(f, this)?;
    let record = st.record_at(f.type_index)?;
    let field = record.fields.get(f.index).ok_or("unknown field")?;
    unsafe {
        marshal::read_field(
            scope,
            st,
            field,
            addr as *const u8,
            Mem::Live(Some(this.into())),
        )
    }
}

fn field_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    f: &FieldAccess,
    this: v8::Local<'s, v8::Object>,
    value: v8::Local<'s, v8::Value>,
) -> Result<(), String> {
    let st = unsafe { &*f.state };
    let addr = checked_view(f, this)?;
    let record = st.record_at(f.type_index)?;
    let field = record.fields.get(f.index).ok_or("unknown field")?;
    // strings stored in char* fields must live as long as the struct
    if matches!(field.ty, Type::CString | Type::ConstCString) && value.is_string() {
        let mut bytes = value.to_rust_string_lossy(scope).into_bytes();
        bytes.push(0);
        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
        let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
        let ptr = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
        unsafe { ((addr + field.offset) as *mut usize).write_unaligned(ptr) };
        let key = keys::keep_field(scope, &field.name);
        this.set_private(scope, key, buffer.into());
        return Ok(());
    }
    let mut cx = CallCx::default();
    unsafe { marshal::write_field(scope, st, field, value, addr as *mut u8, &mut cx) }
}

unsafe extern "C" fn get_callback(info: *const v8::FunctionCallbackInfo) {
    let args = v8::FunctionCallbackArguments::from_function_callback_info(unsafe { &*info });
    let f = unsafe { &*(tagged::external_value(args.data()) as *const FieldAccess) };
    let st = unsafe { &*f.state };
    if let Some(layout) = st.tagged().filter(|l| l.callback_info() && scalar(f.get)) {
        if let Some(p) = f.address(Some(layout), args.this().into()) {
            let raw = unsafe { load(f.get, p as *const u8) };
            if unsafe { crate::ffi::set_scalar_return(info, layout, f.get, raw) } {
                return;
            }
        }
    }
    v8::callback_scope!(unsafe scope, unsafe { &*info });
    let mut rv = v8::ReturnValue::from_function_callback_info(unsafe { &*info });
    match field_get(scope, f, args.this()) {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

unsafe extern "C" fn set_callback(info: *const v8::FunctionCallbackInfo) {
    let args = v8::FunctionCallbackArguments::from_function_callback_info(unsafe { &*info });
    let f = unsafe { &*(tagged::external_value(args.data()) as *const FieldAccess) };
    let st = unsafe { &*f.state };
    if let Some(layout) = st.tagged().filter(|l| l.callback_info() && scalar(f.set)) {
        if unsafe { info::argc(info) } == 1 {
            if let Some(p) = f.address(Some(layout), args.this().into()) {
                let value = unsafe { info::arg(info, 0) };
                let isolate = || unsafe { info::isolate(info) };
                let mut strings = Strings::new();
                if let Some(bits) =
                    fast::simple_arg(Some(layout), isolate, f.set, value, &mut strings)
                {
                    unsafe { store(f.set, p as *mut u8, bits) };
                    return;
                }
            }
        }
    }
    v8::callback_scope!(unsafe scope, unsafe { &*info });
    if let Err(e) = field_set(scope, f, args.this(), args.get(0)) {
        throw_error(scope, &e);
    }
}

/// Fast getter: scalar fields of views of the field's record.
#[inline(always)]
unsafe fn fast_get(recv: v8::Local<v8::Value>, opts: *mut FastApiCallbackOptions) -> u64 {
    let opts = unsafe { &*opts };
    let f = unsafe { &*(tagged::external_value(opts.data) as *const FieldAccess) };
    let st = unsafe { &*f.state };
    match f.address(st.tagged(), recv) {
        Some(p) => fast::fast_ret(f.get, unsafe { load(f.get, p as *const u8) }),
        None => {
            v8::callback_scope!(unsafe scope, opts);
            let this = v8::Local::<v8::Object>::try_from(recv).ok();
            let e = this.map_or("not a struct", |o| {
                checked_view(f, o).err().unwrap_or("not a struct")
            });
            throw_error(scope, e);
            0
        }
    }
}

unsafe extern "C" fn fast_get_int(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> u64 {
    unsafe { fast_get(recv, opts) }
}

unsafe extern "C" fn fast_get_float(
    recv: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) -> f64 {
    f64::from_bits(unsafe { fast_get(recv, opts) })
}

/// Fast setter for integer and bool fields; anything but a plain number or boolean
/// takes the full conversion.
unsafe extern "C" fn fast_set_int(
    recv: v8::Local<v8::Value>,
    value: v8::Local<v8::Value>,
    opts: *mut FastApiCallbackOptions,
) {
    let opts = unsafe { &*opts };
    let f = unsafe { &*(tagged::external_value(opts.data) as *const FieldAccess) };
    let st = unsafe { &*f.state };
    if let Some(p) = f.address(st.tagged(), recv) {
        let isolate = || unsafe { opts.isolate_unchecked() } as *const v8::Isolate;
        let mut strings = Strings::new();
        if let Some(bits) = fast::simple_arg(st.tagged(), isolate, f.set, value, &mut strings) {
            unsafe { store(f.set, p as *mut u8, bits) };
            return;
        }
    }
    v8::callback_scope!(unsafe scope, opts);
    let (recv, value) = (v8::Local::new(scope, recv), v8::Local::new(scope, value));
    let result = match v8::Local::<v8::Object>::try_from(recv) {
        Ok(this) => field_set(scope, f, this, value),
        Err(_) => Err("not a struct".into()),
    };
    if let Err(e) = result {
        throw_error(scope, &e);
    }
}

/// Fast setter for float fields: V8 passes the number unboxed.
unsafe extern "C" fn fast_set_float(
    recv: v8::Local<v8::Value>,
    value: f64,
    opts: *mut FastApiCallbackOptions,
) {
    let opts = unsafe { &*opts };
    let f = unsafe { &*(tagged::external_value(opts.data) as *const FieldAccess) };
    let st = unsafe { &*f.state };
    match f.address(st.tagged(), recv) {
        Some(p) => unsafe {
            if f.set == ValueKind::F32 {
                store(f.set, p as *mut u8, (value as f32).to_bits() as u64)
            } else {
                store(f.set, p as *mut u8, value.to_bits())
            }
        },
        None => {
            v8::callback_scope!(unsafe scope, opts);
            throw_error(scope, "not a struct");
        }
    }
}

fn fast_getter(f: &FieldAccess) -> Option<FastFunction> {
    let (ret, address) = match f.get {
        ValueKind::Bool => (CType::Bool, fast_get_int as *const c_void),
        ValueKind::I8 | ValueKind::U8 | ValueKind::I16 | ValueKind::U16 | ValueKind::I32 => {
            (CType::Int32, fast_get_int as *const c_void)
        }
        ValueKind::U32 => (CType::Uint32, fast_get_int as *const c_void),
        ValueKind::F32 | ValueKind::F64 => (CType::Float64, fast_get_float as *const c_void),
        _ => return None,
    };
    Some(FastFunction::new(address, fast::info(&[], ret)))
}

fn fast_setter(f: &FieldAccess) -> Option<FastFunction> {
    let address = match f.set {
        ValueKind::F32 | ValueKind::F64 => fast_set_float as *const c_void,
        k if scalar(k) => fast_set_int as *const c_void,
        _ => return None,
    };
    // float fields take an unboxed double
    let arg = if f.set.is_float() {
        ValueKind::F64
    } else {
        ValueKind::Pointer
    };
    Some(FastFunction::new(address, fast::info(&[arg], CType::Void)))
}

fn to_json<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let Some((addr, ty)) = this_view(scope, &st, args.this()) else {
        return throw_error(scope, "not a struct");
    };
    match unsafe { marshal::record_to_object(scope, &st, &ty, addr as *const u8) } {
        Ok(o) => rv.set(o.into()),
        Err(e) => throw_error(scope, &e),
    }
}

fn type_name(ty: &Type) -> &str {
    match ty {
        Type::Struct(n) | Type::Union(n) => n,
        Type::AnonymousUnion(..) => "union",
        _ => "struct",
    }
}

/// Class for views of `ty`: field accessors on the prototype, cached per type.
fn factory(
    scope: &mut v8::PinScope<'_, '_>,
    st: &State,
    ty: &Type,
    index: u32,
) -> Result<std::rc::Rc<handles::Factory>, String> {
    if let Some(f) = st.struct_views.borrow().get(&index) {
        return Ok(f.clone());
    }
    let record = st.record(ty)?;
    let tmpl = v8::FunctionTemplate::new(
        scope,
        |scope: &mut v8::PinScope, _: v8::FunctionCallbackArguments, _: v8::ReturnValue| {
            throw_error(
                scope,
                "struct views are created by the runtime, use the struct's constructor",
            );
        },
    );
    let name = v8::String::new(scope, type_name(ty)).unwrap();
    tmpl.set_class_name(name);
    let proto = tmpl.prototype_template(scope);
    let mut accesses = Vec::new();
    for (i, field) in record.fields.iter().enumerate() {
        if field.name.is_empty() {
            continue;
        }
        let (get, set) = match field.bits {
            Some(_) => (ValueKind::Other, ValueKind::Other),
            None => (ValueKind::of_ret(&field.ty), ValueKind::of_arg(&field.ty)),
        };
        let f = Box::new(FieldAccess {
            state: st,
            type_index: index,
            index: i,
            offset: field.offset,
            get,
            set,
            proto_slot: Cell::new(std::ptr::null()),
        });
        let data = v8::External::new(scope, &*f as *const FieldAccess as *mut c_void);
        let getter = v8::FunctionTemplate::builder_raw(get_callback).data(data.into());
        let getter = match fast_getter(&f) {
            Some(fast) => getter.build_fast(scope, &[fast]),
            None => getter.build(scope),
        };
        let setter = v8::FunctionTemplate::builder_raw(set_callback).data(data.into());
        let setter = match fast_setter(&f) {
            Some(fast) => setter.build_fast(scope, &[fast]),
            None => setter.build(scope),
        };
        let key = v8::String::new(scope, &field.name).unwrap();
        proto.set_accessor_property(
            key.into(),
            Some(getter),
            Some(setter),
            v8::PropertyAttribute::NONE,
        );
        accesses.push(f);
    }
    let key = v8::String::new(scope, "toJSON").unwrap();
    let json = v8::FunctionTemplate::new(scope, to_json);
    proto.set(key.into(), json.into());
    let instances = tmpl.instance_template(scope);
    instances.set_internal_field_count(handles::FIELDS);
    // instantiates the class so instances get its prototype
    tmpl.get_function(scope)
        .ok_or("failed to create struct class")?;
    let factory = std::rc::Rc::new(handles::Factory::new(
        scope,
        instances,
        Kind::Struct,
        Some(index),
        st.tagged().is_some(),
    ));
    for f in &accesses {
        f.proto_slot.set(factory.prototype_slot());
    }
    st.fields.borrow_mut().extend(accesses);
    st.struct_views.borrow_mut().insert(index, factory.clone());
    Ok(factory)
}

/// Live view of the record `ty` at `addr`.
pub fn view<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    addr: usize,
    owner: Option<v8::Local<'s, v8::Value>>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let index = st.register_type(ty);
    let object = factory(scope, st, ty, index)?
        .make(scope, addr, owner)
        .ok_or("failed to create struct view")?;
    Ok(object.into())
}

/// A record type's size and view class, resolved once (no lookups by name per value).
pub struct RecordClass {
    size: usize,
    factory: std::rc::Rc<handles::Factory>,
}

impl RecordClass {
    pub fn new(scope: &mut v8::PinScope<'_, '_>, st: &State, ty: &Type) -> Result<Self, String> {
        let size = st.layout(ty)?.size;
        let index = st.register_type(ty);
        Ok(RecordClass {
            size,
            factory: factory(scope, st, ty, index)?,
        })
    }

    /// View over a GC managed copy of the record at `src`.
    pub fn copy<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        st: &State,
        src: *const u8,
    ) -> Result<v8::Local<'s, v8::Value>, String> {
        let (buffer, addr) = st.struct_memory(scope, self.size);
        unsafe { std::ptr::copy_nonoverlapping(src, addr as *mut u8, self.size) };
        let object = self
            .factory
            .make(scope, addr, Some(buffer.into()))
            .ok_or("failed to create struct view")?;
        Ok(object.into())
    }
}

/// View over a GC managed copy of the record at `src` (or zeroed memory).
pub fn copy<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    src: *const u8,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let (buffer, addr) = allocate(scope, st, ty)?;
    if !src.is_null() {
        let size = st.layout(ty)?.size;
        unsafe { std::ptr::copy_nonoverlapping(src, addr as *mut u8, size) };
    }
    view(scope, st, ty, addr, Some(buffer.into()))
}

/// Zeroed, GC managed memory for a record.
pub fn allocate<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
) -> Result<(v8::Local<'s, v8::ArrayBuffer>, usize), String> {
    let size = st.layout(ty)?.size;
    Ok(st.struct_memory(scope, size))
}
