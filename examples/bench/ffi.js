// C call overhead micro benchmarks
const N = 1_000_000;
function bench(name, fn) {
  for (let i = 0; i < 10_000; i++) fn(i); // warm up
  const t0 = performance.now();
  for (let i = 0; i < N; i++) fn(i);
  const ms = performance.now() - t0;
  console.log(`${name.padEnd(28)} ${(ms * 1e6 / N).toFixed(1).padStart(7)} ns/call`);
}
const buf = new Uint8Array(64).fill(65);
buf[63] = 0;
const view = interop.alloc(64);
bench('abs(int)', (i) => abs(-i));
bench('sqrt(double)', (i) => sqrt(i));
bench('strlen(js string)', () => strlen('hello world'));
bench('strlen(Uint8Array)', () => strlen(buf));
bench('memchr(ptr, int, size)', () => memchr(view, 0, 64));
bench('div(int, int) -> struct', (i) => div(i, 7));
bench('g_str_hash(string)', () => g_str_hash('nativescript'));
bench('getpid()', () => getpid());
const items = new Uint8Array(16);
const cmp = (a, b) => 0;
bench('qsort(16, js comparator)', () => qsort(items, 16, 1, cmp));
