// libc
puts("hello from C puts()");
printf("printf: %s is %d years old, pi=%.3f\n", "linux", 33, 3.14159);
console.log("strlen('nativescript') =", strlen("nativescript"));
console.log("abs(-42) =", abs(-42), " sqrt(2) =", sqrt(2), " pow(2, 10) =", pow(2, 10));

// char* returns are Pointers
const home = getenv("HOME");
console.log("getenv('HOME') =", home, "->", interop.stringFromCString(home));

// out parameter through a Reference
// (strings passed to C are temporary, endptr must point into memory that outlives the call)
const src = strdup("1234xyz");
const end = new interop.Reference(interop.types.pointer, null);
const n = strtol(src, end, 10);
console.log("strtol =", n, " rest =", interop.stringFromCString(end.value));
free(src);

// structs returned by value
console.log("div(17, 5) =", div(17, 5), " ldiv(-7, 2) =", ldiv(-7, 2));

// structs: out param from a plain object, copied back after the call
const t = new interop.Reference(interop.types.long, 86400 * 365);
const tmv = new tm();
gmtime_r(t, tmv);
console.log("gmtime_r(1971-01-01):", tmv.tm_year + 1900, tmv.tm_mon + 1, tmv.tm_mday, "yday", tmv.tm_yday);

// callbacks: qsort with a JS comparator
const nums = new Int32Array([5, 3, 9, 1, 7]);
qsort(nums, nums.length, 4, (a, b) => {
  const x = new interop.Reference(interop.types.int32, a).value;
  const y = new interop.Reference(interop.types.int32, b).value;
  return x - y;
});
console.log("qsort:", nums);

// memory
const buf = interop.alloc(32);
snprintf(buf, 32, "%s-%d", "abc", 42);
console.log("snprintf into alloc:", interop.stringFromCString(buf), "sizeof(tm) =", interop.sizeof(tm));

// long double
console.log("strtold('2.5') =", strtold("2.5", null), " sqrtl(16) =", sqrtl(16));

// enums & constants
console.log("G_LOG_LEVEL_ERROR =", G_LOG_LEVEL_ERROR, " GLogLevelFlags[8] =", GLogLevelFlags[8]);

// GLib
console.log("g_get_user_name() =", interop.stringFromCString(g_get_user_name()));
const s = g_strdup_printf("%s has %d items", "list", 3);
console.log("g_strdup_printf:", interop.stringFromCString(s));
g_free(s);

// variables
console.log("stdout =", stdout, " typeof printf =", typeof printf, " 'puts' in globalThis =", 'puts' in globalThis);

// errors
try { strlen(); } catch (e) { console.log("caught:", e.message); }
try { strlen({}); } catch (e) { console.log("caught:", e.message); }

// exceptions thrown in callbacks are reported and the callback returns 0
qsort(new Int32Array([2, 1]), 2, 4, () => { throw new Error("boom from comparator"); });

// GLib main loop with JS callbacks
const loop = g_main_loop_new(null, 0);
let ticks = 0;
g_timeout_add(10, (data) => {
  ticks++;
  console.log("tick", ticks);
  if (ticks === 3) {
    g_main_loop_quit(loop);
    return 0; // G_SOURCE_REMOVE
  }
  return 1; // G_SOURCE_CONTINUE
}, null);
g_main_loop_run(loop);
g_main_loop_unref(loop);
console.log("main loop done after", ticks, "ticks");
