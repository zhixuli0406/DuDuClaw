#!/usr/bin/env node
// Ordinary integration baseline: fresh local accounts, synthetic RSA issuer,
// loopback REST/MCP/model servers. Never a real provider/tenant acceptance run.
import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, symlinkSync, createWriteStream, existsSync, realpathSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { minimalEnv, deadline, withOwnedResources, closeServer } from './google-connectors-harness.mjs';

async function runBaseline(owned) {
const checks = [];
const binary = resolve(process.argv[2] ?? 'target/debug/duduclaw');
const fixtureRoot = realpathSync(mkdtempSync(join(tmpdir(), 'ddc-google-ext-baseline-')));
const home = join(fixtureRoot, 'installation');
const setupHome = join(fixtureRoot, 'setup-os-home');
mkdirSync(setupHome);
console.log(`ARTIFACTS ${fixtureRoot}`);
const instanceName = `fixture-${fixtureRoot.split('/').at(-1)}`;
const sandbox = '/usr/bin/sandbox-exec';
assert(process.platform === 'darwin' && existsSync(sandbox), 'this baseline requires the macOS loopback-only process sandbox');
const networkPolicy = '(version 1)(allow default)(deny network*)(allow network-bind (local ip "localhost:*"))(allow network-inbound (local ip "localhost:*"))(allow network-outbound (remote ip "localhost:*"))';
const scriptDir = dirname(fileURLToPath(import.meta.url));
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
async function until(check, label) {
  const end = Date.now() + 45000;
  while (Date.now() < end) { const value = await check(); if (value) return value; await pause(100); }
  throw new Error(`${label} was not observed`);
}
const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
const requests = [], issued = [], mounts = [], modelCalls = [];
const json = (res, value, status = 200) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
async function bodyOf(req) { let text = ''; for await (const chunk of req) text += chunk; return text; }
const mock = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://127.0.0.1');
    if (url.pathname === '/token') {
      const assertion = new URLSearchParams(await bodyOf(req)).get('assertion');
      const [head, payload, signature] = assertion.split('.');
      assert(verify('RSA-SHA256', Buffer.from(`${head}.${payload}`), publicKey, Buffer.from(signature, 'base64url')));
      const claims = JSON.parse(Buffer.from(payload, 'base64url')); issued.push({ subject: claims.sub, scope: claims.scope });
      if (claims.sub === 'unavailable@c.test') return json(res, { error: 'unauthorized_client' }, 400);
      return json(res, { access_token: `fixture:${claims.sub}`, expires_in: 3600, token_type: 'Bearer' });
    }
    if (['/mcp', '/mcp-home'].includes(url.pathname)) {
      assert.equal(req.method, 'POST');
      const account = url.pathname === '/mcp-home' ? 'company@c.test' : 'alice@c.test';
      assert.equal(req.headers.authorization, `Bearer fixture:${account}`);
      const call = JSON.parse(await bodyOf(req));
      if (call.id === undefined) { res.writeHead(202); return res.end(); }
      let result;
      if (call.method === 'initialize') result = { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'synthetic-google', version: '1' } };
      else if (call.method === 'tools/list') result = { tools: [{ name: 'fixture_google_lookup', description: 'Read a synthetic local record', inputSchema: { type: 'object', properties: { query: { type: 'string' } }, required: ['query'] } }] };
      else if (call.method === 'tools/call') {
        assert.equal(call.params.name, 'fixture_google_lookup');
        mounts.push({ account, query: call.params.arguments.query });
        result = { content: [{ type: 'text', text: JSON.stringify({ fixture: 'mount', account, query: call.params.arguments.query }) }] };
      } else throw new Error(`Unexpected MCP method ${call.method}`);
      return json(res, { jsonrpc: '2.0', id: call.id, result });
    }
    if (url.pathname === '/model/v1/chat/completions') {
      assert.equal(req.headers.authorization, 'Bearer fixture-model-only');
      const call = JSON.parse(await bodyOf(req));
      const tool = call.tools?.find((entry) => entry.function?.name.includes('fixture_google_lookup'));
      const result = call.messages?.filter((message) => message.role === 'tool').at(-1);
      modelCalls.push({ tools: call.tools?.map((entry) => entry.function?.name), saw_tool_result: Boolean(result), tool_result: result?.content });
      const invoke = tool && !result;
      const message = invoke
        ? { role: 'assistant', content: null, tool_calls: [{ id: 'fixture-call', type: 'function', function: { name: tool.function.name, arguments: JSON.stringify({ query: 'mount-native-fixture' }) } }] }
        : { role: 'assistant', content: `Local fixture completed. ${result?.content ?? 'No fixture tool result.'}` };
      const finish = invoke ? 'tool_calls' : 'stop';
      if (call.stream) {
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        const delta = invoke ? { role: 'assistant', tool_calls: message.tool_calls.map((entry) => ({ index: 0, ...entry })) } : message;
        for (const item of [{ choices: [{ index: 0, delta, finish_reason: null }] }, { choices: [{ index: 0, delta: {}, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 5 } }]) res.write(`data: ${JSON.stringify({ id: 'fixture-model', object: 'chat.completion.chunk', model: 'gpt-4o-mini', ...item })}\n\n`);
        return res.end('data: [DONE]\n\n');
      }
      return json(res, { id: 'fixture-model', object: 'chat.completion', model: 'gpt-4o-mini', choices: [{ index: 0, message, finish_reason: finish }], usage: { prompt_tokens: 10, completion_tokens: 5 } });
    }
    const account = req.headers.authorization?.replace('Bearer fixture:', '');
    assert(['alice@c.test', 'company@c.test'].includes(account), 'unexpected synthetic account');
    const text = await bodyOf(req);
    const body = text ? JSON.parse(text) : null;
    requests.push({ method: req.method, account, path: url.pathname, query: url.search, body });
    const path = url.pathname;
    if (path.endsWith('/profile')) return json(res, { emailAddress: account });
    if (path.endsWith('/messages')) return json(res, { messages: [{ id: 'same-id', threadId: 'thread' }] });
    if (path.endsWith('/messages/same-id')) return json(res, { id: 'same-id', snippet: account, payload: { mimeType: 'text/plain', body: { data: Buffer.from(`fixture mail ${account}`).toString('base64url') }, headers: [{ name: 'From', value: 'sender@c.test' }, { name: 'Subject', value: `fixture mail ${account}` }] } });
    if (path.endsWith('/drafts') && req.method === 'POST') return json(res, { id: 'fixture-draft', message: { id: 'fixture-message' } });
    if (path.endsWith('/events')) return json(res, req.method === 'POST' ? { id: 'fixture-event', htmlLink: 'https://example.test/fixture-event', ...body } : { items: [{ id: 'fixture-event', summary: 'fixture event', start: { dateTime: '2026-10-24T01:00:00Z' }, end: { dateTime: '2026-10-24T02:00:00Z' } }] });
    if (path.includes('/values/')) return json(res, req.method === 'POST' ? { updates: { updatedRange: 'Sheet1!A2:B2', updatedRows: 1, updatedCells: 2 } } : { range: 'Sheet1!A1:B1', values: [['fixture sheet', account]] });
    if (path.endsWith('/users/@me/lists')) return json(res, { items: [{ id: 'fixture-list', title: 'fixture list' }] });
    if (path.includes('/tasks/v1/lists/')) {
      const task = { id: 'fixture-task', title: 'fixture task', status: req.method === 'PATCH' ? 'completed' : 'needsAction', ...body };
      return json(res, req.method === 'GET' ? { items: [task] } : task);
    }
    if (path.endsWith('/drive/v3/files') && req.method === 'GET') {
      const query = url.searchParams.get('q') ?? '';
      const kind = query.includes('spreadsheet') ? 'sheets' : query.includes('document') ? 'docs' : query.includes('presentation') ? 'slides' : query.includes('form') ? 'forms' : 'drive';
      return json(res, { files: [{ id: `fixture-${kind}`, name: 'fixture file', mimeType: 'text/plain' }] });
    }
    if (path.endsWith('/files/fixture-drive')) {
      if (url.searchParams.get('alt') === 'media') { res.writeHead(200, { 'content-type': 'text/plain' }); return res.end('fixture drive content'); }
      return json(res, { id: 'fixture-drive', name: 'fixture file', mimeType: 'text/plain' });
    }
    if (path.endsWith('/forms/fixture-forms/responses')) return json(res, { responses: [{ responseId: 'fixture-response', answers: { 'fixture-question': { textAnswers: { answers: [{ value: 'fixture answer' }] } } } }] });
    if (path.endsWith('/forms/fixture-forms')) return json(res, { formId: 'fixture-forms', info: { title: 'fixture form' }, items: [{ title: 'fixture question', questionItem: { question: { questionId: 'fixture-question', textQuestion: {} } } }] });
    if (path.endsWith('/spreadsheets/fixture-sheets')) return json(res, { spreadsheetId: 'fixture-sheets', properties: { title: 'fixture sheet' } });
    if (path.endsWith('/documents/fixture-docs:batchUpdate')) return json(res, { documentId: 'fixture-docs', replies: [] });
    if (path.endsWith('/documents/fixture-docs')) return json(res, { documentId: 'fixture-docs', title: 'fixture doc', body: { content: [{ paragraph: { elements: [{ textRun: { content: 'fixture doc text' } }] } }] } });
    if (path.endsWith('/presentations/fixture-slides')) return json(res, { presentationId: 'fixture-slides', title: 'fixture slides', slides: [{ objectId: 'fixture-slide', pageElements: [{ shape: { text: { textElements: [{ textRun: { content: 'fixture slide text' } }] } } }] }] });
    throw new Error(`Unexpected local fixture request: ${req.method} ${path}`);
  } catch (error) { console.error(`MOCK FAIL ${error.message}`); json(res, { error: error.message }, 500); }
});
owned.defer(() => closeServer(mock));
mock.listen(0, '127.0.0.1'); await deadline(once(mock, 'listening'), 5000, 'local mock listening');
const base = `http://127.0.0.1:${mock.address().port}`;
const lease = createServer(); owned.defer(() => closeServer(lease));
lease.listen(0, '127.0.0.1'); await deadline(once(lease, 'listening'), 5000, 'port lease listening');
const port = lease.address().port; await closeServer(lease);
const setupEnv = minimalEnv(setupHome, '/usr/bin:/bin');
const made = owned.spawn('/bin/bash', [join(scriptDir, 'make-home.sh'), home, '--port', String(port)], setupEnv, { stdio: ['ignore', 'pipe', 'pipe'] });
let setupError = ''; made.child.stdout.resume(); made.child.stderr.on('data', (data) => { setupError = (setupError + data).slice(-64000); });
try { await owned.success(made, 45000, 'isolated home setup'); } catch (error) { throw new Error(`isolated home setup failed: ${setupError}`, { cause: error }); }
const key = JSON.stringify({ type: 'service_account', client_email: 'fixture@project.iam.gserviceaccount.test', client_id: '1234567890', private_key: privateKey.export({ format: 'pem', type: 'pkcs8' }), token_uri: `${base}/token` });
writeFileSync(join(home, 'home-key.json'), key, { mode: 0o600 });
const configPath = join(home, 'config.toml');
writeFileSync(configPath, readFileSync(configPath, 'utf8').replace('[general]', `[general]\nname = "${instanceName}"`) + `
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
[mail]
enabled = true
gmail_enabled = true
dropfolder_enabled = false
auto_trigger = false
poll_interval_secs = 30
default_agent = "mail-off"
[[accounts]]
provider = "openai"
api_key = "fixture-model-only"
base_url = "${base}/model/v1"
`);
const plain = readFileSync(join(home, 'agents/plain/agent.toml'), 'utf8');
for (const agent of ['mail-off', 'mount-native', 'mount-fallback']) {
  mkdirSync(join(home, 'agents', agent));
  let contents = plain.replaceAll('plain', agent);
  if (agent === 'mail-off') contents += '\n[google]\nidentity="none"\n';
  else contents = contents.replace('provider = "claude"', 'provider = "openai_compat"').replaceAll('claude-haiku-4-5', 'gpt-4o-mini') + `\n[[mcp.external]]\nname="synthetic-google"\nurl="${base}/${agent === 'mount-fallback' ? 'mcp-home' : 'mcp'}"\nbearer_token="oauth://google"\n`;
  writeFileSync(join(home, 'agents', agent, 'agent.toml'), contents);
  writeFileSync(join(home, 'agents', agent, 'SOUL.md'), '# Local synthetic validation employee\nUse only the fixture tool requested in this temporary installation.\n');
}
mkdirSync(join(home, 'bin')); symlinkSync(binary, join(home, 'bin/duduclaw'));
// Only this binary is executable through PATH; no host model CLI or credentials
// can be used by an accidental fallback. The HTTP model account is synthetic.
const fixtureVars = { DUDUCLAW_HOME: home, DUDUCLAW_BIN: binary, DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE: base };
const env = minimalEnv(join(home, 'os-home'), join(home, 'bin'), fixtureVars);
const confinement = owned.spawn(sandbox, ['-p', networkPolicy, process.execPath, '-e',
  "const s=require('node:net').connect({host:'192.0.2.1',port:9});s.on('error',e=>process.exit(e.code==='EPERM'?0:1));setTimeout(()=>process.exit(2),2000).unref();"], env, { stdio: 'ignore' });
await owned.success(confinement, 5000, 'loopback-only network confinement');
const gatewayProcess = owned.spawn(sandbox, ['-p', networkPolicy, binary, 'run', '--yes'], env, { stdio: ['ignore', 'pipe', 'pipe'] });
const gateway = gatewayProcess.child;
const log = createWriteStream(join(home, 'gateway.log'), { mode: 0o600 });
let logError; log.on('error', (error) => { logError = error; });
owned.defer(async () => {
  if (logError) throw logError;
  if (!log.writableFinished) { const finished = once(log, 'finish'); log.end(); await deadline(finished, 5000, 'gateway log flush'); }
  if (logError) throw logError;
});
gateway.stdout.pipe(log, { end: false }); gateway.stderr.pipe(log, { end: false });
function ownSocket(url) {
  const connection = new WebSocket(url);
  owned.defer(async () => {
    if (connection.readyState === WebSocket.CLOSED) return;
    const closed = once(connection, 'close'); connection.close();
    await deadline(closed, 3000, 'fixture WebSocket cleanup');
  }, { beforeChildren: true });
  return connection;
}
let socket, chat;
const apiBase = `http://127.0.0.1:${port}`;
async function post(path, body) {
  const response = await fetch(`${apiBase}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  assert(response.ok, `${path} HTTP ${response.status}`); return response.json();
}
let rpc;
async function mcp(agent, tools, overrides = {}) {
  const servers = JSON.parse(readFileSync(join(home, 'agents', agent, '.mcp.json'))).mcpServers;
  const entry = servers.duduclaw ?? servers['duduclaw-pro'];
  // Select only the three platform identity fields from this fresh install.
  const identity = Object.fromEntries(['DUDUCLAW_MCP_API_KEY', 'DUDUCLAW_AGENT_ID', 'DUDUCLAW_AGENT_TOKEN'].map((name) => [name, entry.env?.[name]]));
  const childEnv = minimalEnv(env.HOME, env.PATH, { ...fixtureVars, ...identity, ...overrides });
  const record = owned.spawn(sandbox, ['-p', networkPolicy, binary, ...(entry.args ?? ['mcp-server'])], childEnv, { stdio: ['pipe', 'pipe', 'ignore'] });
  const child = record.child;
  const waiters = new Map(); let id = 0;
  const rejectAll = (error) => { for (const waiter of waiters.values()) waiter.reject(error); };
  child.stdin.on('error', rejectAll);
  record.completion.then(({ code, signal, error }) => rejectAll(error ?? new Error(`MCP child closed: ${code ?? signal}`)));
  const lines = createInterface({ input: child.stdout });
  lines.on('error', rejectAll);
  lines.on('line', (line) => { try { const value = JSON.parse(line); waiters.get(value.id)?.resolve(value); } catch (error) { rejectAll(error); } });
  async function call(method, params, errorCode) {
    const request = ++id; const pending = new Promise((resolve, reject) => waiters.set(request, { resolve, reject }));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: request, method, params }) + '\n', (error) => { if (error) waiters.get(request)?.reject(error); });
    let value;
    try { value = await deadline(pending, 45000, `MCP ${method}`); } finally { waiters.delete(request); }
    if (errorCode !== undefined) { assert.equal(value.error?.code, errorCode); assert.equal(value.result, undefined); return { isError: true }; }
    assert(!value.error, JSON.stringify(value.error)); return value.result;
  }
  try {
    await call('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'local-google-baseline', version: '1' } });
    child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
    const listed = (await call('tools/list', {})).tools.map((tool) => tool.name); const results = [];
    for (const [name, args, options = {}] of tools) {
      const pending = call('tools/call', { name, arguments: args }, options.errorCode);
      // An approval failure may abort before awaiting this call; cleanup then
      // rejects it. Keep it handled while still propagating it at await below.
      pending.catch(() => {});
      if (options.approve) {
        const approval = await until(async () => (await rpc('approvals.list', { agent_id: agent })).approvals.find((row) => row.status === 'pending' && JSON.stringify(row.payload).includes(name)), `normal approval ${name}`);
        await rpc('approvals.decide', { id: approval.id, approve: true });
      }
      results.push(await pending);
    }
    return { listed, results };
  } finally { lines.close(); child.stdin.end(); await owned.stop(record); }
}
  await until(async () => { if (gatewayProcess.closed || gateway.exitCode !== null || gateway.signalCode !== null) throw new Error(`gateway exited ${gateway.exitCode}`); try { return (await fetch(`${apiBase}/health`)).ok; } catch { return false; } }, 'gateway health');
  const health = await (await fetch(`${apiBase}/healthz`)).json();
  assert.equal(health.name, instanceName, 'refusing a listener belonging to another installation');
  assert.equal(health.live_test_mocks, true, 'mock credential-store feature required');
  const login = { email: 'owner@c.test', password: 'fixture-password-only' };
  await post('/api/first-run/claim', login); const tokens = await post('/api/login', login);
  socket = ownSocket(`ws://127.0.0.1:${port}/ws`); await once(socket, 'open');
  let id = 0; const waiters = new Map();
  socket.addEventListener('message', (event) => { const value = JSON.parse(event.data); if (value.type === 'res') waiters.get(value.id)?.(value); });
  rpc = async (method, params = {}) => {
    const request = String(++id); const pending = new Promise((done) => waiters.set(request, done)); socket.send(JSON.stringify({ type: 'req', id: request, method, params }));
    const value = await deadline(pending, 45000, method); waiters.delete(request); assert(value.ok, `${method}: ${JSON.stringify(value.error)}`); return value.payload;
  };
  await rpc('connect', { jwt: tokens.access_token });
  const mail = await until(async () => { const status = await rpc('mail.status'); return status.last_poll?.account_unavailable > 0 && status; }, 'completed unavailable mailbox poll');
  assert.equal(mail.last_poll.gmail_attempted, true); assert.equal(mail.last_poll_error, false);
  assert.equal((await rpc('mail.list')).count, 0);
  checks.push('mail.status distinguishes unavailable Google account from empty mailbox');
  let probe = await mcp('prod-shaped', [['gmail_search', { query: 'fixture inbox' }]]);
  assert(!probe.results[0].isError); assert(JSON.stringify(probe.results).includes('company@c.test'));
  checks.push('existing Home account path');
  await rpc('security.credential_protection_enable'); await rpc('google.delegation.import_key', { key_json: key });
  for (const [agent, email] of [['prod-shaped', 'alice@c.test'], ['mount-native', 'alice@c.test']]) {
    await rpc('agents.google_identity_set', { agent_id: agent, identity: `delegate:${email}`, requesters: 'anyone', confirm_anyone: true });
  }
  await rpc('agents.google_identity_set', { agent_id: 'mount-fallback', identity: 'delegate:unavailable@c.test', fallback: 'home', requesters: 'anyone', confirm_anyone: true });
  const external = `ddc_prod_${'c'.repeat(32)}`;
  writeFileSync(configPath, readFileSync(configPath, 'utf8') + `\n[mcp_keys.${external}]\nclient_id="prod-shaped"\nscopes=["google:read","google:write"]\nis_external=true\ncreated_at="${new Date().toISOString()}"\n`);
  const writes = ['gmail_create_draft', 'calendar_create_event', 'sheets_append', 'gtasks_create', 'gtasks_complete', 'docs_append'];
  const agentPath = join(home, 'agents/prod-shaped/agent.toml'); const agentConfig = readFileSync(agentPath, 'utf8');
  assert(agentConfig.includes('approval_required_tools = ["Bash"]'));
  writeFileSync(agentPath, agentConfig.replace('approval_required_tools = ["Bash"]', `approval_required_tools = ${JSON.stringify(['Bash', ...writes])}`));
  const tools = [
    ['google_status', {}], ['gmail_search', { query: 'fixture inbox' }], ['gmail_read', { message_id: 'same-id' }],
    ['gmail_create_draft', { to: 'recipient@c.test', subject: 'fixture draft', body: 'fixture only' }], ['calendar_list_events', {}],
    ['calendar_create_event', { summary: 'fixture event', start: '2026-10-24T01:00:00Z', end: '2026-10-24T02:00:00Z' }],
    ['sheets_read', { spreadsheet_id: 'fixture-sheets', range: 'Sheet1!A1:B1' }], ['sheets_append', { spreadsheet_id: 'fixture-sheets', range: 'Sheet1!A:B', values: ['fixture', 'row'] }],
    ['forms_get', { form_id: 'fixture-forms' }], ['forms_list_responses', { form_id: 'fixture-forms' }], ['gtasks_lists', {}], ['gtasks_list', { task_list_id: 'fixture-list' }],
    ['gtasks_create', { task_list_id: 'fixture-list', title: 'fixture task' }], ['gtasks_complete', { task_list_id: 'fixture-list', task_id: 'fixture-task' }],
    ['drive_search', { query: 'fixture' }], ['drive_read', { file_id: 'fixture-drive' }], ['docs_read', { document_id: 'fixture-docs' }], ['docs_append', { document_id: 'fixture-docs', text: 'fixture append' }], ['slides_read', { presentation_id: 'fixture-slides' }],
  ];
  const canonical = readFileSync(join(scriptDir, '../../crates/duduclaw-core/src/google_identity/tools.rs'), 'utf8').split('pub const NATIVE_GOOGLE_TOOLS')[1].split('];')[0].match(/"([a-z_]+)"/g).map((name) => JSON.parse(name));
  assert.deepEqual(tools.map(([name]) => name).sort(), canonical.sort()); assert.equal(tools.length, 19);
  const externalEnv = { DUDUCLAW_MCP_API_KEY: external, DUDUCLAW_AGENT_ID: undefined, DUDUCLAW_AGENT_TOKEN: undefined };
  probe = await mcp('prod-shaped', tools.map(([name, args]) => [name, args, { approve: writes.includes(name) }]), externalEnv);
  for (let index = 0; index < tools.length; index++) { assert(probe.listed.includes(tools[index][0])); assert(!probe.results[index].isError, `${tools[index][0]}: ${JSON.stringify(probe.results[index])}`); }
  assert(!JSON.stringify(probe.results).includes('fixture:alice'));
  assert.equal(requests.filter((request) => ['POST', 'PATCH'].includes(request.method)).length, writes.length);
  checks.push('canonical 19 dedicated Google tools, normal write approvals and gateway-only bearer');
  const serviceStart = requests.length;
  const service = await rpc('google.identity.test', { agent_id: 'prod-shaped' }); assert(service.ok, JSON.stringify(service.steps));
  for (const endpoint of ['spreadsheets/fixture-sheets', 'documents/fixture-docs', 'presentations/fixture-slides', 'forms/fixture-forms']) assert(requests.slice(serviceStart).some((request) => request.method === 'GET' && request.account === 'alice@c.test' && request.path.endsWith(endpoint)), endpoint);
  checks.push('service probes perform actual Sheets/Docs/Slides/Forms GET requests');
  for (const agent of ['mount-native', 'mount-fallback']) {
    const mountStart = mounts.length, modelStart = modelCalls.length;
    chat = ownSocket(`ws://127.0.0.1:${port}/ws/chat`); await once(chat, 'open');
    const done = new Promise((resolveChat, rejectChat) => chat.addEventListener('message', (event) => { const frame = JSON.parse(event.data); if (frame.type === 'assistant_done') resolveChat(frame); if (frame.type === 'error') rejectChat(new Error(JSON.stringify(frame))); }));
    chat.send(JSON.stringify({ type: 'auth', token: tokens.access_token })); await pause(150);
    chat.send(JSON.stringify({ type: 'user_message', agent, content: 'Use fixture_google_lookup once for the local fixture check.', conv: `fixture-${agent}` }));
    const answer = await deadline(done, 60000, `mounted MCP tool loop ${agent}`);
    const account = agent === 'mount-native' ? 'alice@c.test' : 'company@c.test';
    const currentMounts = mounts.slice(mountStart), currentModels = modelCalls.slice(modelStart);
    assert.equal(currentMounts.length, 1);
    assert(currentMounts.every((call) => call.account === account && call.query === 'mount-native-fixture'));
    assert(currentModels.some((call) => call.saw_tool_result));
    assert(answer.content.includes('fixture')); assert(!JSON.stringify(answer).includes('fixture:alice'));
    if (agent === 'mount-fallback') {
      assert(currentModels.some((call) => call.tool_result?.includes('已改用公司共用帳號')), 'actual fallback notice must reach the model tool result');
      assert(answer.content.includes('已改用公司共用帳號'), 'actual fallback notice must reach the reply');
    } else {
      assert(currentModels.every((call) => !call.tool_result?.includes('已改用公司共用帳號')));
      assert(!answer.content.includes('已改用公司共用帳號'));
    }
    chat.close(); chat = undefined;
  }
  checks.push('gateway model tool loop uses the person mount, and explicit Home fallback has its per-call notice');
  const audit = readFileSync(join(home, 'tool_calls.jsonl'), 'utf8'); assert(audit.includes('google_execute'));
  assert(!audit.includes('fixture:alice') && !audit.includes('PRIVATE KEY'));
  checks.push('baseline audit has no Google bearer or private key');
  return { home, checks, report: { boundary: 'synthetic-local-only', tools: tools.map(([name]) => name), mail_status: mail, requests, issued, mounts, modelCalls } };
}

try {
  // No success receipt or PASS output can occur until every owned child,
  // process group, socket, server and log has completed cleanup.
  const result = await withOwnedResources(runBaseline);
  writeFileSync(join(result.home, 'baseline-results.json'), JSON.stringify({ ...result.report, cleanup_confirmed: true }, null, 2), { mode: 0o600 });
  for (const check of result.checks) console.log(`PASS ${check}`);
} catch (error) {
  console.error('FAIL local Google baseline:', error);
  process.exitCode = 1;
}
