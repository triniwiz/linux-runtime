//! V8 over Node-API: napi-android's v8-api.cpp on rusty_v8. The platform is
//! set up here since the C++ shim can't create one (libc++ vs libstdc++).

use std::ffi::{c_char, CStr};
use std::sync::OnceLock;

static PLATFORM: OnceLock<v8::SharedRef<v8::Platform>> = OnceLock::new();

/// Initializes V8 once; call before the runtime is created.
pub fn init() {
    PLATFORM.get_or_init(|| {
        if let Ok(flags) = std::env::var("NS_V8_FLAGS") {
            v8::V8::set_flags_from_string(&flags);
        }
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform.clone());
        v8::V8::initialize();
        platform
    });
}

/// Runs the platform's foreground tasks for `isolate` (from js_execute_pending_jobs).
#[no_mangle]
pub unsafe extern "C" fn ns_v8_pump(isolate: *mut v8::RealIsolate) {
    if let (Some(platform), false) = (PLATFORM.get(), isolate.is_null()) {
        let isolate = v8::Isolate::from_raw_ptr(isolate);
        while v8::Platform::pump_message_loop(platform, &isolate, false) {}
    }
}

#[no_mangle]
pub unsafe extern "C" fn ns_v8_fatal_error(location: *const c_char, message: *const c_char) {
    let text = |p: *const c_char| {
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    eprintln!("V8 fatal error: {}: {}", text(location), text(message));
    std::process::abort();
}
