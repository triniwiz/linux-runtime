# Linux Runtime for NativeScript

The NativeScript runtime for the Linux desktop. It makes C libraries (libc, GLib, GTK, X11, ...) callable
from JavaScript: a clang-based metadata generator turns their headers into binary metadata and
TypeScript typings, and the runtime resolves JavaScript globals from that metadata and calls into C.
The same interop is available in Node.js and Electron as the `@nativescript/linux-napi` addon.

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
| [`metadata-generator/`](metadata-generator) | Parses the C headers listed in `linux/sdk.json` (GTK 4) or `linux/sdk-gtk3.json` (GTK 3) and writes binary metadata and `.d.ts` typings |
| [`metadata/`](metadata) | Rust reader for the binary metadata |
| [`core/`](core) | Engine independent parts: module resolution, C layouts and calls, the GLib event loop, `app.nsbundle`, the JS preludes |
| [`runtime/`](runtime) | The V8 runtime (rusty_v8): C interop, ES modules and CommonJS, fetch and WebSocket, workers, DevTools |
| [`cli/`](cli) | `ns-linux`, the runtime executable |
| [`napi/`](napi), [`engines/`](engines) | The same runtime over Node-API, on QuickJS, V8, JavaScriptCore and Hermes ([engines/README.md](engines/README.md)) |
| [`linux-napi/`](linux-napi) | `@nativescript/linux-napi`: the C interop as a Node.js addon, with GTK 4 and GTK 3 metadata ([linux-napi/README.md](linux-napi/README.md)) |
| [`npm/`](npm) | Package template and typings for the npm packages |
| [`tests/conformance/`](tests/conformance) | Scripts every runtime has to pass |
| [`examples/`](examples) | Small scripts, a GTK demo and benchmarks |

## Build Prerequisites

Ubuntu 24.04 or similar, and a Rust toolchain ([rustup](https://rustup.rs)):

```sh
sudo apt install build-essential cmake ninja-build lld pkg-config python3 \
  libglib2.0-dev libgtk-4-dev libgtk-3-dev libgles-dev libegl-dev libxml2-dev libicu-dev uuid-dev \
  zlib1g-dev libfontconfig-dev libpango1.0-dev
```

## How to Build

```sh
# metadata generator -> metadata-generator/dist/<arch>/bin
# (builds LLVM 17.0.6 into llvm/ first, once)
./build_metadata_generator.sh

# metadata and typings for the SDK -> metadata-generator/tests/TestOutput
metadata-generator/tests/test-mdg-executable.sh metadata-generator/dist/$(uname -m)/bin/metadata-generator

# the GTK 3 SDK, for linux-napi's GTK 3 metadata -> metadata-generator/tests/TestOutput-gtk3
NS_SDK_CONFIG=$PWD/metadata-generator/linux/sdk-gtk3.json \
  metadata-generator/tests/test-mdg-executable.sh metadata-generator/dist/$(uname -m)/bin/metadata-generator \
  $PWD/metadata-generator/tests/TestOutput-gtk3

cargo build --release
```

Each module in `metadata-generator/linux/sdk.json` lists its headers, pkg-config packages and the
shared libraries the runtime loads. `sdk-gtk3.json` is the same SDK with GTK 3 instead of GTK 4: the
two can't share a process, and Electron loads GTK 3. The script runs the generator from its own
directory, so pass absolute paths.

Headers that aren't installed can come from a sysroot:
`metadata-generator/linux/fetch-sysroot.sh /tmp/ns-sysroot` (or, for GTK 3,
`fetch-sysroot.sh /tmp/ns-sysroot-gtk3 libgtk-3-dev --pkg gtk+-3.0 --pkg x11`), then set `NS_SYSROOT` to it
for the metadata step.

## How to Run

```sh
export NS_METADATA=metadata-generator/tests/TestOutput/metadata-$(uname -m).bin
./target/release/ns-linux examples/smoke.js
./target/release/ns-linux examples/gtk-demo.js
./target/release/ns-linux -e 'console.log(getpid())'
```

`ns-linux [--metadata <file[:file...]>] [--inspect[-brk][=port]] [--watch] <script | app folder | app.nsbundle> [args]`

- `--metadata` and `NS_METADATA` take one file or a `:`-separated list, like the Node-API addon.
- `--watch` restarts the script when its files change.
- `NS_V8_FLAGS` passes flags to V8.

C structs are globals under their typedef name (`GdkRGBA`, `graphene_rect_t`, `GList`). The struct
tag (`_GdkRGBA`) is the same constructor.

## GObjects

A GObject that reaches JS (a C function's result, a struct field, a signal argument) is an object of
its class, one per GObject: `gtk_widget_get_parent(label) === box`. A class's C functions are its
methods, named without the type's prefix, and its GObject properties read and write in camelCase:

```js
const label = new GtkLabel({ label: 'Hi' }); // g_object_new with construct properties
label.set_text('Hello');                     // gtk_label_set_text(label, 'Hello')
label.maxWidthChars = 20;                    // the max-width-chars property
box.append(label);                           // gtk_box_append(box, label)
const id = label.connect('notify::label', (self) => console.log(self.label));
label.disconnect(id);
```

They are still `interop.Pointer`s and pass to C functions as before. Methods come from functions whose
first parameter is the class's struct, and from the interfaces the class implements. A property has
a getter if it's readable and a setter if it's writable after construction, so writing a read-only
one throws in strict mode; when the class has a matching C getter or setter (`gtk_widget_get_opacity`
for `opacity`), the property uses it. A JS object
holds a reference on its GObject: a floating reference (a new widget) becomes its own, otherwise it
takes one. C headers don't say who owns a result, so a function that returns a new reference
(`transfer full`) leaves the caller's reference to release, as in C. `gpointer` results stay
Pointers.

## GObject Subclasses

A JS class extending a GObject's struct registers a GType: [core/js/gobject.js](core/js/gobject.js)
describes the class once, and the runtime runs it natively ([core/src/gobject.rs](core/src/gobject.rs) and each engine's
`gobject.rs`).
Methods override the vfuncs and interface methods of the same name (or its snake_case form), and
`super.method()` calls the parent's implementation, also when the class has a method of the same
name (`super.measure()` calls the parent's vfunc, not `gtk_widget_measure`):

```js
class Swatch extends GtkWidget {
  static interfaces = [GtkOrientableIface];
  static properties = { orientation: { override: true }, hue: { type: G_TYPE_DOUBLE } };
  static signals = { picked: { params: [G_TYPE_DOUBLE] } };
  snapshot(snapshot) { /* ... */ }
}
const swatch = new Swatch({ hue: 0.5 }); // GObject properties, as g_object_new
swatch.emit('picked', swatch.hue);
```

`GtkWidget.extend(name, members)` does the same without class syntax, and TypeScript's ES5 output
(`@NativeClass`, `@Interfaces`) works too. Instances are views of the GObject's instance struct, so
they pass to C as the object. A JS object holds a toggle reference on its GObject: it is kept alive
while C code references the GObject, and collecting it releases the GObject.

## Metadata for an App's Own C Library

An app or plugin that ships a C library generates metadata from its headers, and the runtime loads it
with the SDK metadata.

### With the NativeScript CLI

`ns build linux` and `ns run linux` do this on every build, as on iOS. Put the native code in
`App_Resources/Linux` for the app, or `platforms/linux` in a plugin:

```
src/              C / C++ sources and headers, compiled into a shared library
src/module.modulemap
include/          public headers (and their module.modulemap), for prebuilt libraries
lib/<x64|arm64>/  prebuilt .so files (bundled) and .a files (linked into the library)
native.json       { "pkg-config": ["gtk4"], "cflags": [], "libs": [] }
CMakeLists.txt    builds the native code instead, see below
```

`native.json` is enough for a few sources. A `CMakeLists.txt` takes over the build: the CLI configures
and builds it (`CMAKE_BUILD_TYPE` follows `--release`, `NS_ARCH` is `x64` or `arm64`), bundles every
shared library it builds (`add_library(mason SHARED ...)`) in `lib/`, and parses the headers with the
include paths and defines from its `compile_commands.json`. `src/` is then left to CMake. The build
folder is kept in `platforms/linux/build/native`, so later builds are incremental.

A `module.modulemap` anywhere in `src/` or `include/` is used as is. Its headers are generated, its
modules are what the metadata contains, and the runtime loads its `link` libraries. Sources in `src/`
are compiled into the library the map's first `link` names (`link "mason"` builds `lib/libmason.so`).
A map can also expose a library the app doesn't build, from headers anywhere:

```
module sqlite3 [system] {
  header "/usr/include/sqlite3.h"
  link "sqlite3"
}
```

Without a module map, the CLI exposes every header in `include/` (or `src/`) as a module named after
the app or plugin, linked to the compiled library or the prebuilt ones. The build writes
`metadata/<name>.bin` and `lib/` next to the executable. `ns typings linux` writes the typings to
`typings/linux`, and later builds keep them up to date.

### By Hand

1. Write a module map for the library. `header` lists the headers to expose, `link` the library to load:

   ```
   module mason [system] {
     header "/path/to/include/mason_c.h"
     link "mason_c"
   }
   ```

   A `link` is a soname (`libmason_c.so.1`), a file name in the app's `lib/` (`libmason_c.so`) or an
   `-l` style name (`mason_c`). An `-l` style name is looked up as `libmason_c.so` or
   `libmason_c.so.<version>` in `lib/`, then as `libmason_c.so` on the system's library paths. If a
   library doesn't load, the runtime prints the names it tried.

2. Generate the metadata and typings. Run the generator from its own directory (it finds clang's
   builtin headers there), with absolute paths and the include paths of the libraries the headers use:

   ```sh
   echo '#include <mason_c.h>' > /tmp/umbrella.h
   cd metadata-generator/dist/$(uname -m)/bin
   ./metadata-generator -input-umbrella /tmp/umbrella.h \
     -output-bin /path/to/app/metadata/mason.bin -output-typescript /path/to/typings \
     Xclang -fmodule-map-file=/path/to/module.modulemap -I/path/to/include $(pkg-config --cflags-only-I gtk4)
   ```

   Only the module map's headers are generated. Types they use from other headers (`GtkWidget *`,
   `GdkRGBA`, `graphene_rect_t *`) are referenced by name, and come from the SDK metadata at run
   time. The typings refer to the SDK's typings for them. A library that passes structs by value from
   headers outside the SDK adds those headers to its module map too.

3. Put the files next to the executable:

   ```
   ns-linux
   metadata-<arch>.bin    SDK metadata
   metadata/mason.bin     every metadata/*.bin is loaded after the SDK metadata
   lib/libmason_c.so      searched first for `link` names
   ```

   The layout is relative to the executable, so the folder can be moved. During development, pass the
   files instead: `ns-linux --metadata sdk.bin:mason.bin app.js` or
   `NS_METADATA=sdk.bin:mason.bin`. The Node-API addon takes the same `NS_METADATA` list. When two
   files declare a name, the first file wins.

## How to Run Tests

```sh
cargo test --release -p nativescript-core -p nativescript-runtime -p nativescript-cli
# each Node-API engine on its own
cargo test --release -p nativescript-quickjs
cargo test --release -p nativescript-v8
```

The tests use the metadata in `metadata-generator/tests/TestOutput`. `tests/conformance` runs on every
runtime through each crate's `conformance` test. C call overhead is measured by
`./target/release/ns-linux examples/bench/ffi.js`.

The addon builds in its own workspace; `npm run build` also copies the generated GTK 4 and GTK 3
metadata next to it:

```sh
cd linux-napi && npm run build && npm test
```

## How to Connect the Inspector

```sh
./target/release/ns-linux --inspect-brk examples/smoke.js
```

The runtime prints a `devtools://` URL to open in Chrome or Chromium. It also shows up under
`chrome://inspect`, and VS Code can attach to `127.0.0.1:9229`. `--inspect-brk` stops before any
script code runs; `--inspect` doesn't wait.

CPU profiles show time spent in C under the C function's name. V8 names native functions when
profiling starts, so a C function first used after the profile started is counted in its JS caller.

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
`STRICT=1` fails instead of skipping one that can't be built. The packages take their metadata from
`metadata-generator/tests/TestOutput` (`METADATA`), and linux-napi its GTK 3 metadata from
`TestOutput-gtk3` (`METADATA_GTK3`).

`.github/workflows/ci.yml` runs the tests on x64 and arm64. `.github/workflows/npm_release.yml` builds
both architectures on a `v*` tag, merges them into one package per engine and publishes to npm.
