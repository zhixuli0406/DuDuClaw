#!/usr/bin/env bash
# RFC-24 — Unix/macOS smoke harness for Decision Continuity.
#
# What it does:
#   1. cargo build the duduclaw-cli binary (brings in gateway + memory + tools).
#   2. Run the memory engine tests (decision query/resolve/ttl/dismiss live here).
#   3. Run the gateway decision_capture + runtime_config tests (detector, capture,
#      injection, the compress()-survival regression, Haiku classify, P2 resolve).
#   4. If a built `duduclaw` binary is found, drive its `mcp-server` over stdio and
#      assert the decision_list / decision_resolve tools are registered (real
#      end-to-end check that the MCP surface ships).
#
# Exit non-zero if any step fails.
#
# Usage:
#   scripts/smoke-decision-continuity.sh
set -euo pipefail

cd "$(dirname "$0")/.."

echo "[smoke] uname -a: $(uname -a)"
echo "[smoke] cargo version: $(cargo --version)"

echo "[smoke] (1/4) cargo build duduclaw-cli"
cargo build -p duduclaw-cli

echo "[smoke] (2/4) cargo test duduclaw-memory --lib"
cargo test -p duduclaw-memory --lib --no-fail-fast

echo "[smoke] (3a/4) cargo test duduclaw-gateway decision_capture::"
cargo test -p duduclaw-gateway --lib --no-fail-fast decision_capture::
echo "[smoke] (3b/4) cargo test duduclaw-gateway runtime_config::"
cargo test -p duduclaw-gateway --lib --no-fail-fast runtime_config::

echo "[smoke] (4/4) shipped-surface check (MCP tools + dashboard RPC methods)"
# We assert the literals are compiled into the binary rather than driving a live
# stdio session: tools/list over stdio is filtered by principal.is_external and
# gated on the MCP initialize handshake / auth env, which is orthogonal to "did
# this surface ship". The string literals only appear if the ToolDef + RPC arms
# compiled in.
BIN=""
for c in target/debug/duduclaw target/release/duduclaw; do
    if [[ -x "$c" ]]; then BIN="$c"; break; fi
done
if [[ -z "$BIN" ]]; then
    echo "[smoke] no duduclaw binary found (target/{debug,release}); skipping surface check"
else
    echo "[smoke] using binary: $BIN"
    for lit in decision_list decision_resolve decisions.list decisions.dismiss; do
        if grep -aqF "$lit" "$BIN"; then
            echo "[smoke]   ✓ $lit shipped"
        else
            echo "[smoke]   ✗ $lit MISSING from binary" >&2
            exit 1
        fi
    done
fi

echo "[smoke] OK — Decision Continuity (RFC-24) smoke passed."
