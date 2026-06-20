#!/usr/bin/env bash
# RFC-26 — Unix/macOS smoke harness for Live Run Forking.
#
# What it does:
#   1. cargo build the `duduclaw-fork` crate.
#   2. Run the `duduclaw-fork` unit tests (branch / budget / overlay / judge /
#      test_runner / merge / controller — 49+ tests).
#   3. Run the cli fork tool-surface + executor tests (mcp_fork + mcp_fork_exec).
#   4. Run the durability checkpoint fork/rewind tests.
#
# Exit non-zero if any step fails.
#
# Usage:
#   scripts/smoke-fork.sh
set -euo pipefail

cd "$(dirname "$0")/.."

echo "[smoke] uname -a: $(uname -a)"
echo "[smoke] cargo version: $(cargo --version)"

echo "[smoke] (1/4) cargo build duduclaw-fork"
cargo build -p duduclaw-fork

echo "[smoke] (2/4) cargo test duduclaw-fork --lib"
cargo test -p duduclaw-fork --lib --no-fail-fast

echo "[smoke] (3/5) cargo test cli fork surface (mcp_fork, mcp_fork_exec, planner, builtin_skills)"
cargo test -p duduclaw-cli --lib mcp_fork --no-fail-fast
cargo test -p duduclaw-cli --lib mcp_planner --no-fail-fast
cargo test -p duduclaw-cli --lib builtin_skills --no-fail-fast
cargo test -p duduclaw-cli --lib "mcp_memory_handlers::tests::cluster" --no-fail-fast

echo "[smoke] (4/5) cargo test durability checkpoint (fork/rewind + persistence)"
cargo test -p duduclaw-durability --lib checkpoint --no-fail-fast

echo "[smoke] (5/5) cargo test gateway fork surface (metrics + task_store cycle detection)"
cargo test -p duduclaw-gateway --lib "metrics::tests::fork" --no-fail-fast
cargo test -p duduclaw-gateway --lib "task_store::tests" --no-fail-fast

echo "[smoke] clippy (duduclaw-fork)"
# Note: not -D warnings — that promotes pre-existing upstream (duduclaw-core)
# lints to errors across the dependency graph. The fork crate's own code is
# warning-clean; grep its lines to fail only on fork-owned warnings.
fork_warns=$(cargo clippy -p duduclaw-fork --all-targets 2>&1 | grep -c "duduclaw-fork/src" || true)
if [ "$fork_warns" -ne 0 ]; then
  echo "[smoke] FAIL — $fork_warns clippy warning(s) in duduclaw-fork"
  cargo clippy -p duduclaw-fork --all-targets 2>&1 | grep -A4 "duduclaw-fork/src"
  exit 1
fi

echo "[smoke] OK — RFC-26 fork smoke passed"
