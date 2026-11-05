# duduclaw-mcp-boundary

Gateway-owned MCP connections for isolated Linux employees. The library signs
the shared contract's canonical capability records, binds each reservation to
one delivered socket, checks live authority on every request, and opens granted
workspace files without following links. `duduclaw-mcp-bridge` forwards stdio to
the inherited unnamed socket at fd 3; it has no secrets or socket pathname.

This is an integration component, not enabled isolation. Package A must provide
authenticated reservation/TakeIo readbacks and lifecycle events through the
`LiveAuthority` seam. Service/Connector calls enter a typed resource boundary:
exact grants are checked before handlers, while explicit EmployeeScope uses the
verified employee's current product authority. File/A/E still require exact
confinement grants. GatewayResourceResolver supplies live record/account policy;
connector selection and execution require installed adapters. Endpoint startup
must pass `IsolationDispatcher::require_complete_services`. Missing connector
adapters fail startup, not silently fall back to the home-account handler.
CSV/XLSX use the injected fd-only parser adapter. Full product parity, production
boot wiring and live-provider acceptance remain incomplete; see the
[security-fix report](../../docs/architecture/native-linux-isolation-security-fixes-2026-11-05.md).

## Usage

Build on Linux with `cargo build -p duduclaw-mcp-boundary`. Run the bridge only
inside a spawnd-created run with the contract fd layout. Direct invocation
without the inherited socket fails. Non-Linux invocation fails closed.

`Boundary::issue` consumes trusted host policy, never JSON-RPC arguments.
`Boundary::activate` consumes the socket from a validated TakeIo response.
`Connection::context` rechecks live run, binding, policy and principal state.
Cancel/revoke before stopping a run; drop the connection on EOF. The authority
owns a sticky watch channel per capability. Revocation, control loss, connection
drop and failed live verification notify the server, cancelling its in-flight
handler without waiting for the 100 ms backup poll. Lifecycle/policy owners must
publish revocation through `Boundary` when changing authority.

Verified contexts carry a live authority verifier tied to the exact active
connection nonce and record. Adapters call `RequestContext::verify_live` immediately
before irreversible I/O, including credential use and retries. A detached
record-only context refuses this check. `SelectedConnector::recheck` combines
this live run/policy/principal check with account binding and catalog checks;
revocation cannot undo an I/O operation already sent to an external service.

`io_authority::scope` carries the same context through legacy handler awaits.
TaskStore insert/update/delete, claim/lease mutations and completion now call
`verify_before_io` after lock acquisition and before SQL mutation/commit; a
revoked transaction is rolled back. Host calls outside the isolation scope keep
their existing gates. Tokio child tasks do not inherit this scope: adapters that
spawn must explicitly re-wrap the child future. `memory_store` uses a checked memory-engine API that verifies after its own
awaited mutex and before commit, rolling back temporal and lineage rows on
revocation. Agent and shared wiki writes recheck after the delivery-fence wait
and before page/scaffold/index/log effects. Both publish pages through a checked
helper which rechecks after staging fsync, immediately before rename; activity
inserts recheck after their store lock. Shared wiki index updates also recheck
after `_index.lock` and before final rename; log appends check before opening and
writing. Other memory mutation APIs, wiki FTS transactions and remaining service
mutation points still require a separate coverage audit.

The optional `binding-store` feature provides SQLite lifetime binding
constraints. Its database must live in the new gateway-owned data directory.
`BindingCheckedAuthority` composes these checks with A's live run/policy authority.

Connector selection requires an installed `LiveConnectorSelectionPolicy` in the
gateway resource resolver. It supplies concrete Personal or SharedHome accounts
after applying today's integration, employee-config, requester and consent rules.
EmployeeScope selects exactly one permitted registered account; explicit resource
grants intersect those same live candidates. Rechecks repeat this policy and
preserve the selected account and binding. Personal credentials require the same
Personal employee owner; Shared employees cannot cross tenants. A Personal employee
may use a SharedHome account only through an explicit live product-policy selection.
The seam does not implement or replace provider-specific consent/selection adapters;
missing adapters remain unavailable and must fail boot readiness.

`config::prepare_runtime_bridge` requires an exact-version root registry receipt
for readonly config publication and real MCP-child fd inheritance. The supplied
fd helper and shell fixture do not constitute vendor CLI acceptance.

`factories::GatewayFactories::assemble` pairs the injected A `ExecutionBackend`
and `ExecutionPlanFactory`. Both factory methods receive only an
`AuthorizedToolCall`. `ResourceRoots::authorize_workspace` mints an opaque
`AuthorizedWorkspace` from the exact selected grant and retained directory fd.
An `AuthorizedExecutionPlan` can only be constructed with that token; admission
compares the call, arguments, selected resources, workspace handle and directory
identity again. Another workspace in the same capability does not qualify. An explicit
`workspace`/`workspace_id` selector must match the token; without a selector the
resolver must select exactly one workspace.
Supplying neither factory keeps execution unavailable; supplying only one is an
error.

Protected tool execution uses `ExecutionBackend::spawn_authorized`, carrying the
workspace token into the backend. The backend must compare its actual retained
execution root with `ExecutionAuthority::verify_workspace` and verify live
authority after waits, before handoff. `supports_authorized_workspace` defaults
to false and the default hook refuses; there is no fallback to `spawn`. SpawndBackend has an explicit root-workspace receipt hookup but still needs
the production B/H receipt producer installed, so these MCP execution tools remain unavailable at startup. This is unfinished
functionality, not completed native execution acceptance.

Worker routes use the separate `worker_execution::WorkerToolBridge`, never A's
employee execution path. `AuthorizedWorkspace::open_file` requires the input path
selected by the call (`path`, `file_path`, `input_path` or `source_path`), opens it
with the existing constrained openat2 path, and returns an opaque `AuthorizedFile`
with the selected grant and dev/inode. `WorkerPlan::new` accepts only that type.
The bridge rechecks its call binding and inode before handing the descriptor to
E. Arbitrary factory-provided regular-file descriptors and a token from another
call are refused. Derived snapshots require a separately reviewed adapter; there
is no raw-FD fallback. Factories retain original handler policy checks, and live
authority is verified immediately before A spawn, E invocation and result effects.
Missing factories remain absent from discovery and refused on calls.

File/table routes also use the selected `AuthorizedFile`; `FileParser::parse`
takes the typed call and token instead of a raw descriptor. The token retains
live authority through every input snapshot read. A per-invocation cancellation
marker fences queued blocking work when its async request is dropped or times
out, independently of the connection's capability lifetime. Worker finalization
runs inside the original `io_authority` scope. Spawned async and blocking work
must explicitly use `io_authority::capture().scope(...)` or `.run(...)`.

E launchers must advertise checked transport support before installation.
Protected calls use E's `invoke_output_reader_checked` and checked session
exchange, revalidating after transport waits and immediately before socket I/O.
Cancellation is not a worker teardown receipt; E retains its admission lease
until its own teardown rules permit release.

On the integration branch, the gateway's `isolation-worker-integration` feature
builds the direct Package E adapter. Trusted boot calls
`isolation_execution::install_worker_services` with a `WorkerLauncher` once,
then uses its `.parser` for CSV/XLSX and `.tools(factory, resolver)` for worker
operations. E owns `WorkerSession`, admission, sealed snapshots and teardown;
the gateway does not enable E's `engine` feature. This feature needs the E
dependency already present on the integration branch and is not buildable in
the standalone Package D worktree. Actual E/gateway compilation and Linux
launcher tests remain remote integration checks.

See [implementation and integration limits](../../docs/architecture/native-linux-isolation.md).

## License

Apache-2.0, as declared by the workspace.
