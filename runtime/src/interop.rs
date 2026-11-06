//! The `interop` global: Pointer, Reference, types, alloc/free, sizeof...
//! Mirrors the API of the NativeScript iOS runtime.

use crate::ffi;
use crate::marshal::{self, CallCx};
use crate::state::{state, State};
use crate::throw_error;
use nativescript_metadata::Type;
use std::ffi::{c_void, CStr};

/// V8 private symbols used to tag interop objects (invisible to JS).
pub mod keys {
    use nativescript_metadata::Type;

    fn key<'s>(scope: &v8::PinScope<'s, '_, ()>, name: &str) -> v8::Local<'s, v8::Private> {
        let name = v8::String::new(scope, name).unwrap();
        v8::Private::for_api(scope, Some(name))
    }

    /// BigInt address of a Pointer
    pub fn pointer<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#pointer")
    }
    /// Keeps memory backing a Pointer / Reference alive (ArrayBuffer)
    pub fn owner<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#owner")
    }
    /// Integer index into `State::types` (type descriptors, struct constructors, struct values)
    pub fn type_tag<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#type")
    }
    pub fn reference<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#reference")
    }
    /// BigInt address of a Reference's storage
    pub fn storage<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#storage")
    }
    /// Value of a Reference that has no storage yet
    pub fn pending<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#pending")
    }
    /// Extra memory kept alive by a Reference (e.g. a C string it points to)
    pub fn keep<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#keep")
    }
    /// BigInt address of a native function
    pub fn function_address<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#fnaddr")
    }
    /// Metadata offset of a C function wrapper
    pub fn function_meta<'s>(scope: &v8::PinScope<'s, '_, ()>) -> v8::Local<'s, v8::Private> {
        key(scope, "ns#fnmeta")
    }
    /// BigInt address of the libffi closure created for a JS function and signature
    pub fn closure<'s>(scope: &v8::PinScope<'s, '_, ()>, signature: &[Type]) -> v8::Local<'s, v8::Private> {
        key(scope, &format!("ns#closure:{signature:?}"))
    }
}

fn private_value<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    v: v8::Local<'s, v8::Value>,
    key: v8::Local<'s, v8::Private>,
) -> Option<v8::Local<'s, v8::Value>> {
    if !v.is_object() {
        return None;
    }
    let object = v8::Local::<v8::Object>::try_from(v).ok()?;
    let value = object.get_private(scope, key)?;
    (!value.is_undefined()).then_some(value)
}

fn bigint_usize(v: v8::Local<v8::Value>) -> Option<usize> {
    v8::Local::<v8::BigInt>::try_from(v).ok().map(|b| b.u64_value().0 as usize)
}

/// Address held by an `interop.Pointer`.
pub fn pointer_address<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<usize> {
    let key = keys::pointer(scope);
    private_value(scope, v, key).and_then(bigint_usize)
}

pub fn new_pointer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    address: usize,
    owner: Option<v8::Local<'s, v8::Value>>,
) -> v8::Local<'s, v8::Object> {
    let object = v8::Object::new(scope);
    if let Some(proto) = st.pointer_prototype.borrow().as_ref() {
        let proto = v8::Local::new(scope, proto);
        object.set_prototype(scope, proto.into());
    }
    init_pointer(scope, object, address, owner);
    object
}

fn init_pointer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    address: usize,
    owner: Option<v8::Local<'s, v8::Value>>,
) {
    let key = keys::pointer(scope);
    let value = v8::BigInt::new_from_u64(scope, address as u64);
    object.set_private(scope, key, value.into());
    if let Some(owner) = owner {
        let key = keys::owner(scope);
        object.set_private(scope, key, owner);
    }
}

pub fn is_reference<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> bool {
    let key = keys::reference(scope);
    private_value(scope, v, key).is_some()
}

/// Storage address of a Reference, if it has storage.
pub fn reference_storage<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Option<usize> {
    if !is_reference(scope, v) {
        return None;
    }
    let key = keys::storage(scope);
    private_value(scope, v, key).and_then(bigint_usize)
}

fn reference_type<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, v: v8::Local<'s, v8::Value>) -> Option<Type> {
    let key = keys::type_tag(scope);
    let index = private_value(scope, v, key)?.uint32_value(scope)?;
    st.type_at(index)
}

/// Storage of a Reference passed for a `pointee*` argument, allocated on first use.
pub fn reference_ensure_storage<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    v: v8::Local<'s, v8::Value>,
    pointee: &Type,
) -> Result<usize, String> {
    if let Some(addr) = reference_storage(scope, v) {
        return Ok(addr);
    }
    let ty = match reference_type(scope, st, v) {
        Some(t) => t,
        None if !pointee.is_void() => pointee.clone(),
        None => return Err("a Reference passed as void* needs a type: new interop.Reference(interop.types.int32, 0)".into()),
    };
    let object = v8::Local::<v8::Object>::try_from(v).unwrap();
    let pending_key = keys::pending(scope);
    let pending = object.get_private(scope, pending_key).filter(|p| !p.is_undefined());
    allocate_reference_storage(scope, st, object, &ty)?;
    if let Some(value) = pending {
        reference_write(scope, st, object, value)?;
    }
    reference_storage(scope, v).ok_or_else(|| "failed to allocate reference storage".into())
}

fn allocate_reference_storage<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    object: v8::Local<'s, v8::Object>,
    ty: &Type,
) -> Result<(), String> {
    let size = st.layout(ty)?.size.max(1);
    let buffer = v8::ArrayBuffer::new(scope, size);
    let address = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
    set_reference_storage(scope, st, object, ty, address, Some(buffer.into()));
    Ok(())
}

fn set_reference_storage<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    object: v8::Local<'s, v8::Object>,
    ty: &Type,
    address: usize,
    owner: Option<v8::Local<'s, v8::Value>>,
) {
    let tag = v8::Integer::new_from_unsigned(scope, st.register_type(ty.clone()));
    let key = keys::type_tag(scope);
    object.set_private(scope, key, tag.into());
    let key = keys::storage(scope);
    let addr = v8::BigInt::new_from_u64(scope, address as u64);
    object.set_private(scope, key, addr.into());
    if let Some(owner) = owner {
        let key = keys::owner(scope);
        object.set_private(scope, key, owner);
    }
}

fn reference_write<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    object: v8::Local<'s, v8::Object>,
    value: v8::Local<'s, v8::Value>,
) -> Result<(), String> {
    let Some(address) = reference_storage(scope, object.into()) else {
        let key = keys::pending(scope);
        object.set_private(scope, key, value);
        return Ok(());
    };
    let ty = reference_type(scope, st, object.into()).ok_or("reference without type")?;
    if ty == Type::CString && value.is_string() {
        // the C string must live as long as the reference
        let mut bytes = value.to_rust_string_lossy(scope).into_bytes();
        bytes.push(0);
        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
        let buffer = v8::ArrayBuffer::with_backing_store(scope, &store);
        let ptr = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
        unsafe { (address as *mut usize).write_unaligned(ptr) };
        let key = keys::keep(scope);
        object.set_private(scope, key, buffer.into());
        return Ok(());
    }
    let mut cx = CallCx::default();
    unsafe { marshal::write(scope, st, &ty, value, address as *mut u8, &mut cx) }
}

/// Address of a native function wrapper (C function global or function pointer).
pub fn native_function_address<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    f: v8::Local<'s, v8::Function>,
) -> Option<usize> {
    let key = keys::function_address(scope);
    if let Some(addr) = private_value(scope, f.into(), key).and_then(bigint_usize) {
        return Some(addr);
    }
    let key = keys::function_meta(scope);
    let offset = private_value(scope, f.into(), key)?.uint32_value(scope)?;
    ffi::function_for(st, offset).ok().map(|f| f.address)
}

/// Type described by an `interop.types.*` value, a struct constructor or a struct value.
pub fn type_from_value<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, v: v8::Local<'s, v8::Value>) -> Option<Type> {
    let key = keys::type_tag(scope);
    let index = private_value(scope, v, key)?.uint32_value(scope)?;
    st.type_at(index)
}

pub fn tag_type<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, object: v8::Local<'s, v8::Object>, ty: Type) {
    let tag = v8::Integer::new_from_unsigned(scope, st.register_type(ty));
    let key = keys::type_tag(scope);
    object.set_private(scope, key, tag.into());
}

fn this_pointer<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<usize> {
    let this = args.this();
    let addr = pointer_address(scope, this.into());
    if addr.is_none() {
        throw_error(scope, "not an interop.Pointer");
    }
    addr
}

fn this_owner<'s>(scope: &mut v8::PinScope<'s, '_>, args: &v8::FunctionCallbackArguments<'s>) -> Option<v8::Local<'s, v8::Value>> {
    let key = keys::owner(scope);
    private_value(scope, args.this().into(), key)
}

fn address_from_value<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>) -> Result<usize, String> {
    if v.is_null_or_undefined() {
        return Ok(0);
    }
    if let Some(addr) = pointer_address(scope, v) {
        return Ok(addr);
    }
    if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
        return Ok(b.u64_value().0 as usize);
    }
    if v.is_number() {
        return Ok(v.number_value(scope).unwrap_or(0.0) as usize);
    }
    Err("expected a number, bigint or Pointer".into())
}

fn set_method<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Template>,
    name: &str,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let f = v8::FunctionTemplate::new(scope, callback);
    let key = v8::String::new(scope, name).unwrap();
    target.set(key.into(), f.into());
}

fn set_function<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    name: &str,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let f = v8::Function::new(scope, callback).unwrap();
    let key = v8::String::new(scope, name).unwrap();
    f.set_name(key);
    target.set(scope, key.into(), f.into());
}

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

fn pointer_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let address = match address_from_value(scope, args.get(0)) {
        Ok(a) => a,
        Err(e) => return throw_error(scope, &e),
    };
    if args.is_construct_call() {
        init_pointer(scope, args.this(), address, None);
    } else {
        let st = state(scope);
        rv.set(new_pointer(scope, &st, address, None).into());
    }
}

fn offset_pointer<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>, sign: i64) {
    let Some(addr) = this_pointer(scope, &args) else { return };
    let delta = match marshal::to_i64(scope, args.get(0), &Type::Long) {
        Ok(d) => d,
        Err(e) => return throw_error(scope, &e),
    };
    let owner = this_owner(scope, &args);
    let st = state(scope);
    let p = new_pointer(scope, &st, (addr as i64).wrapping_add(sign * delta) as usize, owner);
    rv.set(p.into());
}

fn pointer_add<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, rv: Rv<'s>) {
    offset_pointer(scope, args, rv, 1)
}

fn pointer_subtract<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, rv: Rv<'s>) {
    offset_pointer(scope, args, rv, -1)
}

fn pointer_to_number<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    if let Some(addr) = this_pointer(scope, &args) {
        rv.set(v8::Number::new(scope, addr as f64).into());
    }
}

fn pointer_to_bigint<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    if let Some(addr) = this_pointer(scope, &args) {
        rv.set(v8::BigInt::new_from_u64(scope, addr as u64).into());
    }
}

fn pointer_to_hex<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    if let Some(addr) = this_pointer(scope, &args) {
        rv.set(v8::String::new(scope, &format!("{addr:#x}")).unwrap().into());
    }
}

fn pointer_to_string<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    if let Some(addr) = this_pointer(scope, &args) {
        rv.set(v8::String::new(scope, &format!("<Pointer: {addr:#x}>")).unwrap().into());
    }
}

fn pointer_equals<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Some(addr) = this_pointer(scope, &args) else { return };
    let other = address_from_value(scope, args.get(0)).ok();
    rv.set(v8::Boolean::new(scope, other == Some(addr)).into());
}

fn pointer_template<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::FunctionTemplate> {
    let tmpl = v8::FunctionTemplate::new(scope, pointer_ctor);
    let name = v8::String::new(scope, "Pointer").unwrap();
    tmpl.set_class_name(name);
    let proto: v8::Local<v8::Template> = tmpl.prototype_template(scope).into();
    set_method(scope, proto, "add", pointer_add);
    set_method(scope, proto, "subtract", pointer_subtract);
    set_method(scope, proto, "toNumber", pointer_to_number);
    set_method(scope, proto, "toBigInt", pointer_to_bigint);
    set_method(scope, proto, "toHexString", pointer_to_hex);
    set_method(scope, proto, "toString", pointer_to_string);
    set_method(scope, proto, "equals", pointer_equals);
    tmpl
}

fn reference_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    if !args.is_construct_call() {
        return throw_error(scope, "interop.Reference must be called with new");
    }
    let st = state(scope);
    let this = args.this();
    let key = keys::reference(scope);
    let t = v8::Boolean::new(scope, true);
    this.set_private(scope, key, t.into());

    let first = args.get(0);
    let result = match type_from_value(scope, &st, first) {
        // new Reference(type, Pointer) wraps existing memory
        Some(ty) if pointer_address(scope, args.get(1)).is_some() => {
            let addr = pointer_address(scope, args.get(1)).unwrap();
            set_reference_storage(scope, &st, this, &ty, addr, None);
            Ok(())
        }
        // new Reference(type, value?)
        Some(ty) => allocate_reference_storage(scope, &st, this, &ty).and_then(|_| {
            if args.length() > 1 {
                reference_write(scope, &st, this, args.get(1))
            } else {
                Ok(())
            }
        }),
        // new Reference(value): the type comes from the parameter it's passed to
        None => {
            let key = keys::pending(scope);
            this.set_private(scope, key, first);
            Ok(())
        }
    };
    if let Err(e) = result {
        throw_error(scope, &e);
    }
}

fn reference_get<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let this: v8::Local<v8::Value> = args.this().into();
    match reference_storage(scope, this) {
        Some(addr) => {
            let Some(ty) = reference_type(scope, &st, this) else { return };
            match unsafe { marshal::read(scope, &st, &ty, addr as *const u8) } {
                Ok(v) => rv.set(v),
                Err(e) => throw_error(scope, &e),
            }
        }
        None => {
            let key = keys::pending(scope);
            if let Some(v) = private_value(scope, this, key) {
                rv.set(v);
            }
        }
    }
}

fn reference_set<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    if let Err(e) = reference_write(scope, &st, args.this(), args.get(0)) {
        throw_error(scope, &e);
    }
}

fn reference_template<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::FunctionTemplate> {
    let tmpl = v8::FunctionTemplate::new(scope, reference_ctor);
    let name = v8::String::new(scope, "Reference").unwrap();
    tmpl.set_class_name(name);
    let proto = tmpl.prototype_template(scope);
    let key = v8::String::new(scope, "value").unwrap();
    let getter = v8::FunctionTemplate::new(scope, reference_get);
    let setter = v8::FunctionTemplate::new(scope, reference_set);
    proto.set_accessor_property(key.into(), Some(getter), Some(setter), v8::PropertyAttribute::NONE);
    tmpl
}

fn struct_ctor<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let Some(ty) = args.data().uint32_value(scope).and_then(|i| st.type_at(i)) else {
        return;
    };
    let result = (|| {
        let size = st.layout(&ty)?.size;
        let mut cx = CallCx::default();
        let mem = cx.scratch(size);
        let init = args.get(0);
        unsafe {
            if !init.is_undefined() {
                marshal::write(scope, &st, &ty, init, mem, &mut cx)?;
            }
            marshal::read(scope, &st, &ty, mem)
        }
    })();
    match result {
        Ok(v) => rv.set(v),
        Err(e) => throw_error(scope, &e),
    }
}

/// Constructor for struct / union globals: `new tm({ tm_year: 100 })` returns a
/// plain object with every field, `args.data()` holds the type index.
pub fn struct_constructor<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    name: &str,
    ty: Type,
) -> Option<v8::Local<'s, v8::Function>> {
    let index = st.register_type(ty.clone());
    let data = v8::Integer::new_from_unsigned(scope, index);
    let f = v8::Function::builder(struct_ctor).data(data.into()).build(scope)?;
    let name = v8::String::new(scope, name).unwrap();
    f.set_name(name);
    tag_type(scope, st, f.into(), ty);
    Some(f)
}

fn interop_alloc<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let size = args.get(0).integer_value(scope).unwrap_or(0).max(0) as usize;
    let st = state(scope);
    let buffer = v8::ArrayBuffer::new(scope, size.max(1));
    let addr = buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0);
    rv.set(new_pointer(scope, &st, addr, Some(buffer.into())).into());
}

fn interop_free<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let v = args.get(0);
    let Some(addr) = pointer_address(scope, v) else {
        return throw_error(scope, "interop.free expects a Pointer");
    };
    let key = keys::owner(scope);
    // memory from interop.alloc is garbage collected
    if private_value(scope, v, key).is_none() {
        unsafe { libc::free(addr as *mut c_void) };
    }
}

fn interop_sizeof<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let v = args.get(0);
    let size = if let Some(ty) = type_from_value(scope, &st, v) {
        match st.layout(&ty) {
            Ok(l) => l.size,
            Err(e) => return throw_error(scope, &e),
        }
    } else if pointer_address(scope, v).is_some() || is_reference(scope, v) || v.is_function() {
        std::mem::size_of::<usize>()
    } else {
        return throw_error(scope, "interop.sizeof expects a type, struct or Pointer");
    };
    rv.set(v8::Number::new(scope, size as f64).into());
}

fn interop_handleof<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let v = args.get(0);
    if pointer_address(scope, v).is_some() {
        return rv.set(v);
    }
    let addr = if let Some(addr) = reference_storage(scope, v) {
        Some(addr)
    } else if let Ok(f) = v8::Local::<v8::Function>::try_from(v) {
        native_function_address(scope, &st, f)
    } else if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        Some(view.data() as usize)
    } else if let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        buffer.data().map(|p| p.as_ptr() as usize)
    } else {
        None
    };
    match addr {
        Some(addr) => rv.set(new_pointer(scope, &st, addr, Some(v)).into()),
        None => rv.set_null(),
    }
}

fn interop_string_from_cstring<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let addr = match address_from_value(scope, args.get(0)) {
        Ok(a) => a,
        Err(e) => return throw_error(scope, &e),
    };
    if addr == 0 {
        return rv.set_null();
    }
    let bytes: &[u8] = unsafe {
        if args.get(1).is_number() {
            let len = args.get(1).integer_value(scope).unwrap_or(0).max(0) as usize;
            std::slice::from_raw_parts(addr as *const u8, len)
        } else {
            CStr::from_ptr(addr as *const libc::c_char).to_bytes()
        }
    };
    let s = String::from_utf8_lossy(bytes);
    rv.set(v8::String::new(scope, &s).unwrap().into());
}

fn interop_buffer_from_data<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let Some(addr) = pointer_address(scope, args.get(0)) else {
        return throw_error(scope, "interop.bufferFromData expects a Pointer and a length");
    };
    let len = args.get(1).integer_value(scope).unwrap_or(0).max(0) as usize;
    unsafe extern "C" fn no_free(_: *mut c_void, _: usize, _: *mut c_void) {}
    // the memory is not owned by the buffer
    let store = unsafe {
        v8::ArrayBuffer::new_backing_store_from_ptr(addr as *mut c_void, len, no_free, std::ptr::null_mut())
    }
    .make_shared();
    rv.set(v8::ArrayBuffer::with_backing_store(scope, &store).into());
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, st: &State, global: v8::Local<'s, v8::Object>) {
    let interop = v8::Object::new(scope);

    let pointer = pointer_template(scope).get_function(scope).unwrap();
    let proto_key = v8::String::new(scope, "prototype").unwrap();
    let proto = pointer.get(scope, proto_key.into()).unwrap();
    *st.pointer_prototype.borrow_mut() = Some(v8::Global::new(scope, v8::Local::<v8::Object>::try_from(proto).unwrap()));
    let key = v8::String::new(scope, "Pointer").unwrap();
    interop.set(scope, key.into(), pointer.into());

    let reference = reference_template(scope).get_function(scope).unwrap();
    let proto = reference.get(scope, proto_key.into()).unwrap();
    *st.reference_prototype.borrow_mut() = Some(v8::Global::new(scope, v8::Local::<v8::Object>::try_from(proto).unwrap()));
    let key = v8::String::new(scope, "Reference").unwrap();
    interop.set(scope, key.into(), reference.into());

    // interop.types
    let types = v8::Object::new(scope);
    let void_ptr = Type::Pointer(Box::new(Type::Void));
    for (name, ty) in [
        ("void", Type::Void),
        ("bool", Type::Bool),
        ("int8", Type::Char),
        ("uint8", Type::UChar),
        ("int16", Type::Short),
        ("uint16", Type::UShort),
        ("int32", Type::Int),
        ("uint32", Type::UInt),
        ("int64", Type::LongLong),
        ("uint64", Type::ULongLong),
        ("long", Type::Long),
        ("ulong", Type::ULong),
        ("float", Type::Float),
        ("double", Type::Double),
        ("longdouble", Type::LongDouble),
        ("unichar", Type::Unichar),
        ("UTF8CString", Type::CString),
        ("pointer", void_ptr),
    ] {
        let descriptor = v8::Object::new(scope);
        let n = v8::String::new(scope, name).unwrap();
        let name_key = v8::String::new(scope, "name").unwrap();
        descriptor.set(scope, name_key.into(), n.into());
        tag_type(scope, st, descriptor, ty);
        types.set(scope, n.into(), descriptor.into());
    }
    let key = v8::String::new(scope, "types").unwrap();
    interop.set(scope, key.into(), types.into());

    set_function(scope, interop, "alloc", interop_alloc);
    set_function(scope, interop, "free", interop_free);
    set_function(scope, interop, "sizeof", interop_sizeof);
    set_function(scope, interop, "handleof", interop_handleof);
    set_function(scope, interop, "stringFromCString", interop_string_from_cstring);
    set_function(scope, interop, "bufferFromData", interop_buffer_from_data);

    let key = v8::String::new(scope, "interop").unwrap();
    global.set(scope, key.into(), interop.into());
}
