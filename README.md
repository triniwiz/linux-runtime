# NativeScript for Linux

C APIs (libc, GLib, ...) exposed to JavaScript from generated metadata.

- `metadata-generator/` – the NativeScript iOS metadata generator, ported to parse C headers. Emits binary metadata and `.d.ts` typings.
- `metadata/` – Rust reader for the binary metadata.
- `runtime/` – V8 runtime (rusty_v8) that resolves globals from metadata and calls C through libffi.
- `cli/` – `ns-linux`, runs a script.
- `napi/` – the same runtime written against Node-API, run on QuickJS, V8, JavaScriptCore and Hermes by `engines/` (see `engines/README.md`).

## Build

Dependencies: cmake, a C++17 compiler, libxml2-dev, Rust.

```sh
# LLVM 17.0.6 (clang libraries) into llvm/
./build_llvm.sh
# Metadata generator -> metadata-generator/dist/<arch>/bin
./build_metadata_generator.sh
```

Linux headers don't ship clang module maps, so the libraries to expose are described in a module map (see
`metadata-generator/tests/linux/module.modulemap`, its `link` entries are the sonames the runtime `dlopen`s).

```sh
# Generate metadata + typings for libc, GLib and libxml2 into metadata-generator/tests/TestOutput
metadata-generator/tests/test-mdg-executable.sh metadata-generator/dist/$(uname -m)/bin/metadata-generator

cargo build
NS_METADATA=metadata-generator/tests/TestOutput/metadata-$(uname -m).bin ./target/debug/ns-linux examples/smoke.js
```

```js
printf("%s has %d items\n", "list", 3);
const t = new tm();
gmtime_r(new interop.Reference(interop.types.long, 0), t);
qsort(nums, nums.length, 4, (a, b) => /* JS callback */ 0);
```
