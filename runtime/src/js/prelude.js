// Evaluated once per context, receives the native print(stream, text) function.
(function (print) {
  'use strict';
  const g = globalThis;
  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });

  define('global', g);

  // Used by the metadata for C enums, same as the iOS runtime
  Object.defineProperty(g, '__tsEnum', {
    writable: false,
    enumerable: false,
    configurable: false,
    value: function (obj) {
      const result = {};
      const keys = Object.keys(obj);
      for (let i = 0; i < keys.length; i++) {
        const key = keys[i];
        result[key] = obj[key];
        result[obj[key]] = key;
      }
      return result;
    },
  });

  function inspect(value, depth, seen, nested) {
    switch (typeof value) {
      case 'string':
        return nested ? JSON.stringify(value) : value;
      case 'number':
        return Object.is(value, -0) ? '-0' : String(value);
      case 'bigint':
        return `${value}n`;
      case 'boolean':
      case 'undefined':
        return String(value);
      case 'symbol':
        return value.toString();
      case 'function':
        return `[Function: ${value.name || '(anonymous)'}]`;
    }
    if (value === null) return 'null';
    if (value instanceof interop.Pointer) return value.toString();
    if (value instanceof Error) return value.stack || String(value);
    if (value instanceof Date) return value.toISOString();
    if (value instanceof RegExp) return String(value);
    if (seen.includes(value)) return '[Circular]';
    if (depth > 2) return Array.isArray(value) ? '[Array]' : '[Object]';
    seen = seen.concat([value]);
    if (ArrayBuffer.isView(value) && !(value instanceof DataView)) {
      return `${value.constructor.name}(${value.length}) [ ${Array.from(value).join(', ')} ]`;
    }
    if (Array.isArray(value)) {
      return `[ ${value.map((v) => inspect(v, depth + 1, seen, true)).join(', ')} ]`;
    }
    if (value instanceof Map) {
      const entries = Array.from(value, ([k, v]) => `${inspect(k, depth + 1, seen, true)} => ${inspect(v, depth + 1, seen, true)}`);
      return `Map(${value.size}) { ${entries.join(', ')} }`;
    }
    if (value instanceof Set) {
      return `Set(${value.size}) { ${Array.from(value, (v) => inspect(v, depth + 1, seen, true)).join(', ')} }`;
    }
    const keys = Object.keys(value);
    const proto = Object.getPrototypeOf(value);
    const name = proto && proto !== Object.prototype && proto.constructor && proto.constructor.name;
    const body = keys.map((k) => `${/^[A-Za-z_$][\w$]*$/.test(k) ? k : JSON.stringify(k)}: ${inspect(value[k], depth + 1, seen, true)}`);
    const prefix = name ? `${name} ` : '';
    return body.length ? `${prefix}{ ${body.join(', ')} }` : `${prefix}{}`;
  }

  function format(args) {
    let i = 0;
    let out = '';
    if (typeof args[0] === 'string' && args[0].includes('%')) {
      i = 1;
      out = args[0].replace(/%[sdifjoOc%]/g, (m) => {
        if (m === '%%') return '%';
        if (i >= args.length) return m;
        const v = args[i++];
        switch (m) {
          case '%s': return typeof v === 'string' ? v : inspect(v, 0, [], false);
          case '%d':
          case '%i': return typeof v === 'bigint' ? `${v}n` : String(Math.trunc(Number(v)));
          case '%f': return String(Number(v));
          case '%j': return JSON.stringify(v);
          case '%c': return '';
          default: return inspect(v, 0, [], true);
        }
      });
    }
    for (; i < args.length; i++) {
      out += (out.length ? ' ' : '') + inspect(args[i], 0, [], false);
    }
    return out;
  }

  const counts = new Map();
  const timers = new Map();
  const console = {
    log: (...args) => print(1, format(args)),
    info: (...args) => print(1, format(args)),
    debug: (...args) => print(1, format(args)),
    warn: (...args) => print(2, format(args)),
    error: (...args) => print(2, format(args)),
    dir: (value) => print(1, inspect(value, 0, [], true)),
    trace: (...args) => print(2, `Trace: ${format(args)}\n${new Error().stack.split('\n').slice(2).join('\n')}`),
    assert: (cond, ...args) => {
      if (!cond) print(2, `Assertion failed${args.length ? ': ' + format(args) : ''}`);
    },
    count: (label = 'default') => {
      const n = (counts.get(label) || 0) + 1;
      counts.set(label, n);
      print(1, `${label}: ${n}`);
    },
    time: (label = 'default') => timers.set(label, Date.now()),
    timeEnd: (label = 'default') => {
      const start = timers.get(label);
      if (start !== undefined) {
        timers.delete(label);
        print(1, `${label}: ${Date.now() - start}ms`);
      }
    },
  };
  define('console', console);
});
