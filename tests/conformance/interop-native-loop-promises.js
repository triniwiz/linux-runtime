const { eq } = require('./harness');

const loop = g_main_loop_new(null, 0);
let resolved = false;
g_idle_add(() => {
  // microtasks queued from a C -> JS callback run from the main loop
  Promise.resolve().then(() => { resolved = true; g_main_loop_quit(loop); });
  return 0;
}, null);
g_main_loop_run(loop);
g_main_loop_unref(loop);
eq(resolved, true, "promise resolved while g_main_loop_run was blocking");
