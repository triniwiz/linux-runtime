const { eq } = require('./harness');

const G_TYPE_OBJECT = BigInt(20 << 2);
const create = (type = G_TYPE_OBJECT) => g_object_new_with_properties(type, 0, null, null);
const refs = (o) => new interop.Reference(GObject, interop.handleof(o)).value.ref_count;

// GObjects returned by C are objects of their class, and still Pointers
const object = create();
eq([object instanceof GObject, object instanceof interop.Pointer], [true, true], 'instanceof');
eq(g_type_check_instance_is_a(object, G_TYPE_OBJECT), 1, 'passes to C');
eq(refs(object), 2, "the caller's reference and the JS object's");
g_object_unref(object);

// a floating reference becomes the JS object's
const unowned = create(g_initially_unowned_get_type());
eq([refs(unowned), g_object_is_floating(unowned)], [1, 0], 'floating reference sunk');

// constructors make GObjects
const made = new GObject();
eq([made instanceof GObject, refs(made)], [true, 1], 'new GObject()');
eq(new GValue() instanceof GObject, false, 'other structs stay structs');

// opaque structs (G_DECLARE_FINAL_TYPE) are classes too
const menu = g_menu_new();
eq([menu instanceof GMenu, menu instanceof GMenuModel], [true, true], 'opaque class');
eq(new GMenu() instanceof GMenu, true, 'new on an opaque class');
const throws = (f) => { try { f(); return false; } catch (e) { return e.message; } };
eq(throws(() => new GHashTable()), "'GHashTable' is an opaque record, its layout is not available", 'other opaque structs');
eq(g_hash_table_size(g_hash_table_new(null, null)), 0, 'pointers to opaque structs');

// one JS object per GObject; gpointer results stay Pointers
class Items extends GObject {
  static interfaces = [GListModelInterface];
  constructor(items) {
    super();
    this.items = items;
  }
  getItemType() { return G_TYPE_OBJECT; }
  getNItems() { return this.items.length; }
  getItem(i) { return g_object_ref(this.items[i]); }
}
const items = new Items([object, unowned]);
const first = g_list_model_get_object(items, 0);
eq(first === object, true, 'same JS object');
g_object_unref(first);
const item = g_list_model_get_item(items, 0);
eq([item === object, item.equals(object)], [false, true], 'gpointer result');
g_object_unref(item);

// signal handlers get the JS objects
const changed = [];
g_signal_connect(items, 'items-changed', (self, position) => changed.push(self === items, position));
g_list_model_items_changed(items, 1, 0, 0);
eq(changed, [true, 1], 'signal instance');

// lifetime: the JS object holds a toggle reference
if (typeof gc === 'function') {
  let finalized = 0;
  const notify = interop.callback(() => finalized++, { returns: interop.types.void, args: [interop.types.pointer, interop.types.pointer] });
  const settle = (then) => {
    gc();
    setTimeout(() => {
      gc();
      setTimeout(then, 10);
    }, 10);
  };
  (() => {
    const o = create();
    g_object_unref(o);
    g_object_weak_ref(o, notify, null);
  })();
  const held = (() => {
    const o = create();
    g_object_unref(o);
    o.tag = 'kept';
    return new Items([g_object_ref(o)]);
  })();
  settle(() => {
    eq(finalized, 1, 'a collected JS object releases its GObject');
    const again = g_list_model_get_object(held, 0);
    eq(again.tag, 'kept', 'referenced from C: the same JS object stays');
    g_object_unref(again);
  });
}
