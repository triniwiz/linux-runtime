const { eq } = require('./harness');

eq(G_LOG_LEVEL_WARNING, 16, "enum member");
eq(GLogLevelFlags.G_LOG_LEVEL_WARNING, 16, "enum object");
eq(GLogLevelFlags[16], "G_LOG_LEVEL_WARNING", "reverse mapping");
eq(typeof printf, "function", "lazy function global");
eq(Object.getOwnPropertyDescriptor(globalThis, "printf") !== undefined, true, "memoized");
eq(typeof Math.max, "function", "builtins still win");
