# ns-linux-hermes

NativeScript Linux on Hermes, through the Node-API implementation in
[microsoft/hermes-windows](https://github.com/microsoft/hermes-windows) (the engine behind the
Windows runtime's `@nativescript/windows-hermes`), built for Linux from source.

```sh
engines/hermes/build-hermes.sh          # -> engines/hermes/vendor/<arch>/{libhermes.so,hermesc}
cargo build --release -p nativescript-hermes
```

`build-hermes.sh` downloads a pinned hermes-windows revision (`HERMES_REV`, ~43 MB) or uses
`HERMES_SRC`, and builds two targets with CMake: `libhermes.so` (Hermes VM + Node-API +
`csrc/hermes_jsr.cpp`, the js_* host API the runtime calls) and `hermesc`. hermes-windows'
own `hermes_api.cpp` (the `jsr_*` API) is Windows-only (WER, inspector stubs), so it isn't built.
Patches applied to the source:

- `include(CTest)`: CMake module names are case sensitive off Windows
- the global object's prototype may change: metadata globals resolve through a Proxy there

`build.rs` links `libhermes.so` from `vendor/<arch>` (or `NS_HERMES_DIR`) with an `$ORIGIN` rpath and
copies it and `hermesc` next to the binary. The library needs the system ICU (`libicuuc`,
`libicui18n`).

Notes:
- promises are Hermes' JS implementation; unhandled rejections are reported through its rejection
  tracker, which (as in React Native) waits 100 ms (TypeError, ReferenceError) or 2 s first
- ES modules run through the shared module runner (Hermes has no `import`/`export` parser)
- no inspector (`HERMES_ENABLE_DEBUGGER=OFF`) and no `Intl`
