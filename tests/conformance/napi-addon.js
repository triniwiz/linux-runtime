// built by the runner: runtime/tests/fixtures/napi/build/addon.node
const { eq } = require('./harness');
const addon = require('../../runtime/tests/fixtures/napi/build/addon.node');

eq(addon.add(2, 3.5), 5.5, 'numbers');
eq(addon.greet('napi'), 'hello napi', 'strings');
eq(addon.callWith21((x) => x * 2), 42, 'calls into JS');
let error;
try { addon.throws(); } catch (e) { error = [e.message, e.code]; }
eq(error, ['boom from C', 'E_BOOM'], 'errors with codes');
let jsError;
try { addon.callWith21(() => { throw new Error('from js'); }); } catch (e) { jsError = e.message; }
eq(jsError, 'from js', 'JS exceptions propagate');
const c = new addon.Counter();
c.inc(); c.inc();
eq([c.inc(), c.value, c instanceof addon.Counter], [3, 3, true], 'classes');
const ext = addon.externalBuffer();
const before = new TextDecoder().decode(ext);
addon.mutateShared();
eq([before, new TextDecoder().decode(ext)], ['zero-copy', 'Zero-copy'], 'external buffers');
const bytes = new Uint8Array([1, 2, 3]);
eq([addon.sumBytes(bytes), bytes[0]], [6, 99], 'typed arrays');
addon.store({ kept: true });
eq(addon.load(), { kept: true }, 'references');
eq([addon.describe({ a: 1, b: 2 }), addon.describe('x')], [2, -4], 'type checks');

let doubled;
addon.doubleAsync(21).then((v) => (doubled = v));
const fromThread = [];
addon.fromThread((v) => fromThread.push(v));
setTimeout(function check() {
  if (fromThread.length < 3 || doubled === undefined) return setTimeout(check, 5);
  eq([doubled, fromThread], [42, [1, 2, 3]], 'async work and threadsafe functions');
}, 5);
