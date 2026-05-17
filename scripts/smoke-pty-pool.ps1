# Phase 4.5 — Windows smoke harness for the PTY pool runtime.
#
# What it does:
#   1. cargo build the duduclaw-cli-runtime crate (+ spike example).
#   2. Run the cli-runtime unit tests.
#   3. Run the gateway routing-helper + pty_runtime tests.
#   4. If `claude` is on PATH, optionally run the live interactive spike
#      (set $env:CLAUDE_SPIKE = '1' to opt in; consumes OAuth quota).
#
# Validates the ConPTY backend (Win 10 1809+) + Job Object child reaping.
#
# Usage (PowerShell):
#   pwsh scripts/smoke-pty-pool.ps1                  # build + unit tests
#   $env:CLAUDE_SPIKE = '1'; pwsh scripts/smoke-pty-pool.ps1   # + live spike

$ErrorActionPreference = 'Stop'

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..')
Set-Location $repoRoot

Write-Host "[smoke] OS: $([System.Environment]::OSVersion.VersionString)"
Write-Host "[smoke] cargo: $((cargo --version) -join '')"

# ConPTY is only available on Windows 10 1809 (build 17763) and later.
# portable-pty falls back to WinPTY DLLs otherwise, which we don't ship —
# warn loudly if the OS is too old.
$build = [System.Environment]::OSVersion.Version.Build
if ($build -lt 17763) {
    Write-Warning "[smoke] Windows build $build < 17763 — ConPTY unavailable. portable-pty may fall back to WinPTY shim. Phase 4 is validated against Win 10 1809+."
}

Write-Host "[smoke] (1/4) cargo build duduclaw-cli-runtime + spike example"
cargo build `
    -p duduclaw-cli-runtime `
    --example claude_interactive_spike
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "[smoke] (2/4) cargo test duduclaw-cli-runtime --lib"
cargo test -p duduclaw-cli-runtime --lib --no-fail-fast
if ($LASTEXITCODE -ne 0) { throw "cli-runtime tests failed" }

Write-Host "[smoke] (3a/4) cargo test duduclaw-gateway pty_runtime::"
cargo test -p duduclaw-gateway --lib --no-fail-fast 'pty_runtime::'
if ($LASTEXITCODE -ne 0) { throw "gateway pty_runtime tests failed" }

Write-Host "[smoke] (3b/4) cargo test duduclaw-gateway channel_reply::routing_helper_tests"
cargo test -p duduclaw-gateway --lib --no-fail-fast `
    'channel_reply::routing_helper_tests'
if ($LASTEXITCODE -ne 0) { throw "gateway routing-helper tests failed" }

Write-Host "[smoke] (3c/4) cargo test duduclaw-gateway stream_json_parser_tests"
cargo test -p duduclaw-gateway --lib --no-fail-fast `
    'channel_reply::stream_json_parser_tests'
if ($LASTEXITCODE -ne 0) { throw "gateway stream-json parser tests failed" }

if ($env:CLAUDE_SPIKE -eq '1') {
    $claudeCmd = Get-Command claude -ErrorAction SilentlyContinue
    if ($null -ne $claudeCmd) {
        Write-Host "[smoke] (4/4) live interactive spike against claude $(claude --version)"
        cargo run `
            -p duduclaw-cli-runtime `
            --example claude_interactive_spike
        if ($LASTEXITCODE -ne 0) { throw "spike failed" }
    } else {
        throw "CLAUDE_SPIKE=1 but `claude` not on PATH"
    }
} else {
    Write-Host "[smoke] (4/4) SKIPPED — set `$env:CLAUDE_SPIKE = '1' to run live spike"
}

Write-Host "[smoke] ✅ all checks passed"
