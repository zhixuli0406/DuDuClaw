# Live-test scripts

Scripts for validating a build against a real gateway without touching the
production home (`~/.duduclaw`).

| Script | What it does |
|---|---|
| `make-home.sh <target-dir> [--port N]` | Creates an isolated DuDuClaw home: loopback `config.toml` (default port 18977) and two employees. `plain` has no tool allowlist (the old kind of test employee). `prod-shaped` is written like a production employee: `[capabilities] allowed_tools = ["mcp__duduclaw__*", "Read", "Write", "Edit", "Bash"]`, a `denied_tools` entry, `approval_required_tools`, all four `[permissions]` flags, `[budget]`, `[evolution] gvu_enabled = true`, a `CONTRACT.toml` and a `SOUL.md`. Refuses a non-empty target and any target inside `~/.duduclaw`. Writes no secrets. |
| `mcp-probe.sh <home> <agent-id> [tool ...]` | Starts the real `duduclaw mcp-server` for that employee from the command and env in its own `.mcp.json`, sends `initialize`, `tools/list` and one `tools/call` per tool (default: `tasks_list`, `memory_search`, `working_state_get`, `user_profile_get`), and prints `ok`, `REFUSED: ...` or `ERROR: ...` per call. Exits 1 when a default check fails for `prod-shaped`. |
| `node google-delegated-identity.mjs <binary>` | Creates its own temporary home, starts a real gateway and stdio MCP servers, and exchanges signed RSA assertions with a loopback fake Google. Requires a debug build with `--features live-test-mocks` and Node 22+. Credential protection uses an in-memory store; no real keychain, model or Google account is touched. Checks shared-account compatibility, two delegated subjects, same-process account changes, domain refusal, unsigned and unproven callers, provider rejection, explicit fallback, `none`, the master switch, audit redaction and offline gateway refusal. Artifacts remain in the printed temporary folder. |
| `node google-personal-identity.mjs <guarded-binary-wrapper>` | Creates a fresh Enterprise home and a loopback OAuth/service mock. Requires Node 22+, a debug `live-test-mocks` binary and an OS sandbox wrapper that denies real credential/keychain access and all outbound traffic except loopback. Checks exact redirect/PKCE, same-session completion after JWT refresh, cross-session/user refusal, actual partial scopes, self-consent, primary Owner changes, per-call refresh/status checks, local disconnect before delayed revoke, grant coordination and encrypted/redacted storage. Reversible suspension is injected into the fresh fixture database because this base has no reversible status RPC; it verifies resolver revalidation, not a status-management API. Artifacts remain in the printed folder. This is isolated mock evidence, not real Google or OS-keychain acceptance. |

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
