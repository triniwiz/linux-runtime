const pkg = require('pkg');
const { util } = require('@scope/util');
const data = require('./data.json');
module.exports = { pkg, util: util(), json: data.name, esm: require('./lib/math.mjs').add(1, 1), resolved: require.resolve('./data.json') === __dirname + '/data.json' };
globalThis.cjsResults = module.exports;
