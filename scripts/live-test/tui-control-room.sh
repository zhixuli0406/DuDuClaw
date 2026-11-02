#!/usr/bin/env bash
# Real isolated gateway acceptance for the TUI inbox and autonomy RPCs.
# Usage: DUDUCLAW_BIN=/absolute/path/to/duduclaw ./tui-control-room.sh [port]
# Requires Node 22+ (built-in fetch/WebSocket). Keeps artifacts; never uses production home.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
TUI_TEST_PORT="${1:-18979}"
TUI_TEST_BIN="${DUDUCLAW_BIN:-$(command -v duduclaw || true)}"
[ -n "$TUI_TEST_BIN" ] && [ -x "$TUI_TEST_BIN" ] || { echo 'Set DUDUCLAW_BIN to an executable build' >&2; exit 2; }
case "$TUI_TEST_BIN" in /*) ;; *) TUI_TEST_BIN="$PWD/$TUI_TEST_BIN" ;; esac
command -v node >/dev/null || { echo 'Node 22+ required' >&2; exit 2; }
# Refuse an already occupied port before starting or contacting a gateway.
node --input-type=module - "$TUI_TEST_PORT" <<'JS'
import net from 'node:net';
const port = Number(process.argv[2]);
if (!Number.isInteger(port) || port < 1024 || port > 65535 || port === 18889) process.exit(2);
await new Promise((resolve, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(port, '127.0.0.1', () => server.close(resolve));
});
JS
TUI_TEST_HOME="$(mktemp -d "${TMPDIR:-/tmp}/duduclaw-tui-live.XXXXXX")"
bash "$SCRIPT_DIR/make-home.sh" "$TUI_TEST_HOME" --port "$TUI_TEST_PORT" > "$TUI_TEST_HOME.setup.log"
cat >> "$TUI_TEST_HOME/config.toml" <<'TOML'

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
# The subprocess HOME is empty, as required by make-home.sh conventions.
HOME="$TUI_TEST_HOME/os-home" DUDUCLAW_HOME="$TUI_TEST_HOME" "$TUI_TEST_BIN" run --yes > "$TUI_TEST_HOME/gateway.log" 2>&1 &
TUI_TEST_PID=$!
cleanup() {
  kill "$TUI_TEST_PID" 2>/dev/null || true
  wait "$TUI_TEST_PID" 2>/dev/null || true
  echo "Artifacts: $TUI_TEST_HOME"
}
trap cleanup EXIT
node --input-type=module - "$TUI_TEST_BIN" "$TUI_TEST_HOME" "$TUI_TEST_PORT" <<'JS'
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';
const [binary, home, port] = process.argv.slice(2);
const base = `http://127.0.0.1:${port}`;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
let token;
for (let i = 0; i < 120; i++) {
  try {
    const response = await fetch(`${base}/api/session/local`, {
      method: 'POST', headers: {'x-duduclaw-local':'1', 'content-type':'application/json'},
      body: '{}', signal: AbortSignal.timeout(1000),
    });
    if (response.ok) { token = (await response.json()).access_token; if (token) break; }
  } catch {}
  await delay(500);
}
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
  return new Promise((resolve, reject) => {
    const id = `tui-live-${++serial}`;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`RPC ${method} timed out`)); }, 15000);
    pending.set(id, {resolve, reject, timer, method});
    ws.send(JSON.stringify({type:'req', id, method, params}));
  });
}
try {
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('WS open timed out')), 10000);
    ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, {once:true});
    ws.addEventListener('error', () => { clearTimeout(timer); reject(new Error('WS open failed')); }, {once:true});
  });
  const hello = await rpc('connect', {jwt: token, client:'tui'});
  token = undefined;
  assert(hello.user && !hello.admin_fallback, 'account JWT identity required');
  // Same request shapes as Control::Autonomy::request in duduclaw-tui.
  const before = await rpc('autonomy.status', {});
  assert.equal(before.state_error, null);
  assert(!before.agents['prod-shaped']);
  await rpc('autonomy.pause', {agent_id:'prod-shaped'});
  const paused = await rpc('autonomy.status', {});
  assert.equal(paused.state_error, null);
  assert(paused.agents['prod-shaped'], 'pause was not persisted/read back');
  await rpc('autonomy.resume', {agent_id:'prod-shaped'});
  const resumed = await rpc('autonomy.status', {});
  assert.equal(resumed.state_error, null);
  assert(!resumed.agents['prod-shaped'], 'resume was not read back');
  writeFileSync(`${home}/autonomy-status.json`, JSON.stringify({before, paused, resumed}, null, 2));
  console.log('PASS autonomy.pause -> autonomy.status paused -> autonomy.resume -> autonomy.status running');
  const output = await new Promise((resolve, reject) => {
    const child = spawn(binary, ['tui', 'inbox', '--json'], {
      env: {...process.env, HOME:`${home}/os-home`, DUDUCLAW_HOME:home}, stdio:['ignore','pipe','pipe'],
    });
    let stdout = '', stderr = '';
    const timer = setTimeout(() => { child.kill('SIGTERM'); reject(new Error('tui inbox timed out')); }, 30000);
    child.stdout.on('data', bytes => { stdout += bytes; });
    child.stderr.on('data', bytes => { stderr += bytes; });
    child.on('error', reject);
    child.on('close', code => {
      clearTimeout(timer);
      writeFileSync(`${home}/inbox.stderr`, stderr);
      if (code !== 0) reject(new Error(`tui inbox exited ${code}; inspect inbox.stderr (AI-session refusal is intentional)`));
      else resolve(stdout);
    });
  });
  const rows = output.trim().split('\n').map(line => JSON.parse(line));
  assert(rows.length > 0 && rows.every(row => row.type === 'inbox' && Array.isArray(row.items) && Number.isInteger(row.count)));
  assert(rows.every(row => row.stale !== true), 'inbox snapshot is stale');
  writeFileSync(`${home}/inbox.jsonl`, output);
  console.log('PASS duduclaw tui inbox --json (real gateway, isolated home)');
} finally { ws.close(); }
JS
