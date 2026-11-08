// URL imports keep the loop running until ns:module setDevBootComplete(true)
const { eq } = require('./harness');

(async () => {
  let ticks = 0;
  const timer = setInterval(() => ticks++, 10);
  const mod = await import('ns:module');

  const booting = await import(`${process.env.NS_TEST_HTTP}/slow/plain.mjs?boot`);
  eq(booting.value, 'plain', 'slow import during boot');
  eq(ticks > 5, true, 'timers fire during a boot import');

  mod.setDevBootComplete(true);
  ticks = 0;
  await import(`${process.env.NS_TEST_HTTP}/slow/plain.mjs?booted`);
  eq(ticks, 0, 'no timers during an import after boot');
  clearInterval(timer);
})().catch((e) => {
  console.error(e && e.stack || e);
  process.exit(1);
});
