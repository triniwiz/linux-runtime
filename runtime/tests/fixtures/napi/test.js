const addon = require('./build/addon.node');
const results = (globalThis.napiResults = {});
results.add = addon.add(2, 3.5);
results.greet = addon.greet('napi');
results.callback = addon.callWith21((x) => x * 2);
try { addon.throws(); } catch (e) { results.error = [e.message, e.code]; }
try { addon.callWith21(() => { throw new Error('from js'); }); } catch (e) { results.jsError = e.message; }
const c = new addon.Counter();
c.inc(); c.inc();
results.counter = [c.inc(), c.value, c instanceof addon.Counter];
const ext = addon.externalBuffer();
const before = new TextDecoder().decode(ext);
addon.mutateShared();
results.zeroCopy = [before, new TextDecoder().decode(ext)];
const bytes = new Uint8Array([1, 2, 3]);
results.sum = [addon.sumBytes(bytes), bytes[0]];
addon.store({ kept: true });
results.reference = addon.load();
results.describe = [addon.describe({ a: 1, b: 2 }), addon.describe('x')];
addon.doubleAsync(21).then((v) => (results.async = v));
const fromThread = [];
addon.fromThread((v) => fromThread.push(v));
setTimeout(function check() {
  if (fromThread.length < 3 || results.async === undefined) return setTimeout(check, 5);
  results.thread = fromThread;
}, 5);
