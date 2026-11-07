// import() inside CommonJS, ns:module and HTTP modules
const { eq } = require('./harness');

(async () => {
  const ns = await import('./esm-fixtures/cjs-target.mjs');
  eq(ns.value, 'from esm', 'import() from CommonJS');
  eq(String.raw`import('x')` + /* import('y') */ '', "import('x')", 'strings and comments untouched');
  const mod = await import('ns:module');
  eq(['configureLoader', 'invalidateModules', 'getLoadedModuleUrls', 'createRequire'].every((k) => typeof mod[k] === 'function'), true, 'ns:module');
  eq(mod.getLoadedModuleUrls().some((u) => u.endsWith('/esm-fixtures/cjs-target.mjs')), true, 'getLoadedModuleUrls');
  eq(mod.createRequire(__filename)('./harness').eq === eq, true, 'createRequire');
  const remote = await import(`${process.env.NS_TEST_HTTP}/remote.mjs`);
  eq(remote.value, 'remote+dep', 'HTTP module with a relative import');
  eq(remote.url, `${process.env.NS_TEST_HTTP}/remote.mjs`, 'import.meta.url of an HTTP module');
})().catch((e) => {
  console.error(e && e.stack || e);
  process.exit(1);
});
