const { eq } = require('./harness');

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
