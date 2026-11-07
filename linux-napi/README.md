# @nativescript/linux-napi

The NativeScript Linux C interop as a Node-API addon: C libraries (libc, GLib, GTK 4, ...) callable
from an existing Node.js process, resolved from the metadata bundled with the package.

```js
const ns = require('@nativescript/linux-napi');
const { interop } = ns;

ns.gtk_init();
const win = ns.gtk_window_new();
ns.gtk_window_set_child(win, ns.gtk_label_new('Hello from Node.js'));
ns.gtk_window_present(win);
ns.runGLib();
```

Requires Linux (x64 or arm64) and Node.js 18 or later.

## API

- C functions, structs, unions, enums and constants are properties of the module, resolved on
  first use.
- `interop`: `Pointer`, `Reference`, `types`, `alloc`/`free`/`sizeof` and the other interop helpers.
- `runGLib()` runs this thread's GLib main context on Node's event loop: GTK, `g_timeout_add` and C
  callbacks from other threads are dispatched as soon as they're ready. It keeps the process alive
  until `stopGLib()`.
- `pumpGLib()` dispatches what's ready once, without blocking, for hosts that drive GLib themselves.

Exceptions thrown in callbacks from C reach `process.on('uncaughtException')`. The addon works in
`worker_threads`, with one GLib main context per thread.

`NS_METADATA` (paths separated by `:`) replaces the bundled metadata, e.g. with metadata generated for
other libraries.

## Development

```sh
npm run build    # cargo build --release, then linux.<arch>.node
NS_METADATA=../metadata-generator/tests/TestOutput/metadata-$(uname -m).bin npm test
```
