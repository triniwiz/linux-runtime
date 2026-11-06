const { eq } = require('./harness');

const loop = g_main_loop_new(null, 0);
let n = 0;
g_idle_add(() => { n++; if (n === 5) { g_main_loop_quit(loop); return 0; } return 1; }, null);
g_main_loop_run(loop);
g_main_loop_unref(loop);
eq(n, 5, "idle callbacks");
