//! End to end tests against the metadata produced by
//! `metadata-generator/tests/test-mdg-executable.sh` (skipped when missing).

use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;

fn runtime() -> Option<Runtime> {
    let path = std::env::var_os("NS_TEST_METADATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../metadata-generator/tests/TestOutput/metadata-{}.bin",
                std::env::consts::ARCH
            ))
        });
    if !path.exists() {
        eprintln!("skipping: {} not found", path.display());
        return None;
    }
    Some(Runtime::new(MetaFile::open(path).unwrap()).unwrap())
}

const PRELUDE: &str = r#"
function eq(actual, expected, what) {
  const str = (v) => JSON.stringify(v, (_, x) => (typeof x === 'bigint' ? `${x}n` : x));
  const a = str(actual), e = str(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
"#;

/// Tests using GLib's global default main context (g_idle_add,
/// g_main_loop_new(null)) can't run in parallel.
static DEFAULT_CONTEXT: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn default_context() -> std::sync::MutexGuard<'static, ()> {
    DEFAULT_CONTEXT.lock().unwrap_or_else(|e| e.into_inner())
}

fn run(source: &str) {
    let Some(mut rt) = runtime() else { return };
    rt.run_script(&format!("{PRELUDE}\n{source}"), "test.js")
        .unwrap();
}

#[test]
fn primitives_and_strings() {
    run(r#"
      eq(strlen("hello"), 5, "strlen");
      eq(abs(-3), 3, "abs");
      eq(atoll("9007199254740993"), 9007199254740993n, "64-bit results beyond 2^53 are BigInts");
      eq(fabs(-1.5), 1.5, "fabs");
      eq(sqrtl(2.25), 1.5, "long double");
      eq(toupper("a".charCodeAt(0)), "A".charCodeAt(0), "toupper");
      const p = getenv("PATH");
      eq(p instanceof interop.Pointer, true, "char* returns a Pointer");
      eq(interop.stringFromCString(p).length > 0, true, "stringFromCString");
      eq(getenv("NS_DEFINITELY_NOT_SET"), null, "NULL is null");
    "#);
}

#[test]
fn variadic_functions() {
    run(r#"
      const buf = interop.alloc(64);
      const n = snprintf(buf, 64, "%s|%d|%.2f|%lld|%c", "x", -7, 0.5, 1234567890123, 65);
      eq(interop.stringFromCString(buf), "x|-7|0.50|1234567890123|A", "snprintf");
      eq(n, 25, "snprintf result");
    "#);
}

#[test]
fn references_and_structs() {
    run(r#"
      const src = strdup("42rest");
      const end = new interop.Reference(interop.types.pointer, null);
      eq(strtol(src, end, 10), 42, "strtol");
      eq(interop.stringFromCString(end.value), "rest", "out param");
      free(src);

      const r = new interop.Reference(interop.types.int32, 5);
      eq(r.value, 5, "reference value");
      r.value = 7;
      eq(r.value, 7, "reference write");

      eq(div(17, 5), { quot: 3, rem: 2 }, "struct return");
      const t = new tm({ tm_year: 100, tm_mday: 1 });
      eq([t.tm_year, t.tm_mday, t.tm_sec], [100, 1, 0], "struct constructor");
      const secs = new interop.Reference(interop.types.long, 0);
      const out = new tm();
      gmtime_r(secs, out);
      eq([out.tm_year, out.tm_mon, out.tm_mday], [70, 0, 1], "struct pointer copy back");
      eq(interop.sizeof(tm), 56, "sizeof struct");
      eq(interop.sizeof(interop.types.int64), 8, "sizeof type");
    "#);
}

#[test]
fn callbacks() {
    run(r#"
      const nums = new Int32Array([4, 2, 8, 6]);
      let calls = 0;
      qsort(nums, 4, 4, (a, b) => {
        calls++;
        return new interop.Reference(interop.types.int32, a).value - new interop.Reference(interop.types.int32, b).value;
      });
      eq(Array.from(nums), [2, 4, 6, 8], "qsort");
      eq(calls > 0, true, "comparator called");

      // C functions can be passed where function pointers are expected
      const words = [strdup("pear"), strdup("apple"), strdup("fig")];
      const arr = interop.alloc(8 * words.length);
      const view = new BigUint64Array(interop.bufferFromData(arr, 8 * words.length));
      words.forEach((w, i) => (view[i] = w.toBigInt()));
      qsort(arr, words.length, 8, (a, b) =>
        strcmp(new interop.Reference(interop.types.pointer, a).value, new interop.Reference(interop.types.pointer, b).value));
      eq(Array.from(view, (v) => interop.stringFromCString(new interop.Pointer(v))), ["apple", "fig", "pear"], "string sort");
      words.forEach((w) => free(w));
    "#);
}

#[test]
fn constants_and_enums() {
    run(r#"
      eq(G_LOG_LEVEL_WARNING, 16, "enum member");
      eq(GLogLevelFlags.G_LOG_LEVEL_WARNING, 16, "enum object");
      eq(GLogLevelFlags[16], "G_LOG_LEVEL_WARNING", "reverse mapping");
      eq(typeof printf, "function", "lazy function global");
      eq(Object.getOwnPropertyDescriptor(globalThis, "printf") !== undefined, true, "memoized");
      eq(typeof Math.max, "function", "builtins still win");
    "#);
}

#[test]
fn glib_main_loop() {
    let _guard = default_context();
    run(r#"
      const loop = g_main_loop_new(null, 0);
      let n = 0;
      g_idle_add(() => { n++; if (n === 5) { g_main_loop_quit(loop); return 0; } return 1; }, null);
      g_main_loop_run(loop);
      g_main_loop_unref(loop);
      eq(n, 5, "idle callbacks");
    "#);
}

#[test]
fn uncaught_errors_are_reported() {
    let Some(mut rt) = runtime() else { return };
    let err = rt.run_script("strlen(1, 2)", "err.js").unwrap_err();
    assert!(err.contains("expects 1 argument"), "{err}");
}

#[test]
fn live_struct_views() {
    run(r#"
      // nested fields alias the parent's memory
      const it = new itimerval();
      it.it_value.tv_sec = 5;
      it.it_interval = { tv_sec: 2, tv_usec: 7 };
      eq(it.toJSON(), { it_interval: { tv_sec: 2, tv_usec: 7 }, it_value: { tv_sec: 5, tv_usec: 0 } }, "nested writes");

      // a Reference over existing memory gives a live view
      const mem = interop.alloc(interop.sizeof(tm));
      const view = new interop.Reference(tm, mem).value;
      view.tm_mday = 21;
      eq(new interop.Reference(interop.types.int32, mem.add(12)).value, 21, "view writes memory");

      // views passed to T* are passed by address (C writes are visible)
      const t = new tm();
      gmtime_r(new interop.Reference(interop.types.long, 86400), t);
      eq(t.tm_mday, 2, "pointer aliasing");
      eq(JSON.stringify(div(7, 2)), '{"quot":3,"rem":1}', "toJSON");
    "#);
}

#[test]
fn bitfields_match_the_c_layout() {
    run(r#"
      // struct _GHookList { gulong seq_id; guint hook_size : 16; guint is_setup : 1; ... }
      const hooks = new _GHookList();
      hooks.hook_size = 300;
      eq([hooks.hook_size, hooks.is_setup], [300, 0], "bitfield write/read");
      g_hook_list_init(hooks, 64);
      eq([hooks.hook_size, hooks.is_setup], [64, 1], "bitfields written by C");
      g_hook_list_clear(hooks);
    "#);
}

#[test]
fn strings_ownership() {
    run(r#"
      eq(typeof g_get_user_name(), "string", "const char* returns are strings");
      const dup = g_strdup("owned");
      eq(dup instanceof interop.Pointer, true, "char* returns stay Pointers");
      eq(interop.stringFromCString(dup), "owned", "read owned string");
      g_free(dup);

      // interop.cstring outlives the call, unlike temporary strings
      const src = interop.cstring("77tail");
      const end = new interop.Reference(interop.types.pointer, null);
      eq(strtol(src, end, 10), 77, "strtol");
      eq(interop.stringFromCString(end.value), "tail", "endptr into a cstring");
    "#);
}

#[test]
fn explicit_callbacks_and_release() {
    run(r#"
      const cmp = (a, b) => new interop.Reference(interop.types.int32, a).value - new interop.Reference(interop.types.int32, b).value;
      const fp = interop.callback(cmp, { returns: interop.types.int32, args: [interop.types.pointer, interop.types.pointer] });
      eq(fp instanceof interop.Pointer, true, "callback pointer");
      const nums = new Int32Array([3, 1, 2]);
      qsort(nums, 3, 4, fp);
      eq(Array.from(nums), [1, 2, 3], "explicit callback");
      eq(interop.release(cmp), 1, "release frees the closure");
      eq(interop.release(cmp), 0, "nothing left");
    "#);
}

fn run_loop(source: &str) -> Result<(), String> {
    let Some(mut rt) = runtime() else {
        return Ok(());
    };
    rt.run_script(&format!("{PRELUDE}\n{source}"), "loop.js")?;
    rt.run_event_loop()
}

#[test]
fn timers_and_microtasks() {
    run_loop(
        r#"
      const order = [];
      setTimeout(() => order.push("t10"), 10);
      setTimeout(() => order.push("t0"), 0);
      Promise.resolve().then(() => order.push("micro"));
      let n = 0;
      const iv = setInterval(() => { if (++n === 3) clearInterval(iv); }, 1);
      clearTimeout(setTimeout(() => order.push("cancelled"), 1));
      setTimeout(() => {
        eq(order, ["micro", "t0", "t10"], "order");
        eq(n, 3, "interval");
      }, 30);
    "#,
    )
    .unwrap();
}

#[test]
fn promises_resolve_inside_native_main_loops() {
    let _guard = default_context();
    run_loop(
        r#"
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
    "#,
    )
    .unwrap();
}

#[test]
fn callbacks_from_other_threads_run_on_the_js_thread() {
    run_loop(
        r#"
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
    "#,
    )
    .unwrap();
}

#[test]
fn unhandled_rejections_fail() {
    assert!(run_loop("Promise.reject(new Error('x'))").is_err());
    assert!(run_loop("setTimeout(() => Promise.reject(new Error('y')), 1)").is_err());
    assert!(run_loop("Promise.reject(new Error('z')).catch(() => {})").is_ok());
}

#[test]
fn zero_copy_buffers() {
    run(r#"
      // C memory seen from JS without copies, and JS memory seen from C
      const mem = g_malloc0(8);
      const bytes = new Uint8Array(interop.bufferFromData(mem, 8, g_free)); // owned: g_free on GC
      memset(mem, 7, 4);
      eq(Array.from(bytes), [7, 7, 7, 7, 0, 0, 0, 0], 'C writes visible in JS');
      bytes[5] = 42;
      eq(new interop.Reference(interop.types.uint8, mem.add(5)).value, 42, 'JS writes visible in C');

      const js = new Float64Array([1.5, 2.5]);
      const view = new Float64Array(interop.bufferFromData(interop.handleof(js), 16));
      view[0] = 9;
      eq(js[0], 9, 'same memory both ways');
      eq(interop.handleof(js).toBigInt(), interop.handleof(view).toBigInt(), 'same address');
    "#);
}

#[test]
fn gvariant_dict_keys_stay_data() {
    run(r#"
      const v = g_variant_parse(null, '{"__proto__": <{"polluted": <1>}>, "a": <2>}', null, null, null);
      const o = g_variant_unpack(v);
      g_variant_unref(v);
      eq(Object.getPrototypeOf(o) === Object.prototype, true, "prototype kept");
      eq(Object.keys(o), ["__proto__", "a"], "keys");
      eq(o.polluted, undefined, "no inherited keys");
    "#);
}
