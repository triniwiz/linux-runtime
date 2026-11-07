Vendored from NativeScript/windows packages/windows-v8/vendor (napi-android's V8 shim, ported to V8 14.7):
- shim: v8-api.cpp (Node-API over V8's C++ API), SimpleAllocator
- napi: Node-API headers
- compat: `<android/log.h>` stub

`[linux port]` changes:
- napi_wrap / napi_unwrap fall back to a private property on objects without an internal field
  (anything not made by napi_define_class), as Node allows wrapping any object
- napi_get_version is weak: nativescript-napi's compat provides it
