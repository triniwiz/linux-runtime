const { eq } = require('./harness');

// typedef struct _GList GList; ... struct _GList { ... };
// the typedef is the struct's name, its tag the same constructor
eq(typeof GList, 'function', 'struct by typedef name');
eq(_GList === GList, true, 'struct tag is the same constructor');
eq(typeof GValue, 'function', 'GValue by typedef name');
eq(interop.sizeof(new _GList()), interop.sizeof(GList), 'same layout');

// a record from the tag passes where C takes the typedef
const value = new _GValue();
g_value_init(value, BigInt(5 << 2)); // G_TYPE_BOOLEAN
g_value_set_boolean(value, 1);
eq(g_value_get_boolean(value), 1, 'GValue through its tag');
g_value_unset(value);
