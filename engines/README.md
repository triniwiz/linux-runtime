# Node-API engines

The `napi/` runtime (engine independent, the same metadata interop, event loop and modules as the
V8 runtime) on other JavaScript engines.
Each engine crate provides `napi_*` (an engine shim) plus napi-android's `js_*` host functions
(`js_create_runtime`, `js_execute_script`, `js_execute_pending_jobs`, ...) and a `ns-linux-<engine>`
binary with the same command line as `ns-linux`.

| Crate | Binary | npm | Engine | Node-API |
|---|---|---|---|---|
| `quickjs` | `ns-linux-quickjs` | `@nativescript/linux-quickjs` | quickjs-ng, vendored | napi-android shim |
| `v8` | `ns-linux-v8` | `@nativescript/linux-v8` | rusty_v8 (V8 14.7), as the V8 runtime | napi-android shim (g++ / libstdc++) |
| `jsc` | `ns-linux-jsc` | `@nativescript/linux-jsc` | system WebKitGTK `libjavascriptcoregtk` | napi-android shim over the JSC C API |
| `hermes` | `ns-linux-hermes` | `@nativescript/linux-hermes` | microsoft/hermes-windows, built by `hermes/build-hermes.sh` | hermes-windows' own |

`buffer` adds node_api.h's Buffer functions for the shims without them. `jsc` and `hermes` aren't
default workspace members since they need an engine library:

```sh
cargo build --release -p nativescript-v8 -p nativescript-quickjs
cargo build --release -p nativescript-jsc        # libjavascriptcoregtk-6.0 / -4.1 / -4.0, or NS_JSC_LIB
engines/hermes/build-hermes.sh && cargo build --release -p nativescript-hermes
cargo test --release -p nativescript-<engine> --test conformance
scripts/pack-npm.sh                              # dist/npm/nativescript-linux-<engine>-<version>.tgz
```

ES modules run through `napi/src/esm.rs` (oxc's module runner transform): none of the shims
expose a module loader. HTTP modules, `ns:module` and `import()` from CommonJS go through the
same runner.
