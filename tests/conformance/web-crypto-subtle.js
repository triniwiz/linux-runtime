// crypto.subtle: digest, HMAC, AES-GCM, PBKDF2, HKDF
const { eq } = require('./harness');

const hex = (b) => Array.from(new Uint8Array(b), (x) => x.toString(16).padStart(2, '0')).join('');
const bytes = (h) => new Uint8Array(h.match(/../g).map((x) => parseInt(x, 16)));
const utf8 = (s) => new TextEncoder().encode(s);
const rejects = (p) => p.then(() => 'resolved', (e) => e.name);

(async () => {
  const s = crypto.subtle;
  eq(hex(await s.digest('SHA-256', utf8('abc'))), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'digest');
  eq(await rejects(s.digest('MD5', utf8('abc'))), 'NotSupportedError', 'unknown digest');
  eq(await rejects(s.digest('SHA-256', 'abc')), 'TypeError', 'digest of a string');

  const hmac = { name: 'HMAC', hash: 'SHA-256' };
  const jefe = await s.importKey('raw', utf8('Jefe'), hmac, true, ['sign', 'verify']);
  eq([jefe instanceof CryptoKey, jefe.type, jefe.extractable, jefe.usages, jefe.algorithm.length], [true, 'secret', true, ['sign', 'verify'], 32], 'CryptoKey');
  const tag = await s.sign('HMAC', jefe, utf8('what do ya want for nothing?'));
  eq(hex(tag), '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843', 'HMAC RFC 4231');
  eq(await s.verify('HMAC', jefe, tag, utf8('what do ya want for nothing?')), true, 'verify');
  eq(await s.verify('HMAC', jefe, tag, utf8('tampered')), false, 'verify tampered');
  const jwk = await s.exportKey('jwk', jefe);
  eq([jwk.kty, jwk.alg, jwk.k], ['oct', 'HS256', 'SmVmZQ'], 'jwk export');
  eq(hex(await s.exportKey('raw', await s.importKey('jwk', jwk, hmac, true, ['sign']))), hex(utf8('Jefe')), 'jwk import');
  eq(await rejects(s.generateKey(hmac, false, ['encrypt'])), 'SyntaxError', 'bad usage');

  const aes = await s.importKey('raw', bytes('feffe9928665731c6d6a8f9467308308'), 'AES-GCM', false, ['encrypt', 'decrypt']);
  const gcm = { name: 'AES-GCM', iv: bytes('cafebabefacedbaddecaf888'), additionalData: bytes('feedfacedeadbeeffeedfacedeadbeefabaddad2') };
  const plain = bytes('d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39');
  const sealed = await s.encrypt(gcm, aes, plain);
  eq(hex(sealed).slice(-32), '5bc94fbc3221a5db94fae95ae7121a47', 'AES-GCM tag');
  eq(hex(await s.decrypt(gcm, aes, sealed)), hex(plain), 'AES-GCM round trip');
  const broken = new Uint8Array(sealed);
  broken[0] ^= 1;
  eq(await rejects(s.decrypt(gcm, aes, broken)), 'OperationError', 'AES-GCM tampered');
  eq(await rejects(s.exportKey('raw', aes)), 'InvalidAccessError', 'not extractable');

  const password = await s.importKey('raw', utf8('password'), 'PBKDF2', false, ['deriveBits', 'deriveKey']);
  const pbkdf2 = { name: 'PBKDF2', hash: 'SHA-1', salt: utf8('salt'), iterations: 4096 };
  eq(hex(await s.deriveBits(pbkdf2, password, 160)), '4b007901b765489abead49d926f721d065a429c1', 'PBKDF2 RFC 6070');
  const derived = await s.deriveKey(pbkdf2, password, { name: 'AES-GCM', length: 256 }, true, ['encrypt']);
  eq([derived.algorithm.name, (await s.exportKey('raw', derived)).byteLength], ['AES-GCM', 32], 'deriveKey');

  const ikm = await s.importKey('raw', bytes('0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b'), 'HKDF', false, ['deriveBits']);
  const hkdf = { name: 'HKDF', hash: 'SHA-256', salt: bytes('000102030405060708090a0b0c'), info: bytes('f0f1f2f3f4f5f6f7f8f9') };
  eq(hex(await s.deriveBits(hkdf, ikm, 336)), '3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865', 'HKDF RFC 5869');

  let threw = false;
  try { new CryptoKey(); } catch (e) { threw = e instanceof TypeError; }
  eq(threw, true, 'CryptoKey is not constructible');
})().catch((e) => {
  console.error(e && e.stack || e);
  process.exit(1);
});
