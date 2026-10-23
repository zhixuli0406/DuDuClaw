#!/usr/bin/env node
// Isolated B2a acceptance probe. Requires Node 22+ and a debug binary built
// with --features live-test-mocks. No model or real Google account is used.
import assert from 'node:assert/strict';
import { generateKeyPairSync, verify } from 'node:crypto';
import { mkdtempSync, readFileSync, writeFileSync, createWriteStream, unlinkSync } from 'node:fs';
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
const reads = [];
const mintHolds = new Map();
async function until(check, label) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    const value = await check();
    if (value) return value;
    await delay(50);
  }
  throw new Error(`Never reached actual wait point: ${label}`);
}
const mock = createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://127.0.0.1');
    res.setHeader('content-type', 'application/json');
    if (url.pathname === '/token') {
      let body = '';
      for await (const chunk of req) body += chunk;
      const form = new URLSearchParams(body);
      if (form.get('grant_type') === 'refresh_token') {
        const subject = form.get('refresh_token').replace('fixture-refresh:', '');
        const hold = mintHolds.get(subject);
        assert(hold, 'unexpected fixture refresh grant');
        hold.entered = true;
        await hold.wait;
        return res.end(JSON.stringify({ access_token: `fixture:${subject}`, expires_in: 3600, token_type: 'Bearer' }));
      }
      const assertion = form.get('assertion');
      const [head, payload, signature] = assertion.split('.');
      assert(verify('RSA-SHA256', Buffer.from(`${head}.${payload}`), publicKey, Buffer.from(signature, 'base64url')));
      const claims = JSON.parse(Buffer.from(payload, 'base64url'));
      seen.push(claims);
      const hold = mintHolds.get(claims.sub);
      if (hold) {
        hold.entered = true;
        await hold.wait;
      }
      if (claims.sub.startsWith('unauth')) {
        res.statusCode = 400;
        return res.end(JSON.stringify({ error: 'unauthorized_client' }));
      }
      return res.end(JSON.stringify({ access_token: `fixture:${claims.sub}`, expires_in: 3600, token_type: 'Bearer' }));
    }
    assert.equal(req.method, 'GET', 'the probe must never write to a Google service');
    const account = req.headers.authorization?.replace('Bearer fixture:', '') ?? '';
    reads.push({ account, path: url.pathname, query: url.search });
    if (url.pathname.endsWith('/profile')) return res.end(JSON.stringify({ emailAddress: account }));
    if (url.pathname.endsWith('/messages')) return res.end(JSON.stringify({ messages: [{ id: 'same-id', threadId: 'thread' }] }));
    if (url.pathname.endsWith('/messages/same-id')) return res.end(JSON.stringify({
      id: 'same-id', snippet: account, payload: {
        mimeType: 'text/plain', body: { data: Buffer.from(`message for ${account}`).toString('base64url') },
        headers: [{ name: 'From', value: 'sender@c.test' }, { name: 'To', value: account }, { name: 'Subject', value: account }],
      },
    }));
    if (url.pathname.endsWith('/drive/v3/files') && url.searchParams.has('q')) {
      if (account.startsWith('nosample')) return res.end(JSON.stringify({ files: [] }));
      const query = url.searchParams.get('q');
      const kind = query.includes('spreadsheet') ? 'sheets' : query.includes('document') ? 'docs' : query.includes('presentation') ? 'slides' : 'forms';
      return res.end(JSON.stringify({ files: [{ id: `fixture-${kind}` }] }));
    }
    for (const [resource, field, id] of [['spreadsheets', 'spreadsheetId', 'fixture-sheets'], ['documents', 'documentId', 'fixture-docs'],
      ['presentations', 'presentationId', 'fixture-slides'], ['forms', 'formId', 'fixture-forms']]) {
      if (url.pathname.endsWith(`/${resource}/${id}`)) return res.end(JSON.stringify({ [field]: id }));
    }
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
  async function rpc(method, params, expectedRpcError, allowRpcRefusal = false) {
    const key = ++id;
    const reply = new Promise((done) => waiting.set(key, done));
    child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: key, method, params }) + '\n');
    const result = await Promise.race([reply, delay(45000).then(() => { throw new Error(`MCP timeout: ${method}`); })]);
    waiting.delete(key);
    if (expectedRpcError !== undefined) {
      assert.equal(result.error?.code, expectedRpcError, `MCP ${method} expected refusal ${expectedRpcError}: ${JSON.stringify(result)}`);
      assert.equal(result.result, undefined, 'an expected refusal must not also contain a successful result');
      return { isError: true, expectedRpcError: result.error };
    }
    if (allowRpcRefusal && result.error) return { isError: true, rpcError: result.error };
    assert(!result.error, `MCP ${method} returned an error: ${JSON.stringify(result.error)}`);
    return result.result;
  }
  try {
    await rpc('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'google-delegate-live', version: '1' } });
    child.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n');
    const listed = await rpc('tools/list', {});
    const results = [];
    for (const tool of tools) {
      if (typeof tool === 'function') await tool();
      else {
        const pending = rpc('tools/call', { name: tool[0], arguments: tool[1] }, tool[2]?.expectedRpcError, tool[2]?.allowRpcRefusal);
        // Start the real call first; mutate only once its actual broker/provider
        // wait has been observed. Positive calls still reject every RPC error.
        if (tool[2]?.during) await tool[2].during();
        results.push(await pending);
      }
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
  const externalKey = `ddc_prod_${'b'.repeat(32)}`;
  const externalBlock = `\n[mcp_keys."${externalKey}"]\nclient_id = "prod-shaped"\nis_external = true\ncreated_at = "${new Date().toISOString()}"\nscopes = ["google:read"]\n`;
  assert(!readFileSync(configPath, 'utf8').includes(externalKey));
  writeFileSync(configPath, readFileSync(configPath, 'utf8') + externalBlock);
  const externalEnv = { DUDUCLAW_MCP_API_KEY: externalKey, DUDUCLAW_AGENT_ID: undefined, DUDUCLAW_AGENT_TOKEN: undefined,
    DUDUCLAW_TURN_ID: undefined, DUDUCLAW_SESSION_ID: undefined, DUDUCLAW_DISPATCH_RUN_ID: undefined };
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]], externalEnv);
  assert(!probe.results[0].isError, 'a dedicated external key with an explicitly allowed anyone policy must work');
  assert(probe.listed.includes('gmail_search'));
  assert(!probe.listed.includes('gmail_create_draft') && !probe.listed.includes('os_open'));
  assert(JSON.stringify(probe.results[0]).includes('alice@c.test'));
  assert(!JSON.stringify(probe.results[0]).includes('fixture:alice'), 'Google bearer must stay inside the gateway');
  console.log('PASS dedicated external read key executes as its employee without receiving a Google bearer');
  const plainServers = JSON.parse(readFileSync(join(home, 'agents/plain/.mcp.json'))).mcpServers;
  const plainEntry = plainServers.duduclaw ?? plainServers['duduclaw-pro'];
  assert.equal(plainEntry.env.DUDUCLAW_AGENT_ID, 'plain');
  assert(plainEntry.env.DUDUCLAW_AGENT_TOKEN);
  probe = await mcp('plain', [['gmail_search', { query: 'in:inbox' }]], {
    ...externalEnv, DUDUCLAW_AGENT_ID: 'plain',
    DUDUCLAW_AGENT_TOKEN: plainEntry.env.DUDUCLAW_AGENT_TOKEN,
  });
  assert(!probe.results[0].isError, 'same-call key authority must take precedence over a different valid host employee');
  assert(JSON.stringify(probe.results[0]).includes('alice@c.test'));
  assert(!JSON.stringify(probe.results[0]).includes('bob@c.test'));
  console.log('PASS dedicated key identity takes precedence over a different verified process identity');

  // Native race regressions: successful approval is not fresh identity proof;
  // successful mint is not fresh account authority. Every query is unique so
  // the mock API can prove that the refused call made no service request.
  const raceFailures = [];
  async function race(name, run) {
    try { await run(); console.log(`PASS RACE ${name}`); }
    catch (error) { raceFailures.push(error); console.error(`FAIL RACE ${name}: ${error.stack}`); }
  }
  const agentPath = join(home, 'agents/prod-shaped/agent.toml');
  for (const mutation of ['removed', 'malformed']) {
    await race(`P1 approval wait keeps dedicated A after A ${mutation}, ambient B valid`, async () => {
      const saved = readFileSync(agentPath, 'utf8');
      assert(saved.includes('approval_required_tools = ["Bash"]'));
      writeFileSync(agentPath, saved.replace('approval_required_tools = ["Bash"]', 'approval_required_tools = ["Bash", "gmail_search"]'));
      const query = `race-p1-${mutation}`;
      try {
        const result = await mcp('plain', [['gmail_search', { query }, {
          allowRpcRefusal: true,
          during: async () => {
            const approval = await until(async () => {
              const rows = await rpc('approvals.list', { agent_id: 'prod-shaped' });
              return rows.approvals.find((row) => row.status === 'pending' && JSON.stringify(row.payload).includes(query));
            }, `approval pending ${mutation}`);
            if (mutation === 'removed') unlinkSync(agentPath);
            else writeFileSync(agentPath, '[capabilities]\nallowed_tools = [');
            console.log(`OBSERVED actual approval pending for A; ${mutation} A before approving`);
            await rpc('approvals.decide', { id: approval.id, approve: true });
          },
        }]], { ...externalEnv, DUDUCLAW_AGENT_ID: 'plain', DUDUCLAW_AGENT_TOKEN: plainEntry.env.DUDUCLAW_AGENT_TOKEN });
        assert.equal(result.results[0].isError, true, `invalid A must refuse, never execute as ambient B: ${JSON.stringify(result.results)}`);
        assert(!JSON.stringify(result.results).includes('bob@c.test'), 'ambient B data escaped');
        assert(!reads.some((r) => r.query.includes(query)), 'a service effect happened after A lost its authority');
      } finally { writeFileSync(agentPath, saved); }
    });
  }
  for (const mutation of ['requesters', 'none', 'home', 'other-account', 'master', 'domain', 'person', 'key-scope', 'home-refresh', 'home-oauth-refresh', 'home-oauth-positive']) {
    await race(`P2 ${mutation} changes during real credential wait`, async () => {
      const subject = mutation === 'home-refresh' ? 'race-home@c.test' : `race-${mutation}@c.test`;
      const person = mutation === 'person' ? (await rpc('users.create', {
        email: subject, display_name: 'Race test person', password: 'fixture-person-only', role: 'employee',
      })).user : null;
      await delegate('prod-shaped', subject);
      const agentSaved = readFileSync(agentPath, 'utf8');
      const configSaved = readFileSync(configPath, 'utf8');
      const oauth = mutation.startsWith('home-oauth-');
      if (mutation === 'home-refresh' || oauth) {
        writeFileSync(agentPath, agentSaved.replace(`delegate:${subject}`, 'home'));
        assert(configSaved.includes('subject = "company@c.test"'));
        if (oauth) {
          const stripped = configSaved.replace(/\[integrations\.google_service_account\][\s\S]*?(?=\n\[)/, '');
          assert(!stripped.includes('[integrations.google_service_account]'));
          writeFileSync(configPath, stripped);
          writeFileSync(join(home, 'mcp-oauth-tokens.json'), JSON.stringify([{
            provider_id: 'google', access_token: 'fixture-expired', refresh_token: `fixture-refresh:${subject}`,
            expires_at: '2020-01-01T00:00:00Z', scopes: ['fixture'],
          }]), { mode: 0o600 });
          writeFileSync(join(home, 'mcp-oauth-configs.json'), JSON.stringify([{
            provider_id: 'google', client_id: 'fixture', client_secret: 'fixture-only', auth_url: `${mockBase}/authorize`,
            token_url: `${mockBase}/token`, redirect_uri: 'http://127.0.0.1/fixture', scopes: ['fixture'],
          }]), { mode: 0o600 });
        } else writeFileSync(configPath, configSaved.replace('subject = "company@c.test"', `subject = "${subject}"`));
      }
      let release;
      const hold = { entered: false, wait: new Promise((done) => { release = done; }) };
      mintHolds.set(subject, hold);
      const query = `race-p2-${mutation}`;
      try {
        const result = await mcp('prod-shaped', [['gmail_search', { query }, {
          allowRpcRefusal: mutation !== 'home-oauth-positive',
          during: async () => {
            try {
              await until(() => hold.entered, `token issuer ${mutation}`);
              console.log(`OBSERVED actual credential issuer pending; apply ${mutation}`);
              const agentCurrent = readFileSync(agentPath, 'utf8');
              const configCurrent = readFileSync(configPath, 'utf8');
              if (mutation === 'requesters') {
                assert(agentCurrent.includes('requesters = "anyone"'));
                writeFileSync(agentPath, agentCurrent.replace('requesters = "anyone"', 'requesters = "owner_only"'));
              } else if (['none', 'home', 'other-account'].includes(mutation)) {
                writeFileSync(agentPath, agentCurrent.replace(`delegate:${subject}`, mutation === 'other-account' ? 'delegate:bob@c.test' : mutation));
              } else if (mutation === 'master' || mutation === 'home-refresh' || mutation === 'home-oauth-refresh') {
                assert(configCurrent.includes('google_workspace = true'));
                writeFileSync(configPath, configCurrent.replace('google_workspace = true', 'google_workspace = false'));
              } else if (mutation === 'domain') {
                assert(configCurrent.includes('allowed_subject_domains = ["c.test"]'));
                writeFileSync(configPath, configCurrent.replace('allowed_subject_domains = ["c.test"]', 'allowed_subject_domains = ["other.test"]'));
              } else if (mutation === 'person') {
                await rpc('users.remove', { user_id: person.id });
              } else if (mutation === 'key-scope') {
                writeFileSync(configPath, configCurrent.replace('scopes = ["google:read"]', 'scopes = ["memory:read"]'));
              }
            } finally { release(); }
          },
        }]], externalEnv);
        if (mutation === 'home-oauth-positive') {
          assert(!result.results[0].isError, `unchanged home OAuth authority must survive a normal refresh: ${JSON.stringify(result.results)}`);
          assert(reads.some((r) => r.query.includes(query)), 'positive home refresh did not reach the service');
        } else {
          assert.equal(result.results[0].isError, true, `changed authority must refuse: ${JSON.stringify(result.results)}`);
          assert(!reads.some((r) => r.query.includes(query)), 'service API was called with authority that changed during mint');
        }
      } finally {
        release(); mintHolds.delete(subject);
        writeFileSync(agentPath, agentSaved); writeFileSync(configPath, configSaved);
        if (oauth) { unlinkSync(join(home, 'mcp-oauth-tokens.json')); unlinkSync(join(home, 'mcp-oauth-configs.json')); }
      }
    });
  }
  await delegate('prod-shaped', 'alice@c.test');
  writeFileSync(join(home, 'race-results.json'), JSON.stringify({ failed: raceFailures.length,
    failures: raceFailures.map((error) => error.message), reads, seen }, null, 2), { mode: 0o600 });
  if (raceFailures.length) throw new AggregateError(raceFailures, 'Native Google authority races failed');
  if (!process.argv.includes('--races-only')) {
  probe = await mcp('prod-shaped', [['gmail_create_draft', { to: 'fixture@c.test', subject: 'fixture', body: 'fixture' }, { expectedRpcError: -32601 }]], externalEnv);
  assert(probe.results[0].isError, 'a read key must never execute a Google write');
  await delegate('prod-shaped', 'alice@c.test', { requesters: 'owner_only' });
  const beforeExternalRefusal = seen.length;
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]], externalEnv);
  assert(probe.results[0].isError, 'external keys are Unknown requesters, never the owner');
  assert.equal(seen.length, beforeExternalRefusal);
  await delegate('prod-shaped', 'alice@c.test');
  console.log('PASS external read scope and requester restrictions remain independent');
  const services = await rpc('google.identity.test', { agent_id: 'prod-shaped' });
  assert(services.ok, JSON.stringify(services.steps));
  for (const resource of ['spreadsheets/fixture-sheets', 'documents/fixture-docs', 'presentations/fixture-slides', 'forms/fixture-forms']) {
    assert(reads.some((r) => r.account === 'alice@c.test' && r.path.endsWith(`/${resource}`)), `service was only minted, not read: ${resource}`);
  }
  console.log('PASS Sheets/Docs/Slides/Forms read actual service endpoints');
  await delegate('prod-shaped', 'nosample@c.test');
  const noSample = await rpc('google.identity.test', { agent_id: 'prod-shaped' });
  for (const id of ['sheets', 'docs', 'slides', 'forms']) {
    assert.equal(noSample.steps.find((s) => s.id === id)?.ok, false, `${id} without a sample must stay unverified`);
  }
  await delegate('prod-shaped', 'alice@c.test');
  console.log('PASS minting without a sample does not prove data access');
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
  assert(audit.includes('google_execute'), 'dedicated execution must leave its host audit row');
  assert(!audit.includes('fixture:alice') && !audit.includes('PRIVATE KEY'));
  console.log('PASS audit records identity without bearer or private key');
  await rpc('google.integration.set', { enabled: true });
  gateway.kill('SIGTERM');
  await Promise.race([once(gateway, 'exit'), delay(10000)]);
  probe = await mcp('prod-shaped', [['gmail_search', { query: 'in:inbox' }]]);
  assert(probe.results[0].isError);
  console.log('PASS delegate refuses when gateway is offline');
  }
  console.log(`ARTIFACTS ${home}`);
} finally {
  socket?.close(); gateway.kill('SIGTERM'); mock.close(); log.end();
}
