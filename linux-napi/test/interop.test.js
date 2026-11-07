// Run with NS_METADATA pointing at the metadata to test against (see README).
const test = require('node:test');
const assert = require('node:assert');
const ns = require('..');
const { interop } = ns;

test('C functions, strings and 64-bit results', () => {
  assert.strictEqual(ns.abs(-3), 3);
  assert.strictEqual(ns.strlen('hello'), 5);
  assert.strictEqual(ns.atoll('9007199254740993'), 9007199254740993n);
  assert.strictEqual(typeof ns.does_not_exist, 'undefined');
  assert.strictEqual('strlen' in ns, true);
});

test('resolved declarations are memoized on the module, not globalThis', () => {
  assert.strictEqual(ns.abs, ns.abs);
  assert.strictEqual(globalThis.abs, undefined);
});

test('structs, Pointers and References', () => {
  const { quot, rem } = ns.div(17, 5);
  assert.deepStrictEqual([quot, rem], [3, 2]);
  const p = interop.alloc(8);
  assert.ok(p instanceof interop.Pointer);
  const ref = new interop.Reference(interop.types.int32, p);
  ref.value = 42;
  assert.strictEqual(new interop.Reference(interop.types.int32, p).value, 42);
  interop.free(p);
});

test('JS functions as C callbacks', () => {
  const nums = new Int32Array([3, 1, 2]);
  const int = (p) => new interop.Reference(interop.types.int32, p).value;
  ns.qsort(nums, nums.length, 4, (a, b) => int(a) - int(b));
  assert.deepStrictEqual(Array.from(nums), [1, 2, 3]);
});

test('bad arguments throw', () => {
  assert.throws(() => ns.strlen({}), /expected a string/);
});

test('pumpGLib dispatches ready sources', () => {
  let ran = 0;
  ns.g_idle_add(() => (ran++, 0), null);
  assert.strictEqual(ns.pumpGLib(), true);
  assert.strictEqual(ran, 1);
});

test('runGLib dispatches GLib timeouts at their own pace', async () => {
  const times = [];
  ns.g_timeout_add(8, () => (times.push(performance.now()), times.length < 5 ? 1 : 0), null);
  ns.runGLib();
  while (times.length < 5) await new Promise((r) => setTimeout(r, 1));
  ns.stopGLib();
  const gaps = times.slice(1).map((t, i) => t - times[i]);
  assert.ok(gaps.every((g) => g >= 7 && g < 30), `gaps: ${gaps}`);
});

test('promises resolved in GLib callbacks run right after them', async () => {
  ns.runGLib();
  const order = [];
  await new Promise((resolve) => {
    ns.g_idle_add(() => {
      Promise.resolve().then(() => order.push('microtask'));
      order.push('callback');
      setImmediate(resolve);
      return 0;
    }, null);
  });
  ns.stopGLib();
  assert.deepStrictEqual(order, ['callback', 'microtask']);
});
