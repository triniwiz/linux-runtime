// GTK4 from JavaScript, straight through the generated metadata.
//   NS_METADATA=metadata-generator/tests/TestOutput/metadata-x86_64.bin ./target/debug/ns-linux examples/gtk-demo.js

const css = `
  window { background: linear-gradient(160deg, #1d2b64, #3a1c71); }
  .title { color: white; font-size: 26px; font-weight: 800; }
  .subtitle { color: #c9d2ff; font-size: 13px; }
  .counter { color: #65f0c4; font-size: 64px; font-weight: 900; font-family: monospace; }
  .clock { color: #f8d66d; font-family: monospace; font-size: 14px; }
  .greeting { color: white; font-size: 16px; }
  button.accent { background: #65f0c4; color: #1d2b64; font-weight: 700; border-radius: 10px; padding: 8px 18px; }
  entry { border-radius: 10px; }
`;

const text = (ptr) => interop.stringFromCString(ptr);

function label(content, cls) {
  const l = gtk_label_new(content);
  if (cls) gtk_widget_add_css_class(l, cls);
  return l;
}

function buildUi(app) {
  const window = gtk_application_window_new(app);
  gtk_window_set_title(window, 'NativeScript ♥ GTK4');
  gtk_window_set_default_size(window, 460, 520);

  const provider = gtk_css_provider_new();
  gtk_css_provider_load_from_data(provider, css, -1);
  gtk_style_context_add_provider_for_display(gdk_display_get_default(), provider, 800 /* GTK_STYLE_PROVIDER_PRIORITY_USER */);

  const box = gtk_box_new(GtkOrientation.VERTICAL, 14);
  for (const set of [gtk_widget_set_margin_top, gtk_widget_set_margin_bottom, gtk_widget_set_margin_start, gtk_widget_set_margin_end]) {
    set(box, 28);
  }

  gtk_box_append(box, label('Hello from JavaScript', 'title'));
  gtk_box_append(box, label('V8 → libffi → libgtk-4.so.1, no bindings written by hand', 'subtitle'));

  // counter
  let count = 0;
  const counter = label('0', 'counter');
  gtk_box_append(box, counter);
  const row = gtk_box_new(GtkOrientation.HORIZONTAL, 10);
  gtk_widget_set_halign(row, GtkAlign.CENTER);
  const minus = gtk_button_new_with_label('−1');
  const plus = gtk_button_new_with_label('+1');
  gtk_widget_add_css_class(plus, 'accent');
  g_signal_connect(minus, 'clicked', () => gtk_label_set_text(counter, String(--count)));
  g_signal_connect(plus, 'clicked', () => gtk_label_set_text(counter, String(++count)));
  gtk_box_append(row, minus);
  gtk_box_append(row, plus);
  gtk_box_append(box, row);

  // text entry
  const entry = gtk_entry_new();
  gtk_entry_set_placeholder_text(entry, 'Your name');
  const greeting = label(' ', 'greeting');
  const greet = () => {
    const raw = text(gtk_editable_get_text(entry)).trim() || 'stranger';
    const escaped = g_markup_escape_text(raw, -1); // returns newly allocated memory
    const name = text(escaped);
    g_free(escaped);
    gtk_label_set_markup(greeting, `Hi <b>${name}</b>, you are logged in as <i>${text(g_get_user_name())}</i>`);
  };
  g_signal_connect(entry, 'activate', greet);
  const greetButton = gtk_button_new_with_label('Greet');
  g_signal_connect(greetButton, 'clicked', greet);
  gtk_box_append(box, entry);
  gtk_box_append(box, greetButton);
  gtk_box_append(box, greeting);

  // a switch, typed signal arguments (GObject, GParamSpec*)
  const switchRow = gtk_box_new(GtkOrientation.HORIZONTAL, 10);
  const toggle = gtk_switch_new();
  const toggleLabel = label('Dark mode: on', 'subtitle');
  g_signal_connect(toggle, 'notify::active', (self) => {
    const on = gtk_switch_get_active(self);
    gtk_label_set_text(toggleLabel, `Dark mode: ${on ? 'on' : 'off'}`);
    gtk_widget_set_opacity(box, on ? 1 : 0.75);
  });
  gtk_switch_set_active(toggle, 1);
  gtk_box_append(switchRow, toggle);
  gtk_box_append(switchRow, toggleLabel);
  gtk_box_append(box, switchRow);

  // clock driven by a GLib timeout calling back into JS
  const clock = label('', 'clock');
  const tick = () => {
    const now = g_date_time_new_now_local();
    const formatted = g_date_time_format(now, '%H:%M:%S');
    gtk_label_set_text(clock, `GLib clock  ${text(formatted)}`);
    g_free(formatted);
    g_date_time_unref(now);
    return 1; // G_SOURCE_CONTINUE
  };
  tick();
  g_timeout_add(1000, tick, null);
  gtk_box_append(box, clock);

  const quit = gtk_button_new_with_label('Quit');
  g_signal_connect(quit, 'clicked', () => gtk_window_destroy(window));
  gtk_box_append(box, quit);

  gtk_window_set_child(window, box);
  gtk_window_present(window);
}

const app = gtk_application_new('org.nativescript.LinuxDemo', GApplicationFlags.FLAGS_NONE);
g_signal_connect(app, 'activate', buildUi);
const status = g_application_run(app, 0, null);
g_object_unref(app);
console.log('application exited with', status);
