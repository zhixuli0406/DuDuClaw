#!/bin/sh
# Start the standalone MCP server. Without DUDUCLAW_MCP_API_KEY (as in a
# directory sandbox), issue a fresh standalone key in this container's own
# data directory first; the key only reaches memory and wiki tools.
set -eu
if [ -z "${DUDUCLAW_MCP_API_KEY:-}" ]; then
  key=$(duduclaw mcp init --client print 2>/dev/null \
        | grep -o 'DUDUCLAW_MCP_API_KEY=[A-Za-z0-9_.-]*' | head -n 1 | cut -d= -f2)
  if [ -z "$key" ]; then
    echo "entrypoint: could not issue a standalone key" >&2
    exit 1
  fi
  export DUDUCLAW_MCP_API_KEY="$key"
fi
exec duduclaw mcp-server "$@"
