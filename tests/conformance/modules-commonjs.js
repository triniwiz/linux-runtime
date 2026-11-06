const { eq } = require('./harness');
const fixtures = `${__dirname}/../../runtime/tests/fixtures/modules`;

// bare specifiers resolve from the requiring file's node_modules
const local = require('module-fixture');
eq(local, 'pkg:cjs', 'package exports (require condition)');
eq(require(`${fixtures}/data.json`).name, 'fixture', 'json');
eq(require.resolve(`${fixtures}/data.json`).endsWith('/data.json'), true, 'require.resolve');
eq(require('./harness') === require('./harness.js'), true, 'module cache');
let missing = false;
try { require('./does-not-exist'); } catch (e) { missing = /Cannot find module/.test(e.message); }
eq(missing, true, 'missing module');
eq([typeof __filename, __dirname.endsWith('/tests/conformance')], ['string', true], 'module locals');
