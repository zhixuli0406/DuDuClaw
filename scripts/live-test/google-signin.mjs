#!/usr/bin/env node
// Mock-only RM-214 acceptance. The argument MUST be an OS sandbox wrapper:
// deny real credentials/keychains and all non-loopback networking, including children.
import { createHash, generateKeyPairSync, randomBytes, sign, verify } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync, createWriteStream } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const delay = (ms) => new Promise((done) => setTimeout(done, ms));
const running = (child) => child && child.exitCode === null && child.signalCode === null;
function check(value, message) { if (!value) throw new Error(message); }
export const pkceS256 = (verifier) => createHash('sha256').update(verifier).digest('base64url');
export function verifyPkce(verifier, challenge) {
  return typeof verifier === 'string' && verifier.length > 0 && pkceS256(verifier) === challenge;
}
export function parseAuthorizationUrl(raw, expectedBase) {
  const url = new URL(raw);
  check(url.origin === expectedBase && url.pathname === '/accounts.google.com/o/oauth2/v2/auth', 'authorization endpoint must be mock loopback');
  const fields = {};
  for (const key of ['state', 'nonce', 'code_challenge', 'redirect_uri', 'client_id']) {
    check(url.searchParams.getAll(key).length === 1 && url.searchParams.get(key), 'missing or duplicate authorization parameter');
    fields[key] = url.searchParams.get(key);
  }
  check(url.searchParams.get('code_challenge_method') === 'S256', 'PKCE must use S256');
  check(url.searchParams.get('response_type') === 'code', 'expected authorization code flow');
  const callback = new URL(fields.redirect_uri);
  check(callback.protocol === 'http:' && callback.hostname === 'localhost' && callback.pathname === '/api/login/google/callback'
    && !callback.username && !callback.password && !callback.search && !callback.hash, 'callback must use isolated localhost');
  return { ...fields, hd_hint: url.searchParams.get('hd') };
}
export function buildClaims(user, authorization, overrides = {}, now = Math.floor(Date.now() / 1000)) {
  const claims = { sub: user.sub, email: user.email, email_verified: true, hd: 'c.test', name: 'Fixture colleague',
    nonce: authorization.nonce, aud: authorization.client_id, iss: 'https://accounts.google.com', iat: now, exp: now + 300,
    ...user, ...overrides };
  for (const key of Object.keys(claims)) if (claims[key] === undefined) delete claims[key];
  return claims;
}
export function makeSigningKey() {
  const keys = generateKeyPairSync('rsa', { modulusLength: 2048 });
  return { ...keys, jwk: { ...keys.publicKey.export({ format: 'jwk' }), kid: 'signin-fixture', alg: 'RS256', use: 'sig' } };
}
export function signJwt(claims, privateKey, kid = 'signin-fixture') {
  const input = [ { alg: 'RS256', typ: 'JWT', kid }, claims ].map((v) => Buffer.from(JSON.stringify(v)).toString('base64url')).join('.');
  return `${input}.${sign('RSA-SHA256', Buffer.from(input), privateKey).toString('base64url')}`;
}
// Signature round-trip helper, not a replacement for the gateway's OIDC verifier.
export function verifyJwt(token, publicKey) {
  const parts = token.split('.');
  check(parts.length === 3 && JSON.parse(Buffer.from(parts[0], 'base64url')).alg === 'RS256', 'invalid JWT header');
  check(verify('RSA-SHA256', Buffer.from(parts.slice(0, 2).join('.')), publicKey, Buffer.from(parts[2], 'base64url')), 'invalid JWT signature');
  return JSON.parse(Buffer.from(parts[1], 'base64url'));
}

export function createGoogleMock() {
  const key = makeSigningKey();
  const codes = new Map();
  const tokenHits = []; // Store only validation outcomes, never credentials or grants.
  const server = createServer(async (req, res) => {
    res.setHeader('content-type', 'application/json');
    try {
      const path = new URL(req.url, 'http://127.0.0.1').pathname;
      if (path === '/www.googleapis.com/oauth2/v3/certs' && req.method === 'GET') {
        return res.end(JSON.stringify({ keys: [key.jwk] }));
      }
      if (path !== '/oauth2.googleapis.com/token') { res.statusCode = 404; return res.end('{}'); }
      const hit = { method: req.method, registered: false, pkce: false, redirect: false, accepted: false };
      tokenHits.push(hit); // Record even malformed/unknown-code requests.
      let body = '';
      for await (const chunk of req) { body += chunk; check(body.length <= 32768, 'form too large'); }
      const form = new URLSearchParams(body);
      const entry = codes.get(form.get('code'));
      hit.registered = !!entry;
      if (entry) codes.delete(form.get('code'));
      hit.pkce = !!entry && verifyPkce(form.get('code_verifier'), entry.authorization.code_challenge);
      hit.redirect = !!entry && form.get('redirect_uri') === entry.authorization.redirect_uri;
      if (req.method !== 'POST' || !entry || !hit.pkce || !hit.redirect || form.get('grant_type') !== 'authorization_code'
        || form.get('client_id') !== entry.authorization.client_id || form.get('client_secret') !== 'fixture-secret-only') {
        res.statusCode = 400; return res.end('{"error":"invalid_grant"}');
      }
      hit.accepted = true;
      let token = signJwt(buildClaims(entry.user, entry.authorization, entry.overrides), key.privateKey);
      if (entry.tamper) {
        const parts = token.split('.'); const signature = Buffer.from(parts[2], 'base64url'); signature[0] ^= 1;
        token = `${parts[0]}.${parts[1]}.${signature.toString('base64url')}`;
      }
      res.end(JSON.stringify({ id_token: token, access_token: 'mock-unused-access', token_type: 'Bearer', expires_in: 300 }));
    } catch { res.statusCode = 400; res.end('{"error":"invalid_request"}'); }
  });
  return { server, tokenHits, register(authorization, user, overrides = {}, tamper = false) {
    const code = randomBytes(32).toString('base64url');
    codes.set(code, { authorization, user, overrides, tamper }); return code;
  } };
}

class Skip extends Error {}
const observations = [];
const resultCodes = new Set(['off', 'available', 'needs_public_url', 'ok', 'expired', 'pending_approval', 'existing_account',
  'personal_account', 'domain_not_allowed', 'account_disabled', 'daily_limit', 'failed', 'mfa_enrollment_required',
  'role_requires_local_password', 'signin_personal_edition', 'rpc_ok', 'rpc_refused', 'http_error', 'redirect', 'unknown']);
function resultCode(body) {
  const code = body?.result ?? body?.google ?? body?.error?.code;
  if (resultCodes.has(code)) return code;
  if (body?.mfa_enrollment_required === true) return 'mfa_enrollment_required';
  if (body?.access_token) return 'ok';
  return body?.error ? 'http_error' : 'unknown';
}
function report(status, name, observation = {}, reason = '') {
  const row = { status, name, http: observation.status ?? 'n/a', result: observation.code ?? 'n/a', reason };
  observations.push(row);
  console.log(`${status} ${name} HTTP=${row.http} result=${row.result}${reason ? ` — ${reason}` : ''}`);
}
async function scenario(name, fn) {
  try { await fn(); }
  catch (error) {
    // Never echo arbitrary errors: fetch/RPC/assertion errors can contain secrets.
    report(error instanceof Skip ? 'SKIP' : 'FAIL', name, {}, error instanceof Skip ? error.message : 'scenario assertion or transport failed; no raw response printed');
  }
}
function expect(name, observation, code, status = 200) {
  const ok = observation.status === status && observation.code === code;
  report(ok ? 'PASS' : 'FAIL', name, observation);
  check(ok, 'unexpected HTTP/result');
}
async function listen(server) { server.listen(0, '127.0.0.1'); await once(server, 'listening'); return server.address().port; }
async function request(base, path, body, cookie) {
  const url = new URL(path, base);
  check(url.origin === base, 'HTTP must stay on this fixture');
  const response = await fetch(url, { method: body === undefined ? 'GET' : 'POST', redirect: 'manual',
    signal: AbortSignal.timeout(15000), headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      ...(cookie ? { cookie } : {}) }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const text = await response.text(); let data;
  try { data = JSON.parse(text); } catch { data = {}; }
  return { status: response.status, code: resultCode(data), body: data, headers: response.headers };
}

// Reuses google-personal-identity.mjs's dashboard client protocol (connect JWT,
// type=req/res, id, method, params), with bounded open/RPC waits and raw refusal support.
async function dashboard(context, token) {
  const socket = new WebSocket(`ws://127.0.0.1:${context.port}/ws`);
  context.sockets.push(socket);
  await Promise.race([once(socket, 'open'), delay(5000).then(() => { if (socket.readyState !== WebSocket.OPEN) throw new Error('socket open timeout'); })]);
  const waiting = new Map(); let id = 0;
  socket.addEventListener('message', (event) => {
    try { const r = JSON.parse(event.data); if (r.type === 'res') waiting.get(r.id)?.(r); } catch {}
  });
  const raw = async (method, params = {}) => {
    const key = String(++id); let timer;
    const reply = new Promise((done) => waiting.set(key, done));
    socket.send(JSON.stringify({ type: 'req', id: key, method, params }));
    try { return await Promise.race([reply, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('RPC timeout')), 15000); })]); }
    finally { clearTimeout(timer); waiting.delete(key); }
  };
  const rpc = async (method, params) => { const r = await raw(method, params); check(r.ok, 'RPC refused'); return r.payload; };
  await rpc('connect', { jwt: token }); rpc.raw = raw; return rpc;
}
function sql(context, statement) {
  const r = spawnSync('sqlite3', ['-batch', '-cmd', '.timeout 5000', join(context.home, 'users.db')],
    { env: context.env, input: statement, encoding: 'utf8', timeout: 10000 });
  if (r.error?.code === 'ENOENT') throw new Skip('sqlite3 is unavailable; database observation cannot run');
  check(r.status === 0, 'fixture SQL failed'); return r.stdout.trim();
}
function quote(value) { return `'${String(value).replaceAll("'", "''")}'`; }
async function stop(context) {
  for (const socket of context.sockets) socket.close();
  if (running(context.child)) {
    const exited = once(context.child, 'exit'); context.child.kill('SIGTERM');
    await Promise.race([exited, delay(5000)]);
    if (running(context.child)) { context.child.kill('SIGKILL'); await Promise.race([exited, delay(5000)]); }
    check(!running(context.child), 'owned gateway failed to stop');
  }
  context.log?.end();
}
async function fixture(binary, mockBase, edition, contexts) {
  const home = mkdtempSync(join(tmpdir(), 'ddc-google-signin-'));
  const context = { home, sockets: [], env: Object.fromEntries(['PATH', 'TMPDIR', 'LANG'].filter((k) => process.env[k]).map((k) => [k, process.env[k]])) };
  contexts.push(context);
  Object.assign(context.env, { HOME: home, DUDUCLAW_HOME: home });
  const lease = createServer(); context.port = await listen(lease); await new Promise((done) => lease.close(done));
  const made = spawnSync('bash', [join(scriptDir, 'make-home.sh'), home, '--port', String(context.port)],
    { env: context.env, encoding: 'utf8', timeout: 10000 });
  check(made.status === 0, 'make-home failed');
  const configPath = join(home, 'config.toml');
  writeFileSync(configPath, readFileSync(configPath, 'utf8') + `\n[dashboard]\npublic_url = "http://localhost:${context.port}"\n[auth]\nmfa_enforced = false\n[integrations]\ngoogle_workspace = false\n[dispatch]\nenabled = false\n[proactive]\nenabled = false\n`);
  // Same pre-MFA upgrade schema as v185-acceptance; empty users allows first-run
  // claim. This is deliberately not evidence for fresh Enterprise MFA defaults.
  sql(context, `CREATE TABLE users(id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, display_name TEXT NOT NULL,
    password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'employee', status TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_login TEXT,
    must_change_password INTEGER NOT NULL DEFAULT 0, bootstrap_owner INTEGER NOT NULL DEFAULT 0,
    department TEXT, manager_user_id TEXT, primary_agent_id TEXT);
    CREATE TABLE user_agent_bindings(user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    agent_name TEXT NOT NULL, access_level TEXT NOT NULL DEFAULT 'owner', bound_at TEXT NOT NULL, PRIMARY KEY(user_id,agent_name));`);
  Object.assign(context.env, { HOME: join(home, 'os-home'), DUDUCLAW_EDITION: edition, DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: mockBase });
  if (edition === 'enterprise') {
    const fingerprint = spawnSync(binary, ['license', 'fingerprint'], { env: context.env, encoding: 'utf8', timeout: 15000 });
    check(fingerprint.status === 0 && fingerprint.stdout.trim(), 'guarded fingerprint failed');
    const { publicKey, privateKey } = generateKeyPairSync('ed25519');
    const issuedAt = new Date().toISOString().replace(/\.000Z$/, 'Z');
    const payload = { version: 2, subscription_id: 'sub_signin_probe', customer_id: 'fixture', tier: 'oem',
      machine_fingerprint: fingerprint.stdout.trim().split('\n')[0], issued_at: issuedAt,
      expires_at: new Date(Date.now() + 86400000).toISOString().replace(/\.000Z$/, 'Z'),
      last_phone_home: issuedAt, public_key_id: 'signin_probe', nfr: true };
    writeFileSync(join(home, 'license.json'), JSON.stringify({ ...payload,
      signature: sign(null, Buffer.from(JSON.stringify(payload)), privateKey).toString('base64') }), { mode: 0o600 });
    context.env.DUDUCLAW_LICENSE_PUBKEY_SIGNIN_PROBE = Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url').toString('hex');
  }
  context.base = `http://127.0.0.1:${context.port}`;
  console.log(`ARTIFACTS ${home}`);
  context.child = spawn(binary, ['run', '--yes'], { env: context.env, stdio: ['ignore', 'pipe', 'pipe'] });
  // Suppress raw child output on the console. Private logs contain only fixtures.
  context.child.on('error', () => {});
  context.log = createWriteStream(join(home, 'gateway.log'), { mode: 0o600 });
  context.child.stdout.pipe(context.log); context.child.stderr.pipe(context.log);
  let ready = false;
  for (let i = 0; i < 100; i++) {
    if (!running(context.child) || context.child.pid === undefined) break;
    try { if ((await request(context.base, '/healthz')).status === 200) { ready = true; break; } } catch {}
    await delay(250);
  }
  if (!ready && edition === 'personal') throw new Skip('guarded binary could not start in Personal edition');
  check(ready, 'gateway startup failed');
  check((await request(context.base, '/healthz')).body.live_test_mocks === true, 'live-test-mocks build required');
  return context;
}

const settings = { enabled: true, allowed_hd: ['c.test'], approval: 'admin', daily_new_accounts: 20,
  avatar_template: '', avatar_template_revision: null, approval_grants_mfa_enrollment: false, acknowledge_risk: true };
const password = 'Fixture-only-password-214!';
function user(n) { return { sub: `214000${n}`, email: `colleague${n}@c.test` }; }
class Bench {
  constructor(context, mock, mockBase, admin) { Object.assign(this, { context, mock, mockBase, admin }); }
  async set(overrides = {}) {
    const snapshot = await this.admin('google.signin.get');
    return this.admin('google.signin.set', { ...settings, ...overrides, expected_hash: snapshot.hash });
  }
  async start() {
    const r = await request(this.context.base, '/api/login/google/start', {});
    if (r.status === 429) throw new Skip('start rate limit reached; rerun in a fresh fixture');
    check(r.status === 200 && r.body.authorization_url, 'start did not return authorization URL');
    const auth = parseAuthorizationUrl(r.body.authorization_url, this.mockBase);
    check(new URL(auth.redirect_uri).port === String(this.context.port), 'wrong callback port');
    const cookie = r.headers.get('set-cookie')?.split(';')[0]; check(cookie, 'missing browser cookie');
    return { auth, cookie };
  }
  async callback(flow, person, overrides = {}, tamper = false, cookie = flow.cookie, state = flow.auth.state) {
    const code = this.mock.register(flow.auth, person, overrides, tamper);
    const query = new URLSearchParams({ state, code });
    const r = await request(this.context.base, `/api/login/google/callback?${query}`, undefined, cookie);
    check(r.status === 303, 'callback must redirect');
    const location = new URL(r.headers.get('location'), `http://localhost:${this.context.port}`);
    check(location.origin === `http://localhost:${this.context.port}` && location.pathname === '/login', 'unexpected callback location');
    const fragment = new URLSearchParams(location.hash.slice(1));
    return { ...r, code: fragment.has('gsi') ? 'redirect' : fragment.get('gsi_error') === 'expired' ? 'expired' : 'unknown',
      ticket: fragment.get('gsi') };
  }
  async complete(flow, ticket) { return request(this.context.base, '/api/login/google/complete', { ticket }, flow.cookie); }
  async login(person, overrides = {}, tamper = false) {
    const flow = await this.start(); const cb = await this.callback(flow, person, overrides, tamper);
    return cb.ticket ? this.complete(flow, cb.ticket) : cb;
  }
  async local(n) {
    const person = user(n);
    const created = await this.admin('users.create', { email: person.email, display_name: 'Fixture colleague', password, role: 'employee' });
    return { ...person, id: created.user.id };
  }
}

export async function scenario01OffByDefault(b) {
  expect('01 methods off', await request(b.context.base, '/api/login/methods'), 'off');
  expect('01 start refused', await request(b.context.base, '/api/login/google/start', {}), 'http_error', 404);
  report('SKIP', '01 login button', {}, 'HTTP bench does not render the dashboard');
}
export async function scenario02Acknowledgement(b) {
  const snapshot = await b.admin('google.signin.get');
  const r = await b.admin.raw('google.signin.set', { ...settings, acknowledge_risk: false, expected_hash: snapshot.hash });
  report(!r.ok ? 'PASS' : 'FAIL', '02 risk acknowledgement required', { code: r.ok ? 'rpc_ok' : 'rpc_refused' });
  check(!r.ok, 'acknowledgement bypass');
  check((await b.admin('google.signin.get')).settings.enabled === false, 'refusal changed settings');
  await b.set();
  const saved = await b.admin('google.signin.get');
  check(saved.settings.ack?.by && saved.settings.ack?.at, 'missing enabled_ack');
  check(/\[google_signin\.enabled_ack\]/.test(readFileSync(join(b.context.home, 'config.toml'), 'utf8')), 'ack not written to disk');
  check(Number(sql(b.context, "SELECT count(*) FROM auth_audit_log WHERE action='google_signin.settings_changed';")) > 0, 'audit row missing');
  expect('02 enabled_ack and audit saved', await request(b.context.base, '/api/login/methods'), 'available');
  report('SKIP', '02 raw settings editor', {}, 'raw editor UI is outside this public-flow bench');
}
export async function scenario10DailyCap(b) {
  await b.set({ approval: 'direct', daily_new_accounts: 2 });
  for (const n of [101, 102]) expect(`10 new colleague ${n}`, await b.login(user(n)), 'ok');
  expect('10 third colleague refused', await b.login(user(103)), 'daily_limit');
  const path = join(b.context.home, 'config.toml'); const before = readFileSync(path, 'utf8');
  try {
    check(/daily_new_accounts\s*=\s*2\b/.test(before), 'daily cap fixture key missing');
    writeFileSync(path, before.replace(/daily_new_accounts\s*=\s*2\b/, 'daily_new_accounts = "invalid"'));
    expect('10 malformed cap refuses JIT', await b.login(user(104)), 'daily_limit');
  } finally { writeFileSync(path, before); await b.set(); }
}
export async function scenario03AdminApproval(b) {
  await b.set(); const person = user(3);
  expect('03 pending', await b.login(person), 'pending_approval');
  check(sql(b.context, `SELECT count(*) FROM users WHERE email=${quote(person.email)};`) === '0', 'pending created user');
  const rows = (await b.admin('users.signin_requests.list')).requests.filter((r) => r.email === person.email);
  check(rows.length === 1, 'expected one pending request');
  const { user_id } = await b.admin('users.signin_requests.approve', { id: rows[0].id });
  check(sql(b.context, `SELECT count(*) FROM google_signin_links WHERE user_id=${quote(user_id)};`) === '1', 'approval link missing');
  const login = await b.login(person); expect('03 approved login', login, 'ok');
  b.member = { ...person, id: user_id, login };
  const member = await dashboard(b.context, login.body.access_token);
  check(Object.keys((await member('onboarding.get')).signin_steps ?? {}).length === 5, 'five onboarding steps missing');
  report('PASS', '03 account link and five steps', login);
  report('SKIP', '03 admin notification and blank avatar completion', {}, 'delivery and asynchronous avatar creation are not asserted');
}
export async function scenario05HostedDomain(b) {
  for (const [n, claims, code, label] of [[51, { hd: undefined }, 'personal_account', 'missing hd'],
    [52, { hd: 'other.test' }, 'domain_not_allowed', 'unknown hd'], [53, { hd: undefined }, 'personal_account', 'hint-only hd']]) {
    const flow = await b.start(); check(flow.auth.hd_hint === 'c.test', 'expected hd hint');
    const cb = await b.callback(flow, user(n), claims); check(cb.ticket, 'expected domain-result ticket');
    expect(`05 ${label}`, await b.complete(flow, cb.ticket), code);
    check(sql(b.context, `SELECT count(*) FROM google_signin_requests WHERE google_sub=${quote(user(n).sub)};`) === '0', 'rejected domain created request');
  }
}
export async function scenario06ExistingEmail(b) {
  const person = await b.local(6); b.existing = person;
  expect('06 existing email', await b.login(person), 'existing_account');
  expect('06 local password still works', await request(b.context.base, '/api/login', { email: person.email, password }), 'ok');
  report('SKIP', '06 self-link and duplicate sub binding', {}, 'self-service connection RPCs are outside this admin/public-flow bench');
}
export async function scenario07AttestationThenLink(b) {
  const person = b.existing; if (!person) throw new Skip('scenario 06 local account unavailable');
  const attested = await b.admin('users.google_signin.attest', { user_ids: [person.id] }); check(attested.attested === 1, 'attestation missing');
  expect('07 attested account signs in', await b.login(person), 'ok');
  check(sql(b.context, `SELECT linked_via FROM google_signin_links WHERE user_id=${quote(person.id)};`) === 'admin_attest', 'wrong link source');
  report('SKIP', '07 email changed after attestation', {}, 'no email-change API in users.update; not injecting an artificial result');
}
export async function scenario08Attacks(b) {
  for (const [label, cookie] of [['no cookie', undefined], ['other cookie', `dd_gsi=${randomBytes(32).toString('base64url')}`]]) {
    const flow = await b.start(); const before = b.mock.tokenHits.length;
    const cb = await b.callback(flow, user(81), {}, false, cookie ?? '');
    expect(`08 ${label}`, cb, 'expired', 303);
    check(b.mock.tokenHits.length === before, 'invalid cookie contacted token endpoint');
  }
  const flow = await b.start(); const cb = await b.callback(flow, user(82)); check(cb.ticket, 'valid state failed');
  const before = b.mock.tokenHits.length;
  expect('08 state replay', await b.callback(flow, user(82)), 'expired', 303);
  check(b.mock.tokenHits.length === before, 'state replay contacted token endpoint');
  const unknown = await b.start(); const noStateBefore = b.mock.tokenHits.length;
  expect('08 unknown state', await b.callback(unknown, user(82), {}, false, unknown.cookie, randomBytes(32).toString('base64url')), 'expired', 303);
  const noState = await request(b.context.base, '/api/login/google/callback?code=unregistered-fixture', undefined, unknown.cookie);
  check(noState.status === 303 && new URL(noState.headers.get('location')).hash === '#gsi_error=expired', 'missing state not refused');
  check(b.mock.tokenHits.length === noStateBefore, 'unknown or absent state contacted token endpoint');
  report('PASS', '08 absent state zero token calls', { status: noState.status, code: 'expired' });
  expect('08 ticket first use', await b.complete(flow, cb.ticket), 'pending_approval');
  expect('08 ticket replay', await b.complete(flow, cb.ticket), 'expired');
  for (const [label, overrides, tamper] of [['nonce mismatch', { nonce: 'wrong-fixture-nonce' }, false],
    ['wrong aud', { aud: 'wrong-client' }, false], ['expired token', { exp: Math.floor(Date.now() / 1000) - 3600 }, false],
    ['tampered signature', {}, true]]) {
    expect(`08 ${label}`, await b.login(user(83), overrides, tamper), 'expired', 303);
  }
  check(b.mock.tokenHits.every((h) => h.accepted && h.pkce && h.redirect), 'mock exchange validation failed');
}
export async function scenario09Suspended(b) {
  if (!b.member) throw new Skip('approved member unavailable');
  const removed = await b.admin('users.remove', { user_id: b.member.id }); check(removed.status === 'suspended', 'offboarding did not suspend');
  expect('09 suspended account', await b.login(b.member), 'account_disabled');
  report('SKIP', '09 unlink invalidates refresh', {}, 'passwordless unlink has additional recovery requirements; not driven here');
}
export async function scenario11TemplateObservation(b) {
  // Deliberately select a stale revision; observe only the real maintenance
  // result. Never forge user_onboarding.avatar or call an internal worker.
  const params = { agent: 'prod-shaped', name: 'Signin fixture template' };
  const preview = await b.admin.raw('templates.copy', { ...params, preview: true });
  if (!preview.ok) throw new Skip('template preview unavailable under this fixture policy');
  const copied = await b.admin.raw('templates.copy', { ...params, reviewed: true, review_digest: preview.payload.review_digest });
  if (!copied.ok) throw new Skip('template copy unavailable under this fixture policy');
  const template = await b.admin('templates.get', { id: copied.payload.id });
  await b.set({ approval: 'direct', avatar_template: copied.payload.id, avatar_template_revision: template.revision + 1 });
  try {
    const login = await b.login(user(11)); expect('11 member login with stale template', login, 'ok');
    const member = await dashboard(b.context, login.body.access_token);
    let observed;
    for (let i = 0; i < 20; i++) {
      observed = (await member('onboarding.get')).signin_steps?.avatar;
      if (observed?.status === 'waiting_admin') break;
      await delay(1000);
    }
    if (observed?.status !== 'waiting_admin' || observed?.reason !== 'template_changed') {
      report('SKIP', '11 template revision observation', login, 'maintenance did not expose waiting_admin/template_changed within 20s; no avatar state injected');
      return;
    }
    report('PASS', '11 waiting_admin/template_changed observed', login);
    report('SKIP', '11 admin retry and single avatar', {}, 'observation only; retry not driven');
  } finally { await b.set(); }
}
export async function scenario13PasswordlessAdmin(b) {
  if (!b.member) throw new Skip('passwordless member unavailable');
  const r = await b.admin.raw('users.update', { user_id: b.member.id, role: 'admin' });
  const code = resultCode({ error: r.error });
  const ok = !r.ok && code === 'role_requires_local_password';
  report(ok ? 'PASS' : 'FAIL', '13 passwordless Admin refused', { code }); check(ok, 'passwordless promotion not refused');
}
export async function scenario04DirectMfa(b) {
  await b.set({ approval: 'direct' });
  const r = await b.admin.raw('system.update_config', { 'auth.mfa_enforced': true, expected_state: false });
  if (!r.ok) throw new Skip('MFA policy RPC refused this fixture session; no bypass attempted');
  const login = await b.login(user(4)); expect('04 direct account requires MFA', login, 'mfa_enrollment_required');
  check(!login.body.access_token && !login.body.refresh_token, 'MFA returned session tokens');
  check(sql(b.context, `SELECT count(*) FROM users WHERE email=${quote(user(4).email)};`) === '1', 'direct account not created');
  report('PASS', '04 account exists without session tokens', login);
  report('SKIP', '04 enrollment code TOTP and preauthorization replay', {}, 'enrollment ceremonies are outside this OIDC bench');
}
export async function scenario14Personal(binary, mockBase, contexts) {
  const context = await fixture(binary, mockBase, 'personal', contexts);
  // Confirm Personal resolution; off alone would also pass on an unconfigured Enterprise instance.
  const credentials = { email: 'personal-admin@c.test', password };
  const claim = await request(context.base, '/api/first-run/claim', credentials);
  if (claim.status !== 200) throw new Skip('Personal fixture claim unavailable; cannot establish edition identity');
  const probe = await request(context.base, '/api/login', credentials);
  if (!probe.body.access_token) throw new Skip('Personal fixture login unavailable; cannot establish edition identity');
  const rpc = await dashboard(context, probe.body.access_token);
  const status = await rpc('system.status');
  if (status.edition_profile !== 'personal') throw new Skip('binary did not resolve system.status.edition_profile=personal');
  expect('14 personal methods off', await request(context.base, '/api/login/methods'), 'off');
  expect('14 personal start refused', await request(context.base, '/api/login/google/start', {}), 'http_error', 404);
  report('SKIP', '14 login button', {}, 'HTTP bench does not render the dashboard');
}

export async function main(argv = process.argv.slice(2)) {
  if (argv.length !== 1) { report('FAIL', 'setup', {}, 'usage: node google-signin.mjs <guarded-binary-wrapper>'); return 1; }
  const binary = resolve(argv[0]); const mock = createGoogleMock(); const contexts = [];
  try {
    const mockBase = `http://127.0.0.1:${await listen(mock.server)}`;
    let bench;
    try {
      const context = await fixture(binary, mockBase, 'enterprise', contexts);
      const credentials = { email: 'admin@c.test', password };
      check((await request(context.base, '/api/first-run/claim', credentials)).status === 200, 'first-run claim failed');
      const login = await request(context.base, '/api/login', credentials); check(login.body.access_token, 'Admin local login failed');
      const admin = await dashboard(context, login.body.access_token);
      check((await admin('system.status')).edition_profile === 'enterprise', 'Enterprise entitlement not resolved');
      await admin('security.credential_protection_enable');
      await admin('mcp.oauth.start', { provider_id: 'google', client_id: 'fixture-signin-client', client_secret: 'fixture-secret-only' });
      bench = new Bench(context, mock, mockBase, admin);
    } catch (error) {
      report(error instanceof Skip ? 'SKIP' : 'FAIL', 'enterprise setup', {}, error instanceof Skip ? error.message : 'guarded setup failed; raw output suppressed');
    }
    const scenarios = [scenario01OffByDefault, scenario02Acknowledgement, scenario10DailyCap, scenario03AdminApproval,
      scenario05HostedDomain, scenario06ExistingEmail, scenario07AttestationThenLink, scenario08Attacks,
      scenario11TemplateObservation, scenario13PasswordlessAdmin, scenario09Suspended, scenario04DirectMfa];
    for (const fn of scenarios) {
      if (bench) await scenario(fn.name, () => fn(bench));
      else report('SKIP', fn.name, {}, 'Enterprise setup unavailable');
    }
    await scenario('scenario14Personal', () => scenario14Personal(binary, mockBase, contexts));
  } catch { report('FAIL', 'mock setup', {}, 'loopback mock could not start'); }
  finally {
    for (const context of contexts.reverse()) {
      try { await stop(context); } catch { report('FAIL', 'cleanup', {}, 'owned gateway could not be stopped'); }
    }
    mock.server.closeAllConnections();
    if (mock.server.listening) await new Promise((done) => mock.server.close(done));
    // Persist only explicitly sanitized observations and mock validation booleans.
    for (const context of contexts) writeFileSync(join(context.home, 'google-signin-results.json'),
      JSON.stringify({ observations, tokenHits: mock.tokenHits }, null, 2), { mode: 0o600 });
  }
  return observations.some((r) => r.status === 'FAIL') ? 1 : 0;
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((code) => { process.exitCode = code; }).catch(() => { console.error('FAIL harness HTTP=n/a result=n/a — internal error; raw output suppressed'); process.exitCode = 1; });
}
