import { createHash, createHmac } from 'node:crypto';

/** Exact LF-separated identity_token.rs request contract; fixture inputs only. */
export function fixtureControlSignature({ key, identity, agent, body, timestamp, nonce }) {
  const token = createHmac('sha256', identity).update(agent).digest('hex');
  const fields = [agent, token, timestamp, nonce, createHash('sha256').update(body).digest('hex')];
  return createHmac('sha256', key).update(fields.join('\n')).digest('hex');
}
