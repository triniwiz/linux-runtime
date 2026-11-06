import { add, value as v } from './lib/math.mjs';
import data from './data.json' with { type: 'json' };
import cjs, { greet } from './legacy.cjs';
import pkg from 'pkg';
import { util } from '@scope/util';
import def from './lib';

const results = globalThis.results = {};
results.add = add(2, 3);
results.value = v;
results.json = data.name;
results.cjsDefault = cjs.kind;
results.cjsNamed = greet('esm');
results.pkg = pkg;
results.scoped = util();
results.dirIndex = def;
results.metaUrl = import.meta.url.endsWith('/modules/main.mjs');
results.metaResolve = import.meta.resolve('./data.json').startsWith('file://');
results.dynamic = (await import('./lib/dynamic.mjs')).default;
results.tla = await new Promise((r) => setTimeout(() => r('awaited'), 5));
try { await import('./missing.mjs'); } catch (e) { results.missing = e.message.includes('Cannot find module'); }
