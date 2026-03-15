#!/bin/bash
set -e

echo "DuDuClaw Server Starting..."

# Run onboard if no config exists
if [ ! -f ~/.duduclaw/config.toml ]; then
    echo "First run detected, running onboard..."
    duduclaw onboard --yes
fi

# Execute the main command
exec "$@"
