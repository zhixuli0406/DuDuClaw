# RFC-26 — Windows smoke harness for Live Run Forking.
#
# What it does:
#   1. cargo build the `duduclaw-fork` crate.
#   2. Run the `duduclaw-fork` unit tests.
#   3. Run the cli fork tool-surface + executor tests (mcp_fork + mcp_fork_exec).
#   4. Run the durability checkpoint fork/rewind tests.
#
# Usage:
#   pwsh scripts/smoke-fork.ps1
$ErrorActionPreference = "Stop"

Set-Location (Join-Path $PSScriptRoot "..")

Write-Host "[smoke] cargo version: $(cargo --version)"

Write-Host "[smoke] (1/4) cargo build duduclaw-fork"
cargo build -p duduclaw-fork
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[smoke] (2/4) cargo test duduclaw-fork --lib"
cargo test -p duduclaw-fork --lib --no-fail-fast
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[smoke] (3/4) cargo test cli fork surface"
cargo test -p duduclaw-cli --lib mcp_fork --no-fail-fast
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[smoke] (4/4) cargo test durability checkpoint"
cargo test -p duduclaw-durability --lib checkpoint --no-fail-fast
if ($LASTEXITCODE -ne 0) { exit 1 }

Write-Host "[smoke] OK - RFC-26 fork smoke passed"
