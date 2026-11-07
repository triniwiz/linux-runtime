const { eq } = require('./harness');

const buf = interop.alloc(64);
const n = snprintf(buf, 64, "%s|%d|%.2f|%lld|%c", "x", -7, 0.5, 1234567890123, 65);
eq(interop.stringFromCString(buf), "x|-7|0.50|1234567890123|A", "snprintf");
eq(n, 25, "snprintf result");
