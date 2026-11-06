const { eq } = require('./harness');

nativescript.ref();
const js = g_thread_self().toString();
let calls = 0;
const t = g_thread_new("worker", () => {
  calls++;
  eq(g_thread_self().toString(), js, "same thread");
  nativescript.unref();
  return null;
}, null);
setTimeout(() => eq(calls, 1, "called"), 50);
