#!/usr/bin/env node
// Node 22+, no npm dependencies. Run only from an operator shell.
// All gateway/model/MCP children are confined to loopback on macOS.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes, generateKeyPairSync, sign, verify } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync, appendFileSync,
  existsSync, realpathSync, symlinkSync, unlinkSync, chmodSync, renameSync } from 'node:fs';
import net from 'node:net';
import { tmpdir, homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const file = fileURLToPath(import.meta.url);
const scriptDir = dirname(file);
const repo = resolve(scriptDir, '../..');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const children = new Set(), sockets = new Set(), secrets = new Set();
let context, failures = 0;
const checks = [];
function check(value, message) { assert(value, message); }
function sanitise(value) {
  let text = String(value);
  for (const secret of secrets) if (secret) text = text.split(secret).join('[REDACTED]');
  return text.replace(/ddc_(?:run|member)_[A-Za-z0-9_-]+/g, '[REDACTED]')
    .replace(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, '[REDACTED]')
    .replace(/((?:token|secret|password|credential|api[_-]?key|hash)["']?\s*[:=]\s*)[^\s,}]+/gi, '$1[REDACTED]')
    .replace(/[A-Za-z0-9_+/=-]{40,}/g, '[REDACTED]');
}
function artifact(name, value) {
  writeFileSync(join(context.root, name), JSON.stringify(redactArtifact(value), null, 2) + '\n', { mode: 0o600 });
}
function redactArtifact(value) {
  if (Array.isArray(value)) return value.map(redactArtifact);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) =>
    [key, /^(?:password_hash|password|credential|access_token|refresh_token|secret|api_key)$/i.test(key) ? '[REDACTED]' : redactArtifact(item)]));
  return typeof value === 'string' ? sanitise(value) : value;
}
function report(status, id, what, evidence = '') {
  const row = { status, id, what, ...(evidence ? { evidence: sanitise(evidence) } : {}) };
  checks.push(row);
  if (status === 'FAIL') failures++;
  console.log(`${status} ${id} ${what}${evidence ? `: ${sanitise(evidence)}` : ''}`);
}
async function scenario(id, what, action) {
  try { await action(); report('PASS', id, what); }
  catch (error) { report('FAIL', id, what, error.message); }
}
function minimalEnv(home, extra = {}) {
  return { TMPDIR: join(context.root, 'tmp'), LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', ...extra,
    // Registration files cannot redirect either home.
    HOME: join(home, 'os-home'), DUDUCLAW_HOME: home,
    PATH: `${join(context.root, 'bin')}:/usr/bin:/bin:/usr/sbin:/sbin`,
    // No listener can own TCP port zero. Reqwest's delayed update request
    // therefore fails locally without asking DNS for the upstream hostname.
    HTTP_PROXY: 'http://127.0.0.1:0', HTTPS_PROXY: 'http://127.0.0.1:0',
    ALL_PROXY: 'http://127.0.0.1:0', NO_PROXY: '127.0.0.1,localhost',
    DUDUCLAW_AUTO_UPDATE: '0', ...context.fixtureEnv };
}
function logChild(child, label, stdout = false) {
  const path = join(context.root, `${label}.log`);
  writeFileSync(path, '', { mode: 0o600 });
  for (const stream of [child.stderr, ...(stdout ? [child.stdout] : [])].filter(Boolean)) {
    // Redact whole lines, so a credential split across chunks cannot leak.
    const lines = createInterface({ input: stream });
    lines.on('line', line => appendFileSync(path, sanitise(line) + '\n'));
  }
}
function start(command, args, env, label, { confined = true, stdin = 'ignore' } = {}) {
  const argv = confined ? ['-f', context.policy, command, ...args] : args;
  const child = spawn(confined ? '/usr/bin/sandbox-exec' : command, argv, {
    env, cwd: env.HOME, detached: confined, stdio: [stdin, 'pipe', 'pipe'],
  });
  child.ownedGroup = confined;
  children.add(child);
  child.spawnError = null;
  child.on('error', error => { child.spawnError = error; });
  logChild(child, label, stdin === 'ignore');
  return child;
}
function alive(child) { check(!child.spawnError && child.exitCode === null && child.signalCode === null, 'owned child exited; inspect sanitised logs'); }
function groupAlive(child) {
  if (!child.pid) return false;
  if (!child.ownedGroup && (child.exitCode !== null || child.signalCode !== null)) return false;
  try { process.kill(child.ownedGroup ? -child.pid : child.pid, 0); return true; }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}
async function stop(child) {
  if (!child) return;
  if (groupAlive(child)) process.kill(child.ownedGroup ? -child.pid : child.pid, 'SIGTERM');
  let until = Date.now() + 3000;
  while (groupAlive(child) && Date.now() < until) await delay(50);
  if (groupAlive(child)) process.kill(child.ownedGroup ? -child.pid : child.pid, 'SIGKILL');
  until = Date.now() + 3000;
  while (groupAlive(child) && Date.now() < until) await delay(50);
  check(!groupAlive(child), 'owned process group failed to terminate');
  children.delete(child);
}
async function cleanup() {
  for (const ws of sockets) ws.close();
  const results = await Promise.allSettled([...children].map(stop));
  for (const result of results) if (result.status === 'rejected') report('FAIL', 'cleanup', 'owned child termination', result.reason.message);
  for (const path of context?.leasePaths || []) if (existsSync(path)) unlinkSync(path);
}
function system(command, args, input) {
  const result = spawnSync(command, args, { env: minimalEnv(context.home), cwd: context.home,
    input, encoding: 'utf8', timeout: 10000, maxBuffer: 4 * 1024 * 1024 });
  check(!result.error && result.status === 0, `${command} failed: ${sanitise(result.stderr || result.error?.message || result.status)}`);
  return result.stdout;
}
function sql(query, readonly = true) {
  return JSON.parse(system('/usr/bin/sqlite3', [...(readonly ? ['-readonly'] : []), '-json', join(context.home, 'users.db'), query]) || '[]');
}
async function until(action, label, ms = 15000) {
  const end = Date.now() + ms;
  do { const value = await action(); if (value) return value; await delay(100); } while (Date.now() < end);
  throw new Error(`${label} timed out`);
}
async function http(path, body, token) {
  const response = await fetch(`${context.base}${path}`, {
    method: body === undefined ? 'GET' : 'POST', redirect: 'error', signal: AbortSignal.timeout(2000),
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(path === '/api/session/local' ? { 'x-duduclaw-local': '1' } : {}) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  check(response.ok, `${path} HTTP ${response.status}`);
  return response.json();
}
async function freePort() {
  await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen({ host: '127.0.0.1', port: context.port, exclusive: true }, () => server.close(resolve));
  });
}
async function boot(label) {
  await freePort();
  const child = start(context.binary, ['run', '--yes'], minimalEnv(context.home), `gateway-${label}`);
  context.gateway = child;
  await until(async () => {
    alive(child);
    const owners = spawnSync('/usr/sbin/lsof', ['-nP', `-iTCP@127.0.0.1:${context.port}`, '-sTCP:LISTEN', '-t'],
      { env: minimalEnv(context.home), encoding: 'utf8', timeout: 2000 });
    check(!owners.error && [0, 1].includes(owners.status), 'lsof listener ownership check failed');
    if (owners.status === 1) return null;
    check(owners.stdout.trim() === String(child.pid), 'listener is not the owned gateway; refusing contact');
    try { return (await http('/healthz')).ok; } catch { return false; }
  }, 'isolated gateway health', 60000);
  if (!context.claimed) {
    await http('/api/first-run/claim', { email: context.ownerEmail, password: context.ownerPassword });
    context.claimed = true;
  }
  const login = await http('/api/login', { email: context.ownerEmail, password: context.ownerPassword });
  check(login.access_token, `owner login did not issue a session: ${JSON.stringify(login)}`);
  const jwt = login.access_token; secrets.add(login.refresh_token);
  secrets.add(jwt);
  const admin = await rpcClient({ jwt });
  const roster = await admin.call('agents.list');
  check(roster.agents?.some(agent => agent.name === context.canary), 'gateway instance canary missing');
  const status = await admin.call('system.status');
  check(status.edition_profile === 'enterprise', 'signed disposable OEM license did not enable Enterprise; DUDUCLAW_EDITION alone cannot elevate a community license');
  artifact(`edition-${label}.json`, { edition_profile: status.edition_profile, live_test_mocks: (await http('/healthz')).live_test_mocks === true });
  context.admin = admin;
  context.jwt = jwt;
  return admin;
}
async function shutdown() {
  context.admin?.close();
  await stop(context.gateway);
  context.gateway = null;
}

// Queue messages from construction time: no races between auth and session_info.
class Socket {
  constructor(path) {
    this.ws = new WebSocket(`ws://127.0.0.1:${context.port}${path}`);
    sockets.add(this.ws);
    this.queue = []; this.waiters = []; this.closed = false;
    this.ws.addEventListener('message', event => {
      try { this.queue.push(JSON.parse(event.data)); this.pump(); }
      catch { this.badFrame = true; this.pump(); }
    });
    this.ws.addEventListener('close', event => {
      this.closed = true; this.closeInfo = { code: event.code, wasClean: event.wasClean };
      this.pump(); sockets.delete(this.ws);
    });
    this.ws.addEventListener('error', () => { this.transportError = true; this.pump(); });
  }
  async open() {
    if (this.ws.readyState === WebSocket.OPEN) return;
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.ws.close(); reject(new Error('WebSocket open timed out')); }, 5000);
      this.ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      this.ws.addEventListener('error', () => { clearTimeout(timer); reject(new Error('WebSocket open failed')); }, { once: true });
    });
  }
  pump() { for (const wake of this.waiters.splice(0)) wake(); }
  send(value) { this.ws.send(JSON.stringify(value)); }
  async take(predicate, ms = 15000, allowClose = false) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      check(!this.badFrame, 'WebSocket JSON failure');
      check(allowClose || !this.transportError, 'WebSocket transport failure');
      const index = this.queue.findIndex(predicate);
      if (index >= 0) return this.queue.splice(index, 1)[0];
      if (this.closed) { check(allowClose, 'WebSocket closed before expected response'); return null; }
      await new Promise(resolve => {
        const wake = () => { clearTimeout(timer); resolve(); };
        const timer = setTimeout(() => { this.waiters = this.waiters.filter(w => w !== wake); resolve(); }, Math.min(100, end - Date.now()));
        this.waiters.push(wake);
      });
    }
    throw new Error('WebSocket expected frame timed out');
  }
  async expectRefusal(allowAuthError = false, ms = 5000) {
    // The product's member-key socket tests accept EOF, Close or transport
    // failure after retirement. An error event alone is not proof of closure,
    // and malformed JSON or any authenticated data must still fail this probe.
    const end = Date.now() + ms;
    for (;;) {
      const frame = await this.take(() => true, Math.max(0, end - Date.now()), true);
      if (frame === null) return { ...this.closeInfo, transport_error: Boolean(this.transportError) };
      check(allowAuthError && frame?.type === 'error' && /^authentication failed:/.test(frame.message),
        'refused member key received unexpected chat data');
    }
  }
  close() { this.ws.close(); }
}
async function rpcClient(credential) {
  const socket = new Socket('/ws'); await socket.open();
  let serial = 0;
  const raw = async (method, params = {}) => {
    const id = `acceptance-${++serial}`;
    socket.send({ type: 'req', id, method, params });
    return socket.take(frame => frame.type === 'res' && frame.id === id);
  };
  const call = async (method, params = {}) => {
    const response = await raw(method, params);
    check(response.ok, `${method} refused: ${sanitise(JSON.stringify(response.error))}`);
    return response.payload;
  };
  try { const hello = await call('connect', credential); check(hello.user && !hello.admin_fallback, 'account identity required'); }
  catch (error) { socket.close(); throw error; }
  return { raw, call, close: () => socket.close(), socket };
}

async function mcpClient({ home = context.home, binary = context.binary, extra = {}, registration,
  confined = true, label = 'mcp' } = {}) {
  const config = registration || JSON.parse(readFileSync(join(home, 'agents/prod-shaped/.mcp.json'), 'utf8'));
  const entry = config.mcpServers?.duduclaw || config.mcpServers?.['duduclaw-pro'];
  check(entry, 'gateway did not write the employee MCP registration');
  check(entry.env?.DUDUCLAW_AGENT_ID === 'prod-shaped', 'unexpected employee MCP registration');
  const child = start(binary, entry.args || ['mcp-server'], minimalEnv(home, { ...entry.env, ...extra }), label, { confined, stdin: 'pipe' });
  const pending = new Map(); let serial = 0;
  const fail = () => { for (const wait of pending.values()) { clearTimeout(wait.timer); wait.reject(new Error('stdio MCP exited')); } pending.clear(); };
  child.on('close', fail); child.on('error', fail);
  createInterface({ input: child.stdout }).on('line', line => {
    let response; try { response = JSON.parse(line); } catch { return; }
    const waiter = pending.get(response.id); if (!waiter) return;
    pending.delete(response.id); clearTimeout(waiter.timer);
    if (response.error) {
      const error = new Error(sanitise(JSON.stringify(response.error))); error.rpcError = response.error; waiter.reject(error);
    } else waiter.resolve(response.result);
  });
  const rpc = (method, params) => new Promise((resolve, reject) => {
    alive(child); const id = ++serial;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`MCP ${method} timed out`)); }, 15000);
    pending.set(id, { resolve, reject, timer });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, ...(params ? { params } : {}) }) + '\n');
  });
  child.stdin.on('error', fail);
  const client = { rpc, call: (name, args = {}) => rpc('tools/call', { name, arguments: args }), close: () => stop(child) };
  try {
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'v185-acceptance', version: '1' } });
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
    client.tools = (await rpc('tools/list')).tools || [];
    check(client.tools.length > 0, 'no tools visible to prod-shaped employee');
    return client;
  } catch (error) { await stop(child); throw error; }
}
function toolText(result) { return (result.content || []).map(c => c.text || '').join('\n'); }
function toolOK(result, label) { check(!result.isError, `${label}: ${toolText(result)}`); return toolText(result); }
function assertWikiRefusal(result, rpcError, invalidId, pattern) {
  if (invalidId) {
    check(rpcError?.code === -32602 && rpcError.message === 'Invalid wiki page id', 'unsafe page id did not receive the dispatcher refusal');
    return rpcError.message;
  }
  check(!rpcError && result?.isError && pattern.test(toolText(result)), `wiki tool did not give the expected path refusal: ${JSON.stringify(rpcError || result)}`);
  return toolText(result);
}

function seedLegacy(members = true) {
  // Pre-avatar users/bindings schema from auth/db.rs; no avatar column or index.
  system('/usr/bin/sqlite3', [join(context.home, 'users.db')], `
    CREATE TABLE users(id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL, display_name TEXT NOT NULL,
      password_hash TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'employee', status TEXT NOT NULL DEFAULT 'active',
      created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_login TEXT,
      must_change_password INTEGER NOT NULL DEFAULT 0, bootstrap_owner INTEGER NOT NULL DEFAULT 0,
      department TEXT, manager_user_id TEXT, primary_agent_id TEXT);
    CREATE TABLE user_agent_bindings(user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
      agent_name TEXT NOT NULL, access_level TEXT NOT NULL DEFAULT 'owner', bound_at TEXT NOT NULL,
      PRIMARY KEY(user_id,agent_name));
    INSERT INTO users(id,email,display_name,password_hash,role,bootstrap_owner,must_change_password,created_at,updated_at)
      VALUES('fixture-admin','admin@local','Fixture Owner','!','admin',1,1,'2026-01-01','2026-01-01');
  `);
  const rows = [ ['single', 'prod-shaped', 'operator', 'active'], ['multiple', 'plain', 'owner', 'active'],
    ['none', null, null, 'active'], ['shared-a', context.canary, 'operator', 'active'],
    ['shared-b', context.canary, 'operator', 'active'], ['viewer', 'read-only', 'viewer', 'active'],
    ['unbound', 'missing', null, 'active'], ['inactive', 'prod-shaped', 'operator', 'suspended'] ];
  const quote = v => v === null ? 'NULL' : `'${v.replaceAll("'", "''")}'`;
  for (const [id, primary, level, status] of members ? rows : []) {
    system('/usr/bin/sqlite3', [join(context.home, 'users.db')], `
      INSERT INTO users(id,email,display_name,password_hash,status,created_at,updated_at,primary_agent_id,department)
      VALUES(${quote(id)},${quote(`${id}@acceptance.invalid`)},${quote(id)},'!',${quote(status)},'2026-01-01','2026-01-01',${quote(primary)},'fixture');
      ${level ? `INSERT INTO user_agent_bindings VALUES(${quote(id)},${quote(primary)},${quote(level)},'2026-01-01');` : ''}
      ${id === 'multiple' ? "INSERT INTO user_agent_bindings VALUES('multiple','prod-shaped','operator','2026-01-01');" : ''}
    `);
  }
  chmodSync(join(context.home, 'users.db'), 0o600);
  check(!sql("PRAGMA table_info(users)").some(column => column.name === 'avatar_agent_id'), 'fixture is not pre-upgrade');
}
function legacySnapshot() {
  return { users: sql("SELECT id,email,display_name,password_hash,role,status,created_at,updated_at,last_login,must_change_password,bootstrap_owner,department,manager_user_id,primary_agent_id FROM users WHERE id <> 'fixture-admin' ORDER BY id"),
    bindings: sql("SELECT user_id,agent_name,access_level,bound_at FROM user_agent_bindings WHERE user_id <> 'fixture-admin' ORDER BY user_id,agent_name") };
}
function assertLegacyPreserved(before, after) {
  assert.deepEqual(after, before, 'migration changed existing legacy users or binding fields');
}
function avatars() { return sql("SELECT id,avatar_agent_id FROM users WHERE id <> 'fixture-admin' ORDER BY id"); }
async function avatarScenario() {
  const before = legacySnapshot(); artifact('legacy-shape.json', { users: before.users.map(({ password_hash, ...row }) => row), bindings: before.bindings });
  await boot('migration');
  const expected = { single: 'prod-shaped', multiple: 'plain' };
  const first = avatars();
  const after = legacySnapshot();
  artifact('legacy-after.json', after);
  artifact('avatar-first-boot.json', first);
  for (const row of first) check(row.avatar_agent_id === (expected[row.id] ?? null), `unexpected avatar for ${row.id}`);
  assertLegacyPreserved(before, after);
  // google_identity::migrate adds this column with DEFAULT 0; it does not
  // promote legacy owners. Compare the old fields separately from this addition.
  const primary = sql("SELECT user_id,agent_name,is_primary FROM user_agent_bindings WHERE user_id <> 'fixture-admin' ORDER BY user_id,agent_name");
  check(primary.every(row => row.is_primary === 0), 'migration promoted a legacy binding to primary Owner');
  // An intentionally cleared avatar must stay cleared on the second boot.
  await context.admin.call('users.set_avatar', { user_id: 'single', agent_id: null });
  const cleared = avatars(), clearedBefore = legacySnapshot();
  await shutdown();
  await boot('idempotence');
  check(JSON.stringify(avatars()) === JSON.stringify(cleared), 'second boot changed avatar assignments');
  assertLegacyPreserved(clearedBefore, legacySnapshot());
  assert.deepEqual(sql("SELECT user_id,agent_name,is_primary FROM user_agent_bindings WHERE user_id <> 'fixture-admin' ORDER BY user_id,agent_name"), primary);
  artifact('avatar-migration.json', { first, second: avatars(), added_binding_primary_flags: primary, legacy_fields_unchanged: true, cleared_avatar_preserved: true });
}

async function wikiScenario() {
  const client = await mcpClient();
  const wiki = join(context.home, 'agents/prod-shaped/wiki'), outside = join(context.root, 'outside-wiki');
  const body = '---\ntitle: Local acceptance\ncreated: 2026-01-01\nupdated: 2026-01-01\ntags: [local]\nlayer: L1\ntrust: verified\n---\nLocal acceptance page sentinel.\n';
  const observations = [];
  try {
    for (const name of ['wiki_write', 'wiki_read']) check(client.tools.some(t => t.name === name), `${name} not visible`);
    toolOK(await client.call('wiki_write', { page_path: 'concepts/acceptance.md', content: body }), 'normal write');
    check(toolOK(await client.call('wiki_read', { page_path: 'concepts/acceptance.md' }), 'normal read').includes('Local acceptance page sentinel.'), 'normal read lost content');
    mkdirSync(outside, { mode: 0o700 }); writeFileSync(join(outside, 'victim.md'), 'UNCHANGED', { mode: 0o600 });
    symlinkSync(outside, join(wiki, 'linked'));
    const refused = async (name, page_path, pattern, invalidId = false) => {
      let result, rpcError;
      try { result = await client.call(name, { page_path, ...(name === 'wiki_write' ? { content: body } : {}) }); }
      catch (error) { if (!error.rpcError) throw error; rpcError = error.rpcError; }
      const text = assertWikiRefusal(result, rpcError, invalidId, pattern);
      observations.push({ name, case: page_path.startsWith(context.root) ? 'absolute' : page_path, error: text });
    };
    const mutationRefusal = /Not available to AI tools: pages under auto\//;
    for (const path of ['../escape.md', join(outside, 'victim.md')]) {
      await refused('wiki_write', path, null, true);
      await refused('wiki_read', path, null, true);
    }
    await refused('wiki_read', 'linked/victim.md', /Page path refused: it must be a regular file inside the wiki directory/);
    await refused('wiki_write', 'linked/new.md', mutationRefusal);
    // Plant a control-file link to an existing file INSIDE the wiki: confinement
    // alone is insufficient; control files must be regular files too.
    const control = join(wiki, '_index.md'), saved = readFileSync(control);
    unlinkSync(control); symlinkSync(join(wiki, 'concepts/acceptance.md'), control);
    const beforePage = readFileSync(join(wiki, 'concepts/acceptance.md'), 'utf8');
    try { await refused('wiki_write', 'concepts/control-refused.md', /(?:symlink|symbolic|linked|regular file|Too many levels)/i); }
    finally { unlinkSync(control); writeFileSync(control, saved, { mode: 0o600 }); }
    check(!existsSync(join(wiki, 'concepts/control-refused.md')), 'control-link refusal still wrote a page');
    check(readFileSync(join(wiki, 'concepts/acceptance.md'), 'utf8') === beforePage, 'control-link target changed');
    check(readFileSync(join(outside, 'victim.md'), 'utf8') === 'UNCHANGED' && !existsSync(join(outside, 'new.md')), 'wiki boundary changed outside data');
    toolOK(await client.call('wiki_write', { page_path: 'concepts/restored.md', content: body }), 'write after control restoration');
    artifact('wiki-paths.json', observations);
  } finally { await client.close(); }
}

async function memberKeyScenario() {
  const password = `Fixture-${randomBytes(24).toString('hex')}!`; secrets.add(password);
  const created = await context.admin.call('users.create', { email: 'member@acceptance.invalid', display_name: 'Local Member', password, role: 'employee' });
  const uid = created.user?.id; check(uid, 'users.create returned no member');
  await context.admin.call('users.bind_agent', { user_id: uid, agent_name: 'prod-shaped', access_level: 'owner' });
  await context.admin.call('users.set_avatar', { user_id: uid, agent_id: 'prod-shaped' });
  const login = await http('/api/login', { email: 'member@acceptance.invalid', password });
  secrets.add(login.access_token); secrets.add(login.refresh_token);
  const member = await rpcClient({ jwt: login.access_token });
  let keyRPC, chat, phase = 'create and authenticate RPC key';
  try {
    const issued = await member.call('me.member_keys.create', { name: 'Local acceptance', expires_in_days: 1, password });
    const key = issued.secret; secrets.add(key);
    check(key?.startsWith('ddc_member_') && issued.key?.id, 'member key response missing secret/id');
    keyRPC = await rpcClient({ member_key: key });
    const me = await keyRPC.call('users.me');
    check(me.user?.id === uid && Array.isArray(me.bindings) && me.email_setup, 'users.me contract fields/identity mismatch');
    const about = await keyRPC.call('about.get');
    for (const field of ['product_name', 'vendor', 'version', 'tier', 'brand_revision']) check(field in about, `about.get missing ${field}`);
    const agents = await keyRPC.call('agents.list'); check(Array.isArray(agents.agents), 'agents.list contract field missing');
    const denied = await keyRPC.raw('users.list');
    check(!denied.ok && denied.error?.code === 'member_key_forbidden', 'admin-only RPC was not refused by member-key gate');
    phase = 'authenticate chat';
    chat = new Socket('/ws/chat'); await chat.open(); chat.send({ type: 'auth', member_key: key });
    const hello = await chat.take(frame => ['session_info', 'error'].includes(frame.type));
    check(hello.type === 'session_info' && hello.resolved_agent, 'member chat authentication/partner resolution failed');
    phase = 'authenticate HTTP and revoke';
    const meHTTP = await http('/api/me', undefined, key); check(JSON.stringify(meHTTP).includes(uid), 'HTTP key identity mismatch');
    check((await context.admin.call('member_keys.admin_revoke', { id: issued.key.id })).revoked, 'admin revoke failed');
    // Revocation is checked on the next call OR heartbeat (30s). Send a harmless
    // non-model chat command to measure immediate revalidation within 5 seconds.
    phase = 'close revoked chat';
    chat.send({ type: 'ping' });
    const revokedClose = await chat.expectRefusal();
    phase = 'refuse revoked reconnect';
    const reconnect = new Socket('/ws/chat');
    let reconnectClose;
    try {
      await reconnect.open(); reconnect.send({ type: 'auth', member_key: key });
      reconnectClose = await reconnect.expectRefusal(true);
    } finally { reconnect.close(); }
    artifact('member-key.json', { created: true, contract_methods: ['users.me', 'about.get', 'agents.list'], admin_refused: true,
      chat_authenticated: true, revoked_chat_closed_on_next_frame: true, reconnect_refused: true,
      revoked_close: revokedClose, reconnect_close: reconnectClose });
  } catch (error) {
    throw new Error(`member key ${phase}: ${error.message}`);
  } finally { member.close(); keyRPC?.close(); chat?.close(); }
}

async function openChat(token = context.jwt) {
  const chat = new Socket('/ws/chat'); await chat.open();
  chat.send({ type: 'auth', token });
  const hello = await chat.take(f => ['session_info', 'error'].includes(f.type));
  check(hello.type === 'session_info' && hello.session_id, 'chat auth failed');
  chat.sessionId = hello.session_id;
  return chat;
}
function conversationSession(base, agent, conv) { return `${base}#agent:${agent}#conv:${conv}`; }
function matchesFixtureJob(job, runtime, prompt) {
  return job.runtime === runtime && typeof job.marker === 'string' && job.marker.length > 0 && prompt.includes(job.marker);
}
function fixtureAgentConfig(text, name, runtime) {
  return text.replace(/^name = "[^"]+"/m, `name = "${name}"`)
    .replace(/^trigger = "[^"]+"/m, `trigger = "@${name}"`)
    .replace('provider = "claude"', `provider = "${runtime}"`)
    .replaceAll('claude-haiku-4-5', runtime === 'codex' ? 'gpt-5.3-codex' : 'claude-haiku-4-5')
    // The fixture implements foreground turns only. Disable autonomous
    // evolution so it cannot consume a foreground job or clobber its evidence.
    .replace('gvu_enabled = true', 'gvu_enabled = false')
    // Codex cannot enforce Bash approval. Keep the MCP allowlist and denied
    // WebFetch, but use a representable native policy for this identity probe.
    .replace('approval_required_tools = ["Bash"]', runtime === 'codex' ? 'approval_required_tools = []' : 'approval_required_tools = ["Bash"]');
}
function disposableLicense(fingerprint, now = Date.now()) {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519');
  const issuedAt = new Date(now).toISOString().replace(/\.000Z$/, 'Z');
  const expiresAt = new Date(now + 86400000).toISOString().replace(/\.000Z$/, 'Z');
  // Field order is License::canonical_payload, as in google-personal-identity.
  const payload = { version: 2, subscription_id: 'sub_v185_acceptance', customer_id: 'fixture', tier: 'oem',
    machine_fingerprint: fingerprint, issued_at: issuedAt, expires_at: expiresAt,
    last_phone_home: issuedAt, public_key_id: 'v185_fixture', nfr: true };
  const signature = sign(null, Buffer.from(JSON.stringify(payload)), privateKey).toString('base64');
  return { payload, license: { ...payload, signature }, publicKey };
}
function installEnterpriseLicense() {
  const fingerprint = spawnSync('/usr/bin/sandbox-exec', ['-f', context.policy, context.binary, 'license', 'fingerprint'], {
    env: minimalEnv(context.home), cwd: context.home, encoding: 'utf8', timeout: 10000,
  });
  check(!fingerprint.error && fingerprint.status === 0, `isolated license fingerprint failed: ${fingerprint.stderr || fingerprint.error?.message}`);
  const value = fingerprint.stdout.trim().split('\n')[0]; check(value, 'empty license fingerprint');
  const fixture = disposableLicense(value);
  writeFileSync(join(context.home, 'license.json'), JSON.stringify(fixture.license), { mode: 0o600 });
  context.fixtureEnv = { DUDUCLAW_EDITION: 'enterprise',
    DUDUCLAW_LICENSE_PUBKEY_V185_FIXTURE: Buffer.from(fixture.publicKey.export({ format: 'jwk' }).x, 'base64url').toString('hex') };
}
async function fixtureTurn(chat, runtime, job = {}, content = 'Check only the local run identity.', conv = `acceptance-${runtime}`) {
  const id = randomBytes(12).toString('hex'), marker = `fixture-request:${id}`;
  const dir = join(context.root, `turn-${id}`); mkdirSync(dir, { mode: 0o700 });
  const leasePath = join(dir, 'lease.json'), release = join(dir, 'release');
  context.leasePaths.add(leasePath);
  const agent = runtime === 'claude' ? 'prod-shaped' : 'codex-shaped';
  const expectedSession = conversationSession(chat.sessionId, agent, conv);
  const request = { ...job, id, runtime, marker, conv, expected_session: expectedSession };
  writeFileSync(join(context.root, `job-${runtime}.json`), JSON.stringify(request), { mode: 0o600 });
  artifact(`turn-${id}/job.json`, request);
  const terminal = frame => frame.type === 'error' || (frame.type === 'assistant_done' && frame.conv === conv);
  let lease, record;
  try {
    // Echo the connection's session id, as the dashboard does. Conv is a
    // stable bucket nonce; a fresh fixture id identifies only this request.
    chat.send({ type: 'user_message', session_id: chat.sessionId, agent, content: `${content}\n[${marker}]`, conv, msg_id: id });
    await until(() => {
      alive(context.gateway);
      const error = chat.queue.find(f => f.type === 'error'); check(!error, `chat turn refused: ${JSON.stringify(error)}`);
      // The observation is published before assistant_done, but a scheduler
      // can deliver both before this poll. Read evidence first.
      if (existsSync(leasePath)) return true;
      const done = chat.queue.find(terminal);
      check(!done, `turn ended before the CLI fixture produced evidence: ${JSON.stringify(done)}`);
      return false;
    }, `${runtime} fixture observation`, 45000);
    lease = JSON.parse(readFileSync(leasePath, 'utf8')); secrets.add(lease.credential); unlinkSync(leasePath);
    record = JSON.parse(readFileSync(join(dir, 'identity.json'), 'utf8'));
    check(lease.job_id === id && record.job_id === id, 'CLI evidence belongs to a different fixture request');
  } finally {
    writeFileSync(release, 'release\n', { mode: 0o600 });
    artifact(`turn-${id}/chat-frames.json`, chat.queue);
    if (lease) {
      const done = await chat.take(terminal, 45000);
      check(done.type === 'assistant_done', 'shim turn did not finish');
      await until(async () => {
        const response = await fetch(lease.url, { method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ credential: lease.credential }), signal: AbortSignal.timeout(2000) });
        check(response.ok, 'identity listener unavailable during post-turn check');
        return (await response.json()).active === false;
      }, 'completed run credential revocation', 5000);
    }
  }
  check(record.normal.session_id === expectedSession, `foreground run session mismatch: expected ${expectedSession}, got ${record.normal.session_id}`);
  return record;
}
async function identityScenario(runtime) {
  const chat = await openChat();
  try {
    const record = await fixtureTurn(chat, runtime);
    const expected = runtime === 'claude' ? 'prod-shaped' : 'codex-shaped';
    check(record.normal.active && record.normal.agent_id === expected && record.normal.runtime === runtime, 'wrong run employee/runtime');
    check(record.normal.turn_id && record.normal.session_id && record.normal.principal?.type === 'human', 'missing run/session/principal');
    const owner = (await context.admin.call('users.me')).user.id;
    check(record.normal.principal.user_id === owner, 'wrong run principal');
    for (const key of ['agent_id', 'turn_id', 'session_id', 'runtime']) check(record.normal[key] === record.forged[key], `forged environment changed ${key}`);
    check(record.argument_forgery_ignored && record.bad_credential_inactive, 'identity argument/credential negative control failed');
  } finally { chat.close(); }
}
async function memoryScenario() {
  const chat = await openChat();
  try {
    // Both utility extraction and foreground replies use deterministic local
    // CLI fixtures. This verifies persistence/injection, not model judgement.
    writeFileSync(join(context.root, 'extract-pin'), 'enabled', { mode: 0o600 });
    const first = await fixtureTurn(chat, 'claude', { action: 'task-create' }, 'Compile the local acceptance report.', 'memory-fixture');
    check(first.task_id && first.mcp_task_created, 'real stdio MCP did not create the disposable task');
    const quote = value => `'${value.replaceAll("'", "''")}'`;
    const session = first.normal.session_id;
    const rows = query => JSON.parse(system('/usr/bin/sqlite3', ['-readonly', '-json', join(context.home, 'sessions.db'), query]) || '[]');
    await until(() => rows(`SELECT pinned_instructions FROM sessions WHERE id=${quote(session)}`)
      .some(row => row.pinned_instructions?.includes('Compile the local acceptance report')), 'pinned task persistence');
    check(rows(`SELECT ref_id FROM session_tool_refs WHERE session_id=${quote(session)}`)
      .some(row => row.ref_id === first.task_id), 'MCP result reference not stored in the issuing session');
    const second = await fixtureTurn(chat, 'claude', { action: 'recall', task_id: first.task_id }, 'Please continue.', 'memory-fixture');
    check(second.normal.session_id === session && second.normal.turn_id !== first.normal.turn_id, 'second turn did not reuse the same conversation');
    check(second.saw_pinned_task && second.saw_task_reference, 'second turn prompt missing pinned note or remembered result id');
    const unrelated = await fixtureTurn(chat, 'claude', { action: 'recall', task_id: first.task_id }, 'Inspect a fresh local conversation.', 'memory-unrelated');
    check(!unrelated.saw_task_reference && !unrelated.saw_pinned_task, 'reference leaked into an unrelated conversation');
    artifact('memory-across-turns.json', { task_created_via_stdio: true, pinned_extraction: 'local CLI fixture', persisted: true,
      same_conversation_injected: true, unrelated_conversation_isolated: true });
  } finally { if (existsSync(join(context.root, 'extract-pin'))) unlinkSync(join(context.root, 'extract-pin')); chat.close(); }
}
async function selfAddScenario() {
  const owner = (await context.admin.call('users.me')).user.id;
  await context.admin.call('users.bind_agent', { user_id: owner, agent_name: 'prod-shaped', access_level: 'owner' });
  await context.admin.call('users.set_avatar', { user_id: owner, agent_id: 'prod-shaped' });
  const chat = await openChat();
  try {
    const created = await context.admin.call('knowledge.collections.create', { name: 'Local private collection', slug: 'local-private' });
    const cid = created.collection.id;
    const acl = await context.admin.call('knowledge.acl.get', { collection_id: cid });
    await context.admin.call('knowledge.acl.set', { collection_id: cid, expected_revision: acl.revision, entries: [
      { kind: 'user', id: owner, role: 'owner' }, { kind: 'agent', id: 'prod-shaped', role: 'viewer' },
    ] });
    const current = await context.admin.call('knowledge.collections.get', { id: cid });
    await context.admin.call('knowledge.collections.update', { id: cid, expected_updated_at: current.collection.updated_at, owner_self_add: true });
    const pasted = 'The owner supplied this local knowledge source verbatim in this message.';
    const other = 'This different source was found by the fixture and was never pasted by the owner.';
    const before = await context.admin.call('knowledge.proposals.list');
    await fixtureTurn(chat, 'claude', { action: 'knowledge', collection: 'local-private', text: pasted, title: 'Owner pasted source' }, `Add this text to my knowledge base: ${pasted}`, 'knowledge-fixture');
    const afterPaste = await context.admin.call('knowledge.proposals.list');
    check(afterPaste.proposals.length === before.proposals.length, 'owner-pasted source unexpectedly required approval');
    const sources = await context.admin.call('knowledge.sources.list', { collection_id: cid });
    check(sources.sources?.some(source => source.title === 'Owner pasted source'), 'owner-pasted source was not imported');
    await fixtureTurn(chat, 'claude', { action: 'knowledge', collection: 'local-private', text: other, title: 'Tool-discovered source' }, 'Propose the other source discovered by the local tool.', 'knowledge-fixture');
    const pending = await context.admin.call('knowledge.proposals.list');
    const card = pending.proposals.find(item => item.collection_id === cid && item.title === 'Tool-discovered source');
    check(card && card.preview?.includes(other), 'non-pasted content did not create the expected approval card/preview');
    const finalSources = await context.admin.call('knowledge.sources.list', { collection_id: cid });
    check(!finalSources.sources.some(source => source.title === 'Tool-discovered source'), 'unapproved source became visible');
    artifact('knowledge-self-add.json', { owner_paste_imported: true, owner_paste_no_card: true,
      other_content_pending: true, card_preview: true, unapproved_content_not_imported: true });
  } finally { chat.close(); await context.admin.call('users.set_avatar', { user_id: owner, agent_id: null }); }
}

// A local CLI fixture, invoked only by the real runtime's spawn path. It does
// not execute prompts, models or vendor CLIs. Its Bash child uses the real CLI.
async function fixtureCLI(runtime, root, binary, args) {
  context = { root, home: join(root, 'home'), binary,
    fixtureEnv: Object.fromEntries(Object.entries(process.env).filter(([key]) => key === 'DUDUCLAW_EDITION' || key.startsWith('DUDUCLAW_LICENSE_PUBKEY_'))) };
  if (args.includes('--version')) { console.log(runtime === 'codex' ? 'codex-cli 0.200.0' : '99.0.0'); return; }
  if (args.includes('auth') || args.includes('login')) { console.log(JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', email: 'fixture@acceptance.invalid' })); return; }
  const utility = runtime === 'claude' && args.includes('--no-session-persistence');
  let input = '';
  if (runtime === 'codex' || utility) for await (const chunk of process.stdin) input += chunk;
  if (utility) {
    const extract = existsSync(join(root, 'extract-pin')) && input.startsWith('Extract the core task instructions')
      && input.includes('Compile the local acceptance report.');
    console.log(JSON.stringify({ type: 'result', result: extract ? '- Compile the local acceptance report.' : 'No task.', is_error: false }));
    return;
  }
  if (!process.env.DUDUCLAW_RUN_CREDENTIAL) {
    // Background utility work is disabled. Do not pretend it has a run identity.
    if (runtime === 'claude') console.log(JSON.stringify({ type: 'result', result: 'No background work.', is_error: false }));
    return;
  }
  let prompt = `${args.join('\n')}\n${input}`;
  for (const flag of ['--system-prompt-file', '--append-system-prompt-file']) {
    const index = args.indexOf(flag);
    if (index >= 0) prompt += readFileSync(args[index + 1], 'utf8');
  }
  const jobPath = join(root, `job-${runtime}.json`);
  const job = existsSync(jobPath) ? JSON.parse(readFileSync(jobPath, 'utf8')) : {};
  const matched = matchesFixtureJob(job, runtime, prompt);
  appendFileSync(join(root, 'fixture-invocations.jsonl'), JSON.stringify({ runtime, pid: process.pid, job_id: job.id, matched }) + '\n', { mode: 0o600 });
  if (!matched) {
    if (runtime === 'claude') console.log(JSON.stringify({ type: 'result', result: 'No background work.', is_error: false }));
    return;
  }
  check(/^[a-f0-9]{24}$/.test(job.id), 'invalid fixture request id');
  const dir = join(root, `turn-${job.id}`);
  // Foreground and utility spawns must never overwrite one another's proof.
  // A second claimant is a fixture error, not evidence from the desired turn.
  mkdirSync(join(dir, 'claimed'), { mode: 0o700 });
  const home = context.home;
  const lookup = extra => {
    const response = spawnSync('/bin/bash', ['-c', 'exec "$1" identity whoami --json', 'identity-fixture', binary], {
      env: { ...process.env, ...extra, HOME: join(home, 'os-home'), DUDUCLAW_HOME: home },
      cwd: join(home, 'os-home'), encoding: 'utf8', timeout: 10000,
    });
    check(response.status === 0, 'Bash identity whoami failed'); return JSON.parse(response.stdout);
  };
  const normal = lookup({});
  const forged = lookup({ DUDUCLAW_AGENT_ID: 'plain', DUDUCLAW_SESSION_ID: 'forged-session',
    DUDUCLAW_TURN_ID: 'forged-turn', DUDUCLAW_ON_BEHALF_OF: 'user:forged', DUDUCLAW_RUNTIME: 'forged' });
  const url = new URL(process.env.DUDUCLAW_IDENTITY_URL);
  check(url.hostname === '127.0.0.1' && url.protocol === 'http:' && url.pathname === '/api/internal/identity/introspect', 'unsafe identity URL');
  const introspect = async value => {
    const response = await fetch(url, { method: 'POST', redirect: 'error', headers: { 'content-type': 'application/json' },
      body: JSON.stringify(value), signal: AbortSignal.timeout(5000) });
    check(response.ok, 'identity introspection failed'); return response.json();
  };
  const argument = await introspect({ credential: process.env.DUDUCLAW_RUN_CREDENTIAL, agent_id: 'plain', turn_id: 'forged', principal: { type: 'human', user_id: 'forged' } });
  const bad = await introspect({ credential: `ddc_run_${'A'.repeat(43)}` });
  const observation = { job_id: job.id, normal, forged, argument_forgery_ignored: argument.agent_id === normal.agent_id && argument.turn_id === normal.turn_id
    && JSON.stringify(argument.principal) === JSON.stringify(normal.principal), bad_credential_inactive: bad.active === false };
  if (job.action === 'recall') {
    observation.saw_pinned_task = prompt.includes('<pinned_task_data>') && prompt.includes('Compile the local acceptance report.');
    observation.saw_task_reference = prompt.includes('<session_tool_refs_data>') && prompt.includes(job.task_id);
  }
  if (runtime === 'claude' && ['task-create', 'knowledge'].includes(job.action)) {
    const index = args.indexOf('--mcp-config');
    check(index >= 0, 'foreground runtime omitted MCP registration');
    const source = args[index + 1];
    const registration = JSON.parse(source.startsWith('{') ? source : readFileSync(source, 'utf8'));
    const mcp = await mcpClient({ registration, extra: { DUDUCLAW_RUN_CREDENTIAL: process.env.DUDUCLAW_RUN_CREDENTIAL,
      DUDUCLAW_IDENTITY_URL: url.href }, confined: false, label: `shim-mcp-${job.id}` });
    try {
      if (job.action === 'task-create') {
        const result = await mcp.call('tasks_create', { kind: 'task', title: 'Local remembered result', description: 'Disposable local fixture task.', assigned_to: 'prod-shaped' });
        const data = JSON.parse(toolOK(result, 'tasks_create'));
        observation.mcp_task_created = Boolean(data.task?.id); observation.task_id = data.task?.id;
      } else {
        toolOK(await mcp.call('knowledge_add_source', { collection: job.collection, text: job.text, title: job.title }), 'knowledge_add_source');
        observation.knowledge_tool_completed = true;
      }
    } finally { await mcp.close(); }
  }
  artifact(`turn-${job.id}/identity.json`, observation);
  // The parent consumes and unlinks this private transport file before logging.
  const leasePath = join(dir, 'lease.json');
  writeFileSync(`${leasePath}.tmp`, JSON.stringify({ job_id: job.id, credential: process.env.DUDUCLAW_RUN_CREDENTIAL, url: url.href, turn_id: normal.turn_id }), { mode: 0o600 });
  renameSync(`${leasePath}.tmp`, leasePath);
  await until(() => existsSync(join(dir, 'release')), 'fixture release', 50000);
  if (runtime === 'claude') console.log(JSON.stringify({ type: 'result', result: 'Local identity fixture passed.', is_error: false }));
  else {
    console.log(JSON.stringify({ type: 'thread.started', thread_id: 'local-fixture' }));
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'agent_message', text: 'Local identity fixture passed.' } }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: { input_tokens: 0, output_tokens: 0 } }));
  }
}

function parseOptions(args) {
  const options = { port: 18985, only: null };
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (key === '--help' || key === '-h') return { help: true };
    check(key === '--port' || key === '--only', 'usage: v185-acceptance.mjs [--port N] [--only avatar,memory,identity,wiki,member-key,self-add]');
    const value = args[++index]; check(value, `${key} needs a value`);
    if (key === '--port') options.port = Number(value); else options.only = value.split(',');
  }
  check(Number.isInteger(options.port) && options.port >= 1024 && options.port <= 65535 && options.port !== 18889, 'invalid/non-isolated port');
  return options;
}
function selectedNames(values) {
  if (!values) return new Set(['avatar', 'memory', 'identity', 'wiki', 'member-key', 'self-add']);
  // Legacy selectors are resolved from implementation annotations at runtime;
  // public artifacts and source never embed internal work-item numbers.
  const annotations = [
    ['avatar', 'crates/duduclaw-gateway/src/handlers/tests/member_contract_tests.rs', /RM-(\d+)'s avatar/],
    ['member-key', 'crates/duduclaw-gateway/src/member_keys.rs', /^\/\/! RM-(\d+)/m],
    ['identity', 'crates/duduclaw-gateway/src/run_identity.rs', /^\/\/! RM-(\d+)/m],
    ['wiki', 'crates/duduclaw-memory/src/wiki_path.rs', /mod rm(\d+)_identity_tests/],
    ['memory', 'crates/duduclaw-gateway/src/channel_reply/inner.rs', /fn rm(\d+)_off_guards/],
  ];
  const allowed = new Set(['avatar', 'memory', 'identity', 'wiki', 'member-key', 'self-add']);
  const aliases = new Map(annotations.map(([name, path, pattern]) => {
    const match = readFileSync(join(repo, path), 'utf8').match(pattern);
    return [match ? `rm${match[1]}` : '', name];
  }));
  return new Set(values.map(value => { const name = aliases.get(value.toLowerCase()) || value; check(allowed.has(name), 'unknown scenario selector'); return name; }));
}
async function main() {
  const options = parseOptions(process.argv.slice(2));
  if (options.help) { console.log('DUDUCLAW_BIN=/absolute/path node scripts/live-test/v185-acceptance.mjs [--port N] [--only avatar,memory,identity,wiki,member-key,self-add]'); return; }
  check(Number(process.versions.node.split('.')[0]) >= 22 && typeof WebSocket === 'function', 'Node 22+ with global WebSocket required');
  const selected = selectedNames(options.only);
  const root = realpathSync(mkdtempSync(join(tmpdir(), 'duduclaw-v185-')));
  chmodSync(root, 0o700);
  context = { root, home: join(root, 'home'), port: options.port, base: `http://127.0.0.1:${options.port}`,
    binary: resolve(process.env.DUDUCLAW_BIN || ''), canary: `acceptance-${randomBytes(12).toString('hex')}`,
    ownerEmail: 'owner@acceptance.invalid', ownerPassword: `Fixture-${randomBytes(24).toString('hex')}!`, leasePaths: new Set() };
  secrets.add(context.ownerPassword);
  console.log(`ARTIFACTS ${root}`);
  try {
    check(process.env.DUDUCLAW_BIN && existsSync(context.binary), 'set DUDUCLAW_BIN to an already-built executable');
    check(process.platform === 'darwin' && existsSync('/usr/bin/sandbox-exec'), 'macOS sandbox-exec required for guaranteed loopback/keychain isolation; run native Windows acceptance separately');
    for (const path of ['tmp', 'bin', 'setup-home', 'setup-home/os-home', 'home']) mkdirSync(join(root, path), { mode: 0o700 });
    const home = context.home;
    // Setup itself receives both isolated homes; make-home refuses nonempty targets.
    const setup = spawnSync('/bin/bash', [join(scriptDir, 'make-home.sh'), home, '--port', String(options.port)], {
      env: minimalEnv(join(root, 'setup-home')), cwd: root, encoding: 'utf8', timeout: 10000,
    });
    check(!setup.error && setup.status === 0, `make-home.sh failed: ${setup.stderr || setup.error?.message}`);
    artifact('setup.json', { stdout: setup.stdout });
    writeFileSync(join(home, 'config.toml'), `[gateway]\nbind = "127.0.0.1"\nport = ${options.port}\nauto_update = false\n[general]\ndefault_language = "zh-TW"\nlog_level = "info"\n[dashboard]\nlocal_auto_login = true\n[member_api_keys]\nenabled = true\nrate_per_minute = 120\n[memory]\npinned_task = "first_message"\n[runtime]\nutility_provider = "claude"\n[knowledge]\nenabled = true\nowner_self_add = true\n[knowledge.embedding]\nprovider = "none"\n[dispatch]\nenabled = false\n[tick]\nenabled = false\n[proactive]\nenabled = false\n[responsibilities]\nenabled = false\n`, { mode: 0o600 });
    for (const [name, source, runtime] of [[context.canary, 'plain', 'claude'], ['codex-shaped', 'prod-shaped', 'codex']]) {
      const dir = join(home, 'agents', name); mkdirSync(dir, { mode: 0o700 });
      const config = fixtureAgentConfig(readFileSync(join(home, 'agents', source, 'agent.toml'), 'utf8'), name, runtime);
      writeFileSync(join(dir, 'agent.toml'), config, { mode: 0o600 });
      for (const doc of ['SOUL.md', 'CONTRACT.toml']) if (existsSync(join(home, 'agents', source, doc))) writeFileSync(join(dir, doc), readFileSync(join(home, 'agents', source, doc)), { mode: 0o600 });
    }
    const prodConfig = join(home, 'agents/prod-shaped/agent.toml');
    writeFileSync(prodConfig, fixtureAgentConfig(readFileSync(prodConfig, 'utf8'), 'prod-shaped', 'claude'), { mode: 0o600 });
    for (const runtime of ['claude', 'codex']) {
      const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
      writeFileSync(join(root, 'bin', runtime), `#!/bin/sh\nexec ${[process.execPath, file, '--fixture-cli', runtime, root, context.binary].map(quote).join(' ')} "$@"\n`, { mode: 0o700 });
    }
    context.policy = join(root, 'isolation.sb');
    const forbidden = [join(homedir(), '.duduclaw'), join(homedir(), 'Library/Keychains'), '/Library/Keychains', '/System/Library/Keychains'];
    writeFileSync(context.policy, `(version 1)(allow default)\n(deny network*)\n(allow network-bind (local ip "localhost:*"))\n(allow network-inbound (local ip "localhost:*"))\n(allow network-outbound (remote ip "localhost:*"))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*" #".*secd.*"))\n(deny file-read* file-write* ${forbidden.map(path => `(subpath ${JSON.stringify(path)})`).join(' ')})\n`, { mode: 0o600 });
    installEnterpriseLicense();
    // This suite upgrades a disposable pre-avatar store. Its existing legacy
    // MFA policy stays off by design (mfa::boot_policy legacy-store tests),
    // unlike the strict default on an entirely new Enterprise identity store.
    seedLegacy(selected.has('avatar'));
    // No public network probe: the system sandbox is the outbound boundary.
    report('SKIP', 'update-check-switch', 'periodic check has no disabling switch in this branch; auto-install is off and outbound traffic is sandbox-denied');
    if (selected.has('avatar')) {
      await scenario('avatar', 'legacy member migration and second-boot idempotence', avatarScenario);
    }
    if (!context.gateway) await boot('baseline');
    if (selected.has('memory')) await scenario('memory', 'real stdio task result and fixture-extracted pinned note remembered across turns', memoryScenario);
    if (selected.has('identity')) for (const runtime of ['claude', 'codex']) await scenario(`identity-${runtime}`, 'real chat spawn, forged identity refusal and post-turn revocation', () => identityScenario(runtime));
    if (selected.has('wiki')) await scenario('wiki', 'MCP normal I/O, traversal, absolute path, directory/control symlink refusals', wikiScenario);
    if (selected.has('self-add')) {
      await scenario('self-add', 'owner-pasted source imported, other content creates a local approval card', selfAddScenario);
      report('SKIP', 'self-add-delivery', 'verified Telegram/Google Chat private-message card delivery requires a real external channel; local dashboard card is checked');
    }
    if (selected.has('member-key')) await scenario('member-key', 'RPC key creation, member contract, chat authentication, revoke and reconnect refusal', memberKeyScenario);
  } catch (error) { report('FAIL', 'setup', 'isolated harness prerequisites/startup', error.message); }
  finally {
    // Copying only a live WAL-mode .db loses the migration and session rows.
    // SQLite backup produces consistent standalone evidence before shutdown.
    const backups = join(root, 'databases'); mkdirSync(backups, { mode: 0o700 });
    for (const db of ['users', 'sessions']) if (existsSync(join(context.home, `${db}.db`))) {
      try {
        system('/usr/bin/sqlite3', ['-readonly', join(context.home, `${db}.db`), `.backup '${join(backups, `${db}.db`).replaceAll("'", "''")}'`]);
        chmodSync(join(backups, `${db}.db`), 0o600);
      } catch (error) { report('FAIL', 'evidence-backup', `consistent ${db} database backup`, error.message); }
    }
    await cleanup();
    for (const name of selected) for (const id of name === 'identity' ? ['identity-claude', 'identity-codex'] : [name]) {
      if (!checks.some(row => row.id === id)) report('SKIP', id, 'not reached because isolated setup/startup failed; see FAIL evidence');
    }
    artifact('results.json', { checks, failures, live_gateway_run_by_harness: Boolean(context.gateway) });
    console.log(`ARTIFACTS ${root}`);
    if (failures) process.exitCode = 1;
  }
}

async function testHelpers() {
  assert.equal(parseOptions([]).port, 18985);
  assert.deepEqual(parseOptions(['--port', '18986', '--only', 'memory,wiki']), { port: 18986, only: ['memory', 'wiki'] });
  for (const args of [['--port', '18889'], ['--port', '0'], ['--port', 'bad'], ['--port'], ['--wat']]) assert.throws(() => parseOptions(args));
  assert.equal(selectedNames().size, 6);
  assert.deepEqual([...selectedNames(['identity', 'wiki'])], ['identity', 'wiki']);
  assert.throws(() => selectedNames(['unknown']));
  assert(!sanitise('ddc_run_ABC123').includes('ABC123'));
  const redacted = redactArtifact({ password_hash: 'hidden', must_change_password: 0, nested: [{ secret: 'hidden', active: true }] });
  assert.deepEqual(JSON.parse(JSON.stringify(redacted)), { password_hash: '[REDACTED]', must_change_password: 0, nested: [{ secret: '[REDACTED]', active: true }] });
  assert.equal(assertWikiRefusal(undefined, { code: -32602, message: 'Invalid wiki page id' }, true), 'Invalid wiki page id');
  for (const error of [{ code: -32603, message: 'Invalid wiki page id' }, { code: -32602, message: 'unrelated error' }]) assert.throws(() => assertWikiRefusal(undefined, error, true));
  assert.throws(() => assertWikiRefusal({ content: [] }, undefined, false, /refused/));
  assert.equal(assertWikiRefusal({ isError: true, content: [{ text: 'path refused' }] }, undefined, false, /path refused/), 'path refused');
  assert.throws(() => assertWikiRefusal(undefined, { code: -32602, message: 'Invalid wiki page id' }, false, /refused/));
  const before = { users: [{ id: 'single', primary_agent_id: 'prod-shaped', updated_at: 'legacy' }], bindings: [{ user_id: 'single', agent_name: 'prod-shaped', access_level: 'operator', bound_at: 'legacy' }] };
  assertLegacyPreserved(before, structuredClone(before));
  for (const field of ['primary_agent_id', 'updated_at']) {
    const changed = structuredClone(before); changed.users[0][field] = 'changed'; assert.throws(() => assertLegacyPreserved(before, changed));
  }
  for (const field of ['user_id', 'agent_name', 'access_level', 'bound_at']) {
    const changed = structuredClone(before); changed.bindings[0][field] = 'changed'; assert.throws(() => assertLegacyPreserved(before, changed));
  }
  const base = 'webchat:webchat:owner:socket';
  assert.equal(conversationSession(base, 'prod-shaped', 'memory-fixture'), conversationSession(base, 'prod-shaped', 'memory-fixture'));
  assert.notEqual(conversationSession(base, 'prod-shaped', 'memory-fixture'), conversationSession(base, 'prod-shaped', 'memory-unrelated'));
  const job = { marker: 'fixture-request:current', runtime: 'claude' };
  assert(matchesFixtureJob(job, 'claude', 'user [fixture-request:current]'));
  assert(!matchesFixtureJob(job, 'claude', 'user [fixture-request:previous]'));
  assert(!matchesFixtureJob(job, 'codex', 'user [fixture-request:current]'));
  assert(!matchesFixtureJob({}, 'claude', 'background'));
  const source = '[agent]\nname = "prod-shaped"\ntrigger = "@prod-shaped"\nprovider = "claude"\nallowed_tools = ["mcp__duduclaw__*", "Bash"]\ndenied_tools = ["WebFetch"]\napproval_required_tools = ["Bash"]\ngvu_enabled = true\npreferred = "claude-haiku-4-5"';
  const codex = fixtureAgentConfig(source, 'codex-shaped', 'codex');
  for (const expected of ['name = "codex-shaped"', 'provider = "codex"', 'gpt-5.3-codex', 'approval_required_tools = []', 'gvu_enabled = false', '"mcp__duduclaw__*"', 'denied_tools = ["WebFetch"]']) assert(codex.includes(expected));
  assert(fixtureAgentConfig(source, 'prod-shaped', 'claude').includes('approval_required_tools = ["Bash"]'));
  const fixture = disposableLicense('local-fixture-fingerprint', 1795560000000);
  assert.equal(fixture.payload.tier, 'oem');
  assert(!fixture.payload.issued_at.endsWith('.000Z'), 'whole-second dates must match Chrono canonical serialization');
  assert(verify(null, Buffer.from(JSON.stringify(fixture.payload)), fixture.publicKey, Buffer.from(fixture.license.signature, 'base64')));
  assert(!verify(null, Buffer.from(JSON.stringify({ ...fixture.payload, tier: 'business' })), fixture.publicKey, Buffer.from(fixture.license.signature, 'base64')));
  // No network: drive the reader state through clean/abnormal retirement,
  // malformed JSON, continued data and errors that never actually close.
  const socketState = (extra = {}) => Object.assign(Object.create(Socket.prototype), {
    queue: [], waiters: [], closed: true, closeInfo: { code: 1000, wasClean: true }, ...extra,
  });
  assert.deepEqual(await socketState().expectRefusal(), { code: 1000, wasClean: true, transport_error: false });
  const abnormal = socketState({ transportError: true, closeInfo: { code: 1006, wasClean: false } });
  assert.deepEqual(await abnormal.expectRefusal(), { code: 1006, wasClean: false, transport_error: true });
  await assert.rejects(abnormal.take(() => true), /transport failure/);
  const closing = socketState({ closed: false, transportError: true });
  queueMicrotask(() => { closing.closed = true; closing.pump(); });
  await closing.expectRefusal();
  await assert.rejects(socketState({ closed: false, transportError: true }).expectRefusal(false, 5), /timed out/);
  await assert.rejects(socketState({ badFrame: true }).expectRefusal(), /JSON failure/);
  await assert.rejects(socketState({ badFrame: true, queue: [{ type: 'session_info' }] }).take(() => true), /JSON failure/);
  const authError = { type: 'error', message: 'authentication failed: member key invalid' };
  await socketState({ queue: [authError] }).expectRefusal(true);
  await assert.rejects(socketState({ queue: [authError] }).expectRefusal(), /unexpected chat data/);
  await assert.rejects(socketState({ queue: [{ type: 'error', message: 'unrelated failure' }] }).expectRefusal(true), /unexpected chat data/);
  await assert.rejects(socketState({ queue: [authError, { type: 'session_info' }] }).expectRefusal(true), /unexpected chat data/);
  await assert.rejects(socketState({ queue: [{ type: 'assistant_message', content: 'still active' }] }).expectRefusal(), /unexpected chat data/);
  console.log('PASS pure helper tests (options, redaction, wiki refusal layers, legacy preservation, session/job correlation, runtime fixture policy, signed license, member-key socket refusal)');
}

export { parseOptions, selectedNames, sanitise, testHelpers };

if (process.argv[1] && resolve(process.argv[1]) === file) {
  if (process.argv[2] === '--test-helpers') {
    await testHelpers();
  } else if (process.argv[2] === '--fixture-cli') {
    try { await fixtureCLI(process.argv[3], process.argv[4], process.argv[5], process.argv.slice(6)); }
    catch (error) { console.error(sanitise(error.message)); await cleanup(); process.exitCode = 1; }
  } else {
    for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
      void cleanup().finally(() => { console.error('FAIL interrupted harness stopped'); process.exit(signal === 'SIGINT' ? 130 : 143); });
    });
    try { await main(); } catch (error) { console.error(`FAIL usage ${sanitise(error.message)}`); process.exitCode = 1; }
  }
}
