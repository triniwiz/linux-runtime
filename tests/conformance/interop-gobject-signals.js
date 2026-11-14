const { eq } = require('./harness');

// a closure per connection: disconnecting one leaves the others
const action = g_simple_action_new('go', null);
const calls = [];
const fn = (self, parameter, data) => calls.push([self === action, data]);
const first = g_signal_connect(action, 'activate', fn);
g_signal_connect(action, 'activate', fn, new interop.Pointer(42));
g_signal_handler_disconnect(action, first);
g_action_activate(action, null);
eq(calls.length, 1, 'the other connection of a function survives a disconnect');
eq([calls[0][0], calls[0][1].toNumber()], [true, 42], 'instance and user data');
eq(interop.release(fn), 0, 'interop.release leaves connected handlers alone');

// released when disconnected, or when the instance is finalized
if (typeof gc === 'function' && typeof FinalizationRegistry === 'function') {
  const released = [];
  const registry = new FinalizationRegistry((name) => released.push(name));
  const controls = [];
  (() => {
    const a = g_simple_action_new('a', null);
    g_object_unref(a);
    const disconnected = () => {};
    registry.register(disconnected, 'disconnected');
    g_signal_handler_disconnect(a, g_signal_connect(a, 'activate', disconnected));
    const b = g_simple_action_new('b', null);
    g_object_unref(b);
    const finalized = () => {};
    registry.register(finalized, 'finalized');
    g_signal_connect(b, 'activate', finalized);
    // controls: whether this engine's gc() collects released callbacks (JSC's conservative
    // GC may not), and ones whose closures were made in an earlier turn (QuickJS doesn't)
    const sig = { returns: interop.types.void, args: [] };
    const now = () => {};
    registry.register(now, 'control');
    interop.callback(now, sig);
    interop.release(now);
    const later = () => {};
    registry.register(later, 'control later');
    interop.callback(later, sig);
    controls.push(later);
  })();
  const settle = (rounds, then) => {
    gc();
    setTimeout(() => (rounds > 1 ? settle(rounds - 1, then) : then()), 10);
  };
  setTimeout(() => {
    interop.release(controls.pop());
    settle(4, () => {
      if (released.includes('control')) eq(released.includes('disconnected'), true, 'disconnected handler collected');
      if (released.includes('control later')) eq(released.includes('finalized'), true, "finalized instance's handler collected");
    });
  }, 10);
}

// a handler capturing its own instance doesn't keep it alive, and lives as long as it
if (typeof gc === 'function') {
  let finalized = 0;
  const notify = interop.callback(() => finalized++, { returns: interop.types.void, args: [interop.types.pointer, interop.types.pointer] });
  const created = (name) => {
    const a = g_simple_action_new(name, null);
    g_object_unref(a);
    return a;
  };
  (() => {
    const a = created('self');
    g_object_weak_ref(a, notify, null);
    g_signal_connect(a, 'activate', () => a);
  })();
  const kept = created('kept');
  let keptCalls = 0;
  g_signal_connect(kept, 'activate', () => keptCalls++);
  const store = g_list_store_new(g_simple_action_get_type());
  let heldCalls = 0;
  (() => {
    const a = created('held');
    g_signal_connect(a, 'activate', () => (heldCalls++, a));
    g_list_store_append(store, a);
  })();
  const settle = (rounds, then) => {
    gc();
    setTimeout(() => (rounds > 1 ? settle(rounds - 1, then) : then()), 10);
  };
  settle(4, () => {
    eq(finalized, 1, 'a handler capturing its instance is collected with it');
    g_action_activate(kept, null);
    eq(keptCalls, 1, 'a handler lives while JS holds its instance');
    const held = g_list_model_get_object(store, 0);
    g_action_activate(held, null);
    eq(heldCalls, 1, 'a handler lives while C holds its instance');
    g_object_unref(held);
  });
}
