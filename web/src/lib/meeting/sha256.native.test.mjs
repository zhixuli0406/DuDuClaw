// Run with Node 24+: node --experimental-strip-types --test src/lib/meeting/sha256.native.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { sha256Fallback, chunkSha256 } from './sha256.ts';
const bufferOf = bytes => bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
test('empty, abc and multi-block published SHA-256 vectors', () => {
  for (const [text, expected] of [
    ['', 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'],
    ['abc', 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'],
    ['abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq', '248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1'],
  ]) assert.equal(sha256Fallback(new TextEncoder().encode(text).buffer), expected);
});
test('binary values and padding boundaries match native SHA-256', async () => {
  const cases = [Buffer.from(Array.from({ length: 256 }, (_, i) => i)),
    ...[1, 55, 56, 63, 64, 65, 127, 128, 129, 255, 256, 1023, 1024, 4096, 65536].map(size => randomBytes(size))];
  for (const bytes of cases) {
    const buffer = bufferOf(bytes);
    const expected = createHash('sha256').update(bytes).digest('hex');
    assert.equal(sha256Fallback(buffer), expected);
    assert.equal(await chunkSha256(buffer), expected);
  }
});
test('full 8 MiB upload chunk matches native SHA-256', () => {
  const bytes = randomBytes(8 * 1024 * 1024);
  assert.equal(sha256Fallback(bufferOf(bytes)), createHash('sha256').update(bytes).digest('hex'));
});
test('HTTP LAN without WebCrypto uses the same digest', async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
  Object.defineProperty(globalThis, 'crypto', { value: {}, configurable: true });
  try { assert.equal(await chunkSha256(new TextEncoder().encode('abc').buffer), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'); }
  finally { Object.defineProperty(globalThis, 'crypto', descriptor); }
});
test('oversized fallback allocation is refused', () => {
  assert.throws(() => sha256Fallback(new ArrayBuffer(8 * 1024 * 1024 + 1)));
});
