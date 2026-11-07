// expect: boom from comparator
qsort(new Int32Array([2, 1]), 2, 4, () => { throw new Error('boom from comparator'); });
