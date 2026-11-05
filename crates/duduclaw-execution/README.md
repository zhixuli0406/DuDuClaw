# duduclaw-execution

Execution backends for gateway-owned process launches. `LocalBackend` preserves
ordinary native process execution; Linux `SpawndBackend` uses the root daemon's
bounded protocol and never falls back to a local process.

Call `reserve`, create the pending MCP capability in the gateway, then `spawn`
with the returned handle. The daemon derives its executable, arguments,
environment and directories from root policy. Internal `SpawnPolicy` values
are not serialized across the root boundary.

`command::spawn` and `spawn_pty` adapt legacy builders to the contract. Local
execution moves the original builder without recreating its argv, environment,
working directory, stdio or pre-exec hooks. Native strings remain native, including
non-UTF-8 values and values longer than the portable policy fields. The registered
builder is the LocalBackend command authority; the portable policy controls its
lifecycle. Windows pipe children are assigned to a kill-on-close Job before their
initial thread resumes. The async stream adapters preserve native I/O error kinds.

Protected callers supply an immutable `ProtectedLaunch` context. Its required
employee, caller employee and wait origin prevent a synchronous child launch from
waiting on admission already held by that employee. Such nested work is refused
before reservation; Package D must use its asynchronous delegation or existing-run
execution path. The context prepares policy/pending capability state, activates
the exact received MCP connection, and revokes on completion or cancellation.
Missing context, mismatched policy identity/runtime/class and widened requested
memory limits are refused. There is no local fallback.

`CapturedLaunch` carries runtime, execution class, optional protected context and
narrowing memory limit across task/thread boundaries, including explicit absence.
The child facade has streaming stdio, wait, cancellation and PTY resize; a cloned
`RunControl` supports cancellation from synchronous watchdogs without a current
Tokio runtime. Local PIDs are diagnostic only; protected runs expose no PID.
Cancelled writes with unacknowledged data must not be retried with different bytes;
the adapter reports an error rather than acknowledging the wrong buffer.

`SpawndBackend::with_workspace_bridge` is the explicit hookup for protected
`AuthorizedWorkspace` plans. The installed `RootWorkspaceBridge` must return a
root-attested receipt bound to the exact reservation, employee, registered runtime,
workspace handle and relative cwd, retaining the actual work directory and its
root mount generation. The backend verifies the boundary token against that held
directory after receipt/input waits and immediately before root IPC. It retains
the live authority, checks later I/O/status/waits, and cancels the run on revocation.
A cancel acknowledgement or closed connection is not a root cleanup receipt.

The default `SpawndBackend::connect` has no workspace bridge and refuses protected
workspace plans. This crate supplies the hookup and checks, not B/H's production
receipt producer: root IPC v1 still has no workspace selector or receipt RPC.
A gateway path lookup or capability workspace name cannot stand in for a root
attestation. No A execution producers are implemented by this hookup.

This crate alone does not enable employee isolation. Package D's capability
implementation, daemon readiness, migration and runtime registration must all be
connected before protected execution is available. Cross-target checks do not
constitute Windows Job, Linux namespace, root-daemon or provider acceptance.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-execution
```

## License

Apache-2.0.
