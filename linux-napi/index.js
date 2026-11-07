'use strict';
// C declarations from the bundled metadata, `interop`, and GLib's main context on the host loop.
const path = require('node:path');

const arch = { x64: 'x86_64', arm64: 'aarch64' }[process.arch];
if (process.platform !== 'linux' || !arch) {
  throw new Error(`@nativescript/linux-napi doesn't support ${process.platform}-${process.arch}`);
}
const native = require(`./linux.${process.arch}.node`);
const metadata = process.env.NS_METADATA
  ? process.env.NS_METADATA.split(path.delimiter)
  : [path.join(__dirname, `metadata-${arch}.bin`)];
const ns = native.init(metadata);

const runGLib = ns.runGLib;
const stopGLib = ns.stopGLib;
let fallback;
Object.defineProperties(ns, {
  // without libuv, GLib is polled instead
  runGLib: {
    value() {
      if (!runGLib() && !fallback) fallback = setInterval(() => ns.pumpGLib(), 4);
    },
  },
  stopGLib: {
    value() {
      stopGLib();
      clearInterval(fallback);
      fallback = undefined;
    },
  },
});

module.exports = ns;
