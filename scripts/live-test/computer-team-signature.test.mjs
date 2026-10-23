import assert from 'node:assert/strict';
import test from 'node:test';
import { fixtureControlSignature } from './computer-team-signature.mjs';

test('fixture request signatures follow the actual LF contract golden vector', () => {
  const input = { key: 'fixture-only-key', identity: Buffer.alloc(32, 7), agent: 'alice',
    body: '{"op":"start","task":"synthetic"}', timestamp: '1700000000', nonce: '0123456789abcdef0123456789abcdef' };
  // Independently computed with Python hmac/hashlib and four real LF bytes.
  assert.equal(fixtureControlSignature(input), '2effc23a62ff1d9fa4b3d05b459802ec103bbcab3f72fbe60a25e4be1f4bea35');
  assert.notEqual(fixtureControlSignature({ ...input, body: input.body + ' ' }), fixtureControlSignature(input));
  assert.notEqual(fixtureControlSignature({ ...input, agent: 'bob' }), fixtureControlSignature(input));
});
