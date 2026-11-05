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

This crate alone does not enable employee isolation. Package D's capability
implementation, daemon readiness, migration and runtime registration must all be
connected before protected execution is available. Cross-target checks do not
constitute Windows Job, Linux namespace, root-daemon or provider acceptance.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-execution
```

## License

Apache-2.0.
