// Web platform APIs and `process`.
(function (natives) {
  'use strict';
  const g = globalThis;
  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });

  // ---------- DOMException ----------
  class DOMException extends Error {
    constructor(message = '', name = 'Error') {
      super(message);
      Object.defineProperty(this, 'name', { value: name, configurable: true, writable: true });
    }
    get code() {
      return { AbortError: 20, TimeoutError: 23, NotSupportedError: 9, InvalidStateError: 11, DataCloneError: 25 }[this.name] || 0;
    }
  }
  define('DOMException', DOMException);

  // ---------- URL ----------
  const URL_STATE = Symbol('url');

  class URLSearchParams {
    #list = [];
    #url = null;
    constructor(init = '') {
      if (typeof init === 'string') {
        this.#parse(init);
      } else if (init && typeof init[Symbol.iterator] === 'function') {
        for (const [k, v] of init) this.#list.push([String(k), String(v)]);
      } else if (init && typeof init === 'object') {
        for (const k of Object.keys(init)) this.#list.push([k, String(init[k])]);
      }
    }
    #parse(s) {
      this.#list = [];
      for (const pair of s.replace(/^\?/, '').split('&')) {
        if (!pair) continue;
        const i = pair.indexOf('=');
        const dec = (x) => decodeURIComponent(x.replace(/\+/g, ' '));
        this.#list.push(i < 0 ? [dec(pair), ''] : [dec(pair.slice(0, i)), dec(pair.slice(i + 1))]);
      }
    }
    static _attach(params, url) {
      params.#url = url;
    }
    static _reset(params, search) {
      params.#parse(search);
    }
    #update() {
      if (this.#url) this.#url[URL_STATE].setSearch(this.toString());
    }
    get size() { return this.#list.length; }
    append(k, v) { this.#list.push([String(k), String(v)]); this.#update(); }
    delete(k, v) {
      this.#list = this.#list.filter(([a, b]) => a !== String(k) || (v !== undefined && b !== String(v)));
      this.#update();
    }
    get(k) { const e = this.#list.find(([a]) => a === String(k)); return e ? e[1] : null; }
    getAll(k) { return this.#list.filter(([a]) => a === String(k)).map(([, b]) => b); }
    has(k, v) { return this.#list.some(([a, b]) => a === String(k) && (v === undefined || b === String(v))); }
    set(k, v) {
      const i = this.#list.findIndex(([a]) => a === String(k));
      if (i < 0) this.#list.push([String(k), String(v)]);
      else {
        this.#list[i][1] = String(v);
        this.#list = this.#list.filter(([a], j) => j <= i || a !== String(k));
      }
      this.#update();
    }
    sort() { this.#list.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0)); this.#update(); }
    forEach(fn, thisArg) { for (const [k, v] of this.#list) fn.call(thisArg, v, k, this); }
    keys() { return this.#list.map(([k]) => k)[Symbol.iterator](); }
    values() { return this.#list.map(([, v]) => v)[Symbol.iterator](); }
    entries() { return this.#list.map((e) => [...e])[Symbol.iterator](); }
    [Symbol.iterator]() { return this.entries(); }
    toString() {
      const enc = (s) => encodeURIComponent(s).replace(/%20/g, '+').replace(/[!'()~]/g, (c) => '%' + c.charCodeAt(0).toString(16).toUpperCase());
      return this.#list.map(([k, v]) => `${enc(k)}=${enc(v)}`).join('&');
    }
    get [Symbol.toStringTag]() { return 'URLSearchParams'; }
  }

  class URL {
    #parts;
    #params;
    constructor(input, base) {
      const parts = natives.urlParse(String(input), base === undefined ? undefined : String(base));
      if (!parts) throw new TypeError(`Invalid URL: ${input}`);
      this.#parts = parts;
      this.#params = new URLSearchParams(parts[8]);
      URLSearchParams._attach(this.#params, this);
      Object.defineProperty(this, URL_STATE, { value: { setSearch: (q) => this.#set('search', q ? '?' + q : '', false) } });
    }
    static canParse(input, base) { return natives.urlParse(String(input), base === undefined ? undefined : String(base)) !== null; }
    static parse(input, base) { try { return new URL(input, base); } catch { return null; } }
    #set(part, value, resetParams = true) {
      const parts = part === 'href' ? natives.urlParse(String(value)) : natives.urlSet(this.#parts[0], part, String(value));
      if (!parts) {
        if (part === 'href') throw new TypeError(`Invalid URL: ${value}`);
        return;
      }
      this.#parts = parts;
      if (resetParams) URLSearchParams._reset(this.#params, parts[8]);
    }
    get href() { return this.#parts[0]; }
    set href(v) { this.#set('href', v); }
    get protocol() { return this.#parts[1]; }
    set protocol(v) { this.#set('protocol', v); }
    get username() { return this.#parts[2]; }
    set username(v) { this.#set('username', v); }
    get password() { return this.#parts[3]; }
    set password(v) { this.#set('password', v); }
    get host() { return this.#parts[4]; }
    set host(v) { this.#set('host', v); }
    get hostname() { return this.#parts[5]; }
    set hostname(v) { this.#set('hostname', v); }
    get port() { return this.#parts[6]; }
    set port(v) { this.#set('port', v); }
    get pathname() { return this.#parts[7]; }
    set pathname(v) { this.#set('pathname', v); }
    get search() { return this.#parts[8]; }
    set search(v) { this.#set('search', v); }
    get hash() { return this.#parts[9]; }
    set hash(v) { this.#set('hash', v); }
    get origin() { return this.#parts[10]; }
    get searchParams() { return this.#params; }
    toString() { return this.#parts[0]; }
    toJSON() { return this.#parts[0]; }
    get [Symbol.toStringTag]() { return 'URL'; }
  }
  define('URL', URL);
  define('URLSearchParams', URLSearchParams);

  // ---------- encoding ----------
  class TextEncoder {
    get encoding() { return 'utf-8'; }
    encode(input = '') { return natives.textEncode(String(input)); }
    encodeInto(input, dest) {
      const bytes = natives.textEncode(String(input));
      const n = Math.min(bytes.length, dest.length);
      let written = n;
      // don't cut a multi-byte sequence
      while (written > 0 && written < bytes.length && (bytes[written] & 0xc0) === 0x80) written--;
      dest.set(bytes.subarray(0, written));
      const read = new TextDecoder().decode(bytes.subarray(0, written)).length;
      return { read, written };
    }
  }
  class TextDecoder {
    #fatal;
    #ignoreBOM;
    constructor(label = 'utf-8', options = {}) {
      if (!['utf-8', 'utf8', 'unicode-1-1-utf-8'].includes(String(label).toLowerCase())) {
        throw new RangeError(`The encoding '${label}' is not supported`);
      }
      this.#fatal = !!options.fatal;
      this.#ignoreBOM = !!options.ignoreBOM;
    }
    get encoding() { return 'utf-8'; }
    get fatal() { return this.#fatal; }
    get ignoreBOM() { return this.#ignoreBOM; }
    decode(input) {
      if (input === undefined) return '';
      return natives.textDecode(input, this.#fatal);
    }
  }
  define('TextEncoder', TextEncoder);
  define('TextDecoder', TextDecoder);

  const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  define('btoa', (input) => {
    const s = String(input);
    let out = '';
    for (let i = 0; i < s.length; i += 3) {
      const a = s.charCodeAt(i), b = s.charCodeAt(i + 1), c = s.charCodeAt(i + 2);
      if (a > 255 || b > 255 || c > 255) throw new DOMException('Invalid character', 'InvalidCharacterError');
      const n = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += B64[n >> 18] + B64[(n >> 12) & 63] + (i + 1 < s.length ? B64[(n >> 6) & 63] : '=') + (i + 2 < s.length ? B64[n & 63] : '=');
    }
    return out;
  });
  define('atob', (input) => {
    const s = String(input).replace(/[\t\n\f\r ]/g, '');
    if (s.length % 4 === 1 || /[^A-Za-z0-9+/=]/.test(s) || /=[^=]/.test(s)) {
      throw new DOMException('The string to be decoded is not correctly encoded.', 'InvalidCharacterError');
    }
    let out = '';
    let bits = 0, buf = 0;
    for (const ch of s.replace(/=+$/, '')) {
      buf = (buf << 6) | B64.indexOf(ch);
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out += String.fromCharCode((buf >> bits) & 255);
      }
    }
    return out;
  });

  // ---------- events ----------
  const STOP = Symbol('stop');
  class Event {
    #type;
    #defaultPrevented = false;
    constructor(type, init = {}) {
      this.#type = String(type);
      this.bubbles = !!init.bubbles;
      this.cancelable = !!init.cancelable;
      this.composed = !!init.composed;
      this.timeStamp = natives.now();
      this.target = null;
      this.currentTarget = null;
      this[STOP] = false;
    }
    get type() { return this.#type; }
    get defaultPrevented() { return this.#defaultPrevented; }
    preventDefault() { if (this.cancelable) this.#defaultPrevented = true; }
    stopPropagation() {}
    stopImmediatePropagation() { this[STOP] = true; }
  }
  class CustomEvent extends Event {
    constructor(type, init = {}) {
      super(type, init);
      this.detail = init.detail ?? null;
    }
  }
  class EventTarget {
    #listeners = new Map();
    addEventListener(type, listener, options = {}) {
      if (!listener) return;
      const opts = typeof options === 'boolean' ? { capture: options } : options;
      const list = this.#listeners.get(type) || [];
      if (list.some((l) => l.listener === listener)) return;
      const entry = { listener, once: !!opts.once };
      list.push(entry);
      this.#listeners.set(type, list);
      opts.signal?.addEventListener('abort', () => this.removeEventListener(type, listener));
    }
    removeEventListener(type, listener) {
      const list = this.#listeners.get(type);
      if (list) this.#listeners.set(type, list.filter((l) => l.listener !== listener));
    }
    dispatchEvent(event) {
      event.target = event.currentTarget = this;
      for (const entry of [...(this.#listeners.get(event.type) || [])]) {
        if (entry.once) this.removeEventListener(event.type, entry.listener);
        try {
          if (typeof entry.listener === 'function') entry.listener.call(this, event);
          else entry.listener.handleEvent(event);
        } catch (e) {
          reportError(e);
        }
        if (event[STOP]) break;
      }
      const handler = this[`on${event.type}`];
      if (typeof handler === 'function') handler.call(this, event);
      event.currentTarget = null;
      return !event.defaultPrevented;
    }
  }
  const reportError = (e) => queueMicrotask(() => { throw e; });
  define('Event', Event);
  define('CustomEvent', CustomEvent);
  define('EventTarget', EventTarget);
  define('reportError', reportError);

  class AbortSignal extends EventTarget {
    #aborted = false;
    #reason;
    get aborted() { return this.#aborted; }
    get reason() { return this.#reason; }
    throwIfAborted() { if (this.#aborted) throw this.#reason; }
    static _abort(signal, reason) {
      if (signal.#aborted) return;
      signal.#aborted = true;
      signal.#reason = reason === undefined ? new DOMException('This operation was aborted', 'AbortError') : reason;
      signal.dispatchEvent(new Event('abort'));
    }
    static abort(reason) {
      const s = new AbortSignal();
      AbortSignal._abort(s, reason);
      return s;
    }
    static timeout(ms) {
      const s = new AbortSignal();
      setTimeout(() => AbortSignal._abort(s, new DOMException('The operation timed out.', 'TimeoutError')), ms);
      return s;
    }
    static any(signals) {
      const s = new AbortSignal();
      for (const other of signals) {
        if (other.aborted) {
          AbortSignal._abort(s, other.reason);
          break;
        }
        other.addEventListener('abort', () => AbortSignal._abort(s, other.reason), { once: true });
      }
      return s;
    }
  }
  class AbortController {
    #signal = new AbortSignal();
    get signal() { return this.#signal; }
    abort(reason) { AbortSignal._abort(this.#signal, reason); }
  }
  define('AbortSignal', AbortSignal);
  define('AbortController', AbortController);

  // ---------- performance ----------
  const marks = [];
  const performance = {
    timeOrigin: natives.timeOrigin,
    now: natives.now,
    mark(name, opts = {}) {
      const entry = { name, entryType: 'mark', startTime: opts.startTime ?? natives.now(), duration: 0, detail: opts.detail ?? null };
      marks.push(entry);
      return entry;
    },
    measure(name, start, end) {
      const at = (m) => (typeof m === 'string' ? marks.findLast((e) => e.name === m)?.startTime ?? 0 : m);
      const s = start === undefined ? 0 : at(start);
      const e = end === undefined ? natives.now() : at(end);
      const entry = { name, entryType: 'measure', startTime: s, duration: e - s };
      marks.push(entry);
      return entry;
    },
    getEntries: () => [...marks],
    getEntriesByName: (name) => marks.filter((m) => m.name === name),
    getEntriesByType: (type) => marks.filter((m) => m.entryType === type),
    clearMarks: (name) => { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'mark' && (!name || marks[i].name === name)) marks.splice(i, 1); },
    clearMeasures: (name) => { for (let i = marks.length - 1; i >= 0; i--) if (marks[i].entryType === 'measure' && (!name || marks[i].name === name)) marks.splice(i, 1); },
    toJSON: () => ({ timeOrigin: natives.timeOrigin }),
  };
  define('performance', performance);

  // ---------- structuredClone ----------
  define('structuredClone', (value, options) => {
    const seen = new Map();
    const transfer = new Set(options?.transfer || []);
    const clone = (v) => {
      if (v === null || typeof v !== 'object') {
        if (typeof v === 'function' || typeof v === 'symbol') throw new DOMException(`${String(v)} could not be cloned.`, 'DataCloneError');
        return v;
      }
      if (seen.has(v)) return seen.get(v);
      let out;
      if (v instanceof Date) out = new Date(v.getTime());
      else if (v instanceof RegExp) out = new RegExp(v.source, v.flags);
      else if (v instanceof ArrayBuffer) out = transfer.has(v) ? v : v.slice(0);
      else if (ArrayBuffer.isView(v)) {
        const buffer = clone(v.buffer);
        out = v instanceof DataView ? new DataView(buffer, v.byteOffset, v.byteLength) : new v.constructor(buffer, v.byteOffset, v.length);
      } else if (v instanceof Map) {
        out = new Map();
        seen.set(v, out);
        for (const [k, x] of v) out.set(clone(k), clone(x));
        return out;
      } else if (v instanceof Set) {
        out = new Set();
        seen.set(v, out);
        for (const x of v) out.add(clone(x));
        return out;
      } else if (v instanceof Error) {
        out = new (g[v.name] || Error)(v.message);
        if (v.stack) out.stack = v.stack;
      } else if (Array.isArray(v)) {
        out = [];
        seen.set(v, out);
        v.forEach((x, i) => (out[i] = clone(x)));
        return out;
      } else if (v instanceof interop.Pointer || typeof v.then === 'function') {
        throw new DOMException(`${Object.prototype.toString.call(v)} could not be cloned.`, 'DataCloneError');
      } else {
        out = {};
        seen.set(v, out);
        for (const k of Object.keys(v)) out[k] = clone(v[k]);
        return out;
      }
      seen.set(v, out);
      return out;
    };
    return clone(value);
  });

  // ---------- crypto ----------
  const crypto = {
    getRandomValues: (array) => natives.randomFill(array),
    randomUUID() {
      const b = natives.randomFill(new Uint8Array(16));
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
      return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
    },
  };
  define('crypto', crypto);

  define('setImmediate', (fn, ...args) => setTimeout(fn, 0, ...args));
  define('clearImmediate', (id) => clearTimeout(id));
  define('self', g);

  // ---------- process ----------
  const env = new Proxy({}, {
    get: (_, k) => (typeof k === 'string' ? natives.getenv(k) : undefined),
    set: (_, k, v) => (natives.setenv(String(k), String(v)), true),
    deleteProperty: (_, k) => (natives.setenv(String(k), undefined), true),
    has: (_, k) => natives.getenv(String(k)) !== undefined,
    ownKeys: () => natives.envKeys(),
    getOwnPropertyDescriptor: (_, k) => {
      const value = natives.getenv(String(k));
      return value === undefined ? undefined : { value, writable: true, enumerable: true, configurable: true };
    },
  });
  const exitListeners = [];
  const stream = (fd) => ({ write: (chunk) => (natives.print(fd, typeof chunk === 'string' ? chunk : new TextDecoder().decode(chunk), false), true), isTTY: false });
  const hrtime = (prev) => {
    const ns = natives.hrtime();
    const t = [Number(ns / 1000000000n), Number(ns % 1000000000n)];
    if (!prev) return t;
    let s = t[0] - prev[0], n = t[1] - prev[1];
    if (n < 0) { s--; n += 1e9; }
    return [s, n];
  };
  hrtime.bigint = natives.hrtime;
  const process = {
    argv: natives.argv,
    env,
    platform: 'linux',
    arch: natives.arch || 'x64',
    pid: natives.pid,
    version: 'v22.0.0',
    versions: { nativescript: '0.1.0', v8: natives.v8 },
    exitCode: undefined,
    cwd: natives.cwd,
    chdir: natives.chdir,
    hrtime,
    uptime: () => natives.now() / 1000,
    nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),
    stdout: stream(1),
    stderr: stream(2),
    on(event, fn) { if (event === 'exit') exitListeners.push(fn); return process; },
    once(event, fn) { return process.on(event, fn); },
    off() { return process; },
    emit(event, ...args) { if (event === 'exit') exitListeners.forEach((f) => f(...args)); return true; },
    exit(code = process.exitCode ?? 0) {
      process.emit('exit', code);
      natives.exit(code);
    },
  };
  define('process', process);
});
