#!/usr/bin/env bash
# Real isolated gateway acceptance for third-party MCP in chat and cron.
# Usage: DUDUCLAW_BIN=/absolute/duduclaw MCP_FIXTURE_BIN=/absolute/third_party_mcp_fixture ./third-party-mcp.sh [port]
# Requires Node 22+ (built-in fetch/WebSocket). Keeps artifacts; never uses production home.
set -euo pipefail
MCP_TEST_FIXTURE="${MCP_FIXTURE_BIN:?Set MCP_FIXTURE_BIN to the built Rust example third_party_mcp_fixture}"
case "$MCP_TEST_FIXTURE" in /*) ;; *) echo 'MCP_FIXTURE_BIN must be absolute' >&2; exit 2 ;; esac
[ -x "$MCP_TEST_FIXTURE" ] || { echo 'Rust fixture binary missing' >&2; exit 2; }
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MCP_TEST_PORT="${1:-18983}"
MCP_TEST_BIN="${DUDUCLAW_BIN:-$(command -v duduclaw || true)}"
[ -n "$MCP_TEST_BIN" ] && [ -x "$MCP_TEST_BIN" ] || { echo 'Set DUDUCLAW_BIN to an executable build' >&2; exit 2; }
case "$MCP_TEST_BIN" in /*) ;; *) MCP_TEST_BIN="$PWD/$MCP_TEST_BIN" ;; esac
MCP_TEST_PATH='/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin'
MCP_TEST_NODE="$(command -v node || true)"
[ -n "$MCP_TEST_NODE" ] || { echo 'Node 22+ required' >&2; exit 2; }
case "$MCP_TEST_NODE" in /*) ;; *) MCP_TEST_NODE="$(cd "$(dirname "$MCP_TEST_NODE")" && pwd)/$(basename "$MCP_TEST_NODE")" ;; esac
MCP_TEST_NODE_MAJOR="$("$MCP_TEST_NODE" --version | sed 's/^v//' | cut -d. -f1)"
case "$MCP_TEST_NODE_MAJOR" in ''|*[!0-9]*) echo 'Node 22+ required' >&2; exit 2 ;; esac
[ "$MCP_TEST_NODE_MAJOR" -ge 22 ] || { echo 'Node 22+ required' >&2; exit 2; }
# Refuse an already occupied port before starting or contacting a gateway.
/usr/bin/env -i PATH="$MCP_TEST_PATH" HOME="$HOME" LANG=C LC_ALL=C "$MCP_TEST_NODE" --input-type=module - "$MCP_TEST_PORT" <<'JS'
import net from 'node:net';
const port = Number(process.argv[2]);
if (!Number.isInteger(port) || port < 1024 || port > 65535 || port === 18889) process.exit(2);
await new Promise((resolve, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(port, '127.0.0.1', () => server.close(resolve));
});
JS
MCP_TEST_HOME="$(mktemp -d "/tmp/duduclaw-third-party-mcp-live.XXXXXX")"
/usr/bin/env -i PATH="$MCP_TEST_PATH" HOME="$HOME" LANG=C LC_ALL=C /bin/bash "$SCRIPT_DIR/make-home.sh" "$MCP_TEST_HOME" --port "$MCP_TEST_PORT" > "$MCP_TEST_HOME.setup.log"
# The instance endpoint is owned by this fixture, never inherited from the caller.
cat > "$MCP_TEST_HOME/config.toml" <<TOML
[gateway]
bind = "127.0.0.1"
port = $MCP_TEST_PORT
[general]
default_language = "zh-TW"
log_level = "info"
[dashboard]
local_auto_login = true
[dispatch]
enabled = false
[tick]
enabled = false
[proactive]
enabled = false
[responsibilities]
enabled = false
TOML
# A random employee proves which home answered the authenticated RPC.
/usr/bin/env -i PATH="$MCP_TEST_PATH" HOME="$MCP_TEST_HOME/os-home" DUDUCLAW_HOME="$MCP_TEST_HOME" LANG=C LC_ALL=C "$MCP_TEST_NODE" --input-type=module - "$MCP_TEST_HOME" <<'JS'
import { randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
const home = process.argv[2];
const canary = `tui-canary-${randomBytes(16).toString('hex')}`;
mkdirSync(`${home}/agents/${canary}`, {mode:0o700});
const config = readFileSync(`${home}/agents/plain/agent.toml`, 'utf8')
  .replace('name = "plain"', `name = "${canary}"`)
  .replace('display_name = "Plain Test Employee"', `display_name = "${canary}"`)
  .replace('trigger = "@plain"', `trigger = "@${canary}"`);
writeFileSync(`${home}/agents/${canary}/agent.toml`, config, {mode:0o600});
writeFileSync(`${home}/instance-canary`, canary, {mode:0o600});
JS
# Resolve Claude only to the local Rust shim; it never contacts a model.
mkdir -p "$MCP_TEST_HOME/stubbin"
{
  printf '#!/bin/bash\nexec '
  printf '%q ' "$MCP_TEST_FIXTURE" --claude --record "$MCP_TEST_HOME/results.jsonl"
  printf '"$@"\n'
} > "$MCP_TEST_HOME/stubbin/claude"
chmod 700 "$MCP_TEST_HOME/stubbin/claude"
MCP_TEST_PATH="$MCP_TEST_HOME/stubbin:$MCP_TEST_PATH"
# Both the gateway and TUI use only this environment, with no credentials,
# provider/cloud settings, AI-session identity, proxy or loader variables.
(
  cd "$MCP_TEST_HOME/os-home"
  exec /usr/bin/env -i PATH="$MCP_TEST_PATH" HOME="$MCP_TEST_HOME/os-home" DUDUCLAW_HOME="$MCP_TEST_HOME" LANG=C LC_ALL=C "$MCP_TEST_BIN" run --yes
) > "$MCP_TEST_HOME/gateway.log" 2>&1 &
MCP_TEST_PID=$!
cleanup() {
  kill "$MCP_TEST_PID" 2>/dev/null || true
  wait "$MCP_TEST_PID" 2>/dev/null || true
  echo "Artifacts: $MCP_TEST_HOME"
}
trap cleanup EXIT
/usr/bin/env -i PATH="$MCP_TEST_PATH" HOME="$MCP_TEST_HOME/os-home" DUDUCLAW_HOME="$MCP_TEST_HOME" LANG=C LC_ALL=C "$MCP_TEST_NODE" --input-type=module - "$MCP_TEST_BIN" "$MCP_TEST_HOME" "$MCP_TEST_PORT" "$MCP_TEST_PID" "$MCP_TEST_FIXTURE" <<'JS'
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
const [binary, home, port, gatewayPid, fixture] = process.argv.slice(2);
const canary = readFileSync(`${home}/instance-canary`, 'utf8');
function assertChildAlive() {
  process.kill(Number(gatewayPid), 0);
}
const startDeadline = Date.now() + 120 * 500;
async function assertChildOwnsListener() {
  while (true) {
    assertChildAlive();
    const result = spawnSync('lsof', ['-nP', `-iTCP@127.0.0.1:${port}`, '-sTCP:LISTEN', '-t'], {encoding:'utf8'});
    assert.equal(result.error, undefined, 'lsof is required to verify the gateway listener owner');
    if (result.status === 0) {
      const listenerPids = result.stdout.trim().split(/\s+/).filter(Boolean);
      assert.deepEqual(listenerPids, [gatewayPid], `listener on 127.0.0.1:${port} does not belong to the started gateway (expected PID ${gatewayPid}, found ${listenerPids.join(', ') || 'none'})`);
      return;
    }
    assert.equal(result.status, 1, `lsof failed while checking listener on 127.0.0.1:${port}`);
    assert(Date.now() < startDeadline, `gateway did not bind 127.0.0.1:${port} before the startup deadline`);
    await delay(200);
  }
}
const base = `http://127.0.0.1:${port}`;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
let token;
for (let i = 0; i < 120; i++) {
  await assertChildOwnsListener();
  try {
    const response = await fetch(`${base}/api/session/local`, {
      method: 'POST', headers: {'x-duduclaw-local':'1', 'content-type':'application/json'},
      body: '{}', signal: AbortSignal.timeout(1000),
    });
    if (response.ok) { token = (await response.json()).access_token; if (token) break; }
  } catch {}
  await delay(500);
}
assertChildAlive();
assert(token, 'isolated local account login did not become ready; inspect gateway.log');
const ws = new WebSocket(`ws://127.0.0.1:${port}/ws`);
const pending = new Map();
let serial = 0;
ws.addEventListener('message', event => {
  const value = JSON.parse(event.data);
  const waiter = pending.get(value.id);
  if (value.type !== 'res' || !waiter) return;
  pending.delete(value.id); clearTimeout(waiter.timer);
  if (value.ok) waiter.resolve(value.payload);
  else waiter.reject(new Error(`RPC ${waiter.method} refused`));
});
ws.addEventListener('close', () => {
  for (const waiter of pending.values()) { clearTimeout(waiter.timer); waiter.reject(new Error('RPC socket closed')); }
  pending.clear();
});
function rpc(method, params) {
  assertChildAlive();
  return new Promise((resolve, reject) => {
    const id = `tui-live-${++serial}`;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`RPC ${method} timed out`)); }, 15000);
    pending.set(id, {resolve, reject, timer, method});
    ws.send(JSON.stringify({type:'req', id, method, params}));
  });
}
async function confirmInstance() {
  assertChildAlive();
  const roster = await rpc('agents.list', {});
  assertChildAlive();
  assert(Array.isArray(roster.agents) && roster.agents.some(agent =>
    agent.name === canary && agent.display_name === canary), 'RPC answered by a different gateway; refusing changes');
}
try {
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('WS open timed out')), 10000);
    ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, {once:true});
    ws.addEventListener('error', () => { clearTimeout(timer); reject(new Error('WS open failed')); }, {once:true});
  });
  const hello = await rpc('connect', {jwt: token, client:'third-party-mcp-live'});

  assert(hello.user && !hello.admin_fallback, 'account JWT identity required');
  await confirmInstance();
  const installed = await rpc('mcp.update', {agent_id:'prod-shaped', action:'add', server_name:'fixture',
    server_def:{command:fixture, args:['--stdio'], env:{}}});
  assert(installed.success && installed.probe.ok && installed.probe.tool_count === 1, 'install probe failed');
  const chat = new WebSocket(`ws://127.0.0.1:${port}/ws/chat`);
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('chat open timeout')), 10000);
      chat.addEventListener('open', () => { clearTimeout(timer); resolve(); }, {once:true});
    });
    const done = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('chat timeout')), 45000);
      chat.addEventListener('message', event => {
        const frame = JSON.parse(event.data);
        if (frame.type === 'assistant_done') { clearTimeout(timer); resolve(frame); }
        if (frame.type === 'error') { clearTimeout(timer); reject(new Error('chat refused')); }
      });
    });
    chat.send(JSON.stringify({type:'auth', token}));
    await delay(150);
    chat.send(JSON.stringify({type:'user_message', agent:'prod-shaped', content:'Local MCP fixture check', conv:'third-party-mcp'}));
    await done;
  } finally { chat.close(); }
  await confirmInstance();
  const job = await rpc('cron.add', {name:'third-party-mcp-fixture', agent_id:'prod-shaped', cron:'0 3 * * *',
    task:'Local MCP fixture check'});
  await rpc('cron.run_now', {id:job.task.id, request_id:'third-party-mcp-live'});
  const deadline = Date.now() + 45000;
  while (true) {
    assertChildAlive();
    const rows = (() => { try { return readFileSync(`${home}/results.jsonl`, 'utf8').trim().split('\n').map(JSON.parse); } catch { return []; } })();
    if (['chat','cron'].every(kind => rows.some(r => r.kind === kind && r.initialize && r.tools_list && r.missing_token_refused))) break;
    assert(Date.now() < deadline, 'chat/cron proxy handshake or negative control missing; inspect artifacts');
    await delay(250);
  }
  console.log('PASS chat + cron: real spawn preparation, initialize, tools/list, missing-token refusal');
} finally { ws.close(); }
JS
