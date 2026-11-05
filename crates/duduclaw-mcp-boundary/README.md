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

See [implementation and integration limits](../../docs/architecture/native-linux-isolation.md).

## License

Apache-2.0, as declared by the workspace.
