#!/usr/bin/env node
// Isolated B2b binary probe. Run only through an OS sandbox wrapper that denies
// real credentials/keychains and every outbound address except loopback.
// Requires Node 22+ and a debug live-test-mocks binary; never uses Google.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync, createWriteStream } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';

assert(process.argv[2], 'pass the guarded binary wrapper, never an unguarded binary');
const binary = resolve(process.argv[2]);
const home = mkdtempSync(join(tmpdir(), 'ddc-google-personal-'));
const scriptDir = dirname(fileURLToPath(import.meta.url));
const delay = (ms) => new Promise((done) => setTimeout(done, ms));
const scopes = 'openid email https://www.googleapis.com/auth/gmail.readonly';
const codes = new Map();
const refreshes = new Map();
const events = [];
let invalidRefresh = false;
let serviceWrites = 0;
let heldRevoke;
const mock = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://127.0.0.1');
    res.setHeader('content-type', 'application/json');
    if (url.pathname === '/oauth2.googleapis.com/token') {
      let body = ''; for await (const chunk of req) body += chunk;
      const form = new URLSearchParams(body);
      let account;
      if (form.get('grant_type') === 'authorization_code') {
        const code = codes.get(form.get('code'));
        assert(code, 'unknown or replayed code');
        assert.equal(form.get('redirect_uri'), code.redirect);
        assert.equal(createHash('sha256').update(form.get('code_verifier')).digest('base64url'), code.challenge);
        codes.delete(form.get('code'));
        account = code.account;
        events.push(['exchange', account]);
      } else {
        assert.equal(form.get('grant_type'), 'refresh_token');
        events.push(['refresh']);
        if (invalidRefresh) { res.statusCode = 400; return res.end('{"error":"invalid_grant"}'); }
        account = refreshes.get(form.get('refresh_token'));
        assert(account, 'unknown refresh grant');
      }
      const refresh = `mock-refresh:${account}`;
      refreshes.set(refresh, account);
      return res.end(JSON.stringify({ access_token: `mock-access:${account}`, refresh_token: refresh,
        token_type: 'Bearer', expires_in: 1, scope: scopes }));
    }
    if (url.pathname === '/openidconnect.googleapis.com/v1/userinfo') {
      const account = req.headers.authorization?.replace('Bearer mock-access:', '');
      assert(account?.endsWith('@c.test'));
      return res.end(JSON.stringify({ sub: `sub:${account}`, email: account, email_verified: true, hd: 'c.test' }));
    }
    if (url.pathname === '/oauth2.googleapis.com/revoke') {
      events.push(['revoke']);
      if (heldRevoke) await heldRevoke.wait;
      return res.end('');
    }
    if (req.method !== 'GET') serviceWrites++;
    assert.equal(req.method, 'GET', 'native Google service writes are forbidden in this probe');
    const account = req.headers.authorization?.replace('Bearer mock-access:', '') ?? '';
    if (url.pathname.endsWith('/messages')) return res.end('{"messages":[{"id":"receipt"}]}');
    if (url.pathname.endsWith('/messages/receipt')) return res.end(JSON.stringify({
      id: 'receipt', snippet: account, payload: { mimeType: 'text/plain',
        body: { data: Buffer.from(`for ${account}`).toString('base64url') },
        headers: [{ name: 'From', value: 'sender@c.test' }, { name: 'Subject', value: account }] },
    }));
    res.statusCode = 404; res.end('{}');
  } catch (error) { res.statusCode = 500; res.end(JSON.stringify({ error: error.message })); }
});
mock.listen(0, '127.0.0.1'); await once(mock, 'listening');
const mockBase = `http://127.0.0.1:${mock.address().port}`;
const portLease = createServer(); portLease.listen(0, '127.0.0.1'); await once(portLease, 'listening');
const port = portLease.address().port; await new Promise((done) => portLease.close(done));
const made = spawnSync('bash', [join(scriptDir, 'make-home.sh'), home, '--port', String(port)], { encoding: 'utf8' });
assert.equal(made.status, 0, made.stderr);
writeFileSync(join(home, 'config.toml'), readFileSync(join(home, 'config.toml'), 'utf8') + `
[integrations]
google_workspace = false
[integrations.google_identity]
allowed_domains = ["c.test"]
[dispatch]
enabled = false
[proactive]
enabled = false
`);
const env = Object.fromEntries(['PATH', 'TMPDIR', 'LANG'].filter((k) => process.env[k]).map((k) => [k, process.env[k]]));
Object.assign(env, { HOME: join(home, 'os-home'), DUDUCLAW_HOME: home, DUDUCLAW_EDITION: 'enterprise', DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: mockBase });
console.log(`ARTIFACTS ${home}`);
const gateway = spawn(binary, ['run', '--yes'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
const log = createWriteStream(join(home, 'gateway.log'), { mode: 0o600 });
gateway.stdout.pipe(log); gateway.stderr.pipe(log);
const apiBase = `http://127.0.0.1:${port}`;
const sockets = [];
async function post(path, body) {
  const r = await fetch(`${apiBase}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  assert(r.ok, `${path}: HTTP ${r.status}`); return r.json();
}
async function dashboard(token) {
  const socket = new WebSocket(`ws://127.0.0.1:${port}/ws`); sockets.push(socket); await once(socket, 'open');
  const waiting = new Map(); let id = 0;
  socket.addEventListener('message', (event) => { const r = JSON.parse(event.data); if (r.type === 'res') waiting.get(r.id)?.(r); });
  const rpc = async (method, params = {}) => {
    const key = String(++id); const reply = new Promise((done) => waiting.set(key, done));
    socket.send(JSON.stringify({ type: 'req', id: key, method, params }));
    let timer;
    try {
      const r = await Promise.race([reply, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`timeout: ${method}`)), 45000); })]);
      assert(r.ok, `${method}: ${JSON.stringify(r.error)}`); return r.payload;
    } finally { clearTimeout(timer); waiting.delete(key); }
  };
  await rpc('connect', { jwt: token }); return rpc;
}
async function mcp(agent, name, args) {
  const config = JSON.parse(readFileSync(join(home, 'agents', agent, '.mcp.json')));
  const entry = config.mcpServers.duduclaw ?? config.mcpServers['duduclaw-pro'];
  const child = spawn(binary, entry.args ?? ['mcp-server'], { env: { ...env, ...entry.env }, stdio: ['pipe', 'pipe', 'ignore'] });
  const lines = createInterface({ input: child.stdout }); const waiting = new Map(); let id = 0;
  lines.on('line', (line) => { const r = JSON.parse(line); waiting.get(r.id)?.(r); });
  async function rpc(method, params) {
    const key = ++id; const reply = new Promise((done) => waiting.set(key, done));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: key, method, params }) + '\n');
    let timer;
    try {
      const r = await Promise.race([reply, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('MCP timeout')), 45000); })]);
      assert(!r.error); return r.result;
    } finally { clearTimeout(timer); waiting.delete(key); }
  }
  try {
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'personal-isolated', version: '1' } });
    child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
    return await rpc('tools/call', { name, arguments: args });
  } finally {
    lines.close(); child.stdin.end();
    if (child.exitCode === null) { child.kill(); await once(child, 'exit'); }
  }
}
async function callback(start, account) {
  const authorize = new URL(start.authorization_url);
  assert.equal(authorize.searchParams.get('access_type'), 'offline');
  assert(authorize.searchParams.get('prompt').includes('consent'));
  assert.equal(authorize.searchParams.get('code_challenge_method'), 'S256');
  const code = `fixture-code-${codes.size}-${Date.now()}`;
  codes.set(code, { account, redirect: start.redirect_uri, challenge: authorize.searchParams.get('code_challenge') });
  const r = await fetch(`${start.redirect_uri}?state=${start.state}&code=${code}`, { redirect: 'manual' });
  assert.equal(r.status, 303); assert.equal(r.headers.get('cache-control'), 'no-store');
  assert(!r.headers.get('location').includes(code)); assert(!(await r.text()).includes(code));
}
try {
  for (let i = 0; ; i++) {
    try { if ((await fetch(`${apiBase}/health`)).ok) break; } catch {}
    assert(i < 120 && gateway.exitCode === null, 'gateway failed to start'); await delay(500);
  }
  assert.equal((await (await fetch(`${apiBase}/healthz`)).json()).live_test_mocks, true);
  const login = { email: 'admin@c.test', password: 'fixture-password-only' };
  await post('/api/first-run/claim', login); const admin = await dashboard((await post('/api/login', login)).access_token);
  await admin('security.credential_protection_enable');
  await admin('mcp.oauth.start', { provider_id: 'google', client_id: 'fixture-client', client_secret: 'fixture-secret' });
  const users = [];
  for (const name of ['alice', 'bob']) {
    const user = (await admin('users.create', { email: `${name}@c.test`, display_name: name, password: login.password, role: 'employee' })).user;
    await admin('users.bind_agent', { user_id: user.id, agent_name: 'prod-shaped', access_level: 'owner' }); users.push(user);
  }
  const aliceLogin = { email: users[0].email, password: login.password };
  const tokens = await post('/api/login', aliceLogin);
  const alice = await dashboard(tokens.access_token);
  const otherLogin = await dashboard((await post('/api/login', aliceLogin)).access_token);
  const bob = await dashboard((await post('/api/login', { email: users[1].email, password: login.password })).access_token);
  let start = await alice('me.google.connect_start', { redirect_origin: apiBase });
  await callback(start, 'different-google@c.test');
  await assert.rejects(() => otherLogin('me.google.connect_finish', { state: start.state }));
  await assert.rejects(() => bob('me.google.connect_finish', { state: start.state }));
  const renewed = await post('/api/refresh', { refresh_token: tokens.refresh_token });
  const refreshedSession = await dashboard(renewed.access_token);
  const saved = await refreshedSession('me.google.connect_finish', { state: start.state });
  assert.equal(saved.connection.account_email, 'different-google@c.test');
  assert.deepEqual(saved.connection.granted_scopes, scopes.split(' '));
  const connectedVault = readFileSync(join(home, 'person_credentials/store.json'), 'utf8');
  assert(connectedVault.includes('pk:v1:'));
  assert(!connectedVault.includes('mock-access:') && !connectedVault.includes('mock-refresh:'));
  await assert.rejects(() => alice('me.google.connect_finish', { state: start.state }));
  console.log('PASS exact redirect/PKCE, actual scopes, cross-user/session refusal, refresh keeps session, single use');
  let status = await alice('me.google.status'); assert.equal(status.integration_enabled, false);
  await admin('agents.google_identity_set', { agent_id: 'prod-shaped', identity: `user:${users[0].id}`, requesters: 'anyone', confirm_anyone: true });
  await admin('google.integration.set', { enabled: true });
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  await assert.rejects(() => admin('me.google.consents.set', { agent_id: 'prod-shaped', user_id: users[0].id, allowed: true, confirm_shared_access: true }));
  await alice('me.google.consents.set', { agent_id: 'prod-shaped', allowed: true, confirm_shared_access: true });
  await delay(1200);
  const search = await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' });
  assert(!search.isError, JSON.stringify(search)); assert(JSON.stringify(search).includes('different-google@c.test'));
  assert(events.some(([kind]) => kind === 'refresh'));
  assert((await mcp('prod-shaped', 'gmail_create_draft', { to: 'sink@c.test', subject: 'refused', body: 'refused' })).isError);
  console.log('PASS connection saved with global switch off; explicit Anyone still requires self consent; refresh/read scopes refuse draft writes');
  await admin('agents.google_identity_set', { agent_id: 'prod-shaped', identity: 'owner', requesters: 'anyone', confirm_anyone: true });
  await admin('agents.primary_owner_set', { agent_id: 'prod-shaped', user_id: users[1].id });
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  await admin('agents.primary_owner_set', { agent_id: 'prod-shaped', user_id: users[0].id });
  assert(!(await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  console.log('PASS primary Owner change reselects the person without borrowing the previous grant');
  await alice('me.google.consents.set', { agent_id: 'prod-shaped', allowed: false });
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  await alice('me.google.consents.set', { agent_id: 'prod-shaped', allowed: true, confirm_shared_access: true });
  // There is no reversible status RPC in this base. Change only this fresh
  // fixture DB, then exercise the actual resolver's live status revalidation.
  const fixtureStatus = (status) => {
    assert(['active', 'suspended'].includes(status));
    assert(/^[0-9a-f-]+$/.test(users[0].id));
    const change = spawnSync('/usr/bin/sqlite3', [join(home, 'users.db'),
      `UPDATE users SET status='${status}' WHERE id='${users[0].id}';`], { encoding: 'utf8' });
    assert.equal(change.status, 0, change.stderr);
  };
  fixtureStatus('suspended');
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  fixtureStatus('active');
  assert(!(await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  console.log('PASS personal refusal and live fixture suspension fail closed; reactivation retains grant');
  invalidRefresh = true; await delay(1200);
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  status = await alice('me.google.status'); assert.equal(status.connection.status, 'needs_reconnect');
  invalidRefresh = false;
  let release; heldRevoke = { wait: new Promise((done) => { release = done; }) };
  const before = events.filter(([kind]) => kind === 'revoke').length;
  const disconnected = alice('me.google.disconnect', { confirm: true });
  for (let i = 0; events.filter(([kind]) => kind === 'revoke').length === before; i++) { assert(i < 100); await delay(25); }
  assert.equal((await otherLogin('me.google.status')).connection, null, 'local grant must disappear before slow revoke');
  let started = false;
  const next = bob('me.google.connect_start', { redirect_origin: apiBase }).then((value) => { started = true; return value; });
  await delay(100); assert.equal(started, false, 'new grant must wait through revoke completion');
  release(); assert.equal((await disconnected).provider_revoked, true); heldRevoke = undefined;
  const bobStart = await next;
  assert((await mcp('prod-shaped', 'gmail_search', { query: 'in:inbox' })).isError);
  console.log('PASS invalid_grant, immediate local disconnect and delayed revoke/new-grant coordination');
  await callback(bobStart, 'bob-google@c.test');
  await bob('me.google.connect_finish', { state: bobStart.state });
  const removed = await admin('users.remove', { user_id: users[1].id });
  assert.equal(removed.status, 'suspended');
  await assert.rejects(() => bob('me.google.status'));
  const vault = readFileSync(join(home, 'person_credentials/store.json'), 'utf8');
  const audit = readFileSync(join(home, 'tool_calls.jsonl'), 'utf8');
  assert(!vault.includes('mock-access:') && !vault.includes('mock-refresh:'));
  assert(!audit.includes('mock-access:') && !audit.includes('mock-refresh:'));
  assert.equal(serviceWrites, 0, 'denied native service writes must never reach transport');
  assert(!vault.includes('bob-google@c.test'), 'offboard must remove the personal record');
  console.log('PASS real users.remove offboarding removes grant and rejects the former user');
  console.log('PASS encrypted vault and audit contain no raw access/refresh token');
  writeFileSync(join(home, 'result.json'), JSON.stringify({ status: 'PASS_ISOLATED_MOCK', events, profile: 'debug_assertions=true; debug symbols=0' }, null, 2), { mode: 0o600 });
} finally {
  heldRevoke = undefined;
  for (const socket of sockets) socket.close();
  gateway.kill('SIGTERM');
  if (gateway.exitCode === null) await Promise.race([once(gateway, 'exit'), delay(10000)]);
  if (gateway.exitCode === null) { gateway.kill('SIGKILL'); await once(gateway, 'exit'); }
  mock.closeAllConnections(); await new Promise((done) => mock.close(done)); log.end();
}
