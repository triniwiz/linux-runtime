// Worker, MessageChannel/MessagePort and the worker global scope.
(function (natives) {
  'use strict';
  const g = globalThis;
  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });

  const workers = new Map();
  const selfTarget = new EventTarget();

  natives.workerSetDispatch((id, type, data) => {
    const target = id === 0 ? selfTarget : workers.get(id);
    if (!target) return;
    if (type === 'exit') {
      workers.delete(id);
      return;
    }
    const event = type === 'error' ? Object.assign(new Event('error'), { message: data }) : new MessageEvent(type, { data });
    target.dispatchEvent(event);
    if (id === 0 && typeof g[`on${type}`] === 'function') g[`on${type}`](event);
  });

  class Worker extends EventTarget {
    #id;
    constructor(url, options = {}) {
      super();
      this.name = options.name || '';
      this.#id = natives.workerSpawn(url instanceof URL ? url.href : String(url));
      workers.set(this.#id, this);
    }
    postMessage(data) { natives.workerPost(this.#id, data); }
    terminate() { natives.workerTerminate(this.#id); }
  }
  define('Worker', Worker);

  // same-thread channels
  class MessagePort extends EventTarget {
    _other = null;
    #closed = false;
    postMessage(data) {
      const other = this._other;
      if (this.#closed || !other) return;
      const copy = structuredClone(data);
      setTimeout(() => {
        const event = new MessageEvent('message', { data: copy });
        other.dispatchEvent(event);
      }, 0);
    }
    start() {}
    close() { this.#closed = true; }
  }
  class MessageChannel {
    constructor() {
      this.port1 = new MessagePort();
      this.port2 = new MessagePort();
      this.port1._other = this.port2;
      this.port2._other = this.port1;
    }
  }
  define('MessagePort', MessagePort);
  define('MessageChannel', MessageChannel);

  if (natives.isWorker) {
    define('postMessage', (data) => natives.workerPost(0, data));
    define('close', () => natives.workerClose());
    define('addEventListener', (...a) => selfTarget.addEventListener(...a));
    define('removeEventListener', (...a) => selfTarget.removeEventListener(...a));
    define('dispatchEvent', (e) => selfTarget.dispatchEvent(e));
  }
});
