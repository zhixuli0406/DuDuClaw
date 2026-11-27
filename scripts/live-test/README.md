# Live-test scripts

Scripts for validating a build against a real gateway without touching the
production home (`~/.duduclaw`).

| Script | What it does |
|---|---|
| `v185-acceptance.mjs [--port N] [--only avatar,memory,identity,wiki,member-key,self-add,avatar-seat,avatar-admin-read,google-local]` | Requires Node 22+ (global `WebSocket`), macOS `sandbox-exec`, `sqlite3`, `lsof`, and `DUDUCLAW_BIN` pointing to an already-built phase-two executable. Creates a disposable Enterprise upgrade fixture with a signed OEM license (6 ordinary seats when `avatar-seat` is selected), isolated homes, local CLI shims, loopback TCP and private local IPC. Checks avatar migration/idempotence, remembered MCP results/pinned notes, Claude/Codex IPC identity (live unrelated-token-holder refusal and forged-server zero-byte check), wiki refusals, member-key revocation, Owner self-add plus inherited `run_id` approval cards, and avatar creation/unset/ordinary-create behavior at capacity. `avatar-admin-read` explicitly SKIPs because D4 needs native Linux protected isolation; RM-159/RM-062/RM-147/RM-182 each SKIP real Google acceptance. `google-local` checks source effect classes and hidden/refused tools without a connection; it does not prove provider behavior or ticket consumption. Existing fixture/evidence limits are below. `--test-helpers` runs without a gateway, subprocesses or listeners. |
| `tui-control-room.sh [port]` | Requires Node 22+ and `DUDUCLAW_BIN` pointing to a built executable. Creates an isolated home with `make-home.sh`, starts a real loopback gateway (default port 18979), logs in with a local account JWT, checks `autonomy.pause` / `autonomy.resume` via the same `/ws` RPC shapes as the TUI and reads back `autonomy.status`. Runs the real `tui inbox --json`, checks freshness, stops its own gateway and retains artifacts. Run from an operator shell: AI-session markers are not removed. This implementation was not live-run in the restricted sandbox. |
| `third-party-mcp.sh [port]` | Requires Node 22+, `lsof`, `DUDUCLAW_BIN` and `MCP_FIXTURE_BIN` (the Rust `third_party_mcp_fixture` example). Creates its own isolated home and loopback gateway (default 18983), installs a Rust stdio fixture, checks the install probe, then checks real chat and cron preparation with a local Claude shim. Both runs verify initialize/tools/list and the missing-token negative control. No model or external MCP service is used. Stops its own gateway and retains sanitised results and gateway logs. Not live-run during implementation. |
| `make-home.sh <target-dir> [--port N]` | Creates an isolated DuDuClaw home: loopback `config.toml` (default port 18977) and two employees. `plain` has no tool allowlist (the old kind of test employee). `prod-shaped` is written like a production employee: `[capabilities] allowed_tools = ["mcp__duduclaw__*", "Read", "Write", "Edit", "Bash"]`, a `denied_tools` entry, `approval_required_tools`, all four `[permissions]` flags, `[budget]`, `[evolution] gvu_enabled = true`, a `CONTRACT.toml` and a `SOUL.md`. Refuses a non-empty target and any target inside `~/.duduclaw`. Writes no secrets. |
| `mcp-probe.sh <home> <agent-id> [tool ...]` | Starts the real `duduclaw mcp-server` for that employee from the command and env in its own `.mcp.json`, sends `initialize`, `tools/list` and one `tools/call` per tool (default: `tasks_list`, `memory_search`, `working_state_get`, `user_profile_get`), and prints `ok`, `REFUSED: ...` or `ERROR: ...` per call. Exits 1 when a default check fails for `prod-shaped`. |
| `google-delegated-identity.mjs` (historical, paused) | Do not run during the narrowed EXT follow-up; use the ordinary baseline below. Retained for the original evidence. Creates its own temporary home, starts a real gateway and stdio MCP servers, and exchanges signed RSA assertions with a loopback fake Google. Requires a debug build with `--features live-test-mocks` and Node 22+. Credential protection uses an in-memory store; no real keychain, model or Google account is touched. Checks shared-account compatibility, two delegated subjects, same-process account changes, domain refusal, unsigned and unproven callers, provider rejection, explicit fallback, `none`, the master switch, audit redaction and offline gateway refusal. Artifacts remain in the printed temporary folder. |
| `node google-personal-identity.mjs <guarded-binary-wrapper>` | Creates a fresh Enterprise home and a loopback OAuth/service mock. Requires Node 22+, a debug `live-test-mocks` binary and an OS sandbox wrapper that denies real credential/keychain access and all outbound traffic except loopback. Checks exact redirect/PKCE, same-session completion after JWT refresh, cross-session/user refusal, actual partial scopes, self-consent, primary Owner changes, per-call refresh/status checks, local disconnect before delayed revoke, grant coordination and encrypted/redacted storage. Reversible suspension is injected into the fresh fixture database because this base has no reversible status RPC; it verifies resolver revalidation, not a status-management API. Artifacts remain in the printed folder. This is isolated mock evidence, not real Google or OS-keychain acceptance. |
| `python3 on-behalf-of.py <home> <binary>` | After booting and stopping the isolated gateway, calls the real `prod-shaped` stdio MCP server. Checks host identity stamping on tasks and queue rows, forged tool arguments, rejection auditing, enforce refusals and absent-principal behavior. Writes disposable test rows and temporarily changes the isolated delegation policy. It calls no model; runtime delivery, real channels and human acceptance require separate checks. |

## v1.85 acceptance rerun

Run from the repository root, outside the implementation sandbox:

```bash
# Build only if the executable needs rebuilding. This suite needs no special feature.
cargo build -p duduclaw-cli --bin duduclaw
node --check scripts/live-test/v185-acceptance.mjs
node scripts/live-test/v185-acceptance.mjs --test-helpers
DUDUCLAW_BIN="$PWD/target/debug/duduclaw" node scripts/live-test/v185-acceptance.mjs --port 18985
```

A debug binary built with `--features live-test-mocks` also works; it is not
required for the RPC/MCP/identity paths exercised here. The suite does not
invoke credential-protection enablement or the Google mock routes that require
that feature. `/healthz.live_test_mocks` is recorded in `edition-*.json`.
The first run's edition refusals came from the community license ceiling,
not from the absence of `live-test-mocks`.

The fixture follows `google-personal-identity.mjs`'s disposable Ed25519-signed
OEM license setup: fingerprint the test machine under the same OS sandbox,
keep the private signing key in memory, write only the disposable license,
and pass its public key and `DUDUCLAW_EDITION=enterprise` to isolated children.
The harness verifies `system.status.edition_profile` before scenarios. An env
override alone cannot elevate a community license, even in a debug build
(`duduclaw-core/src/types.rs`, `EditionProfile::resolve`). Login uses the
loopback first-run claim once and `/api/login` on each boot; Personal-only
`/api/session/local` is not used.

This is an Enterprise **upgrade** fixture: it seeds a wholly pre-MFA,
pre-avatar identity schema before the first boot, including when avatar is
not selected (then only the bootstrap owner is seeded). The legacy MFA policy
remains off through the documented upgrade path in `mfa.rs::read_boot_policy_state`
and `boot_policy_legacy_store_allows_migration_then_reads_durable_policy`.
It does not claim to test a brand-new Enterprise identity store's strict MFA
onboarding default.

The first macOS run exposed these harness errors:

| Scenario | Evidence and correction |
| --- | --- |
| Avatar | `legacy-shape.json` has four binding fields; `google_identity.rs::migrate` deliberately adds `is_primary INTEGER NOT NULL DEFAULT 0`. `SELECT *` therefore always differs. Compare every existing user/binding field explicitly, require all added primary flags to be zero, retain avatar rules from `avatar.rs` migration tests, and still require cleared avatars/legacy fields/primary flags to survive the second boot unchanged. |
| Memory | Both messages used the same socket, employee and `conv=memory-fixture`; `webchat.rs::compose_session_id` forms a stable bucket from those values. Logs show real `tasks_create`, pinned extraction into that exact bucket, and concurrent background CLI work. A single `identity-claude.json`/lease/release per runtime was shared with that work. Each request now has a unique prompt marker, directory, atomic lease and matching job id; the WebChat connection session id is echoed on every turn. Same-session, distinct-turn, pinned-note/reference and unrelated-session isolation checks remain mandatory. |
| Claude identity | The retained `identity-claude.json` contains active human `fixture-admin`, a turn id and a full conversation session despite the reported missing fields. Its shared file could be overwritten after the failed assertion. Use per-request evidence, disable fixture autonomous evolution, and let only the matching prompt claim the job. Gateway-owned identity remains authoritative; no field is supplied or invented by the fixture. |
| Codex identity | `gateway-migration.log` says native constraints cannot enforce `approval_required_tools: Bash`; the CLI fixture was never reached. The Codex fixture now uses an empty native approval list while keeping its MCP wildcard allowlist and `WebFetch` denial. The Claude fixture retains Bash approval. |
| Wiki | `mcp.log` records successful `wiki_write` and `wiki_read`; the normal `concepts/acceptance.md` file exists. The next input, `../escape.md`, has no valid page id, so `mcp_dispatch.rs` returns JSON-RPC `-32602`, `Invalid wiki page id`, before the tool handler. Traversal/absolute-path tests now require that exact dispatcher refusal. Valid relative `.md` paths and symlink/control-file tests still exercise tool-layer refusals and verify no outside/target modification. |
| Self-add / member key | `users.bind_agent` / `users.create` returned `enterprise_edition_required`. The isolated home now has a verified Enterprise entitlement and an ordinary owner session. No product authorization gate is bypassed. |

The copied first-run `users.db` lacks the avatar column and `sessions.db`
lacks the sessions table; no WAL sidecars were included. Those copies cannot
prove the full post-migration row contents. The known binding-column mismatch
is deterministic from source, but any additional legacy data changes must
still fail the rerun. No product defect is established by the retained
artifacts; the corrected live scenarios have not been rerun in the
implementation sandbox.

Each run prints its private `ARTIFACTS` directory and retains valid, redacted
JSON observations, `legacy-after.json`, per-request `turn-*/{job,identity,chat-frames}.json`,
`fixture-invocations.jsonl`, shim/MCP/gateway logs and standalone SQLite
backups in `databases/{users,sessions}.db`. Preserve the whole directory;
copying only a live WAL-mode `.db` loses committed data still in its WAL.
Backups and the isolated home contain fixture account data and stay private.
The harness stops its owned process groups, prints PASS/FAIL/SKIP and exits 1
on failure. External DM delivery remains SKIP. Auto-install is off; the
periodic check has no disable switch, so its outbound request is routed to an
unreachable loopback proxy and confined by the inherited OS sandbox.

The second run's member-key failure was a harness false negative after
revocation, not an authentication failure: the retained audit records show
key creation/use and `member_key.revoked` at `2026-11-25T01:07:51.149626Z`;
the gateway logs show the authenticated member chat terminating immediately
afterward at `01:07:51.150152Z`. The scenario had already passed chat
`session_info`, HTTP `/api/me` and the revoke RPC before awaiting that close.
`webchat/turns/socket_tests.rs::member_key_chat_socket_closes_after_revoke_expiry_epoch_or_admin`
accepts EOF, a Close frame or a transport error on retirement. The harness
previously conflated a transport error with malformed JSON and failed before
observing the close event. It now requires actual closure within five seconds,
accepts an abnormal close only during the expected refusal phase, and still
rejects malformed JSON or subsequent chat/session data. Reconnect must also
close; only an authentication-failure frame may precede that close. Successful
evidence records both close codes and transport-error flags, and failures name
the scenario phase. Pure helper cases cover these outcomes without opening a
socket. This correction has not been live-rerun in the implementation sandbox.
The old evidence did not retain the close code or distinguish transport errors
from JSON errors; the transport diagnosis follows the shutdown path and the
existing socket contract, and the rerun must capture those details.

## Runtime fallback reproduction

`runtime-fallback-repro.mjs` observes runtime fallback using any supplied release
or development executable, with no special build features or npm dependencies.
Requires Node 20+ and macOS `sandbox-exec`/`lsof` for enforced loopback-only
network access. Run outside the implementation sandbox:

```bash
node scripts/live-test/runtime-fallback-repro.mjs --bin /path/to/v1.85.0/duduclaw
node scripts/live-test/runtime-fallback-repro.mjs --bin /path/to/new/duduclaw --scenario C-fallback-model --keep --verbose
```

Each scenario gets fresh `HOME`/`DUDUCLAW_HOME`, fake vendor CLIs, no inherited
credentials, and a random loopback port. The script sends one ordinary WebChat
turn; C first calls the real `agents.update` RPC with `fallback` set to
`grok-4.7-build-fast`. Claude missing-model output is a synthetic stream with
system/init, assistant text and an error result (no captured vendor transcript).
`A-chat-exec-error` instead emits stderr and exits non-zero without a stream.
A-dispatch is explicitly skipped:
the existing acceptance harness does not drive dispatch, and `tasks.create`
alone only inserts a task-board row; scheduler/judge execution is unverified.

Stdout is JSON Lines: one observation per scenario followed by a summary of
observed fallback attempts, with no version-specific verdict. RUN counts exclude
Claude utility calls and calls before the chat turn. Probe counts include startup:
help/version flags, auth/login/status commands, and other non-run commands
(including Codex sandbox preflight) are probes. RUN forms are Claude `-p`/`--print`,
Codex `exec`, and Grok `-p`/`--single`. Retained `invocations.jsonl` contains every
shim invocation, its kind, arguments, model environment, inherited/effective homes,
working directory and timestamp. Scrubbed subprocesses may omit `DUDUCLAW_HOME`;
the shim validates HOME and restores only its baked-in isolated home. Successful
fake version probes register Codex/Grok; no real or fake login files are needed.
`fallback_model_arg` is the last observed fallback RUN model (null if absent).
`primary_models_tried` and `fallback_models_tried` list model arguments in
invocation order, including repeated attempts. Models come from `--model`/`-m`
or Codex `-c model=...`; missing values appear in `model_extraction_errors`,
never as invented model names or null array entries.
Primary CLI fixtures fail for every requested model, including the singular
model fallback. Claude scenarios use `claude-does-not-exist-9` as preferred;
Codex scenarios use `gpt-5.3-codex`. Both keep `claude-haiku-4-5` as the initial
singular model fallback, matching the `agents.create` scaffold.
`agent_toml_model_section` preserves the actual model section, including singular
or plural fallback keys; C also records `agent_toml_model_section_before` before
`agents.update`. C also reports `agents_update_fallback_requested`,
`model_fallback_written`, `model_fallbacks_written` (null if absent), and
`grok_run_models_received` as plain observations. `diagnostics` contains every
runtime-unavailable diagnostic JSON; parse failures remain visible separately.
`explicit_fallback_logged`/`explicit_fallback_lines` retain fallback log evidence.
`fallback_attempted` requires a fallback RUN; `fallback_step_recorded` instead
checks for an `explicit_fallback` diagnostic step (successful fallback can have
no failure diagnostic). A missing fixture agent error includes the last matching
gateway parse/load log lines. Control requires a Grok RUN after the failing Codex
RUN; otherwise it reports `harness_invalid` with diagnostics and exits 2. A/C
have no fallback expectation. Other harness errors/timeouts also exit 2;
exit 0 means turns completed and any selected control was valid. Gateways are always stopped;
temporary homes/logs are removed unless `--keep` is supplied. `--verbose` prints
log paths to stderr, and repeatable `--scenario <name>` selects scenarios.
These commands have not been live-run during implementation.

## Authorization scope reproduction (K / M / U)

`authz-scope-repro.mjs` observes member-key history reads (K1–K3), Manager
budget/heartbeat access (M1–M3), and employee MCP `agent_update` authority
(U1–U4). Requires Node 20+, macOS `sandbox-exec`, `sqlite3`, `lsof`, and an
already-built executable; other operating systems fail closed before boot.

```sh
node scripts/live-test/authz-scope-repro.mjs --bin /absolute/path/to/duduclaw
node scripts/live-test/authz-scope-repro.mjs --bin /absolute/path/to/duduclaw --scenario K1 --scenario U3 --keep --verbose
```

Each run creates fresh OS/product homes, production wildcard employee
allowlists, a locally signed disposable OEM entitlement and a pre-MFA upgrade
store, following `v185-acceptance.mjs`. Scenario accounts and bindings use
Admin RPCs. PATH contains only local CLI shims plus `/usr/bin:/bin`; shims use
the running Node executable. No inherited provider credentials are passed.
Only generated internal MCP credentials and employee identity are forwarded
from each employee's registration. An inherited OS sandbox blocks external
network, real CLI fallback paths and access to the operator's product/Claude
homes and keychains. Gateway, MCP and fake CLI processes use isolated HOME and
DUDUCLAW_HOME. All owned process groups stop on completion or SIGINT/SIGTERM;
the temporary home is deleted unless `--keep` is supplied (its path is in the
summary). Retained homes contain disposable credentials; keep them private.

Stdout is JSON Lines: one record per executed scenario, then a summary.
`http_status` reports the WebSocket upgrade (normally 101), not the RPC
outcome; `rpc_error` and per-call success fields record that separately.
Stdio MCP has no HTTP status. No vulnerability verdict is assumed, and a
refusal is a completed observation. Summary `null` means not observed;
`false` means a completed observation did not find the named behavior.
Consult `scenario_status` and the per-call evidence. Shared setup/helper
failures emit one top-level `status: "harness_error"` record with a
`harness_error` message and exit 2; scenarios that never ran emit no records.
Exit 1 indicates scenario/cleanup errors, not an authorization verdict.
Verbose, redacted logs go to stderr. Fake CLI records distinguish `probe`
(`--help`, `--version`, `auth status`) from `run`; invocation evidence counts
only matching runs.

M2 runs before budget seeding. B has its automatic heartbeat disabled, with a
local proactive check enabled to make an explicit trigger observable. M1
seeds synthetic spend into the isolated telemetry database, as the budget
unit tests do, then calls Admin `heartbeat.trigger`. Only a budget event
written by the actual gateway gate qualifies; otherwise M1 is skipped with
the reason. M3's heartbeat runs after seeding, so budget denial can prevent a
CLI invocation even if its RPC was accepted. U4 is skipped if the tested
binary does not advertise the database grant parameter. Nothing connects to
a real model or database service. Implementation verification was limited to
`node --check`; this harness has not been live-run in the restricted sandbox.

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
tools/list  168 tools visible to prod-shaped
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

## Actual VNC authentication

After installing the dashboard dependencies, run:

```sh
node scripts/live-test/computer-vnc-auth.mjs <already-built-local-image>
```

The test creates its own labelled container with no network, a read-only root,
private tmpfs, no-new-privileges and CPU/memory limits. It checks actual RFB
password authentication twice, rejects a wrong password and the sandbox user,
checks both TCP tables for VNC listeners, then stops VNC and verifies socket
removal. Its fixture directory is retained. Cleanup verifies the random ownership
label before removing only that container. No image is pulled or published.

An RFB greeting alone cannot verify these checks. The old helper emitted the
greeting and then rejected the Unix peer named `localhost` against its
`127.0.0.1` allowlist. The corrected helper binds to loopback without that IP-only
allowlist; root-only socket permissions and VNC password authentication remain.

## Google connectors: ordinary local baseline

`google-connectors-baseline.mjs <debug-binary>` uses a new temporary home,
synthetic RSA issuer and loopback REST/MCP/model servers. On macOS, each
gateway/MCP process runs under a loopback-only sandbox; an outbound-denial
self-check must pass first. Other native platforms are not covered by this
script. The binary must be
built with `--no-default-features --features live-test-mocks`; `/healthz`
refuses any other build before credential setup. Runtime `HOME` is empty and
`PATH` contains only the provided DuDuClaw binary, so a model fallback cannot
use an installed host model CLI or its account. No production home or real
provider credential is read.

The baseline exercises the canonical 19 Google tools (mock-only writes use
normal Admin approvals), actual service GET probes, an HTTP Google MCP mount
through a local model tool loop, and unavailable-account vs empty-mailbox
status. It records synthetic observations under the printed `ARTIFACTS`
directory. A local PASS does not verify a real Google tenant, Google's own
remote MCP acceptance, keychain behavior or user acceptance.

This is separate from the historical `google-delegated-identity.mjs` race
driver. Do not use that driver's paused identity-downgrade scenarios as part
of the ordinary baseline. Its prior failures remain historical evidence.

The lifecycle helper starts every child with an explicit minimal environment,
including the initial setup shell. Owned POSIX process groups have bounded
TERM -> KILL cleanup and confirmed termination; any cleanup failure fails the
baseline. PASS lines and `baseline-results.json` are produced only after
cleanup succeeds. The fixture root is canonicalized before setup so the
mock credential store uses one home path.

```bash
node --test scripts/live-test/google-connectors-harness.test.mjs
node scripts/live-test/google-connectors-baseline.mjs /path/to/mock-debug-binary
```

## Third-party stdio MCP regression

Build the CLI and Rust fixture in the reviewer tree, then run:

```bash
cargo build -p duduclaw-cli --bin duduclaw --example third_party_mcp_fixture
DUDUCLAW_BIN="$PWD/target/debug/duduclaw" \
MCP_FIXTURE_BIN="$PWD/target/debug/examples/third_party_mcp_fixture" \
scripts/live-test/third-party-mcp.sh 18983
```

`results.jsonl` must contain successful `chat` and `cron` rows, including
`missing_token_refused: true`. The fixture never writes the temporary MCP
configuration or its tokens to artifacts. Negative controls deliberately add
`identity_rejected` records to the isolated `channel_failures.jsonl`.
The test uses a local Claude shim; real vendor CLI behavior and native protected
execution still require separate acceptance. Run from an operator test shell.
No production home, account, model endpoint or external MCP endpoint is used.

For the Rust regression, set `DUDUCLAW_P1_PILOT_BINARY` to the built CLI and
select `third_party_mcp_tests::isolated_spawn_regression` in the
`duduclaw-cli` library tests. It runs the full regression in a separate test
process (including Rust stdio children); without a binary it prints an explicit
SKIP. Its process helpers are registered in `scripts/ci/privileged_effects.json`.

## Google Drive write: RM-131 P15

With Node 22+, macOS `sandbox-exec`, `sqlite3`, and an already-built debug
binary with `--features live-test-mocks`, run from the repository root:

```bash
node --check scripts/live-test/google-drive-write.mjs
node scripts/live-test/google-drive-write.mjs /path/to/mock-debug-duduclaw
```

The sibling driver leaves the historical delegated identity probe unchanged.
It uses `make-home.sh`, temporary `HOME` and `DUDUCLAW_HOME`, a disposable signed
Enterprise entitlement, the legacy MFA upgrade fixture, and a production-shaped
employee with the `mcp__duduclaw__*` allowlist. A local fake Claude launches real
stdio MCP calls within authenticated WebChat turns, including an admitted
current-turn attachment. No installed model CLI or real Google credential is
used. Google issuer/REST traffic uses the existing mock-base environment seam,
including the new write transport; a loopback-only process sandbox also denies
any accidental external request. The remote Drive MCP control uses a local
HTTP MCP server and a local fake model tool loop.

Design §7 rows **3–8, 10, 11** assert tampered consent hiding/refusal, delegated
multipart upload and confirmed audit, three folder refusals with GET-only
traffic, exact trash PATCH, empty-folder move refusal, member approval/cancel/
changed-filename invalidation, separate personal write OAuth consent and read
scopes, and overgranted refresh refusal at the signed token broker and remote
MCP mount. Row 8 decides real member cards using `me.google.write_confirm`;
Telegram/LINE/Google Chat delivery and buttons are not exercised. Row 11 first
executes the mounted read tool as a positive control, then checks that the
rejected connection cannot even initialize/list/call the remote MCP.

Every mock request records method, path, query and body. Global assertions
require zero DELETE requests and no Google bearer in captured stdio/HTTP MCP
messages. Artifacts (requests, MCP wire, scenario results, mock failures and
gateway logs) remain in the printed temporary directory; issuer bodies contain
only disposable fixture grants. This script was syntax-checked only during
implementation: it has not been live-run. Other §7 rows and real Google,
external bots, native Linux and Windows acceptance are outside this driver.

## Company Google sign-in (RM-214 P20)

Requires Node 22+, `sqlite3`, and an already-built debug `live-test-mocks`
executable. Pass an **OS sandbox wrapper**, as with
`google-personal-identity.mjs`: it must deny real credentials/keychains and
all non-loopback network access for the binary and its children.

```bash
node scripts/live-test/google-signin.mjs /path/to/guarded-binary-wrapper
node --test scripts/live-test/google-signin.test.mjs
```

The bench creates isolated `HOME` (empty `os-home`) and `DUDUCLAW_HOME`, a
`make-home.sh` production-shaped employee, and
`[dashboard] public_url = "http://localhost:<port>"`. It sets
`DUDUCLAW_LIVE_TEST_GOOGLE_API_BASE` to its loopback mock, serves RSA JWKS and
RS256 ID tokens, and verifies PKCE S256 plus the exact redirect URI on every
code exchange. It never visits Google's authorization page or uses real Google
credentials. The wrapper is an operator prerequisite; the argument alone does
not enforce OS isolation.

Named scenarios cover design §7 scenarios 1–11 and 13–14; Odoo (12) is excluded.
Daily-cap checks run before other JIT accounts so the third new account can be
checked against a cap of two. The Enterprise fixture uses the existing
pre-MFA upgrade schema with MFA off and a disposable signed OEM license;
it does not validate fresh-install Enterprise MFA defaults. The direct-MFA
scenario enables policy through the real admin RPC at the end. A separate
Personal process confirms its resolved edition before checking public routes.

The script prints PASS/FAIL/SKIP with observed HTTP status and result code
(`n/a` for RPC-only or unavailable observations), exits nonzero only for FAIL,
and never prints tokens, authorization codes, cookies, nonces or client secrets.
It retains private fixture homes, gateway logs and sanitized
`google-signin-results.json` (including every token-endpoint hit's validation
booleans), and stops its owned gateways. Inspect private logs locally; do not
publish the fixture home as a production acceptance artifact.

UI button rendering, admin notification delivery, completed blank-avatar creation,
self-service linking, changed-email attestation, unlink/refresh, and full MFA
code/TOTP/preauthorization ceremonies are explicitly SKIP. Scenario 11 selects
a deliberately stale company-template revision, then only reads the member's
`onboarding.get`; it SKIPs if real maintenance has not exposed
`waiting_admin/template_changed` within 20 seconds. It does not inject avatar
state or drive retry. Personal startup or edition-identification limitations
also produce SKIP. Real Google, native keychain and platform acceptance remain
separate. The self-tests import pure helpers without a binary, subprocess,
listener, or npm dependencies.
