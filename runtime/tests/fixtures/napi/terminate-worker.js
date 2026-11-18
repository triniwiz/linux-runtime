onmessage = (e) => {
  const addon = require(e.data);
  // terminate() lands in the loop below, outside a microtask checkpoint: the call stays queued.
  queueMicrotask(() => addon.add(1, 2));
  postMessage('spinning');
  for (;;) {}
};
