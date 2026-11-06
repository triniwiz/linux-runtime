const { eq } = require('./harness');

const cmp = (a, b) => new interop.Reference(interop.types.int32, a).value - new interop.Reference(interop.types.int32, b).value;
const fp = interop.callback(cmp, { returns: interop.types.int32, args: [interop.types.pointer, interop.types.pointer] });
eq(fp instanceof interop.Pointer, true, "callback pointer");
const nums = new Int32Array([3, 1, 2]);
qsort(nums, 3, 4, fp);
eq(Array.from(nums), [1, 2, 3], "explicit callback");
eq(interop.release(cmp), 1, "release frees the closure");
eq(interop.release(cmp), 0, "nothing left");
