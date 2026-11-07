const { eq } = require('./harness');

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
