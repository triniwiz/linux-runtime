// C call overhead micro benchmarks: ns-linux examples/bench/ffi.js [filter]
const N = 1_000_000;
const filter = process.argv[2];
function bench(name, fn) {
  if (filter && !name.includes(filter)) return;
  for (let i = 0; i < 10_000; i++) fn(i); // warm up
  const t0 = performance.now();
  for (let i = 0; i < N; i++) fn(i);
  const ms = performance.now() - t0;
  console.log(`${name.padEnd(32)} ${(ms * 1e6 / N).toFixed(1).padStart(7)} ns/call`);
}
const buf = new Uint8Array(64).fill(65);
buf[63] = 0;
const view = interop.alloc(64);
const out = interop.alloc(64);

// fast API calls
bench('abs(int)', (i) => abs(-i));
bench('sqrt(double)', (i) => sqrt(i));
bench('pow(double, double)', (i) => pow(i, 0.5));
bench('toupper(int)', (i) => toupper(i & 127));
bench('g_direct_hash(Pointer)', () => g_direct_hash(view));
bench('g_str_hash(string)', () => g_str_hash('nativescript'));
bench('g_str_equal(string, string)', () => g_str_equal('nativescript', 'nativescript'));
bench('memcmp(buffer, buffer, size)', () => memcmp(buf, buf, 16));
bench('getpid()', () => getpid());

// 64-bit, pointer and struct results
bench('labs(long)', (i) => labs(-i));
bench('strlen(js string)', () => strlen('hello world'));
bench('strlen(Uint8Array)', () => strlen(buf));
bench('memchr(ptr, int, size)', () => memchr(view, 0, 64));
bench('getenv(string) -> Pointer', () => getenv('HOME'));
bench('div(int, int) -> struct', (i) => div(i, 7));

// variadic, long double and structs by value
bench('snprintf(buf, n, "%d", int)', (i) => snprintf(out, 64, '%d', i));
bench('sqrtl(long double)', (i) => sqrtl(i));
const fa = new FcValue({ type: 1 });
const fb = new FcValue({ type: 1 });
bench('FcValueEqual(struct, struct)', () => FcValueEqual(fa, fb));

// struct views
const t = new tm({ tm_year: 100 });
bench('struct field read', () => t.tm_year);
bench('struct field write', (i) => { t.tm_year = i; });
bench('div(int, int).quot', (i) => div(i, 7).quot);

// C calling back into JS
const items = new Uint8Array(16);
const cmp = (a, b) => 0;
bench('qsort(16, js comparator)', () => qsort(items, 16, 1, cmp));
