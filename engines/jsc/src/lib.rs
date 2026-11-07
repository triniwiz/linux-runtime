//! JavaScriptCore engine: napi_* and js_* (jsr) from the vendored shim over
//! WebKitGTK's JavaScriptCore C API.

/// The libjavascriptcoregtk this build links.
pub const JSC_SONAME: &str = env!("NS_JSC_SONAME");
