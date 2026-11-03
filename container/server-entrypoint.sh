#!/bin/bash
# The protected-key launcher performs onboarding separately after dropping UID.
if [ "${DUDUCLAW_PROTECTED_KEY_MODE:-}" = root-launcher ]; then
    if [ "$(/usr/bin/id -u)" = 0 ]; then
        exec /usr/local/lib/duduclaw/protected-key-launcher.sh "$@"
    else
        echo "WARNING: protected-key mode needs the person-credentials Compose overlay to start as root; personal accounts remain unavailable." >&2
    fi
fi
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
