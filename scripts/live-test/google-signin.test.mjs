import test from 'node:test';
import assert from 'node:assert/strict';
import { createPublicKey } from 'node:crypto';
import { pkceS256, verifyPkce, parseAuthorizationUrl, buildClaims, makeSigningKey, signJwt, verifyJwt } from './google-signin.mjs';

const base = 'http://127.0.0.1:19876';
const authorization = { nonce: 'fixture-nonce', client_id: 'fixture-client' };
const person = { sub: '123456789', email: 'colleague@c.test' };
function authorizationUrl() {
  const url = new URL('/accounts.google.com/o/oauth2/v2/auth', base);
  for (const [key, value] of Object.entries({ state: 'fixture-state', nonce: 'fixture-nonce',
    code_challenge: pkceS256('fixture-verifier'), code_challenge_method: 'S256',
    redirect_uri: 'http://localhost:19877/api/login/google/callback', client_id: 'fixture-client', response_type: 'code', hd: 'c.test' })) {
    url.searchParams.set(key, value);
  }
  return url;
}

test('RS256 JWT and published RSA JWKS verify the same claims', () => {
  const key = makeSigningKey();
  const claims = buildClaims(person, authorization, { auth_time: 1234 }, 1234);
  const token = signJwt(claims, key.privateKey);
  const header = JSON.parse(Buffer.from(token.split('.')[0], 'base64url'));
  assert.deepEqual(header, { alg: 'RS256', typ: 'JWT', kid: key.jwk.kid });
  assert.equal(key.jwk.use, 'sig'); assert.equal(key.jwk.alg, 'RS256');
  assert.equal(key.jwk.d, undefined);
  assert.deepEqual(verifyJwt(token, createPublicKey({ key: key.jwk, format: 'jwk' })), claims);
});

test('JWT verification rejects changed payload, wrong key, changed signature and wrong algorithm', () => {
  const key = makeSigningKey(); const other = makeSigningKey();
  const token = signJwt(buildClaims(person, authorization), key.privateKey);
  const parts = token.split('.');
  const changedPayload = [parts[0], Buffer.from('{"sub":"999"}').toString('base64url'), parts[2]].join('.');
  assert.throws(() => verifyJwt(changedPayload, key.publicKey), /signature/);
  assert.throws(() => verifyJwt(token, other.publicKey), /signature/);
  const signature = Buffer.from(parts[2], 'base64url'); signature[0] ^= 1;
  assert.throws(() => verifyJwt(`${parts[0]}.${parts[1]}.${signature.toString('base64url')}`, key.publicKey), /signature/);
  const header = Buffer.from('{"alg":"none"}').toString('base64url');
  assert.throws(() => verifyJwt(`${header}.${parts[1]}.${parts[2]}`, key.publicKey), /header/);
});

test('PKCE S256 matches RFC 7636 vector and rejects incorrect or missing verifiers', () => {
  const verifier = 'dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk';
  const challenge = 'E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM';
  assert.equal(pkceS256(verifier), challenge);
  assert.equal(verifyPkce(verifier, challenge), true);
  for (const bad of ['wrong', '', null, undefined]) assert.equal(verifyPkce(bad, challenge), false);
  assert.equal(verifyPkce(verifier, `${challenge}=`), false);
});

test('authorization URL extracts state nonce challenge redirect and client without making requests', () => {
  const parsed = parseAuthorizationUrl(authorizationUrl(), base);
  assert.deepEqual(parsed, { state: 'fixture-state', nonce: 'fixture-nonce', code_challenge: pkceS256('fixture-verifier'),
    redirect_uri: 'http://localhost:19877/api/login/google/callback', client_id: 'fixture-client', hd_hint: 'c.test' });
});

test('authorization URL refuses external endpoints, duplicate/missing bindings, weak PKCE and external redirects', () => {
  const changes = [
    (u) => { u.hostname = 'accounts.google.com'; },
    (u) => { u.pathname = '/wrong'; },
    (u) => { u.searchParams.delete('nonce'); },
    (u) => { u.searchParams.append('state', 'second-state'); },
    (u) => { u.searchParams.set('code_challenge_method', 'plain'); },
    (u) => { u.searchParams.set('response_type', 'token'); },
    (u) => { u.searchParams.set('redirect_uri', 'http://localhost.evil.test/api/login/google/callback'); },
    (u) => { u.searchParams.set('redirect_uri', 'http://localhost:19877/other'); },
    (u) => { u.searchParams.set('redirect_uri', 'http://fixture:secret@localhost:19877/api/login/google/callback'); },
  ];
  for (const change of changes) { const url = authorizationUrl(); change(url); assert.throws(() => parseAuthorizationUrl(url, base)); }
});

test('claim builder supplies verified company identity and authorization bindings', () => {
  const claims = buildClaims(person, authorization, {}, 1700000000);
  assert.deepEqual(claims, { ...person, email_verified: true, hd: 'c.test', name: 'Fixture colleague',
    nonce: authorization.nonce, aud: authorization.client_id, iss: 'https://accounts.google.com', iat: 1700000000, exp: 1700000300 });
  assert.equal(Object.hasOwn(claims, 'auth_time'), false);
});

test('claim builder supports absent hd, per-user claims and malicious claim overrides without inference', () => {
  const source = { ...person, email_verified: false, hd: undefined, name: '測試同仁', auth_time: 100 };
  const claims = buildClaims(source, authorization, { nonce: 'wrong', aud: 'wrong-aud', iss: 'wrong-issuer', iat: 90, exp: 99 }, 100);
  assert.equal(Object.hasOwn(claims, 'hd'), false);
  assert.equal(claims.email_verified, false); assert.equal(claims.name, '測試同仁');
  assert.equal(claims.auth_time, 100); assert.equal(claims.nonce, 'wrong'); assert.equal(claims.aud, 'wrong-aud');
  assert.equal(claims.iss, 'wrong-issuer'); assert.equal(claims.iat, 90); assert.equal(claims.exp, 99);
  assert.equal(source.nonce, undefined);
  assert.equal(buildClaims(person, authorization, { hd: 'unknown.test' }).hd, 'unknown.test');
  assert.equal(Object.hasOwn(buildClaims(person, authorization, { hd: undefined }), 'hd'), false);
});
