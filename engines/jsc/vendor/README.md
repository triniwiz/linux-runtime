Vendored from NativeScript/windows packages/windows-jsc/vendor:
- shim: napi-android's Node-API over JavaScriptCore's public C API (jsc-api.cpp, jsr.cpp)
- napi: Node-API headers
- include: JavaScriptCore public C API headers

The engine is the system WebKitGTK JavaScriptCore (`libjavascriptcoregtk-6.0` / `-4.1` / `-4.0`),
which exports the same C API, the JSWeak* calls the shim uses and a few JSC internals.

`[linux port]` changes:
- the global object is created with a class, so the global's (immutable) prototype is an
  ordinary object whose own prototype can change; `js_set_global_lookup` puts a classed object
  with has/get callbacks there for metadata globals (JSC rejects a Proxy in that chain)
- `js_execute_pending_jobs` drains microtasks (`JSC::VM::drainMicrotasks`, under a `JSLockHolder`):
  JSC only drains when the outermost VM entry returns, which a nested GLib main loop never does
- unhandled rejections reach `onUnhandledPromiseRejectionTracker` through
  `JSGlobalContextSetUnhandledRejectionCallback`
- the GC suspends threads with SIGPWR (`JSConfigureSignalForGC`) instead of SIGUSR1, the
  livesync signal, whose handler made the first collection hang
- one JSClass per native info type, not per wrapped object (JSC keeps per-class data in the
  global object: struct views leaked and slowed down); cached property keys; sized UTF-8 strings
  without a codecvt round trip
- `napi_get_property_names` passes the object (for-in keys); array buffers are zero-filled
- the env releases its context last; missing `<climits>`, `<mutex>`, `<cstring>` includes
- `napi_get_version` is weak: nativescript-napi's compat provides it
