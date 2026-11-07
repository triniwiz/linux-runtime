// Battery status from UPower over D-Bus (GIO), live in a GTK4 window.
//   NS_METADATA=metadata-generator/tests/TestOutput/metadata-x86_64.bin ./target/debug/ns-linux examples/battery.js

const css = `
  window { background: linear-gradient(160deg, #0f2027, #203a43, #2c5364); }
  .title { color: white; font-size: 22px; font-weight: 800; }
  .percent { color: #65f0c4; font-size: 72px; font-weight: 900; font-family: monospace; }
  .percent.low { color: #ff7675; }
  .state { color: #f8d66d; font-size: 16px; font-weight: 600; }
  .detail { color: #c9d2ff; font-size: 13px; font-family: monospace; }
  .device { color: white; font-size: 13px; }
  levelbar trough { min-height: 14px; border-radius: 7px; }
  levelbar block.filled { background: #65f0c4; border-radius: 7px; }
  levelbar block.low { background: #ff7675; border-radius: 7px; }
`;

const UPOWER = 'org.freedesktop.UPower';
const STATES = ['Unknown', 'Charging', 'Discharging', 'Empty', 'Fully charged', 'Pending charge', 'Pending discharge'];
const KINDS = ['Unknown', 'Line power', 'Battery', 'UPS', 'Monitor', 'Mouse', 'Keyboard', 'PDA', 'Phone', 'Media player', 'Tablet', 'Computer', 'Gaming input', 'Pen', 'Touchpad', 'Modem', 'Network', 'Headset', 'Speakers', 'Headphones', 'Video', 'Other audio', 'Remote control', 'Printer', 'Scanner', 'Camera', 'Wearable', 'Toy', 'Bluetooth'];

const device = (path) => DBus.system(UPOWER, path, `${UPOWER}.Device`);

function duration(seconds) {
  if (!seconds) return '—';
  const h = Math.floor(seconds / 3600);
  const m = Math.round((seconds % 3600) / 60);
  return h ? `${h} h ${m} min` : `${m} min`;
}

function label(content, cls) {
  const l = gtk_label_new(content);
  if (cls) gtk_widget_add_css_class(l, cls);
  gtk_widget_set_halign(l, GtkAlign.CENTER);
  return l;
}

/** Peripheral batteries (mouse, keyboard, headset...): the system's own goes in the header. */
function peripherals(upower) {
  const [paths] = upower.call('EnumerateDevices');
  return paths
    .map(device)
    .filter((d) => d.type !== 1 && d.type !== 2 && d.isPresent !== false)
    .map((d) => ({ kind: KINDS[d.type] ?? 'Device', model: d.model, percent: d.percentage }));
}

function buildUi(app) {
  const window = gtk_application_window_new(app);
  gtk_window_set_title(window, 'Battery');
  gtk_window_set_default_size(window, 380, 480);

  const provider = gtk_css_provider_new();
  gtk_css_provider_load_from_data(provider, css, -1);
  gtk_style_context_add_provider_for_display(gdk_display_get_default(), provider, 800 /* GTK_STYLE_PROVIDER_PRIORITY_USER */);

  const box = gtk_box_new(GtkOrientation.VERTICAL, 12);
  for (const set of [gtk_widget_set_margin_top, gtk_widget_set_margin_bottom, gtk_widget_set_margin_start, gtk_widget_set_margin_end]) {
    set(box, 28);
  }

  const icon = gtk_image_new_from_icon_name('battery-missing-symbolic');
  gtk_image_set_pixel_size(icon, 64);
  gtk_box_append(box, icon);
  gtk_box_append(box, label('Battery', 'title'));
  const percent = label('—', 'percent');
  gtk_box_append(box, percent);
  const level = gtk_level_bar_new_for_interval(0, 100);
  gtk_level_bar_add_offset_value(level, 'low', 20);
  gtk_box_append(box, level);
  const state = label('', 'state');
  gtk_box_append(box, state);
  const time = label('', 'detail');
  const source = label('', 'detail');
  const rate = label('', 'detail');
  gtk_box_append(box, time);
  gtk_box_append(box, source);
  gtk_box_append(box, rate);
  const devices = gtk_box_new(GtkOrientation.VERTICAL, 4);
  gtk_box_append(box, devices);

  const upower = DBus.system(UPOWER, '/org/freedesktop/UPower');
  const display = device('/org/freedesktop/UPower/devices/DisplayDevice');
  if (!display.isPresent) {
    gtk_label_set_text(percent, 'n/a');
    gtk_label_set_text(state, 'No battery found (UPower)');
    gtk_window_set_child(window, box);
    gtk_window_present(window);
    return;
  }

  const refresh = () => {
    const pct = display.percentage;
    const charging = display.state === 1;
    gtk_label_set_text(percent, `${Math.round(pct)}%`);
    (pct <= 20 ? gtk_widget_add_css_class : gtk_widget_remove_css_class)(percent, 'low');
    gtk_level_bar_set_value(level, pct);
    gtk_label_set_text(state, STATES[display.state] ?? STATES[0]);
    gtk_image_set_from_icon_name(icon, display.iconName);
    gtk_label_set_text(time, `${charging ? 'Full in' : 'Remaining'}: ${duration(charging ? display.timeToFull : display.timeToEmpty)}`);
    gtk_label_set_text(source, `Power source: ${upower.onBattery ? 'battery' : 'AC adapter'}`);
    gtk_label_set_text(rate, `${display.energyRate.toFixed(1)} W  ·  ${display.energy.toFixed(1)} / ${display.energyFull.toFixed(1)} Wh`);

    for (let child = gtk_widget_get_first_child(devices); child; child = gtk_widget_get_first_child(devices)) {
      gtk_box_remove(devices, child);
    }
    for (const d of peripherals(upower)) {
      gtk_box_append(devices, label(`${d.kind}${d.model ? ` (${d.model})` : ''}: ${Math.round(d.percent)}%`, 'device'));
    }
  };

  // UPower updates the cached properties and tells us; nothing polls
  display.onChanged(refresh);
  upower.onChanged(refresh);
  refresh();

  gtk_window_set_child(window, box);
  gtk_window_present(window);
}

const app = gtk_application_new('org.nativescript.BatteryDemo', GApplicationFlags.FLAGS_NONE);
g_signal_connect(app, 'activate', buildUi);
const status = g_application_run(app, 0, null);
g_object_unref(app);
console.log('application exited with', status);
