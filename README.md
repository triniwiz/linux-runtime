# Linux Runtime for NativeScript

The NativeScript runtime for the Linux desktop. It makes C libraries (libc, GLib, GTK 4, ...) callable
from JavaScript: a clang-based metadata generator turns their headers into binary metadata and
TypeScript typings, and the runtime resolves JavaScript globals from that metadata and calls into C.

```js
gtk_init();
const label = gtk_label_new('Hello from JavaScript'); // GtkWidget* as an interop.Pointer
printf('%s has %d items\n', 'list', 3);
const { quot, rem } = div(17, 5); // structs by value
const nums = new Int32Array([3, 1, 2]);
const int = (p) => new interop.Reference(interop.types.int32, p).value;
qsort(nums, nums.length, 4, (a, b) => int(a) - int(b)); // JS function as a C callback
```

## Projects

| Path | |
|---|---|
| [`metadata-generator/`](metadata-generator) | Parses the C headers listed in `linux/sdk.json` and writes binary metadata and `.d.ts` typings |
| [`metadata/`](metadata) | Rust reader for the binary metadata |
| [`core/`](core) | Engine independent parts: module resolution, C layouts and calls, the GLib event loop, `app.nsbundle`, the JS preludes |
| [`runtime/`](runtime) | The V8 runtime (rusty_v8): C interop, ES modules and CommonJS, fetch and WebSocket, workers, DevTools |
| [`cli/`](cli) | `ns-linux`, the runtime executable |
| [`napi/`](napi), [`engines/`](engines) | The same runtime over Node-API, on QuickJS, V8, JavaScriptCore and Hermes ([engines/README.md](engines/README.md)) |
| [`linux-napi/`](linux-napi) | `@nativescript/linux-napi`: the C interop as a Node.js addon ([linux-napi/README.md](linux-napi/README.md)) |
| [`npm/`](npm) | Package template and typings for the npm packages |
| [`tests/conformance/`](tests/conformance) | Scripts every runtime has to pass |
| [`examples/`](examples) | Small scripts, a GTK demo and benchmarks |

## Build Prerequisites

Ubuntu 24.04 or similar, and a Rust toolchain ([rustup](https://rustup.rs)):

```sh
sudo apt install build-essential cmake ninja-build lld pkg-config python3 \
  libglib2.0-dev libgtk-4-dev libgles-dev libegl-dev libxml2-dev libicu-dev uuid-dev \
  zlib1g-dev libfontconfig-dev libpango1.0-dev
```

## How to Build

```sh
# metadata generator -> metadata-generator/dist/<arch>/bin
# (builds LLVM 17.0.6 into llvm/ first, once)
./build_metadata_generator.sh

# metadata and typings for the SDK -> metadata-generator/tests/TestOutput
metadata-generator/tests/test-mdg-executable.sh metadata-generator/dist/$(uname -m)/bin/metadata-generator

cargo build --release
```

Each module in `metadata-generator/linux/sdk.json` lists its headers, pkg-config packages and the
shared libraries the runtime loads. Headers that aren't installed can come from a sysroot:
`metadata-generator/linux/fetch-sysroot.sh /tmp/ns-sysroot`, then set `NS_SYSROOT=/tmp/ns-sysroot`
for the metadata step.

## How to Run

```sh
export NS_METADATA=metadata-generator/tests/TestOutput/metadata-$(uname -m).bin
./target/release/ns-linux examples/smoke.js
./target/release/ns-linux examples/gtk-demo.js
./target/release/ns-linux -e 'console.log(getpid())'
```

`ns-linux [--metadata <file>] [--inspect[-brk][=port]] [--watch] <script | app folder | app.nsbundle> [args]`

- `--watch` restarts the script when its files change.
- `NS_V8_FLAGS` passes flags to V8.

## How to Run Tests

```sh
cargo test --release -p nativescript-core -p nativescript-runtime -p nativescript-cli
# each Node-API engine on its own
cargo test --release -p nativescript-quickjs
cargo test --release -p nativescript-v8
```

The tests use the metadata in `metadata-generator/tests/TestOutput`. The addon builds in its own
workspace:

```sh
cd linux-napi && npm run build
NS_METADATA=../metadata-generator/tests/TestOutput/metadata-$(uname -m).bin npm test
```

 `tests/conformance` runs on every
runtime through each crate's `conformance` test. C call overhead is measured by
`./target/release/ns-linux examples/bench/ffi.js`.

## How to Connect the Inspector

```sh
./target/release/ns-linux --inspect-brk examples/smoke.js
```

The runtime prints a `devtools://` URL to open in Chrome or Chromium. It also shows up under
`chrome://inspect`, and VS Code can attach to `127.0.0.1:9229`. `--inspect-brk` stops before any
script code runs; `--inspect` doesn't wait.

## Release Builds

Release executables have the development features (command line flags, environment overrides,
LiveSync, the inspector) switched off by fuses, settings written into the executable. `ns-bundle`
packs an app folder into a read-only `app.nsbundle`, optionally encrypted, and sets the fuses:

```sh
./target/release/ns-bundle pack --input app --output app.nsbundle --exe ns-linux [--seal] [--fuse inspector=on]
./target/release/ns-bundle fuses ns-linux
```

## Building a Distribution Package

```sh
scripts/pack-npm.sh 0.1.0
```

This builds `@nativescript/linux` (V8), the Node-API engine packages and `@nativescript/linux-napi`
into `dist/npm/`, for the host architecture. `ENGINES="quickjs v8 napi"` limits the packages, and
`STRICT=1` fails instead of skipping one that can't be built.

`.github/workflows/ci.yml` runs the tests on x64 and arm64. `.github/workflows/npm_release.yml` builds
both architectures on a `v*` tag, merges them into one package per engine and publishes to npm.
