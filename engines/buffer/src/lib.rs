//! node_api.h Buffer functions over typed arrays, for engines whose shim
//! lacks them. Buffers are plain Uint8Arrays.

use nativescript_napi::sys::*;
use std::ffi::c_void;
use std::ptr::null_mut;

const OK: napi_status = 0;
const INVALID_ARG: napi_status = 1;
const UINT8_ARRAY: i32 = 1;

unsafe fn uint8_array(
    env: napi_env,
    buffer: napi_value,
    length: usize,
    result: *mut napi_value,
) -> napi_status {
    napi_create_typedarray(env, UINT8_ARRAY, length, buffer, 0, result)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer(
    env: napi_env,
    length: usize,
    data: *mut *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    if env.is_null() || result.is_null() {
        return INVALID_ARG;
    }
    let (mut ab, mut ptr) = (null_mut(), null_mut());
    let status = napi_create_arraybuffer(env, length, &mut ptr, &mut ab);
    if status != OK {
        return status;
    }
    if !data.is_null() {
        *data = ptr;
    }
    uint8_array(env, ab, length, result)
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_buffer_copy(
    env: napi_env,
    length: usize,
    source: *const c_void,
    data: *mut *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    let mut ptr = null_mut();
    let status = napi_create_buffer(env, length, &mut ptr, result);
    if status == OK && length > 0 {
        std::ptr::copy_nonoverlapping(source as *const u8, ptr as *mut u8, length);
    }
    if !data.is_null() {
        *data = ptr;
    }
    status
}

#[no_mangle]
pub unsafe extern "C" fn napi_create_external_buffer(
    env: napi_env,
    length: usize,
    data: *mut c_void,
    finalize_cb: napi_finalize,
    finalize_hint: *mut c_void,
    result: *mut napi_value,
) -> napi_status {
    if env.is_null() || result.is_null() {
        return INVALID_ARG;
    }
    let mut ab = null_mut();
    let status =
        napi_create_external_arraybuffer(env, data, length, finalize_cb, finalize_hint, &mut ab);
    if status != OK {
        return status;
    }
    uint8_array(env, ab, length, result)
}

#[no_mangle]
pub unsafe extern "C" fn node_api_create_buffer_from_arraybuffer(
    env: napi_env,
    arraybuffer: napi_value,
    byte_offset: usize,
    byte_length: usize,
    result: *mut napi_value,
) -> napi_status {
    napi_create_typedarray(env, UINT8_ARRAY, byte_length, arraybuffer, byte_offset, result)
}

#[no_mangle]
pub unsafe extern "C" fn napi_is_buffer(
    env: napi_env,
    value: napi_value,
    result: *mut bool,
) -> napi_status {
    if env.is_null() || result.is_null() {
        return INVALID_ARG;
    }
    let (mut typed, mut view) = (false, false);
    napi_is_typedarray(env, value, &mut typed);
    if !typed {
        napi_is_dataview(env, value, &mut view);
    }
    *result = typed || view;
    OK
}

#[no_mangle]
pub unsafe extern "C" fn napi_get_buffer_info(
    env: napi_env,
    value: napi_value,
    data: *mut *mut c_void,
    length: *mut usize,
) -> napi_status {
    if env.is_null() {
        return INVALID_ARG;
    }
    let (mut ty, mut len, mut ptr, mut ab, mut offset) = (0, 0, null_mut(), null_mut(), 0);
    let mut typed = false;
    napi_is_typedarray(env, value, &mut typed);
    let status = if typed {
        let status = napi_get_typedarray_info(env, value, &mut ty, &mut len, &mut ptr, &mut ab, &mut offset);
        // element count to bytes
        len *= match ty {
            0..=2 => 1,
            3 | 4 => 2,
            5..=7 => 4,
            _ => 8,
        };
        status
    } else {
        napi_get_dataview_info(env, value, &mut len, &mut ptr, &mut ab, &mut offset)
    };
    if !data.is_null() {
        *data = ptr;
    }
    if !length.is_null() {
        *length = len;
    }
    status
}

struct Symbols(#[allow(dead_code)] &'static [*const ()]);
unsafe impl Sync for Symbols {}

#[used]
static SYMBOLS: Symbols = Symbols(&[
    napi_create_buffer as *const (),
    napi_create_buffer_copy as *const (),
    napi_create_external_buffer as *const (),
    node_api_create_buffer_from_arraybuffer as *const (),
    napi_is_buffer as *const (),
    napi_get_buffer_info as *const (),
]);

/// Called by engine binaries so the exports above are linked in.
pub fn keep() {
    std::hint::black_box(&SYMBOLS);
}
