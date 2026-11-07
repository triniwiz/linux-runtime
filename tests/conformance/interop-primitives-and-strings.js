const { eq } = require('./harness');

eq(strlen("hello"), 5, "strlen");
eq(abs(-3), 3, "abs");
eq(atoll("9007199254740993"), 9007199254740993n, "64-bit results beyond 2^53 are BigInts");
eq(fabs(-1.5), 1.5, "fabs");
eq(sqrtl(2.25), 1.5, "long double");
eq(toupper("a".charCodeAt(0)), "A".charCodeAt(0), "toupper");
const p = getenv("PATH");
eq(p instanceof interop.Pointer, true, "char* returns a Pointer");
eq(interop.stringFromCString(p).length > 0, true, "stringFromCString");
eq(getenv("NS_DEFINITELY_NOT_SET"), null, "NULL is null");
