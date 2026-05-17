#!/usr/bin/env bash
# Phase 4.5 — Unix/macOS smoke harness for the PTY pool runtime.
#
# What it does:
#   1. cargo build the `duduclaw-cli-runtime` crate (+ spike example).
#   2. Run the cli-runtime unit tests (53+ tests).
#   3. Run the gateway routing-helper + pty_runtime tests (15+ tests).
#   4. If `claude` is on PATH, optionally run the live interactive spike
#      binary (`CLAUDE_SPIKE=1` env var to opt in; consumes OAuth quota).
#
# Exit non-zero if any step fails.
#
# Usage:
#   scripts/smoke-pty-pool.sh             # build + unit tests only
#   CLAUDE_SPIKE=1 scripts/smoke-pty-pool.sh   # plus live spike
set -euo pipefail

cd "$(dirname "$0")/.."

echo "[smoke] uname -a: $(uname -a)"
echo "[smoke] cargo version: $(cargo --version)"

echo "[smoke] (1/4) cargo build duduclaw-cli-runtime + spike example"
cargo build \
    -p duduclaw-cli-runtime \
    --example claude_interactive_spike

echo "[smoke] (2/4) cargo test duduclaw-cli-runtime --lib"
cargo test -p duduclaw-cli-runtime --lib --no-fail-fast

echo "[smoke] (3a/4) cargo test duduclaw-gateway pty_runtime::"
cargo test -p duduclaw-gateway --lib --no-fail-fast pty_runtime::
echo "[smoke] (3b/4) cargo test duduclaw-gateway channel_reply::routing_helper_tests"
cargo test -p duduclaw-gateway --lib --no-fail-fast \
    channel_reply::routing_helper_tests
echo "[smoke] (3c/4) cargo test duduclaw-gateway stream_json_parser_tests"
cargo test -p duduclaw-gateway --lib --no-fail-fast \
    channel_reply::stream_json_parser_tests

if [[ "${CLAUDE_SPIKE:-0}" == "1" ]]; then
    if command -v claude >/dev/null 2>&1; then
        echo "[smoke] (4/4) live interactive spike against claude $(claude --version)"
        cargo run \
            -p duduclaw-cli-runtime \
            --example claude_interactive_spike
    else
        echo "[smoke] (4/4) SKIPPED — CLAUDE_SPIKE=1 but \`claude\` not on PATH"
        exit 1
    fi
else
    echo "[smoke] (4/4) SKIPPED — set CLAUDE_SPIKE=1 to run live spike"
fi

echo "[smoke] ✅ all checks passed"
