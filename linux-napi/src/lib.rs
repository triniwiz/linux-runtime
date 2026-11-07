//! `linux.<arch>.node`: see `nativescript_napi::addon`.

use nativescript_napi::sys::{napi_env, napi_value};
use std::ffi::c_void;

/// Keeps this library loaded for the life of the process. Hosts unload a worker's addons when
/// the worker stops, but Rust's thread-local destructors (e.g. `std::thread::current()`) run
/// later, at thread exit.
fn pin() {
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(pin as *const c_void, &mut info) != 0 && !info.dli_fname.is_null() {
            libc::dlopen(info.dli_fname, libc::RTLD_NOW | libc::RTLD_NODELETE);
        }
    }
}

/// # Safety
/// Called by the Node-API host when it loads the addon.
#[no_mangle]
pub unsafe extern "C" fn napi_register_module_v1(env: napi_env, exports: napi_value) -> napi_value {
    static PIN: std::sync::Once = std::sync::Once::new();
    PIN.call_once(pin);
    nativescript_napi::addon::register(env, exports)
}
