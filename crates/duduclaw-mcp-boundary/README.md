# duduclaw-mcp-boundary

Gateway-owned MCP connections for isolated Linux employees. The library signs
the shared contract's canonical capability records, binds each reservation to
one delivered socket, checks live authority on every request, and opens granted
workspace files without following links. `duduclaw-mcp-bridge` forwards stdio to
the inherited unnamed socket at fd 3; it has no secrets or socket pathname.

This is an integration component, not enabled isolation. Package A must provide
authenticated reservation/TakeIo readbacks and lifecycle events through the
`LiveAuthority` seam. The CLI adapter reuses the existing dispatcher. GatewayResourceResolver supplies
live record/account policy; GatewayResourceToolService serves bound native
connectors and own-wiki fd reads. The exact 259-tool map separates intentional
v1 restrictions from missing A/E factories. CSV/XLSX use the injected fd-only
FileParser worker adapter. Boot can require complete coverage through
IsolationDispatcher::require_complete_services; this is readiness, not live
provider acceptance. See the linked handoff for the deliberately reduced surface.

## Usage

Build on Linux with `cargo build -p duduclaw-mcp-boundary`. Run the bridge only
inside a spawnd-created run with the contract fd layout. Direct invocation
without the inherited socket fails. Non-Linux invocation fails closed.

`Boundary::issue` consumes trusted host policy, never JSON-RPC arguments.
`Boundary::activate` consumes the socket from a validated TakeIo response.
`Connection::context` rechecks live run, binding, policy and principal state.
Cancel/revoke before stopping a run; drop the connection on EOF. Expiry and
authority changes are rechecked during idle and active calls by the server.

The optional `binding-store` feature provides SQLite lifetime binding
constraints. Its database must live in the new gateway-owned data directory.
`BindingCheckedAuthority` composes these checks with A's live run/policy authority.

`config::prepare_runtime_bridge` requires an exact-version root registry receipt
for readonly config publication and real MCP-child fd inheritance. The supplied
fd helper and shell fixture do not constitute vendor CLI acceptance.

`factories::GatewayFactories::assemble` pairs the injected A `ExecutionBackend`
and `ExecutionPlanFactory`, with resource authorization around factory awaits.
Assembly also receives trusted `ResourceRoots`. After plan preparation, the
wrapper requires the selected workspace in the fresh grant set and mints an
`AuthorizedWorkspace` proof over an opened directory, bound to employee, caller
run and exact policy. This proof is not a mount descriptor handoff to spawnd;
root still resolves its own workspace and native acceptance remains necessary.
Supplying neither keeps execution unavailable; supplying only one is an error.
Worker routes use the separate `worker_execution::WorkerToolBridge`, never A's
employee execution path. Its injected `WorkerPlanFactory` must retain each
handler's policy checks, open authorized inputs by fd, and consume only validated
results. Missing factories remain absent from discovery and refused on calls.

On the integration branch, the gateway's `isolation-worker-integration` feature
builds the direct Package E adapter. Trusted boot calls
`isolation_execution::install_worker_services` with a `WorkerLauncher` once,
then uses its `.parser` for CSV/XLSX and `.tools(factory, resolver)` for worker
operations. E owns `WorkerSession`, admission, sealed snapshots and teardown;
the gateway does not enable E's `engine` feature. This feature needs the E
dependency already present on the integration branch and is not buildable in
the standalone Package D worktree. Actual E/gateway compilation and Linux
launcher tests remain remote integration checks.

`ExecutionToolBridge::with_jobs` installs a gateway-owned result/control store.
Factories opting into typed result collection require it for both list and call.
The tool immediately returns an accepted job; a separate host collector retries
queued stdio admission, drains bounded streams and observes the backend outcome.
Control requests require the current verified capability, live resource checks,
exact employee, principal generation, employee binding and original tool name.
Opaque job handles never authorize access by themselves. Cancel acknowledgement
remains `cancel_requested`; broken transport or restart remains `uncertain`.
`SqliteExecutionJobs` (binding-store feature) retains bounded metadata with
32 rows per employee and 4096 total; completed result retention can evict the
oldest own completion and completed/uncertain metadata expires after seven days.
Metadata expiry is not a statement that any process has retired.

Recording factories may opt into cooperative stop. `browser_record_stop` is
an exact alias to the same actor's `browser_record_start` job; it never reserves
a child or authorizes another tool's run. A durable `stop_requested` flag is
consumed by the host collector, which writes only `stop\n`, handles partial
writes and waits for bounded output plus terminal outcome before finishing.
Restart turns unfinished stop requests uncertain. The concrete SQLite store
also exposes a host-only recording status/stop method; gateway callers must
revalidate dashboard Admin authority before using it. The host path does not
fabricate a live employee capability or wait for that employee's D8 slot.

See [implementation and integration limits](../../docs/architecture/native-linux-isolation.md).

The `forks` module (`binding-store`) binds the exact actor/employee/principal
generation, granted branch workspace and owned child reservation. In v1.80.0,
protected TEAM explicitly defers `fork_run`, `merge_or_select`, and
`terminate_branch`. The protected dispatcher and host service refuse these
mutations even when a service is injected. The retained pointer-CAS/cancellation
implementation is not a published product path. Non-protected installs are unchanged.

Inspect and recorded-manifest diff remain authenticated host reads; no protected
handler opens the legacy fork database. `fork_cost` checks the same actor and
workspace grants, then reports `measurement_status=unavailable` and null spend:
the partial records have no measured usage. No zero-cost claim is made.
`fork_policy` centralizes the exact three-tool deferral and zh-TW/en/ja text.
A readiness checks every remaining helper and fork read service; absent signed
publications still refuse readiness, independently of the other composite gates.

`fork_queue` serializes bounded batches per employee without waiting for a child
inside the caller's tool response; root D8 remains authoritative across all run
classes. Durable queued/running records become uncertain on restart, without
replay or retirement claims. The collector bounds streams, observes terminal
outcome before snapshots, and cancels its own run on interruption. It is not yet
installed as the production `fork_run` service: fresh child grants, D MCP
activation and original account/budget/approval policy still need a concrete
producer. No arbitrary branch grants are synthesized.

`AuthorizedWorkspace::delivery()` now supplies a retained directory proof for the
additive root launch contract. ResourceCheckedPlans retains this proof in the
actual plan. Root still reopens only fixed employee-storage selectors and verifies
the identity; a gateway directory fd alone is not permission to mount a host path.

## License

Apache-2.0, as declared by the workspace.
