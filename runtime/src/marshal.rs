//! Conversions between JS values and C memory.

use crate::interop;
use crate::state::{Field, State};
use crate::{ffi, longdouble, structs};
use nativescript_metadata::Type;
use std::ffi::CStr;

/// Where memory being read comes from, decides how records are exposed.
#[derive(Clone, Copy)]
pub enum Mem<'s> {
    /// Memory that dies after the call (return values, callback arguments):
    /// records are copied into GC managed memory.
    Temp,
    /// Memory that stays valid (struct fields, variables, references):
    /// records become live views, `owner` keeps the memory alive.
    Live(Option<v8::Local<'s, v8::Value>>),
}

const INLINE: usize = 64; // 1 KiB of u128

/// Temporaries that must outlive a native call. Small calls allocate nothing.
pub struct CallCx<'s> {
    inline: [std::mem::MaybeUninit<u128>; INLINE],
    used: usize,
    heap: Vec<Box<[u128]>>,
    /// structs passed by pointer from plain JS objects, copied back after the call
    copy_back: Vec<(v8::Local<'s, v8::Object>, Type, *mut u8)>,
}

impl Default for CallCx<'_> {
    fn default() -> Self {
        CallCx { inline: [std::mem::MaybeUninit::uninit(); INLINE], used: 0, heap: Vec::new(), copy_back: Vec::new() }
    }
}

impl<'s> CallCx<'s> {
    /// Zeroed, 16-byte aligned scratch memory that lives until the call returns.
    #[inline]
    pub fn scratch(&mut self, size: usize) -> *mut u8 {
        let n = size.div_ceil(16).max(1);
        let ptr = if self.used + n <= INLINE {
            let p = self.inline[self.used..].as_mut_ptr() as *mut u8;
            self.used += n;
            p
        } else {
            let mut buf = vec![0u128; n].into_boxed_slice();
            let p = buf.as_mut_ptr() as *mut u8;
            self.heap.push(buf);
            return p;
        };
        unsafe { std::ptr::write_bytes(ptr, 0, n * 16) };
        ptr
    }

    /// NUL terminated UTF-8 copy of a JS string.
    #[inline]
    fn c_string(&mut self, scope: &mut v8::PinScope<'s, '_>, s: v8::Local<'s, v8::String>) -> *mut u8 {
        let len = s.utf8_length(scope);
        let p = self.scratch(len + 1);
        let out = unsafe { std::slice::from_raw_parts_mut(p, len) };
        s.write_utf8_v2(scope, out, v8::WriteFlags::kReplaceInvalidUtf8, None);
        p
    }

    /// Writes structs that were passed by pointer back into their JS objects.
    pub fn finish(self, scope: &mut v8::PinScope<'s, '_>, st: &State) -> Result<(), String> {
        if self.copy_back.is_empty() {
            return Ok(());
        }
        for (object, ty, ptr) in self.copy_back {
            let record = st.record(&ty)?;
            for field in record.fields.iter().filter(|f| !f.name.is_empty()) {
                let v = unsafe { read_field(scope, st, field, ptr, Mem::Temp)? };
                let key = v8::String::new(scope, &field.name).unwrap();
                object.set(scope, key.into(), v);
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

fn to_f64<'s>(scope: &mut v8::PinScope<'s, '_>, v: v8::Local<'s, v8::Value>, ty: &Type) -> Result<f64, String> {
    if let Ok(b) = v8::Local::<v8::BigInt>::try_from(v) {
        return Ok(b.i64_value().0 as f64);
    }
    if v.is_number() || v.is_boolean() || v.is_null_or_undefined() {
        return Ok(v.number_value(scope).unwrap_or(0.0));
    }
    Err(type_error("a number", ty))
}

/// Address of memory backing an ArrayBuffer / typed array / DataView.
pub fn buffer_address(v: v8::Local<v8::Value>) -> Option<usize> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        return Some(view.data() as usize);
    }
    if let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        return Some(buffer.data().map(|p| p.as_ptr() as usize).unwrap_or(0));
    }
    None
}

fn is_signed(ty: &Type) -> bool {
    matches!(ty, Type::Char | Type::Short | Type::Int | Type::Long | Type::LongLong)
}

fn bit_span(offset_bits: u32, width: u8) -> (usize, u32, usize, u128) {
    let byte = (offset_bits / 8) as usize;
    let shift = offset_bits % 8;
    let len = (shift as usize + width as usize).div_ceil(8);
    let mask = if width >= 128 { u128::MAX } else { (1u128 << width) - 1 };
    (byte, shift, len, mask)
}

unsafe fn read_bits(base: *const u8, offset_bits: u32, width: u8, signed: bool) -> i64 {
    let (byte, shift, len, mask) = bit_span(offset_bits, width);
    let mut buf = [0u8; 16];
    std::ptr::copy_nonoverlapping(base.add(byte), buf.as_mut_ptr(), len);
    let raw = ((u128::from_le_bytes(buf) >> shift) & mask) as u64;
    if signed && width < 64 && raw >> (width - 1) & 1 == 1 {
        (raw | (u64::MAX << width)) as i64
    } else {
        raw as i64
    }
}

unsafe fn write_bits(base: *mut u8, offset_bits: u32, width: u8, value: i64) {
    let (byte, shift, len, mask) = bit_span(offset_bits, width);
    let mut buf = [0u8; 16];
    std::ptr::copy_nonoverlapping(base.add(byte), buf.as_mut_ptr(), len);
    let mut raw = u128::from_le_bytes(buf);
    raw &= !(mask << shift);
    raw |= ((value as u64 as u128) & mask) << shift;
    let bytes = raw.to_le_bytes();
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(byte), len);
}

/// Reads a struct / union field at `base`.
pub unsafe fn read_field<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    field: &Field,
    base: *const u8,
    mem: Mem<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    match field.bits {
        Some((offset, width)) => {
            let v = read_bits(base, offset, width, is_signed(&field.ty));
            Ok(if field.ty == Type::Bool {
                v8::Boolean::new(scope, v != 0).into()
            } else {
                number_or_bigint(scope, v)
            })
        }
        None => read(scope, st, &field.ty, base.add(field.offset), mem),
    }
}

/// Writes a struct / union field at `base`.
pub unsafe fn write_field<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    field: &Field,
    value: v8::Local<'s, v8::Value>,
    base: *mut u8,
    cx: &mut CallCx<'s>,
) -> Result<(), String> {
    match field.bits {
        Some((offset, width)) => {
            let v = to_i64(scope, value, &field.ty)?;
            write_bits(base, offset, width, v);
            Ok(())
        }
        None => write(scope, st, &field.ty, value, base.add(field.offset), cx),
    }
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
        Type::CString | Type::ConstCString => {
            let addr = if let Ok(s) = v8::Local::<v8::String>::try_from(value) {
                cx.c_string(scope, s) as usize
            } else {
                pointer_like(scope, st, &Type::Char, value)
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
                pointer_like(scope, st, &Type::Void, value)
                    .ok_or_else(|| type_error("a function, Pointer or null", ty))?
            };
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(..) | Type::AnonymousUnion(..) => {
            let record = st.record(ty)?;
            if let Some(src) = structs::address(scope, value)
                .or_else(|| interop::pointer_address(scope, value))
                .or_else(|| interop::reference_storage(scope, value))
                .or_else(|| buffer_address(value))
            {
                if src != 0 && src != dst as usize {
                    std::ptr::copy(src as *const u8, dst, record.size);
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
                write_field(scope, st, field, v, dst, cx)?;
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
                *dst.add(len) = 0;
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

/// Pointer, Reference, struct view, ArrayBuffer(View) or null -> address.
fn pointer_like<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    pointee: &Type,
    value: v8::Local<'s, v8::Value>,
) -> Option<usize> {
    if value.is_null_or_undefined() {
        return Some(0);
    }
    if let Some(addr) = buffer_address(value) {
        return Some(addr);
    }
    match crate::handles::of(value) {
        Some((o, crate::handles::Kind::Pointer | crate::handles::Kind::Struct)) => Some(crate::handles::address(o)),
        Some((_, crate::handles::Kind::Reference)) => interop::reference_ensure_storage(scope, st, value, pointee).ok(),
        None => None,
    }
}

/// Value passed for a `T*` parameter.
unsafe fn pointer_arg<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    pointee: &Type,
    value: v8::Local<'s, v8::Value>,
    cx: &mut CallCx<'s>,
) -> Result<usize, String> {
    if let Some(addr) = pointer_like(scope, st, pointee, value) {
        return Ok(addr);
    }
    // conveniences: strings for char* / void*, plain objects for struct pointers
    if matches!(pointee, Type::Char | Type::UChar | Type::Void) {
        if let Ok(s) = v8::Local::<v8::String>::try_from(value) {
            return Ok(cx.c_string(scope, s) as usize);
        }
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
        "expected a Pointer, Reference, struct, ArrayBuffer or null for pointer to {pointee:?}"
    ))
}

pub fn number_or_bigint<'s>(scope: &mut v8::PinScope<'s, '_>, v: i64) -> v8::Local<'s, v8::Value> {
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
    mem: Mem<'s>,
) -> Result<v8::Local<'s, v8::Value>, String> {
    macro_rules! num {
        ($t:ty) => {
            v8::Number::new(scope, (src as *const $t).read_unaligned() as f64).into()
        };
    }
    // small integers are Smis, no heap number
    macro_rules! int {
        ($t:ty) => {
            v8::Integer::new(scope, (src as *const $t).read_unaligned() as i32).into()
        };
    }
    Ok(match ty {
        Type::Void => v8::undefined(scope).into(),
        Type::Bool => v8::Boolean::new(scope, *src != 0).into(),
        Type::Char => int!(i8),
        Type::UChar => int!(u8),
        Type::Short => int!(i16),
        Type::UShort | Type::Unichar => int!(u16),
        Type::Int => int!(i32),
        Type::UInt => v8::Integer::new_from_unsigned(scope, (src as *const u32).read_unaligned()).into(),
        Type::Float => num!(f32),
        Type::Double => num!(f64),
        Type::Long | Type::LongLong => number_or_bigint(scope, (src as *const i64).read_unaligned()),
        Type::ULong | Type::ULongLong => unsigned_number_or_bigint(scope, (src as *const u64).read_unaligned()),
        Type::LongDouble => {
            let mut bytes = [0u8; 16];
            std::ptr::copy_nonoverlapping(src, bytes.as_mut_ptr(), 16);
            v8::Number::new(scope, longdouble::from_bytes(&bytes)).into()
        }
        // const char* isn't owned by the caller: copy it into a JS string
        Type::ConstCString => {
            let addr = (src as *const usize).read_unaligned();
            if addr == 0 {
                v8::null(scope).into()
            } else {
                let s = CStr::from_ptr(addr as *const libc::c_char).to_string_lossy();
                v8::String::new(scope, &s).ok_or("string too long")?.into()
            }
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
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(..) | Type::AnonymousUnion(..) => match mem {
            Mem::Temp => structs::copy(scope, st, ty, src)?,
            Mem::Live(owner) => structs::view(scope, st, ty, src as usize, owner)?,
        },
        Type::ConstantArray(n, inner) | Type::ExtVector(n, inner) => {
            let elem = st.layout(inner)?;
            let mut items = Vec::with_capacity(*n as usize);
            for i in 0..*n as usize {
                items.push(read(scope, st, inner, src.add(i * elem.size), mem)?);
            }
            v8::Array::new_with_elements(scope, &items).into()
        }
        other => return Err(format!("reading {other:?} values is not supported")),
    })
}

/// Plain JS object with the fields of the record at `src` (copied).
pub unsafe fn record_to_object<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    st: &State,
    ty: &Type,
    src: *const u8,
) -> Result<v8::Local<'s, v8::Object>, String> {
    let record = st.record(ty)?;
    let object = v8::Object::new(scope);
    for field in record.fields.iter().filter(|f| !f.name.is_empty()) {
        let mut v = read_field(scope, st, field, src, Mem::Temp)?;
        // nested records become plain objects too
        if let Some(addr) = structs::address(scope, v) {
            v = record_to_object(scope, st, &field.ty, addr as *const u8)?.into();
        }
        let key = v8::String::new(scope, &field.name).unwrap();
        object.set(scope, key.into(), v);
    }
    Ok(object)
}
