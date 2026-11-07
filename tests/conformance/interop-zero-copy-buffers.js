const { eq } = require('./harness');

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
