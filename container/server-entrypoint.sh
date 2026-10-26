#!/bin/bash
set -e

echo "DuDuClaw Server Starting..."

# Run onboard if no config exists
if [ ! -f ~/.duduclaw/config.toml ]; then
    echo "First run detected, running onboard..."
    if [ -n "${DUDUCLAW_PORT:-}" ]; then
        duduclaw onboard --yes
    elif [ -n "${DUDUCLAW_INITIAL_PORT:-}" ]; then
        DUDUCLAW_PORT="$DUDUCLAW_INITIAL_PORT" duduclaw onboard --yes
    else
        duduclaw onboard --yes
    fi
fi

# Execute the main command
exec "$@"
