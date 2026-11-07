const { eq } = require('./harness');

const target = new EventTarget();
const seen = [];
target.addEventListener('ping', (e) => seen.push(e.detail), { once: true });
target.dispatchEvent(new CustomEvent('ping', { detail: 1 }));
target.dispatchEvent(new CustomEvent('ping', { detail: 2 }));
eq(seen, [1], 'once');

const controller = new AbortController();
let aborted = null;
controller.signal.addEventListener('abort', () => (aborted = controller.signal.reason.name));
controller.abort();
eq([controller.signal.aborted, aborted], [true, 'AbortError'], 'abort');

const timeout = AbortSignal.timeout(5);
setTimeout(() => eq(timeout.reason?.name, 'TimeoutError', 'timeout signal'), 30);
