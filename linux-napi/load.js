'use strict';
// Loads the addon with the bundled GTK 4 or GTK 3 metadata (or $NS_METADATA).
const fs = require('node:fs');
const path = require('node:path');

const arch = { x64: 'x86_64', arm64: 'aarch64' }[process.arch];
let loaded;

/** GTK 3 when the process already has it (Electron loads it before the main script). */
function gtkInProcess() {
  try {
    return /libgtk-3\.so/.test(fs.readFileSync('/proc/self/maps', 'utf8')) ? 3 : 4;
  } catch {
    return 4;
  }
}

/** `gtk`: 3, 4, or undefined to match the process. */
module.exports = function load(gtk) {
  if (process.platform !== 'linux' || !arch) {
    throw new Error(`@nativescript/linux-napi doesn't support ${process.platform}-${process.arch}`);
  }
  gtk ??= loaded?.gtk ?? gtkInProcess();
  if (loaded) {
    if (loaded.gtk !== gtk) {
      // GTK aborts the process when both are loaded
      throw new Error(`@nativescript/linux-napi is loaded for GTK ${loaded.gtk}; GTK 3 and GTK 4 can't share a process`);
    }
    return loaded.ns;
  }
  const name = gtk === 3 ? 'metadata-gtk3' : 'metadata';
  const native = require(`./linux.${process.arch}.node`);
  const metadata = process.env.NS_METADATA
    ? process.env.NS_METADATA.split(path.delimiter)
    : [path.join(__dirname, `${name}-${arch}.bin`)];
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
  loaded = { gtk, ns };
  return ns;
};
