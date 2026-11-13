const { eq } = require('./harness');

// functions of a type are methods of its objects, named without the type's prefix
const action = g_simple_action_new('go', null);
eq(action.get_name(), 'go', 'method');
action.set_enabled(0);
eq(g_action_get_enabled(action), 0, 'method calls the C function');

// GObject properties, camelCase
eq(action.enabled, false, 'property');
action.enabled = true;
eq([action.enabled, action.parameterType], [true, null], 'property set');
const setter = Object.getOwnPropertyDescriptor(GSimpleAction.prototype, 'enabled').set;
// JSC's native functions have no names
if (g_simple_action_set_enabled.name) eq(setter.name, 'g_simple_action_set_enabled', 'written by the C setter when there is one');
const throws = (f) => { try { f(); return false; } catch (e) { return e.constructor.name; } };
eq(throws(() => { 'use strict'; action.name = 'other'; }), 'TypeError', 'construct-only properties are read-only');
eq(action.name, 'go', 'unchanged');

// inherited and interface methods
const menu = g_menu_new();
menu.append('A', 'app.a');
eq(menu.get_n_items(), 1, 'parent class method');
const store = g_list_store_new(G_TYPE_OBJECT);
store.append(menu);
eq([store.get_n_items(), store.get_item_type() === G_TYPE_OBJECT], [1, true], 'interface method');

// signals
const seen = [];
const id = action.connect('activate', (self) => seen.push(self === action));
action.activate(null);
action.disconnect(id);
action.activate(null);
eq(seen, [true], 'connect, disconnect');

// JS subclasses inherit them
class Tagged extends GObject {}
const tagged = new Tagged();
tagged.set_data('tag', new interop.Pointer(7));
eq(tagged.get_data('tag').toNumber(), 7, 'methods on JS class instances');

// a method with a vfunc of the same name: super reaches the parent class's implementation
const calls = [];
class App extends GApplication {
  activate() {
    calls.push('js');
    super.activate();
  }
}
const app = new App({ flags: 32 /* G_APPLICATION_NON_UNIQUE */ });
app.connect('activate', () => calls.push('handler'));
app.register(null, null);
g_application_activate(app);
eq(calls, ['handler', 'js'], 'super of a paired vfunc');
calls.length = 0;
app.activate();
eq(calls, ['js'], 'a method a JS class overrides is its override');

// the parent's slot is empty: super does nothing
class Quiet extends GObject {
  notify(pspec) {
    super.notify(pspec);
  }
}
new Quiet().notify('no-such-property');
