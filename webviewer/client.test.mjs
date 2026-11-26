import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import vm from 'node:vm';

// Isolate the transport code from the browser UI without opening a real socket.
const source = await readFile(new URL('./app.js', import.meta.url), 'utf8');
const transport = source.split('const canvas = $(\'screen\');')[0];
const { VrClient, AsyncQueue } = vm.runInNewContext(`${transport}\n({ VrClient, AsyncQueue })`, {
  TextEncoder, TextDecoder, WebSocket: { OPEN: 1 },
});

function client(crypto) {
  return new VrClient({ code: 'test-code', sessionId: 'test', crypto });
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

test('a socket close unblocks a waiting handshake or frame read', async () => {
  const queue = new AsyncQueue();
  const waiting = queue.next();
  queue.close();
  queue.push('ignored');
  assert.equal(await waiting, null);
  assert.equal(await queue.next(), null);
  const connection = client();
  const handshake = connection.nextJson();
  connection.abort();
  await assert.rejects(handshake, /连接已关闭/);
});

test('WebCrypto completion cannot reorder pointer-down and release frames', async () => {
  const first = deferred();
  const entered = deferred();
  const sent = [];
  let counter = 0;
  const connection = client({
    async encrypt(_key, iv, bytes) {
      const n = new DataView(iv.buffer).getBigUint64(4);
      counter += 1;
      if (n === 0n) {
        entered.resolve();
        await first.promise;
      }
      return bytes; // Fake ciphertext for testing order, not a crypto test.
    },
  });
  connection.aes = {};
  connection.ws = { readyState: 1, send: (frame) => sent.push(frame) };
  const down = { t: 'input', event: { k: 'mouse_button', down: true } };
  const release = { t: 'input', event: { k: 'release_all' } };
  const pending = [connection.sendControl(down), connection.sendControl(release)];
  await entered.promise;
  assert.equal(counter, 1);
  assert.equal(sent.length, 0);
  first.resolve();
  await Promise.all(pending);
  assert.deepEqual(sent.map((frame) => new DataView(frame.buffer).getBigUint64(0)), [0n, 1n]);
  assert.deepEqual(sent.map((frame) => JSON.parse(new TextDecoder().decode(frame.subarray(9)))), [down, release]);
});
