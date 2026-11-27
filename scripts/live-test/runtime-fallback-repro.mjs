#!/usr/bin/env node
// Node 20+, no npm dependencies. Run from an operator shell on macOS.
import { spawn, spawnSync } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import { appendFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync,
  realpathSync, rmSync, writeFileSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const file = fileURLToPath(import.meta.url), scriptDir = dirname(file);
const names = ['A-chat', 'A-chat-exec-error', 'A-dispatch', 'C-fallback-model', 'control-codex-to-grok'];
const markers = ['Explicit runtime fallback', 'all providers failed', 'model_not_found'];
const delay = ms => new Promise(done => setTimeout(done, ms));
const quote = s => `'${s.replaceAll("'", "'\\''")}'`;
const check = (ok, message) => { if (!ok) throw new Error(message); };
const read = path => existsSync(path) ? readFileSync(path, 'utf8') : '';
const modelSection = path => read(path)
  .match(/^\[model\][\s\S]*?(?=^\[|$(?![\s\S]))/m)?.[0]?.trim() ?? null;
let active, interrupted = false;

function invocationKind(runtime, args) {
  if (args.some(a => ['--help', '-h', '--version', '-V'].includes(a)) ||
      ['auth', 'login', 'status', 'help', 'version'].includes(args[0])) return 'probe';
  const run = runtime === 'codex' ? args[0] === 'exec'
    : args.some(a => (runtime === 'grok' ? ['-p', '--single'] : ['-p', '--print']).includes(a));
  return run ? 'run' : 'probe';
}
function runModel(args) {
  let model = null, source = null;
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--model' || arg === '-m') { model = args[++i]; source = arg; }
    else if (/^(--model|-m)=/.test(arg)) { model = arg.slice(arg.indexOf('=') + 1); source = 'model='; }
    else if (arg === '-c' || arg === '--config' || arg.startsWith('--config=')) {
      const value = arg.startsWith('--config=') ? arg.slice(9) : args[++i];
      const match = value?.match(/^model\s*=\s*(.+)$/);
      if (match) {
        model = match[1].trim(); source = 'config.model';
        if (model.startsWith('"')) model = JSON.parse(model);
        else if (model.startsWith("'")) model = model.slice(1, -1);
      }
    }
  }
  // These three gateway spawn paths pass the model in argv, not an env var.
  return { model: typeof model === 'string' && model.length ? model : null, source };
}

// The shim has baked-in paths: the spawn allowlist need not forward test env vars.
async function fakeCLI(runtime, root, scenario, args) {
  const home = join(root, 'home');
  const kind = invocationKind(runtime, args), probe = kind === 'probe';
  const utility = runtime === 'claude' && args.includes('--no-session-persistence');
  const { model, source } = probe ? { model: null, source: null } : runModel(args);
  const inheritedHome = process.env.DUDUCLAW_HOME ?? null;
  // Availability and utility probes scrub DUDUCLAW_HOME. Restore only this
  // fixture's baked-in home; never accept an inherited home outside it.
  check(process.env.HOME?.startsWith(root + '/') &&
    (inheritedHome === null || inheritedHome === home), 'fake CLI received a non-isolated home');
  process.env.DUDUCLAW_HOME = home;
  const modelEnv = Object.fromEntries(Object.entries(process.env).filter(([k]) =>
    /MODEL|THINKING|EFFORT/.test(k) && !/KEY|TOKEN|SECRET/.test(k)));
  appendFileSync(join(root, 'invocations.jsonl'), JSON.stringify({ runtime, argv: args,
    kind, model_arg: model, model_source: source, inherited_duduclaw_home: inheritedHome,
    env: { ...modelEnv, HOME: process.env.HOME, DUDUCLAW_HOME: process.env.DUDUCLAW_HOME },
    cwd: process.cwd(), timestamp: new Date().toISOString(), probe, utility }) + '\n', { mode: 0o600 });
  if (!['claude', 'codex', 'grok'].includes(runtime)) {
    console.error(`fixture disabled runtime: ${runtime}`); process.exitCode = 1; return;
  }
  if (args.includes('--version') || args.includes('-V') || args[0] === 'version') {
    console.log(runtime === 'codex' ? 'codex-cli 0.200.0' : '99.0.0'); return;
  }
  if (probe) {
    if (args.includes('--help') || args.includes('-h')) console.log(`fixture ${runtime}: --model <MODEL>`);
    else console.log(JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', email: 'fixture@local.invalid' }));
    return;
  }
  if (utility) {
    process.stdin.resume();
    console.log(JSON.stringify({ type: 'result', result: 'No task.', is_error: false })); return;
  }
  check(model, `fixture could not extract ${runtime} RUN model from argv`);
  if (runtime === 'claude') {
    if (scenario === 'A-chat-exec-error') console.error('fixture execution failed: child process exited unexpectedly');
    else {
      // Parser-compatible synthetic stream, not a captured vendor transcript.
      // cli_env.rs rejects assistant.error before reading result, so leave it
      // absent to exercise work_outcome::result_error's terminal stream path.
      const session = '00000000-0000-4000-8000-000000000001';
      console.log(JSON.stringify({ type: 'system', subtype: 'init', cwd: process.cwd(),
        session_id: session, tools: [], mcp_servers: [], model, permissionMode: 'default',
        apiKeySource: 'none', uuid: '00000000-0000-4000-8000-000000000002' }));
      const message = 'API Error: 404 ' + JSON.stringify({ type: 'error', error: {
        type: 'not_found_error', code: 'model_not_found', message: `model: ${model}` } });
      console.log(JSON.stringify({ type: 'assistant', session_id: session, parent_tool_use_id: null,
        uuid: '00000000-0000-4000-8000-000000000003', message: { id: 'msg_fixture', type: 'message',
          role: 'assistant', model, content: [{ type: 'text', text: message }],
          stop_reason: 'end_turn', stop_sequence: null, usage: { input_tokens: 0, output_tokens: 0 } } }));
      console.log(JSON.stringify({ type: 'result', subtype: 'error_during_execution',
        session_id: session, uuid: '00000000-0000-4000-8000-000000000004',
        duration_ms: 1, duration_api_ms: 0, num_turns: 1, total_cost_usd: 0,
        is_error: true, result: message, errors: [message], usage: { input_tokens: 0, output_tokens: 0 } }));
    }
    process.exitCode = 1; return;
  }
  if (runtime === 'codex') {
    process.stdin.resume();
    // Fail every primary Codex attempt, including a singular model fallback;
    // success is reserved for Codex acting as the A scenarios' fallback runtime.
    if (!scenario.startsWith('A-')) { console.error('fixture codex execution failed'); process.exitCode = 1; return; }
    console.log(JSON.stringify({ type: 'item.completed', item: { type: 'agent_message', text: 'FALLBACK_OK' } }));
    console.log(JSON.stringify({ type: 'turn.completed', usage: { input_tokens: 0, output_tokens: 0 } }));
    return;
  }
  if (runtime === 'grok') { console.log('FALLBACK_OK'); return; }
  console.error(`fixture disabled runtime: ${runtime}`); process.exitCode = 1;
}

// Minimal RFC6455 client so Node 20 does not require a WebSocket dependency.
// HTTP performs the upgrade; server text fragments and ping frames are supported.
class Socket {
  constructor(port, path) {
    this.queue = []; this.buffer = Buffer.alloc(0); this.fragments = [];
    this.ready = new Promise((done, fail) => {
      const key = randomBytes(16).toString('base64');
      const request = http.request({ hostname: '127.0.0.1', port, path,
        headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': '13' } });
      this.request = request;
      request.setTimeout(5000, () => request.destroy(new Error('WebSocket upgrade timeout')));
      request.on('error', fail);
      request.on('response', response => { response.resume(); fail(new Error(`WebSocket HTTP ${response.statusCode}`)); });
      request.on('upgrade', (response, socket, head) => {
        const expected = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
        if (response.headers['sec-websocket-accept'] !== expected) { socket.destroy(); fail(new Error('Invalid WebSocket upgrade')); return; }
        socket.setTimeout(0); this.socket = socket;
        socket.on('error', error => { this.error = error; });
        socket.on('close', () => { this.closed = true; });
        socket.on('data', data => this.consume(data));
        if (head.length) this.consume(head);
        done();
      });
      request.end();
    });
  }
  consume(data) {
    try {
      this.buffer = Buffer.concat([this.buffer, data]);
      while (this.buffer.length >= 2) {
        const b = this.buffer, opcode = b[0] & 15, final = Boolean(b[0] & 128);
        let length = b[1] & 127, offset = 2;
        check(!(b[1] & 128), 'Masked server WebSocket frame');
        if (length === 126) { if (b.length < 4) return; length = b.readUInt16BE(2); offset = 4; }
        if (length === 127) { if (b.length < 10) return; check(b.readBigUInt64BE(2) <= 8388608n, 'Oversized WebSocket frame'); length = Number(b.readBigUInt64BE(2)); offset = 10; }
        if (b.length < offset + length) return;
        const payload = b.subarray(offset, offset + length); this.buffer = b.subarray(offset + length);
        if (opcode === 8) { this.closed = true; this.close(); return; }
        if (opcode === 9) { this.frame(10, payload); continue; }
        if (opcode === 10) continue;
        check(opcode === 0 || opcode === 1, 'Unexpected WebSocket opcode');
        this.fragments.push(payload);
        if (final) { this.queue.push(JSON.parse(Buffer.concat(this.fragments).toString('utf8'))); this.fragments = []; }
      }
    } catch (error) { this.error = error; this.close(); }
  }
  frame(opcode, payload) {
    const mask = randomBytes(4), size = payload.length;
    check(size < 65536, 'Client WebSocket frame too large');
    const header = Buffer.alloc(size < 126 ? 2 : 4);
    header[0] = 128 | opcode; header[1] = 128 | (size < 126 ? size : 126);
    if (size >= 126) header.writeUInt16BE(size, 2);
    const body = Buffer.from(payload); for (let i = 0; i < size; i++) body[i] ^= mask[i % 4];
    this.socket.write(Buffer.concat([header, mask, body]));
  }
  send(value) { this.frame(1, Buffer.from(JSON.stringify(value))); }
  async take(predicate, ms = 15000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      const index = this.queue.findIndex(predicate);
      if (index >= 0) return this.queue.splice(index, 1)[0];
      if (this.error) throw this.error;
      check(!interrupted && !this.closed, 'WebSocket closed/interrupted before terminal response');
      check(active?.gateway?.exitCode === null, 'Gateway exited before terminal response');
      await delay(30);
    }
    throw new Error('Timed out waiting for terminal response (not an observed runtime failure)');
  }
  close() { this.socket?.destroy(); this.request.destroy(); }
}

function envFor(c, home = c.home) {
  return { HOME: join(home, 'os-home'), DUDUCLAW_HOME: home,
    PATH: `${join(c.root, 'bin')}:/usr/bin:/bin`, TMPDIR: join(c.root, 'tmp'),
    LANG: 'en_US.UTF-8', LC_ALL: 'en_US.UTF-8', RUST_LOG: 'info',
    HTTP_PROXY: 'http://127.0.0.1:0', HTTPS_PROXY: 'http://127.0.0.1:0', ALL_PROXY: 'http://127.0.0.1:0',
    NO_PROXY: '127.0.0.1,localhost', DUDUCLAW_AUTO_UPDATE: '0' };
}
async function freePort() {
  const server = net.createServer();
  return new Promise((done, fail) => {
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => { const port = server.address().port; server.close(error => error ? fail(error) : done(port)); });
  });
}
async function request(c, path, body) {
  const response = await fetch(`http://127.0.0.1:${c.port}${path}`, { redirect: 'error',
    signal: AbortSignal.timeout(3000), method: body === undefined ? 'GET' : 'POST',
    headers: { 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  check(response.ok, `${path}: HTTP ${response.status}: ${await response.clone().text()}`);
  return response.json();
}
async function rpc(c, method, params = {}) {
  const id = randomBytes(8).toString('hex');
  c.admin.send({ type: 'req', id, method, params });
  const response = await c.admin.take(f => f.type === 'res' && f.id === id);
  check(response.ok, `${method}: ${JSON.stringify(response.error)}`);
  return response.payload;
}
function groupAlive(pid) {
  if (!pid) return false;
  try { process.kill(-pid, 0); return true; } catch (error) { if (error.code === 'ESRCH') return false; throw error; }
}
function logDiagnostics(log) {
  const diagnostics = [], errors = [];
  for (const line of log.split('\n')) {
    const clean = line.replace(/\x1b\[[0-9;]*m/g, '');
    const marker = 'Reply runtime unavailable diagnostic=';
    const at = clean.indexOf(marker);
    if (at < 0) continue;
    // Tracing can append fields after the JSON. Find its balanced object,
    // respecting braces and escaped quotes inside JSON strings.
    const value = clean.slice(at + marker.length).trimStart();
    let depth = 0, quoted = false, escaped = false, end = -1;
    for (let i = 0; i < value.length; i++) {
      const char = value[i];
      if (quoted) {
        if (escaped) escaped = false;
        else if (char === '\\') escaped = true;
        else if (char === '"') quoted = false;
      } else if (char === '"') quoted = true;
      else if (char === '{') depth++;
      else if (char === '}' && --depth === 0) { end = i + 1; break; }
    }
    try {
      check(value.startsWith('{') && end > 0, 'missing diagnostic JSON object');
      diagnostics.push(JSON.parse(value.slice(0, end)));
    } catch (error) { errors.push({ line, error: error.message }); }
  }
  return { diagnostics, errors };
}
function modelFallbackValues(section) {
  // agents.update serializes model IDs as TOML basic strings / string arrays.
  const string = '"(?:[^"\\\\]|\\\\.)*"';
  const fallback = section?.match(new RegExp(`^fallback\\s*=\\s*(${string})`, 'm'));
  const fallbacks = section?.match(/^fallbacks\s*=\s*\[([\s\S]*?)\]/m);
  return { fallback: fallback ? JSON.parse(fallback[1]) : null,
    fallbacks: fallbacks ? [...fallbacks[1].matchAll(new RegExp(string, 'g'))].map(m => JSON.parse(m[0])) : null };
}
async function cleanup(c, keep, row) {
  for (const socket of c.sockets) socket.close();
  if (c.gateway?.pid) {
    const pid = c.gateway.pid;
    if (groupAlive(pid)) process.kill(-pid, 'SIGTERM');
    const end = Date.now() + 3000;
    while (groupAlive(pid) && Date.now() < end) await delay(50);
    if (groupAlive(pid)) process.kill(-pid, 'SIGKILL');
    const killed = Date.now() + 3000;
    while (groupAlive(pid) && Date.now() < killed) await delay(50);
    check(!groupAlive(pid), 'Gateway process group did not terminate; retained temp home');
    await Promise.race([c.closed, delay(3000).then(() => {
      throw new Error('Gateway stdio did not close; retained temp home');
    })]);
  }
  // Snapshot evidence after process shutdown and before removing the home.
  const records = read(join(c.root, 'invocations.jsonl')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
  const calls = records.slice(c.startIndex ?? records.length).filter(r => r.kind === 'run' && !r.utility);
  const primary = calls.filter(r => r.runtime === row.primary_runtime);
  const fallback = calls.filter(r => r.runtime === row.fallback_runtime);
  row.primary_invocations = primary.length;
  row.fallback_invocations = fallback.length;
  // Probe totals include startup; RUN totals cover only the ordinary turn.
  row.primary_probe_invocations = records.filter(r => r.runtime === row.primary_runtime && r.kind === 'probe').length;
  row.fallback_probe_invocations = records.filter(r => r.runtime === row.fallback_runtime && r.kind === 'probe').length;
  row.primary_models_tried = primary.map(r => r.model_arg).filter(Boolean);
  row.fallback_models_tried = fallback.map(r => r.model_arg).filter(Boolean);
  row.model_extraction_errors = calls.filter(r => !r.model_arg).map(r => ({ runtime: r.runtime, argv: r.argv }));
  row.fallback_model_arg = fallback.length ? fallback.at(-1).model_arg : null;
  row.fallback_attempted = fallback.length > 0;
  const log = read(c.log || '');
  const parsed = logDiagnostics(log);
  row.diagnostics = parsed.diagnostics;
  row.diagnostic_parse_errors = parsed.errors;
  row.fallback_step_recorded = row.diagnostics.some(d => d.steps?.some(s => s.role === 'explicit_fallback'));
  row.explicit_fallback_lines = log.split('\n').filter(line => line.includes('Explicit runtime fallback'));
  row.explicit_fallback_logged = row.explicit_fallback_lines.length > 0;
  row.gateway_log_markers = markers.filter(marker => log.includes(marker));
  if (c.agentPath) row.agent_toml_model_section = modelSection(c.agentPath);
  if (row.scenario === 'C-fallback-model') {
    const values = modelFallbackValues(row.agent_toml_model_section);
    row.model_fallback_written = values.fallback;
    row.model_fallbacks_written = values.fallbacks;
    row.grok_run_models_received = row.fallback_models_tried;
  }
  if (row.error?.includes('Fixture agent missing')) {
    const diagnostics = read(c.log || '').split('\n')
      .filter(line => /failed to parse agent\.toml|failed to load agent/.test(line)).slice(-10);
    row.error += `; gateway registry log: ${diagnostics.join('\n') || '(no matching registry log lines)'}`;
  }
  if (row.scenario === 'control-codex-to-grok') {
    // Primary retries may finish/log after the first fallback. The control
    // needs one failed primary RUN followed by a fallback RUN, not a 1:1 count.
    row.control_run_order_valid = primary.some(failed =>
      fallback.some(run => calls.indexOf(run) > calls.indexOf(failed)));
    if (!row.control_run_order_valid) {
      row.status = 'harness_invalid';
      row.error = `Control requires a Grok RUN after the failing Codex RUN; primary=${primary.length}, fallback=${fallback.length}; ${row.error || ''}`;
    }
  }
  if (keep) console.error(`Kept scenario home/logs: ${c.root}`);
  else rmSync(c.root, { recursive: true });
}

async function setup(c, binary, scenario, row, verbose) {
  c.port = await freePort();
  for (const path of ['bin', 'tmp', 'setup-home/os-home']) mkdirSync(join(c.root, path), { recursive: true, mode: 0o700 });
  for (const runtime of ['claude', 'codex', 'grok', 'agy', 'gemini', 'antigravity']) {
    writeFileSync(join(c.root, 'bin', runtime), `#!/bin/sh\nexec ${[process.execPath, file, '--fixture-cli', runtime, c.root, scenario].map(quote).join(' ')} "$@"\n`, { mode: 0o700 });
  }
  const result = spawnSync('/bin/bash', [join(scriptDir, 'make-home.sh'), c.home, '--port', String(c.port)], {
    env: envFor(c, join(c.root, 'setup-home')), cwd: c.root, encoding: 'utf8', timeout: 10000 });
  check(!result.error && result.status === 0, `make-home.sh: ${result.error?.message || result.stderr}`);
  // Leave one production-shaped employee; remove unrelated boot fixtures.
  rmSync(join(c.home, 'agents/plain'), { recursive: true });
  c.agentPath = join(c.home, 'agents/prod-shaped/agent.toml');
  // agents.create keeps the singular model fallback at claude-haiku-4-5,
  // including when the caller selects a Codex preferred model. It is separate
  // from [runtime] fallback. Preserve every other make-home required field.
  const source = read(c.agentPath)
    .replace(/^\[model\][\s\S]*?(?=^\[|$(?![\s\S]))/m, section => section
      .replace(/^preferred = .*$/m,
        `preferred = "${row.primary_runtime === 'claude' ? 'claude-does-not-exist-9' : 'gpt-5.3-codex'}"`)
      .replace(/^fallback = .*$/m, 'fallback = "claude-haiku-4-5"'))
    .replace(/^\[runtime\][\s\S]*?(?=^\[|$(?![\s\S]))/m, section => section
      .replace(/^provider = "claude"$/m,
        `provider = "${row.primary_runtime}"\nfallback = "${row.fallback_runtime}"`))
    .replace(/^approval_required_tools = .*$/m, 'approval_required_tools = []')
    .replace(/^gvu_enabled = true$/m, 'gvu_enabled = false');
  writeFileSync(c.agentPath, source, { mode: 0o600 });
  writeFileSync(join(c.home, 'config.toml'), `[gateway]\nbind = "127.0.0.1"\nport = ${c.port}\nauto_update = false\n[general]\nlog_level = "info"\n[runtime]\nutility_provider = "claude"\n[knowledge.embedding]\nprovider = "none"\n[dispatch]\nenabled = ${scenario === 'A-dispatch'}\n[goal_loop]\niteration_cap = 1\niteration_cap_simple = 1\nmax_concurrent = 1\ntick_secs = 1\n[tick]\nenabled = false\n[proactive]\nenabled = false\n[responsibilities]\nenabled = false\n`, { mode: 0o600 });
  // The inherited OS boundary also denies libraries that ignore proxy settings.
  const policy = join(c.root, 'isolation.sb');
  writeFileSync(policy, `(version 1)(allow default)\n(deny network*)\n(allow network-bind (local ip "localhost:*"))\n(allow network-inbound (local ip "localhost:*"))\n(allow network-outbound (remote ip "localhost:*"))\n(allow network* (subpath ${JSON.stringify(c.root)}))\n(deny mach-lookup (global-name-regex #".*[Ss]ecurity.*" #".*secd.*"))\n(deny file-read* file-write* ${['.duduclaw', '.claude', 'Library/Keychains'].map(p => `(subpath ${JSON.stringify(join(homedir(), p))})`).join(' ')})\n`, { mode: 0o600 });
  c.log = join(c.root, 'gateway.log'); writeFileSync(c.log, '', { mode: 0o600 });
  if (verbose) console.error(`${scenario} gateway log: ${c.log}`);
  c.gateway = spawn('/usr/bin/sandbox-exec', ['-f', policy, binary, 'run', '--yes'], {
    env: envFor(c), cwd: join(c.home, 'os-home'), detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
  c.gateway.on('error', error => { c.spawnError = error; });
  c.closed = new Promise(done => c.gateway.once('close', done));
  for (const stream of [c.gateway.stdout, c.gateway.stderr]) stream.on('data', data => appendFileSync(c.log, data));
  const end = Date.now() + 60000;
  let ready = false;
  while (Date.now() < end && !interrupted) {
    if (c.spawnError) throw c.spawnError;
    check(c.gateway.exitCode === null && c.gateway.signalCode === null, 'Gateway exited during startup');
    // Verify listener ownership before sending credentials or a request.
    const owner = spawnSync('/usr/sbin/lsof', ['-nP', `-iTCP@127.0.0.1:${c.port}`, '-sTCP:LISTEN', '-t'],
      { env: envFor(c), encoding: 'utf8', timeout: 2000 });
    check(!owner.error && [0, 1].includes(owner.status), 'lsof listener ownership check failed');
    if (owner.status === 0) {
      check(owner.stdout.trim() === String(c.gateway.pid), 'Port was claimed by another process');
      try { ready = (await request(c, '/healthz')).ok === true; } catch { /* Readiness only. */ }
      if (ready) break;
    }
    await delay(100);
  }
  check(ready, 'Gateway readiness timed out/interrupted');
  const email = 'owner@fixture.invalid', password = `Fixture-${randomBytes(24).toString('hex')}!`;
  await request(c, '/api/first-run/claim', { email, password });
  const login = await request(c, '/api/login', { email, password });
  check(login.access_token, 'Login did not return access_token');
  c.admin = new Socket(c.port, '/ws'); c.sockets.add(c.admin); await c.admin.ready;
  const connected = await rpc(c, 'connect', { jwt: login.access_token });
  check(connected.user && !connected.admin_fallback, 'Account identity required');
  check((await rpc(c, 'agents.list')).agents?.some(a => a.name === 'prod-shaped'), 'Fixture agent missing');
  c.jwt = login.access_token;
}

async function runScenario(scenario, options) {
  const claude = scenario.startsWith('A-');
  const row = { scenario, status: 'ran', primary_runtime: claude ? 'claude' : 'codex', primary_invocations: 0,
    fallback_runtime: claude ? 'codex' : 'grok', fallback_invocations: 0, fallback_model_arg: null,
    primary_probe_invocations: 0, fallback_probe_invocations: 0, fallback_attempted: false,
    fallback_step_recorded: false, diagnostics: [], explicit_fallback_logged: false, explicit_fallback_lines: [],
    primary_models_tried: [], fallback_models_tried: [],
    reply: null, error: null, agent_toml_model_section: null, gateway_log_markers: [] };
  const root = realpathSync(mkdtempSync('/private/tmp/ddc-fallback-'));
  const c = { root, home: join(root, 'home'), sockets: new Set() }; active = c;
  try {
    await setup(c, options.binary, scenario, row, options.verbose);
    if (scenario === 'A-dispatch') {
      // Same RPC as the dashboard's complex-assignment form. Observe the
      // worker's first completed dispatch, not the goal verifier's verdict.
      c.startIndex = read(join(root, 'invocations.jsonl')).trim().split('\n').filter(Boolean).length;
      const created = await rpc(c, 'tasks.goal_create', {
        agent_id: 'prod-shaped', description: 'Reply FALLBACK_OK for this local runtime reproduction.',
        acceptance_criteria: 'Return the literal FALLBACK_OK.', priority: 'medium',
        plan_first: false, timeout_secs: 60,
      });
      check(created.dispatch_enabled === true && created.task?.id,
        'A-dispatch requires an enabled dispatcher and a created goal');
      row.task_id = created.task.id;
      row.dispatch_scope = 'first_worker_run';
      const deadline = Date.now() + 120000;
      let terminal;
      while (Date.now() < deadline && !interrupted) {
        const listed = await rpc(c, 'runs.list', { agent_id: 'prod-shaped', limit: 100 });
        terminal = listed.runs?.find(run => run.task_id === row.task_id &&
          run.id?.startsWith('dispatch:') && ['completed', 'error'].includes(run.status));
        if (terminal) break;
        check(c.gateway.exitCode === null && c.gateway.signalCode === null,
          'Gateway exited before the dispatch finished');
        await delay(250);
      }
      check(terminal, 'A-dispatch did not produce a terminal worker run before the deadline');
      const detail = await rpc(c, 'runs.get', { run_id: terminal.id });
      row.dispatch_run_id = terminal.id;
      row.dispatch_status = terminal.status;
      const output = detail.events?.filter(event => event.kind === 'text' && event.role === 'assistant')
        .map(event => event.preview || '').join('\n') || '';
      row.reply = terminal.status === 'completed' ? output : null;
      row.error = terminal.status === 'error' ? output : null;
    } else {
      if (scenario === 'C-fallback-model') {
        row.agent_toml_model_section_before = modelSection(c.agentPath);
        row.agents_update_fallback_requested = 'grok-4.7-build-fast';
        await rpc(c, 'agents.update', { agent_id: 'prod-shaped', fallback: row.agents_update_fallback_requested });
      }
      row.agent_toml_model_section = modelSection(c.agentPath);
      const chat = new Socket(c.port, '/ws/chat'); c.sockets.add(chat); await chat.ready;
      chat.send({ type: 'auth', token: c.jwt });
      const hello = await chat.take(f => ['session_info', 'error'].includes(f.type));
      check(hello.type === 'session_info' && hello.session_id, `Chat auth: ${JSON.stringify(hello)}`);
      const conv = `fallback-${randomBytes(8).toString('hex')}`;
      // Start counting after auth/configuration, before the one ordinary turn.
      c.startIndex = read(join(root, 'invocations.jsonl')).trim().split('\n').filter(Boolean).length;
      chat.send({ type: 'user_message', session_id: hello.session_id, agent: 'prod-shaped',
        content: 'Reply to this local runtime reproduction message.', conv, msg_id: conv });
      const terminal = await chat.take(f => f.type === 'error' ||
        (['assistant_done', 'cancelled', 'no_reply', 'turn_consumed'].includes(f.type) && (!f.conv || f.conv === conv)), 120000);
      row.reply = terminal.content ?? null;
      row.error = terminal.type === 'error' ? (terminal.message ?? JSON.stringify(terminal))
        : terminal.type === 'assistant_done' ? null : JSON.stringify(terminal);
    }
  } catch (error) { row.status = 'error'; row.error = error.message; }
  finally {
    try { await cleanup(c, options.keep, row); }
    catch (error) { row.status = 'error'; row.error = `${row.error || ''} cleanup: ${error.message}`; }
    active = null;
  }
  if (scenario === 'control-codex-to-grok' && !row.fallback_attempted && row.status !== 'harness_invalid') {
    row.status = 'harness_invalid';
    row.error = `Control has no observed Grok RUN; ${row.error || ''}`;
  }
  return row;
}

async function main() {
  const args = process.argv.slice(2), options = { selected: [], keep: false, verbose: false };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === '--bin' || arg === '--scenario') {
      check(args[i + 1] && !args[i + 1].startsWith('--'), `${arg} requires a value`);
      if (arg === '--bin') options.binary = resolve(args[++i]); else options.selected.push(args[++i]);
    } else if (arg === '--keep') options.keep = true;
    else if (arg === '--verbose') options.verbose = true;
    else throw new Error(`Unknown option: ${arg}`);
  }
  check(options.binary && existsSync(options.binary), '--bin <existing executable> is required');
  check(options.selected.every(s => names.includes(s)), `Unknown scenario; choose: ${names.join(', ')}`);
  check(process.platform === 'darwin' && existsSync('/usr/bin/sandbox-exec'), 'macOS sandbox-exec required for enforced offline isolation');
  for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { interrupted = true; if (active?.gateway?.pid && groupAlive(active.gateway.pid)) process.kill(-active.gateway.pid, 'SIGTERM'); });
  const summary = {}; let failed = false;
  for (const scenario of names.filter(s => !options.selected.length || options.selected.includes(s))) {
    const row = await runScenario(scenario, options);
    console.log(JSON.stringify(row)); summary[scenario] = { status: row.status,
      fallback_attempted: row.fallback_attempted, fallback_step_recorded: row.fallback_step_recorded };
    failed ||= ['error', 'harness_invalid'].includes(row.status);
    if (interrupted) { failed = true; break; }
  }
  console.log(JSON.stringify({ summary })); process.exitCode = failed ? 2 : 0;
}

if (process.argv[2] === '--fixture-cli') {
  await fakeCLI(process.argv[3], process.argv[4], process.argv[5], process.argv.slice(6)).catch(error => {
    console.error(error.message); process.exitCode = 2;
  });
} else await main().catch(error => {
  console.error(error.message); console.log(JSON.stringify({ summary: {}, error: error.message })); process.exitCode = 2;
});
