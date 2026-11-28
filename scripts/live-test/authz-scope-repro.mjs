#!/usr/bin/env node
// Node 20+, built-ins only. Observations, never expected vulnerability verdicts.
import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomBytes, generateKeyPairSync, sign } from 'node:crypto';
import { appendFileSync, chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync,
  realpathSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, basename, join, resolve } from 'node:path';
import net from 'node:net';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const file = fileURLToPath(import.meta.url), scriptDir = dirname(file);
const names = ['K1', 'K2', 'K3', 'M1', 'M2', 'M3', 'U1', 'U2', 'U3', 'U4', 'U5', 'M4'];
const observations = ['key_received_other_members_content', 'unbound_key_received_other_members_content',
  'dashboard_owner_received_other_members_content', 'manager_saw_other_department_incidents',
  'heartbeat_trigger_accepted_without_binding', 'employee_control_accepted_either_call',
  'superior_raised_subordinate_budget', 'unrelated_ai_raised_budget_under_open',
  'subordinate_raised_superior_budget', 'superior_granted_subordinate_database_source',
  'superior_narrowed_subordinate_budget', 'heartbeat_trigger_accepted_with_binding'];
const children = new Set(), sockets = new Set(), secrets = new Set(), rows = new Map();
const summary = Object.fromEntries(observations.map(k => [k, null]));
const delay = ms => new Promise(r => setTimeout(r, ms));
let ctx, options, interrupted = false, cleanupPromise;
class HarnessError extends Error {}
function ensure(value, message) { if (!value) throw new Error(message); }
function password() { const p = 'Fixture-' + randomBytes(24).toString('hex') + '!'; secrets.add(p); return p; }
function credential(value) { ensure(typeof value === 'string' && value.length > 0, 'credential missing'); secrets.add(value); return value; }
function redact(value) {
  if (Array.isArray(value)) return value.map(redact);
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([k, v]) =>
    [k, /^(?:secret|password|current_password|new_password|access_token|refresh_token|token_hash|password_hash|signature)$/i.test(k)
      ? '[REDACTED]' : redact(v)]));
  if (typeof value !== 'string') return value;
  for (const s of secrets) value = value.split(s).join('[REDACTED]');
  return value.replace(/ddc_(?:member|run)_[A-Za-z0-9_-]+/g, '[REDACTED]')
    .replace(/eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/g, '[REDACTED]')
    .replace(/[A-Za-z0-9_+/=-]{40,}/g, '[REDACTED]');
}
function emit(row) { process.stdout.write(JSON.stringify(redact(row)) + '\n'); }
function verbose(message) { if (options?.verbose) process.stderr.write(String(redact(message)) + '\n'); }
function args(argv) {
  const o = { selected: new Set(), keep: false, verbose: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--bin') { ensure(argv[i + 1] && !argv[i + 1].startsWith('--'), '--bin requires a path'); o.bin = realpathSync(resolve(argv[++i])); }
    else if (arg === '--scenario') {
      const n = argv[++i]?.toUpperCase(); ensure(names.includes(n), '--scenario requires K1..K3, M1..M4 or U1..U5'); o.selected.add(n);
    } else if (arg === '--keep') o.keep = true;
    else if (arg === '--verbose') o.verbose = true;
    else throw new Error('Usage: node authz-scope-repro.mjs --bin <path> [--scenario K1] [--keep] [--verbose]');
  }
  ensure(o.bin, '--bin is required');
  if (!o.selected.size) o.selected = new Set(names);
  return o;
}
function env(extra = {}) {
  return { LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', TMPDIR: join(ctx.root, 'tmp'),
    DUDUCLAW_AUTO_UPDATE: '0', ...ctx.licenseEnv, ...extra,
    HOME: join(ctx.home, 'os-home'), DUDUCLAW_HOME: ctx.home, PATH: join(ctx.root, 'bin') + ':/usr/bin:/bin',
    HTTP_PROXY: 'http://127.0.0.1:0', HTTPS_PROXY: 'http://127.0.0.1:0',
    ALL_PROXY: 'http://127.0.0.1:0', NO_PROXY: '127.0.0.1,localhost' };
}
function helper(command, argv, input, acceptedStatuses = [0]) {
  const r = spawnSync('/usr/bin/sandbox-exec', ['-f', ctx.policy, command, ...argv],
    { env: env(), cwd: ctx.home, input, encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024 });
  if (r.error || !acceptedStatuses.includes(r.status))
    throw new HarnessError(command + ' failed: ' + redact(r.stderr || r.error?.message || String(r.status)));
  return r;
}
function sync(command, argv, input) { return helper(command, argv, input).stdout; }
function start(argv, label, extra = {}, stdin = 'ignore') {
  ensure(!interrupted, 'interrupted');
  const child = spawn('/usr/bin/sandbox-exec', ['-f', ctx.policy, options.bin, ...argv],
    { env: env(extra), cwd: ctx.home, detached: true, stdio: [stdin, 'pipe', 'pipe'] });
  children.add(child);
  child.on('error', e => { child.failure = e; });
  for (const stream of [child.stderr, ...(stdin === 'ignore' ? [child.stdout] : [])]) {
    createInterface({ input: stream }).on('line', line => {
      appendFileSync(join(ctx.root, label + '.log'), redact(line) + '\n', { mode: 0o600 });
      verbose(label + ': ' + line);
    });
  }
  return child;
}
function groupAlive(child) {
  if (!child.pid) return false;
  try { process.kill(-child.pid, 0); return true; } catch (e) { if (e.code === 'ESRCH') return false; throw e; }
}
async function stop(child) {
  if (groupAlive(child)) process.kill(-child.pid, 'SIGTERM');
  const end = Date.now() + 2500;
  while (groupAlive(child) && Date.now() < end) await delay(50);
  if (groupAlive(child)) process.kill(-child.pid, 'SIGKILL');
  const killed = Date.now() + 2500;
  while (groupAlive(child) && Date.now() < killed) await delay(50);
  ensure(!groupAlive(child), 'owned process group failed to terminate');
  children.delete(child);
}
async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  cleanupPromise = (async () => {
    for (const s of sockets) s.close();
    const results = await Promise.allSettled([...children].map(stop));
    ensure(results.every(r => r.status === 'fulfilled'), 'cleanup could not terminate every owned process group; home retained');
    if (ctx && !options?.keep) rmSync(ctx.root, { recursive: true });
  })();
  return cleanupPromise;
}
async function until(fn, label, ms = 20000) {
  const end = Date.now() + ms;
  do { ensure(!interrupted, 'interrupted'); const v = await fn(); if (v) return v; await delay(100); } while (Date.now() < end);
  throw new Error(label + ' timed out');
}

// A small RFC 6455 loopback client avoids requiring Node 22's global WebSocket.
// Supports fragmented text, masked client frames, ping/pong and bounded buffers.
class Socket {
  constructor(path) {
    this.queue = []; this.buffer = Buffer.alloc(0); this.fragments = []; this.httpStatus = null;
    this.key = randomBytes(16).toString('base64');
    this.socket = net.createConnection({ host: '127.0.0.1', port: ctx.port });
    sockets.add(this);
    this.socket.on('error', e => { this.failure = e; });
    this.socket.on('close', () => { this.closed = true; sockets.delete(this); });
    this.socket.on('connect', () => this.socket.write('GET ' + path + ' HTTP/1.1\r\nHost: 127.0.0.1:' + ctx.port
      + '\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ' + this.key + '\r\n\r\n'));
    this.socket.on('data', chunk => {
      try { this.consume(chunk); } catch (e) { this.failure = e; this.close(); }
    });
  }
  consume(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    ensure(this.buffer.length <= 8 * 1024 * 1024, 'WebSocket buffer limit exceeded');
    if (!this.upgraded) {
      const end = this.buffer.indexOf('\r\n\r\n'); if (end < 0) return;
      const header = this.buffer.subarray(0, end).toString('utf8');
      this.httpStatus = Number(header.split(' ')[1]);
      ensure(this.httpStatus === 101, 'WebSocket upgrade HTTP ' + this.httpStatus);
      const accept = createHash('sha1').update(this.key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
      ensure(header.split('\r\n').some(h => h.toLowerCase().startsWith('sec-websocket-accept:') && h.split(':')[1].trim() === accept),
        'invalid WebSocket accept');
      this.buffer = this.buffer.subarray(end + 4); this.upgraded = true;
    }
    while (this.buffer.length >= 2) {
      const b = this.buffer, opcode = b[0] & 15, fin = Boolean(b[0] & 128);
      ensure(!(b[0] & 112) && !(b[1] & 128), 'unexpected WebSocket flags');
      let length = b[1] & 127, offset = 2;
      if (length === 126) { if (b.length < 4) return; length = b.readUInt16BE(2); offset = 4; }
      if (length === 127) { if (b.length < 10) return; ensure(b.readBigUInt64BE(2) <= 8388608n, 'WebSocket frame too large'); length = Number(b.readBigUInt64BE(2)); offset = 10; }
      if (b.length < offset + length) return;
      const data = b.subarray(offset, offset + length); this.buffer = b.subarray(offset + length);
      if (opcode === 8) { this.close(); return; }
      if (opcode === 9) { this.frame(10, data); continue; }
      if (opcode === 10) continue;
      ensure(opcode === 1 || opcode === 0, 'expected WebSocket text');
      ensure(opcode === 0 ? this.fragments.length > 0 : this.fragments.length === 0, 'invalid fragmentation');
      this.fragments.push(data);
      ensure(this.fragments.reduce((n, d) => n + d.length, 0) <= 8388608, 'WebSocket message too large');
      if (fin) {
        ensure(this.queue.length < 2048, 'WebSocket queue limit exceeded');
        this.queue.push(JSON.parse(Buffer.concat(this.fragments).toString('utf8'))); this.fragments = [];
      }
    }
  }
  frame(opcode, data) {
    const mask = randomBytes(4), header = Buffer.alloc(data.length < 126 ? 2 : 4);
    header[0] = 128 | opcode; header[1] = 128 | (data.length < 126 ? data.length : 126);
    if (data.length >= 126) { ensure(data.length < 65536, 'client frame too large'); header.writeUInt16BE(data.length, 2); }
    const body = Buffer.from(data); for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
    this.socket.write(Buffer.concat([header, mask, body]));
  }
  send(value) { ensure(this.upgraded && !this.closed && !this.failure, 'WebSocket unavailable'); this.frame(1, Buffer.from(JSON.stringify(value))); }
  async open() {
    try { await until(() => { if (this.failure) throw this.failure; ensure(!this.closed, 'WebSocket closed'); return this.upgraded; }, 'WebSocket upgrade', 5000); }
    catch (e) { e.httpStatus = this.httpStatus; this.close(); throw e; }
    return this;
  }
  async take(predicate, ms = 20000) {
    return until(() => {
      const i = this.queue.findIndex(predicate); if (i >= 0) return this.queue.splice(i, 1)[0];
      if (this.failure) throw this.failure; ensure(!this.closed, 'WebSocket closed before response'); return null;
    }, 'WebSocket response', ms);
  }
  close() { this.socket.destroy(); sockets.delete(this); }
}
async function http(path, body) {
  const r = await fetch(ctx.base + path, { method: body === undefined ? 'GET' : 'POST',
    redirect: 'error', signal: AbortSignal.timeout(3000), headers: { 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const value = await r.json();
  for (const key of ['access_token', 'refresh_token']) if (value[key]) secrets.add(value[key]);
  ensure(r.ok, path + ' HTTP ' + r.status + ': ' + JSON.stringify(redact(value))); return value;
}
async function rpcClient(auth) {
  const socket = await new Socket('/ws').open(); let serial = 0;
  const raw = async (method, params = {}) => {
    const id = 'authz-' + ++serial; socket.send({ type: 'req', id, method, params });
    const r = await socket.take(f => f.type === 'res' && f.id === id);
    return { http_status: socket.httpStatus, ok: r.ok, rpc_error: r.error || null, payload: r.payload ?? null };
  };
  const call = async (method, params = {}) => {
    const r = await raw(method, params);
    ensure(r.ok, method + ': ' + JSON.stringify(redact(r.rpc_error))); return r.payload;
  };
  try { const connected = await call('connect', auth); ensure(connected.user && !connected.admin_fallback, 'account authentication required'); }
  catch (e) { socket.close(); throw e; }
  return { raw, call, close: () => socket.close() };
}
function parseSection(text, section, key) {
  const body = text.split(new RegExp('^\\[' + section + '\\]\\s*$', 'm'))[1]?.split(/^\[/m)[0];
  const match = body?.match(new RegExp('^\\s*' + key + '\\s*=\\s*', 'm'));
  if (!match && key === 'db_sources') return []; // Serde omits an empty grant list.
  ensure(match, 'missing ' + section + '.' + key + ' in effective agent.toml');
  const tail = body.slice(match.index + match[0].length);
  const raw = tail.startsWith('[') ? tail.slice(0, tail.indexOf(']') + 1) : tail.split(/[\n#]/)[0].trim();
  return JSON.parse(raw.replace(/,\s*]$/, ']'));
}
function agentState(id) {
  const text = readFileSync(join(ctx.home, 'agents', id, 'agent.toml'), 'utf8');
  return { budget: parseSection(text, 'budget', 'monthly_limit_cents'),
    db_sources: parseSection(text, 'capabilities', 'db_sources') };
}
function validateAgentConfig(text, id) {
  const required = {
    agent: ['name', 'display_name', 'role', 'status', 'trigger', 'reports_to', 'icon'],
    model: ['preferred', 'fallback', 'account_pool'], container: ['timeout_ms'],
    heartbeat: ['enabled', 'interval_seconds', 'max_concurrent_runs', 'cron'],
    budget: ['monthly_limit_cents', 'warn_threshold_percent', 'hard_stop'],
    permissions: ['can_create_agents', 'can_send_cross_agent', 'can_modify_own_skills',
      'can_modify_own_soul', 'can_schedule_tasks', 'allowed_channels'], evolution: [],
  };
  for (const [section, keys] of Object.entries(required)) {
    ensure(new RegExp('^\\[' + section + '\\]\\s*$', 'm').test(text), id + ': missing [' + section + ']');
    for (const key of keys) parseSection(text, section, key);
  }
}
function invocationRows() {
  const p = join(ctx.root, 'invocations.jsonl');
  return existsSync(p) ? readFileSync(p, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
}
function incidentRows() {
  const p = join(ctx.home, 'budget_events.jsonl');
  return existsSync(p) ? readFileSync(p, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
}
async function fakeCLI() {
  const root = process.argv[3], runtime = process.argv[4], argv = process.argv.slice(5);
  const probe = argv.includes('--help') ? '--help' : argv.includes('--version') ? '--version'
    : argv[0] === 'auth' && argv[1] === 'status' ? 'auth status' : null;
  if (probe) {
    appendFileSync(join(root, 'invocations.jsonl'), JSON.stringify({ runtime, kind: 'probe', probe,
      agent: basename(process.cwd()), at: Date.now() }) + '\n', { mode: 0o600 });
    if (probe === '--help') console.log('Local Claude fixture: --help --version auth status -p --output-format stream-json');
    else if (probe === '--version') console.log('99.0.0 (Claude Code)');
    else console.log(JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', apiProvider: 'firstParty',
      subscriptionType: 'max', email: 'fixture@acceptance.invalid' }));
    return;
  }
  const fixture = JSON.parse(readFileSync(join(root, 'fixture.json'), 'utf8'));
  let prompt = argv.join('\n');
  if (argv.includes('--no-session-persistence')) for await (const chunk of process.stdin) prompt += chunk;
  for (const flag of ['--system-prompt-file', '--append-system-prompt-file']) {
    const i = argv.indexOf(flag); if (i >= 0) prompt += readFileSync(argv[i + 1], 'utf8');
  }
  // Store no arguments, environment or prompt: these may carry credentials.
  const row = { runtime, kind: 'run', agent: basename(process.cwd()), at: Date.now(),
    user_marker: prompt.includes(fixture.userMarker), heartbeat_marker: prompt.includes(fixture.heartbeatMarker) };
  appendFileSync(join(root, 'invocations.jsonl'), JSON.stringify(row) + '\n', { mode: 0o600 });
  console.log(JSON.stringify({ type: 'result', result: row.user_marker ? fixture.replyMarker : 'HEARTBEAT_OK', is_error: false }));
}
async function setup() {
  ensure(Number(process.versions.node.split('.')[0]) >= 20, 'Node 20+ required');
  ensure(process.platform === 'darwin' && existsSync('/usr/bin/sandbox-exec'),
    'macOS sandbox-exec required to enforce no external network; other platforms are unsupported');
  ensure(existsSync('/usr/bin/sqlite3') && existsSync('/usr/sbin/lsof'), 'sqlite3 and lsof required');
  const root = realpathSync(mkdtempSync('/private/tmp/ddc-authz-')); chmodSync(root, 0o700);
  ctx = { root, home: join(root, 'home'), licenseEnv: {}, users: {} };
  for (const p of ['home', 'home/os-home', 'bin', 'tmp']) mkdirSync(join(root, p), { mode: 0o700 });
  const server = net.createServer();
  await new Promise((ok, fail) => { server.once('error', fail); server.listen({ host: '127.0.0.1', port: 0 }, ok); });
  ctx.port = server.address().port;
  await new Promise((ok, fail) => server.close(e => e ? fail(e) : ok()));
  ctx.base = 'http://127.0.0.1:' + ctx.port;
  const cliNames = ['claude', 'codex', 'grok', 'agy', 'gemini'];
  const forbidden = [join(homedir(), '.duduclaw'), join(homedir(), '.claude'), join(homedir(), 'Library/Keychains'),
    '/Library/Keychains', '/System/Library/Keychains',
    ...['/opt/homebrew/bin', '/usr/local/bin'].flatMap(d => cliNames.map(n => join(d, n)))];
  ctx.policy = join(root, 'isolation.sb');
  writeFileSync(ctx.policy, '(version 1)(allow default)\n(deny network*)\n'
    + '(allow network-bind (local ip "localhost:*"))\n(allow network-inbound (local ip "localhost:*"))\n'
    + '(allow network-outbound (remote ip "localhost:*"))\n(allow network* (subpath ' + JSON.stringify(root) + '))\n'
    + '(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*" #".*secd.*"))\n'
    + '(deny file-read* file-write* ' + forbidden.map(p => '(subpath ' + JSON.stringify(p) + ')').join(' ') + ')\n', { mode: 0o600 });
  const quote = s => "'" + s.replaceAll("'", "'\\''") + "'";
  for (const n of cliNames) writeFileSync(join(root, 'bin', n),
    '#!/bin/sh\nexec ' + [process.execPath, file, '--fake-cli', root, n].map(quote).join(' ') + ' "$@"\n', { mode: 0o700 });
  ctx.userMarker = 'U2_' + randomBytes(12).toString('hex');
  ctx.replyMarker = 'REPLY_' + randomBytes(12).toString('hex');
  ctx.heartbeatMarker = 'HB_' + randomBytes(12).toString('hex');
  writeFileSync(join(root, 'fixture.json'), JSON.stringify(ctx), { mode: 0o600 });
  // Exercise the shell -> Node fixture and each discovery probe before boot.
  // The gateway auto-registers Claude and discovers its binary through PATH;
  // the account rotator creates oauth-default from a successful auth status.
  const fakeClaude = join(root, 'bin', 'claude');
  ensure(sync(fakeClaude, ['--help']).includes('Local Claude fixture'), 'fake Claude help probe failed');
  ensure(sync(fakeClaude, ['--version']).startsWith('99.0.0'), 'fake Claude version probe failed');
  ensure(JSON.parse(sync(fakeClaude, ['auth', 'status'])).loggedIn === true, 'fake Claude auth probe failed');
  const config = '[gateway]\nbind = "127.0.0.1"\nport = ' + ctx.port + '\nauto_update = false\n'
    + '[general]\ndefault_language = "zh-TW"\nlog_level = "info"\n[auth]\nmfa_enforced = false\n'
    + '[dashboard]\nlocal_auto_login = false\n[member_api_keys]\nenabled = true\nrate_per_minute = 120\n'
    + '[people]\nbound_history_privacy = true\n[delegation]\npolicy = "hierarchy"\nallow = []\n'
    + '[dispatch]\nenabled = false\n[tick]\nenabled = false\n[proactive]\nnatural_timing = false\n'
    + '[responsibilities]\nenabled = false\n[knowledge.embedding]\nprovider = "none"\n'
    + '[runtime]\nutility_provider = "claude"\n[db_sources.fixture_db]\ndriver = "sqlite"\nurl = '
    + JSON.stringify(join(root, 'fixture.sqlite')) + '\nallowed_tables = ["fixture"]\n';
  writeFileSync(join(ctx.home, 'config.toml'), config, { mode: 0o600 });
  const template = readFileSync(join(scriptDir, 'make-home.sh'), 'utf8').split("cat > \"$TARGET/agents/prod-shaped/agent.toml\" <<'TOML'\n")[1]?.split('\nTOML')[0];
  ensure(template, 'production employee template missing');
  for (const [id, parent, dept] of [['e', '', 'shared'], ['a', '', 'x'], ['b', '', 'y'],
    ['s', '', 'x'], ['t', 's', 'x'], ['p', '', 'x'], ['q', '', 'y']]) {
    const dir = join(ctx.home, 'agents', id); mkdirSync(dir, { recursive: true, mode: 0o700 });
    const text = template.replace('name = "prod-shaped"', 'name = "' + id + '"')
      .replace('reports_to = ""', 'reports_to = "' + parent + '"\ndepartment = "' + dept + '"')
      .replace('trigger = "@prod-shaped"', 'trigger = "@' + id + '"')
      .replace('[capabilities]', '[capabilities]\ndb_sources = []')
      .replace('monthly_limit_cents = 500', 'monthly_limit_cents = ' + (id === 'b' ? 1 : 500))
      .replace('gvu_enabled = true', 'gvu_enabled = false\nenabled = false')
      + '\n[proactive]\nenabled = ' + (id === 'b') + '\nquiet_hours_start = 0\nquiet_hours_end = 0\n';
    const configPath = join(dir, 'agent.toml');
    writeFileSync(configPath, text + '\n', { mode: 0o600 });
    validateAgentConfig(readFileSync(configPath, 'utf8'), id);
    writeFileSync(join(dir, 'SOUL.md'), '# Local fixture\nUse only local tools. Never contact external services.\n', { mode: 0o600 });
    writeFileSync(join(dir, 'CONTRACT.toml'), '[boundaries]\nmust_not = ["contact external services"]\nmax_tool_calls_per_turn = 20\n', { mode: 0o600 });
    if (id === 'b') writeFileSync(join(dir, 'PROACTIVE.md'), '# Local check\n' + ctx.heartbeatMarker + '\nReply HEARTBEAT_OK.\n', { mode: 0o600 });
  }
  // Pre-MFA upgrade shape, as in v185-acceptance; all scenario users use RPCs.
  sync('/usr/bin/sqlite3', [join(ctx.home, 'users.db')], [
    'CREATE TABLE users(id TEXT PRIMARY KEY,email TEXT UNIQUE NOT NULL,display_name TEXT NOT NULL,password_hash TEXT NOT NULL,',
    "role TEXT NOT NULL DEFAULT 'employee',status TEXT NOT NULL DEFAULT 'active',created_at TEXT NOT NULL,updated_at TEXT NOT NULL,",
    'last_login TEXT,must_change_password INTEGER NOT NULL DEFAULT 0,bootstrap_owner INTEGER NOT NULL DEFAULT 0,',
    'department TEXT,manager_user_id TEXT,primary_agent_id TEXT);',
    "CREATE TABLE user_agent_bindings(user_id TEXT NOT NULL,agent_name TEXT NOT NULL,access_level TEXT NOT NULL DEFAULT 'owner',bound_at TEXT NOT NULL,PRIMARY KEY(user_id,agent_name));",
    "INSERT INTO users(id,email,display_name,password_hash,role,must_change_password,bootstrap_owner,created_at,updated_at)",
    "VALUES('fixture-admin','admin@local','Fixture Owner','!','admin',1,1,'2026-01-01','2026-01-01');",
  ].join('\n'));
  const fingerprint = sync(options.bin, ['license', 'fingerprint']).trim().split('\n')[0]; ensure(fingerprint, 'missing machine fingerprint');
  const { publicKey, privateKey } = generateKeyPairSync('ed25519'), now = Date.now();
  const timestamp = ms => new Date(ms).toISOString().replace(/\.000Z$/, 'Z');
  const payload = { version: 2, subscription_id: 'sub_authz_fixture', customer_id: 'fixture', tier: 'oem',
    machine_fingerprint: fingerprint, issued_at: timestamp(now), expires_at: timestamp(now + 86400000),
    last_phone_home: timestamp(now), public_key_id: 'authz_fixture', nfr: true };
  const signature = sign(null, Buffer.from(JSON.stringify(payload)), privateKey).toString('base64'); secrets.add(signature);
  writeFileSync(join(ctx.home, 'license.json'), JSON.stringify({ ...payload, signature }), { mode: 0o600 });
  ctx.licenseEnv = { DUDUCLAW_EDITION: 'enterprise',
    DUDUCLAW_LICENSE_PUBKEY_AUTHZ_FIXTURE: Buffer.from(publicKey.export({ format: 'jwk' }).x, 'base64url').toString('hex') };
  ctx.gateway = start(['run', '--yes'], 'gateway');
  await until(async () => {
    ensure(!ctx.gateway.failure && ctx.gateway.exitCode === null && ctx.gateway.signalCode === null, 'gateway exited before readiness');
    const owner = helper('/usr/sbin/lsof', ['-nP', '-iTCP@127.0.0.1:' + ctx.port, '-sTCP:LISTEN', '-t'], undefined, [0, 1]);
    if (owner.status === 1) return false;
    ensure(owner.stdout.trim() === String(ctx.gateway.pid), 'port belongs to another process; refusing contact');
    try { return (await http('/healthz')).ok; } catch { return false; }
  }, 'gateway readiness', 60000);
  const ownerPassword = password();
  await http('/api/first-run/claim', { email: 'owner@acceptance.invalid', password: ownerPassword });
  const login = await http('/api/login', { email: 'owner@acceptance.invalid', password: ownerPassword });
  ctx.admin = await rpcClient({ jwt: credential(login.access_token) });
  ensure((await ctx.admin.call('agents.list')).agents?.some(a => a.name === 'e'), 'isolated employee canary missing');
  ensure((await ctx.admin.call('system.status')).edition_profile === 'enterprise', 'fixture Enterprise entitlement unavailable');
}

async function account(name, role, department, bindings = []) {
  const pw = password(), email = name.toLowerCase() + '@acceptance.invalid';
  const created = await ctx.admin.call('users.create', { email, display_name: name, password: pw, role, department });
  ensure(created.user?.id, 'users.create missing id');
  for (const [agent_name, access_level] of bindings)
    await ctx.admin.call('users.bind_agent', { user_id: created.user.id, agent_name, access_level });
  const login = await http('/api/login', { email, password: pw });
  const client = await rpcClient({ jwt: credential(login.access_token) });
  const me = await client.call('users.me');
  ensure(me.user?.id === created.user.id && me.user.role === role && Array.isArray(me.bindings), 'live account identity/role unavailable');
  ensure(me.bindings.length === bindings.length && bindings.every(([a, l]) =>
    me.bindings.some(b => b.agent_name === a && b.access_level === l)), 'unexpected live bindings');
  return { id: created.user.id, pw, jwt: login.access_token, client, me };
}
async function personalKey(user) {
  const issued = await user.client.call('me.member_keys.create', { name: 'Authz fixture', expires_in_days: 1, password: user.pw });
  const key = credential(issued.secret); ensure(key.startsWith('ddc_member_'), 'unexpected member key prefix');
  return key;
}
async function sendChat(user, agent, marker, conv) {
  const s = await new Socket('/ws/chat').open();
  try {
    s.send({ type: 'auth', token: user.jwt });
    const hello = await s.take(f => ['session_info', 'error'].includes(f.type));
    ensure(hello.type === 'session_info' && hello.session_id, 'WebChat authentication refused');
    s.send({ type: 'user_message', content: marker, agent, session_id: hello.session_id, conv });
    return { socket: s, session: hello.session_id + '#agent:' + agent + '#conv:' + conv };
  } catch (e) { s.close(); throw e; }
}
async function keyFixture() {
  if (ctx.k) return ctx.k;
  const u1 = await account('U1', 'employee', 'shared', [['e', 'owner']]);
  const u2 = await account('U2', 'employee', 'shared', [['e', 'operator']]);
  const u3 = await account('U3', 'employee', 'other');
  const before = invocationRows().length;
  const chat = await sendChat(u2, 'e', ctx.userMarker, 'authz-' + randomBytes(8).toString('hex'));
  try {
    await until(async () => {
      const r = await ctx.admin.raw('chat.sessions.history', { session_id: chat.session, limit: 100 });
      const text = JSON.stringify(r.payload);
      return r.ok && text.includes(ctx.userMarker) && text.includes(ctx.replyMarker);
    }, 'U2 conversation persistence with both markers', 30000);
  } finally { chat.socket.close(); }
  const runs = invocationRows().slice(before).filter(r => r.kind === 'run' && r.agent === 'e' && r.user_marker);
  ensure(runs.length > 0, 'U2 fixture reply has no matching fake CLI run');
  ctx.k = { u1, u2, u3, session: chat.session, invocation_count: runs.length }; return ctx.k;
}
async function kScenario(n, row) {
  const k = await keyFixture(), user = n === 'K2' ? k.u3 : k.u1;
  const key = n === 'K3' ? null : await personalKey(user);
  const client = key ? await rpcClient({ member_key: key }) : user.client;
  row.observed.credential = key ? { prefix: 'ddc_member_', length: key.length } : { prefix: 'eyJ', length: user.jwt.length };
  try {
    row.call = [{ method: 'chat.sessions.list', params: { agent_id: 'e', limit: 100 } },
      { method: 'chat.sessions.history', params: { session_id: k.session, limit: 100 } }];
    const list = await client.raw(row.call[0].method, row.call[0].params);
    const history = await client.raw(row.call[1].method, row.call[1].params);
    const text = JSON.stringify(history.payload), listed = list.payload?.sessions || [];
    row.http_status = { list: list.http_status, history: history.http_status };
    row.rpc_error = { list: list.rpc_error, history: history.rpc_error };
    Object.assign(row.observed, { list_ok: list.ok, history_ok: history.ok, session_id: k.session,
      u2_session_listed: listed.some(s => s.session_id === k.session), sessions_returned: listed.length,
      history_contains_u2_marker: text.includes(ctx.userMarker), history_contains_reply_marker: text.includes(ctx.replyMarker),
      u2_marker: ctx.userMarker, reply_marker: ctx.replyMarker, fixture_run_invocation_count: k.invocation_count });
    summary[observations[names.indexOf(n)]] = text.includes(ctx.userMarker) && text.includes(ctx.replyMarker);
    row.notes = 'Shared employee E; U1 Owner, U2 Operator, U3 unbound. HTTP status is the actual WebSocket upgrade (101); RPC success is separate.';
  } finally { if (key) client.close(); }
}
async function managerFixture() {
  if (!ctx.m) ctx.m = {
    manager: await account('M', 'manager', 'x', [['a', 'owner']]),
    employee: await account('EmployeeControl', 'employee', 'x', [['a', 'owner']]),
  };
  return ctx.m;
}
async function heartbeat(user) {
  const before = invocationRows().length, started = Date.now();
  const response = await user.client.raw('heartbeat.trigger', { agent_id: 'b' });
  const accepted = response.ok && response.payload?.success === true;
  // Only B's unique PROACTIVE.md marker counts as invocation by this trigger.
  const matching = () => invocationRows().slice(before).filter(r => r.kind === 'run' && r.agent === 'b' && r.heartbeat_marker && r.at >= started);
  if (accepted) {
    const end = Date.now() + 10000;
    while (!matching().length && Date.now() < end && !interrupted) await delay(100);
  } else await delay(500);
  return { response, accepted, invoked: matching().length > 0, invocation_count: matching().length, observation_window_ms: Date.now() - started };
}
async function seedIncident() {
  if (ctx.incidentSeed) return ctx.incidentSeed;
  const seed = { generated: false, reason: null, method: 'synthetic cost row + Admin heartbeat.trigger; gateway writes incident' };
  ctx.incidentSeed = seed;
  try {
    // Mirrors budget.rs::budget_stored_cents_499_allows_500_refuses:
    // telemetry is synthetic input, but the budget event MUST come from gateway.
    sync('/usr/bin/sqlite3', ['-cmd', '.timeout 2000', join(ctx.home, 'cost_telemetry.db')],
      "INSERT INTO token_usage(agent_id,request_type,model,cost_millicents,created_at) VALUES"
      + "('b','authz-synthetic-cost','fixture',100,'" + new Date().toISOString() + "');");
    const r = await ctx.admin.raw('heartbeat.trigger', { agent_id: 'b' });
    seed.trigger = r; ensure(r.ok && r.payload?.success, 'Admin incident-seeding heartbeat refused');
    await until(() => incidentRows().some(e => e.agent_id === 'b' && e.event === 'budget_breaker_open'), 'gateway-produced B budget incident', 10000);
    seed.generated = true;
  } catch (e) { if (e instanceof HarnessError) throw e; seed.reason = e.message; }
  return seed;
}
async function mScenario(n, row) {
  const m = await managerFixture(), user = n === 'M3' ? m.employee : m.manager;
  if (n === 'M4') {
    row.call = { method: 'heartbeat.trigger', params: { agent_id: 'a' } };
    const r = await user.client.raw(row.call.method, row.call.params);
    row.http_status = r.http_status; row.rpc_error = r.rpc_error;
    const accepted = r.ok && r.payload?.success === true;
    Object.assign(row.observed, { accepted, response: r.payload, actor_has_binding_to_a: true });
    summary.heartbeat_trigger_accepted_with_binding = accepted;
    row.notes = 'M is Manager in X with an Owner binding to A; records actual heartbeat acceptance.';
    return;
  }
  if (n === 'M2') {
    row.call = { method: 'heartbeat.trigger', params: { agent_id: 'b' } };
    const h = await heartbeat(user);
    row.http_status = h.response.http_status; row.rpc_error = h.response.rpc_error;
    Object.assign(row.observed, { accepted: h.accepted, fake_cli_invoked_for_b: h.invoked, response: h.response.payload,
      invocation_count: h.invocation_count, observation_window_ms: h.observation_window_ms, actor_has_binding_to_b: false });
    summary.heartbeat_trigger_accepted_without_binding = h.accepted;
    row.notes = 'M is Manager in X, bound only to A; B is in Y. B heartbeat timer disabled; local proactive check enabled with no quiet hours or natural-timing deferral.';
    return;
  }
  const seed = await seedIncident();
  row.call = [{ method: 'budget.incidents', params: { limit: 50 } }];
  row.observed.incident_seed = seed;
  if (n === 'M1' && !seed.generated) {
    row.status = 'skipped'; row.notes = 'No gateway-produced B incident: ' + seed.reason; return;
  }
  const r = await user.client.raw('budget.incidents', { limit: 50 });
  row.http_status = { incidents: r.http_status }; row.rpc_error = { incidents: r.rpc_error };
  const incidents = r.payload?.incidents || [], byAgent = r.payload?.by_agent || [];
  Object.assign(row.observed, { incidents_ok: r.ok, incidents, by_agent: byAgent,
    rows_naming_b_returned: [...incidents, ...byAgent].some(e => e.agent_id === 'b'), actor_has_binding_to_b: false });
  if (n === 'M1') summary.manager_saw_other_department_incidents = row.observed.rows_naming_b_returned;
  else {
    row.call.push({ method: 'heartbeat.trigger', params: { agent_id: 'b' } });
    const h = await heartbeat(user);
    row.http_status.heartbeat = h.response.http_status; row.rpc_error.heartbeat = h.response.rpc_error;
    Object.assign(row.observed, { heartbeat_accepted: h.accepted, fake_cli_invoked_for_b: h.invoked,
      heartbeat_response: h.response.payload, heartbeat_observation_window_ms: h.observation_window_ms });
    summary.employee_control_accepted_either_call = r.ok || h.accepted;
  }
  row.notes = 'B budget is 1 cent; synthetic stored spend is 100 cents (historical cost_millicents column). Incident is generated by the real budget gate, never fabricated. M3 heartbeat occurs after seeding and may be budget-blocked even if accepted.';
}
async function mcpClient(actor) {
  const registration = JSON.parse(readFileSync(join(ctx.home, 'agents', actor, '.mcp.json'), 'utf8'));
  const entry = registration.mcpServers?.duduclaw || registration.mcpServers?.['duduclaw-pro'];
  ensure(entry?.env?.DUDUCLAW_AGENT_ID === actor && entry.env.DUDUCLAW_AGENT_TOKEN, 'employee MCP identity registration missing');
  // Only internal authentication/identity fields cross from the registration.
  const extra = Object.fromEntries(['DUDUCLAW_AGENT_ID', 'DUDUCLAW_AGENT_TOKEN', 'DUDUCLAW_MCP_API_KEY',
    'DUDUCLAW_PORT', 'DUDUCLAW_INSTANCE'].filter(k => entry.env[k]).map(k => [k, entry.env[k]]));
  for (const [k, v] of Object.entries(extra)) if (k !== 'DUDUCLAW_AGENT_ID') secrets.add(v);
  ensure((entry.args || ['mcp-server'])[0] === 'mcp-server', 'unexpected MCP registration arguments');
  const child = start(entry.args || ['mcp-server'], 'mcp-' + actor, extra, 'pipe'), pending = new Map(); let serial = 0;
  const fail = () => { for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error('MCP exited')); } pending.clear(); };
  child.on('close', fail); child.on('error', fail); child.stdin.on('error', fail);
  createInterface({ input: child.stdout }).on('line', line => {
    let r; try { r = JSON.parse(line); } catch { return; }
    const p = pending.get(r.id); if (!p) return; pending.delete(r.id); clearTimeout(p.timer); p.resolve(r);
  });
  const rpc = (method, params) => new Promise((resolve, reject) => {
    const id = ++serial, timer = setTimeout(() => { pending.delete(id); reject(new Error('MCP ' + method + ' timed out')); }, 15000);
    pending.set(id, { resolve, reject, timer }); child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
  try {
    const init = await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'authz-scope-repro', version: '1' } });
    ensure(!init.error, 'MCP initialize refused: ' + JSON.stringify(init.error));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }) + '\n');
    const list = await rpc('tools/list', {}); ensure(!list.error, 'MCP tools/list refused');
    return { rpc, tools: list.result?.tools || [], close: () => stop(child) };
  } catch (e) { await stop(child); throw e; }
}
async function uScenario(n, row) {
  const policy = n === 'U2' ? 'open' : 'hierarchy', actor = n === 'U2' ? 'p' : n === 'U3' ? 't' : 's';
  const target = n === 'U2' ? 'q' : n === 'U3' ? 's' : 't';
  const delegation = await ctx.admin.call('delegation.get', {});
  ensure(typeof delegation.hash === 'string', 'delegation.get hash missing');
  await ctx.admin.call('delegation.set', { policy, allow: [], expected_hash: delegation.hash });
  const before = agentState(target);
  if (n === 'U5' && (!Number.isSafeInteger(before.budget) || before.budget <= 1)) {
    row.status = 'skipped'; row.observed.before = before;
    row.notes = 'No positive cap below the current budget is available (0 means unlimited).'; return;
  }
  const params = { agent_id: target, ...(n === 'U4' ? { db_sources_add: 'fixture_db' }
    : { budget_cents: n === 'U5' ? before.budget - 1 : before.budget + 1000 }) };
  row.call = { method: 'tools/call', params: { name: 'agent_update', arguments: params } };
  Object.assign(row.observed, { policy, target, before });
  const client = await mcpClient(actor);
  try {
    const tool = client.tools.find(t => t.name === 'agent_update');
    row.observed.tool_listed = Boolean(tool);
    if (n === 'U4' && !tool?.inputSchema?.properties?.db_sources_add) {
      row.status = 'skipped'; row.notes = 'agent_update does not advertise db_sources_add on this binary'; return;
    }
    const r = await client.rpc('tools/call', row.call.params), after = agentState(target);
    row.rpc_error = r.error || null;
    Object.assign(row.observed, { tool_result: r.result ?? null, after, budget_increased: after.budget > before.budget,
      db_source_added: !before.db_sources.includes('fixture_db') && after.db_sources.includes('fixture_db') });
    if (n === 'U5') {
      Object.assign(row.observed, { budget_decreased: after.budget > 0 && after.budget < before.budget,
        update_succeeded: !r.error && Boolean(r.result) && r.result.isError !== true && after.budget === params.budget_cents });
      summary.superior_narrowed_subordinate_budget = row.observed.update_succeeded;
    } else summary[observations[names.indexOf(n)]] = n === 'U4' ? row.observed.db_source_added : row.observed.budget_increased;
    row.notes = 'Real employee registration and identity token; --bin overrides registered executable. Effective agent.toml read before and after; MCP stdio has no HTTP status.';
  } finally { await client.close(); }
}
async function run(n) {
  const row = { scenario: n, status: 'ran', actor: { K1: 'U1 member key', K2: 'U3 member key', K3: 'U1 dashboard JWT',
    M1: 'M Manager', M2: 'M Manager', M3: 'EmployeeControl', U1: 'S', U2: 'P', U3: 'T', U4: 'S', U5: 'S', M4: 'M Manager' }[n],
    call: n[0] === 'K' ? ['chat.sessions.list', 'chat.sessions.history'] : n[0] === 'M' ? ['budget.incidents', 'heartbeat.trigger'] : 'agent_update',
    http_status: null, rpc_error: null, observed: {}, notes: '' };
  try {
    if (n[0] === 'K') await kScenario(n, row);
    else if (n[0] === 'M') await mScenario(n, row);
    else await uScenario(n, row);
  } catch (e) {
    if (e instanceof HarnessError) throw e;
    row.status = 'error'; row.http_status ??= e.httpStatus ?? null; row.notes = e.message;
  }
  if (row.status !== 'ran') summary[observations[names.indexOf(n)]] = null;
  rows.set(n, row);
}
async function main() {
  try { options = args(process.argv.slice(2)); }
  catch (e) { emit({ status: 'harness_error', harness_error: e.message }); process.exitCode = 2; return; }
  for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => {
    interrupted = true;
    // Terminate promptly even if a transport read is waiting.
    void cleanup().catch(e => verbose(e.message));
  });
  let failure = null, cleanupError = null;
  try {
    await setup();
    // Trigger B before adding spend, so the CLI observation is not budget-blocked.
    for (const n of ['K1', 'K2', 'K3', 'M2', 'M1', 'M3', 'U1', 'U2', 'U3', 'U4', 'U5', 'M4'])
      if (options.selected.has(n)) { ensure(!interrupted, 'interrupted'); await run(n); }
  } catch (e) { failure = e.message; }
  finally { try { await cleanup(); } catch (e) { cleanupError = e.message; } }
  if (failure) emit({ status: 'harness_error', harness_error: failure });
  for (const n of names.filter(n => options.selected.has(n))) {
    // A shared setup/helper failure is one harness error, not ten observations.
    if (rows.has(n)) emit(rows.get(n));
  }
  emit({ summary, scenario_status: Object.fromEntries(names.map(n => [n,
    rows.get(n)?.status ?? (options.selected.has(n) ? 'not_run' : 'not_selected')])),
    notes: 'Null means not observed. Boolean values describe completed observations only; consult scenario_status and per-call evidence.',
    cleanup_error: cleanupError, ...(options.keep || cleanupError ? { retained_home: ctx?.root ?? null } : {}) });
  process.exitCode = failure ? 2 : interrupted ? 130 : cleanupError || [...rows.values()].some(r => r.status === 'error') ? 1 : 0;
}
if (process.argv[2] === '--fake-cli') await fakeCLI();
else await main();
