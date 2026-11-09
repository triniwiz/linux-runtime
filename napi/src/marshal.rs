//! Conversions between JS values and C memory.

use crate::js::*;
use crate::state::{handle, Kind, State};
use crate::sys::*;
use crate::{ffi, interop, structs};
use nativescript_core::{longdouble, Field};
use nativescript_metadata::Type;
use std::ffi::CStr;

/// Where memory being read comes from, decides how records are exposed.
#[derive(Clone, Copy)]
pub enum Mem {
    /// dies after the call: records are copied
    Temp,
    /// stays valid: records become live views kept alive by `owner`
    Live(napi_value),
}

const INLINE: usize = 64;

/// Temporaries that must outlive a native call. Small calls allocate nothing.
pub struct CallCx {
    inline: [std::mem::MaybeUninit<u128>; INLINE],
    used: usize,
    heap: Vec<Box<[u128]>>,
    /// structs passed by pointer from plain JS objects, copied back after the call
    copy_back: Vec<(napi_value, Type, *mut u8)>,
}

impl Default for CallCx {
    fn default() -> Self {
        CallCx {
            inline: [std::mem::MaybeUninit::uninit(); INLINE],
            used: 0,
            heap: Vec::new(),
            copy_back: Vec::new(),
        }
    }
}

impl CallCx {
    /// Zeroed, 16-byte aligned scratch memory that lives until the call returns.
    #[inline]
    pub fn scratch(&mut self, size: usize) -> *mut u8 {
        let n = size.div_ceil(16).max(1);
        if self.used + n <= INLINE {
            let p = self.inline[self.used..].as_mut_ptr() as *mut u8;
            self.used += n;
            unsafe { std::ptr::write_bytes(p, 0, n * 16) };
            return p;
        }
        let mut buf = vec![0u128; n].into_boxed_slice();
        let p = buf.as_mut_ptr() as *mut u8;
        self.heap.push(buf);
        p
    }

    #[inline]
    fn c_string(&mut self, js: Js, s: napi_value) -> Result<*mut u8, String> {
        let len = js.utf8_len(s).map_err(|_| "string expected")?;
        let p = self.scratch(len + 1);
        js.write_utf8(s, p, len + 1)
            .map_err(|_| "string expected")?;
        Ok(p)
    }

    pub fn finish(&mut self, st: &State) -> Result<(), String> {
        if self.copy_back.is_empty() {
            return Ok(());
        }
        let js = st.js();
        for (object, ty, ptr) in std::mem::take(&mut self.copy_back) {
            let record = st.record(&ty)?;
            for field in record.fields.iter().filter(|f| !f.name.is_empty()) {
                let v = unsafe { read_field(st, field, ptr, Mem::Temp)? };
                js.set(object, &field.name, v);
            }
        }
        Ok(())
    }
}

fn type_error(expected: &str, ty: &Type) -> String {
    format!("expected {expected} for {ty:?}")
}

pub fn to_i64(js: Js, v: napi_value, ty: &Type) -> Result<i64, String> {
    match js.type_of(v) {
        NUMBER => {
            let n = js.f64_of(v).unwrap_or(0.0);
            Ok(if n.is_finite() { n as i64 } else { 0 })
        }
        BIGINT => Ok(js.bigint_i64(v).unwrap_or(0)),
        BOOLEAN => Ok(js.bool_of(v) as i64),
        UNDEFINED | NULL => Ok(0),
        OBJECT => match handle(js, v) {
            Some(h) if h.kind == Kind::Pointer => Ok(h.address as i64),
            _ => Err(type_error("a number or bigint", ty)),
        },
        _ => Err(type_error("a number or bigint", ty)),
    }
}

fn to_f64(js: Js, v: napi_value, ty: &Type) -> Result<f64, String> {
    match js.type_of(v) {
        NUMBER => Ok(js.f64_of(v).unwrap_or(0.0)),
        BIGINT => Ok(js.bigint_i64(v).unwrap_or(0) as f64),
        BOOLEAN => Ok(js.bool_of(v) as i64 as f64),
        UNDEFINED | NULL => Ok(0.0),
        _ => Err(type_error("a number", ty)),
    }
}

fn is_signed(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Char | Type::Short | Type::Int | Type::Long | Type::LongLong
    )
}

fn bit_span(offset_bits: u32, width: u8) -> (usize, u32, usize, u128) {
    let byte = (offset_bits / 8) as usize;
    let shift = offset_bits % 8;
    let len = (shift as usize + width as usize).div_ceil(8);
    let mask = if width >= 128 {
        u128::MAX
    } else {
        (1u128 << width) - 1
    };
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
    std::ptr::copy_nonoverlapping(raw.to_le_bytes().as_ptr(), base.add(byte), len);
}

pub unsafe fn read_field(
    st: &State,
    field: &Field,
    base: *const u8,
    mem: Mem,
) -> Result<napi_value, String> {
    match field.bits {
        Some((offset, width)) => {
            let v = read_bits(base, offset, width, is_signed(&field.ty));
            Ok(if field.ty == Type::Bool {
                st.js().boolean(v != 0)
            } else {
                st.js().i64(v)
            })
        }
        None => read(st, &field.ty, base.add(field.offset), mem),
    }
}

pub unsafe fn write_field(
    st: &State,
    field: &Field,
    value: napi_value,
    base: *mut u8,
    cx: &mut CallCx,
) -> Result<(), String> {
    match field.bits {
        Some((offset, width)) => {
            write_bits(base, offset, width, to_i64(st.js(), value, &field.ty)?);
            Ok(())
        }
        None => write(st, &field.ty, value, base.add(field.offset), cx),
    }
}

/// Address of a Pointer / struct view / buffer, null for null/undefined.
fn pointer_like(st: &State, pointee: &Type, value: napi_value) -> Result<Option<usize>, String> {
    let js = st.js();
    match js.type_of(value) {
        UNDEFINED | NULL => return Ok(Some(0)),
        OBJECT | FUNCTION => {}
        _ => return Ok(None),
    }
    if let Some((p, _)) = js.buffer_of(value) {
        return Ok(Some(p as usize));
    }
    match handle(js, value) {
        Some(h) if matches!(h.kind, Kind::Pointer | Kind::Struct) => Ok(Some(h.address)),
        Some(h) if h.kind == Kind::Reference => {
            interop::reference_ensure_storage(st, value, h, pointee).map(Some)
        }
        _ => Ok(None),
    }
}

/// Writes `value` as a `ty` at `dst` (`layout(ty).size` writable bytes).
pub unsafe fn write(
    st: &State,
    ty: &Type,
    value: napi_value,
    dst: *mut u8,
    cx: &mut CallCx,
) -> Result<(), String> {
    let js = st.js();
    macro_rules! int {
        ($t:ty) => {{
            (dst as *mut $t).write_unaligned(to_i64(js, value, ty)? as $t);
        }};
    }
    match ty {
        Type::Void => {}
        Type::Bool => *dst = js.bool_of(value) as u8,
        Type::Char => int!(i8),
        Type::UChar => int!(u8),
        Type::Short => int!(i16),
        Type::UShort | Type::Unichar => int!(u16),
        Type::Int => int!(i32),
        Type::UInt => int!(u32),
        Type::Long | Type::LongLong => int!(i64),
        Type::ULong | Type::ULongLong => {
            let v = if js.type_of(value) == BIGINT {
                js.bigint_u64_of(value).unwrap_or(0)
            } else {
                to_i64(js, value, ty)? as u64
            };
            (dst as *mut u64).write_unaligned(v);
        }
        Type::Float => (dst as *mut f32).write_unaligned(to_f64(js, value, ty)? as f32),
        Type::Double => (dst as *mut f64).write_unaligned(to_f64(js, value, ty)?),
        Type::LongDouble => {
            let bytes = longdouble::to_bytes(to_f64(js, value, ty)?);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, 16);
        }
        Type::CString | Type::ConstCString => {
            let addr = if js.type_of(value) == STRING {
                cx.c_string(js, value)? as usize
            } else {
                pointer_like(st, &Type::Char, value)?.ok_or_else(|| {
                    type_error("a string, Pointer, Reference, ArrayBuffer or null", ty)
                })?
            };
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::Pointer(inner) | Type::IncompleteArray(inner) => {
            let addr = pointer_arg(st, inner, value, cx)?;
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::FunctionPointer(sig) | Type::Block(sig) => {
            let addr = if js.type_of(value) == FUNCTION {
                match interop::native_function_address(st, value) {
                    Some(a) => a,
                    None => ffi::closure_for(st, value, sig)?,
                }
            } else {
                pointer_like(st, &Type::Void, value)?
                    .ok_or_else(|| type_error("a function, Pointer or null", ty))?
            };
            (dst as *mut usize).write_unaligned(addr);
        }
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(..) | Type::AnonymousUnion(..) => {
            let record = st.record(ty)?;
            match js.type_of(value) {
                UNDEFINED | NULL => return Ok(()),
                OBJECT | FUNCTION => {}
                _ => return Err(type_error("an object", ty)),
            }
            if let Some(src) = pointer_like(st, ty, value)? {
                if src != 0 && src != dst as usize {
                    std::ptr::copy(src as *const u8, dst, record.size);
                }
                return Ok(());
            }
            for field in &record.fields {
                if field.name.is_empty() {
                    continue;
                }
                let v = js
                    .get(value, &field.name)
                    .map_err(|_| "failed to read field")?;
                if js.type_of(v) == UNDEFINED {
                    continue;
                }
                write_field(st, field, v, dst, cx)?;
            }
        }
        Type::ConstantArray(n, inner) => {
            let elem = st.layout(inner)?;
            let mut is_array = false;
            napi_is_array(js.0, value, &mut is_array);
            if is_array {
                let mut len = 0;
                napi_get_array_length(js.0, value, &mut len);
                for i in 0..len.min(*n) {
                    let mut v = std::ptr::null_mut();
                    napi_get_element(js.0, value, i, &mut v);
                    write(st, inner, v, dst.add(i as usize * elem.size), cx)?;
                }
            } else if let Some((src, len)) = js.buffer_of(value) {
                std::ptr::copy_nonoverlapping(src, dst, len.min(elem.size * *n as usize));
            } else if js.type_of(value) == STRING && matches!(**inner, Type::Char | Type::UChar) {
                let s = js.rust_string(value).unwrap_or_default();
                let len = s.len().min(*n as usize - 1);
                std::ptr::copy_nonoverlapping(s.as_ptr(), dst, len);
                *dst.add(len) = 0;
            } else if !matches!(js.type_of(value), UNDEFINED | NULL) {
                return Err(type_error("an array", ty));
            }
        }
        other => return Err(format!("passing {other:?} values is not supported")),
    }
    Ok(())
}

/// Value passed for a `T*` parameter.
unsafe fn pointer_arg(
    st: &State,
    pointee: &Type,
    value: napi_value,
    cx: &mut CallCx,
) -> Result<usize, String> {
    let js = st.js();
    if let Some(addr) = pointer_like(st, pointee, value)? {
        return Ok(addr);
    }
    if js.type_of(value) == STRING && matches!(pointee, Type::Char | Type::UChar | Type::Void) {
        return Ok(cx.c_string(js, value)? as usize);
    }
    if js.type_of(value) == OBJECT {
        if let Ok(record) = st.record(pointee) {
            let mem = cx.scratch(record.size);
            write(st, pointee, value, mem, cx)?;
            cx.copy_back.push((value, pointee.clone(), mem));
            return Ok(mem as usize);
        }
        let mut is_array = false;
        napi_is_array(js.0, value, &mut is_array);
        if let (true, Ok(elem)) = (is_array, st.layout(pointee)) {
            let mut len = 0;
            napi_get_array_length(js.0, value, &mut len);
            let mem = cx.scratch(elem.size * len as usize);
            for i in 0..len {
                let mut v = std::ptr::null_mut();
                napi_get_element(js.0, value, i, &mut v);
                write(st, pointee, v, mem.add(i as usize * elem.size), cx)?;
            }
            return Ok(mem as usize);
        }
    }
    Err(format!(
        "expected a Pointer, Reference, struct, ArrayBuffer or null for pointer to {pointee:?}"
    ))
}

/// Reads a `ty` from `src`.
pub unsafe fn read(st: &State, ty: &Type, src: *const u8, mem: Mem) -> Result<napi_value, String> {
    let js = st.js();
    Ok(match ty {
        Type::Void => js.undefined(),
        Type::Bool => js.boolean(*src != 0),
        Type::Char => js.int((src as *const i8).read_unaligned() as i32),
        Type::UChar => js.int(*src as i32),
        Type::Short => js.int((src as *const i16).read_unaligned() as i32),
        Type::UShort | Type::Unichar => js.int((src as *const u16).read_unaligned() as i32),
        Type::Int => js.int((src as *const i32).read_unaligned()),
        Type::UInt => js.uint((src as *const u32).read_unaligned()),
        Type::Float => js.number((src as *const f32).read_unaligned() as f64),
        Type::Double => js.number((src as *const f64).read_unaligned()),
        Type::Long | Type::LongLong => js.i64((src as *const i64).read_unaligned()),
        Type::ULong | Type::ULongLong => js.u64((src as *const u64).read_unaligned()),
        Type::LongDouble => {
            let mut bytes = [0u8; 16];
            std::ptr::copy_nonoverlapping(src, bytes.as_mut_ptr(), 16);
            js.number(longdouble::from_bytes(&bytes))
        }
        Type::ConstCString => {
            let addr = (src as *const usize).read_unaligned();
            if addr == 0 {
                js.null()
            } else {
                let s = CStr::from_ptr(addr as *const libc::c_char).to_string_lossy();
                js.string(&s)
            }
        }
        t if t.is_pointer_like() => {
            let addr = (src as *const usize).read_unaligned();
            if addr == 0 {
                js.null()
            } else if let Type::FunctionPointer(sig) = t {
                ffi::function_from_pointer(st, addr, sig)?
            } else {
                interop::new_pointer(st, addr, None)
            }
        }
        Type::Struct(_) | Type::Union(_) | Type::AnonymousStruct(..) | Type::AnonymousUnion(..) => {
            match mem {
                Mem::Temp => structs::copy(st, ty, src)?,
                Mem::Live(owner) => structs::view(st, ty, src as usize, Some(owner))?,
            }
        }
        Type::ConstantArray(n, inner) | Type::ExtVector(n, inner) => {
            let elem = st.layout(inner)?;
            let mut items = Vec::with_capacity(*n as usize);
            for i in 0..*n as usize {
                items.push(read(st, inner, src.add(i * elem.size), mem)?);
            }
            js.array(&items)
        }
        other => return Err(format!("reading {other:?} values is not supported")),
    })
}

/// Plain object with the fields of the record at `src` (copied).
pub unsafe fn record_to_object(
    st: &State,
    ty: &Type,
    src: *const u8,
) -> Result<napi_value, String> {
    let js = st.js();
    let record = st.record(ty)?;
    let object = js.object();
    for field in record.fields.iter().filter(|f| !f.name.is_empty()) {
        let mut v = read_field(st, field, src, Mem::Temp)?;
        if let Some(h) = handle(js, v).filter(|h| h.kind == Kind::Struct) {
            v = record_to_object(st, &field.ty, h.address as *const u8)?;
        }
        js.set(object, &field.name, v);
    }
    Ok(object)
}
