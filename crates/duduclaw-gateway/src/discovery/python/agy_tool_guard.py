"""Antigravity PreToolUse hook: allow only the attempt's file/shell tools.

Second line of defence; the gateway's host-side stream guard is authoritative.
Reads the hook payload on stdin and answers with one JSON decision. Fails
closed: any exception, an unparsed payload or an unrendered allowlist denies.
"""
import json
import sys

DENY = {"decision": "deny", "reason": "tool is outside the attempt tool surface"}


def decide():
    allowed = frozenset(json.loads(r"""__DUDU_ALLOWED_TOOLS_JSON__"""))
    payload = json.loads(sys.stdin.read(32 * 1024 * 1024))
    name = payload["toolCall"]["name"]
    if isinstance(name, str) and name in allowed:
        return {"decision": "allow"}
    return DENY


try:
    answer = decide()
except BaseException:
    answer = DENY
try:
    sys.stdout.write(json.dumps(answer) + "\n")
    sys.stdout.flush()
except BaseException:
    sys.exit(2)
