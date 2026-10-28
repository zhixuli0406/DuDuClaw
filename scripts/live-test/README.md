# Live-test scripts

Scripts for validating a build against a real gateway without touching the
production home (`~/.duduclaw`).

| Script | What it does |
|---|---|
| `make-home.sh <target-dir> [--port N]` | Creates an isolated DuDuClaw home: loopback `config.toml` (default port 18977) and two employees. `plain` has no tool allowlist (the old kind of test employee). `prod-shaped` is written like a production employee: `[capabilities] allowed_tools = ["mcp__duduclaw__*", "Read", "Write", "Edit", "Bash"]`, a `denied_tools` entry, `approval_required_tools`, all four `[permissions]` flags, `[budget]`, `[evolution] gvu_enabled = true`, a `CONTRACT.toml` and a `SOUL.md`. Refuses a non-empty target and any target inside `~/.duduclaw`. Writes no secrets. |
| `mcp-probe.sh <home> <agent-id> [tool ...]` | Starts the real `duduclaw mcp-server` for that employee from the command and env in its own `.mcp.json`, sends `initialize`, `tools/list` and one `tools/call` per tool (default: `tasks_list`, `memory_search`, `working_state_get`, `user_profile_get`), and prints `ok`, `REFUSED: ...` or `ERROR: ...` per call. Exits 1 when a default check fails for `prod-shaped`. |
| `python3 on-behalf-of.py <home> <binary>` | After booting and stopping the isolated gateway, calls the real `prod-shaped` stdio MCP server. Checks host identity stamping on tasks and queue rows, forged tool arguments, rejection auditing, enforce refusals and absent-principal behavior. Writes disposable test rows and temporarily changes the isolated delegation policy. It calls no model; runtime delivery, real channels and human acceptance require separate checks. |

## Why `prod-shaped` exists

v1.68.1 fixed a regression present since v1.67.0: an employee whose
`allowed_tools` used the Claude CLI wildcard `mcp__duduclaw__*` had every
platform MCP tool refused. Live validation missed it because the test
employee had no allowlist. The rule: after an upgrade, call real tools through
each employee's own MCP registration, and keep one employee in the live-test
environment configured like production.

## Sequence

```bash
# 1. Build the binary under test and create the home
scripts/live-test/make-home.sh /tmp/ddc-live --port 18977

# 2. Boot the gateway once on that home. Boot writes each employee's .mcp.json,
#    the internal MCP key and identity.key; the probe reads them from there.
#    HOME is replaced with the empty directory make-home.sh created: the gateway
#    and the AI CLI it spawns look for logins and settings under HOME, so
#    without this they would use your own account and quota.
export DUDUCLAW_BIN=$PWD/target/debug/duduclaw   # optional, default: duduclaw on PATH
HOME=/tmp/ddc-live/os-home DUDUCLAW_HOME=/tmp/ddc-live duduclaw run --yes &

# 3. Probe both employees
scripts/live-test/mcp-probe.sh /tmp/ddc-live plain
scripts/live-test/mcp-probe.sh /tmp/ddc-live prod-shaped

# 4. Dashboard checks: open http://127.0.0.1:18977, create the admin account,
#    open the AI employees page and confirm both employees load and show
#    their settings.
```

Expected probe output for a healthy build:

```
tools/list  175 tools visible to prod-shaped
tasks_list  ok
memory_search  ok
working_state_get  ok
user_profile_get  ok
```

The probe needs `.mcp.json`, and a valid agent token and internal key only
exist after the gateway has booted once on that home. A missing `.mcp.json`
makes the probe stop with that instruction. The gateway does not need to keep
running while you probe. `DUDUCLAW_BIN` overrides the binary named in
`.mcp.json`, so a fresh debug build can be probed even when `.mcp.json` pins an
installed one.

To probe a production home after an upgrade, run
`scripts/live-test/mcp-probe.sh ~/.duduclaw <agent-id>` for each employee that
has an allowlist. The default tools only read.

## Caveat: connectors are not isolated

An isolated home isolates files, databases, ports and keys. It does not isolate
the operator's Claude account. A test employee that runs under the operator's
Claude subscription can reach the Drive, Gmail and Calendar connectors attached
to that account. When you give a test employee a task, write "do not query
external services (Drive, Gmail, Calendar, web)" into it. The generated
`SOUL.md` and `CONTRACT.toml` say this too, but a contract line is not a
technical block.

## Dedicated local-browser backend fixture (macOS, review required)

`local-browser-backend-native.mjs` runs one exact ignored gateway test from this
worktree's freshly built test binary. It requires the binary's SHA256 and
`--run-reviewed-native`; execute it only after independent review of the frozen
launcher, cleanup helper and matching Rust test source. The original
`local-browser-spike.mjs` is a separate lower-level spike and its result is not
product acceptance.

```bash
node scripts/live-test/local-browser-backend-native.mjs \
  --run-reviewed-native --test-binary /absolute/worktree/target/debug/deps/duduclaw_gateway-HASH \
  --sha256 EXACT_TEST_BINARY_SHA256
```

The launcher creates a new `ddc-b9b-native-*` home, sets a minimal environment
from its first subprocess, and records PID/birth/executable ownership before
fallible CDP operations. Chrome retains its native sandbox; mock Keychain and
synthetic page fulfillment exist only under `cfg(test)`. The native app key,
presence and human ingress are synthetic fixtures, while the backend, Chrome,
profile, pipe, masking and network guards run real code. No real account or OS
Keychain is used. This is native-host trust, not an extra OS sandbox or VM.
The machine-wide lease logic also runs, but `cfg(test)` routes its file to
`<fresh-home>/owned-machine-lease.lock` after checking the canonical owned
root and marker. Neither a dirty/unknown fixture cleanup nor a fault injection
may dirty the user's real UID lease. PASS requires that exact owned file to
exist safely and be clear after verified product cleanup; fallback cleanup
never clears the barrier or converts the run to PASS.

The result is written after bounded child cleanup and browser ownership checks.
A missing inventory, fallback cleanup, unknown surviving child, signal, test
failure or evidence-write failure cannot produce PASS. Artifacts remain in the
new home. Physical screen-lock behavior, a signed Tauri app, actual native
approval gestures, provider sign-in and release approval remain separate gates.

Pure cleanup-policy regressions (no browser or process signals):
`python3 scripts/live-test/local-browser-native-cleanup.test.py`.

## Cleanup

```bash
kill %1                      # the gateway started in step 2
rm -rf /tmp/ddc-live
scripts/clean-build-cache.sh --dry-run   # build cache: see below
```

`scripts/clean-build-cache.sh` reclaims disk from this workspace's own build
artifacts under `target/debug` and keeps third-party dependency artifacts. It
prints free space and the size of `target/debug`, refuses to run while a
`cargo` or `rustc` process is alive (`--force` overrides), and `--dry-run`
lists what would go with sizes. `target/release` and other target triples are
untouched unless you pass `--all-profiles`. The cache once reached 134 GB and a
full disk crashed Docker Desktop; run it when free space drops below about
100 GB.
