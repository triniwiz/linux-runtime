const { eq } = require('./harness');

const bytes = new TextEncoder().encode('héllo €');
eq(Array.from(bytes), [104, 195, 169, 108, 108, 111, 32, 226, 130, 172], 'encode');
eq(new TextDecoder().decode(bytes), 'héllo €', 'decode');
eq(new TextDecoder().decode(bytes.buffer), 'héllo €', 'decode buffer');
let threw = false; try { new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array([0xff])); } catch { threw = true; }
eq(threw, true, 'fatal');
const dest = new Uint8Array(4);
eq(new TextEncoder().encodeInto('a€', dest), { read: 2, written: 4 }, 'encodeInto');
eq([btoa('hello'), atob('aGVsbG8=')], ['aGVsbG8=', 'hello'], 'base64');
