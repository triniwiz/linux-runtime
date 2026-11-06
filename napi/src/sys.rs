//! Raw js_native_api bindings (generated from the Node.js headers) and the jsr host API.
#![allow(non_camel_case_types, dead_code, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void};

pub type napi_callback_info = *mut c_void;
pub type napi_deferred = *mut c_void;
pub type napi_env = *mut c_void;
pub type napi_escapable_handle_scope = *mut c_void;
pub type napi_handle_scope = *mut c_void;
pub type napi_ref = *mut c_void;
pub type napi_value = *mut c_void;
pub type napi_status = i32;
pub type napi_runtime = *mut c_void;
pub type napi_callback = Option<unsafe extern "C" fn(napi_env, napi_callback_info) -> napi_value>;
pub type napi_finalize = Option<unsafe extern "C" fn(napi_env, *mut c_void, *mut c_void)>;

#[repr(C)]
pub struct napi_property_descriptor {
    pub utf8name: *const c_char,
    pub name: napi_value,
    pub method: napi_callback,
    pub getter: napi_callback,
    pub setter: napi_callback,
    pub value: napi_value,
    pub attributes: i32,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct napi_type_tag {
    pub lower: u64,
    pub upper: u64,
}

extern "C" {
    pub fn napi_get_last_error_info(env: napi_env, result: *mut *const c_void) -> napi_status;
    pub fn napi_get_undefined(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_get_null(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_get_global(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_get_boolean(env: napi_env, value: bool, result: *mut napi_value) -> napi_status;
    pub fn napi_create_object(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_create_array(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_create_array_with_length(env: napi_env, length: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_create_double(env: napi_env, value: f64, result: *mut napi_value) -> napi_status;
    pub fn napi_create_int32(env: napi_env, value: i32, result: *mut napi_value) -> napi_status;
    pub fn napi_create_uint32(env: napi_env, value: u32, result: *mut napi_value) -> napi_status;
    pub fn napi_create_int64(env: napi_env, value: i64, result: *mut napi_value) -> napi_status;
    pub fn napi_create_string_latin1(env: napi_env, str: *const c_char, length: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_create_string_utf8(env: napi_env, str: *const c_char, length: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_create_string_utf16(env: napi_env, str: *const u16, length: usize, result: *mut napi_value) -> napi_status;
    pub fn node_api_create_external_string_latin1(env: napi_env, str: *mut c_char, length: usize, finalize_callback: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_value, copied: *mut bool) -> napi_status;
    pub fn node_api_create_external_string_utf16(env: napi_env, str: *mut u16, length: usize, finalize_callback: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_value, copied: *mut bool) -> napi_status;
    pub fn node_api_create_property_key_utf16(env: napi_env, str: *const u16, length: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_create_symbol(env: napi_env, description: napi_value, result: *mut napi_value) -> napi_status;
    pub fn node_api_symbol_for(env: napi_env, utf8description: *const c_char, length: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_create_function(env: napi_env, utf8name: *const c_char, length: usize, cb: napi_callback, data: *mut c_void, result: *mut napi_value) -> napi_status;
    pub fn napi_create_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_create_type_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_create_range_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status;
    pub fn node_api_create_syntax_error(env: napi_env, code: napi_value, msg: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_typeof(env: napi_env, value: napi_value, result: *mut i32) -> napi_status;
    pub fn napi_get_value_double(env: napi_env, value: napi_value, result: *mut f64) -> napi_status;
    pub fn napi_get_value_int32(env: napi_env, value: napi_value, result: *mut i32) -> napi_status;
    pub fn napi_get_value_uint32(env: napi_env, value: napi_value, result: *mut u32) -> napi_status;
    pub fn napi_get_value_int64(env: napi_env, value: napi_value, result: *mut i64) -> napi_status;
    pub fn napi_get_value_bool(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_get_value_string_latin1(env: napi_env, value: napi_value, buf: *mut c_char, bufsize: usize, result: *mut usize) -> napi_status;
    pub fn napi_get_value_string_utf8(env: napi_env, value: napi_value, buf: *mut c_char, bufsize: usize, result: *mut usize) -> napi_status;
    pub fn napi_get_value_string_utf16(env: napi_env, value: napi_value, buf: *mut u16, bufsize: usize, result: *mut usize) -> napi_status;
    pub fn napi_coerce_to_bool(env: napi_env, value: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_coerce_to_number(env: napi_env, value: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_coerce_to_object(env: napi_env, value: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_coerce_to_string(env: napi_env, value: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_get_prototype(env: napi_env, object: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_get_property_names(env: napi_env, object: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_set_property(env: napi_env, object: napi_value, key: napi_value, value: napi_value) -> napi_status;
    pub fn napi_has_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_get_property(env: napi_env, object: napi_value, key: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_delete_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_has_own_property(env: napi_env, object: napi_value, key: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_set_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, value: napi_value) -> napi_status;
    pub fn napi_has_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, result: *mut bool) -> napi_status;
    pub fn napi_get_named_property(env: napi_env, object: napi_value, utf8name: *const c_char, result: *mut napi_value) -> napi_status;
    pub fn napi_set_element(env: napi_env, object: napi_value, index: u32, value: napi_value) -> napi_status;
    pub fn napi_has_element(env: napi_env, object: napi_value, index: u32, result: *mut bool) -> napi_status;
    pub fn napi_get_element(env: napi_env, object: napi_value, index: u32, result: *mut napi_value) -> napi_status;
    pub fn napi_delete_element(env: napi_env, object: napi_value, index: u32, result: *mut bool) -> napi_status;
    pub fn napi_define_properties(env: napi_env, object: napi_value, property_count: usize, properties: *const napi_property_descriptor) -> napi_status;
    pub fn napi_is_array(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_get_array_length(env: napi_env, value: napi_value, result: *mut u32) -> napi_status;
    pub fn napi_strict_equals(env: napi_env, lhs: napi_value, rhs: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_call_function(env: napi_env, recv: napi_value, func: napi_value, argc: usize, argv: *const napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_new_instance(env: napi_env, constructor: napi_value, argc: usize, argv: *const napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_instanceof(env: napi_env, object: napi_value, constructor: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_get_cb_info(env: napi_env, cbinfo: napi_callback_info, argc: *mut usize, argv: *mut napi_value, this_arg: *mut napi_value, data: *mut *mut c_void) -> napi_status;
    pub fn napi_get_new_target(env: napi_env, cbinfo: napi_callback_info, result: *mut napi_value) -> napi_status;
    pub fn napi_define_class(env: napi_env, utf8name: *const c_char, length: usize, constructor: napi_callback, data: *mut c_void, property_count: usize, properties: *const napi_property_descriptor, result: *mut napi_value) -> napi_status;
    pub fn napi_wrap(env: napi_env, js_object: napi_value, native_object: *mut c_void, finalize_cb: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_ref) -> napi_status;
    pub fn napi_unwrap(env: napi_env, js_object: napi_value, result: *mut *mut c_void) -> napi_status;
    pub fn napi_remove_wrap(env: napi_env, js_object: napi_value, result: *mut *mut c_void) -> napi_status;
    pub fn napi_create_external(env: napi_env, data: *mut c_void, finalize_cb: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_value) -> napi_status;
    pub fn napi_get_value_external(env: napi_env, value: napi_value, result: *mut *mut c_void) -> napi_status;
    pub fn napi_create_reference(env: napi_env, value: napi_value, initial_refcount: u32, result: *mut napi_ref) -> napi_status;
    pub fn napi_delete_reference(env: napi_env, ref_: napi_ref) -> napi_status;
    pub fn napi_reference_ref(env: napi_env, ref_: napi_ref, result: *mut u32) -> napi_status;
    pub fn napi_reference_unref(env: napi_env, ref_: napi_ref, result: *mut u32) -> napi_status;
    pub fn napi_get_reference_value(env: napi_env, ref_: napi_ref, result: *mut napi_value) -> napi_status;
    pub fn napi_open_handle_scope(env: napi_env, result: *mut napi_handle_scope) -> napi_status;
    pub fn napi_close_handle_scope(env: napi_env, scope: napi_handle_scope) -> napi_status;
    pub fn napi_open_escapable_handle_scope(env: napi_env, result: *mut napi_escapable_handle_scope) -> napi_status;
    pub fn napi_close_escapable_handle_scope(env: napi_env, scope: napi_escapable_handle_scope) -> napi_status;
    pub fn napi_escape_handle(env: napi_env, scope: napi_escapable_handle_scope, escapee: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_throw(env: napi_env, error: napi_value) -> napi_status;
    pub fn napi_throw_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status;
    pub fn napi_throw_type_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status;
    pub fn napi_throw_range_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status;
    pub fn node_api_throw_syntax_error(env: napi_env, code: *const c_char, msg: *const c_char) -> napi_status;
    pub fn napi_is_error(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_is_exception_pending(env: napi_env, result: *mut bool) -> napi_status;
    pub fn napi_get_and_clear_last_exception(env: napi_env, result: *mut napi_value) -> napi_status;
    pub fn napi_is_arraybuffer(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_create_arraybuffer(env: napi_env, byte_length: usize, data: *mut *mut c_void, result: *mut napi_value) -> napi_status;
    pub fn napi_create_external_arraybuffer(env: napi_env, external_data: *mut c_void, byte_length: usize, finalize_cb: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_value) -> napi_status;
    pub fn napi_get_arraybuffer_info(env: napi_env, arraybuffer: napi_value, data: *mut *mut c_void, byte_length: *mut usize) -> napi_status;
    pub fn napi_is_typedarray(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_create_typedarray(env: napi_env, type_: i32, length: usize, arraybuffer: napi_value, byte_offset: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_get_typedarray_info(env: napi_env, typedarray: napi_value, type_: *mut i32, length: *mut usize, data: *mut *mut c_void, arraybuffer: *mut napi_value, byte_offset: *mut usize) -> napi_status;
    pub fn napi_create_dataview(env: napi_env, length: usize, arraybuffer: napi_value, byte_offset: usize, result: *mut napi_value) -> napi_status;
    pub fn napi_is_dataview(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_get_dataview_info(env: napi_env, dataview: napi_value, bytelength: *mut usize, data: *mut *mut c_void, arraybuffer: *mut napi_value, byte_offset: *mut usize) -> napi_status;
    pub fn napi_get_version(env: napi_env, result: *mut u32) -> napi_status;
    pub fn napi_create_promise(env: napi_env, deferred: *mut napi_deferred, promise: *mut napi_value) -> napi_status;
    pub fn napi_resolve_deferred(env: napi_env, deferred: napi_deferred, resolution: napi_value) -> napi_status;
    pub fn napi_reject_deferred(env: napi_env, deferred: napi_deferred, rejection: napi_value) -> napi_status;
    pub fn napi_is_promise(env: napi_env, value: napi_value, is_promise: *mut bool) -> napi_status;
    pub fn napi_run_script(env: napi_env, script: napi_value, result: *mut napi_value) -> napi_status;
    pub fn napi_adjust_external_memory(env: napi_env, change_in_bytes: i64, adjusted_value: *mut i64) -> napi_status;
    pub fn napi_create_date(env: napi_env, time: f64, result: *mut napi_value) -> napi_status;
    pub fn napi_is_date(env: napi_env, value: napi_value, is_date: *mut bool) -> napi_status;
    pub fn napi_get_date_value(env: napi_env, value: napi_value, result: *mut f64) -> napi_status;
    pub fn napi_add_finalizer(env: napi_env, js_object: napi_value, finalize_data: *mut c_void, finalize_cb: napi_finalize, finalize_hint: *mut c_void, result: *mut napi_ref) -> napi_status;
    pub fn node_api_post_finalizer(env: napi_env, finalize_cb: napi_finalize, finalize_data: *mut c_void, finalize_hint: *mut c_void) -> napi_status;
    pub fn napi_create_bigint_int64(env: napi_env, value: i64, result: *mut napi_value) -> napi_status;
    pub fn napi_create_bigint_uint64(env: napi_env, value: u64, result: *mut napi_value) -> napi_status;
    pub fn napi_create_bigint_words(env: napi_env, sign_bit: i32, word_count: usize, words: *const u64, result: *mut napi_value) -> napi_status;
    pub fn napi_get_value_bigint_int64(env: napi_env, value: napi_value, result: *mut i64, lossless: *mut bool) -> napi_status;
    pub fn napi_get_value_bigint_uint64(env: napi_env, value: napi_value, result: *mut u64, lossless: *mut bool) -> napi_status;
    pub fn napi_get_value_bigint_words(env: napi_env, value: napi_value, sign_bit: *mut i32, word_count: *mut usize, words: *mut u64) -> napi_status;
    pub fn napi_get_all_property_names(env: napi_env, object: napi_value, key_mode: i32, key_filter: i32, key_conversion: i32, result: *mut napi_value) -> napi_status;
    pub fn napi_set_instance_data(env: napi_env, data: *mut c_void, finalize_cb: napi_finalize, finalize_hint: *mut c_void) -> napi_status;
    pub fn napi_get_instance_data(env: napi_env, data: *mut *mut c_void) -> napi_status;
    pub fn napi_detach_arraybuffer(env: napi_env, arraybuffer: napi_value) -> napi_status;
    pub fn napi_is_detached_arraybuffer(env: napi_env, value: napi_value, result: *mut bool) -> napi_status;
    pub fn napi_type_tag_object(env: napi_env, value: napi_value, type_tag: *const napi_type_tag) -> napi_status;
    pub fn napi_check_object_type_tag(env: napi_env, value: napi_value, type_tag: *const napi_type_tag, result: *mut bool) -> napi_status;
    pub fn napi_object_freeze(env: napi_env, object: napi_value) -> napi_status;
    pub fn napi_object_seal(env: napi_env, object: napi_value) -> napi_status;

    pub fn js_create_runtime(runtime: *mut napi_runtime) -> napi_status;
    pub fn js_create_napi_env(env: *mut napi_env, runtime: napi_runtime) -> napi_status;
    pub fn js_free_napi_env(env: napi_env) -> napi_status;
    pub fn js_free_runtime(runtime: napi_runtime) -> napi_status;
    pub fn js_execute_script(env: napi_env, script: napi_value, file: *const c_char, result: *mut napi_value) -> napi_status;
    pub fn js_execute_pending_jobs(env: napi_env) -> napi_status;
}

