//! End to end tests against the metadata produced by
//! `metadata-generator/tests/test-mdg-executable.sh` (skipped when missing).

use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;

fn runtime() -> Option<Runtime> {
    let path = std::env::var_os("NS_TEST_METADATA").map(PathBuf::from).unwrap_or_else(|| {
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

fn run(source: &str) {
    let Some(mut rt) = runtime() else { return };
    rt.run_script(&format!("{PRELUDE}\n{source}"), "test.js").unwrap();
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
