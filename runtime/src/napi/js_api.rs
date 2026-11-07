//! js_native_api.h
#![allow(unused_unsafe)]

use super::*;
use std::cell::Cell;
use std::ffi::CStr;

macro_rules! env {
    ($e:expr) => {{
        if $e.is_null() {
            return INVALID_ARG;
        }
        unsafe { &mut *$e }
    }};
}
macro_rules! arg {
    ($env:ident, $($p:expr),+) => { $( if $p.is_null() { return $env.status(INVALID_ARG); } )+ };
}
macro_rules! scope {
    ($env:ident, $s:ident) => {
        let __ctx = $env.context();
        v8::callback_scope!(unsafe $s, __ctx);
    };
}
macro_rules! tc {
    ($env:ident, $s:ident) => {
        scope!($env, __outer);
        v8::tc_scope!(let $s, __outer);
    };
}
macro_rules! val {
    ($env:ident, $v:expr) => {
        match unsafe { local($v) } {
            Some(v) => v,
            None => return $env.status(INVALID_ARG),
        }
    };
}
macro_rules! cast {
    ($env:ident, $v:expr, $t:ty, $status:expr) => {
        match v8::Local::<$t>::try_from(val!($env, $v)) {
            Ok(v) => v,
            Err(_) => return $env.status($status),
        }
    };
}
macro_rules! object {
    ($env:ident, $s:ident, $v:expr) => {{
        let v = val!($env, $v);
        if !v.is_object() {
            return $env.status(OBJECT_EXPECTED);
        }
        v.to_object($s).unwrap()
    }};
}
/// Stores the caught exception for the caller (rethrown when back in JS).
macro_rules! pending {
    ($env:ident, $tc:ident) => {{
        match $tc.exception() {
            Some(e) => {
                $env.exception = Some(v8::Global::new($tc, e));
                $tc.reset();
                return $env.status(PENDING_EXCEPTION);
            }
            None => return $env.status(GENERIC_FAILURE),
        }
    }};
}
macro_rules! js {
    ($env:ident, $tc:ident, $e:expr) => {
        match $e {
            Some(v) => v,
            None => pending!($env, $tc),
        }
    };
}
macro_rules! preamble {
    ($env:ident) => {
        if $env.exception.is_some() {
            return $env.status(PENDING_EXCEPTION);
        }
    };
}
macro_rules! out {
    ($ptr:expr, $v:expr) => {
        unsafe { *$ptr = $v }
    };
}

const AUTO_LENGTH: usize = usize::MAX;

unsafe fn bytes<'a, T>(ptr: *const T, len: usize, zero: T) -> Option<&'a [T]>
where
    T: PartialEq + Copy,
{
    if ptr.is_null() {
        return (len == 0 || len == AUTO_LENGTH).then_some(&[]);
    }
    if len == AUTO_LENGTH {
        let mut n = 0;
        while *ptr.add(n) != zero {
            n += 1;
        }
        return Some(std::slice::from_raw_parts(ptr, n));
    }
    (len <= i32::MAX as usize).then(|| std::slice::from_raw_parts(ptr, len))
}

fn attributes(a: i32) -> v8::PropertyAttribute {
    let mut r = v8::PropertyAttribute::NONE;
    if a & 1 == 0 {
        r = r | v8::PropertyAttribute::READ_ONLY;
    }
    if a & 2 == 0 {
        r = r | v8::PropertyAttribute::DONT_ENUM;
    }
    if a & 4 == 0 {
        r = r | v8::PropertyAttribute::DONT_DELETE;
    }
    r
}

const STATIC: i32 = 1 << 10;

fn callback_data<'s>(
    env: &mut Env,
    scope: &mut v8::PinScope<'s, '_>,
    cb: napi_callback,
    data: *mut c_void,
) -> Option<v8::Local<'s, v8::External>> {
    let cb = cb?;
    let env_ptr: napi_env = env;
    let mut boxed = Box::new(CallbackData {
        env: env_ptr,
        cb,
        data,
    });
    let ptr = &mut *boxed as *mut CallbackData as *mut c_void;
    env.callbacks.push(boxed);
    Some(v8::External::new(scope, ptr))
}

pub(crate) fn make_function<'s>(
    env: &mut Env,
    scope: &mut v8::PinScope<'s, '_>,
    name: Option<v8::Local<'s, v8::String>>,
    cb: napi_callback,
    data: *mut c_void,
) -> Option<v8::Local<'s, v8::Function>> {
    let ext = callback_data(env, scope, cb, data)?;
    let f = v8::Function::builder(trampoline)
        .data(ext.into())
        .build(scope)?;
    if let Some(name) = name {
        f.set_name(name);
    }
    Some(f)
}

fn template<'s>(
    env: &mut Env,
    scope: &mut v8::PinScope<'s, '_>,
    cb: napi_callback,
    data: *mut c_void,
) -> Option<v8::Local<'s, v8::FunctionTemplate>> {
    let ext = callback_data(env, scope, cb, data)?;
    Some(
        v8::FunctionTemplate::builder(trampoline)
            .data(ext.into())
            .build(scope),
    )
}

#[repr(C)]
pub struct napi_property_descriptor {
    utf8name: *const c_char,
    name: napi_value,
    method: napi_callback,
    getter: napi_callback,
    setter: napi_callback,
    value: napi_value,
    attributes: i32,
    data: *mut c_void,
}

fn property_name<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    p: &napi_property_descriptor,
) -> Option<v8::Local<'s, v8::Name>> {
    if !p.utf8name.is_null() {
        let s = unsafe { CStr::from_ptr(p.utf8name) }.to_str().ok()?;
        return Some(v8::String::new(scope, s)?.into());
    }
    v8::Local::<v8::Name>::try_from(unsafe { local(p.name) }?).ok()
}

fn define_property<'s>(
    env: &mut Env,
    scope: &mut v8::PinScope<'s, '_>,
    object: v8::Local<'s, v8::Object>,
    p: &napi_property_descriptor,
) -> Option<bool> {
    let name = property_name(scope, p)?;
    let enumerable = p.attributes & 2 != 0;
    let configurable = p.attributes & 4 != 0;
    let mut desc = if p.getter.is_some() || p.setter.is_some() {
        let undefined: v8::Local<v8::Value> = v8::undefined(scope).into();
        let getter = make_function(env, scope, None, p.getter, p.data)
            .map(Into::into)
            .unwrap_or(undefined);
        let setter = make_function(env, scope, None, p.setter, p.data)
            .map(Into::into)
            .unwrap_or(undefined);
        v8::PropertyDescriptor::new_from_get_set(getter, setter)
    } else if p.method.is_some() {
        let name = v8::Local::<v8::String>::try_from(name).ok();
        let f = make_function(env, scope, name, p.method, p.data)?;
        v8::PropertyDescriptor::new_from_value_writable(f.into(), p.attributes & 1 != 0)
    } else {
        let value = unsafe { local(p.value) }.unwrap_or_else(|| v8::undefined(scope).into());
        v8::PropertyDescriptor::new_from_value_writable(value, p.attributes & 1 != 0)
    };
    desc.set_enumerable(enumerable);
    desc.set_configurable(configurable);
    object.define_property(scope, name, &desc)
}

fn new_string<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap_or_else(|| v8::String::empty(scope))
}

fn private_key<'s>(scope: &mut v8::PinScope<'s, '_>, name: &str) -> v8::Local<'s, v8::Private> {
    let name = new_string(scope, name);
    v8::Private::for_api(scope, Some(name))
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_last_error_info(
    env: napi_env,
    result: *mut *const napi_extended_error_info,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let code = env.last_error.error_code;
    env.last_error.error_message = if code > 0 && (code as usize) < env.messages.len() {
        env.messages[code as usize].as_ptr()
    } else {
        std::ptr::null()
    };
    out!(result, &env.last_error);
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_undefined(env: napi_env, result: *mut napi_value) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::undefined(s)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_null(env: napi_env, result: *mut napi_value) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::null(s)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_global(env: napi_env, result: *mut napi_value) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let global = env.context().global(s);
    out!(result, napi(global));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_boolean(
    env: napi_env,
    value: bool,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Boolean::new(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_object(env: napi_env, result: *mut napi_value) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Object::new(s)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_array(env: napi_env, result: *mut napi_value) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Array::new(s, 0)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_array_with_length(
    env: napi_env,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(
        result,
        napi(v8::Array::new(s, length.min(i32::MAX as usize) as i32))
    );
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_double(
    env: napi_env,
    value: f64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Number::new(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_int32(
    env: napi_env,
    value: i32,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Integer::new(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_uint32(
    env: napi_env,
    value: u32,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Integer::new_from_unsigned(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_int64(
    env: napi_env,
    value: i64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::Number::new(s, value as f64)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_string_latin1(
    env: napi_env,
    str: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(b) = bytes(str as *const u8, length, 0) else {
        return env.status(INVALID_ARG);
    };
    scope!(env, s);
    match v8::String::new_from_one_byte(s, b, v8::NewStringType::Normal) {
        Some(v) => out!(result, napi(v)),
        None => return env.status(GENERIC_FAILURE),
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_string_utf8(
    env: napi_env,
    str: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(b) = bytes(str as *const u8, length, 0) else {
        return env.status(INVALID_ARG);
    };
    scope!(env, s);
    match v8::String::new_from_utf8(s, b, v8::NewStringType::Normal) {
        Some(v) => out!(result, napi(v)),
        None => return env.status(GENERIC_FAILURE),
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_string_utf16(
    env: napi_env,
    str: *const u16,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(b) = bytes(str, length, 0) else {
        return env.status(INVALID_ARG);
    };
    scope!(env, s);
    match v8::String::new_from_two_byte(s, b, v8::NewStringType::Normal) {
        Some(v) => out!(result, napi(v)),
        None => return env.status(GENERIC_FAILURE),
    }
    env.status(OK)
}

/// Strings are always copied: the finalizer runs right away.
#[no_mangle]
pub unsafe extern "C" fn node_api_create_external_string_latin1(
    env: napi_env,
    str: *mut c_char,
    length: usize,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    let status = napi_create_string_latin1(env, str, length, result);
    if status == OK {
        if let Some(cb) = finalize_cb {
            cb(env, str as *mut c_void, hint);
        }
        if !copied.is_null() {
            *copied = true;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_external_string_utf16(
    env: napi_env,
    str: *mut u16,
    length: usize,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_value,
    copied: *mut bool,
) -> napi_status {
    let status = napi_create_string_utf16(env, str, length, result);
    if status == OK {
        if let Some(cb) = finalize_cb {
            cb(env, str as *mut c_void, hint);
        }
        if !copied.is_null() {
            *copied = true;
        }
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_property_key_utf16(
    env: napi_env,
    str: *const u16,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(b) = bytes(str, length, 0) else {
        return env.status(INVALID_ARG);
    };
    scope!(env, s);
    match v8::String::new_from_two_byte(s, b, v8::NewStringType::Internalized) {
        Some(v) => out!(result, napi(v)),
        None => return env.status(GENERIC_FAILURE),
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_symbol(
    env: napi_env,
    description: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let desc = match local(description) {
        None => None,
        Some(d) => match v8::Local::<v8::String>::try_from(d) {
            Ok(d) => Some(d),
            Err(_) => return env.status(STRING_EXPECTED),
        },
    };
    out!(result, napi(v8::Symbol::new(s, desc)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_symbol_for(
    env: napi_env,
    utf8: *const c_char,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let Some(b) = bytes(utf8 as *const u8, length, 0) else {
        return env.status(INVALID_ARG);
    };
    tc!(env, s);
    let key = js!(
        env,
        s,
        v8::String::new_from_utf8(s, b, v8::NewStringType::Normal)
    );
    // Symbol.for(key)
    let global = env.context().global(s);
    let symbol_name = new_string(s, "Symbol");
    let symbol = js!(env, s, global.get(s, symbol_name.into()))
        .to_object(s)
        .unwrap();
    let for_name = new_string(s, "for");
    let for_fn = js!(env, s, symbol.get(s, for_name.into()));
    let for_fn = v8::Local::<v8::Function>::try_from(for_fn).unwrap();
    let v = js!(env, s, for_fn.call(s, symbol.into(), &[key.into()]));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_function(
    env: napi_env,
    utf8name: *const c_char,
    length: usize,
    cb: napi_callback,
    data: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    if cb.is_none() {
        return env.status(INVALID_ARG);
    }
    scope!(env, s);
    let name = bytes(utf8name as *const u8, length, 0)
        .and_then(|b| v8::String::new_from_utf8(s, b, v8::NewStringType::Normal));
    match make_function(env, s, name, cb, data) {
        Some(f) => out!(result, napi(f)),
        None => return env.status(GENERIC_FAILURE),
    }
    env.status(OK)
}

fn create_error(
    env: &mut Env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
    kind: u8,
) -> napi_status {
    arg!(env, result);
    let msg = cast!(env, msg, v8::String, STRING_EXPECTED);
    scope!(env, s);
    let error = match kind {
        1 => v8::Exception::type_error(s, msg),
        2 => v8::Exception::range_error(s, msg),
        3 => v8::Exception::syntax_error(s, msg),
        _ => v8::Exception::error(s, msg),
    };
    if let Some(code) = unsafe { local(code) } {
        if !code.is_string() {
            return env.status(STRING_EXPECTED);
        }
        let key = new_string(s, "code");
        error.to_object(s).unwrap().set(s, key.into(), code);
    }
    out!(result, napi(error));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
) -> napi_status {
    create_error(env!(env), code, msg, result, 0)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_type_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
) -> napi_status {
    create_error(env!(env), code, msg, result, 1)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_range_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
) -> napi_status {
    create_error(env!(env), code, msg, result, 2)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_syntax_error(
    env: napi_env,
    code: napi_value,
    msg: napi_value,
    result: *mut napi_value,
) -> napi_status {
    create_error(env!(env), code, msg, result, 3)
}

#[no_mangle]
pub unsafe extern "C" fn napi_typeof(
    env: napi_env,
    value: napi_value,
    result: *mut i32,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = val!(env, value);
    let t = if v.is_undefined() {
        0
    } else if v.is_null() {
        1
    } else if v.is_boolean() {
        2
    } else if v.is_number() {
        3
    } else if v.is_string() {
        4
    } else if v.is_symbol() {
        5
    } else if v.is_function() {
        7
    } else if v.is_external() {
        8
    } else if v.is_big_int() {
        9
    } else if v.is_object() {
        6
    } else {
        return env.status(INVALID_ARG);
    };
    out!(result, t);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_double(
    env: napi_env,
    value: napi_value,
    result: *mut f64,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = cast!(env, value, v8::Number, NUMBER_EXPECTED);
    out!(result, v.value());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_int32(
    env: napi_env,
    value: napi_value,
    result: *mut i32,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = cast!(env, value, v8::Number, NUMBER_EXPECTED);
    scope!(env, s);
    out!(result, v.int32_value(s).unwrap_or(0));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_uint32(
    env: napi_env,
    value: napi_value,
    result: *mut u32,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = cast!(env, value, v8::Number, NUMBER_EXPECTED);
    scope!(env, s);
    out!(result, v.uint32_value(s).unwrap_or(0));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_int64(
    env: napi_env,
    value: napi_value,
    result: *mut i64,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = cast!(env, value, v8::Number, NUMBER_EXPECTED);
    let n = v.value();
    out!(result, if n.is_finite() { n as i64 } else { 0 });
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_bool(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = cast!(env, value, v8::Boolean, BOOLEAN_EXPECTED);
    out!(result, v.is_true());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_string_latin1(
    env: napi_env,
    value: napi_value,
    buf: *mut c_char,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    let env = env!(env);
    let v = cast!(env, value, v8::String, STRING_EXPECTED);
    scope!(env, s);
    if buf.is_null() {
        arg!(env, result);
        out!(result, v.length());
    } else if bufsize > 0 {
        let n = v.length().min(bufsize - 1);
        let out = std::slice::from_raw_parts_mut(buf as *mut u8, n);
        v.write_one_byte_v2(s, 0, out, v8::WriteFlags::empty());
        *buf.add(n) = 0;
        if !result.is_null() {
            *result = n;
        }
    } else if !result.is_null() {
        *result = 0;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_string_utf8(
    env: napi_env,
    value: napi_value,
    buf: *mut c_char,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    let env = env!(env);
    let v = cast!(env, value, v8::String, STRING_EXPECTED);
    scope!(env, s);
    if buf.is_null() {
        arg!(env, result);
        out!(result, v.utf8_length(s));
    } else if bufsize > 0 {
        let out = std::slice::from_raw_parts_mut(buf as *mut u8, bufsize - 1);
        let n = v.write_utf8_v2(s, out, v8::WriteFlags::kReplaceInvalidUtf8, None);
        *buf.add(n) = 0;
        if !result.is_null() {
            *result = n;
        }
    } else if !result.is_null() {
        *result = 0;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_string_utf16(
    env: napi_env,
    value: napi_value,
    buf: *mut u16,
    bufsize: usize,
    result: *mut usize,
) -> napi_status {
    let env = env!(env);
    let v = cast!(env, value, v8::String, STRING_EXPECTED);
    scope!(env, s);
    if buf.is_null() {
        arg!(env, result);
        out!(result, v.length());
    } else if bufsize > 0 {
        let n = v.length().min(bufsize - 1);
        let out = std::slice::from_raw_parts_mut(buf, n);
        v.write_v2(s, 0, out, v8::WriteFlags::empty());
        *buf.add(n) = 0;
        if !result.is_null() {
            *result = n;
        }
    } else if !result.is_null() {
        *result = 0;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_coerce_to_bool(
    env: napi_env,
    value: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = val!(env, value);
    scope!(env, s);
    out!(result, napi(v.to_boolean(s)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_coerce_to_number(
    env: napi_env,
    value: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let v = val!(env, value);
    tc!(env, s);
    let n = js!(env, s, v.to_number(s));
    out!(result, napi(n));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_coerce_to_object(
    env: napi_env,
    value: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let v = val!(env, value);
    tc!(env, s);
    let o = js!(env, s, v.to_object(s));
    out!(result, napi(o));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_coerce_to_string(
    env: napi_env,
    value: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let v = val!(env, value);
    tc!(env, s);
    let o = js!(env, s, v.to_string(s));
    out!(result, napi(o));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_prototype(
    env: napi_env,
    object: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let p = js!(env, s, o.get_prototype(s));
    out!(result, napi(p));
    env.status(OK)
}

fn property_names(
    env: &mut Env,
    object: napi_value,
    args: v8::GetPropertyNamesArgs,
    result: *mut napi_value,
) -> napi_status {
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let names = js!(env, s, o.get_property_names(s, args));
    out!(result, napi(names));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_property_names(
    env: napi_env,
    object: napi_value,
    result: *mut napi_value,
) -> napi_status {
    property_names(
        env!(env),
        object,
        v8::GetPropertyNamesArgs {
            mode: v8::KeyCollectionMode::IncludePrototypes,
            property_filter: v8::PropertyFilter::ONLY_ENUMERABLE | v8::PropertyFilter::SKIP_SYMBOLS,
            index_filter: v8::IndexFilter::IncludeIndices,
            key_conversion: v8::KeyConversionMode::ConvertToString,
        },
        result,
    )
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_all_property_names(
    env: napi_env,
    object: napi_value,
    key_mode: i32,
    key_filter: i32,
    key_conversion: i32,
    result: *mut napi_value,
) -> napi_status {
    let mut filter = v8::PropertyFilter::ALL_PROPERTIES;
    for (bit, f) in [
        (1, v8::PropertyFilter::ONLY_WRITABLE),
        (2, v8::PropertyFilter::ONLY_ENUMERABLE),
        (4, v8::PropertyFilter::ONLY_CONFIGURABLE),
        (8, v8::PropertyFilter::SKIP_STRINGS),
        (16, v8::PropertyFilter::SKIP_SYMBOLS),
    ] {
        if key_filter & bit != 0 {
            filter = filter | f;
        }
    }
    property_names(
        env!(env),
        object,
        v8::GetPropertyNamesArgs {
            mode: if key_mode == 1 {
                v8::KeyCollectionMode::OwnOnly
            } else {
                v8::KeyCollectionMode::IncludePrototypes
            },
            property_filter: filter,
            index_filter: v8::IndexFilter::IncludeIndices,
            key_conversion: if key_conversion == 1 {
                v8::KeyConversionMode::ConvertToString
            } else {
                v8::KeyConversionMode::KeepNumbers
            },
        },
        result,
    )
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_property(
    env: napi_env,
    object: napi_value,
    key: napi_value,
    value: napi_value,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    let (k, v) = (val!(env, key), val!(env, value));
    tc!(env, s);
    let o = object!(env, s, object);
    js!(env, s, o.set(s, k, v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_has_property(
    env: napi_env,
    object: napi_value,
    key: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let k = val!(env, key);
    tc!(env, s);
    let o = object!(env, s, object);
    out!(result, js!(env, s, o.has(s, k)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_property(
    env: napi_env,
    object: napi_value,
    key: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let k = val!(env, key);
    tc!(env, s);
    let o = object!(env, s, object);
    let v = js!(env, s, o.get(s, k));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_delete_property(
    env: napi_env,
    object: napi_value,
    key: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    let k = val!(env, key);
    tc!(env, s);
    let o = object!(env, s, object);
    let deleted = js!(env, s, o.delete(s, k));
    if !result.is_null() {
        *result = deleted;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_has_own_property(
    env: napi_env,
    object: napi_value,
    key: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let k = cast!(env, key, v8::Name, NAME_EXPECTED);
    tc!(env, s);
    let o = object!(env, s, object);
    out!(result, js!(env, s, o.has_own_property(s, k)));
    env.status(OK)
}

fn named_key<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: *const c_char,
) -> Option<v8::Local<'s, v8::Value>> {
    if name.is_null() {
        return None;
    }
    let b = unsafe { CStr::from_ptr(name) }.to_bytes();
    v8::String::new_from_utf8(scope, b, v8::NewStringType::Normal).map(Into::into)
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_named_property(
    env: napi_env,
    object: napi_value,
    utf8name: *const c_char,
    value: napi_value,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    let v = val!(env, value);
    tc!(env, s);
    let o = object!(env, s, object);
    let Some(k) = named_key(s, utf8name) else {
        return env.status(INVALID_ARG);
    };
    js!(env, s, o.set(s, k, v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_has_named_property(
    env: napi_env,
    object: napi_value,
    utf8name: *const c_char,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let Some(k) = named_key(s, utf8name) else {
        return env.status(INVALID_ARG);
    };
    out!(result, js!(env, s, o.has(s, k)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_named_property(
    env: napi_env,
    object: napi_value,
    utf8name: *const c_char,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let Some(k) = named_key(s, utf8name) else {
        return env.status(INVALID_ARG);
    };
    let v = js!(env, s, o.get(s, k));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_element(
    env: napi_env,
    object: napi_value,
    index: u32,
    value: napi_value,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    let v = val!(env, value);
    tc!(env, s);
    let o = object!(env, s, object);
    js!(env, s, o.set_index(s, index, v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_has_element(
    env: napi_env,
    object: napi_value,
    index: u32,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let k = v8::Integer::new_from_unsigned(s, index);
    out!(result, js!(env, s, o.has(s, k.into())));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_element(
    env: napi_env,
    object: napi_value,
    index: u32,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let v = js!(env, s, o.get_index(s, index));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_delete_element(
    env: napi_env,
    object: napi_value,
    index: u32,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    let k = v8::Integer::new_from_unsigned(s, index);
    let deleted = js!(env, s, o.delete(s, k.into()));
    if !result.is_null() {
        *result = deleted;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_define_properties(
    env: napi_env,
    object: napi_value,
    count: usize,
    properties: *const napi_property_descriptor,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    if count > 0 {
        arg!(env, properties);
    }
    tc!(env, s);
    let o = object!(env, s, object);
    for p in std::slice::from_raw_parts(properties, count) {
        if property_name(s, p).is_none() {
            return env.status(NAME_EXPECTED);
        }
        js!(env, s, define_property(env, s, o, p));
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_array(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_array());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_array_length(
    env: napi_env,
    value: napi_value,
    result: *mut u32,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let a = cast!(env, value, v8::Array, ARRAY_EXPECTED);
    out!(result, a.length());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_strict_equals(
    env: napi_env,
    lhs: napi_value,
    rhs: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, lhs).strict_equals(val!(env, rhs)));
    env.status(OK)
}

unsafe fn argv_of<'s>(
    argc: usize,
    argv: *const napi_value,
) -> Option<Vec<v8::Local<'s, v8::Value>>> {
    if argc > 0 && argv.is_null() {
        return None;
    }
    (0..argc).map(|i| local(*argv.add(i))).collect()
}

#[no_mangle]
pub unsafe extern "C" fn napi_call_function(
    env: napi_env,
    recv: napi_value,
    func: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    preamble!(env);
    let f = cast!(env, func, v8::Function, FUNCTION_EXPECTED);
    let Some(args) = argv_of(argc, argv) else {
        return env.status(INVALID_ARG);
    };
    tc!(env, s);
    let recv = local(recv).unwrap_or_else(|| v8::undefined(s).into());
    let v = js!(env, s, f.call(s, recv, &args));
    if !result.is_null() {
        *result = napi(v);
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_new_instance(
    env: napi_env,
    constructor: napi_value,
    argc: usize,
    argv: *const napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let f = cast!(env, constructor, v8::Function, FUNCTION_EXPECTED);
    let Some(args) = argv_of(argc, argv) else {
        return env.status(INVALID_ARG);
    };
    tc!(env, s);
    let v = js!(env, s, f.new_instance(s, &args));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_instanceof(
    env: napi_env,
    object: napi_value,
    constructor: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let v = val!(env, object);
    let f = cast!(env, constructor, v8::Function, FUNCTION_EXPECTED);
    tc!(env, s);
    out!(result, js!(env, s, v.instance_of(s, f.into())));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_cb_info(
    env: napi_env,
    cbinfo: napi_callback_info,
    argc: *mut usize,
    argv: *mut napi_value,
    this_arg: *mut napi_value,
    data: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, cbinfo);
    let args = args_of(cbinfo);
    scope!(env, s);
    if !argv.is_null() {
        arg!(env, argc);
        let undefined = napi(v8::undefined(s));
        for i in 0..*argc {
            *argv.add(i) = if (i as i32) < args.length() {
                napi(args.get(i as i32))
            } else {
                undefined
            };
        }
    }
    if !argc.is_null() {
        *argc = args.length() as usize;
    }
    if !this_arg.is_null() {
        *this_arg = napi(args.this());
    }
    if !data.is_null() {
        *data = (*cbinfo).data;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_new_target(
    env: napi_env,
    cbinfo: napi_callback_info,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, cbinfo, result);
    let target = args_of(cbinfo).new_target();
    out!(
        result,
        if target.is_undefined() {
            std::ptr::null_mut()
        } else {
            napi(target)
        }
    );
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_define_class(
    env: napi_env,
    utf8name: *const c_char,
    length: usize,
    constructor: napi_callback,
    data: *mut c_void,
    property_count: usize,
    properties: *const napi_property_descriptor,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    if constructor.is_none() || (property_count > 0 && properties.is_null()) {
        return env.status(INVALID_ARG);
    }
    tc!(env, s);
    let Some(tmpl) = template(env, s, constructor, data) else {
        return env.status(GENERIC_FAILURE);
    };
    if let Some(name) = bytes(utf8name as *const u8, length, 0)
        .and_then(|b| v8::String::new_from_utf8(s, b, v8::NewStringType::Normal))
    {
        tmpl.set_class_name(name);
    }
    let props = std::slice::from_raw_parts(properties, property_count);
    let proto = tmpl.prototype_template(s);
    for p in props.iter().filter(|p| p.attributes & STATIC == 0) {
        let Some(name) = property_name(s, p) else {
            return env.status(NAME_EXPECTED);
        };
        if p.getter.is_some() || p.setter.is_some() {
            let getter = template(env, s, p.getter, p.data);
            let setter = template(env, s, p.setter, p.data);
            let mut attr = v8::PropertyAttribute::NONE;
            if p.attributes & 2 == 0 {
                attr = attr | v8::PropertyAttribute::DONT_ENUM;
            }
            if p.attributes & 4 == 0 {
                attr = attr | v8::PropertyAttribute::DONT_DELETE;
            }
            proto.set_accessor_property(name, getter, setter, attr);
        } else if p.method.is_some() {
            let Some(m) = template(env, s, p.method, p.data) else {
                return env.status(GENERIC_FAILURE);
            };
            proto.set_with_attr(name, m.into(), attributes(p.attributes));
        } else {
            let value = local(p.value).unwrap_or_else(|| v8::undefined(s).into());
            proto.set_with_attr(name, value.into(), attributes(p.attributes));
        }
    }
    let f = js!(env, s, tmpl.get_function(s));
    for p in props.iter().filter(|p| p.attributes & STATIC != 0) {
        js!(env, s, define_property(env, s, f.into(), p));
    }
    out!(result, napi(f));
    env.status(OK)
}

struct Wrap {
    native: *mut c_void,
    finalize: napi_finalize,
    hint: *mut c_void,
    removed: Cell<bool>,
}

unsafe extern "C" fn finalize_wrap(env: napi_env, data: *mut c_void, _: *mut c_void) {
    let wrap = Box::from_raw(data as *mut Wrap);
    if !wrap.removed.get() {
        if let Some(cb) = wrap.finalize {
            cb(env, wrap.native, wrap.hint);
        }
    }
}

fn wrap_of<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
) -> Option<*mut Wrap> {
    let key = private_key(scope, "napi.wrap");
    let v = o.get_private(scope, key)?;
    v8::Local::<v8::External>::try_from(v)
        .ok()
        .map(|e| e.value() as *mut Wrap)
}

#[no_mangle]
pub unsafe extern "C" fn napi_wrap(
    env: napi_env,
    js_object: napi_value,
    native: *mut c_void,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    let env = env!(env);
    scope!(env, s);
    let o = object!(env, s, js_object);
    if wrap_of(s, o).is_some() {
        return env.status(INVALID_ARG);
    }
    let wrap = Box::into_raw(Box::new(Wrap {
        native,
        finalize: finalize_cb,
        hint,
        removed: Cell::new(false),
    }));
    let key = private_key(s, "napi.wrap");
    let ext = v8::External::new(s, wrap as *mut c_void);
    o.set_private(s, key, ext.into());
    env.finalize_on_gc(
        o.into(),
        Some(finalize_wrap),
        wrap as *mut c_void,
        std::ptr::null_mut(),
    );
    if !result.is_null() {
        *result = Reference::create(env, o.into(), 0);
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_unwrap(
    env: napi_env,
    js_object: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let o = object!(env, s, js_object);
    match wrap_of(s, o) {
        Some(w) if !(*w).removed.get() => out!(result, (*w).native),
        _ => return env.status(INVALID_ARG),
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_remove_wrap(
    env: napi_env,
    js_object: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    scope!(env, s);
    let o = object!(env, s, js_object);
    let Some(w) = wrap_of(s, o).filter(|w| !(**w).removed.get()) else {
        return env.status(INVALID_ARG);
    };
    (*w).removed.set(true);
    if !result.is_null() {
        *result = (*w).native;
    }
    let key = private_key(s, "napi.wrap");
    o.delete_private(s, key);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external(
    env: napi_env,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let ext = v8::External::new(s, data);
    env.finalize_on_gc(ext.into(), finalize_cb, data, hint);
    out!(result, napi(ext));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_external(
    env: napi_env,
    value: napi_value,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let e = cast!(env, value, v8::External, INVALID_ARG);
    out!(result, e.value());
    env.status(OK)
}

pub struct Reference {
    count: u32,
    strong: Option<v8::Global<v8::Value>>,
    weak: Option<v8::Weak<v8::Value>>,
}
pub type napi_ref = *mut Reference;

impl Reference {
    fn create(env: &mut Env, value: v8::Local<v8::Value>, count: u32) -> napi_ref {
        let r = if count > 0 {
            Reference {
                count,
                strong: Some(v8::Global::new(env.isolate(), value)),
                weak: None,
            }
        } else {
            Reference {
                count,
                strong: None,
                weak: Some(v8::Weak::new(env.isolate(), value)),
            }
        };
        Box::into_raw(Box::new(r))
    }
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_reference(
    env: napi_env,
    value: napi_value,
    initial_refcount: u32,
    result: *mut napi_ref,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = val!(env, value);
    out!(result, Reference::create(env, v, initial_refcount));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_delete_reference(env: napi_env, r: napi_ref) -> napi_status {
    let env = env!(env);
    arg!(env, r);
    drop(Box::from_raw(r));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_reference_ref(
    env: napi_env,
    r: napi_ref,
    result: *mut u32,
) -> napi_status {
    let env = env!(env);
    arg!(env, r);
    let r = &mut *r;
    r.count += 1;
    if r.count == 1 {
        if let Some(weak) = r.weak.take() {
            r.strong = weak.to_global(env.isolate());
        }
    }
    if !result.is_null() {
        *result = r.count;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_reference_unref(
    env: napi_env,
    r: napi_ref,
    result: *mut u32,
) -> napi_status {
    let env = env!(env);
    arg!(env, r);
    let r = &mut *r;
    if r.count == 0 {
        return env.status(GENERIC_FAILURE);
    }
    r.count -= 1;
    if r.count == 0 {
        if let Some(strong) = r.strong.take() {
            r.weak = Some(v8::Weak::new(env.isolate(), &strong));
        }
    }
    if !result.is_null() {
        *result = r.count;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_reference_value(
    env: napi_env,
    r: napi_ref,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, r, result);
    scope!(env, s);
    let r = &*r;
    let v = match (&r.strong, &r.weak) {
        (Some(g), _) => Some(v8::Local::new(s, g)),
        (None, Some(w)) => w.to_local(s),
        _ => None,
    };
    out!(result, v.map(napi).unwrap_or(std::ptr::null_mut()));
    env.status(OK)
}

// Handles live in the enclosing callback's scope; these are bookkeeping only.
#[no_mangle]
pub unsafe extern "C" fn napi_open_handle_scope(
    env: napi_env,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, 1 as *mut c_void);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_close_handle_scope(env: napi_env, scope: *mut c_void) -> napi_status {
    let env = env!(env);
    arg!(env, scope);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_open_escapable_handle_scope(
    env: napi_env,
    result: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, Box::into_raw(Box::new(false)) as *mut c_void);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_close_escapable_handle_scope(
    env: napi_env,
    scope: *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, scope);
    drop(Box::from_raw(scope as *mut bool));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_escape_handle(
    env: napi_env,
    scope: *mut c_void,
    escapee: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, scope, escapee, result);
    let escaped = &mut *(scope as *mut bool);
    if *escaped {
        return env.status(ESCAPE_CALLED_TWICE);
    }
    *escaped = true;
    out!(result, escapee);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_throw(env: napi_env, error: napi_value) -> napi_status {
    let env = env!(env);
    let e = val!(env, error);
    env.exception = Some(v8::Global::new(env.isolate(), e));
    env.status(OK)
}

fn throw(env: &mut Env, code: *const c_char, msg: *const c_char, kind: u8) -> napi_status {
    scope!(env, s);
    let text = if msg.is_null() {
        "".into()
    } else {
        unsafe { CStr::from_ptr(msg) }.to_string_lossy()
    };
    let msg = new_string(s, &text);
    let error = match kind {
        1 => v8::Exception::type_error(s, msg),
        2 => v8::Exception::range_error(s, msg),
        3 => v8::Exception::syntax_error(s, msg),
        _ => v8::Exception::error(s, msg),
    };
    if !code.is_null() {
        let code = new_string(s, &unsafe { CStr::from_ptr(code) }.to_string_lossy());
        let key = new_string(s, "code");
        error.to_object(s).unwrap().set(s, key.into(), code.into());
    }
    env.exception = Some(v8::Global::new(s, error));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_throw_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    throw(env!(env), code, msg, 0)
}

#[no_mangle]
pub unsafe extern "C" fn napi_throw_type_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    throw(env!(env), code, msg, 1)
}

#[no_mangle]
pub unsafe extern "C" fn napi_throw_range_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    throw(env!(env), code, msg, 2)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_throw_syntax_error(
    env: napi_env,
    code: *const c_char,
    msg: *const c_char,
) -> napi_status {
    throw(env!(env), code, msg, 3)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_error(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_native_error());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_exception_pending(
    env: napi_env,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, env.exception.is_some());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_and_clear_last_exception(
    env: napi_env,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let v = match env.exception.take() {
        Some(e) => v8::Local::new(s, e),
        None => v8::undefined(s).into(),
    };
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_arraybuffer(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_array_buffer());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_arraybuffer(
    env: napi_env,
    byte_length: usize,
    data: *mut *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let ab = v8::ArrayBuffer::new(s, byte_length);
    if !data.is_null() {
        *data = ab
            .data()
            .map(|p| p.as_ptr())
            .unwrap_or(std::ptr::null_mut());
    }
    out!(result, napi(ab));
    env.status(OK)
}

pub(crate) struct ExternalFinalize {
    pub env: usize,
    pub cb: napi_finalize,
    pub hint: *mut c_void,
}

/// Backing store deleter: may run on any thread, so it only queues.
pub(crate) unsafe extern "C" fn external_deleter(
    data: *mut c_void,
    _len: usize,
    deleter_data: *mut c_void,
) {
    let f = Box::from_raw(deleter_data as *mut ExternalFinalize);
    if let Some(cb) = f.cb {
        super::queue_external_finalizer(f.env, cb, data, f.hint);
    }
}

pub(crate) fn external_arraybuffer<'s>(
    env: &mut Env,
    scope: &mut v8::PinScope<'s, '_>,
    data: *mut c_void,
    len: usize,
    cb: napi_finalize,
    hint: *mut c_void,
) -> v8::Local<'s, v8::ArrayBuffer> {
    let finalize = Box::into_raw(Box::new(ExternalFinalize {
        env: env as *mut Env as usize,
        cb,
        hint,
    }));
    let store = unsafe {
        v8::ArrayBuffer::new_backing_store_from_ptr(
            data,
            len,
            external_deleter,
            finalize as *mut c_void,
        )
    }
    .make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &store)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external_arraybuffer(
    env: napi_env,
    data: *mut c_void,
    byte_length: usize,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    let ab = external_arraybuffer(env, s, data, byte_length, finalize_cb, hint);
    out!(result, napi(ab));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_arraybuffer_info(
    env: napi_env,
    arraybuffer: napi_value,
    data: *mut *mut c_void,
    byte_length: *mut usize,
) -> napi_status {
    let env = env!(env);
    let ab = cast!(env, arraybuffer, v8::ArrayBuffer, ARRAYBUFFER_EXPECTED);
    if !data.is_null() {
        *data = ab
            .data()
            .map(|p| p.as_ptr())
            .unwrap_or(std::ptr::null_mut());
    }
    if !byte_length.is_null() {
        *byte_length = ab.byte_length();
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_detach_arraybuffer(
    env: napi_env,
    arraybuffer: napi_value,
) -> napi_status {
    let env = env!(env);
    let ab = cast!(env, arraybuffer, v8::ArrayBuffer, ARRAYBUFFER_EXPECTED);
    if !ab.is_detachable() {
        return env.status(DETACHABLE_ARRAYBUFFER_EXPECTED);
    }
    ab.detach(None);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_detached_arraybuffer(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let v = val!(env, value);
    out!(
        result,
        v8::Local::<v8::ArrayBuffer>::try_from(v)
            .map(|ab| ab.was_detached())
            .unwrap_or(false)
    );
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_typedarray(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_typed_array());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_typedarray(
    env: napi_env,
    kind: i32,
    length: usize,
    arraybuffer: napi_value,
    byte_offset: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let ab = cast!(env, arraybuffer, v8::ArrayBuffer, INVALID_ARG);
    let size = match kind {
        0..=2 => 1,
        3 | 4 => 2,
        5..=7 => 4,
        8..=10 => 8,
        _ => return env.status(INVALID_ARG),
    };
    tc!(env, s);
    if byte_offset % size != 0 || byte_offset + length * size > ab.byte_length() {
        let msg = new_string(
            s,
            if byte_offset % size != 0 {
                "start offset of TypedArray should be a multiple of the element size"
            } else {
                "Invalid typed array length"
            },
        );
        let e = v8::Exception::range_error(s, msg);
        env.exception = Some(v8::Global::new(s, e));
        return env.status(PENDING_EXCEPTION);
    }
    let v: Option<v8::Local<v8::Value>> = match kind {
        0 => v8::Int8Array::new(s, ab, byte_offset, length).map(Into::into),
        1 => v8::Uint8Array::new(s, ab, byte_offset, length).map(Into::into),
        2 => v8::Uint8ClampedArray::new(s, ab, byte_offset, length).map(Into::into),
        3 => v8::Int16Array::new(s, ab, byte_offset, length).map(Into::into),
        4 => v8::Uint16Array::new(s, ab, byte_offset, length).map(Into::into),
        5 => v8::Int32Array::new(s, ab, byte_offset, length).map(Into::into),
        6 => v8::Uint32Array::new(s, ab, byte_offset, length).map(Into::into),
        7 => v8::Float32Array::new(s, ab, byte_offset, length).map(Into::into),
        8 => v8::Float64Array::new(s, ab, byte_offset, length).map(Into::into),
        9 => v8::BigInt64Array::new(s, ab, byte_offset, length).map(Into::into),
        _ => v8::BigUint64Array::new(s, ab, byte_offset, length).map(Into::into),
    };
    out!(result, napi(js!(env, s, v)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_typedarray_info(
    env: napi_env,
    typedarray: napi_value,
    kind: *mut i32,
    length: *mut usize,
    data: *mut *mut c_void,
    arraybuffer: *mut napi_value,
    byte_offset: *mut usize,
) -> napi_status {
    let env = env!(env);
    let v = val!(env, typedarray);
    let ta = cast!(env, typedarray, v8::TypedArray, INVALID_ARG);
    scope!(env, s);
    if !kind.is_null() {
        *kind = if v.is_int8_array() {
            0
        } else if v.is_uint8_array() {
            1
        } else if v.is_uint8_clamped_array() {
            2
        } else if v.is_int16_array() {
            3
        } else if v.is_uint16_array() {
            4
        } else if v.is_int32_array() {
            5
        } else if v.is_uint32_array() {
            6
        } else if v.is_float32_array() {
            7
        } else if v.is_float64_array() {
            8
        } else if v.is_big_int64_array() {
            9
        } else {
            10
        };
    }
    if !length.is_null() {
        *length = ta.length();
    }
    let view: v8::Local<v8::ArrayBufferView> = ta.into();
    if !data.is_null() {
        *data = view.data();
    }
    if !arraybuffer.is_null() {
        *arraybuffer = view.buffer(s).map(napi).unwrap_or(std::ptr::null_mut());
    }
    if !byte_offset.is_null() {
        *byte_offset = view.byte_offset();
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_dataview(
    env: napi_env,
    length: usize,
    arraybuffer: napi_value,
    byte_offset: usize,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let ab = cast!(env, arraybuffer, v8::ArrayBuffer, INVALID_ARG);
    scope!(env, s);
    if byte_offset + length > ab.byte_length() {
        let msg = new_string(s, "byte_offset + byte_length should be less than or equal to the size in bytes of the array passed in");
        let e = v8::Exception::range_error(s, msg);
        env.exception = Some(v8::Global::new(s, e));
        return env.status(PENDING_EXCEPTION);
    }
    out!(result, napi(v8::DataView::new(s, ab, byte_offset, length)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_dataview(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_data_view());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_dataview_info(
    env: napi_env,
    dataview: napi_value,
    byte_length: *mut usize,
    data: *mut *mut c_void,
    arraybuffer: *mut napi_value,
    byte_offset: *mut usize,
) -> napi_status {
    let env = env!(env);
    let view = cast!(env, dataview, v8::DataView, INVALID_ARG);
    let view: v8::Local<v8::ArrayBufferView> = view.into();
    scope!(env, s);
    if !byte_length.is_null() {
        *byte_length = view.byte_length();
    }
    if !data.is_null() {
        *data = view.data();
    }
    if !arraybuffer.is_null() {
        *arraybuffer = view.buffer(s).map(napi).unwrap_or(std::ptr::null_mut());
    }
    if !byte_offset.is_null() {
        *byte_offset = view.byte_offset();
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_version(env: napi_env, result: *mut u32) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, 9);
    env.status(OK)
}

pub type napi_deferred = *mut v8::Global<v8::PromiseResolver>;

#[no_mangle]
pub unsafe extern "C" fn napi_create_promise(
    env: napi_env,
    deferred: *mut napi_deferred,
    promise: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, deferred, promise);
    scope!(env, s);
    let Some(resolver) = v8::PromiseResolver::new(s) else {
        return env.status(GENERIC_FAILURE);
    };
    out!(promise, napi(resolver.get_promise(s)));
    out!(
        deferred,
        Box::into_raw(Box::new(v8::Global::new(s, resolver)))
    );
    env.status(OK)
}

unsafe fn settle(
    env: &mut Env,
    deferred: napi_deferred,
    value: napi_value,
    resolve: bool,
) -> napi_status {
    arg!(env, deferred);
    let v = val!(env, value);
    let resolver = Box::from_raw(deferred);
    tc!(env, s);
    let resolver = v8::Local::new(s, *resolver);
    let done = if resolve {
        resolver.resolve(s, v)
    } else {
        resolver.reject(s, v)
    };
    js!(env, s, done);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_resolve_deferred(
    env: napi_env,
    deferred: napi_deferred,
    resolution: napi_value,
) -> napi_status {
    settle(env!(env), deferred, resolution, true)
}

#[no_mangle]
pub unsafe extern "C" fn napi_reject_deferred(
    env: napi_env,
    deferred: napi_deferred,
    rejection: napi_value,
) -> napi_status {
    settle(env!(env), deferred, rejection, false)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_promise(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_promise());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_run_script(
    env: napi_env,
    script: napi_value,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    let source = cast!(env, script, v8::String, STRING_EXPECTED);
    tc!(env, s);
    let compiled = js!(env, s, v8::Script::compile(s, source, None));
    let v = js!(env, s, compiled.run(s));
    out!(result, napi(v));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_adjust_external_memory(
    env: napi_env,
    change: i64,
    adjusted: *mut i64,
) -> napi_status {
    let env = env!(env);
    let v = env
        .isolate()
        .adjust_amount_of_external_allocated_memory(change);
    if !adjusted.is_null() {
        *adjusted = v;
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_date(
    env: napi_env,
    time: f64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    tc!(env, s);
    let d = js!(env, s, v8::Date::new(s, time));
    out!(result, napi(d));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_date(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    out!(result, val!(env, value).is_date());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_date_value(
    env: napi_env,
    value: napi_value,
    result: *mut f64,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    let d = cast!(env, value, v8::Date, DATE_EXPECTED);
    out!(result, d.value_of());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_add_finalizer(
    env: napi_env,
    js_object: napi_value,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
    result: *mut napi_ref,
) -> napi_status {
    let env = env!(env);
    if finalize_cb.is_none() {
        return env.status(INVALID_ARG);
    }
    scope!(env, s);
    let o = object!(env, s, js_object);
    env.finalize_on_gc(o.into(), finalize_cb, data, hint);
    if !result.is_null() {
        *result = Reference::create(env, o.into(), 0);
    }
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_post_finalizer(
    env: napi_env,
    finalize_cb: napi_finalize,
    data: *mut c_void,
    hint: *mut c_void,
) -> napi_status {
    let env = env!(env);
    let Some(cb) = finalize_cb else {
        return env.status(INVALID_ARG);
    };
    env.finalizers
        .borrow_mut()
        .push(PendingFinalize { cb, data, hint });
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_bigint_int64(
    env: napi_env,
    value: i64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::BigInt::new_from_i64(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_bigint_uint64(
    env: napi_env,
    value: u64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    scope!(env, s);
    out!(result, napi(v8::BigInt::new_from_u64(s, value)));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_bigint_words(
    env: napi_env,
    sign_bit: i32,
    word_count: usize,
    words: *const u64,
    result: *mut napi_value,
) -> napi_status {
    let env = env!(env);
    arg!(env, result);
    preamble!(env);
    if word_count > i32::MAX as usize || (word_count > 0 && words.is_null()) {
        return env.status(INVALID_ARG);
    }
    let words = if word_count == 0 {
        &[][..]
    } else {
        std::slice::from_raw_parts(words, word_count)
    };
    tc!(env, s);
    let b = js!(env, s, v8::BigInt::new_from_words(s, sign_bit != 0, words));
    out!(result, napi(b));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_bigint_int64(
    env: napi_env,
    value: napi_value,
    result: *mut i64,
    lossless: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result, lossless);
    let b = cast!(env, value, v8::BigInt, BIGINT_EXPECTED);
    let (v, l) = b.i64_value();
    out!(result, v);
    out!(lossless, l);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_bigint_uint64(
    env: napi_env,
    value: napi_value,
    result: *mut u64,
    lossless: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, result, lossless);
    let b = cast!(env, value, v8::BigInt, BIGINT_EXPECTED);
    let (v, l) = b.u64_value();
    out!(result, v);
    out!(lossless, l);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_value_bigint_words(
    env: napi_env,
    value: napi_value,
    sign_bit: *mut i32,
    word_count: *mut usize,
    words: *mut u64,
) -> napi_status {
    let env = env!(env);
    arg!(env, word_count);
    let b = cast!(env, value, v8::BigInt, BIGINT_EXPECTED);
    if words.is_null() {
        *word_count = b.word_count();
        return env.status(OK);
    }
    arg!(env, sign_bit);
    let buf = std::slice::from_raw_parts_mut(words, *word_count);
    let (sign, written) = b.to_words_array(buf);
    *sign_bit = sign as i32;
    *word_count = written.len();
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_set_instance_data(
    env: napi_env,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    hint: *mut c_void,
) -> napi_status {
    let env = env!(env);
    env.instance_data = Some((data, finalize_cb, hint));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_instance_data(
    env: napi_env,
    data: *mut *mut c_void,
) -> napi_status {
    let env = env!(env);
    arg!(env, data);
    out!(
        data,
        env.instance_data
            .map(|d| d.0)
            .unwrap_or(std::ptr::null_mut())
    );
    env.status(OK)
}

#[repr(C)]
pub struct napi_type_tag {
    lower: u64,
    upper: u64,
}

#[no_mangle]
pub unsafe extern "C" fn napi_type_tag_object(
    env: napi_env,
    value: napi_value,
    tag: *const napi_type_tag,
) -> napi_status {
    let env = env!(env);
    arg!(env, tag);
    scope!(env, s);
    let o = object!(env, s, value);
    let key = private_key(s, "napi.typetag");
    if o.get_private(s, key).is_some_and(|v| !v.is_undefined()) {
        return env.status(INVALID_ARG);
    }
    let Some(b) = v8::BigInt::new_from_words(s, false, &[(*tag).lower, (*tag).upper]) else {
        return env.status(GENERIC_FAILURE);
    };
    o.set_private(s, key, b.into());
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_check_object_type_tag(
    env: napi_env,
    value: napi_value,
    tag: *const napi_type_tag,
    result: *mut bool,
) -> napi_status {
    let env = env!(env);
    arg!(env, tag, result);
    scope!(env, s);
    let o = object!(env, s, value);
    let key = private_key(s, "napi.typetag");
    let matches = o
        .get_private(s, key)
        .and_then(|v| v8::Local::<v8::BigInt>::try_from(v).ok())
        .map(|b| {
            let mut words = [0u64; 2];
            let (_, w) = b.to_words_array(&mut words);
            let w = w.to_vec();
            w.first().copied().unwrap_or(0) == (*tag).lower
                && w.get(1).copied().unwrap_or(0) == (*tag).upper
        })
        .unwrap_or(false);
    out!(result, matches);
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_object_freeze(env: napi_env, object: napi_value) -> napi_status {
    let env = env!(env);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    js!(env, s, o.set_integrity_level(s, v8::IntegrityLevel::Frozen));
    env.status(OK)
}

#[no_mangle]
pub unsafe extern "C" fn napi_object_seal(env: napi_env, object: napi_value) -> napi_status {
    let env = env!(env);
    preamble!(env);
    tc!(env, s);
    let o = object!(env, s, object);
    js!(env, s, o.set_integrity_level(s, v8::IntegrityLevel::Sealed));
    env.status(OK)
}

/// Referenced so the linker keeps every export.
pub struct Symbols(pub &'static [*const ()]);
unsafe impl Sync for Symbols {}
#[used]
pub static SYMBOLS: Symbols = Symbols(&[
    napi_get_last_error_info as *const (),
    napi_get_undefined as *const (),
    napi_get_null as *const (),
    napi_get_global as *const (),
    napi_get_boolean as *const (),
    napi_create_object as *const (),
    napi_create_array as *const (),
    napi_create_array_with_length as *const (),
    napi_create_double as *const (),
    napi_create_int32 as *const (),
    napi_create_uint32 as *const (),
    napi_create_int64 as *const (),
    napi_create_string_latin1 as *const (),
    napi_create_string_utf8 as *const (),
    napi_create_string_utf16 as *const (),
    node_api_create_external_string_latin1 as *const (),
    node_api_create_external_string_utf16 as *const (),
    node_api_create_property_key_utf16 as *const (),
    napi_create_symbol as *const (),
    node_api_symbol_for as *const (),
    napi_create_function as *const (),
    napi_create_error as *const (),
    napi_create_type_error as *const (),
    napi_create_range_error as *const (),
    node_api_create_syntax_error as *const (),
    napi_typeof as *const (),
    napi_get_value_double as *const (),
    napi_get_value_int32 as *const (),
    napi_get_value_uint32 as *const (),
    napi_get_value_int64 as *const (),
    napi_get_value_bool as *const (),
    napi_get_value_string_latin1 as *const (),
    napi_get_value_string_utf8 as *const (),
    napi_get_value_string_utf16 as *const (),
    napi_coerce_to_bool as *const (),
    napi_coerce_to_number as *const (),
    napi_coerce_to_object as *const (),
    napi_coerce_to_string as *const (),
    napi_get_prototype as *const (),
    napi_get_property_names as *const (),
    napi_get_all_property_names as *const (),
    napi_set_property as *const (),
    napi_has_property as *const (),
    napi_get_property as *const (),
    napi_delete_property as *const (),
    napi_has_own_property as *const (),
    napi_set_named_property as *const (),
    napi_has_named_property as *const (),
    napi_get_named_property as *const (),
    napi_set_element as *const (),
    napi_has_element as *const (),
    napi_get_element as *const (),
    napi_delete_element as *const (),
    napi_define_properties as *const (),
    napi_is_array as *const (),
    napi_get_array_length as *const (),
    napi_strict_equals as *const (),
    napi_call_function as *const (),
    napi_new_instance as *const (),
    napi_instanceof as *const (),
    napi_get_cb_info as *const (),
    napi_get_new_target as *const (),
    napi_define_class as *const (),
    napi_wrap as *const (),
    napi_unwrap as *const (),
    napi_remove_wrap as *const (),
    napi_create_external as *const (),
    napi_get_value_external as *const (),
    napi_create_reference as *const (),
    napi_delete_reference as *const (),
    napi_reference_ref as *const (),
    napi_reference_unref as *const (),
    napi_get_reference_value as *const (),
    napi_open_handle_scope as *const (),
    napi_close_handle_scope as *const (),
    napi_open_escapable_handle_scope as *const (),
    napi_close_escapable_handle_scope as *const (),
    napi_escape_handle as *const (),
    napi_throw as *const (),
    napi_throw_error as *const (),
    napi_throw_type_error as *const (),
    napi_throw_range_error as *const (),
    node_api_throw_syntax_error as *const (),
    napi_is_error as *const (),
    napi_is_exception_pending as *const (),
    napi_get_and_clear_last_exception as *const (),
    napi_is_arraybuffer as *const (),
    napi_create_arraybuffer as *const (),
    napi_create_external_arraybuffer as *const (),
    napi_get_arraybuffer_info as *const (),
    napi_detach_arraybuffer as *const (),
    napi_is_detached_arraybuffer as *const (),
    napi_is_typedarray as *const (),
    napi_create_typedarray as *const (),
    napi_get_typedarray_info as *const (),
    napi_create_dataview as *const (),
    napi_is_dataview as *const (),
    napi_get_dataview_info as *const (),
    napi_get_version as *const (),
    napi_create_promise as *const (),
    napi_resolve_deferred as *const (),
    napi_reject_deferred as *const (),
    napi_is_promise as *const (),
    napi_run_script as *const (),
    napi_adjust_external_memory as *const (),
    napi_create_date as *const (),
    napi_is_date as *const (),
    napi_get_date_value as *const (),
    napi_add_finalizer as *const (),
    node_api_post_finalizer as *const (),
    napi_create_bigint_int64 as *const (),
    napi_create_bigint_uint64 as *const (),
    napi_create_bigint_words as *const (),
    napi_get_value_bigint_int64 as *const (),
    napi_get_value_bigint_uint64 as *const (),
    napi_get_value_bigint_words as *const (),
    napi_set_instance_data as *const (),
    napi_get_instance_data as *const (),
    napi_type_tag_object as *const (),
    napi_check_object_type_tag as *const (),
    napi_object_freeze as *const (),
    napi_object_seal as *const (),
]);
