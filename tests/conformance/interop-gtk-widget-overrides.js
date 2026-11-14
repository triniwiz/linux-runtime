const { eq } = require('./harness');

// GTK calling a JS widget's vfuncs from the loop while it lays out and paints, the first
// time it hands JS a GtkSnapshot. Skipped without a display.
if (gtk_init_check()) {
  const calls = [];
  class Canvas extends GtkWidget {
    sizeAllocate(width, height, baseline) {
      calls.push(['size_allocate', this === canvas, width, height]);
      super.sizeAllocate(width, height, baseline);
    }
    snapshot(snapshot) {
      calls.push(['snapshot', this === canvas, snapshot instanceof GdkSnapshot]);
      super.snapshot(snapshot);
    }
  }
  const canvas = new Canvas();
  gtk_widget_set_size_request(canvas, 40, 30);
  const window = gtk_window_new();
  gtk_window_set_child(window, canvas);
  gtk_window_present(window);
  const start = Date.now();
  const poll = setInterval(() => {
    const painted = calls.some(([name]) => name === 'snapshot');
    if (!painted && Date.now() - start < 5000) return;
    clearInterval(poll);
    gtk_window_destroy(window);
    eq(calls.find(([name]) => name === 'size_allocate')?.slice(0, 2), ['size_allocate', true], 'size_allocate override');
    eq(calls.find(([name]) => name === 'snapshot'), ['snapshot', true, true], 'snapshot override');
  }, 20);
}
