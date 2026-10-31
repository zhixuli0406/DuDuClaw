#!/usr/bin/env bash
# Run the real Codex employee permission profile without credentials or model calls.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <image>" >&2
  exit 2
fi
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
docker run --rm -i --pull=never \
  --security-opt "seccomp=$repo_root/container/seccomp-bwrap.json" \
  --security-opt "apparmor=${DUDUCLAW_APPARMOR_PROFILE:-unconfined}" \
  --security-opt no-new-privileges:true \
  --cap-drop ALL --entrypoint /bin/bash "$1" -s <<'CONTAINER'
set -euo pipefail
suite="$(mktemp -d "$HOME/codex-sandbox-XXXXXX")"
export HOME="$suite/home"
export CODEX_HOME="$HOME/.codex"
export TEST_SUITE="$suite"
mkdir -p "$CODEX_HOME" "$suite/agents/employee" "$suite/agents/other"
printf 'control\n' > "$suite/agents/employee/agent.toml"
printf 'control\n' > "$suite/agents/employee/SOUL.md"
printf 'control\n' > "$suite/agents/other/agent.toml"
python3 - <<'PY'
import json
import os
from pathlib import Path
suite = Path(os.environ['TEST_SUITE'])
agent = suite / 'agents/employee'
config = Path(os.environ['CODEX_HOME']) / 'config.toml'
lines = [
    'approval_policy = "never"',
    'default_permissions = "duduclaw-employee"',
    '[permissions.duduclaw-employee.filesystem]',
    '":root" = "read"',
    '":tmpdir" = "write"',
    '"/tmp" = "write"',
]
for path in (agent / 'agent.toml', agent / 'SOUL.md'):
    lines.append(f'{json.dumps(str(path))} = "read"')
lines += ['[permissions.duduclaw-employee.filesystem.":project_roots"]', '"." = "write"']
config.write_text('\n'.join(lines) + '\n')
PY
# A listener in the outer network namespace makes the denied probe deterministic.
# An unrestricted sandbox would connect successfully; a timeout is never a pass.
python3 - <<'PY' &
import os
import socket
from pathlib import Path
with socket.socket() as server:
    server.bind(('127.0.0.1', 0))
    server.listen()
    Path(os.environ['TEST_SUITE'], 'listener-port').write_text(str(server.getsockname()[1]))
    while True:
        connection, _ = server.accept()
        connection.close()
PY
listener_pid=$!
trap 'kill "$listener_pid" 2>/dev/null || true' EXIT
for attempt in {1..100}; do
  [[ -s "$suite/listener-port" ]] && break
  kill -0 "$listener_pid"
  sleep 0.05
done
[[ -s "$suite/listener-port" ]]
cat > "$suite/agents/employee/inner.sh" <<'INNER'
#!/bin/sh
set -eu
python3 - <<'PY'
import errno
import os
from pathlib import Path
import socket
suite = Path(os.environ['TEST_SUITE'])
agent = suite / 'agents/employee'
(agent / '.sandbox-ran').write_text('ran\n')
(agent / 'shell-write.txt').write_text('writable\n')
for path in (agent / 'agent.toml', agent / 'SOUL.md',
             suite / 'agents/other/agent.toml',
             Path(os.environ['CODEX_HOME']) / 'config.toml'):
    try:
        with path.open('a') as stream:
            stream.write('UNEXPECTED WRITE\n')
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM, errno.EROFS):
            raise
    else:
        raise AssertionError(f'protected file was writable: {path.name}')
try:
    with socket.socket() as connection:
        connection.settimeout(3)
        connection.connect(('127.0.0.1', int((suite / 'listener-port').read_text())))
except OSError as error:
    if error.errno not in (errno.EPERM, errno.EACCES, errno.ENETUNREACH,
                           errno.EHOSTUNREACH, errno.ECONNREFUSED):
        raise
else:
    raise AssertionError('network access was allowed')
(agent / '.sandbox-passed').write_text('passed\n')
PY
INNER
cd "$suite/agents/employee"
config_digest="$(sha256sum "$CODEX_HOME/config.toml")"
timeout 30 codex sandbox -- sh ./inner.sh
kill -0 "$listener_pid"
[[ "$(sha256sum "$CODEX_HOME/config.toml")" == "$config_digest" ]]
[[ "$(cat .sandbox-ran)" == ran ]]
[[ "$(cat .sandbox-passed)" == passed ]]
[[ "$(cat shell-write.txt)" == writable ]]
for file in agent.toml SOUL.md ../other/agent.toml; do
  [[ "$(cat "$file")" == control ]]
done
echo 'PASS: shell ran; employee directory writable; control files and other employee protected; network denied.'
CONTAINER
