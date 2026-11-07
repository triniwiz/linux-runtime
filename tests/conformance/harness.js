// Shared by every conformance test: a failing `eq` throws, which makes the runtime exit non-zero.
function eq(actual, expected, what) {
  const str = (v) => JSON.stringify(v, (_, x) => (typeof x === 'bigint' ? `${x}n` : x));
  const a = str(actual), e = str(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}
module.exports = { eq };
