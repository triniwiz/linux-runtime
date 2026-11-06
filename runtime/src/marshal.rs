//! Conversions between JS values and C memory.

use crate::interop::{self, keys};
use crate::state::State;
use crate::{ffi, longdouble};
use nativescript_metadata::Type;
use std::ffi::CString;

/// Temporaries that must outlive a native call.
#[derive(Default)]
pub struct CallCx<'s> {
    strings: Vec<CString>,
    buffers: Vec<Vec<u128>>,
    /// structs passed by pointer from plain JS objects, copied back after the call
    copy_back: Vec<(v8::Local<'s, v8::Object>, Type, *mut u8)>,
}

impl<'s> CallCx<'s> {
    /// Zeroed, 16-byte aligned scratch memory that lives until the call returns.
    pub fn scratch(&mut self, size: usize) -> *mut u8 {
        let mut buf = vec![0u128; size.div_ceil(16).max(1)];
        let ptr = buf.as_mut_ptr() as *mut u8;
        self.buffers.push(buf);
        ptr
    }

    /// Writes structs that were passed by pointer back into their JS objects.
    pub fn finish(self, scope: &mut v8::PinScope<'s, '_>, st: &State) -> Result<(), String> {
        for (object, ty, ptr) in self.copy_back {
            let value = unsafe { read(scope, st, &ty, ptr)? };
            if let Ok(fields) = v8::Local::<v8::Object>::try_from(value) {
                let names = fields
                    .get_own_property_names(scope, Default::default())
                    .ok_or("failed to read struct fields")?;
                for i in 0..names.length() {
                    let key = names.get_index(scope, i).unwrap();
                    let v = fields.get(scope, key).unwrap();
                    object.set(scope, key, v);
                }
            }
        }
        Ok(())
    }
}

fn type_error(expected: &str, ty: &Type) -> String {
    format!("expected {expected} for {ty:?}")
}

pub fn to_i64<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, ty: &Type) -> Result<i64, String> {
    if v.is_number() {
        let n = v.number_value(scope).unwrap_or(0.0);
        return Ok(if n.is_finite() { n as i64 } else { 0 });
    }
    if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
        return Ok(b.i64_value().0);
    }
    if v.is_boolean() {
        return Ok(v.boolean_value(scope) as i64);
    }
    if v.is_null_or_undefined() {
        return Ok(0);
    }
    if let Some(addr) = interop::pointer_address(scope, v) {
        return Ok(addr as i64);
    }
    Err(type_error("a number or bigint", ty))
}

fn to_f64(scope: &mut v8::PinScope, v: v8::Local<v8::Value>, ty: &Type) -> Result<f64, String> {
    if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
        return Ok(b.i64_value().0 as f64);
    }
    if v.is_number() || v.is_boolean() || v.is_null_or_undefined() {
        return Ok(v.number_value(scope).unwrap_or(0.0));
    }
    Err(type_error("a number", ty))
}

/// Address of memory backing an ArrayBuffer / typed array / DataView.
fn buffer_address(v: v8::Local<v8::Value>) -> Option<usize> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        return Some(view.data() as usize);
    }
    if let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        return Some(buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0));
    }
    None
}

/// Writes `value` as a `ty` at `dst` (`layout(ty).size` writable bytes).
pub unsafe fn write<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    value: v8::Local<'s, v8::Value>,
    dst: *mut u8,
    cx: &mut CallCx<'s>,
) -> Result<(), String> {
    macro_rules! int {
        ($t:ty) => {{
            let v = to_i64(scope, value, ty)? as $t;
            (dst as *mut $t).write_unaligned(v);
        }};
    }
    match ty {
        Type::Void => {}
        Type::Bool => (dst as *mut u8).write(value.boolean_value(scope) as u8),
        Type::Char => int!(i8),
        Type::UChar => int!(u8),
        Type::Short => int!(i16),
        Type::UShort => int!(u16),
        Type::Unichar => {
            if value.is_string() {
                let s = value.to_rust_string_lossy(scope);
                let c = s.encode_utf16().next().unwrap_or(0);
                (dst as *mut u16).write_unaligned(c);
            } else {
                int!(u16)
            }
        }
        Type::Int => int!(i32),
        Type::UInt => int!(u32),
        Type::Long | Type::LongLong => int!(i64),
        Type::ULong | Type::ULongLong => {
            let v = match v8::Local::<v8::BigInt>::try_from(value) {
                Ok(b) => b.u64_value().0,
                Err(_) => to_i64(scope, value, ty)? as u64,
            };
            (dst as *mut u64).write_unaligned(v);
        }
        Type::Float => (dst as *mut f32).write_unaligned(to_f64(scope, value, ty)? as f32),
        Type::Double => (dst as *mut f64).write_unaligned(to_f64(scope, value, ty)?),
        Type::LongDouble => {
            let bytes = longdouble::to_bytes(to_f64(scope, value, ty)?);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, 16);
        }
        Type::CString => {
            let addr = if value.is_string() {
                let s = value.to_rust_string_lossy(scope);
                let c = CString::new(s).map_err(|_| "string contains a NUL character".to_string())?;
                let p = c.as_ptr() as usize;
                cx.strings.push(c);
                p
            } else {
                pointer_like(scope, st, &Type::Char, value, cx)
                    .ok_or_else(|| type_error("a string, Pointer, Reference, ArrayBuffer or null", ty))?
            };
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::Pointer(inner) | Type::IncompleteArray(inner) => {
            let addr = pointer_arg(scope, st, inner, value, cx)?;
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::FunctionPointer(sig) | Type::Block(sig) => {
            let addr = if let Ok(f) = v8::Local::<v8::Function>::try_from(value) {
                if let Some(addr) = interop::native_function_address(scope, st, f) {
                    addr
                } else {
                    ffi::closure_for(scope, st, f, sig)?
                }
            } else {
                pointer_like(scope, st, &Type::Void, value, cx)
                    .ok_or_else(|| type_error("a function, Pointer or null", ty))?
            };
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(_) | Type::AnonymousUnion(_) => {
            let record = st.record(ty)?;
            if let Some(src) = interop::pointer_address(scope, value)
                .or_else(|| interop::reference_storage(scope, value))
                .or_else(|| buffer_address(value))
            {
                if src != 0 {
                    std::ptr::copy_nonoverlapping(src as *const u8, dst, record.size);
                }
                return Ok(());
            }
            if value.is_null_or_undefined() {
                return Ok(());
            }
            let object = value
                .to_object(scope)
                .filter(|_| value.is_object())
                .ok_or_else(|| type_error("an object", ty))?;
            for field in &record.fields {
                if field.name.is_empty() {
                    continue;
                }
                let key = v8::String::new(scope, &field.name).unwrap();
                let v = object.get(scope, key.into()).unwrap();
                if v.is_undefined() {
                    continue;
                }
                write(scope, st, &field.ty, v, dst.add(field.offset), cx)?;
            }
        }
        Type::ConstantArray(n, inner) => {
            let elem = st.layout(inner)?;
            if let Ok(array) = v8::Local::<v8::Array>::try_from(value) {
                for i in 0..array.length().min(*n) {
                    let v = array.get_index(scope, i).unwrap();
                    write(scope, st, inner, v, dst.add(i as usize * elem.size), cx)?;
                }
            } else if let Some(src) = buffer_address(value) {
                std::ptr::copy_nonoverlapping(src as *const u8, dst, elem.size * *n as usize);
            } else if value.is_string() && matches!(**inner, Type::Char | Type::UChar) {
                let s = value.to_rust_string_lossy(scope);
                let len = s.len().min(*n as usize - 1);
                std::ptr::copy_nonoverlapping(s.as_ptr(), dst, len);
            } else if !value.is_null_or_undefined() {
                return Err(type_error("an array", ty));
            }
        }
        Type::ExtVector(n, inner) => {
            let elem = st.layout(inner)?;
            let array = v8::Local::<v8::Array>::try_from(value).map_err(|_| type_error("an array", ty))?;
            for i in 0..array.length().min(*n) {
                let v = array.get_index(scope, i).unwrap();
                write(scope, st, inner, v, dst.add(i as usize * elem.size), cx)?;
            }
        }
        other => return Err(format!("passing {other:?} values is not supported")),
    }
    Ok(())
}

/// Pointer, Reference, ArrayBuffer(View) or null -> address.
fn pointer_like<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    pointee: &Type,
    value: v8::Local<'s, v8::Value>,
    _cx: &mut CallCx<'s>,
) -> Option<usize> {
    if value.is_null_or_undefined() {
        return Some(0);
    }
    if let Some(addr) = interop::pointer_address(scope, value) {
        return Some(addr);
    }
    if interop::is_reference(scope, value) {
        return interop::reference_ensure_storage(scope, st, value, pointee).ok();
    }
    buffer_address(value)
}

/// Value passed for a `T*` parameter.
unsafe fn pointer_arg<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    pointee: &Type,
    value: v8::Local<'s, v8::Value>,
    cx: &mut CallCx<'s>,
) -> Result<usize, String> {
    if let Some(addr) = pointer_like(scope, st, pointee, value, cx) {
        return Ok(addr);
    }
    // conveniences: strings for char* / void*, plain objects for struct pointers
    if value.is_string() && matches!(pointee, Type::Char | Type::UChar | Type::Void) {
        let s = value.to_rust_string_lossy(scope);
        let c = CString::new(s).map_err(|_| "string contains a NUL character".to_string())?;
        let p = c.as_ptr() as usize;
        cx.strings.push(c);
        return Ok(p);
    }
    if value.is_object() && !value.is_function() {
        if let Ok(record) = st.record(pointee) {
            let object = value.to_object(scope).unwrap();
            let mem = cx.scratch(record.size);
            write(scope, st, pointee, value, mem, cx)?;
            cx.copy_back.push((object, pointee.clone(), mem));
            return Ok(mem as usize);
        }
        if let (Ok(array), Ok(elem)) = (v8::Local::<v8::Array>::try_from(value), st.layout(pointee)) {
            let mem = cx.scratch(elem.size * array.length() as usize);
            for i in 0..array.length() {
                let v = array.get_index(scope, i).unwrap();
                write(scope, st, pointee, v, mem.add(i as usize * elem.size), cx)?;
            }
            return Ok(mem as usize);
        }
    }
    Err(format!(
        "expected a Pointer, Reference, ArrayBuffer or null for pointer to {pointee:?}"
    ))
}

fn number_or_bigint<'s>(scope: &mut v8::PinScope<'s, '_>, v: i64) -> v8::Local<'s, v8::Value> {
    if v.unsigned_abs() <= (1u64 << 53) {
        v8::Number::new(scope, v as f64).into()
    } else {
        v8::BigInt::new_from_i64(scope, v).into()
    }
}

fn unsigned_number_or_bigint<'s>(scope: &mut v8::PinScope<'s, '_>, v: u64) -> v8::Local<'s, v8::Value> {
    if v <= (1u64 << 53) {
        v8::Number::new(scope, v as f64).into()
    } else {
        v8::BigInt::new_from_u64(scope, v).into()
    }
}

/// Reads a `ty` from `src`.
pub unsafe fn read<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    src: *const u8,
) -> Result<v8::Local<'s, v8::Value>, String> {
    macro_rules! num {
        ($t:ty) => {
            v8::Number::new(scope, (src as *const $t).read_unaligned() as f64).into()
        };
    }
    Ok(match ty {
        Type::Void => v8::undefined(scope).into(),
        Type::Bool => v8::Boolean::new(scope, *src != 0).into(),
        Type::Char => num!(i8),
        Type::UChar => num!(u8),
        Type::Short => num!(i16),
        Type::UShort | Type::Unichar => num!(u16),
        Type::Int => num!(i32),
        Type::UInt => num!(u32),
        Type::Float => num!(f32),
        Type::Double => num!(f64),
        Type::Long | Type::LongLong => number_or_bigint(scope, (src as *const i64).read_unaligned()),
        Type::ULong | Type::ULongLong => unsigned_number_or_bigint(scope, (src as *const u64).read_unaligned()),
        Type::LongDouble => {
            let mut bytes = [0u8; 16];
            std::ptr::copy_nonoverlapping(src, bytes.as_mut_ptr(), 16);
            v8::Number::new(scope, longdouble::from_bytes(&bytes)).into()
        }
        t if t.is_pointer_like() => {
            let addr = (src as *const usize).read_unaligned();
            if addr == 0 {
                v8::null(scope).into()
            } else if let Type::FunctionPointer(sig) = t {
                ffi::function_from_pointer(scope, st, addr, sig)?
            } else {
                interop::new_pointer(scope, st, addr, None).into()
            }
        }
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(_) | Type::AnonymousUnion(_) => {
            let record = st.record(ty)?;
            let object = v8::Object::new(scope);
            for field in &record.fields {
                if field.name.is_empty() {
                    continue;
                }
                let v = read(scope, st, &field.ty, src.add(field.offset))?;
                let key = v8::String::new(scope, &field.name).unwrap();
                object.set(scope, key.into(), v);
            }
            let tag = v8::Integer::new_from_unsigned(scope, st.register_type(ty.clone()));
            let key = keys::type_tag(scope);
            object.set_private(scope, key, tag.into());
            object.into()
        }
        Type::ConstantArray(n, inner) | Type::ExtVector(n, inner) => {
            let elem = st.layout(inner)?;
            let mut items = Vec::with_capacity(*n as usize);
            for i in 0..*n as usize {
                items.push(read(scope, st, inner, src.add(i * elem.size))?);
            }
            v8::Array::new_with_elements(scope, &items).into()
        }
        other => return Err(format!("reading {other:?} values is not supported")),
    })
}
