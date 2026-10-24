#!/usr/bin/env node
// Isolated B2a acceptance probe. Requires Node 22+ and a debug binary built
// with --features live-test-mocks. No model or real Google account is used.
import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync, createWriteStream } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';

const binary = resolve(process.argv[2] ?? 'target/debug/duduclaw');
const home = mkdtempSync(join(tmpdir(), 'ddc-google-delegate-'));
console.log(`ARTIFACTS ${home}`);
const scriptDir = dirname(fileURLToPath(import.meta.url));
const delay = (ms) => new Promise((done) => setTimeout(done, ms));
const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
const seen = [];
const mock = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://127.0.0.1');
    res.setHeader('content-type', 'application/json');
    if (url.pathname === '/token') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const assertion = new URLSearchParams(body).get('assertion');
      const [head, payload, signature] = assertion.split('.');
      assert(verify('RSA-SHA256', Buffer.from(`${head}.${payload}`), publicKey, Buffer.from(signature, 'base64url')));
      const claims = JSON.parse(Buffer.from(payload, 'base64url'));
      seen.push(claims);
      if (claims.sub.startsWith('unauth')) {
        res.statusCode = 400;
        return res.end(JSON.stringify({ error: 'unauthorized_client' }));
      }
      return res.end(JSON.stringify({ access_token: `fixture:${claims.sub}`, expires_in: 3600, token_type: 'Bearer' }));
    }
    assert.equal(req.method, 'GET', 'the probe must never write to a Google service');
    const account = req.headers.authorization?.replace('Bearer fixture:', '') ?? '';
    if (url.pathname.endsWith('/profile')) return res.end(JSON.stringify({ emailAddress: account }));
    if (url.pathname.endsWith('/messages')) return res.end(JSON.stringify({ messages: [{ id: 'same-id', threadId: 'thread' }] }));
    if (url.pathname.endsWith('/messages/same-id')) return res.end(JSON.stringify({
      id: 'same-id', snippet: account, payload: {
        mimeType: 'text/plain', body: { data: Buffer.from(`message for ${account}`).toString('base64url') },
        headers: [{ name: 'From', value: 'sender@c.test' }, { name: 'To', value: account }, { name: 'Subject', value: account }],
      },
    }));
    res.end(JSON.stringify({ items: [] }));
  } catch (error) {
    res.statusCode = 500;
    res.end(JSON.stringify({ error: error.message }));
  }
});
mock.listen(0, '127.0.0.1');
await once(mock, 'listening');
const mockBase = `http://127.0.0.1:${mock.address().port}`;
const portLease = createServer();
portLease.listen(0, '127.0.0.1');
await once(portLease, 'listening');
const port = portLease.address().port;
await new Promise((done) => portLease.close(done));
const made = spawnSync('bash', [join(scriptDir, 'make-home.sh'), home, '--port', String(port)], { encoding: 'utf8' });
assert.equal(made.status, 0, made.stderr);
const key = JSON.stringify({
  type: 'service_account', client_email: 'fixture@project.iam.gserviceaccount.test', client_id: '1234567890',
  private_key: privateKey.export({ format: 'pem', type: 'pkcs8' }), token_uri: `${mockBase}/token`,
});
writeFileSync(join(home, 'home-key.json'), key, { mode: 0o600 });
const configPath = join(home, 'config.toml');
writeFileSync(configPath, readFileSync(configPath, 'utf8') + `
[integrations]
google_workspace = true
[integrations.google_identity]
allowed_subject_domains = ["c.test"]
[integrations.google_service_account]
key_file = "home-key.json"
subject = "company@c.test"
[dispatch]
enabled = false
[proactive]
enabled = false
`);
const env = Object.fromEntries(['PATH', 'TMPDIR', 'LANG'].filter((k) => process.env[k]).map((k) => [k, process.env[k]]));
Object.assign(env, { HOME: join(home, 'os-home'), DUDUCLAW_HOME: home, DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: mockBase });
const gateway = spawn(binary, ['run', '--yes'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
const log = createWriteStream(join(home, 'gateway.log'), { mode: 0o600 });
gateway.stdout.pipe(log); gateway.stderr.pipe(log);
let socket;
const apiBase = `http://127.0.0.1:${port}`;
async function post(path, body) {
  const response = await fetch(`${apiBase}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  assert(response.ok, `HTTP ${response.status} on ${path}`);
  return response.json();
}

async function mcp(agent, tools, overrides = {}) {
  const config = JSON.parse(readFileSync(join(home, 'agents', agent, '.mcp.json')));
  const entry = config.mcpServers.duduclaw ?? config.mcpServers['duduclaw-pro'];
  const child = spawn(binary, entry.args ?? ['mcp-server'], { env: { ...env, ...entry.env, ...overrides }, stdio: ['pipe', 'pipe', 'ignore'] });
  const waiting = new Map();
  const lines = createInterface({ input: child.stdout });
  lines.on('line', (line) => {
    const reply = JSON.parse(line);
    waiting.get(reply.id)?.(reply);
  });
  let id = 0;
  async function rpc(method, params) {
    const key = ++id;
    const reply = new Promise((done) => waiting.set(key, done));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: key, method, params }) + '\n');
    const result = await Promise.race([reply, delay(45000).then(() => { throw new Error(`MCP timeout: ${method}`); })]);
    waiting.delete(key);
    assert(!result.error, `MCP ${method} returned an error`);
    return result.result;
  }
  try {
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'google-delegate-live', version: '1' } });
    child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
    const listed = await rpc('tools/list', {});
    const results = [];
    for (const tool of tools) {
      if (typeof tool === 'function') await tool();
      else results.push(await rpc('tools/call', { name: tool[0], arguments: tool[1] }));
    }
    return { listed: listed.tools.map((t) => t.name), results };
  } finally {
    lines.close(); child.stdin.end(); child.kill();
  }
}

try {
  let healthy = false;
  for (let i = 0; i < 120; i++) {
    try { if ((await fetch(`${apiBase}/health`)).ok) { healthy = true; break; } } catch {}
    if (gateway.exitCode !== null) throw new Error(`gateway exited: ${gateway.exitCode}`);
    await delay(500);
  }
  assert(healthy, 'gateway did not start');
  const buildMode = await (await fetch(`${apiBase}/healthz`)).json();
  assert.equal(buildMode.live_test_mocks, true, 'refusing a binary without the in-memory credential-store seam');
  const login = { email: 'owner@c.test', password: 'fixture-password-only' };
  await post('/api/first-run/claim', login);
  const tokens = await post('/api/login', login);
  socket = new WebSocket(`ws://127.0.0.1:${port}/ws`);
  await once(socket, 'open');
  const waiting = new Map();
  socket.addEventListener('message', (event) => {
    const reply = JSON.parse(event.data);
    if (reply.type === 'res') waiting.get(reply.id)?.(reply);
  });
  let id = 0;
  async function rpc(method, params = {}) {
    const key = String(++id);
    const reply = new Promise((done) => waiting.set(key, done));
    socket.send(JSON.stringify({ type: 'req', id: key, method, params }));
    const result = await Promise.race([reply, delay(45000).then(() => { throw new Error(`RPC timeout: ${method}`); })]);
    waiting.delete(key);
    assert(result.ok, `RPC ${method}: ${JSON.stringify(result.error)}`);
    return result.payload;
  }
  await rpc('connect', { jwt: tokens.access_token });
  const unsigned = await fetch(`${apiBase}/api/internal/google-token`, { method: 'POST', body: '{}' });
  assert.equal(unsigned.status, 401);
  console.log('PASS unsigned token requests refused');
  let probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(!probe.results[0].isError);
  assert(JSON.stringify(probe.results).includes('company@c.test'));
  console.log('PASS unconfigured employee keeps shared-account path');
  await rpc('security.credential_protection_enable');
  await rpc('google.delegation.import_key', { key_json: key });
  const delegate = async (agent, email, extra = {}) => rpc('agents.google_identity_set', {
    agent_id: agent, identity: `delegate:${email}`, requesters: 'anyone', confirm_anyone: true, ...extra,
  });
  await delegate('prod-shaped', 'alice@c.test');
  await delegate('plain', 'bob@c.test');
  for (const [agent, email] of [['prod-shaped', 'alice@c.test'], ['plain', 'bob@c.test']]) {
    probe = await mcp(agent, [['gmail_search', { query: 'in:inbox' }]]);
    assert(!probe.results[0].isError);
    assert(JSON.stringify(probe.results).includes(email));
    assert(seen.some((claims) => claims.sub === email));
  }
  console.log('PASS two employee registrations receive their own signed JWT subjects');
  probe = await mcp('prod-shaped', [
    ['gmail_search', { query: 'in:inbox' }],
    () => delegate('prod-shaped', 'bob@c.test'),
    ['gmail_search', { query: 'in:inbox' }],
  ]);
  assert(JSON.stringify(probe.results[0]).includes('alice@c.test'));
  assert(JSON.stringify(probe.results[1]).includes('bob@c.test'));
  console.log('PASS one MCP process reauthorizes after the delegated account changes');
  await delegate('prod-shaped', 'alice@c.test');
  const previousMints = seen.length;
  await assert.rejects(() => delegate('prod-shaped', 'someone@other.test'));
  assert.equal(seen.length, previousMints);
  console.log('PASS out-of-domain assignment refused before minting');
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]], { DUDUCLAW_AGENT_TOKEN: 'invalid' });
  assert(probe.results[0].isError);
  console.log('PASS unproven delegate refused without shared fallback');
  await delegate('prod-shaped', 'unauth@c.test');
  const tested = await rpc('google.identity.test', { agent_id: 'prod-shaped' });
  assert.equal(tested.ok, false);
  assert(tested.steps.some((s) => s.id === 'authorize' && !s.ok));
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(probe.results[0].isError);
  console.log('PASS Google authorization rejection surfaced by RPC and native tool');
  await delegate('prod-shaped', 'unauth@c.test', { fallback: 'home' });
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(!probe.results[0].isError);
  assert(JSON.stringify(probe.results).includes('company@c.test'));
  assert(JSON.stringify(probe.results).includes('已改用公司共用帳號'));
  console.log('PASS explicit fallback uses company account with notice');
  await rpc('agents.google_identity_set', { agent_id: 'prod-shaped', identity: 'none' });
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(!probe.listed.includes('gmail_search'));
  assert(probe.results[0].isError);
  console.log('PASS none hides Google tools and refuses direct calls');
  await delegate('prod-shaped', 'alice@c.test');
  await rpc('google.integration.set', { enabled: false });
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(probe.results[0].isError);
  console.log('PASS master switch refuses native calls');
  const audit = readFileSync(join(home, 'tool_calls.jsonl'), 'utf8');
  assert(audit.includes('google_token') && audit.includes('google_call'));
  assert(!audit.includes('fixture:alice') && !audit.includes('PRIVATE KEY'));
  console.log('PASS audit records identity without bearer or private key');
  await rpc('google.integration.set', { enabled: true });
  gateway.kill('SIGTERM');
  await Promise.race([once(gateway, 'exit'), delay(10000)]);
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(probe.results[0].isError);
  console.log('PASS delegate refuses when gateway is offline');
  console.log(`ARTIFACTS ${home}`);
} finally {
  socket?.close(); gateway.kill('SIGTERM'); mock.close(); log.end();
}
