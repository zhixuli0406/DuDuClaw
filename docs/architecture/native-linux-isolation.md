# Native Linux isolation implementation

## Package D

Status: **partial integration, not enabled**. This change adds the gateway-owned
MCP boundary component and an adapter into the existing CLI dispatcher. It does
not activate the protected execution backend, alter existing employee MCP
configuration, or establish production employee isolation. The contract crate's
wire formats, signing bytes and hard limits are unchanged.

### Capability and connection lifecycle

`duduclaw-mcp-boundary::Boundary` creates an OS-random MAC key and epoch for one
gateway lifetime. `issue` requires a live, matching `Reserved` run from the
authenticated spawnd client. The caller supplies trusted `GrantPolicy`; the
component generates the random capability hash, nonce and jti. It keeps only
the capability hash and HMAC-SHA256 of `CapabilityRecord::canonical_bytes()`.
Neither a bearer nor the MAC is exposed over MCP, argv, env or a config file.

Pending records cannot authorize requests. After StartRun admission, package A
validates the TakeIo peer, response correlation, ancillary truncation and exact
fd plan, then passes its `SpawnResponse::Io`, control-connection identity and
McpGateway socket to `socket::McpDelivery::from_take_io`. Activation compares the
reply's run/control with the original reservation and the current running run.
It accepts only an unnamed connected AF_UNIX SOCK_STREAM endpoint, exactly once.
Socket metadata cannot itself prove which run spawnd assigned: that evidence
comes from A's authenticated response/fd handoff, not employee JSON.

Every request verifies the MAC in constant time, epoch, time interval, run,
employee, control connection, policy revision, principal generation, immutable
binding and host registry slug. `LiveAuthority` is an explicit integration seam:
its implementation must validate current state (including the binding store),
reject unreadable/disconnected authority, and never supply a cached allow after
revocation. `BindingCheckedAuthority` now composes the supplied live run/policy authority
with fresh SQLite principal/member/tombstone checks. The production A run/policy
authority is still not supplied. It must account for
key/scope, agent policy, principal and binding changes in the live policy check.
External snapshot reads occur outside the capability mutex; state is checked
again under the mutex before activation or authorization.

`revoke_run`, `revoke_control` and `revoke_all` cover cancellation, control loss
and shutdown. The pending sweep detects unused-reservation expiry and terminal
or unavailable runs. Capability expiry removes storage on the next issuance;
records are capped at 4096 per epoch. Drop revokes connection authority. A live
server rechecks every 100 ms while idle or awaiting a handler; this polling is a
backstop, not permission for A to omit explicit lifecycle revocation.

`gateway::isolation_mcp::McpBoundaryService` supplies a one-second lifecycle
sweep, attachment entry point and `McpRunSession` guard. Dropping the guard
revokes and aborts rather than detaching its task. This service is not registered
in gateway startup yet. The CLI depends on the gateway, so the service accepts
an injected `Dispatcher` trait and does not create a dependency cycle.

### Existing MCP gates and resource access

`cli::mcp_isolation::IsolationDispatcher` creates a per-employee dispatcher,
preserves the host-supplied MCP scope ceiling, initializes redaction fail-closed
and resolves the employee namespace from the trusted registry mapping. An
immutable Tokio task-local carries the verified context and registered roots.
Employee classification, audit attribution, record actor and explore lane prefer
this context. The isolated caller is never classified as an operator.

List and call both intersect the existing policy with capability grants and the
same migration allowlist. Calls still enter `McpDispatcher::dispatch_tool_call`,
including scope, name allow/deny lists, permissions, action rules, explore lane,
approvals, record authorization and redaction. The adapter never calls handlers
directly. Identity/root/cwd fields in arguments are refused.

`file_read` uses an opaque `workspace` handle and relative `path`. The server
owns the dirfd registry and checks the employee and exact workspace grant.
Before opening, it verifies nodev and uses Linux `openat2` with `RESOLVE_BENEATH`,
`RESOLVE_NO_SYMLINKS`, `RESOLVE_NO_MAGICLINKS` and `RESOLVE_NO_XDEV`. Nonblocking,
no-controlling-terminal open plus statx rejects non-regular and hardlinked files.
Reads use that same fd with the existing 512 KiB ceiling and structured-extension
refusal. `ResourceRoots::open_directory` provides the same fenced directory-fd
access for code-map/execution adapters. There is no legacy path fallback.

Round 2 adds `csv_read` and `xlsx_read` handler migration through `FileParser`,
a named E integration seam. D opens the granted fd, builds the unchanged worker
`ParseRequest`, applies a 30-second timeout and validates response correlation,
operation, byte budget and table shape. The worker returns bounded JSON table
content through `ParsedItem::Text`; D reconstructs path/table/row_count and keeps
the structured shape needed by field redaction. The old in-process CSV/XLSX
parsers reject isolated calls. E must implement the isolated parser and enforce
`TableOptions` inside the worker; this adapter is not supplied here.

`tool_policy` enumerates all 259 current tool declarations with exact names and
File/Service/Execution/Worker/Connector routes. It verifies mandatory resource
kinds, exact selected workspaces/tasks/dependencies/database tables/wiki
namespaces/URL origins/target employees, Personal/Shared connector constraints,
and every grant returned by the trusted `ResourceResolver`. Unknown names,
wildcards, empty identifiers, missing grants and unavailable resolution deny.
URL grants are now accepted only as canonical HTTP(S) origins without credentials,
path, query or fragment. This is origin authorization, not C's DNS/IP/SSRF policy.
`target_employee` is a lowercase 32-hex resource id; it does not replace the caller.
Schemas rename legacy `agent_id` selectors, remove impersonation fields and
project code_map to a workspace plus optional relative directory.

**The other 256 handlers are not fully migrated.** The CLI has a fail-closed
`ResourceToolService` seam for their implementations. Its required
`check_handler_policy` must retain existing handler-local record/audience/owner
checks, and `execute` must use only the resolved resource collection. D re-resolves
after that async check and refuses a changed collection. Shared dispatcher gates
(scope, allow/deny, permissions, action rules, explore lane, approvals, egress,
redaction) and common completion audit/usage/pruning/dashboard feedback remain
on the path. Handler-local checks are NOT automatically reused by the service
seam. No production ResourceResolver/ResourceToolService implementation is
included; therefore these tools remain hidden and refused by default. Execution
and Worker services must route through A/E, connector services must use live
binding/credential leases and C where applicable. No generic legacy handler or
gateway-uid execution fallback exists.

Round 2 also adds `isolation_egress` and a trusted `RunTurnResolver` seam. The
isolated scope routes the existing egress client directly to the gateway ledger,
checks home/employee/turn identity and never reads process credentials. Read
completion captures its identity beyond the scope; sink leases use direct
release. Missing turn mapping stays unknown/tainted and never registers a clean
turn. Approvals use RequestContext.task, and their cancellation origin comes
from the trusted session/turn mapping; missing mapping refuses approval creation
rather than falling back to process environment. Legacy transports are unchanged.

### Personal and Shared bindings

The optional `binding-store` feature provides gateway-owned SQLite tables with
immutable employee-to-Personal-owner or employee-to-Shared-tenant bindings.
Triggers reject changes, deletion/replacement and tombstone reuse. A Personal
employee has only its owner membership; a second owner, operator or viewer is
rejected by SQLite itself. Generation/revocation checks are read live. Shared
member removal atomically advances the human principal generation, preventing
old capabilities from reviving after re-addition.

Connector accounts have their own immutable Personal/Shared binding and
revocation. Authorization checks both employee/principal and connector binding;
Shared employees cannot use personal credentials. The credential caller still
needs the capability's exact connector resource grant and the legacy tool gates.
These tables are not yet connected to user administration, migration, existing
connector execution or lease revocation. Those call sites must stop runs and
leases after durable changes. The store does not protect against a compromised
gateway/database owner.

The employee still can read its own CLI login token inside its own runtime home,
as accepted in the design; neither this shim nor the boundary hides that token
from the same employee. Cross-employee OS isolation belongs to B/H.

### Bridge and protocol

The workspace binary `duduclaw-mcp-bridge` duplicates inherited fd 3 CLOEXEC,
validates the socket, closes the original slot, and pumps stdio with backpressure.
Missing/invalid fd or non-Linux fails silently without a socket-path fallback.
Host EOF terminates even while runtime stdin remains open. It has no key.

`RuntimeBridgeRegistry` is the named A/B integration seam. D generates fresh
Claude, Codex, Grok and API-loop config projections. The preparation gate binds
exact runtime/bridge digests and version, projection bytes/argv, employee/run,
root-owned ancestors, recursive readonly publication, and a matching
`FdInheritanceReceipt`. Unknown runtimes or missing/mismatched evidence refuse.
`McpBoundaryService::prepare_runtime` exposes this gate for A. B must publish the
projection; A must consume it before protected spawning. `McpChildFd` retains a
CLOEXEC duplicate in the parent and only installs fd 3 in the child pre_exec.

These APIs do not make an arbitrary vendor CLI preserve fd 3 into its MCP child.
Every exact registered CLI version still needs a real Linux inheritance and
exclusive-config acceptance receipt. The ignored root test exercises uid drop
and two execs through a shell fixture, not Claude/Codex/Grok. No runtime has been
accepted or activated by this patch.

The server bounds each request to 256 KiB and each encoded response to 4 MiB,
rejects unknown envelope fields and batches, allows one pending handler, and
refuses concurrent work rather than growing an unbounded queue. Immediate
notifications can precede another request in the same input buffer. Pending
handlers have a 300-second deadline; output has a five-second deadline. Actual
CLI inheritance, notification sequences and approval timing need runtime tests.
Each connection is capped at 120 requests per minute; tool denials also pass
the existing tool rate limiter and argument-free denial audit. Revocation is
rechecked while output is backpressured.

### Validation and handoff

No gateway/CLI/workspace build was run locally, per the shared-machine limit.
Portable tests exercise forged claims/MACs, replay, cross-run/control mismatch,
pending activation, cancellation/expiry/restart, malformed authority, resource
paths, unavailable platform behavior, bounds and SQLite constraints.
Linux-only tests add socketpair notification/EOF/revocation/concurrency, pathname
socket rejection, symlink-swap, hardlink/FIFO refusal and real bridge byte pump.
They need no root; filesystem fixtures use `/dev/shm` for a nodev mount. They
have been cross-checked for compilation, not run on Linux in this session.

Upstream commands:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store
cargo test -p duduclaw-isolation
cargo test -p duduclaw-cli mcp_isolation::tests
cargo test -p duduclaw-cli mcp::record_authz_tests
cargo test -p duduclaw-cli mcp::action_rules_tests
cargo test -p duduclaw-cli mcp_redaction
```

Required before calling Package D complete: production ResourceResolver and
ResourceToolService adapters preserving every handler-local gate; A/B lifecycle and TakeIo wiring;
runtime registry/projection and fd-inheritance validation for every CLI;
production `LiveAuthority` backed by durable policy/principal/binding state;
administration/migration/connector/credential-lease integration; live acceptance of the in-process egress
ledger; all remaining handler resource/openat2 migrations and
execution/parser routing to A/E; native Linux tests, gateway/CLI builds and
designated Daybreak security review. No contract change and no new external
dependency versions were introduced; round 2 adds a direct dependency on the
already-locked url 2.5.8 for canonical origin parsing; the component reuses locked crypto,
Tokio, rustix/libc and optional SQLite dependencies.

### Round 1 evidence (2026-11-05)

Executed on macOS in the supplied worktree, with no commit and no cargo fmt:

| Command | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store --offline` | First run: 17 passed, 1 failed (test input did not exceed JSON output cap). Corrected the boundary fixture; subsequent run: 19 passed. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary -p duduclaw-isolation --features duduclaw-mcp-boundary/binding-store --offline` | 50 passed: 29 unchanged contract tests, 20 boundary unit tests, 1 unsupported-platform bridge test; 0 failures. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-mcp-boundary --all-targets --offline` | Passed after fixing the raw statx mask check. Includes Linux tests and bridge compilation; default features, without optional SQLite store. Unsafe warnings are the documented fd handoff and test pre_exec blocks. No Linux execution. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-mcp-boundary --all-targets --offline` | Passed, default features. No Windows execution. |
| `git diff --check` | Passed. |

An independent read-only code review found fd ownership, notification ordering,
nodev, lock ordering, Linux fixture and denial-rate-limit issues; these were
corrected and re-reviewed. This is not the designated Daybreak security review.
There are eight new CLI tests under `mcp_isolation::tests`, not executed locally.
Public security feature pages are unchanged because this component is not
startup-enabled and adds no deployed isolation guarantee.

Round 1 file inventory (paths relative to the repository root):

| File | Change |
| --- | --- |
| `Cargo.toml` | Register boundary workspace member/dependency. |
| `Cargo.lock` | Add boundary and local dependency edges; no external version updates. |
| `CHANGELOG.md` | Record the still-disabled isolation foundation. |
| `crates/duduclaw-mcp-boundary/Cargo.toml` | New small library/bridge crate and optional binding store. |
| `crates/duduclaw-mcp-boundary/README.md` | Component usage and integration limits. |
| `crates/duduclaw-mcp-boundary/src/lib.rs` | Modules and argument authority checks. |
| `crates/duduclaw-mcp-boundary/src/authority.rs` | MAC, lifecycle, live verifier, revocation and sweep. |
| `crates/duduclaw-mcp-boundary/src/authority_tests.rs` | Claim, lifecycle and Linux connection tests. |
| `crates/duduclaw-mcp-boundary/src/bindings.rs` | Durable Personal/Shared/member/connector constraints. |
| `crates/duduclaw-mcp-boundary/src/bindings_tests.rs` | Raw SQL bypass and live-generation regression tests. |
| `crates/duduclaw-mcp-boundary/src/config.rs` | Secret-free bridge projection builder and tests. |
| `crates/duduclaw-mcp-boundary/src/resources.rs` | Granted dirfd lookup and Linux openat2/statx. |
| `crates/duduclaw-mcp-boundary/src/resources_tests.rs` | Path, grant, symlink-swap and special-file tests. |
| `crates/duduclaw-mcp-boundary/src/socket.rs` | Typed TakeIo receipt and unnamed socket validation. |
| `crates/duduclaw-mcp-boundary/src/server.rs` | Bounded JSONL, per-call verification, cancellation and deadlines. |
| `crates/duduclaw-mcp-boundary/src/bin/duduclaw-mcp-bridge.rs` | Fixed-fd stdio byte pump. |
| `crates/duduclaw-mcp-boundary/tests/bridge.rs` | Invalid-fd and Linux subprocess bridge tests. |
| `crates/duduclaw-cli/Cargo.toml` | Depend on contracts and boundary. |
| `crates/duduclaw-cli/src/lib.rs` | Export the isolation adapter. |
| `crates/duduclaw-cli/src/mcp_isolation.rs` | Trusted-context adapter into shared dispatch gates. |
| `crates/duduclaw-cli/src/mcp_isolation_tests.rs` | Eight adapter, identity, file and rate/audit tests. |
| `crates/duduclaw-cli/src/mcp_dispatch.rs` | Capability narrowing, trusted lane, employee classification, denial audit. |
| `crates/duduclaw-cli/src/mcp/tools_list.rs` | Same capability filter for discovery. |
| `crates/duduclaw-cli/src/mcp/record_authz.rs` | Trusted isolated employee record actor. |
| `crates/duduclaw-cli/src/mcp/util.rs` | Trusted isolated audit attribution. |
| `crates/duduclaw-cli/src/mcp_files.rs` | Bounded file_read from the authorized fd. |
| `crates/duduclaw-cli/src/mcp_redaction.rs` | Explicit host identity/session initialization. |
| `crates/duduclaw-gateway/Cargo.toml` | Depend on boundary and binding store. |
| `crates/duduclaw-gateway/src/lib.rs` | Export the disabled integration seam. |
| `crates/duduclaw-gateway/src/isolation_mcp.rs` | Gateway service/sweep and abort-on-drop run guard. |
| `docs/README.md` | Link this implementation note. |
| `docs/architecture/native-linux-isolation.md` | Component behavior, exact evidence and remaining work. |

### Round 2 evidence and remaining work (2026-11-05)

Status: **PARTIAL**, not enabled. This round did not finish every item requested.
It completes the D-side resource policy registry, table-worker adapter,
config/descriptor handoff APIs and in-process egress identity wiring. It does
not complete the 256 non-file tool implementations or their handler-local gates.
Those are outstanding implementation work, not merely unrun tests. The defaults
refuse them. The gateway startup and every real runtime remain unintegrated.

Executed locally on macOS, only within the supplied worktree:

| Command | Result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary -p duduclaw-isolation --features duduclaw-mcp-boundary/binding-store --offline` | Final run: 71 passed (29 contract, 41 boundary, 1 bridge unsupported-platform test), 0 failed. Earlier run: 36 boundary passed / 1 failed because the inventory-test parser assumed whitespace after `name:`; fixed the parser, preserving exact catalog coverage. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-mcp-boundary --all-targets --offline` | Passed, default features. Includes compilation of both ignored root tests. Expected unsafe warnings for documented fd and mount syscalls. No Linux execution. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-mcp-boundary --all-targets --offline` | Passed, default features. No Windows execution. |
| `git diff --check` | Passed. |

Remote normal checks (gateway/CLI intentionally not built locally):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store
cargo test -p duduclaw-isolation
cargo test -p duduclaw-cli mcp_isolation::tests
cargo test -p duduclaw-cli mcp::record_authz_tests
cargo test -p duduclaw-cli mcp::action_rules_tests
cargo test -p duduclaw-cli mcp_redaction
cargo test -p duduclaw-cli mcp_dispatch
cargo test -p duduclaw-gateway isolation_egress::tests
cargo test -p duduclaw-gateway egress_client::tests
cargo test -p duduclaw-gateway egress_guard::tests
cargo test -p duduclaw-gateway approval::turn_origin::tests
```

Run as root on the Ubuntu 22.04/24.04 and Debian 12 acceptance VMs:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store -- --ignored iso_root_ --test-threads=1
```

This selects `resources::tests::iso_root_openat2_mount_crossing_devices_and_nodev`
and `iso_root_bridge_fd_survives_uid_drop_and_two_execs`. Neither was executed
locally. Real vendor CLI acceptance must separately prove that each exact
manifest version preserves fd 3 into its MCP child and loads only the projected
server definition. The shell/uid fixture cannot issue those receipts.

Independent static review found schema/identity mismatch, process-env approval
attribution, skipped completion audit/feedback, and connector I/O inside the
ledger mutex. These were fixed and re-reviewed. The review still identifies the
missing production service/record-policy implementations. This was not a
`gpt-daybreak-blue-latest` security review; that remains unperformed.

No changes to isolation wire formats, canonical signing bytes or hard limits.
No new external dependency versions; `url = "2"` resolves to the already-locked
2.5.8. No wiki notes were added; scanning `wiki/impl` and `wiki/reports` found no
Package D handoff notes to move. No commit, release, deployment, remote execution,
worktree creation/removal, or cargo fmt was performed.

Round 2 file changes (in addition to the retained round 1 working tree):

| File | Change |
| --- | --- |
| `crates/duduclaw-mcp-boundary/src/tool_policy.rs` | Exact 259-tool resource policy, immutable authorized calls, resolver seam and canonical origins. |
| `crates/duduclaw-mcp-boundary/src/tool_policy_tests.rs` | Catalog and resource/selector/connector/origin regression tests. |
| `crates/duduclaw-mcp-boundary/src/parsing.rs` | fd-only FileParser seam and bounded table-result validation. |
| `crates/duduclaw-mcp-boundary/src/parsing_tests.rs` | Worker correlation, bounds and table-shape tests. |
| `crates/duduclaw-mcp-boundary/src/config.rs` | Native runtime projections and registry/publication/inheritance receipt checks. |
| `crates/duduclaw-mcp-boundary/src/config_tests.rs` | Projection identity and refusal matrix. |
| `crates/duduclaw-mcp-boundary/src/config_fd.rs` | Child-only inherited socket installation; parent keeps CLOEXEC. |
| `crates/duduclaw-mcp-boundary/src/resources.rs` | Directory fd support and shared resource validation at issuance. |
| `crates/duduclaw-mcp-boundary/src/resources_tests.rs` | Connector mismatch plus ignored root mount/device tests. |
| `crates/duduclaw-mcp-boundary/src/bindings.rs` | BindingCheckedAuthority wrapper for live durable constraints. |
| `crates/duduclaw-mcp-boundary/src/bindings_tests.rs` | Wrapper observes durable revocation. |
| `crates/duduclaw-mcp-boundary/src/lib.rs` | Export new modules and remove obsolete file_read-only predicate. |
| `crates/duduclaw-mcp-boundary/src/authority_tests.rs` | Use current explicit route policy in regression assertions. |
| `crates/duduclaw-mcp-boundary/src/bin/duduclaw-mcp-bridge.rs` | Shared fd constant and close inherited original after validation. |
| `crates/duduclaw-mcp-boundary/tests/bridge.rs` | Owned-fd helper, named-socket rejection and ignored uid/exec fixture. |
| `crates/duduclaw-cli/src/mcp_isolation.rs` | Service/worker/turn builders, schema projection and authorization handoff. |
| `crates/duduclaw-cli/src/mcp_isolation_tests.rs` | Service-policy and schema regressions, updated context fixtures. |
| `crates/duduclaw-cli/src/mcp_file_isolation_tests.rs` | Missing worker, fd confinement and forged table-result tests. |
| `crates/duduclaw-cli/src/mcp_files.rs` | Async isolated CSV/XLSX path and no legacy parser fallback. |
| `crates/duduclaw-cli/src/mcp/dispatch.rs` | Route isolated handlers through the common completion path. |
| `crates/duduclaw-cli/src/mcp/approval.rs` | Task audience correlation from immutable context, no env fallback. |
| `crates/duduclaw-gateway/src/isolation_mcp.rs` | Expose runtime preparation seam. |
| `crates/duduclaw-gateway/src/isolation_egress.rs` | Trusted run-to-turn/session scope and two remote-only tests. |
| `crates/duduclaw-gateway/src/egress_client.rs` | In-process ledger routing and captured direct read/sink lifecycle. |
| `crates/duduclaw-gateway/src/egress_guard.rs` | Direct ledger operations retaining existing policy/connector checks. |
| `crates/duduclaw-gateway/src/approval/turn_origin.rs` | Isolated approval origin precedes legacy feedback/env lookup. |
| `crates/duduclaw-gateway/src/lib.rs` | Export isolated egress scope. |
| `crates/duduclaw-mcp-boundary/Cargo.toml` | Direct dependency on already-locked URL parser. |
| `Cargo.lock` | Record that dependency edge. |
| `crates/duduclaw-mcp-boundary/README.md` | Current adapter usage and incomplete integration boundary. |
| `CHANGELOG.md` | Describe still-disabled additions without an isolation guarantee. |
| `docs/architecture/native-linux-isolation.md` | Current behavior, test evidence and explicit incomplete scope. |

### Round 3 handoff (2026-11-05)

Status: **PARTIAL — the requested production wiring of all 256 non-file tools
is NOT complete.** This section supersedes the round-2 execution description,
not its outstanding integration/acceptance gates. No commit or startup activation.

The adapter now separates an effect classification (`tool_effect::effect_of_builtin`)
from its execution route. Current source inventory: **259 ToolDefs, 259 exact
policy entries, zero missing names/effects**. Routes are 62 `Records`, 111
`Resource`, 83 `Execution`, and 3 `File`. These are routing counts, **not counts
of working production tools**. The CLI's `mcp/tools_def/mod.rs` now re-exports
`duduclaw_core::mcp_tool_defs`; the new enumeration test iterates that actual
registry rather than counting source strings.

- `Records` calls use the original `mcp/dispatch.rs` match and handlers after
  the existing scope/allow-deny/permissions/action-rules/approval/egress gates.
  They no longer need a duplicate `ResourceToolService::check_handler_policy`
  or a separate handler implementation. They still require an injected live
  `ResourceResolver`; `tool_policy::authorize` checks the signed resource set
  and selected task IDs before the original handler runs. No production
  resolver has been supplied. Absence stays hidden/refused. The existing
  handler retains RecordActor, namespace, audience and ownership checks.
- Collection tools (`tasks_list`, `goals_list`, `activity_list`, `list_agents`,
  `check_responses`, cron/reminder/decision/autopilot lists, `mail_list`,
  `responsibility_get` and `plan_get`)
  remain `Resource`: a resolver snapshot followed by an original handler's
  independent query would admit rows added after authorization. They need
  authorized IDs consumed in the query or a consistent snapshot. Similarly,
  `cost_*` remains `Resource` because the existing handlers query global
  gateway telemetry without an employee predicate. `identity_resolve` stays
  Resource because its provider may call Notion, requiring connector binding.
- `ExecutionToolBridge` concretely calls A's `ExecutionBackend`: existing
  handler authorization through `ExecutionPlanFactory`, reserve, prepare,
  validate employee/run/registered runtime/class/workspace, spawn. It never
  launches a local process or waits on the same employee. It returns
  `accepted` / `completion: pending`, not a fabricated synchronous tool result.
  Preparation failures cancel the reservation; cancellation of the request
  also attempts cleanup. Spawn failures remain uncertain. A still owns
  authoritative lifecycle cancellation. Worker plans must be offline.
  **No production `ExecutionPlanFactory` exists in this worktree**; it must
  reuse actual handler policy and provide trusted registered operations.
- `hardware_info`, `model_search`, and `model_recommend` use Execution despite
  their read effect: `detect_hardware` can start `nvidia-smi` / `rocm-smi`.
  Shared approval judges can also invoke CLI utility runtimes. Under the
  isolated request scope, `run_utility_prompt_with_hint` now returns an error
  before runtime selection, retaining the existing fail-closed escalation.
  A utility-backend integration is still needed; no gateway-uid fallback.
- `mcp_isolation_identity` is a task-local compatibility projection, never
  `set_var`. Existing identity helpers and handler reads get the verified
  employee/home/task/lane/principal/turn. Signed loopback clients may read an
  existing authenticated internal key and mint the employee's existing HMAC
  identity token; neither key creation nor rotation occurs in this adapter.
  Unknown environment fields are absent, including gateway provider secrets.
- Gateway `isolation_egress` creates a request-lifetime principal-context
  reference in the existing registry from the capability. It does not capture
  ambient attendance, another runtime's task/run, or a gateway principal.
  Drop removes the reference. This allows existing human-rights loopback checks
  to resolve the employee's context without inventing a new permission system.
- `target_employee` is translated to the original handler's `agent_id` only
  after wire validation and only for the capability's own employee. A live
  registry mapping is still required for cross-employee selectors. Missing
  delegation depth/hop metadata stays at 255 (deny), never reset to zero;
  memory writes without a host-resolved conversation stay refused.

Remaining implementation, not merely unrun tests:

1. Production record resolver plus use-time collection constraints; file/wiki/
   skill adapters beyond `file_read` and the fd-only table parser seam. Round 2
   did **not** convert all wiki/skill pathname I/O to openat2.
2. Connector credential selection and live account/binding checks. Legacy
   Google can select the home account, and channel/Notion/GitHub/Odoo handlers
   can use shared gateway credentials. They are not made safe just by selecting
   the employee; the default Resource route still refuses them.
3. A/E execution plan factories and utility execution, host delegation metadata,
   cross-employee target mapping, and background provenance. No executable path
   or host argv is accepted from an employee to fill these gaps.
4. Existing startup, live authority, runtime fd receipts, native Linux tests,
   and designated Daybreak security review remain outstanding.

Round-3 local verification (no gateway/CLI build, no cargo fmt):

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store --offline execution::tests -- --nocapture` | 7 passed, 0 failed (macOS). |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary --features binding-store --offline` | 48 unit + 1 integration passed, 0 failed (macOS); 0 doc tests. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-mcp-boundary --features binding-store --target x86_64-unknown-linux-gnu --offline` | Failed before Rust crate checking: bundled SQLite requires missing `x86_64-linux-gnu-gcc`. |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-mcp-boundary --target x86_64-unknown-linux-gnu --offline` | Passed without binding-store; existing documented unsafe-fd warnings. Compilation only, not Linux execution. |
| Python exact ToolDef/policy/effect source inventory | 259 unique definitions and matching policies; no missing effects. The Rust enumeration test below remains unrun. |
| `git diff --check` | Passed. |

Remote test filters (run in addition to the round-2 list):

```sh
cargo test -p duduclaw-cli isolated_every_tool_def_has_exact_boundary_route_and_effect
cargo test -p duduclaw-cli isolated_record_selector_cannot_escape_signed_task_grant
cargo test -p duduclaw-cli isolated_record_route_reuses_original_dispatch_and_handler
cargo test -p duduclaw-cli mcp_isolation::tests
cargo test -p duduclaw-cli mcp_isolation_identity::tests
cargo test -p duduclaw-cli mcp::agent_identity_tests
cargo test -p duduclaw-cli mcp::record_authz_tests
cargo test -p duduclaw-cli mcp::round_parent_tests
cargo test -p duduclaw-cli mcp_namespace
cargo test -p duduclaw-cli mcp_memory_handlers
cargo test -p duduclaw-cli mcp_dispatch
cargo test -p duduclaw-gateway isolation_egress::tests
cargo test -p duduclaw-gateway on_behalf_of::context::tests
cargo test -p duduclaw-gateway approval::turn_origin::tests
cargo test -p duduclaw-mcp-boundary --features binding-store execution::tests
```

Round-3 file changes (rounds 1–2 are retained):

| File | Change |
| --- | --- |
| `crates/duduclaw-cli/src/mcp_isolation.rs` | Exact effect/route classification; original record handler path, resource validation, A bridge injection, own-target translation. |
| `crates/duduclaw-cli/src/mcp_isolation_tests.rs` | Actual ToolDef inventory, Task A/B negative test, original dispatch and deny-gate test; update service fixture. |
| `crates/duduclaw-cli/src/mcp_isolation_identity.rs` | Immutable request identity/environment compatibility; concurrency and original soul-permission tests. |
| `crates/duduclaw-cli/src/mcp_isolation_execution.rs` | Re-export the small boundary execution bridge. |
| `crates/duduclaw-cli/src/lib.rs` | Register identity and execution modules. |
| `crates/duduclaw-cli/src/mcp/dispatch.rs` | Preserve original handler dispatch and translate only verified resource selectors. |
| `crates/duduclaw-cli/src/mcp/agent_admin.rs` | Soul permission reads request identity. |
| `crates/duduclaw-cli/src/mcp/autopilot_skills.rs` | Request-local employee claim. |
| `crates/duduclaw-cli/src/mcp/computer_use_client.rs` | Request-local signed-client context and turn metadata. |
| `crates/duduclaw-cli/src/mcp/config_read.rs` | Verified employee/home branch before legacy environment fallback. |
| `crates/duduclaw-cli/src/mcp/decision_tools.rs` | Request-local audit home. |
| `crates/duduclaw-cli/src/mcp/delegation.rs` | Request-local channel/turn/hop/lane inputs. |
| `crates/duduclaw-cli/src/mcp/discovery.rs` | Request-local identity claims. |
| `crates/duduclaw-cli/src/mcp/google_identity_client.rs` | Request-local identity/environment inputs; connector execution still gated separately. |
| `crates/duduclaw-cli/src/mcp/group_history.rs` | Verified employee branch with caller/home agreement. |
| `crates/duduclaw-cli/src/mcp/knowledge_client.rs` | Request-local signed-client identity and lineage. |
| `crates/duduclaw-cli/src/mcp/meeting_tools.rs` | Request-local internal credentials and turn. |
| `crates/duduclaw-cli/src/mcp/mod.rs` | Request-local delegation metadata. |
| `crates/duduclaw-cli/src/mcp/on_behalf_of.rs` | Existing human-rights checks use capability principal and guarded context reference. |
| `crates/duduclaw-cli/src/mcp/skills_lifecycle.rs` | Isolated calls do not inherit provider keys or alternate event paths. |
| `crates/duduclaw-cli/src/mcp/spawn.rs` | Request-local employee/turn/delegation/lane inputs. |
| `crates/duduclaw-cli/src/mcp/tasks.rs` | Parent task comes from the capability. |
| `crates/duduclaw-cli/src/mcp/workflow_operation.rs` | No ambient workflow-session identity in an isolated call. |
| `crates/duduclaw-cli/src/mcp_memory_handlers.rs` | Request-local provenance/home; absent linkage refuses writes. |
| `crates/duduclaw-cli/src/mcp_namespace.rs` | Capability employee verification without process credentials. |
| `crates/duduclaw-cli/src/mcp_os_ops.rs` | No ambient reply-channel attribution. |
| `crates/duduclaw-gateway/src/isolation_egress.rs` | Principal context lifetime and exported request identity; revoke/utility refusal tests. |
| `crates/duduclaw-gateway/src/on_behalf_of/context.rs` | Explicit capability registration without ambient attendance capture. |
| `crates/duduclaw-gateway/src/runtime_dispatch.rs` | Refuse legacy utility execution in isolated MCP scope. |
| `crates/duduclaw-mcp-boundary/src/lib.rs` | Register execution module. |
| `crates/duduclaw-mcp-boundary/src/execution.rs` | Concrete A backend handoff and reservation cancellation. |
| `crates/duduclaw-mcp-boundary/src/execution_tests.rs` | Seven backend/plan/cancellation tests. |
| `CHANGELOG.md` | Keep the feature explicitly unfinished and disabled. |
| `docs/architecture/native-linux-isolation.md` | This handoff, evidence, remote filters and unresolved production work. |

An independent code review found the initial record-route resource bypass,
global telemetry exposure, and collection snapshot problem. Signed-resource
checks were restored, global/collection handlers remain on Resource, and the
service test was corrected. This was a general code review, not the required
Daybreak security review. No contract schema, signing bytes, hard limits or
new dependency version changed in round 3.

## Package D round 5 — employee feature parity

This section supersedes the restrictions and finite record-grant semantics in
rounds 1–4 above. Those sections record earlier implementation history, not the
current product contract. The current [tool matrix](native-linux-isolation-tools.md)
enumerates all 259 ToolDefs and the 34 host-authority exceptions.

### Current dispatch contract

- `GatewayHandler` calls continue through the original `McpDispatcher` and
  `mcp/dispatch.rs` handler. Odoo, channels, delegation, cron/reminders, mail,
  database sources, global collections, wiki publishing/cross-namespace and
  knowledge tools retain existing employee gates. Google/Notion/GitHub also
  use the original account-selection handlers, including delegated/personal
  account consent; the round-4 bound-native-only restriction no longer applies.
- Scope, allow/deny lists, `[permissions]`, `action_rules`, approvals, record
  ownership/delegation/audience, egress/private-data, input guards and redaction
  stay on the original path. Admin/operator denials come from these gates;
  there is no isolation-specific admin tool blacklist. A key's Admin scope
  never changes the verified employee `RecordActor` to Operator.
- `RequestContext` supplies caller identity. Original schema target selectors
  such as `agent_id` stay intact and go through the original target policy;
  they do not replace that identity. The call capability does not turn employee
  records into per-run records. `tasks_list`, `activity_list`, global goal lists
  and mail ownership use the same rules as non-isolated installations.
- `tasks_create(schedule=...)`, cron and reminder operations create/manage the
  original employee-owned records. No run capability is stored in those rows.
  Later executions require fresh gateway admission and go through the normal
  isolated `ExecutionBackend` integration; expiration of the creating call
  does not delete employee schedules or transfer their ownership.
- `computer_*` calls use the original gateway-owned container handlers.
  Computer workspaces keep their own lease, owner credential, revision and
  approval protocol; they are not replaced by run workspace handles.
- Employee execution still uses A's `ExecutionBackend` and operation factory;
  hostile parsing still uses E and fd inputs. File routes retain their fd-only
  workspace boundary. Missing execution/parser factories fail boot readiness,
  not a new product permission rule. Utility judges retain original provider/model/autonomy/budget checks, then
  use the host `UtilityRunner` execution seam. CLI utilities can retain Read
  tools, so they must not fall through to a gateway-uid subprocess.
- Host desktop/device/power/network/system-service tools are the only explicit
  isolation refusals. The exact set is in the matrix. Host model lifecycle and
  shared co-drive desktop tools are included; computer containers are excluded.

### Trusted run metadata

`RunTurnResolver::metadata` supplies original delegation/hop depths, origin,
reply routing and memory provenance from the gateway's run mapping. It must
preserve nested depths, not reset every call to zero. `RunMetadata::root()` is
an explicit host decision for a root run. Missing metadata cannot inherit
another request's process environment. Original turn/session and dispatch-run
provenance remain paired; background work is not forced to have a conversation.
The request scope also binds the existing channel reply task-locals. An attended
computer run can reuse the original host-owned principal context through
`RunTurnResolver::principal_context`; the boundary rechecks employee, human
principal, task, dispatch run and attendance turn, and leaves lifetime/revocation
with the original runtime guard. A turn id alone never manufactures attendance.

The A boot integration must install its real live resolver and populate this
metadata when it admits a run, plus a `UtilityRunner` for policy judges.
Optional workflow sessions are supplied with `with_workflow_session(run, id, secret)`
and only attached to that verified run; secrets stay gateway-side.
This worktree contains the consumer contract and
regression fixtures, not A's production admission wiring. Without that wiring,
readiness of the tool inventory alone is not a runnable-install acceptance.

### Validation and remote filters

Local validation for round 5:

- `cargo test -p duduclaw-mcp-boundary --lib --offline`: **58 passed**; this does
  not compile gateway/CLI and does not establish end-to-end feature parity.
- Rust parser validation with output discarded, and `git diff --check`.
- No gateway/CLI build or tests were run locally. Linux acceptance `connect.py
  check` failed before connection with `Operation not permitted` in this sandbox.
  The remote tests below have **not** been executed here.

Run on the Linux integration checkout containing A/E and these uncommitted edits:

```bash
cargo test -p duduclaw-cli --lib isolated_round5_ -- --test-threads=1
cargo test -p duduclaw-cli --lib mcp_isolation::tests -- --test-threads=1
cargo test -p duduclaw-cli --lib mcp_isolation_identity::tests -- --test-threads=1
cargo test -p duduclaw-cli --lib isolated_collection_handlers_preserve_employee_visibility
cargo test -p duduclaw-cli --lib record_authz -- --test-threads=1
cargo test -p duduclaw-cli --lib action_rules -- --test-threads=1
cargo test -p duduclaw-cli --lib odoo_pool_dispatch_tests -- --test-threads=1
cargo test -p duduclaw-cli --lib google_identity_client -- --test-threads=1
cargo test -p duduclaw-gateway --lib isolation_egress::tests -- --test-threads=1
cargo test -p duduclaw-gateway --lib utility -- --test-threads=1
cargo test -p duduclaw-mcp-boundary --features binding-store
```

The parity regression compares real employee discovery under wildcard tool
permission against isolation, with the documented host set as its only allowed
difference. A/E doubles are used only for readiness and panic on execution;
representative original-handler dispatch and policy-denial comparisons are
separate tests. These fixtures are not provider calls, execution acceptance,
or proof that the eventual protected boot has supplied every operation factory.

No commits, merges, deployment or production-data changes were made.
