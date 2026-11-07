const { eq } = require('./harness');
const fixtures = `${__dirname}/../../runtime/tests/fixtures/modules`;

// require() of an ES module without top-level await, and packages' require condition
const results = require(`${fixtures}/entry.cjs`);
eq([results.pkg, results.util, results.json, results.esm, results.resolved], ['pkg:cjs', 'scoped', 'fixture', 2, true], 'require(esm)');
const ns = require('./esm-fixtures/cycle-a.mjs');
eq([ns.a, ns.fromB()], ['a', 'b sees a'], 'module namespace');
