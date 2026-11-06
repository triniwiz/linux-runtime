const { eq } = require('./harness');

const order = [];
setTimeout(() => order.push("t10"), 10);
setTimeout(() => order.push("t0"), 0);
Promise.resolve().then(() => order.push("micro"));
let n = 0;
const iv = setInterval(() => { if (++n === 3) clearInterval(iv); }, 1);
clearTimeout(setTimeout(() => order.push("cancelled"), 1));
setTimeout(() => {
  eq(order, ["micro", "t0", "t10"], "order");
  eq(n, 3, "interval");
}, 30);
