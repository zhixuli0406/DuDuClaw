#!/usr/bin/env node
// RM-131 P15: isolated gateway + stdio MCP + synthetic Google. Node 22+.
// Run with an already-built debug --features live-test-mocks binary on macOS.
import assert from 'node:assert/strict';
import { createHash, createHmac, generateKeyPairSync, randomBytes, sign, verify } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, readdirSync, existsSync, renameSync, symlinkSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { withOwnedResources, closeServer, deadline } from './google-connectors-harness.mjs';

const self = fileURLToPath(import.meta.url);
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
async function until(check, label) {
  const end = Date.now() + 45000;
  while (Date.now() < end) { const value = await check(); if (value) return value; await pause(50); }
  throw new Error(`Not observed: ${label}`);
}
const save = (path, data) => writeFileSync(path, JSON.stringify(data, null, 2), { mode: 0o600 });

// The runtime launches this local fake Claude. MCP remains a descendant of
// the authenticated runtime, so the gateway supplies the real turn identity.
async function fixtureCLI(home, binary, args) {
  if (args.includes('--version')) { console.log('99.0.0'); return; }
  if (args.includes('auth') || args.includes('login')) {
    console.log(JSON.stringify({ loggedIn: true, authMethod: 'claude.ai', email: 'fixture@c.test' })); return;
  }
  let prompt = args.join('\n');
  for (const flag of ['--system-prompt-file', '--append-system-prompt-file']) {
    const at = args.indexOf(flag); if (at >= 0) prompt += readFileSync(args[at + 1], 'utf8');
  }
  const finish = () => console.log(JSON.stringify({ type: 'result', result: 'Local fixture completed.', is_error: false }));
  const jobPath = join(home, 'job.json');
  if (args.includes('--no-session-persistence') || !process.env.DUDUCLAW_RUN_CREDENTIAL || !existsSync(jobPath)) { finish(); return; }
  const job = JSON.parse(readFileSync(jobPath));
  if (!prompt.includes(job.marker)) { finish(); return; }
  mkdirSync(join(home, `${job.id}.claimed`));
  const at = args.indexOf('--mcp-config'); assert(at >= 0, 'runtime MCP registration missing');
  const source = args[at + 1];
  const registration = JSON.parse(source.startsWith('{') ? source : readFileSync(source, 'utf8'));
  const entry = registration.mcpServers.duduclaw ?? registration.mcpServers['duduclaw-pro'];
  assert(entry, 'platform MCP registration missing');
  const wire = [];
  await withOwnedResources(async (owned) => {
    const child = owned.spawn(binary, entry.args ?? ['mcp-server'], {
      ...process.env, ...entry.env, ...JSON.parse(readFileSync(join(home, 'fixture-env.json'))),
      HOME: join(home, 'os-home'), DUDUCLAW_HOME: home,
    }, { stdio: ['pipe', 'pipe', 'inherit'] }).child;
    const waiting = new Map(); let id = 0;
    const lines = createInterface({ input: child.stdout }); owned.defer(() => lines.close());
    lines.on('line', (line) => { wire.push(line); const r = JSON.parse(line); waiting.get(r.id)?.(r); });
    const rpc = async (method, params) => {
      const key = ++id, message = JSON.stringify({ jsonrpc: '2.0', id: key, method, params });
      wire.push(message);
      const response = new Promise((done) => waiting.set(key, done)); child.stdin.write(`${message}\n`);
      try { return await deadline(response, 60000, `MCP ${method}`); } finally { waiting.delete(key); }
    };
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'drive-write-fixture', version: '1' } });
    const initialized = '{"jsonrpc":"2.0","method":"notifications/initialized"}';
    wire.push(initialized); child.stdin.write(`${initialized}\n`);
    const listed = await rpc('tools/list', {});
    const arguments_ = { ...job.arguments };
    if (job.attachment) {
      const files = readdirSync(join(home, 'agents/prod-shaped/attachments'));
      const matches = files.filter((name) => readFileSync(join(home, 'agents/prod-shaped/attachments', name), 'utf8') === job.attachment);
      assert.equal(matches.length, 1, 'must use the attachment actually admitted this turn');
      arguments_.attachment = matches[0];
    }
    const reply = await rpc('tools/call', { name: job.tool, arguments: arguments_ });
    const observation = { listed: listed.result?.tools.map((t) => t.name), reply, wire };
    save(join(home, `${job.id}.json.tmp`), observation); renameSync(join(home, `${job.id}.json.tmp`), join(home, `${job.id}.json`));
  });
  finish();
}

if (process.argv[2] === '--fixture-cli') {
  await fixtureCLI(process.argv[3], process.argv[4], process.argv.slice(5));
} else {
  await main();
}

async function main() {
  assert(process.argv[2], 'usage: node google-drive-write.mjs <debug-live-test-mocks-binary>');
  const binary = resolve(process.argv[2]);
  assert(process.platform === 'darwin' && existsSync('/usr/bin/sandbox-exec'), 'requires macOS loopback-only sandbox');
  const home = mkdtempSync(join(tmpdir(), 'ddc-google-drive-write-'));
  console.log(`ARTIFACTS ${home}`);
  const policy = '(version 1)(allow default)(deny network*)(allow network-bind (local ip "localhost:*"))(allow network-inbound (local ip "localhost:*"))(allow network-outbound (remote ip "localhost:*"))';
  const requests = [], wire = [], mockFailures = [], issued = [], results = [], modelCalls = [];
  let mountCalls = 0;
  const codes = new Map(), refreshes = new Map(), accessScopes = new Map();
  const drive = 'https://www.googleapis.com/auth/drive';
    const writeScopes = [drive, 'https://www.googleapis.com/auth/documents', 'https://www.googleapis.com/auth/spreadsheets'];
  const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  let poison = false;
  const folder = 'fixture-folder-inside', outside = 'fixture-folder-outside';
  const files = new Map();
  const metadata = (id, parents = [folder], mimeType = 'text/plain') => ({ id, name: id, mimeType, parents, trashed: false,
    capabilities: { canEdit: true, canRename: true, canAddChildren: true, canTrash: true, canMoveItemWithinDrive: true } });
  files.set(folder, metadata(folder, ['fixture-root-folder'], 'application/vnd.google-apps.folder'));
  files.set('fixture-root-folder', metadata('fixture-root-folder', [], 'application/vnd.google-apps.folder'));
  files.set(outside, metadata(outside, [], 'application/vnd.google-apps.folder'));
  files.set('fixture-file-inside', metadata('fixture-file-inside'));
  files.set('fixture-file-outside', metadata('fixture-file-outside', [outside]));
  files.set('fixture-file-mixed', metadata('fixture-file-mixed', [folder, outside]));
  files.set('fixture-file-hidden', metadata('fixture-file-hidden', ['fixture-parent-hidden']));
  const json = (res, value, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
  await withOwnedResources(async (owned) => {
    owned.defer(() => {
      save(join(home, 'google-requests.json'), requests);
      save(join(home, 'mcp-wire.json'), wire);
      save(join(home, 'results.json'), results);
      save(join(home, 'mock-failures.json'), mockFailures);
    });
    const mock = createServer(async (req, res) => {
      let raw = ''; for await (const chunk of req) raw += chunk;
      const url = new URL(req.url, 'http://127.0.0.1');
      let body = raw; try { body = JSON.parse(raw); } catch {}
      // Record EVERY request, including issuer calls; headers are never saved.
      requests.push({ method: req.method, path: url.pathname, query: url.search, body,
        granted_scopes: accessScopes.get(req.headers.authorization?.replace('Bearer ', '')) });
      try {
        if (url.pathname === '/model/v1/chat/completions') {
          assert.equal(req.headers.authorization, 'Bearer fixture-model-only');
          const tool = body.tools?.find((t) => t.function?.name.includes('fixture_drive_lookup'));
          const result = body.messages?.filter((m) => m.role === 'tool').at(-1);
          modelCalls.push({ tools: body.tools?.map((t) => t.function?.name), result: result?.content });
          const invoke = tool && !result;
          const message = invoke ? { role: 'assistant', content: null, tool_calls: [{ id: 'fixture-call', type: 'function', function: { name: tool.function.name, arguments: '{}' } }] }
            : { role: 'assistant', content: `Local mount fixture completed. ${result?.content ?? 'No mount available.'}` };
          const finish = invoke ? 'tool_calls' : 'stop';
          if (body.stream) {
            res.writeHead(200, { 'content-type': 'text/event-stream' });
            const delta = invoke ? { role: 'assistant', tool_calls: message.tool_calls.map((t) => ({ index: 0, ...t })) } : message;
            for (const choice of [{ delta, finish_reason: null }, { delta: {}, finish_reason: finish }]) res.write(`data: ${JSON.stringify({ id: 'fixture-model', object: 'chat.completion.chunk', model: 'gpt-4o-mini', choices: [{ index: 0, ...choice }] })}\n\n`);
            return res.end('data: [DONE]\n\n');
          }
          return json(res, { id: 'fixture-model', object: 'chat.completion', model: 'gpt-4o-mini', choices: [{ index: 0, message, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 5 } });
        }
        if (url.pathname === '/mcp') {
          assert.equal(req.method, 'POST');
          assert(accessScopes.has(req.headers.authorization?.replace('Bearer ', '')));
          wire.push(raw);
          if (body.id === undefined) { res.writeHead(202); return res.end(); }
          let result;
          if (body.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'synthetic-drive', version: '1' } };
          else if (body.method === 'tools/list') result = { tools: [{ name: 'fixture_drive_lookup', description: 'Read a synthetic Drive record', annotations: { readOnlyHint: true }, inputSchema: { type: 'object', properties: {} } }] };
          else { assert.equal(body.method, 'tools/call'); assert.equal(body.params.name, 'fixture_drive_lookup'); mountCalls++; result = { content: [{ type: 'text', text: 'fixture Drive record' }] }; }
          const response = { jsonrpc: '2.0', id: body.id, result }; wire.push(JSON.stringify(response)); return json(res, response);
        }
        if (url.pathname === '/token' || url.pathname === '/oauth2.googleapis.com/token') {
          const form = new URLSearchParams(raw); let scopes, account, refresh;
          if (form.has('assertion')) {
            const [head, payload, signature] = form.get('assertion').split('.');
            assert(verify('RSA-SHA256', Buffer.from(`${head}.${payload}`), publicKey, Buffer.from(signature, 'base64url')));
            const claims = JSON.parse(Buffer.from(payload, 'base64url')); assert.equal(claims.sub, 'alice@c.test');
            scopes = claims.scope.split(' '); account = claims.sub;
          } else if (form.get('grant_type') === 'authorization_code') {
            const code = codes.get(form.get('code')); assert(code, 'unknown authorization code'); codes.delete(form.get('code'));
            assert.equal(form.get('redirect_uri'), code.redirect);
            assert.equal(createHash('sha256').update(form.get('code_verifier')).digest('base64url'), code.challenge);
            scopes = code.scopes; account = 'alice@c.test'; refresh = `fixture-refresh-${randomBytes(8).toString('hex')}`;
            refreshes.set(refresh, { scopes, account });
          } else {
            assert.equal(form.get('grant_type'), 'refresh_token');
            const grant = refreshes.get(form.get('refresh_token')); assert(grant, 'unknown refresh token');
            ({ scopes, account } = grant); refresh = form.get('refresh_token');
            if (poison && !scopes.includes(drive)) scopes = [...scopes, drive];
          }
          const access = `fixture-bearer-${randomBytes(12).toString('hex')}`;
          issued.push({ access, scopes, account }); accessScopes.set(access, scopes);
          return json(res, { access_token: access, token_type: 'Bearer', expires_in: scopes.includes(drive) ? 3600 : 135, scope: scopes.join(' '), ...(refresh ? { refresh_token: refresh } : {}) });
        }
        if (url.pathname === '/openidconnect.googleapis.com/v1/userinfo') {
          assert(accessScopes.has(req.headers.authorization?.replace('Bearer ', '')));
          return json(res, { sub: 'fixture-google-alice', email: 'alice@c.test', email_verified: true, hd: 'c.test' });
        }
        assert(accessScopes.has(req.headers.authorization?.replace('Bearer ', '')), 'only fixture credentials accepted');
        const path = url.pathname.replace(/^\/[^/]+\.googleapis\.com(?=\/)/, '');
        const match = path.match(/^\/drive\/v3\/files\/([A-Za-z0-9_-]+)$/);
        if (match) {
          const file = files.get(match[1]); if (!file) return json(res, { error: { code: 404 } }, 404);
          if (req.method === 'PATCH') { Object.assign(file, body); return json(res, file); }
          assert.equal(req.method, 'GET');
          if (url.searchParams.get('alt') === 'media') { res.writeHead(200, { 'content-type': 'text/plain' }); return res.end('fixture content'); }
          return json(res, file);
        }
        if (req.method === 'POST' && ['/upload/drive/v3/files', '/drive/v3/files'].includes(path)) {
          let meta = body;
          if (path.startsWith('/upload/')) {
            assert.equal(url.searchParams.get('uploadType'), 'multipart');
            const part = raw.match(/Content-Type: application\/json[^\r\n]*\r\n\r\n([^\r\n]+)/i); assert(part, 'multipart JSON metadata missing');
            meta = JSON.parse(part[1]);
          }
          const id = `fixture-created-${files.size}`; const file = { ...metadata(id), ...meta, id };
          files.set(id, file); return json(res, file);
        }
        if (path === '/drive/v3/files' && req.method === 'GET') return json(res, { files: [...files.values()] });
        if (path === '/v1/documents/fixture-document') return json(res, { documentId: 'fixture-document', body: { content: [{ startIndex: 1, paragraph: { elements: [{ startIndex: 1, textRun: { content: 'unique anchor\n' } }] } }] } });
        if (path === '/v4/spreadsheets/fixture-spreadsheet') return json(res, { spreadsheetId: 'fixture-spreadsheet', sheets: [{ properties: { sheetId: 0, title: 'Sheet1', gridProperties: { rowCount: 100, columnCount: 26 } } }] });
        if (req.method === 'POST' && ['/v1/documents/fixture-document:batchUpdate', '/v4/spreadsheets/fixture-spreadsheet:batchUpdate'].includes(path)) return json(res, { replies: [] });
        throw new Error(`Unexpected mock request ${req.method} ${url.pathname}`);
      } catch (error) { mockFailures.push(error.message); json(res, { error: error.message }, 500); }
    });
    owned.defer(() => closeServer(mock)); mock.listen(0, '127.0.0.1'); await once(mock, 'listening');
    const base = `http://127.0.0.1:${mock.address().port}`;
    const lease = createServer(); owned.defer(() => closeServer(lease)); lease.listen(0, '127.0.0.1'); await once(lease, 'listening');
    const port = lease.address().port; await closeServer(lease);
    const setup = spawnSync('bash', [join(dirname(self), 'make-home.sh'), home, '--port', String(port)], {
      env: { HOME: home, PATH: '/usr/bin:/bin', TMPDIR: tmpdir() }, encoding: 'utf8',
    }); assert.equal(setup.status, 0, setup.stderr);
    const configPath = join(home, 'config.toml');
    writeFileSync(configPath, readFileSync(configPath, 'utf8') + `
[integrations]
google_workspace = true
[integrations.google_identity]
allowed_domains = ["c.test"]
allowed_subject_domains = ["c.test"]
[dispatch]
enabled = false
[proactive]
enabled = false
[tick]
enabled = false
[[accounts]]
provider = "openai"
api_key = "fixture-model-only"
base_url = "${base}/model/v1"
`);
    // Upgrade fixture, matching v185-acceptance: legacy MFA remains off.
    // This is not fresh-install MFA onboarding acceptance.
    const seed = spawnSync('/usr/bin/sqlite3', [join(home, 'users.db')], { env: { HOME: home, PATH: '/usr/bin:/bin' }, encoding: 'utf8', input: `
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
    ` }); assert.equal(seed.status, 0, seed.stderr);
    const mountDir = join(home, 'agents/drive-mount'); mkdirSync(mountDir);
    writeFileSync(join(mountDir, 'agent.toml'), readFileSync(join(home, 'agents/plain/agent.toml'), 'utf8')
      .replaceAll('plain', 'drive-mount').replace('provider = "claude"', 'provider = "openai_compat"').replaceAll('claude-haiku-4-5', 'gpt-4o-mini')
      + `\n[[mcp.external]]\nname="synthetic-drive"\nurl="${base}/mcp"\nbearer_token="oauth://google"\n`);
    writeFileSync(join(mountDir, 'SOUL.md'), '# Local Drive mount fixture\nUse only the synthetic Drive lookup.\n');
    const binDir = join(home, 'bin'); mkdirSync(binDir); symlinkSync(binary, join(binDir, 'duduclaw'));
    const quote = (s) => `'${s.replaceAll("'", "'\\''")}'`;
    writeFileSync(join(binDir, 'claude'), `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(self)} --fixture-cli ${quote(home)} ${quote(binary)} "$@"\n`, { mode: 0o700 });
    const env = { HOME: join(home, 'os-home'), DUDUCLAW_HOME: home, PATH: `${binDir}:/usr/bin:/bin`, TMPDIR: tmpdir(), LANG: 'C',
      DUDUCLAW_EDITION: 'enterprise', DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: base };
    const fingerprint = spawnSync(binary, ['license', 'fingerprint'], { env, encoding: 'utf8' }); assert.equal(fingerprint.status, 0, fingerprint.stderr);
    const licenseKeys = generateKeyPairSync('ed25519');
    const payload = { version: 2, subscription_id: 'sub_drive_write_probe', customer_id: 'fixture', tier: 'oem', machine_fingerprint: fingerprint.stdout.trim().split('\n')[0],
      issued_at: new Date().toISOString().replace(/\.000Z$/, 'Z'), expires_at: new Date(Date.now() + 86400000).toISOString().replace(/\.000Z$/, 'Z'), last_phone_home: new Date().toISOString().replace(/\.000Z$/, 'Z'), public_key_id: 'drive_write_probe', nfr: true };
    save(join(home, 'license.json'), { ...payload, signature: sign(null, Buffer.from(JSON.stringify(payload)), licenseKeys.privateKey).toString('base64') });
    env.DUDUCLAW_LICENSE_PUBKEY_DRIVE_WRITE_PROBE = Buffer.from(licenseKeys.publicKey.export({ format: 'jwk' }).x, 'base64url').toString('hex');
    save(join(home, 'fixture-env.json'), { DUDUCLAW_EDITION: 'enterprise', DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: base,
      DUDUCLAW_LICENSE_PUBKEY_DRIVE_WRITE_PROBE: env.DUDUCLAW_LICENSE_PUBKEY_DRIVE_WRITE_PROBE });
    // Confine gateway and every runtime descendant. A missing mock redirect
    // fails locally rather than ever contacting a real Google endpoint.
    const gateway = owned.spawn('/usr/bin/sandbox-exec', ['-p', policy, binary, 'run', '--yes'], env, { stdio: ['ignore', 'pipe', 'pipe'] });
    let gatewayLog = '';
    gateway.child.stdout.on('data', (data) => { gatewayLog += data; }); gateway.child.stderr.on('data', (data) => { gatewayLog += data; });
    owned.defer(() => writeFileSync(join(home, 'gateway.log'), gatewayLog, { mode: 0o600 }));
    const api = `http://127.0.0.1:${port}`;
    await until(async () => { assert(!gateway.closed, 'gateway exited'); try { return (await fetch(`${api}/health`)).ok; } catch { return false; } }, 'gateway health');
    assert.equal((await (await fetch(`${api}/healthz`)).json()).live_test_mocks, true);
    const post = async (path, body) => { const r = await fetch(`${api}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }); assert(r.ok, `${path}: ${r.status}`); return r.json(); };
    const dashboard = async (token) => {
      const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`); owned.defer(() => ws.close(), { beforeChildren: true }); await once(ws, 'open');
      const pending = new Map(); let seq = 0;
      ws.addEventListener('message', ({ data }) => { const r = JSON.parse(data); if (r.type === 'res') pending.get(r.id)?.(r); });
      const rpc = async (method, params = {}) => {
        const id = String(++seq), answer = new Promise((done) => pending.set(id, done)); ws.send(JSON.stringify({ type: 'req', id, method, params }));
        try { const r = await deadline(answer, 45000, method); assert(r.ok, `${method}: ${JSON.stringify(r.error)}`); return r.payload; } finally { pending.delete(id); }
      };
      await rpc('connect', { jwt: token }); return rpc;
    };
    const login = { email: 'admin@c.test', password: 'fixture-password-only' }; await post('/api/first-run/claim', login);
    const admin = await dashboard((await post('/api/login', login)).access_token);
    assert.equal((await admin('system.status')).edition_profile, 'enterprise', 'signed fixture entitlement required');
    await admin('security.credential_protection_enable');
    const key = JSON.stringify({ type: 'service_account', client_email: 'fixture@project.iam.gserviceaccount.test', client_id: '1234567890', private_key: privateKey.export({ format: 'pem', type: 'pkcs8' }), token_uri: `${base}/token` });
    await admin('google.delegation.import_key', { key_json: key });
    const member = (await admin('users.create', { email: 'alice@c.test', display_name: 'Alice', password: login.password, role: 'employee' })).user;
    await admin('users.bind_agent', { user_id: member.id, agent_name: 'prod-shaped', access_level: 'owner' });
    const tokens = await post('/api/login', { email: member.email, password: login.password });
    const alice = await dashboard(tokens.access_token);
    const identity = (value) => admin('agents.google_identity_set', { agent_id: 'prod-shaped', identity: value, requesters: 'anyone', confirm_anyone: true });
    await identity('delegate:alice@c.test');
    const setPolicy = async (extra = {}) => {
      const current = await admin('google.drive_write.get');
      assert.equal(typeof current.hash, 'string', 'google.drive_write.get hash missing');
      return admin('google.drive_write.set', { create: true, modify: true, move: true, trash: false,
        approval: { create: 'none', modify: 'none', move: 'none', trash: 'none' }, folders: [folder], acknowledge_risk: true,
        ...extra, expected_hash: current.hash });
    };
    const service = (from) => requests.slice(from).filter((r) => !['/token', '/oauth2.googleapis.com/token', '/openidconnect.googleapis.com/v1/userinfo'].includes(r.path));
    const writes = (from) => service(from).filter((r) => r.method !== 'GET');
    const checkReply = (observation, ok, pattern) => {
      const reply = observation.reply; assert.equal(Boolean(reply.error || reply.result?.isError), !ok, JSON.stringify(reply));
      if (pattern) assert.match(JSON.stringify(reply), pattern);
    };
    const passed = (row, detail) => { results.push({ row, detail }); console.log(`PASS §7 #${row}: ${detail}`); };
    const audit = () => readFileSync(join(home, 'tool_calls.jsonl'), 'utf8').trim().split('\n').map((line) => JSON.parse(line)).filter((r) => r.event === 'google_drive_write' || JSON.stringify(r).includes('"google_drive_write"'));
    const chat = new WebSocket(`ws://127.0.0.1:${port}/ws/chat`); owned.defer(() => chat.close(), { beforeChildren: true }); await once(chat, 'open');
    const frames = []; chat.addEventListener('message', ({ data }) => frames.push(JSON.parse(data)));
    chat.send(JSON.stringify({ type: 'auth', token: tokens.access_token }));
    const hello = await until(() => frames.find((f) => f.type === 'session_info'), 'member WebChat session');
    async function turn(tool, arguments_, { attachment, decide } = {}) {
      const id = randomBytes(12).toString('hex'), marker = `fixture-request:${id}`;
      const job = { id, marker, tool, arguments: arguments_, attachment }; save(join(home, 'job.json'), job);
      const from = frames.length;
      chat.send(JSON.stringify({ type: 'user_message', session_id: hello.session_id, agent: 'prod-shaped', conv: id, msg_id: id,
        content: `Use only the local fixture tool. [${marker}]`, attachments: attachment ? [{ filename: `${id}.txt`, mime: 'text/plain', data_base64: Buffer.from(attachment).toString('base64') }] : [] }));
      if (decide) {
        const card = await until(async () => {
          assert(!existsSync(join(home, `${id}.json`)), 'tool finished before confirmation');
          return (await alice('me.google.write_confirmations')).requests.find((r) => r.employee?.includes('prod-shaped'));
        }, '本人確認卡');
        await decide(card);
      }
      const observation = await until(() => {
        const error = frames.slice(from).find((f) => f.type === 'error'); assert(!error, JSON.stringify(error));
        return existsSync(join(home, `${id}.json`)) && JSON.parse(readFileSync(join(home, `${id}.json`)));
      }, `tool result ${tool}`);
      wire.push(...observation.wire);
      await until(() => frames.slice(from).find((f) => f.type === 'assistant_done' && f.conv === id), 'WebChat turn completion');
      return observation;
    }

    // #3: invalidate the signed policy by editing only the isolated file.
    await setPolicy(); const savedConfig = readFileSync(configPath, 'utf8');
    const changed = savedConfig.replace(/(\[google\.drive_write\][\s\S]*?\btrash\s*=\s*)false/, '$1true');
    assert.notEqual(changed, savedConfig, 'must actually change trash'); writeFileSync(configPath, changed);
    let from = requests.length;
    let observation = await turn('drive_trash_file', { file_id: 'fixture-file-inside' });
    checkReply(observation, false); assert(!observation.listed.includes('drive_trash_file')); assert.equal(writes(from).length, 0);
    assert(['drive_create_file', 'drive_copy_file', 'drive_update_file', 'drive_move_file', 'drive_trash_file', 'docs_insert_text', 'docs_format', 'sheets_add_sheet', 'sheets_add_note'].every((t) => !observation.listed.includes(t)));
    passed(3, 'tampered digest hides tools and refuses calls');
    // Restore only the fixture's prior consent, then use the Admin RPC again.
    writeFileSync(configPath, savedConfig); await setPolicy();

    // #4: real admitted attachment, delegated subject, multipart parents + audit.
    from = requests.length;
    observation = await turn('drive_create_file', { name: 'fixture attachment.txt', parent_folder_id: folder }, { attachment: 'current-turn synthetic attachment' });
    checkReply(observation, true);
    const upload = writes(from); assert.equal(upload.length, 1); assert.equal(upload[0].method, 'POST');
    assert(upload[0].path.endsWith('/upload/drive/v3/files')); assert.equal(new URLSearchParams(upload[0].query).get('uploadType'), 'multipart');
    const meta = upload[0].body.match(/Content-Type: application\/json[^\r\n]*\r\n\r\n([^\r\n]+)/i); assert(meta);
    assert.deepEqual(JSON.parse(meta[1]).parents, [folder]); assert(upload[0].body.includes('current-turn synthetic attachment'));
    assert(audit().some((r) => JSON.stringify(r).includes('"confirmed"') && JSON.stringify(r).includes('drive_create_file')));
    passed(4, 'delegated current-turn multipart upload and confirmed audit');

    // #5: one outside parent is enough; invisible parent is never assumed safe.
    for (const id of ['fixture-file-outside', 'fixture-file-mixed', 'fixture-file-hidden']) {
      from = requests.length; observation = await turn('drive_update_file', { file_id: id, name: 'refused rename' });
      checkReply(observation, false, /folder_out_of_scope|parent_not_visible|資料夾|上層/);
      assert(service(from).length > 0); assert(service(from).every((r) => r.method === 'GET'));
    }
    passed(5, 'outside, mixed-parent and invisible-parent refusals send GET only');

    // #6: trash is PATCH with an exact, fixed body.
    await setPolicy({ trash: true }); from = requests.length;
    observation = await turn('drive_trash_file', { file_id: 'fixture-file-inside' }); checkReply(observation, true);
    assert.equal(writes(from).length, 1); assert.equal(writes(from)[0].method, 'PATCH'); assert.deepEqual(writes(from)[0].body, { trashed: true });
    files.get('fixture-file-inside').trashed = false;
    passed(6, 'trash sends exactly PATCH {"trashed":true}');

    // #7: empty allowlist does not permit moves.
    await setPolicy({ folders: [] }); from = requests.length;
    observation = await turn('drive_move_file', { file_id: 'fixture-file-inside', destination_folder_id: folder });
    checkReply(observation, false, /folder_required|這項操作需要先限定可使用的資料夾，請管理員設定。|限定資料夾/); assert(!observation.listed.includes('drive_move_file')); assert.equal(writes(from).length, 0);
    passed(7, 'empty folder list hides and refuses move');

    // #8: decide the real member card through the dashboard, without a bot.
    await setPolicy({ approval: { create: 'member', modify: 'member', move: 'member', trash: 'member' } });
    for (const decision of ['approve', 'cancel', 'rename']) {
      from = requests.length;
      observation = await turn('drive_update_file', { file_id: 'fixture-file-inside', name: `requested-${decision}` }, { decide: async (card) => {
        assert.equal(writes(from).length, 0, 'no write before member decision');
        if (decision === 'rename') files.get('fixture-file-inside').name = 'provider changed name while awaiting approval';
        await alice('me.google.write_confirm', { id: card.id, approve: decision !== 'cancel' });
      } });
      checkReply(observation, decision === 'approve');
      assert.equal(writes(from).length, decision === 'approve' ? 1 : 0);
      if (decision === 'cancel') assert(audit().some((r) => JSON.stringify(r).includes('"not_sent"')));
      if (decision === 'rename') assert.match(JSON.stringify(observation.reply), /changed|變更|重新核准/);
    }
    passed(8, 'member approval executes, cancellation not_sent, changed filename invalidates approval');

    // #10: base + Drive-read consents stay separate from the write grant.
    await setPolicy({ folders: [] });
    await admin('mcp.oauth.start', { provider_id: 'google', client_id: 'fixture-client', client_secret: 'fixture-secret' });
    async function consent(method, extra = {}) {
      const start = await alice(method, { redirect_origin: api, acknowledge_google_data: true, acknowledge_write: true, ...extra });
      const auth = new URL(start.authorization_url); assert.equal(auth.origin, base); assert.equal(new URL(start.redirect_uri).origin, api);
      assert.equal(auth.searchParams.get('code_challenge_method'), 'S256');
      if (method === 'me.google.write_connect_start') assert.equal(auth.searchParams.get('include_granted_scopes'), 'false');
      const code = randomBytes(12).toString('hex'); codes.set(code, { scopes: auth.searchParams.get('scope').split(' '), redirect: start.redirect_uri, challenge: auth.searchParams.get('code_challenge') });
      const response = await fetch(`${start.redirect_uri}?state=${encodeURIComponent(start.state)}&code=${code}`, { redirect: 'manual' }); assert.equal(response.status, 303);
      return alice('me.google.connect_finish', { state: start.state });
    }
    await consent('me.google.connect_start'); await consent('me.google.connect_start', { drive_tools: true });
    await identity(`user:${member.id}`); await alice('me.google.consents.set', { agent_id: 'prod-shaped', allowed: true, confirm_shared_access: true });
    from = requests.length; observation = await turn('drive_create_file', { name: 'personal.txt', parent_folder_id: folder, text: 'fixture personal content' });
    checkReply(observation, false, /drive_write|重新連結|連結/); assert.match(JSON.stringify(observation.reply), /\/me\/google\/authorize[^\s]*#ticket=/); assert.equal(writes(from).length, 0);
    // The authorization request purpose must be visible in the stored request,
    // not inferred just from generic consent wording in the tool response.
    const requestEvidence = await alice('me.google.status');
    assert(requestEvidence.authorization_requests.some((r) => r.purpose === 'drive_write'));
    await consent('me.google.write_connect_start');
    observation = await turn('drive_create_file', { name: 'personal.txt', parent_folder_id: folder, text: 'fixture personal content' }); checkReply(observation, true);
    const beforeRead = requests.length;
    observation = await turn('drive_read', { file_id: 'fixture-file-inside' }); checkReply(observation, true);
    const readRequests = service(beforeRead); assert(readRequests.length > 0);
    assert(readRequests.every((r) => r.granted_scopes?.includes(`${drive}.readonly`) && !r.granted_scopes.includes(drive)));
    passed(10, 'personal write consent required, OAuth completion succeeds, drive_read scopes stay isolated');

    // #11: force the ordinary refresh to overgrant. Test the signed broker
    // route directly so refusal code cannot be hidden by tool prose.
    await admin('users.bind_agent', { user_id: member.id, agent_name: 'drive-mount', access_level: 'owner' });
    await admin('agents.google_identity_set', { agent_id: 'drive-mount', identity: `user:${member.id}`, requesters: 'anyone', confirm_anyone: true });
    await alice('me.google.consents.set', { agent_id: 'drive-mount', allowed: true, confirm_shared_access: true });
    async function mountTurn() {
      const conv = randomBytes(12).toString('hex'), fromFrame = frames.length;
      chat.send(JSON.stringify({ type: 'user_message', session_id: hello.session_id, agent: 'drive-mount', conv, msg_id: conv,
        content: 'Use fixture_drive_lookup once against the local synthetic Drive MCP.' }));
      return until(() => {
        const error = frames.slice(fromFrame).find((f) => f.type === 'error'); assert(!error, JSON.stringify(error));
        return frames.slice(fromFrame).find((f) => f.type === 'assistant_done' && f.conv === conv);
      }, 'gateway HTTP Drive MCP tool loop');
    }
    await mountTurn(); assert.equal(mountCalls, 1, 'positive control must execute the remote MCP tool');
    assert(modelCalls.some((r) => r.result?.includes('fixture Drive record')));
    // Ordinary tokens last 135 seconds; the resolver refreshes at expiry-120.
    // Leave that freshness window so the next broker call must really refresh.
    poison = true; await pause(16000);
    const entryConfig = JSON.parse(readFileSync(join(home, 'agents/prod-shaped/.mcp.json'))).mcpServers;
    const entry = entryConfig.duduclaw ?? entryConfig['duduclaw-pro'];
    const body = JSON.stringify({ tool: 'drive_read' }), timestamp = String(Math.floor(Date.now() / 1000)), nonce = randomBytes(16).toString('hex');
    const signature = createHmac('sha256', entry.env.DUDUCLAW_MCP_API_KEY).update(['prod-shaped', entry.env.DUDUCLAW_AGENT_TOKEN, timestamp, nonce, createHash('sha256').update(body).digest('hex')].join('\n')).digest('hex');
    const tokenResponse = await fetch(`${api}/api/internal/google-token`, { method: 'POST', headers: { 'content-type': 'application/json', 'X-Duduclaw-Agent-Id': 'prod-shaped', 'X-Duduclaw-Timestamp': timestamp, 'X-Duduclaw-Nonce': nonce, 'X-Duduclaw-Signature': signature }, body });
    const tokenResult = await tokenResponse.json(); assert.notEqual(tokenResponse.status, 200); assert.equal(tokenResult.code, 'write_scope_isolated');
    const mountStart = requests.filter((r) => r.path === '/mcp').length;
    const modelStart = modelCalls.length;
    await mountTurn();
    assert.equal(mountCalls, 1, 'overgrant must not execute another mounted tool');
    assert.equal(requests.filter((r) => r.path === '/mcp').length, mountStart, 'refuse before remote MCP initialization/list/call');
    assert(modelCalls.length > modelStart, 'must reach the real model-loop preparation');
    assert(modelCalls.slice(modelStart).every((r) => !r.tools?.some((t) => t.includes('fixture_drive_lookup'))));
    assert.equal((await alice('me.google.status')).connection.status, 'needs_reconnect');
    passed(11, 'overgrant refuses signed google-token and remote Drive MCP mount after a successful control');

    assert.equal(requests.filter((r) => r.method === 'DELETE').length, 0, 'global DELETE count');
    assert.deepEqual(mockFailures, [], 'mock protocol failures');
    for (const message of wire) {
      assert(!/\bBearer\s+(?:ya29\.|eyJ|fixture-bearer-)|fixture-bearer-/i.test(message), 'MCP wire must contain no bearer');
      assert(issued.every((token) => !message.includes(token.access)), 'MCP wire contains an issued token');
    }
    assert(issued.some((t) => writeScopes.every((scope) => t.scopes.includes(scope))), 'must actually mint a write-only grant');
    save(join(home, 'mcp-wire.json'), wire);
    save(join(home, 'results.json'), results);
    save(join(home, 'google-requests.json'), requests);
    console.log('PASS global DELETE=0 and no bearer in any MCP wire message');
  });
}
