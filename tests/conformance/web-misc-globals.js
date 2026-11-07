const { eq } = require('./harness');

const a = { d: new Date(0), m: new Map([[1, { x: 1 }]]), t: new Uint8Array([1, 2]) };
a.self = a;
const c = structuredClone(a);
eq([c !== a, c.self === c, c.d.getTime(), c.m.get(1).x, Array.from(c.t)], [true, true, 0, 1, [1, 2]], 'structuredClone');
eq(/^[0-9a-f-]{36}$/.test(crypto.randomUUID()), true, 'randomUUID');
eq(crypto.getRandomValues(new Uint32Array(4)).length, 4, 'getRandomValues');
const t0 = performance.now();
eq(typeof t0 === 'number' && t0 >= 0 && performance.timeOrigin > 1e12, true, 'performance');
let immediate = false;
setImmediate(() => (immediate = true));
setTimeout(() => eq(immediate, true, 'setImmediate'), 5);
