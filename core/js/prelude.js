(function (natives) {
  'use strict';
  const print = natives.print;
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
    // struct views: fields are accessors on the prototype
    if (typeof value.toJSON === 'function' && Object.keys(value).length === 0) {
      const ctor = Object.getPrototypeOf(value)?.constructor;
      return `${ctor && ctor.name ? ctor.name + ' ' : ''}${inspect(value.toJSON(), depth, seen.concat([value]), true)}`;
    }
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
  // V8's console reports to the inspector (DevTools)
  const devtools = g.console;
  const out = (method, stream, args) => {
    print(stream, format(args));
    devtools?.[method]?.(...args);
  };
  const console = {
    log: (...args) => out('log', 1, args),
    info: (...args) => out('info', 1, args),
    debug: (...args) => out('debug', 1, args),
    warn: (...args) => out('warn', 2, args),
    error: (...args) => out('error', 2, args),
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

// GObject conveniences: g_signal_connect is a C macro, so it isn't in the
// metadata. Handlers get arguments typed from the signal's GSignalQuery.
(function () {
  'use strict';
  const g = globalThis;
  if (g.g_signal_connect !== undefined || typeof g.g_signal_connect_data !== 'function') return;

  const t = interop.types;
  // fundamental GType -> interop type (G_TYPE_* = n << 2)
  const fundamentals = {
    4: t.void, 12: t.int8, 16: t.uint8, 20: t.int32, 24: t.int32, 28: t.uint32, 32: t.long, 36: t.ulong,
    40: t.int64, 44: t.uint64, 48: t.int32, 52: t.uint32, 56: t.float, 60: t.double, 64: t.UTF8CString,
  };
  // mask G_SIGNAL_TYPE_STATIC_SCOPE, GTypes are pointer sized so use BigInts
  const typeFor = (gtype) => fundamentals[Number(g_type_fundamental(BigInt(gtype) & ~1n))] || t.pointer;
  const signatures = new Map();

  function signature(instance, signal) {
    const name = String(signal).split('::')[0];
    const itype = g_type_from_instance_ptr(instance);
    const key = `${itype}:${name}`;
    let sig = signatures.get(key);
    if (sig) return sig;
    const id = g_signal_lookup(name, itype);
    if (!id) throw new Error(`unknown signal '${name}'`);
    const query = new _GSignalQuery();
    g_signal_query(id, query);
    const args = [t.pointer];
    const params = query.n_params ? new BigUint64Array(interop.bufferFromData(query.param_types, query.n_params * 8)) : [];
    for (const p of params) args.push(typeFor(p));
    args.push(t.pointer);
    sig = { returns: typeFor(query.return_type), args };
    signatures.set(key, sig);
    return sig;
  }

  // G_TYPE_FROM_INSTANCE: instance->g_class->g_type
  function g_type_from_instance_ptr(instance) {
    const klass = new interop.Reference(t.pointer, instance).value;
    return new interop.Reference(t.ulong, klass).value;
  }

  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  define('g_signal_connect', (instance, signal, handler, data = null) =>
    g_signal_connect_data(instance, signal, interop.callback(handler, signature(instance, signal)), data, null, 0));
  define('g_signal_connect_after', (instance, signal, handler, data = null) =>
    g_signal_connect_data(instance, signal, interop.callback(handler, signature(instance, signal)), data, null, 1 /* G_CONNECT_AFTER */));
})();

// GVariant to JS values, and D-Bus objects whose properties read as JS properties.
(function () {
  'use strict';
  const g = globalThis;
  if (typeof g.g_variant_get_type_string !== 'function') return;
  const define = (name, value) =>
    Object.defineProperty(g, name, { value, writable: true, configurable: true, enumerable: false });
  const text = (p) => interop.stringFromCString(p);
  // 64-bit integers stay BigInts only when a Number would lose precision
  const int64 = (x) => (typeof x === 'bigint' && x >= BigInt(Number.MIN_SAFE_INTEGER) && x <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(x) : x);

  function children(v, each) {
    const n = Number(g_variant_n_children(v));
    for (let i = 0; i < n; i++) {
      const child = g_variant_get_child_value(v, i);
      try {
        each(child, i);
      } finally {
        g_variant_unref(child);
      }
    }
  }

  function unpack(v) {
    if (!v) return null;
    const type = text(g_variant_get_type_string(v));
    switch (type[0]) {
      case 'b': return !!g_variant_get_boolean(v);
      case 'y': return g_variant_get_byte(v);
      case 'n': return g_variant_get_int16(v);
      case 'q': return g_variant_get_uint16(v);
      case 'i': return g_variant_get_int32(v);
      case 'h': return g_variant_get_handle(v);
      case 'u': return g_variant_get_uint32(v);
      case 'x': return int64(g_variant_get_int64(v));
      case 't': return int64(g_variant_get_uint64(v));
      case 'd': return g_variant_get_double(v);
      case 's': case 'o': case 'g': return text(g_variant_get_string(v, null));
      case 'v': {
        const inner = g_variant_get_variant(v);
        try {
          return unpack(inner);
        } finally {
          g_variant_unref(inner);
        }
      }
      case 'm': {
        const inner = g_variant_get_maybe(v);
        if (!inner) return null;
        try {
          return unpack(inner);
        } finally {
          g_variant_unref(inner);
        }
      }
      case 'a': {
        if (type === 'ay') return new Uint8Array(collect(v));
        if (type[1] === '{') {
          const out = {};
          children(v, (entry) => {
            const key = g_variant_get_child_value(entry, 0);
            const value = g_variant_get_child_value(entry, 1);
            out[String(unpack(key))] = unpack(value);
            g_variant_unref(key);
            g_variant_unref(value);
          });
          return out;
        }
        return collect(v);
      }
      default: // tuples
        return collect(v);
    }
  }

  function collect(v) {
    const out = [];
    children(v, (child) => out.push(unpack(child)));
    return out;
  }

  define('g_variant_unpack', unpack);

  if (typeof g.g_dbus_proxy_new_for_bus_sync !== 'function') return;
  const t = interop.types;

  // GError** out parameter: the message, and the error freed
  function failure(ref, what) {
    const error = ref.value;
    if (!error) return new Error(what);
    const message = text(new interop.Reference(t.pointer, error.add(8)).value); // GError.message
    g_error_free(error);
    return new Error(`${what}: ${message}`);
  }

  function dbus(bus, name, path, iface = name) {
    const error = new interop.Reference(t.pointer, null);
    const proxy = g_dbus_proxy_new_for_bus_sync(bus, 0, null, name, path, iface, null, error);
    if (!proxy) throw failure(error, `D-Bus ${name} ${path}`);
    const read = (prop) => {
      const v = g_dbus_proxy_get_cached_property(proxy, prop);
      if (!v) return undefined;
      try {
        return unpack(v);
      } finally {
        g_variant_unref(v);
      }
    };
    const api = {
      proxy,
      /** Calls a D-Bus method; `parameters` is a GVariant tuple (or null). */
      call(method, parameters = null) {
        const err = new interop.Reference(t.pointer, null);
        const result = g_dbus_proxy_call_sync(proxy, method, parameters, 0, -1, null, err);
        if (!result) throw failure(err, `D-Bus ${method}`);
        try {
          return unpack(result);
        } finally {
          g_variant_unref(result);
        }
      },
      /** Runs `listener` whenever the service changes a property. */
      onChanged(listener) {
        return g_signal_connect(proxy, 'g-properties-changed', () => listener());
      },
    };
    // D-Bus properties are PascalCase; camelCase reads them too
    return new Proxy(api, {
      get(target, key) {
        if (typeof key !== 'string' || key in target) return target[key];
        const value = read(key);
        return value !== undefined ? value : read(key[0].toUpperCase() + key.slice(1));
      },
    });
  }

  define('DBus', {
    system: (name, path, iface) => dbus(1 /* G_BUS_TYPE_SYSTEM */, name, path, iface),
    session: (name, path, iface) => dbus(2 /* G_BUS_TYPE_SESSION */, name, path, iface),
  });
})();

  // DevTools domains implemented in JS (core's inspector_modules: Network, DOM, CSS).
  // Instances exist only while an inspector runs; other engines just keep the API.
  (() => {
    const domains = new Map();
    const send = (message) => natives.inspectorSend?.(message);
    const route = (text) => {
      const { id, method, params } = JSON.parse(text);
      const dot = method.indexOf('.');
      const target = domains.get(method.slice(0, dot));
      const fn = target?.[method.slice(dot + 1)];
      const reply = (body) => send(JSON.stringify({ id, ...body }));
      const fail = (e) => reply({ error: { code: -32000, message: String(e?.message ?? e) } });
      if (typeof fn !== 'function') {
        return reply({ error: { code: -32601, message: `'${method}' wasn't found` } });
      }
      try {
        Promise.resolve(fn.call(target, params)).then((result) => reply({ result: result ?? {} }), fail);
      } catch (e) {
        fail(e);
      }
    };
    define('__registerDomainDispatcher', (domain, klass) => {
      if (!natives.inspectorActive?.()) return;
      domains.set(domain, new klass());
      natives.inspectorRegister(domain, route);
    });
    define('__inspectorSendEvent', (message) => send(typeof message === 'string' ? message : JSON.stringify(message)));
    // CDP MonotonicTime: seconds
    define('__inspectorTimestamp', () => performance.now() / 1000);
  })();
});
