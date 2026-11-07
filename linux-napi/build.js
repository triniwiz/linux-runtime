// cargo build --release, then linux.<arch>.node next to index.js, with the metadata generated
// in metadata-generator/tests (GTK 4 and GTK 3) when it's there
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');

const release = !process.argv.includes('--debug');
execFileSync('cargo', ['build', ...(release ? ['--release'] : [])], { cwd: __dirname, stdio: 'inherit' });
const lib = path.join(__dirname, 'target', release ? 'release' : 'debug', 'liblinux_napi.so');
fs.copyFileSync(lib, path.join(__dirname, `linux.${process.arch}.node`));

const arch = { x64: 'x86_64', arm64: 'aarch64' }[process.arch];
const tests = path.join(__dirname, '..', 'metadata-generator', 'tests');
for (const [dir, name] of [['TestOutput', 'metadata'], ['TestOutput-gtk3', 'metadata-gtk3']]) {
  const from = path.join(tests, dir, `metadata-${arch}.bin`);
  if (fs.existsSync(from)) fs.copyFileSync(from, path.join(__dirname, `${name}-${arch}.bin`));
}
