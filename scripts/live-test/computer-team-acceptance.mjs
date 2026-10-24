#!/usr/bin/env node
// Operates only the private fresh home created by computer-team-dashboard.mjs.
// No provider account or external destination is used.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { fixtureControlSignature } from './computer-team-signature.mjs';
import { existsSync, readFileSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { join, basename } from 'node:path';

const root = realpathSync(process.argv[2]);
assert(basename(root).startsWith('dudu-team-native-') && (statSync(root).mode & 0o777) === 0o700, 'pass the private fixture root');
const home = join(root, 'data');
const phase = process.argv[3];
assert(['baseline', 'watch-context', 'off', 'restore', 'sid-fence', 'finish', 'abort'].includes(phase), 'select an explicit fixture phase');
const ready = JSON.parse(readFileSync(join(home, 'team-ui-ready.json'), 'utf8'));
assert(ready.status === 'READY' && Number.isInteger(ready.port) && ready.port > 0 && ready.port < 65536);
const base = `http://127.0.0.1:${ready.port}`;
const sockets = [];
const cases = [];
const check = (condition, label) => { assert(condition, label); cases.push(label); };
const resultPath = join(home, `api-${phase}.json`);
assert(!existsSync(resultPath), 'fixture phase already recorded; preserve its evidence');
const save = (name, value) => writeFileSync(join(home, name), JSON.stringify(value, null, 2), { mode: 0o600 });

async function login(email) {
  const response = await fetch(`${base}/api/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email, password: ready.password }), signal: AbortSignal.timeout(10_000) });
  assert(response.ok, `fixture login HTTP ${response.status}`);
  const json = await response.json();
  assert(typeof json.access_token === 'string');
  return json.access_token;
}
async function connect(email) {
  const jwt = await login(email);
  const ws = new WebSocket(`ws://127.0.0.1:${ready.port}/ws`, { headers: { Origin: base } });
  sockets.push(ws);
  await new Promise((done, fail) => {
    const timer = setTimeout(() => fail(new Error('fixture socket open timeout')), 10_000);
    ws.addEventListener('open', () => { clearTimeout(timer); done(); }, { once: true });
    ws.addEventListener('error', () => { clearTimeout(timer); fail(new Error('fixture socket open error')); }, { once: true });
  });
  let sequence = 0;
  const call = (method, params = {}) => new Promise((done, fail) => {
    const id = String(++sequence);
    const finish = (error, value) => {
      clearTimeout(timer); ws.removeEventListener('message', receive); ws.removeEventListener('close', closed);
      if (error) fail(error); else done(value);
    };
    const timer = setTimeout(() => finish(new Error(`fixture RPC timeout: ${method}`)), 25_000);
    const closed = () => finish(new Error(`fixture socket closed: ${method}`));
    function receive(event) {
      let value;
      try { value = JSON.parse(event.data); } catch { return; }
      if (value.id !== id) return;
      finish(null, value);
    }
    ws.addEventListener('message', receive);
    ws.addEventListener('close', closed, { once: true });
    try { ws.send(JSON.stringify({ type: 'req', id, method, params })); } catch (error) { finish(error); }
  });
  assert((await call('connect', { jwt })).ok, 'fixture socket authentication');
  return call;
}
async function startReplacement() {
  assert(typeof ready.gateway_internal_test_key === 'string');
  const identity = readFileSync(join(home, 'identity.key'));
  const agent = 'alice';
  const timestamp = String(Math.floor(Date.now() / 1000));
  const nonce = randomBytes(16).toString('hex');
  const body = JSON.stringify({ op: 'start', task: 'Synthetic replacement-session acceptance' });
  const signature = fixtureControlSignature({ key: ready.gateway_internal_test_key, identity, agent, body, timestamp, nonce });
  const response = await fetch(`${base}/api/internal/computer-use`, { method: 'POST', body, headers: { 'content-type': 'application/json', 'X-Duduclaw-Agent-Id': agent, 'X-Duduclaw-Timestamp': timestamp, 'X-Duduclaw-Nonce': nonce, 'X-Duduclaw-Signature': signature }, signal: AbortSignal.timeout(430_000) });
  const value = await response.json();
  assert(response.ok && value.ok && typeof value.session_id === 'string', 'fixture replacement start');
  return value.session_id;
}

let failure;
try {
  if (phase === 'abort') save('team-ui-finish.json', { status: 'FAIL', reason: 'explicit fixture abort' });
  else if (phase === 'finish') {
    for (const prior of ['baseline', 'watch-context', 'off', 'restore', 'sid-fence']) check(JSON.parse(readFileSync(join(home, `api-${prior}.json`), 'utf8')).status === 'PASS', `${prior} evidence present`);
    const ui = JSON.parse(readFileSync(join(home, 'team-ui-review.json'), 'utf8'));
    check(ui.status === 'PASS' && ui.revocation_cleared === true, 'actual dashboard review recorded');
    save('team-ui-finish.json', { status: 'PASS', cases, ui, boundary: 'controlled synthetic gateway/container/UI; no channel/model/provider or physical owner acceptance' });
  } else {
    const manager = await connect(ready.manager_email);
    const admin = await connect(ready.admin_email);
    if (phase === 'baseline') {
      const status = await manager('computer_sessions.status', { agent_id: 'alice' });
      check(status.ok && status.payload.active && status.payload.view_mode === 'masked', 'team-only manager sees masked status');
      const image = await manager('computer_sessions.snapshot', { agent_id: 'alice', session_id: ready.session_id });
      check(image.ok && Object.keys(image.payload).length === 9, 'closed screenshot response');
      const png = Buffer.from(image.payload.png_base64, 'base64');
      check(png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])), 'real masked PNG');
      writeFileSync(join(home, 'team-masked-pipeline.png'), png, { mode: 0o600 });
      for (const method of ['computer_sessions.view', 'computer_sessions.takeover', 'computer_sessions.hand_back', 'computer_sessions.resume', 'computer_profiles.status', 'computer_profiles.reset', 'agents.get']) {
        check(!(await manager(method, { agent_id: 'alice', name: 'alice', confirm: true })).ok, `${method} denied for team-only viewer`);
      }
      check(!(await manager('computer_sessions.snapshot', { agent_id: 'alice', session_id: 'cu-expired' })).ok, 'snapshot cannot silently switch session');
    } else if (phase === 'watch-context') {
      const opened = await manager('computer_sessions.open_link', { token: ready.watch_token });
      check(opened.ok && opened.payload.agent_id === 'alice' && opened.payload.session_id === ready.session_id, 'valid context opens only the signed live session');
      check(Object.keys(opened.payload).sort().join(',') === 'agent_id,expires_at,ok,session_id', 'open returns closed navigation fields without credentials or page content');
      const peer = await connect(ready.peer_email);
      check(!(await peer('computer_sessions.open_link', { token: ready.watch_token })).ok, 'signed link never grants an unbound peer access');
      for (const [name, token] of [['expired', ready.expired_token], ['other session', ready.other_session_token], ['tampered', `${ready.watch_token}A`]]) {
        check(!(await admin('computer_sessions.open_link', { token })).ok, `${name} context refused even for Admin`);
      }
    } else if (phase === 'off') {
      check((await admin('users.org_settings.update', { manager_team_history: false })).ok, 'shared setting disabled');
      check(!(await manager('computer_sessions.status', { agent_id: 'alice' })).ok, 'same socket loses team status');
      check(!(await manager('computer_sessions.snapshot', { agent_id: 'alice', session_id: ready.session_id })).ok, 'cached image refused after shared setting disabled');
      check(!(await manager('computer_sessions.open_link', { token: ready.watch_token })).ok, 'navigation context refused after shared setting disabled');
    } else if (phase === 'restore') {
      check((await admin('users.org_settings.update', { manager_team_history: true })).ok, 'shared setting restored');
      const status = await manager('computer_sessions.status', { agent_id: 'alice' });
      check(status.ok && !status.payload.active, 'reactivation does not reveal the older session');
      check(!(await manager('computer_sessions.snapshot', { agent_id: 'alice', session_id: ready.session_id })).ok, 'old-window screenshot remains hidden');
      check(!(await manager('computer_sessions.open_link', { token: ready.watch_token })).ok, 'old-window navigation context remains hidden');
    } else {
      check((await admin('computer_sessions.stop', { agent_id: 'alice', session_id: ready.session_id })).ok, 'original fixture session stopped');
      const replacement = await startReplacement();
      check(replacement !== ready.session_id, 'replacement has a distinct session');
      check((await admin('users.bind_agent', { user_id: ready.manager, agent_name: 'alice', access_level: 'operator' })).ok, 'same manager receives direct binding');
      const promoted = await connect(ready.manager_email);
      const stale = await promoted('computer_sessions.stop', { agent_id: 'alice', session_id: ready.session_id });
      check(!stale.ok, 'team confirmation remains fenced after direct access is granted');
      const current = await promoted('computer_sessions.status', { agent_id: 'alice' });
      check(current.ok && current.payload.active && current.payload.session_id === replacement, 'replacement is still running after stale confirmation');
      check(!(await promoted('computer_sessions.open_link', { token: ready.watch_token })).ok, 'old navigation context cannot select a replacement after direct access is granted');
      check((await promoted('computer_sessions.stop', { agent_id: 'alice', session_id: replacement })).ok, 'current explicit confirmation can stop its own target');
    }
  }
} catch (error) {
  failure = error.message;
  save('team-ui-finish.json', { status: 'FAIL', phase, reason: failure });
} finally {
  for (const ws of sockets) {
    if (ws.readyState === WebSocket.CLOSED) continue;
    await new Promise((done) => { const timer = setTimeout(done, 2000); ws.addEventListener('close', () => { clearTimeout(timer); done(); }, { once: true }); ws.close(); });
  }
  save(`api-${phase}.json`, { status: failure ? 'FAIL' : 'PASS', phase, cases, error: failure ?? null });
}
console.log(JSON.stringify({ phase, status: failure ? 'FAIL' : 'PASS', checks: cases.length, evidence: resultPath }));
if (failure) process.exitCode = 1;
