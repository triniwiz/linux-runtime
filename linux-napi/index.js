'use strict';
// C declarations for GTK, GLib, X11, libc..., plus `interop` and GLib's main context.
// GTK 3 if the process already has it (Electron), GTK 4 otherwise.
module.exports = require('./load')();
