const { eq } = require('./harness');

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
