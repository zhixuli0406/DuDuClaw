#!/bin/bash
set -e

echo "DuDuClaw Agent Container Starting..."
echo "Agent: ${AGENT_NAME:-unknown}"
echo "Model: ${CLAUDE_MODEL:-claude-sonnet-4-6}"

# Execute the agent command
exec "$@"
