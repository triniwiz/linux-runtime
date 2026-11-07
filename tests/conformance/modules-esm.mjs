// ES modules: static and dynamic imports, JSON, CommonJS interop, packages, import.meta, top-level await
import { eq } from './harness.js';
import * as cycle from './esm-fixtures/cycle-a.mjs';

await import('../../runtime/tests/fixtures/modules/main.mjs');
eq(globalThis.results, {
  add: 5, value: 42, json: 'fixture', cjsDefault: 'commonjs', cjsNamed: 'hello esm from legacy.cjs',
  pkg: 'pkg:esm', scoped: 'scoped', dirIndex: 'index', metaUrl: true, metaResolve: true,
  dynamic: 'dynamic', tla: 'awaited', missing: true,
}, 'module graph');

eq([cycle.a, cycle.fromB(), cycle.liveCount()], ['a', 'b sees a', 1], 'cycles and live bindings');
eq(import.meta.url.endsWith('/tests/conformance/modules-esm.mjs'), true, 'import.meta.url');
eq(import.meta.filename.endsWith('/modules-esm.mjs'), true, 'import.meta.filename');

let syntax;
try { await import('./esm-fixtures/syntax-error.mjs'); } catch (e) { syntax = e instanceof SyntaxError; }
eq(syntax, true, 'syntax errors reject import()');
let thrown;
try { await import('./esm-fixtures/throws.mjs'); } catch (e) { thrown = e.message; }
eq(thrown, 'module body', 'evaluation errors reject import()');
