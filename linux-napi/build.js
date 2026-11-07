// cargo build --release, then linux.<arch>.node next to index.js
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const release = !process.argv.includes('--debug');
execFileSync('cargo', ['build', ...(release ? ['--release'] : [])], { cwd: __dirname, stdio: 'inherit' });
const lib = path.join(__dirname, 'target', release ? 'release' : 'debug', 'liblinux_napi.so');
fs.copyFileSync(lib, path.join(__dirname, `linux.${process.arch}.node`));
