# ns-linux-hermes

NativeScript Linux on Hermes, through the Node-API implementation in
[microsoft/hermes-windows](https://github.com/microsoft/hermes-windows), built for Linux from source.

```sh
engines/hermes/build-hermes.sh          # -> engines/hermes/vendor/<arch>/{libhermes.a,hermesc}
cargo build --release -p nativescript-hermes
```

`build-hermes.sh` downloads a pinned hermes-windows revision (`HERMES_REV`, ~43 MB) or uses
`HERMES_SRC`, and builds two targets with CMake: Hermes VM + Node-API + `csrc/hermes_jsr.cpp`
(the js_* host API the runtime calls), merged into one `libhermes.a`, and `hermesc`. hermes-windows'
own `hermes_api.cpp` (the `jsr_*` API) is Windows-only (WER, inspector stubs), so it isn't built.
Patches applied to the source:

- `include(CTest)`: CMake module names are case sensitive off Windows
- the global object's prototype may change: metadata globals resolve through a Proxy there

`build.rs` links `libhermes.a` from `vendor/<arch>` (or `NS_HERMES_DIR`) whole, so the executable
exports `napi_*` to addons like the other engines, and copies `hermesc` next to it. The
`node_api_*` functions nativescript-napi's compat also defines are weakened in the archive. Hermes
needs the system ICU (`libicuuc`, `libicui18n`), a soname that differs between distributions.

Notes:
- promises are Hermes' JS implementation; unhandled rejections are reported through its rejection
  tracker, which (as in React Native) waits 100 ms (TypeError, ReferenceError) or 2 s first
- ES modules run through the shared module runner (Hermes has no `import`/`export` parser)
- no inspector (`HERMES_ENABLE_DEBUGGER=OFF`) and no `Intl`
