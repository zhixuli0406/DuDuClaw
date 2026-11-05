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

Protected PTYs bind the requested initial size to the reservation with
`configure_pty` before StartRun. B2 sets and reads back that size before forking
the runtime; direct backend callers that omit configuration get 24×80. A real
master is allocated inside B2's per-run devpts.
Successful nonempty reads/writes notify B1 with the additive `PtyActivity` RPC on
the same authenticated run control connection. The root service supplies the
idle clock; polling, resizing, empty transfers and EOF do not renew it. The
protocol notification must be supported by both A and B1 in the same release.
Resize retries bounded asynchronous `NotReady` replies. B1 retains a master
only for resize/retirement; it never consumes bytes behind A's reader.
## Installed helper protocols

All four helpers require Linux, the exact root-published fixed arguments,
sealed bounded fd 4, a non-root matching uid/euid, NNP and fixed `/work`.
The gateway conditionally installs plans only from the composite hook's verified
generation. Missing root profiles, entitlements, workspace proofs or jobs refuse
execution. This does not turn composite readiness on.

| Helper | Root-published fixed arguments | Result |
| --- | --- | --- |
| `duduclaw-program-helper` | `--program-fd-v1 <python-sha256> <bash-sha256> <node-sha256>` | Strict `ProgramResult`; code ≤64 KiB, 1–300 s, combined output ≤128 KiB. |
| `duduclaw-office-helper` | `--office-fd-v1 <skill> <script> <script-sha256> <python-sha256>` | Strict `ProgramResult`; 180 s; exact bundled `create`/`extract` argument schemas. |
| `duduclaw-code-map-helper` | `--code-map-fd-v1` | Validated structured map, ranked files, counts and truncation flag. |
| `duduclaw-recording-helper` | `--browser-record-fd-v1 <node-sha256> <playwright-bundle-sha256> <chromium-sha256>` | Typed headless recording result and three bounded artifact digests, or explicit `not_recorded` for early stop. |

Program interpreters are fixed `/runtime/python3`, `/runtime/bash` and
`/runtime/node`; Office uses `/runtime/python3` and
`/runtime/office-<skill>-<script>.py`. Helper profiles pin SHA-256 digests of
these immutable root-owned files. There is no OS interpreter/PATH fallback,
package auto-install or caller-selected executable. Dependencies must ship with
the signed runtime generation; actual Python stdlib/Office dependency loading
still requires native packaging acceptance. `to_pdf` is refused until its
converter dependencies have a signed execution protocol.

Office copies at most 16 MiB from a regular single-link input opened beneath the
workspace with openat2 into private per-run temporary storage. The script gets
only snapshot/private output paths. Successful output is size/type checked,
fsynced and atomically published through the retained workspace directory fd
without replacing any existing file or link. Missing parent directories refuse.
Code maps index at most 5,000 entries, depth 32, 500 source files, 128 KiB per
file and 8 MiB total; the bounded lexical symbol index explicitly reports partial
results rather than claiming a complete parser index.

The gateway owns asynchronous job records and collection. Accepted work can
remain queued until its caller exits under D8. The same authenticated tool with
`action="status"` or `action="cancel"`, `job=<returned job>` and the authorized
workspace performs host control; no fake execution plan or synchronous child
wait is used. Cancellation acknowledgement is not retirement. B2 retains
namespace/cgroup/descendant teardown authority. Signing/publishing these helpers,
full gateway/CLI compilation and native Linux acceptance remain outstanding.
No shared isolation wire format or canonical signing bytes were changed.

Recording is explicitly headless (`headless=true`), 1–300 seconds (default 30).
The fixed helper pins `/runtime/node`, a self-contained `/runtime/playwright.cjs`
bundle and `/runtime/chromium`; the driver is embedded in the signed helper.
It requires a signed egress profile and uses only the root-derived loopback
proxy. Chromium's sandbox stays enabled. There is no desktop socket, ambient
Node/Playwright cache, dependency installation or `--no-sandbox` fallback.
A signed, compatible browser profile still needs native acceptance: factory
support does not prove Chromium can launch under the protected namespace policy.

The helper keeps trace, masked actions and origin/status-only network metadata
in private temporary storage, then publishes the complete directory under
`recordings/<returned id>` in authorized employee-work. Each of the three
artifacts is at most 16 MiB. Publication checks regular single-link files and
atomically renames through retained dirfds without replacement. Only bounded
metadata/digests enter the gateway. **Trace screenshots/snapshots may contain
secrets**; the trace is sensitive raw content, not a redacted HAR, and is never
automatically delivered or distilled into a skill.

`browser_record_stop {id}` persists `stop_requested` for the exact job owner;
the collector sends only `stop\n` on the helper's stdin. Acknowledgement is not
completion. Completed results can also be read using the start tool's job
status action. A live dashboard Admin can independently call
`isolation.recording_status {id}` or `isolation.recording_stop {id}` while D8 is
occupied; the gateway rereads the Admin role at use time and audits stop calls.
These RPCs have no new dashboard button. Interrupted jobs become uncertain on
restart and are not resumed. Early stop before launch yields `not_recorded`
without fabricated artifacts. Human demonstration, distillation compatibility,
signed publication and end-to-end recording acceptance remain outstanding.

**CONTRACT CHANGE (I1b round 5):** `SpawnPolicy.workspace_delivery` may retain
an `AuthorizedWorkspace::delivery()` directory proof. The additive optional
StartRun `workspace` selector carries only a fixed handle, validated relative cwd
and device/inode identity. Its final SCM_RIGHTS descriptor pins that authorized
directory throughout root's D8 queue. No host path or caller mount is accepted.
Root resolves `employee-work` or `fork-<32 lowercase hex>` (stored at
`.forks/<id>`) beneath the employee's attached H work filesystem, verifies the
same inode/device and employee UID, and mounts the selected directory/cwd at
`/work` using the existing root-owned detached-mount trampoline protocol. A
relative cwd therefore narrows the exposed run workspace to that directory.
Without delivery, only the original `employee-work` plus empty/`.` cwd is allowed.
Old omitted selectors keep their wire representation; old roots reject new
selectors rather than silently executing the wrong directory. There is one new
workspace-directory fd role, with 64 attachment and all byte limits unchanged
(maximum StartRun descriptors: 66). Cross-compilation is not root/native mount
acceptance. Runtime signing and full provider acceptance remain separate gates.

## License

Apache-2.0.
