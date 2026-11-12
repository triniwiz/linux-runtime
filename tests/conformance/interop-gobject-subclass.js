const { eq } = require('./harness');

const throws = (f) => { try { f(); return false; } catch (e) { return e.message; } };
const same = (a, b) => interop.handleof(a).equals(interop.handleof(b));

// vfuncs, properties, signals
class Counter extends GObject {
  static properties = {
    count: { type: G_TYPE_INT, default: 0 },
    label: { type: G_TYPE_STRING, default: 'none' },
    scale: { type: G_TYPE_DOUBLE, minimum: 0, maximum: 10, default: 1 },
  };
  static signals = { ticked: { params: [G_TYPE_INT, G_TYPE_DOUBLE] } };
  constructor(props) {
    super(props);
    this.ticks = [];
  }
  constructed() {
    this.wasConstructed = true;
    super.constructed();
  }
  tick() {
    this.count++;
    this.emit('ticked', this.count, 0.5);
  }
}

const counter = new Counter();
eq(g_type_name(Counter.$gtype), 'Counter', 'GType named after the class');
eq([counter instanceof Counter, counter instanceof GObject], [true, true], 'instanceof');
eq(counter.wasConstructed, true, 'constructed vfunc overridden');
eq(new interop.Reference(GObject, interop.handleof(counter)).value.ref_count, 1, 'only the toggle reference');
eq([counter.count, counter.label, counter.scale], [0, 'none', 1], 'property defaults');

const notified = [];
g_signal_connect(counter, 'notify', (self, pspec) => notified.push(g_param_spec_get_name(pspec)));
g_signal_connect(counter, 'ticked', (self, n, x) => counter.ticks.push([n, x]));
counter.tick();
eq(counter.ticks, [[1, 0.5]], 'signal emitted with typed arguments');
eq(notified, ['count'], 'JS property set notifies');

const value = new GValue();
g_value_init(value, G_TYPE_INT);
g_object_get_property(counter, 'count', value);
eq(g_value_get_int(value), 1, 'g_object_get_property');
g_value_set_int(value, 42);
g_object_set_property(counter, 'count', value);
eq(counter.count, 42, 'g_object_set_property');
eq(notified, ['count', 'count'], 'C property set notifies');

const configured = new Counter({ count: 7, label: 'seven' });
eq([configured.count, configured.label], [7, 'seven'], 'construct properties');
eq(typeof throws(() => new Counter({ missing: 1 })), 'string', 'unknown property throws');

// subclass of a JS class: chain up to JS, then to native
class Doubler extends Counter {
  tick() {
    super.tick();
    super.tick();
  }
}
const doubler = new Doubler();
doubler.tick();
eq(doubler.count, 2, 'super calls the JS parent');
eq(g_type_parent(Doubler.$gtype), Counter.$gtype, 'GType parent');

// interfaces: GListModel implemented in JS, used from C
class Items extends GObject {
  static interfaces = [GListModelInterface];
  constructor() {
    super();
    this.items = [new Counter(), new Counter()];
  }
  getItemType() { return Counter.$gtype; }
  getNItems() { return this.items.length; }
  getItem(i) { return i < this.items.length ? g_object_ref(this.items[i]) : null; }
}
const items = new Items();
eq(g_type_is_a(Items.$gtype, g_list_model_get_type()), 1, 'implements GListModel');
eq(g_list_model_get_n_items(items), 2, 'interface method');
eq(g_list_model_get_item_type(items), Counter.$gtype, 'camelCase method names');
const second = g_list_model_get_item(items, 1);
eq(same(second, items.items[1]), true, 'returns the JS object');
g_object_unref(second);
eq(g_list_model_get_item(items, 5), null, 'NULL return');

// a native container hands back the same JS object
const store = g_list_store_new(Counter.$gtype);
g_list_store_append(store, counter);
const stored = g_list_model_get_item(store, 0);
eq(same(stored, counter), true, 'same object from a GListStore');
g_object_unref(stored);

// extend
let initArgs;
const Point = GObject.extend('JSPoint', {
  properties: { x: { type: G_TYPE_DOUBLE }, y: { type: G_TYPE_DOUBLE } },
  init(...args) { initArgs = args; },
  constructed() { this.super.constructed(); this.ready = true; },
  length() { return Math.hypot(this.x, this.y); },
});
const point = new Point({ x: 3, y: 4 });
eq([g_type_name(Point.$gtype), point.ready, point.length()], ['JSPoint', true, 5], 'extend');
eq(initArgs, [{ x: 3, y: 4 }], 'init gets the constructor arguments');

// TypeScript's ES5 output, with @NativeClass and @Interfaces
var Es5Items = (function (_super) {
  __extends(Es5Items, _super);
  function Es5Items() {
    var _this = _super.call(this) || this;
    _this.n = 3;
    return _this;
  }
  Es5Items.prototype.get_n_items = function () { return this.n; };
  Es5Items.prototype.get_item_type = function () { return G_TYPE_OBJECT; };
  Es5Items.prototype.get_item = function () { return null; };
  return __decorate([NativeClass(), Interfaces([g_list_model_get_type()])], Es5Items);
})(GObject);
const es5 = new Es5Items();
eq([es5 instanceof Es5Items, g_list_model_get_n_items(es5)], [true, 3], 'ES5 classes');

// errors
eq(throws(() => { class Bad extends tm {} new Bad(); }), 'tm is not a GObject class', 'non-GObject base');
eq(typeof throws(() => { class Bad extends GObject { static interfaces = [GObjectClass]; } new Bad(); }), 'string', 'not an interface');

// lifetime: the JS object holds a toggle reference
if (typeof gc === 'function') {
  let disposed = 0;
  class Tracked extends GObject {
    dispose() {
      disposed++;
      super.dispose();
    }
  }
  (() => {
    for (let i = 0; i < 3; i++) new Tracked();
  })();
  const native = g_object_ref(new Tracked());
  const kept = new Tracked();
  const settle = (then) => {
    gc();
    setTimeout(() => {
      gc();
      setTimeout(then, 10);
    }, 10);
  };
  settle(() => {
    eq(disposed, 3, 'unreferenced objects are collected');
    g_object_unref(native);
    settle(() => {
      eq(disposed, 4, 'collected once native code releases it');
      eq(kept instanceof Tracked, true, 'referenced objects stay');
    });
  });
}
