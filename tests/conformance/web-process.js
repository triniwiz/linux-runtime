const { eq } = require('./harness');

eq([process.platform, typeof process.pid, Array.isArray(process.argv)], ['linux', 'number', true], 'process');
process.env.NS_TEST_VAR = 'value';
eq([process.env.NS_TEST_VAR, 'NS_TEST_VAR' in process.env, typeof getenv('NS_TEST_VAR')], ['value', true, 'object'], 'env visible to C');
delete process.env.NS_TEST_VAR;
eq(process.env.NS_TEST_VAR, undefined, 'env delete');
eq(process.cwd().startsWith('/'), true, 'cwd');
eq(typeof process.hrtime.bigint(), 'bigint', 'hrtime');
let ticked = false;
process.nextTick(() => (ticked = true));
Promise.resolve().then(() => eq(ticked, true, 'nextTick'));
