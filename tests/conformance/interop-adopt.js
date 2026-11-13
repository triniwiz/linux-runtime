const { eq } = require('./harness');

const throws = (f) => { try { f(); return false; } catch { return true; } };

// same memory, a new Pointer
const mem = g_malloc0(16);
const adopted = interop.adopt(mem, g_free);
eq(adopted instanceof interop.Pointer, true, 'returns a Pointer');
eq(adopted.equals(mem), true, 'same address');
eq(interop.adopt(null), null, 'NULL stays null');

// the garbage collector owns it now
interop.free(adopted); // no-op
eq(throws(() => interop.adopt(adopted)), true, 'adopting twice throws');
eq(throws(() => interop.adopt(interop.alloc(8))), true, 'GC-owned memory throws');
eq(throws(() => interop.adopt(g_malloc0(8), () => {})), true, 'free must be native');

// default free: libc free
interop.adopt(malloc(32));

// collected: the free function runs from the event loop
if (typeof gc === 'function') {
  const G_TYPE_OBJECT = BigInt(20 << 2);
  const object = g_object_new_with_properties(G_TYPE_OBJECT, 0, null, null);
  const refs = () => new interop.Reference(GObject, object).value.ref_count;
  const before = refs();
  (() => {
    interop.adopt(g_object_ref(object), g_object_unref);
  })();
  eq(refs(), before + 1, 'adopted reference');
  gc();
  setTimeout(() => {
    eq(refs(), before, 'unreffed after collection');
    g_object_unref(object);
  }, 0);
}
