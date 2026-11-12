//! Thin helpers over the raw Node-API calls.

use crate::sys::*;
use std::ffi::{c_char, c_void, CString};
use std::ptr::null_mut;

pub const UNDEFINED: i32 = 0;
pub const NULL: i32 = 1;
pub const BOOLEAN: i32 = 2;
pub const NUMBER: i32 = 3;
pub const STRING: i32 = 4;
pub const SYMBOL: i32 = 5;
pub const OBJECT: i32 = 6;
pub const FUNCTION: i32 = 7;
pub const EXTERNAL: i32 = 8;
pub const BIGINT: i32 = 9;

/// A JS exception is pending (or the call failed).
#[derive(Debug)]
pub struct Thrown;

pub type JsResult<T> = Result<T, Thrown>;

#[inline]
pub fn ok(status: napi_status) -> JsResult<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Thrown)
    }
}

macro_rules! out {
    ($ty:ty, |$p:ident| $call:expr) => {{
        let mut v: $ty = unsafe { std::mem::zeroed() };
        let $p = &mut v;
        ok(unsafe { $call })?;
        Ok(v)
    }};
}

#[derive(Clone, Copy)]
pub struct Js(pub napi_env);

impl Js {
    #[inline]
    pub fn undefined(self) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_get_undefined(self.0, &mut v) };
        v
    }
    #[inline]
    pub fn null(self) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_get_null(self.0, &mut v) };
        v
    }
    #[inline]
    pub fn global(self) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_get_global(self.0, &mut v) };
        v
    }
    #[inline]
    pub fn boolean(self, b: bool) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_get_boolean(self.0, b, &mut v) };
        v
    }
    #[inline]
    pub fn number(self, n: f64) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_double(self.0, n, &mut v) };
        v
    }
    #[inline]
    pub fn int(self, n: i32) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_int32(self.0, n, &mut v) };
        v
    }
    #[inline]
    pub fn uint(self, n: u32) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_uint32(self.0, n, &mut v) };
        v
    }
    pub fn i64(self, n: i64) -> napi_value {
        if n.unsigned_abs() <= 1 << 53 {
            return self.number(n as f64);
        }
        let mut v = null_mut();
        unsafe { napi_create_bigint_int64(self.0, n, &mut v) };
        v
    }
    pub fn u64(self, n: u64) -> napi_value {
        if n <= 1 << 53 {
            return self.number(n as f64);
        }
        let mut v = null_mut();
        unsafe { napi_create_bigint_uint64(self.0, n, &mut v) };
        v
    }
    pub fn bigint_u64(self, n: u64) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_bigint_uint64(self.0, n, &mut v) };
        v
    }
    pub fn string(self, s: &str) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_string_utf8(self.0, s.as_ptr() as *const c_char, s.len(), &mut v) };
        v
    }
    pub fn object(self) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_object(self.0, &mut v) };
        v
    }
    pub fn array(self, items: &[napi_value]) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_array_with_length(self.0, items.len(), &mut v) };
        for (i, item) in items.iter().enumerate() {
            unsafe { napi_set_element(self.0, v, i as u32, *item) };
        }
        v
    }
    #[inline]
    pub fn type_of(self, v: napi_value) -> i32 {
        let mut t = UNDEFINED;
        unsafe { napi_typeof(self.0, v, &mut t) };
        t
    }
    #[inline]
    pub fn f64_of(self, v: napi_value) -> JsResult<f64> {
        out!(f64, |p| napi_get_value_double(self.0, v, p))
    }
    pub fn bool_of(self, v: napi_value) -> bool {
        let mut b = false;
        let mut coerced = null_mut();
        unsafe {
            napi_coerce_to_bool(self.0, v, &mut coerced);
            napi_get_value_bool(self.0, coerced, &mut b);
        }
        b
    }
    pub fn bigint_i64(self, v: napi_value) -> JsResult<i64> {
        let mut lossless = false;
        out!(i64, |p| napi_get_value_bigint_int64(
            self.0,
            v,
            p,
            &mut lossless
        ))
    }
    pub fn bigint_u64_of(self, v: napi_value) -> JsResult<u64> {
        let mut lossless = false;
        out!(u64, |p| napi_get_value_bigint_uint64(
            self.0,
            v,
            p,
            &mut lossless
        ))
    }
    pub fn utf8_len(self, v: napi_value) -> JsResult<usize> {
        out!(usize, |p| napi_get_value_string_utf8(
            self.0,
            v,
            null_mut(),
            0,
            p
        ))
    }
    /// Writes the string NUL terminated into `buf` (`utf8_len + 1` bytes).
    pub fn write_utf8(self, v: napi_value, buf: *mut u8, cap: usize) -> JsResult<usize> {
        out!(usize, |p| napi_get_value_string_utf8(
            self.0,
            v,
            buf as *mut c_char,
            cap,
            p
        ))
    }
    pub fn rust_string(self, v: napi_value) -> JsResult<String> {
        let len = self.utf8_len(v)?;
        let mut buf = vec![0u8; len + 1];
        self.write_utf8(v, buf.as_mut_ptr(), len + 1)?;
        buf.truncate(len);
        Ok(String::from_utf8(buf).unwrap_or_else(|e| wtf8_lossy(e.into_bytes())))
    }
    /// String value, coercing non strings.
    pub fn to_string(self, v: napi_value) -> String {
        let mut s = null_mut();
        if unsafe { napi_coerce_to_string(self.0, v, &mut s) } != 0 {
            self.clear_exception();
            return String::new();
        }
        self.rust_string(s).unwrap_or_default()
    }
    pub fn get(self, object: napi_value, key: &str) -> JsResult<napi_value> {
        let key = CString::new(key).unwrap();
        out!(napi_value, |p| napi_get_named_property(
            self.0,
            object,
            key.as_ptr(),
            p
        ))
    }
    pub fn set(self, object: napi_value, key: &str, value: napi_value) {
        let key = CString::new(key).unwrap();
        unsafe { napi_set_named_property(self.0, object, key.as_ptr(), value) };
    }
    pub fn call(
        self,
        f: napi_value,
        recv: napi_value,
        args: &[napi_value],
    ) -> JsResult<napi_value> {
        out!(napi_value, |p| napi_call_function(
            self.0,
            recv,
            f,
            args.len(),
            args.as_ptr(),
            p
        ))
    }
    pub fn new_instance(self, ctor: napi_value, args: &[napi_value]) -> JsResult<napi_value> {
        out!(napi_value, |p| napi_new_instance(
            self.0,
            ctor,
            args.len(),
            args.as_ptr(),
            p
        ))
    }
    pub fn function(self, name: &str, cb: napi_callback, data: *mut c_void) -> napi_value {
        let mut v = null_mut();
        unsafe {
            napi_create_function(
                self.0,
                name.as_ptr() as *const c_char,
                name.len(),
                cb,
                data,
                &mut v,
            )
        };
        v
    }
    pub fn external(self, data: *mut c_void) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_create_external(self.0, data, None, null_mut(), &mut v) };
        v
    }
    pub fn throw(self, message: &str) {
        let m = CString::new(message.replace('\0', "")).unwrap();
        unsafe { napi_throw_error(self.0, null_mut(), m.as_ptr()) };
    }
    pub fn throw_type(self, message: &str) {
        let m = CString::new(message.replace('\0', "")).unwrap();
        unsafe { napi_throw_type_error(self.0, null_mut(), m.as_ptr()) };
    }
    pub fn is_exception_pending(self) -> bool {
        let mut b = false;
        unsafe { napi_is_exception_pending(self.0, &mut b) };
        b
    }
    pub fn take_exception(self) -> Option<napi_value> {
        if !self.is_exception_pending() {
            return None;
        }
        let mut v = null_mut();
        unsafe { napi_get_and_clear_last_exception(self.0, &mut v) };
        Some(v)
    }
    pub fn clear_exception(self) {
        let _ = self.take_exception();
    }
    /// `stack` (or string form) of an error value.
    pub fn describe(self, e: napi_value) -> String {
        if self.type_of(e) == OBJECT {
            if let Ok(stack) = self.get(e, "stack") {
                if self.type_of(stack) == STRING {
                    let stack = self.rust_string(stack).unwrap_or_default();
                    let head = self.to_string(e);
                    // some engines leave the message out of `stack`
                    return if stack.starts_with(&head) {
                        stack
                    } else {
                        format!("{head}\n{}", stack.trim_end())
                    };
                }
            }
        }
        self.to_string(e)
    }
    pub fn reference(self, v: napi_value, count: u32) -> napi_ref {
        let mut r = null_mut();
        unsafe { napi_create_reference(self.0, v, count, &mut r) };
        r
    }
    #[inline]
    pub fn deref(self, r: napi_ref) -> napi_value {
        let mut v = null_mut();
        unsafe { napi_get_reference_value(self.0, r, &mut v) };
        v
    }
    /// Non-enumerable, read-only property.
    pub fn define_hidden(self, object: napi_value, key: &str, value: napi_value) {
        self.define(object, key, value, 0);
    }
    /// napi_property_attributes: 1 writable, 2 enumerable, 4 configurable
    pub fn define(self, object: napi_value, key: &str, value: napi_value, attributes: i32) {
        let desc = napi_property_descriptor {
            utf8name: std::ptr::null(),
            name: self.string(key),
            method: None,
            getter: None,
            setter: None,
            value,
            attributes,
            data: null_mut(),
        };
        unsafe { napi_define_properties(self.0, object, 1, &desc) };
    }
    pub fn delete_ref(self, r: napi_ref) {
        if !r.is_null() {
            unsafe { napi_delete_reference(self.0, r) };
        }
    }
    pub fn strict_equals(self, a: napi_value, b: napi_value) -> bool {
        let mut r = false;
        unsafe { napi_strict_equals(self.0, a, b, &mut r) };
        r
    }
    pub fn arraybuffer(self, len: usize) -> (napi_value, *mut u8) {
        let mut v = null_mut();
        let mut data = null_mut();
        unsafe { napi_create_arraybuffer(self.0, len, &mut data, &mut v) };
        (v, data as *mut u8)
    }
    /// Address and length of an ArrayBuffer / typed array / DataView.
    pub fn buffer_of(self, v: napi_value) -> Option<(*mut u8, usize)> {
        let mut is = false;
        let mut data = null_mut();
        let mut len = 0usize;
        unsafe {
            napi_is_typedarray(self.0, v, &mut is);
            if is {
                let mut kind = 0;
                let mut ab = null_mut();
                let mut offset = 0;
                napi_get_typedarray_info(
                    self.0,
                    v,
                    &mut kind,
                    &mut len,
                    &mut data,
                    &mut ab,
                    &mut offset,
                );
                let size = match kind {
                    0..=2 => 1,
                    3 | 4 => 2,
                    5..=7 => 4,
                    _ => 8,
                };
                return Some((data as *mut u8, len * size));
            }
            napi_is_arraybuffer(self.0, v, &mut is);
            if is {
                napi_get_arraybuffer_info(self.0, v, &mut data, &mut len);
                return Some((data as *mut u8, len));
            }
            napi_is_dataview(self.0, v, &mut is);
            if is {
                let mut ab = null_mut();
                let mut offset = 0;
                napi_get_dataview_info(self.0, v, &mut len, &mut data, &mut ab, &mut offset);
                return Some((data as *mut u8, len));
            }
        }
        None
    }
}

/// Up to `N` arguments (missing ones are undefined), argc, `this` and the callback data.
#[inline]
pub unsafe fn cb_args<const N: usize>(
    env: napi_env,
    info: napi_callback_info,
) -> ([napi_value; N], usize, napi_value, *mut c_void) {
    let mut argv = [null_mut(); N];
    let mut argc = N;
    let mut this = null_mut();
    let mut data = null_mut();
    napi_get_cb_info(
        env,
        info,
        &mut argc,
        argv.as_mut_ptr(),
        &mut this,
        &mut data,
    );
    if argc < N {
        let undefined = Js(env).undefined();
        for a in &mut argv[argc..] {
            *a = undefined;
        }
    }
    (argv, argc, this, data)
}

impl Js {
    pub fn uint8array(self, bytes: &[u8]) -> napi_value {
        let (buffer, data) = self.arraybuffer(bytes.len());
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
        let mut v = null_mut();
        unsafe { napi_create_typedarray(self.0, 1, bytes.len(), buffer, 0, &mut v) };
        v
    }
    pub fn i32_of(self, v: napi_value) -> Option<i32> {
        let mut n = 0;
        (unsafe { napi_get_value_int32(self.0, v, &mut n) } == 0).then_some(n)
    }
}

/// Engines hand lone surrogates out as WTF-8 (`ED A0..BF xx`): one U+FFFD each.
fn wtf8_lossy(mut bytes: Vec<u8>) -> String {
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == 0xED && (0xA0..=0xBF).contains(&bytes[i + 1]) {
            bytes[i..i + 3].copy_from_slice(&[0xEF, 0xBF, 0xBD]);
            i += 3;
        } else {
            i += 1;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
