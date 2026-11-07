const path = require('./lib/path-helper.js');
exports.kind = 'commonjs';
exports.greet = (who) => `hello ${who} from ${path.base(__filename)}`;
