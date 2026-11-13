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
