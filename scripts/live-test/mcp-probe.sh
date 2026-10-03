#!/usr/bin/env bash
# Call real platform tools through one employee's own MCP registration.
#
#   scripts/live-test/mcp-probe.sh <home> <agent-id> [tool ...]
#
# Reads <home>/agents/<agent-id>/.mcp.json (the file the gateway writes at
# boot, with DUDUCLAW_HOME, DUDUCLAW_AGENT_ID, the internal MCP key and, when
# identity.key exists, DUDUCLAW_AGENT_TOKEN), starts the exact command and env
# it names, then sends initialize, tools/list and one tools/call per tool.
# Prints one line per call: `<tool>  ok`, `<tool>  REFUSED: <text>` or `<tool>  ERROR: <text>`.
# Exit 1 when a default tool is refused for the `prod-shaped` employee
# (or when the server cannot be started); an `ERROR` line on any other employee is reported but does not change the exit code; exit 2 on usage errors.
#
# The server is started with HOME set to <home>/os-home (created empty when
# missing), never the operator's own home, so nothing it spawns can pick up
# the operator's logins or settings.
#
# The minted agent token and internal key only exist after the gateway has
# booted once on that home, so run `duduclaw run` there first (with
# HOME=<home>/os-home, as make-home.sh prints).
# Binary override: DUDUCLAW_BIN (only used when .mcp.json names no command).
set -euo pipefail

[ $# -ge 2 ] || { sed -n 2,21p "$0" | sed 's/^# \{0,1\}//'; exit 2; }
HOME_DIR="$1"; AGENT="$2"; shift 2
TOOLS=("$@")
DEFAULTS=0
if [ ${#TOOLS[@]} -eq 0 ]; then
  TOOLS=(tasks_list memory_search working_state_get user_profile_get)
  DEFAULTS=1
fi

MCP_JSON="$HOME_DIR/agents/$AGENT/.mcp.json"
if [ ! -f "$MCP_JSON" ]; then
  echo "missing $MCP_JSON: boot the gateway once on this home first (HOME=$HOME_DIR/os-home DUDUCLAW_HOME=$HOME_DIR duduclaw run --yes), it writes .mcp.json at boot" >&2
  exit 1
fi

mkdir -p "$HOME_DIR/os-home"
chmod 700 "$HOME_DIR/os-home"

export PROBE_MCP_JSON="$MCP_JSON" PROBE_AGENT="$AGENT" PROBE_HOME="$HOME_DIR" \
       PROBE_DEFAULTS="$DEFAULTS" PROBE_BIN="${DUDUCLAW_BIN:-duduclaw}"

exec python3 - "${TOOLS[@]}" <<'PY'
import json, os, subprocess, sys, shutil, threading, queue

tools = sys.argv[1:]
agent = os.environ["PROBE_AGENT"]
defaults = os.environ["PROBE_DEFAULTS"] == "1"

cfg = json.load(open(os.environ["PROBE_MCP_JSON"]))
servers = cfg.get("mcpServers", {})
entry = servers.get("duduclaw") or servers.get("duduclaw-pro")
if not entry:
    print("no duduclaw entry in .mcp.json", file=sys.stderr); sys.exit(1)

# DUDUCLAW_BIN wins when set explicitly, so a freshly built binary can be probed
# even though .mcp.json pins an installed one.
cmd = os.environ["PROBE_BIN"] if "DUDUCLAW_BIN" in os.environ else entry.get("command", "duduclaw")
if shutil.which(cmd) is None and not os.path.isfile(cmd):
    print(f"binary not found: {cmd}", file=sys.stderr); sys.exit(1)
args = entry.get("args", ["mcp-server"])
env = {k: os.environ[k] for k in ("PATH", "TMPDIR", "LANG") if k in os.environ}
env.update(entry.get("env", {}))
env["DUDUCLAW_HOME"] = os.path.realpath(os.environ["PROBE_HOME"])
# Never the operator's own HOME: logins and CLI settings are looked up there.
env["HOME"] = os.path.join(env["DUDUCLAW_HOME"], "os-home")
env.setdefault("DUDUCLAW_AGENT_ID", agent)
if env["DUDUCLAW_AGENT_ID"] != agent:
    print(f".mcp.json names agent {env['DUDUCLAW_AGENT_ID']!r}, expected {agent!r}", file=sys.stderr); sys.exit(1)

p = subprocess.Popen([cmd] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, env=env, text=True, bufsize=1)
lines = queue.Queue()
threading.Thread(target=lambda: [lines.put(l) for l in p.stdout] or lines.put(None), daemon=True).start()

_id = 0
def rpc(method, params=None, timeout=60):
    global _id
    _id += 1
    msg = {"jsonrpc": "2.0", "id": _id, "method": method}
    if params is not None:
        msg["params"] = params
    p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
    while True:
        try:
            l = lines.get(timeout=timeout)
        except queue.Empty:
            return {"error": {"message": f"timeout after {timeout}s"}}
        if l is None:
            return {"error": {"message": "server exited"}}
        try:
            r = json.loads(l)
        except ValueError:
            continue
        if r.get("id") == _id:
            return r

# Default calls carry the one required argument these tools have, so any
# error result (refusal or otherwise) counts as a failure.
DEFAULT_ARGS = {"memory_search": {"query": "live-test probe"},
                "user_profile_get": {"user_id": "live-test-probe"}}
failed = []
try:
    r = rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                           "clientInfo": {"name": "live-test-probe", "version": "1"}})
    if "error" in r:
        print("initialize  FAILED:", r["error"].get("message")); sys.exit(1)
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"); p.stdin.flush()

    r = rpc("tools/list")
    listed = [t["name"] for t in r.get("result", {}).get("tools", [])]
    print(f"tools/list  {len(listed)} tools visible to {agent}")
    if not listed:
        failed.append("tools/list")

    for t in tools:
        r = rpc("tools/call", {"name": t, "arguments": DEFAULT_ARGS.get(t, {})})
        if "error" in r:
            text = r["error"].get("message", "error")
        else:
            res = r.get("result", {})
            text = " ".join(c.get("text", "") for c in res.get("content", []) if isinstance(c, dict))
            text = text if res.get("isError") else None
            if text is not None and not text.strip():
                text = "isError with empty text"
        if text is None:
            print(f"{t}  ok")
        else:
            one = " ".join(text.split())[:200]
            refused = any(k in one.lower() for k in ("denied", "not allowed", "refus", "permission", "scope", "-32003", "forbidden", "unauthor", "allowed_tools", "允許清單", "拒絕"))
            print(f"{t}  {'REFUSED' if refused else 'ERROR'}: {one}"); failed.append(t)
finally:
    try: p.stdin.close()
    except Exception: pass
    try: p.terminate()
    except Exception: pass

if failed and defaults and agent == "prod-shaped":
    print("FAIL: default checks failed for prod-shaped: " + ", ".join(failed)); sys.exit(1)
sys.exit(0)
PY
