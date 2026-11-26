// Exercise the bundled artifact (not just the upstream npm modules).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { p256, hkdf, hmac, sha256, gcm } from './vendor/noble.js';

const encode = (s) => new TextEncoder().encode(s);

test('VRP/1 fallback matches the Rust and Go handshake vector', () => {
  assert.equal(p256.utils.randomSecretKey().length, 32);
  const viewer = new Uint8Array(32); viewer[31] = 1;
  const agent = new Uint8Array(32); agent[31] = 2;
  const shared = p256.getSharedSecret(viewer, p256.getPublicKey(agent, false), true).slice(1);
  const key = hkdf(sha256, shared, encode('s1|123456'), encode('velarouter/vrp1/session-key'), 32);
  assert.equal(Buffer.from(key).toString('hex'), '251d80ca3d587258fef045dad50e815efce55f634cad47f89a3a913be2b56238');
  assert.equal(Buffer.from(hmac(sha256, key, encode('agent|s1'))).toString('base64'), 'DgVzWu+hMaJiooLH+ErnGm8Kwqr52QHSHJLskvzikN0=');
});

test('fallback AES-GCM ciphertext opens with WebCrypto', async () => {
  const key = crypto.getRandomValues(new Uint8Array(32));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const aad = encode('vrp1');
  const message = encode('文件传输');
  const sealed = gcm(key, iv, aad).encrypt(message);
  const imported = await crypto.subtle.importKey('raw', key, 'AES-GCM', false, ['decrypt']);
  const opened = await crypto.subtle.decrypt({ name:'AES-GCM', iv, additionalData:aad }, imported, sealed);
  assert.deepEqual(new Uint8Array(opened), message);
});
