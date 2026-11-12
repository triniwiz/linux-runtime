// GObject classes written in JavaScript: `class X extends GtkWidget {}` and
// `GtkWidget.extend(...)`. This describes a class once, on its first construction
// (`__nsGObject.register`); the runtime registers the GType and runs it natively.
(function (natives) {
  'use strict';
  const g = globalThis;
  if (typeof g.g_type_register_static_simple !== 'function' || typeof natives.gobjectRegister !== 'function') return;

  const define = (target, name, value) =>
    Object.defineProperty(target, name, { value, writable: true, configurable: true, enumerable: false });

  // G_TYPE_* are macros: fundamental type n is n << 2
  const G = {};
  ['INVALID', 'NONE', 'INTERFACE', 'CHAR', 'UCHAR', 'BOOLEAN', 'INT', 'UINT', 'LONG', 'ULONG', 'INT64', 'UINT64', 'ENUM', 'FLAGS', 'FLOAT', 'DOUBLE', 'STRING', 'POINTER', 'BOXED', 'PARAM', 'OBJECT', 'VARIANT'].forEach((name, i) => {
    G[name] = i << 2;
    if (g[`G_TYPE_${name}`] === undefined) define(g, `G_TYPE_${name}`, i << 2);
  });
  const G_SIGNAL_RUN_LAST = 2;
  const G_PARAM_READWRITE = 3;

  const view = (S, p) => new interop.Reference(S, p).value;
  const fundamentalOf = (type) => Number(g_type_fundamental(type));
  const snake = (s, sep = '_') =>
    s.replace(/([a-z0-9])([A-Z])/g, `$1${sep}$2`).replace(/([A-Z])([A-Z][a-z])/g, `$1${sep}$2`).replace(/[_-]/g, sep).toLowerCase();
  const own = (o, key) => Object.prototype.hasOwnProperty.call(o, key);
  const ownStatic = (cls, name) => (own(cls, name) ? cls[name] : undefined);

  // GType of a C type name: the class may not be registered yet, so try its
  // `*_get_type` function, whose name splits the type name at capitals
  // differently from type to type (GtkCenterBox: gtk_center_box, GDBusProxy: g_dbus_proxy).
  function typeFromName(name) {
    const type = g_type_from_name(name);
    if (type) return type;
    const words = name.match(/[A-Z][^A-Z]*|^[^A-Z]+/g) || [name];
    const combinations = 1 << Math.min(words.length - 1, 10);
    for (let mask = combinations - 1; mask >= 0; mask--) {
      let symbol = words[0];
      for (let i = 1; i < words.length; i++) symbol += (mask & (1 << (i - 1)) ? '_' : '') + words[i];
      const f = g[`${symbol.toLowerCase()}_get_type`];
      if (typeof f === 'function') return f();
    }
    return 0;
  }

  function typeOf(x) {
    if (x == null) throw new TypeError(`${x} is not a GType: pass a GType, a type name or a struct constructor`);
    if (typeof x === 'number' || typeof x === 'bigint') return x;
    if (typeof x === 'function' && Object.getPrototypeOf(x) !== Function.prototype) {
      if (!register(x)) throw new TypeError(`${x.name} is not a GObject class`);
      return classes.get(x).type;
    }
    const name = typeof x === 'function' ? String(x.name).replace(/^_/, '') : String(x);
    const type = typeFromName(name) || typeFromName(name.replace(/(Interface|Iface|Class)$/, ''));
    if (!type) throw new TypeError(`${name} is not a GType`);
    return type;
  }

  // the class or interface struct of a type, e.g. GtkWidgetClass
  function structFor(type, suffixes) {
    const name = g_type_name(type);
    for (const suffix of suffixes) {
      for (const prefix of ['', '_']) {
        const S = g[prefix + name + suffix];
        if (typeof S === 'function') return S;
      }
    }
    return undefined;
  }

  function slot(structs, method) {
    for (const field of new Set([method, snake(method)])) {
      for (const S of structs) {
        if (natives.gobjectSlot(S, field)) return [method, S, field];
      }
    }
    return undefined;
  }

  function paramSpec(name, spec) {
    const type = typeOf(spec.type ?? G.STRING);
    const nick = spec.nick ?? name;
    const blurb = spec.blurb ?? nick;
    const flags = spec.flags ?? G_PARAM_READWRITE;
    const def = spec.default;
    const range = (min, max, d = 0) => [spec.minimum ?? min, spec.maximum ?? max, def ?? d];
    switch (fundamentalOf(type)) {
      case G.BOOLEAN: return g_param_spec_boolean(name, nick, blurb, def ? 1 : 0, flags);
      case G.CHAR: return g_param_spec_char(name, nick, blurb, ...range(-128, 127), flags);
      case G.UCHAR: return g_param_spec_uchar(name, nick, blurb, ...range(0, 255), flags);
      case G.INT: return g_param_spec_int(name, nick, blurb, ...range(-2147483648, 2147483647), flags);
      case G.UINT: return g_param_spec_uint(name, nick, blurb, ...range(0, 4294967295), flags);
      case G.LONG: return g_param_spec_long(name, nick, blurb, ...range(-(2n ** 63n), 2n ** 63n - 1n), flags);
      case G.ULONG: return g_param_spec_ulong(name, nick, blurb, ...range(0, 2n ** 64n - 1n), flags);
      case G.INT64: return g_param_spec_int64(name, nick, blurb, ...range(-(2n ** 63n), 2n ** 63n - 1n), flags);
      case G.UINT64: return g_param_spec_uint64(name, nick, blurb, ...range(0, 2n ** 64n - 1n), flags);
      case G.FLOAT: return g_param_spec_float(name, nick, blurb, ...range(-3.4028234663852886e38, 3.4028234663852886e38), flags);
      case G.DOUBLE: return g_param_spec_double(name, nick, blurb, ...range(-Number.MAX_VALUE, Number.MAX_VALUE), flags);
      case G.STRING: return g_param_spec_string(name, nick, blurb, def ?? null, flags);
      case G.ENUM: return g_param_spec_enum(name, nick, blurb, type, def ?? firstEnumValue(type), flags);
      case G.FLAGS: return g_param_spec_flags(name, nick, blurb, type, def ?? 0, flags);
      case G.OBJECT:
      case G.INTERFACE: return g_param_spec_object(name, nick, blurb, type, flags);
      case G.BOXED: return g_param_spec_boxed(name, nick, blurb, type, flags);
      case G.POINTER: return g_param_spec_pointer(name, nick, blurb, flags);
      case G.VARIANT: return g_param_spec_variant(name, nick, blurb, g_variant_type_new(spec.variantType ?? 'v'), def ?? null, flags);
    }
    throw new TypeError(`properties of type ${g_type_name(type)} are not supported`);
  }

  function firstEnumValue(type) {
    const klass = g_type_class_ref(type);
    try {
      return view(GEnumValue, view(GEnumClass, klass).values).value;
    } finally {
      g_type_class_unref(klass);
    }
  }

  // Values live on the instance under a Symbol, defaults on the prototype; the
  // runtime's get_property / set_property use the same Symbol.
  function properties(cls) {
    const proto = cls.prototype;
    const list = [];
    for (const [key, spec] of Object.entries(ownStatic(cls, 'properties') || {})) {
      const name = snake(key, '-');
      const slot = Symbol(key);
      if (spec.override) {
        list.push([key, slot, true, null, name]);
        continue;
      }
      const pspec = paramSpec(name, spec);
      const custom = own(proto, key);
      if (!custom) {
        define(proto, slot, spec.default);
        Object.defineProperty(proto, key, {
          configurable: true,
          get() {
            return this[slot];
          },
          set(value) {
            this[slot] = value;
            g_object_notify_by_pspec(this, pspec);
          },
        });
      }
      list.push([key, slot, custom, pspec, null]);
    }
    return list;
  }

  const classes = new Map(); // JS class -> { base, baseType, type }
  const reserved = new Set(['constructor', 'finalize', 'init', 'emit']);

  function methodNames(cls, base) {
    const names = new Set();
    for (let proto = cls.prototype; proto && proto !== base.prototype; proto = Object.getPrototypeOf(proto)) {
      for (const name of Object.getOwnPropertyNames(proto)) {
        if (name !== 'constructor' && typeof Object.getOwnPropertyDescriptor(proto, name).value === 'function') names.add(name);
      }
    }
    return names;
  }

  function typeName(cls) {
    let name = String(own(cls, 'GTypeName') ? cls.GTypeName : cls.name || 'JSObject');
    name = name.replace(/[^A-Za-z0-9_+-]/g, '_');
    if (!/^[A-Za-z_]/.test(name) || name.length < 3) name = `JS_${name}`;
    let unique = name;
    for (let n = 2; g_type_from_name(unique); n++) unique = `${name}_${n}`;
    return unique;
  }

  // Registers a JS class (and its JS ancestors): false when it doesn't derive from a GObject class.
  function register(cls) {
    if (classes.has(cls)) return true;
    const parent = Object.getPrototypeOf(cls);
    if (typeof parent !== 'function' || parent === Function.prototype) return false;
    let base, parentType, baseType;
    const root = Object.getPrototypeOf(parent) === Function.prototype;
    if (root) {
      base = parent;
      parentType = baseType = typeFromName(String(parent.name).replace(/^_/, ''));
      if (!parentType || !g_type_is_a(parentType, G.OBJECT)) throw new TypeError(`${parent.name} is not a GObject class`);
      // instances are views of the base's struct: keep its fields reachable
      if (Object.getPrototypeOf(base.prototype) === Object.prototype) {
        Object.setPrototypeOf(base.prototype, Object.getPrototypeOf(new base()));
      }
    } else {
      if (!register(parent)) return false;
      ({ base, baseType, type: parentType } = classes.get(parent));
    }
    const structs = [];
    for (let type = parentType; type; type = g_type_parent(type)) {
      const S = structFor(type, ['Class']);
      if (S) structs.push(S);
    }
    const proto = cls.prototype;
    const vfuncs = [];
    for (const name of Object.getOwnPropertyNames(proto)) {
      if (reserved.has(name) || typeof Object.getOwnPropertyDescriptor(proto, name).value !== 'function') continue;
      const s = slot(structs, name);
      if (s) vfuncs.push(s);
    }
    const interfaces = (ownStatic(cls, 'interfaces') || []).map((iface) => {
      const itype = typeOf(iface);
      if (fundamentalOf(itype) !== G.INTERFACE) throw new TypeError(`${g_type_name(itype)} is not an interface`);
      const S = structFor(itype, ['Interface', 'Iface']);
      if (!S) throw new TypeError(`the ${g_type_name(itype)} interface struct is not in the metadata`);
      const slots = [...methodNames(cls, base)].map((name) => slot([S], name)).filter(Boolean);
      return [itype, slots];
    });
    const signals = Object.entries(ownStatic(cls, 'signals') || {}).map(([name, spec]) => [
      name,
      spec.flags ?? G_SIGNAL_RUN_LAST,
      typeOf(spec.returnType ?? G.NONE),
      (spec.params || []).map(typeOf),
    ]);
    const type = natives.gobjectRegister(cls, {
      parent: parentType,
      name: typeName(cls),
      base,
      root,
      vfuncs,
      interfaces,
      properties: properties(cls),
      signals,
    });
    classes.set(cls, { base, baseType, type });
    define(cls, '$gtype', type);
    if (root && !('emit' in proto)) {
      define(proto, 'emit', function (signal, ...args) {
        return natives.gobjectEmit(this, signal, ...args);
      });
    }
    // `super.method()` from a class whose parent is native reaches the base's prototype:
    // give it the base type's implementation of the vfunc
    const baseClass = g_type_class_ref(baseType);
    for (const [name, S, field] of vfuncs) {
      if (name in base.prototype) continue;
      const fn = view(S, baseClass)[field];
      define(base.prototype, name, fn ? function (...args) { return fn(this, ...args); } : function () {});
    }
    return true;
  }

  const config = new Set(['interfaces', 'properties', 'signals', 'GTypeName']);

  function extend(base, name, members) {
    if (typeof name !== 'string') {
      members = name;
      name = undefined;
    }
    members = members || {};
    const className = name || `${String(base.name).replace(/^_/, '')}Subclass`;
    const cls = {
      [className]: class extends base {
        constructor(...args) {
          super(...args);
          if (typeof this.init === 'function') this.init(...args);
        }
      },
    }[className];
    for (const key of Reflect.ownKeys(members)) {
      if (config.has(key)) define(cls, key, members[key]);
      else Object.defineProperty(cls.prototype, key, Object.getOwnPropertyDescriptor(members, key));
    }
    if (name) define(cls, 'GTypeName', name);
    const parent = Object.getPrototypeOf(cls.prototype);
    Object.defineProperty(cls.prototype, 'super', {
      configurable: true,
      get() {
        const self = this;
        return new Proxy(
          {},
          {
            get(_, key) {
              const value = parent[key];
              return typeof value === 'function' ? value.bind(self) : value;
            },
          },
        );
      },
    });
    return cls;
  }

  define(g, '__nsGObject', Object.freeze({ register, extend }));
  if (g.NativeClass === undefined) {
    define(g, 'NativeClass', (...args) => (typeof args[0] === 'function' ? args[0] : (cls) => cls));
  }
  if (g.Interfaces === undefined) {
    define(g, 'Interfaces', (...list) => (cls) => {
      define(cls, 'interfaces', list.length === 1 && Array.isArray(list[0]) ? list[0] : list);
    });
  }
});
