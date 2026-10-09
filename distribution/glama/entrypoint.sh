#!/bin/sh
# Start the standalone MCP server. Without DUDUCLAW_MCP_API_KEY (as in a
# directory sandbox), issue a fresh standalone key in this container's own
# data directory first; the key only reaches memory and wiki tools.
#
# A read-only root cannot write ~/.duduclaw. Fall back to /tmp only when the
# chosen directory is not writable. A writable DUDUCLAW_HOME is kept.
set -eu

candidate="${DUDUCLAW_HOME:-${HOME:-/tmp}/.duduclaw}"
if mkdir -p "$candidate" 2>/dev/null && [ -w "$candidate" ]; then
  data_dir=$candidate
else
  data_dir=/tmp/duduclaw-home
  if ! mkdir -p "$data_dir" 2>/dev/null || [ ! -w "$data_dir" ]; then
    echo "entrypoint: no writable data directory" >&2
    exit 1
  fi
  echo "entrypoint: data directory not writable, using /tmp/duduclaw-home" >&2
  export HOME=/tmp
  export DUDUCLAW_HOME=$data_dir
fi

# Init's transcript contains the fresh key. Show the failure, never the key.
redact_and_show() {
  sed -E \
    -e 's/DUDUCLAW_MCP_API_KEY=[^[:space:]]*/DUDUCLAW_MCP_API_KEY=<redacted>/g' \
    -e 's/(sk-|mcp_)[A-Za-z0-9._-]*/<redacted>/g' \
    "$1" | head -c 2000 >&2
  echo >&2
}

if [ -z "${DUDUCLAW_MCP_API_KEY:-}" ]; then
  umask 077
  init_log=$(mktemp "$data_dir/mcp-init.XXXXXX") || {
    echo "entrypoint: could not create a private init log" >&2
    exit 1
  }
  set +e
  duduclaw mcp init --client print >"$init_log" 2>&1
  init_status=$?
  set -e
  key=$(grep -o 'DUDUCLAW_MCP_API_KEY=[A-Za-z0-9_.-]*' "$init_log" | head -n 1 | cut -d= -f2 || true)
  if [ "$init_status" -ne 0 ] || [ -z "$key" ]; then
    echo "entrypoint: could not issue a standalone key" >&2
    redact_and_show "$init_log"
    rm -f "$init_log"
    exit 1
  fi
  rm -f "$init_log"
  export DUDUCLAW_MCP_API_KEY="$key"
fi
exec duduclaw mcp-server "$@"
