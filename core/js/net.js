// fetch, Headers, Request, Response, Blob, WebSocket.
(function (natives) {
  'use strict';
  const g = globalThis;
  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();

  const toBytes = (data) => {
    if (data == null) return new Uint8Array(0);
    if (typeof data === 'string') return encoder.encode(data);
    if (data instanceof ArrayBuffer) return new Uint8Array(data.slice(0));
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength));
    if (data instanceof Blob) return data._bytes();
    if (data instanceof URLSearchParams) return encoder.encode(data.toString());
    return encoder.encode(String(data));
  };

  class Blob {
    #bytes;
    #type;
    constructor(parts = [], options = {}) {
      const chunks = parts.map(toBytes);
      this.#bytes = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
      let offset = 0;
      for (const c of chunks) {
        this.#bytes.set(c, offset);
        offset += c.length;
      }
      this.#type = String(options.type || '').toLowerCase();
    }
    _bytes() { return this.#bytes.slice(); }
    get size() { return this.#bytes.length; }
    get type() { return this.#type; }
    slice(start, end, type) { return new Blob([this.#bytes.slice(start, end)], { type }); }
    async arrayBuffer() { return this.#bytes.slice().buffer; }
    async bytes() { return this.#bytes.slice(); }
    async text() { return decoder.decode(this.#bytes); }
    get [Symbol.toStringTag]() { return 'Blob'; }
  }
  define('Blob', Blob);

  class Headers {
    #map = new Map(); // lower-case name -> [values]
    constructor(init) {
      if (init instanceof Headers) init.forEach((v, k) => this.append(k, v));
      else if (Array.isArray(init)) init.forEach(([k, v]) => this.append(k, v));
      else if (init) Object.keys(init).forEach((k) => this.append(k, init[k]));
    }
    append(k, v) {
      const key = String(k).toLowerCase();
      this.#map.set(key, [...(this.#map.get(key) || []), String(v).trim()]);
    }
    set(k, v) { this.#map.set(String(k).toLowerCase(), [String(v).trim()]); }
    get(k) {
      const v = this.#map.get(String(k).toLowerCase());
      return v ? v.join(', ') : null;
    }
    getSetCookie() { return [...(this.#map.get('set-cookie') || [])]; }
    has(k) { return this.#map.has(String(k).toLowerCase()); }
    delete(k) { this.#map.delete(String(k).toLowerCase()); }
    forEach(fn, thisArg) { for (const [k, v] of this.entries()) fn.call(thisArg, v, k, this); }
    *entries() {
      for (const k of [...this.#map.keys()].sort()) yield [k, this.get(k)];
    }
    keys() { return Array.from(this.entries(), ([k]) => k)[Symbol.iterator](); }
    values() { return Array.from(this.entries(), ([, v]) => v)[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    _pairs() {
      const out = [];
      for (const [k, values] of this.#map) for (const v of values) out.push([k, v]);
      return out;
    }
  }
  define('Headers', Headers);

  class Body {
    #bytes;
    #used = false;
    _setBody(body, headers) {
      this.#bytes = body == null ? null : toBytes(body);
      if (body != null && !headers.has('content-type')) {
        if (typeof body === 'string') headers.set('content-type', 'text/plain;charset=UTF-8');
        else if (body instanceof URLSearchParams) headers.set('content-type', 'application/x-www-form-urlencoded;charset=UTF-8');
        else if (body instanceof Blob && body.type) headers.set('content-type', body.type);
      }
    }
    _raw() { return this.#bytes; }
    get bodyUsed() { return this.#used; }
    #consume() {
      if (this.#used) return Promise.reject(new TypeError('Body has already been consumed'));
      this.#used = true;
      return Promise.resolve(this.#bytes || new Uint8Array(0));
    }
    arrayBuffer() { return this.#consume().then((b) => b.slice().buffer); }
    bytes() { return this.#consume().then((b) => b.slice()); }
    text() { return this.#consume().then((b) => decoder.decode(b)); }
    json() { return this.text().then(JSON.parse); }
    blob() { return this.#consume().then((b) => new Blob([b], { type: this.headers.get('content-type') || '' })); }
  }

  class Request extends Body {
    constructor(input, init = {}) {
      super();
      const from = input instanceof Request ? input : null;
      this.url = from ? from.url : new URL(String(input)).href;
      this.method = String(init.method || from?.method || 'GET').toUpperCase();
      this.headers = new Headers(init.headers || from?.headers);
      this.signal = init.signal || from?.signal || null;
      this.redirect = init.redirect || from?.redirect || 'follow';
      const body = init.body !== undefined ? init.body : from?._raw();
      if (body != null && (this.method === 'GET' || this.method === 'HEAD')) {
        throw new TypeError('Request with GET/HEAD method cannot have body');
      }
      this._setBody(body, this.headers);
    }
    clone() { return new Request(this); }
  }
  define('Request', Request);

  class Response extends Body {
    constructor(body = null, init = {}) {
      super();
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? '';
      this.headers = new Headers(init.headers);
      this.url = init.url || '';
      this.redirected = !!init.redirected;
      this.type = init.type || 'default';
      this._setBody(body, this.headers);
    }
    get ok() { return this.status >= 200 && this.status < 300; }
    clone() {
      return new Response(this._raw(), { status: this.status, statusText: this.statusText, headers: this.headers, url: this.url });
    }
    static json(data, init = {}) {
      const headers = new Headers(init.headers);
      if (!headers.has('content-type')) headers.set('content-type', 'application/json');
      return new Response(JSON.stringify(data), { ...init, headers });
    }
    static error() { return new Response(null, { status: 0, type: 'error' }); }
    static redirect(url, status = 302) { return new Response(null, { status, headers: { location: String(url) } }); }
  }
  define('Response', Response);

  define('fetch', (input, init) => {
    let request;
    try {
      request = new Request(input, init);
    } catch (e) {
      return Promise.reject(e);
    }
    const { signal } = request;
    if (signal?.aborted) return Promise.reject(signal.reason);
    return new Promise((resolve, reject) => {
      const onAbort = () => reject(signal.reason);
      signal?.addEventListener('abort', onAbort, { once: true });
      natives
        .httpRequest(request.method, request.url, request.headers._pairs(), request._raw(), request.redirect)
        .then((r) => {
          signal?.removeEventListener('abort', onAbort);
          if (signal?.aborted) return;
          if (request.redirect === 'error' && r.status >= 300 && r.status < 400) {
            throw new TypeError('fetch failed: redirect mode is "error"');
          }
          resolve(new Response(r.body, { status: r.status, statusText: r.statusText, headers: r.headers, url: r.url, redirected: r.url !== request.url }));
        })
        .catch(reject);
    });
  });

  class MessageEvent extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.data = init.data;
      this.origin = init.origin || '';
    }
  }
  class CloseEvent extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.code = init.code || 0;
      this.reason = init.reason || '';
      this.wasClean = !!init.wasClean;
    }
  }
  define('MessageEvent', MessageEvent);
  define('CloseEvent', CloseEvent);

  const sockets = new Map();
  natives.wsSetDispatch((id, type, a, b, c) => {
    const ws = sockets.get(id);
    if (ws) ws._event(type, a, b, c);
  });

  class WebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;
    #id;
    #url;
    #state = 0;
    #protocol = '';
    binaryType = 'blob';
    constructor(url, protocols = []) {
      super();
      const u = new URL(String(url));
      if (u.protocol === 'http:') u.protocol = 'ws:';
      if (u.protocol === 'https:') u.protocol = 'wss:';
      if (u.protocol !== 'ws:' && u.protocol !== 'wss:') throw new DOMException(`Invalid URL scheme ${u.protocol}`, 'SyntaxError');
      this.#url = u.href;
      this.#id = natives.wsConnect(this.#url, typeof protocols === 'string' ? [protocols] : protocols);
      sockets.set(this.#id, this);
    }
    get url() { return this.#url; }
    get readyState() { return this.#state; }
    get protocol() { return this.#protocol; }
    get extensions() { return ''; }
    get bufferedAmount() { return 0; }
    send(data) {
      if (this.#state === 0) throw new DOMException('WebSocket is still connecting', 'InvalidStateError');
      if (this.#state !== 1) return;
      natives.wsSend(this.#id, typeof data === 'string' ? data : toBytes(data));
    }
    close(code = 1000, reason = '') {
      if (this.#state >= 2) return;
      this.#state = 2;
      natives.wsClose(this.#id, code, reason);
    }
    _event(type, a, b, c) {
      switch (type) {
        case 'open':
          this.#state = 1;
          this.#protocol = a;
          this.dispatchEvent(new Event('open'));
          break;
        case 'message':
          this.dispatchEvent(new MessageEvent('message', {
            data: typeof a === 'string' || this.binaryType === 'arraybuffer' ? a : new Blob([a]),
            origin: new URL(this.#url).origin,
          }));
          break;
        case 'error':
          this.dispatchEvent(Object.assign(new Event('error'), { message: a }));
          break;
        case 'close':
          this.#state = 3;
          sockets.delete(this.#id);
          this.dispatchEvent(new CloseEvent('close', { code: a, reason: b, wasClean: c }));
          break;
      }
    }
  }
  define('WebSocket', WebSocket);
});
