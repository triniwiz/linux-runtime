/// <reference path="./typings-gtk3/index.d.ts" />

/** C functions, structs, enums and constants by name, plus `interop`. */
declare const ns: typeof globalThis & {
  interop: typeof interop;
  /** Dispatches what's ready on this thread's GLib main context; true if anything ran. */
  pumpGLib(): boolean;
  /**
   * Runs this thread's GLib main context (GTK, g_timeout_add, callbacks from other threads)
   * on the host's event loop, dispatching sources as they become ready. Keeps the process
   * alive until `stopGLib()`.
   */
  runGLib(): void;
  stopGLib(): void;
};
export = ns;
