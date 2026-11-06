const { eq } = require('./harness');

eq(typeof g_get_user_name(), "string", "const char* returns are strings");
const dup = g_strdup("owned");
eq(dup instanceof interop.Pointer, true, "char* returns stay Pointers");
eq(interop.stringFromCString(dup), "owned", "read owned string");
g_free(dup);

// interop.cstring outlives the call, unlike temporary strings
const src = interop.cstring("77tail");
const end = new interop.Reference(interop.types.pointer, null);
eq(strtol(src, end, 10), 77, "strtol");
eq(interop.stringFromCString(end.value), "tail", "endptr into a cstring");
