# Native Linux isolation implementation

This note records implementation boundaries for native Linux employee isolation.
Source migration, compilation, protocol fixtures, native acceptance and deployment
are separate gates. Nothing in this note is production isolation acceptance.

## Package A

### Round 2 handoff (2026-11-05)

The employee spawn inventory is now routed through `ExecutionBackend`, with
explicit gateway exceptions for installation/account/model probes and interactive
login. The 91 outstanding entries from round 1 included command constructors and
11 E/G worker/import entry points, not 91 independent child processes. The source
inventory gate now checks backend calls as well as native constructors/terminal
APIs, including imported backend aliases. It is not a call-graph or type checker.

`duduclaw-core::execution_gate::{spawn,output,status,spawn_pty}` and
`process_tree::BackendChild` adapt existing asynchronous callers.
`execution_blocking` adapts synchronous discovery protocols without nesting a
Tokio runtime. It keeps a dedicated runtime alive for owned run/stream handles.
Its `output` helper is intentionally restricted to closed-stdin/captured-output
callers; commands with explicit stdio redirection use `spawn`/`status` instead.

Local execution moves the original native Command/PTY builder into LocalBackend.
It does not reconstruct argv/env/cwd from UTF-8 policy fields, so native OS strings,
explicit environment removal, env_clear, cwd, platform quoting and pre-exec hooks
survive. The portable SpawnPolicy supplies typed class/runtime and lifecycle;
the registered builder is authoritative for local argv/env/cwd. Native spawn and
I/O error kinds (including NotFound fallback and BrokenPipe) are retained.
Windows pipe children use a suspended launch and kill-on-close Job. Windows PTY
still uses portable-pty termination: descendant-tree cleanup is not accepted.

Migrated paths:

| Paths | Execution routing |
| --- | --- |
| Claude runner and channel reply | BackendChild; exact native builder, streaming output, terminal isolation error |
| Codex, Grok and generic CLI | Typed runtime scope, backend output/streaming; Grok PTY uses backend PTY and real resize |
| cron, dispatcher, heartbeat, mail, reminder | Shared host-derived class scope, retained across the runtime boundary |
| judge, memory summary, fork, discovery | Backend process ownership; synchronous watchdogs cancel RunControl, not PIDs |
| Office, execute_program, third-party MCP, Codrive | Typed backend handoff; protected PTC does not enter the local Docker fallback |
| duduclaw-llm stdio MCP | BackendChild and owned async streams, existing handshake/lifecycle retained |
| computer use, container cleanup, device/helper commands | Backend handoff; no direct protected gateway Docker fallback |
| E/G document/media/model/import callers | Typed ParseWorker/ImportWorker handoff; E/G still owns bounded input/worker policy integration |

`CapturedLaunch` preserves runtime, optional class, protected context and narrowing
memory ceiling across task/thread boundaries. Discovery policy planning/solving,
detection and delayed cleanup retain it; task sandbox and computer-use cleanup
store it when their lifecycle object is created. ParseWorker RSS cancellation no
longer signals a cached PID after the backend may have reaped it. Local PID reads
are diagnostic only. Explicit media/extract memory ceilings constrain protected
policies as well as the legacy local guards.

### Protected backend and Package D seam

Protected native Linux uses SpawndBackend only. Missing context/services, policy
binding mismatches or capability setup failures refuse execution; they never
choose LocalBackend. Gateway runtime wrappers propagate the terminal spawn-gate
error through account/model/runtime and direct-API fallback decisions.

`ProtectedLaunch` is an in-process integration seam, not an implementation of D.
D must supply immutable employee/task identity, caller employee and wait origin,
construct bounded prompt/attachment inputs from the native invocation, prepare
pending capabilities before StartRun, activate only the exact live MCP connection,
and revoke on every completion/cancellation. No identity is inferred from child
environment variables. Same-employee nested synchronous launches are refused
before reservation rather than waiting for admission held by their caller; D must
route that work through its asynchronous delegation/existing-run path.

A cancellation guard covers preparation, bind, spawn, queued TakeIo and activation.
Queued I/O is retried only for Unavailable within the queue/startup deadline.
The exact runtime/class/employee/run/PTY binding and any requested memory ceiling
are checked before StartRun. Spawnd authenticates the root-owned seqpacket endpoint,
validates reply bindings/descriptors and carries sealed inputs; it does not send
arbitrary native argv/env/cwd to root. Registered helper runtime IDs and payload
mapping must be provisioned with B/D/E/G before those paths can execute protected.
An employee host temporary filename is not a valid remote input by itself.

### Inventory and remaining acceptance gates

```sh
python3 -m unittest discover -s scripts/ci -p 'test_spawn_inventory.py' -v
python3 scripts/ci/check_spawn_inventory.py --require-migrated
```

Each registry entry has a symbol, exact API counts, effect class, identity and
reason. Pure platform builders never start a process. Retained native-handle core
compatibility primitives still refuse employee classes in protected mode; actual
employee callers use the async backend adapters. Login removes the eight declared
desktop/SSH/GPG/session variables after explicit environment injection.

The inventory gate covers process effects. The design's full file/network/parser/
connector effect audit, root daemon readiness, capability implementation, E/G
DynamicUser worker integration, root runtime catalog, migration and real-provider
acceptance remain separate work. Passing this gate must not be presented as those
being complete. In particular:

- Gateway/CLI and all caller-crate compilation/tests are pending on the remote
  builder. Local syntax parsing is not type checking.
- Linux privileged namespace/cgroup/fd/admission tests and live vendor tests were
  not run. No new iso_root_ acceptance fixtures are claimed.
- Windows pipe Job code was cross-checked only; native cancellation acceptance is
  pending. Windows PTY descendant cleanup remains a known limitation.
- Codrive builder equality does not establish GUI lifetime equivalence. The local
  backend now cleans descendants in its process group when the leader exits,
  unlike the previous unguarded detached native child. A launcher that forks a
  same-group GUI and exits would lose that child. The current Chromium path was
  not observed doing that; native new-profile launch, existing-instance handoff
  and gateway shutdown behavior still require acceptance before release.
- LocalBackend does not provide memory/CPU/disk/network isolation. Its byte-stream
  queues are bounded, but output_buffer_bytes is not a general local kernel limit.
- Attachments exceeding the existing 16 MiB memfd threshold still require the
  read-only snapshot provider. Until it is supplied, they are refused.
- Daybreak security review has not been performed. Ordinary independent code
  review is not that signoff.

### Validation record

Round 1: execution 18 passed; core gate 2 passed; contract 29 passed; Linux and
Windows execution test code cross-checked; Python scanner 14 passed; 402 symbols
classified with 91 pending. This round supersedes that source inventory status.

Round 2 final results are recorded below. All Cargo commands use
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0` and `--offline`. No gateway/CLI/workspace
build/test, cargo fmt, commit, merge or deployment was run. No isolation contract
source, wire format, canonical bytes or hard limit changed. The execution Windows
Job adapter uses the already-locked windows-sys version; no new external library
version was introduced.

| Command | Observed result |
| --- | --- |
| `cargo test -p duduclaw-execution --offline` | 32 passed, 0 failed |
| `cargo test -p duduclaw-core execution_ --offline` | 4 passed, 0 failed |
| `cargo test -p duduclaw-core backend_child_ --offline` | 2 passed, 0 failed |
| `cargo check -p duduclaw-core --tests --offline` | Passed |
| `cargo check --target x86_64-unknown-linux-gnu -p duduclaw-execution --tests --offline` | Passed; type check only |
| `cargo check --target x86_64-pc-windows-gnu -p duduclaw-execution --tests --offline` | Passed; type check only |
| `python3 -m unittest discover -s scripts/ci -p 'test_spawn_inventory.py' -v` | 16 passed, 0 failed |
| `python3 scripts/ci/check_spawn_inventory.py --require-migrated` | Passed: 455 symbols, 0 pending |
| Temporary `/private/tmp/iso-a-syntax/target/debug/iso-a-syntax` over changed Rust paths | 72 files parsed, 0 syntax errors; not compilation |
| `git diff --check` | Passed |

The 455 classifications are 113 backend, 66 gateway exceptions, 45 non-process,
203 test-only and 28 other-package OS/desktop effects outside this native-Linux
employee migration. Their written reasons remain in the registry. CI invokes the
readiness form (`--require-migrated`), not coverage-only checking.

An intermediate supervisor shutdown-guard change produced 20 passing and 12
failing execution tests because Rust captured individual guard fields. Capturing
the complete guard through its method fixed the cleanup ownership; the final
32-test run and both target checks passed. These are the final-version results,
not a claim that every intermediate run passed.

The temporary syn parser reads touched Rust files only; it does not compile
caller crates. Ordinary independent review found and fixed PID ownership,
PTY class propagation, synchronous context propagation, same-employee deadlock,
local BrokenPipe preservation and write-cancellation acknowledgement issues.
A cancelled write with unacknowledged bytes rejects a different retry buffer.

### Remote builder commands

Prefix Cargo commands with `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0` as appropriate.
These tests are written but caller-crate results have not been observed locally.

```sh
cargo check -p duduclaw-gateway -p duduclaw-cli -p duduclaw-agent -p duduclaw-cli-runtime -p duduclaw-fork -p duduclaw-llm -p duduclaw-memory -p duduclaw-inference -p duduclaw-container --tests
cargo test -p duduclaw-execution
# Linux-only fixture (also included in the full execution suite):
cargo test -p duduclaw-execution protected_same_employee_is_refused_before_connecting_or_waiting_for_io
cargo test -p duduclaw-isolation
cargo test -p duduclaw-core execution_
cargo test -p duduclaw-core backend_child_
cargo test -p duduclaw-core process_tree
cargo test -p duduclaw-agent execution_backend_equivalence_tests
cargo test -p duduclaw-cli-runtime execution_backend_equivalence_tests
cargo test -p duduclaw-cli-runtime pty::tests
cargo test -p duduclaw-cli-runtime oneshot
cargo test -p duduclaw-cli-runtime scheduled_grok_pty_keeps_typed_policy_across_task_boundary
cargo test -p duduclaw-cli execution_backend
cargo test -p duduclaw-fork execution_backend_equivalence_tests
cargo test -p duduclaw-fork test_runner
cargo test -p duduclaw-fork overlay
cargo test -p duduclaw-llm execution_backend_equivalence_tests
cargo test -p duduclaw-llm mcp_client
cargo test -p duduclaw-memory execution_backend_equivalence_tests
cargo test -p duduclaw-gateway local_backend_equivalence_
cargo test -p duduclaw-gateway l1_stub_claude_thinking_flows_by_policy
cargo test -p duduclaw-gateway execution_classes_follow_host_paths_and_do_not_cross_tasks
cargo test -p duduclaw-gateway isolation_backend_error_is_terminal_across_runtime_fallbacks
cargo test -p duduclaw-gateway process_group::
cargo test -p duduclaw-gateway discovery::
cargo test -p duduclaw-gateway task_sandbox::
cargo test -p duduclaw-gateway condition_eval::
cargo test -p duduclaw-gateway knowledge::extract_spawn::
cargo test -p duduclaw-gateway tts::
cargo test -p duduclaw-inference transcription::pipeline::
python3 -m unittest discover -s scripts/ci -p 'test_spawn_inventory.py' -v
python3 scripts/ci/check_spawn_inventory.py --require-migrated
```

LocalBackend equivalence fixtures exercise production builders and registration,
not live provider execution. Claude, Codex utility/conversation, Grok, all five generic CLI catalog variants,
judge and Codrive compare independent legacy argv/env/cwd expectations with the
production builder before LocalBackend registration. Claude includes a generated
system-prompt file; Codex conversation independently derives dynamic permission/MCP
paths using their existing helpers. Heartbeat's fixture covers the base command
before later MCP/env additions.
The channel fixture exercises the complete captured channel command. Existing
runtime, cancellation, MCP handshake and discovery tests must also run remotely.

### File-by-file handoff

All paths are relative to the repository root; all changes remain uncommitted.
The list includes the inherited round-1 implementation and round-2 migrations.

| File | Change |
| --- | --- |
| `.github/workflows/ci.yml` | Run scanner regression tests and require the migrated inventory in CI. |
| `CHANGELOG.md` | Describe source migration and outstanding integration/acceptance in zh-TW. |
| `Cargo.lock` | Record the internal execution crate and already-locked dependencies. |
| `Cargo.toml` | Register the execution workspace member and shared dependency. |
| `crates/duduclaw-agent/src/heartbeat.rs` | Route heartbeat CLI through typed backend execution and pin its base builder. |
| `crates/duduclaw-agent/src/runner.rs` | Route employee CLI runs through backend execution with legacy builder fixture. |
| `crates/duduclaw-cli-runtime/src/oneshot.rs` | Await asynchronous backend PTY creation. |
| `crates/duduclaw-cli-runtime/src/pty.rs` | Replace native PTY ownership with backend handles, resize, class propagation and fixtures. |
| `crates/duduclaw-cli/src/eval/runner.rs` | Launch evaluation CLI with Judge policy. |
| `crates/duduclaw-cli/src/lib.rs` | Route namespace helper execution through the backend instead of raw exec. |
| `crates/duduclaw-cli/src/mcp/exec.rs` | Migrate Office Python/uv execution while retaining NotFound fallback and builder fixture. |
| `crates/duduclaw-cli/src/mcp_fork_exec.rs` | Migrate fork CLI spawning and retain exact branch command construction. |
| `crates/duduclaw-cli/src/mcp_proxy.rs` | Use backend-owned third-party MCP stdio processes with builder fixture. |
| `crates/duduclaw-cli/src/mcp_recording.rs` | Own recording/helper processes through backend controls; persisted PIDs are diagnostic only. |
| `crates/duduclaw-cli/src/ptc/sandbox.rs` | Route execute_program through backend and refuse protected local-container fallback. |
| `crates/duduclaw-container/src/apple.rs` | Route task container commands through backend; retain installation version probe. |
| `crates/duduclaw-container/src/lib.rs` | Use backend status for detached container cleanup with existing null redirection. |
| `crates/duduclaw-container/src/wsl2.rs` | Route WSL task execution/cleanup through backend; retain distro installation probe. |
| `crates/duduclaw-core/Cargo.toml` | Wire execution/isolation contracts and existing PTY dependency into core. |
| `crates/duduclaw-core/src/execution_blocking.rs` | Bridge synchronous child/stream protocols to a retained backend runtime. |
| `crates/duduclaw-core/src/execution_gate.rs` | Select the fail-closed backend and expose typed scopes, lifecycle seam and command adapters. |
| `crates/duduclaw-core/src/lib.rs` | Export shared execution adapters. |
| `crates/duduclaw-core/src/process_tree.rs` | Add BackendChild with backend cancellation and conservative cleanup observation. |
| `crates/duduclaw-execution/Cargo.toml` | Define the new backend crate using existing dependency versions. |
| `crates/duduclaw-execution/README.md` | Document adapters, platform limitations and the D capability seam. |
| `crates/duduclaw-execution/src/command.rs` | Preserve native builders; own child/PTY I/O, context, policy binding and capability lifecycle. |
| `crates/duduclaw-execution/src/io.rs` | Provide async stream adapters with error and write-cancellation fidelity. |
| `crates/duduclaw-execution/src/lib.rs` | Expose backend implementations, selection and native-command adapters. |
| `crates/duduclaw-execution/src/local.rs` | Implement native stdio/PTY runs, supervision, cancellation and builder inspection. |
| `crates/duduclaw-execution/src/selection.rs` | Select local versus protected backend with explicit gateway exception classes. |
| `crates/duduclaw-execution/src/spawnd.rs` | Implement daemon reserve/start/TakeIo, wait, kill, PTY and reply validation. |
| `crates/duduclaw-execution/src/spawnd/fd_io.rs` | Adapt received daemon I/O descriptors into owned streams. |
| `crates/duduclaw-execution/src/spawnd/transport.rs` | Authenticate the seqpacket endpoint and receive bounded SCM_RIGHTS replies. |
| `crates/duduclaw-execution/src/windows_job.rs` | Attach suspended Windows children to kill-on-close Jobs before resuming. |
| `crates/duduclaw-fork/src/overlay.rs` | Route overlay copy subprocess through the synchronous backend adapter. |
| `crates/duduclaw-fork/src/test_runner.rs` | Replace PID cancellation with backend controls and pin test-command construction. |
| `crates/duduclaw-gateway/src/audio_bridge.rs` | Route media helper output through EmployeeTool execution. |
| `crates/duduclaw-gateway/src/backup_schedule.rs` | Route archive creation through ParseWorker execution. |
| `crates/duduclaw-gateway/src/channel_reply/cli_env.rs` | Retain the full channel command and spawn it through typed Claude BackendChild. |
| `crates/duduclaw-gateway/src/channel_reply/cli_env_thinking_tests.rs` | Check complete channel command registration against its captured builder. |
| `crates/duduclaw-gateway/src/channel_reply/cli_spawn.rs` | Use backend child/stream types for channel reply lifecycle. |
| `crates/duduclaw-gateway/src/claude_runner.rs` | Route Claude calls through typed backend scope and add command equivalence fixture. |
| `crates/duduclaw-gateway/src/cli_auth.rs` | Scrub desktop/SSH/GPG/session variables from the explicit gateway login exception. |
| `crates/duduclaw-gateway/src/codrive/registry.rs` | Spawn Codrive via backend with exact command/environment fixture. |
| `crates/duduclaw-gateway/src/computer_use.rs` | Route sensitive-region helper through EmployeeTool execution. |
| `crates/duduclaw-gateway/src/computer_use_orchestrator.rs` | Route container/profile commands and retain launch context for delayed cleanup. |
| `crates/duduclaw-gateway/src/computer_use_sessions/live_view.rs` | Use backend-owned live-view process and stream types. |
| `crates/duduclaw-gateway/src/condition_eval.rs` | Route command conditions through Scheduled execution. |
| `crates/duduclaw-gateway/src/device_ops.rs` | Route device helper subprocesses through the typed backend. |
| `crates/duduclaw-gateway/src/discovery/attempt_container.rs` | Use backend control processes and retain launch context in cleanup guards. |
| `crates/duduclaw-gateway/src/discovery/evaluator.rs` | Use backend child ownership and captured context for evaluator cleanup. |
| `crates/duduclaw-gateway/src/discovery/maintenance.rs` | Route bounded maintenance commands through the blocking backend. |
| `crates/duduclaw-gateway/src/discovery/online.rs` | Carry immutable launch context across discovery blocking-worker boundaries. |
| `crates/duduclaw-gateway/src/discovery/policy_runner.rs` | Adapt synchronous policy I/O, watchdog cancellation and scoped container cleanup. |
| `crates/duduclaw-gateway/src/discovery/process.rs` | Route discovery child execution and compare local builder/output to the legacy command. |
| `crates/duduclaw-gateway/src/discovery/service_lifecycle.rs` | Capture launch context before the discovery blocking task is created. |
| `crates/duduclaw-gateway/src/dispatcher.rs` | Set Dispatch execution class at the host dispatch boundary. |
| `crates/duduclaw-gateway/src/eval_runner.rs` | Route evaluation scripts through Judge execution. |
| `crates/duduclaw-gateway/src/finetune/jobs.rs` | Route training helper subprocesses through EmployeeTool execution. |
| `crates/duduclaw-gateway/src/handlers/experts_import_rpc.rs` | Use ImportWorker backend child/control types for importer lifecycle. |
| `crates/duduclaw-gateway/src/handlers/fork_migrate_rpc.rs` | Route migration subprocess through ImportWorker execution. |
| `crates/duduclaw-gateway/src/judge_mode.rs` | Use Judge backend execution and preserve external judge command construction. |
| `crates/duduclaw-gateway/src/knowledge/extract_spawn.rs` | Use ParseWorker with narrowing memory ceiling and backend cancellation. |
| `crates/duduclaw-gateway/src/mail_worker.rs` | Set Mail execution class around the runtime call. |
| `crates/duduclaw-gateway/src/process_group.rs` | Collect backend output, preserve host classes and propagate terminal isolation failures. |
| `crates/duduclaw-gateway/src/reminder_scheduler.rs` | Set Reminder execution class around the runtime call. |
| `crates/duduclaw-gateway/src/runtime/codex.rs` | Migrate Codex utility/conversation execution and add legacy command fixtures. |
| `crates/duduclaw-gateway/src/runtime/generic_cli.rs` | Migrate catalog-driven CLI execution with fixtures for every built-in catalog variant. |
| `crates/duduclaw-gateway/src/runtime/grok.rs` | Migrate Grok pipe/PTY execution and preserve terminal backend refusal semantics. |
| `crates/duduclaw-gateway/src/runtime_dispatch.rs` | Carry the initiating request class through runtime dispatch. |
| `crates/duduclaw-gateway/src/server.rs` | Route preview extraction subprocesses through ParseWorker execution. |
| `crates/duduclaw-gateway/src/stt.rs` | Route transcription command execution through ParseWorker. |
| `crates/duduclaw-gateway/src/task_sandbox/container.rs` | Use backend container commands and retain launch context across cleanup threads. |
| `crates/duduclaw-gateway/src/tick_source_poll.rs` | Route command tick polling through Scheduled execution. |
| `crates/duduclaw-gateway/src/tts.rs` | Route synthesis processes through ParseWorker and backend stream ownership. |
| `crates/duduclaw-inference/src/llamafile.rs` | Use a backend-owned ParseWorker model subprocess. |
| `crates/duduclaw-inference/src/transcription/pipeline.rs` | Use RunControl for media cancellation and constrain protected memory policy. |
| `crates/duduclaw-llm/src/mcp_client.rs` | Migrate stdio MCP processes/streams to BackendChild with legacy builder fixture. |
| `crates/duduclaw-memory/src/engine.rs` | Route CLI summarization through Judge execution and pin command construction. |
| `docs/README.md` | Index the native Linux isolation implementation note. |
| `docs/architecture/native-linux-isolation.md` | Record Package A migration, evidence, seams, limitations and remote test commands. |
| `docs/features/05-security-defense.md` | Describe enabled backend routing and pending protected integration in English. |
| `docs/features/ja-JP/05-security-defense.md` | Keep Japanese security behavior and acceptance boundaries aligned. |
| `docs/features/zh-TW/05-security-defense.md` | Keep Traditional Chinese security behavior and acceptance boundaries aligned. |
| `scripts/ci/check_spawn_inventory.py` | Scan native/backend process APIs and reject drift or pending migrations. |
| `scripts/ci/privileged_effects.json` | Classify all observed process symbols with identity and reviewed scope rationale. |
| `scripts/ci/test_spawn_inventory.py` | Cover scanner aliases, literals, new calls, API drift and classification failures. |
# Native Linux isolation implementation notes

This is implementation status, not a claim of enabled isolation or acceptance.
The shared v1 contract is in `crates/duduclaw-isolation`.

## Package B1

Status: **Linux host wiring implemented; default composition remains fail closed.**
`main` now selects `host::LinuxHost`, not the unavailable test host. Its installed
socket, account registry, recovery, systemd operations, relays, cancellation and
readiness publication are real Linux code. The default `Components` intentionally
supplies missing B2/H/F/G evidence adapters and missing C DNS evidence, so it cannot
admit employee runs. No gateway execution path has been changed by B1. Compilation
is not native Linux acceptance or independent security approval.

### This round: disposition of the previous seven items

| Previous item | B1 implementation now | Integration / acceptance still required |
| --- | --- | --- |
| Production Host | `LinuxHost` composes registry/accounts, systemd, journal, scanner, output relays, endpoint revokers, B2/C/H interfaces and root publication. Slow lifecycle work uses a bounded mailbox; control requests do not wait for it. | F/G must construct `Publication` and installation evidence; B2/H must implement their adapters. |
| Bootstrap and mount delivery | Fixed `/usr/lib/duduclaw/duduclaw-run`, argv containing only that path, fd 0 root seqpacket and fd 1/2 bounded pipes. B1 builds/seals the plan and prepares detached private mounts, minimal `/etc`, root projections and digest-checked executable/script fds. | B2 must explicitly accept this internal bootstrap version and implement nonce exchange, exact rights delivery, cancellation barrier and all isolation/PTY readbacks. |
| Restart reconciliation | Permanent allocation tombstones; orphan unit inventory/stop/reset-failed; no-UID scan; C unregister; H persistent attachment inventory/detach; projection cleanup; durable run-journal removal only after proof. Old in-memory gateway reservations cannot survive a new daemon. | C/H inventories and acknowledgements must be available; unprovable cleanup leaves readiness false. |
| Publication and provisioning | Typed root-only `Publication`; `RegistryStore::allocate/reconcile`; account creation/quarantine/readback; H provision/destroy request binds operation id and full proposal digest. | Signature verification/publication belongs to F; approved proposal authority belongs to G. Normal provisioning runs before activation or while the registry owner is stopped, not through gateway StartRun. |
| Readiness evidence | Atomic root-owned, gateway-readable evidence; boot/time expiry; registry/account/SSH/IAM, B2, C/DNS, H, resource and scanner checks. `--check` returns the same evidence plus failure status. | Actual SSH login/forwarding negatives, IAM reservation/owner scan, B2 self-tests and C DNS route proof cannot be replaced with booleans in config. |
| Live I/O and lifecycle | Revocable MCP/egress pairs; acknowledged C registration; stdin/output pipes; bounded relay pumping and truncation audit; PTY ownership retained before terminal cleanup; cancelled preparation fenced before B2 release. | B2 supplies actual validated per-run PTY and status. D must not activate a revoked terminal-run MCP endpoint. A must retry bounded `NotReady` TakeIo/Resize responses. |
| Installer and acceptance | Installer stages root registry/log/projection directories, fixed binaries/units and locked supervisor; service permits account quarantine; ignored root tests added. | F/G publish the signed compatible generation and activate it; A/F/C join the TCB slice; three-distro root execution and Daybreak review remain required. |

### Authority, ownership and fail-closed lifecycle

The shared gateway IPC v1, canonical signing bytes, UID rules and hard limits were
not changed. Runtime/profile/argv/environment/limits remain root-derived; the
caller cannot provide a path, UID, unit property, mount source or policy override.
`RootPolicy` rejects unknown or ambiguous profiles. A missing component never
selects a gateway-UID fallback.

The listener is `/run/duduclaw-spawnd/control.sock`, root-owned `0660` with the
installed gateway group, authenticated by exact peer UID/GID. Activation validates
the actual descriptor, bound path and listening state. Standalone mode creates
and verifies the fixed root parent. Under the registry singleton lock, it only
removes an existing trusted socket after `ECONNREFUSED` and inode/device readback;
a live socket, wrong owner/mode or unknown error is refused.

`RegistryStore` holds the root allocation flock for its lifetime. State lives in
`/var/lib/duduclaw-spawnd` (`0700`); registry and active-run journal are root-only
`0600`, temp-write + file fsync + rename + directory fsync. Failed durability
poisons the writer instead of allowing a later successful write to hide the
failure. Employee UID mappings are permanent and never recycled. Registry
initialization rejects writable ancestors, symlinks, wrong ownership and hardlinks.

An interrupted `Reserved` allocation is first durably tombstoned. Recovery checks
exact NSS/local/userdb identities before quarantining a group-only or half-created
account. It locks/expires the password/account, installs nologin and `/nonexistent`,
removes supplementary groups, disables linger and user managers, fixes effective
cron/at ACLs, reloads SSH, and reads the results back. Unknown aliases or jobs are
not silently accepted. User creation disables subordinate-ID allocation and
lastlog initialization. External IAM range reservation, complete existing-owner
scan and live SSH public-key/forwarding negatives remain explicit evidence hooks;
`MissingInstallationEvidence` always refuses them. `userdbctl` supports bounded
JSON arrays or object streams and rejects unknown record shapes.

The service now allows `/etc` and the existing linger directory to be written for
root-only crash quarantine; normal provisioning remains the installer/G operation.
This is an explicit change from the first-round read-only service policy. The
service retains the root capabilities required by its account tools and mount
preparation. No gateway RPC for arbitrary account mutation was added. Installation
must provide explicit login.defs UID/GID range entries; missing compiled-default
range evidence remains a refusal, not a guessed distro default.

`runs.json` is written before H attachment, capability delivery or unit creation.
A lost StartTransientUnit or H attach reply is recovered by inventory, not treated
as no effect. Cleanup closes B2 originals, recovers/detaches every matching H
attachment, removes its fixed projection, then repeats unit/cgroup/UID checks.
It keeps the journal and D8 lease on any uncertainty. Ended output endpoints remain
until one-shot TakeIo or retention expiry; capability endpoints are already revoked.

The monitor runs separately from lifecycle/account/D-Bus work. It rechecks the
current admission generation after scanning `/proc`, sets a shared cancellation
fence on intrusion/failure/deadline, revokes sockets, and signals employee processes
through pidfds with UID/start-time/cgroup revalidation. Unit stops use an independent
bounded emergency queue. No D-Bus call holds the monitor map lock. Readiness rejects
stale/error scan results, and account failures do not turn an unreadable scan into
an empty result. This is a fail-closed deadline mechanism, not a measured two-second
latency guarantee under kernel stalls or very large process tables.

The lifecycle mailbox holds at most 128 commands/results, with bounded cached
slots and finite operation deadlines. Worker timeout does not make an OS operation
transactional: cancellation fences prevent a delayed preparation from releasing
B2, and journal/inventory recovery preserves partial effects. Relay errors poison
readiness and cancel the affected run. SIGTERM/SIGINT cancel and tick cleanup for
up to ten seconds. Transient units bind to `duduclaw-spawnd.service`, so PID 1 stops
them if the daemon exits or is killed; crash residue still requires reconciliation.

### systemd and B2 launch contract

`Systemd::start` calls D-Bus `StartTransientUnit`, with `AddRef` in the creation
transaction, fixed trampoline ExecStart, real Unix fd variants and all root-selected
hardening/resource properties. It verifies configured values and actual cgroup
memory/swap/PID/CPU/IO ceilings. Cleanup handles failed, inactive, missing and
half-started units without interpreting an unknown state as proof. Unit references
are released only after cleanup. `resources_ready` checks TCB/parent memory
reservations, aggregate employee ceilings, trusted service slice placement and
host PTY capacity (`max - reserve`, with 10% headroom).

B1's internal `BootstrapPlan` version 1 is an **integration proposal for B2**, not a
change to shared gateway protocol v1 and not a claim of compatibility with the
parallel B2 implementation. It binds domain/version, nonce, run/employee, static
employee and supervisor identities, runtime version/profile, fixed argv and fresh
environment, quotas and an exact ordered fd manifest. Plan bytes use a bounded
memfd with WRITE/GROW/SHRINK/SEAL seals. The dedicated bootstrap transport must
support up to 80 rights; it must not reuse the gateway transport's 65-right limit.

The manifest contains the sealed plan; ELF/interpreter and optional script;
home/work/minimal-etc/runtime/employee-projection detached mounts; prompt; optional
stdin; ordered attachments; optional MCP and egress. Runtime destinations remain
stdin 0, MCP 3, prompt 4 and attachments 5 onward; executable/script descriptors
are handled by B2's high-fd algorithm. Runtime source count and NOFILE requirements
remain those of the approved design. No secrets or employee payload enter argv,
environment, audit text or journal records.

`bootstrap_mounts::prepare` clones H home/work mounts before changing attributes,
sets recursive private propagation, and checks restrictive flags/mount identities.
Work is conservatively noexec. It creates the five fixed minimal-etc files under
a root-only anchor and fsyncs publication. Employee projections come from the
fixed EmployeeId hex child of a root-published projection anchor, with recursive
RO/NOSUID/NODEV/NOEXEC/NOSYMFOLLOW. Runtime executable/script openat2 is relative to
the detached runtime mount, forbids links/traversal/cross-mount resolution, checks
root ownership/ELF/statx identity and SHA-256, and rechecks after hashing. Hashing
has a 512 MiB per-file cap and a shared 15-second loop deadline; kernel-stalled I/O
is fenced externally, not forcibly undone by a thread timeout.

`Trampoline::prepare` receives B1's already sealed plan and owned prepared fds;
it must keep the release barrier closed. `bind_unit` receives the actual systemd
receipt and shared cancellation flag. It must check that flag immediately before
release. `poll` may report Running only after complete namespace, capability,
UID/GID, seccomp, Landlock and fd readbacks. `take_pty` transfers the validated
per-run devpts master once; B1 retains it through short-command completion.
`close` is idempotent even after a partially failed prepare and must drop originals.
The shipped `UnavailableTrampoline` refuses every operation.

### H and publication seams

`storage::Storage` is the B1-facing adapter for `duduclaw-isolation-storage`:
`provision`, `attach`, `detach`, `destroy`, `usage`, `readiness`, `attachments`.
No image path, raw image fd or loop device goes to B2. Attachment receipts bind
employee/run and H's durable generation token. Usage must prove hard byte/inode
limits, backing-device graph and host reserve. Attach failures can have effects;
H's persistent inventory must expose them. Detach must be generation-safe and
idempotent. Provision/destroy bind G's operation ID and full approved digest;
neither destroys nor recycles a UID mapping. No H implementation was duplicated.

`Publication` supplies F/G's verified root policy, gateway snapshot mount authority,
supervisor identity and root runtime/projection anchors. It is not deserializable
from StartRun. `InstallationEvidence` supplies actual IAM/owner-scan/SSH results.
C uses the authenticated fixed control endpoint, correlated Register/Unregister
acknowledgements and a separate DNS/secondary-layer evidence adapter. Missing
adapters and unknown evidence reject readiness. C's installed socket ownership and
peer credentials still need compatibility acceptance with its parallel package.

### Gateway readiness evidence

A reads `/run/duduclaw-spawnd/readiness.json` or invokes `duduclaw-spawnd --check`.
The file is atomic root:gateway-group `0640`; readers validate root ancestry,
regular type, no links, exact ownership/mode, bounded JSON, schema version, boot
ID and monotonic boottime expiry. The check command works as the gateway account,
prints the structured report, and exits nonzero when unavailable/stale.

The report has no secrets, command lines, host process IDs or free-form errors.
It contains the primary `Readiness` booleans, secondary BPF status, observed host
PTY budget and timestamps. Evidence is dated at probe **start**, with a maximum
one-second report lifetime; scanner evidence must be less than one second old at
publication. Expired/missing evidence denies. Slow probes can therefore keep a
large installation unavailable; no performance acceptance has been claimed.
Production ReserveRun/policy selection uses the same verified in-process view,
not an extra per-request slow-probe RPC. Start performs fresh host/UID/storage
checks before effects. A must not equate service active or a file's existence with
readiness, nor treat a failed check as permission to use the legacy executor.

### Validation and remote handoff

This round ran only the small contract/spawnd crates in this worktree. No commit,
worktree creation/removal, gateway/CLI/workspace build, cargo fmt, deployment,
real account mutation or remote root test was performed. This session did not
create this worktree; it remains intact with the requested uncommitted handoff.

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation -p duduclaw-spawnd --offline` | 29 contract + 45 spawnd tests passed on macOS; 0 failed. Includes five mailbox regression tests. |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --tests --target x86_64-unknown-linux-gnu --offline` | Passed; Linux-only/root test bodies compiled, not executed. |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --tests --target x86_64-pc-windows-gnu --offline` | Passed; Windows test bodies compiled, not executed. |
| `git diff --check` | Passed for tracked changes; new Rust/text files also checked for trailing whitespace. |

The first check found an invalid workspace-inherited sha2 dependency; the existing
locked `sha2 0.10.9` is now a direct Linux dependency. Linux compilation then found
a duplicate Clone derive and one ambiguous closure Result type; both were fixed.
Unsafe-block lint notices and the pre-existing fetch_update deprecation remain.
Independent code review found and drove fixes for cgroup membership formatting,
partial-start cleanup, H lost receipts, scan generations/cancellation, evidence
freshness, durability poisoning, short-lived PTY retention and shutdown draining.
This is **not** the required `gpt-daybreak-blue-latest` security review; that review
and live acceptance remain outstanding.

On each disposable Ubuntu 22.04/systemd 249, Debian 12/systemd 252 and Ubuntu
24.04/systemd 255 VM, run as root with the compatible package fixtures installed.
The account test changes login ACLs and uses fixture UID 99997; do not run these
on a production host or concurrently with a live spawnd. Resource-readback tests
need the complete installed TCB units; missing components should fail that test.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation -p duduclaw-spawnd --offline
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1
cargo test -p duduclaw-cli spawnd_install_tests
```

Individual root filters (crate `duduclaw-spawnd`):

- `iso_root_fixed_socket_bind`
- `iso_root_intruder_is_signalled_through_pidfd`
- `iso_root_registry_restart_quarantines_reserved_uid_and_preserves_tombstone`
- `iso_root_registry_rejects_untrusted_state_directory`
- `iso_root_half_created_account_is_locked_and_read_back`
- `iso_root_systemd_fd_delivery_and_restart_reconciliation`
- `iso_root_systemd_resource_hierarchy_readback`
- `iso_root_bootstrap_plan_seals_and_readback`
- `iso_root_mounts_private_recursive_readonly_and_exec_identity`
- `iso_root_mounts_projection_residue_cleanup`
- `iso_root_run_journal_fsync_failure_poison_and_reopen`
- `iso_root_readiness_report_atomic_mode_and_boot_validation`

Focused non-root filters: `admission::tests`, `registry::tests`, `accounts::tests`,
`policy::tests`, `systemd::tests`, `readiness::tests`, `bootstrap::tests`,
`storage::tests`, `host_worker::tests`, `host::tests`, `host_state::tests`,
`fds::tests`, `transport::tests`, `channels::tests`, `server::tests`, `audit::tests`.
The systemd ignored FD test uses test-only fixed cat/sleep/false fixtures; it proves
PID 1 fd delivery/recovery, not a real B2 isolated employee run.

### Changed file inventory

| Files | Responsibility |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `crates/duduclaw-spawnd/Cargo.toml` | New workspace member; reuse locked dependencies, including Linux SHA-256. |
| `src/host.rs`, `src/host_worker.rs`, `src/host_worker_tests.rs`, `src/host_state.rs` | Production composition, separate scanner/cancellation, bounded lifecycle jobs, durable runs and expiring readiness. |
| `src/bootstrap.rs`, `src/bootstrap_mounts.rs`, `src/storage.rs` | Sealed B2 contract, concrete root fd/mount preparation, H adapter API. |
| `src/accounts.rs`, `src/registry.rs` | Permanent identities, account lockdown and restart quarantine. |
| `src/systemd.rs` | Fixed transient launch, AddRef, actual resource readback and orphan recovery. |
| `src/main.rs`, `src/server.rs`, `src/transport.rs`, `src/fds.rs`, `src/lib.rs` | Actual host selection, check/serve, shutdown drain, authenticated sockets and descriptor validation. |
| `src/admission.rs`, `src/admission_tests.rs`, `src/policy.rs` | D8/queue/timeout state machine, async polling/shutdown tests, root policy. |
| `src/channels.rs`, `src/egress.rs`, `src/audit.rs` | Bounded output, capability revocation, control acknowledgements and typed audit. |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Optional signed-binary-dependent staging and template tests; not compiled locally. |
| `scripts/linux-protected/spawnd/*` | Socket/service/resource unit templates and installation boundary. |
| crate README, this note, `docs/README.md`, `CHANGELOG.md` | Current development status, compatibility seams and exact acceptance filters. |

No shared contract change and no new dependency version. The three public security
feature pages remain unchanged because employee isolation is not enabled. B1 uses
[systemd v249's fd setter](https://raw.githubusercontent.com/systemd/systemd/v249/src/core/dbus-service.c)
and [execution properties](https://raw.githubusercontent.com/systemd/systemd/v249/src/core/dbus-execute.c);
source inspection does not substitute for the native tests above.

The fixed account utility paths follow the [Debian passwd file list](https://packages.debian.org/bookworm/amd64/passwd/filelist)
(`/usr/bin/chage`); scheduler inspection uses [at -l](https://manpages.debian.org/bookworm/at/at.1.en.html)
instead of executing the atq symlink. These paths still require distro acceptance.
This page records implementation boundaries; it is not an installation guide or
evidence that protected execution is enabled.

## Package B2

`crates/duduclaw-run` contains the Linux root trampoline library and executable.
The gateway is not connected to it. The executable uses `UnavailableRegistry` and
refuses with `RegistryUnavailable`; package B1 must supply a trusted `RootRegistry`
adapter before any production run can start. No fallback executes as the gateway.
There is no setuid installation, systemd deployment, runtime manifest or default
permissive runtime profile in this package.

### Launch and trust boundaries

`linux::runner::serve` owns a fresh, single-threaded process. It first disables
dumping, closes inherited descriptors above 2 and authenticates bootstrap fd 0 as
an AF_UNIX SOCK_SEQPACKET endpoint whose peer uid is root. Output fds must be
write-only pipes. The receiver bounds frames, uses `MSG_CMSG_CLOEXEC`, closes all
received descriptors on rejection, and refuses truncation and unknown ancillary
messages. A random one-use challenge binds the response to this bootstrap.

The private bootstrap uses the existing `ipc::Envelope` framing/version. Its
challenge contains only a 32-byte nonce, and its offer contains that nonce and a
`RunId`; both use request id zero. No uid, path, argv, environment, profile or
mount option is accepted from the offer. `RootRegistry::take_run` resolves and
consumes the run against B1's root-owned verified registry. `TrustedRun` is a
non-serializable integration object; it is not safe to construct it from a gateway
request. B1 owns manifest/digest verification, account allocation, admission,
resource quotas, minimum `/etc` projection, transient unit policy and cleanup.
B1 must enforce `KillMode=control-group` and kill the entire run cgroup on any
bootstrap error, EOF or startup timeout. Library launch outside that transient
unit is unsupported. PID 1 waits at a bounded shared-memory startup barrier until
the supervisor has dropped/verified privileges and installed its final filter;
supervisor initialization failure therefore cannot release a runtime.

Ancillary slots are fixed in this order:

1. Detached mounts: employee home, workspace, configuration projection, runtime
   registry, minimal `/etc` projection (including empty `ssl/certs`).
2. Runtime ELF or interpreter, then its script fd if applicable.
3. Sealed prompt, then the declared attachments in order.
4. MCP socketpair endpoint if enabled.
5. Egress socketpair endpoint and forwarder ELF if enabled.

Bootstrap caps descriptors at 80; the runtime shuffle independently enforces at
most 72 sources, 64 attachments and `RLIMIT_NOFILE >= 1024 + 2N + 16`. The shared
contract's prompt, attachment and aggregate memfd bounds are unchanged. Prompt
pipes are not accepted: the shared `StartRun` contract requires sealed memfds.
Prompt and attachment file offsets are reset to zero after validation.

Mounts are detached clones with registry-supplied identities. B2 validates them,
reasserts recursive private propagation and restrictive attributes, moves them
by fd, checks mount identities/filesystem flags and closes their descriptors.
The new filesystem contains read-only system trees and runtime registry, minimal
`/etc`, per-run bounded tmpfs mounts, validated device nodes and a fresh bounded
devpts. PTY ownership uses **fchmod before fchown**; B1 must include CAP_CHOWN in
the transient unit bounding set. The master is sent to B1 and closed locally.
After pivot the old root is detached and the new root itself becomes read-only.
Only loopback is brought up. PID 1 mounts fresh procfs with nosuid/nodev/noexec;
removing `subset=pid` requires an explicit trusted runtime compatibility exception.

The outer parent keeps only bootstrap, status-read and pidfd, drops to the
registered supervisor identity, verifies all IDs/capabilities/securebits/NNP and
non-dumpability, then installs the tight supervisor filter. PID 1 independently
drops to the employee identity and applies the common Landlock/seccomp policy.
Persistent home/work rules deny socket, FIFO and device creation. Init then forks
the optional forwarder and runtime, closes capabilities and becomes a reaper and
signal forwarder. Termination signals are blocked during fork/registration and
restored before exec. A forwarder exit terminates the run. The runtime uses a new
session, optional controlling PTY, fixed cwd `/work`, fixed descriptor numbers,
an independently digest-bound allowlist and `execveat` on the validated ELF.

Runtime descriptor targets are 0/1/2 stdio, 3 MCP, 4 prompt and 5+ attachments.
Disabled MCP uses `/dev/null` at 3. Without a PTY, stdin is `/dev/null`; the prompt
remains at 4. Forwarders receive only their egress endpoint at 3, with null stdio
and null fd 4. Every source is first duplicated above 1024 before originals are
closed. Final readback scans `F_GETFD` through `exec_fd + 1` and compares every
target's statx identity; no `/proc/self/fd` enumeration allocates a descriptor.
Script execution bypasses shebang, verifies a root-published path relative to
`/runtime` with `openat2`, compares inode/device/mount, and closes the script fd.

### Status and implementation clarifications

Status packets are exactly 12 bytes: `DCR1`, one-byte kind, one-byte fixed code,
two zero reserved bytes and a big-endian u32 value. Kinds are 0 error, 1 employee
privilege-readback complete (value = Landlock ABI), 2 PTY delivery (exactly one
master fd), 3 children forked, 4 exited, 5 signalled and 6 supervisor verified and
filtered. Kind 3 is not an exec or
product-readiness receipt. Error codes 1–16 map in order to the variants in
`bootstrap::error_code`. No packet contains an errno, path, prompt, uid, argv or
credential. B1 must combine these records with actual unit/cgroup readback.

Two sequencing clarifications resolve contradictions in the design text:

- Supervisor capget/prctl readback occurs immediately **before** installing the
  filter, because that filter intentionally forbids both syscalls. Installation
  is checked; only the fixed reporting/wait loop runs afterwards.
- A shared anonymous page containing an atomic fixed error code and a supervisor
  startup barrier spans the final fd sweep and exec. Failed filter installation
  or exec stores a code and exits;
  the supervisor reports the code after PID 1 exits. Successful exec removes the
  mapping, so employee code never obtains it. This preserves the exact fd
  whitelist and reports late errors without retaining an extra error fd. The
  first error wins. The page contains no credentials or reusable capabilities.

All runtime seccomp profiles are expanded syscall lists bound to SHA-256 over
`duduclaw-runtime-seccomp-v1\0`, the big-endian audit architecture, the big-endian
u32 count and sorted big-endian u32 syscall numbers. Duplicates are rejected.
This is a B2 private profile encoding, not a change to shared wire/canonical
capability contracts. Common restrictions cannot be overridden by a runtime list.
The common filter additionally refuses ptrace/process_vm operations and limits
socketpair to AF_UNIX. These are deliberate narrowing defenses.

Kernel readiness checks require Linux 5.15+, actual Landlock ABI 1–7, seccomp
KILL_PROCESS, clone3, openat2, statx mount IDs, close_range and detached fs-context
mount APIs including recursive mount_setattr/NOSYMFOLLOW. Unknown Landlock ABIs
fail closed pending review. Launch still verifies every actual namespace, mount
and privilege syscall. Kernel capability probing does not prove B1/systemd/egress
readiness or replace the three-distribution acceptance run.

### Round 2 implementation and handoff

The B2 launch implementation and its integration-test fixtures are present.
Production activation is still blocked on B1 integration and Linux acceptance.
The executable retains `UnavailableRegistry`; there is no permissive default.
No shared `duduclaw-isolation` wire format, canonical bytes or hard limit changed.

The root sender can now use `bootstrap::receive_challenge`, `send_offer` and
`receive_status` rather than reconstructing the private protocol. The first two
validate the root peer and apply bounded socket I/O deadlines. `send_offer` sends
only the run key, nonce and ordered descriptors; B1 keeps descriptor ownership.
The same connected endpoint must be retained after the supervisor drops its uid.
`StatusRecord::decode` rejects unknown kinds/codes, nonzero reserved bytes,
out-of-range ABI/exit/signal values, trailing bytes, and incorrect ancillary counts.
A PTY record still requires B1's independent identity/mount readback. B1 must poll
with its startup/runtime deadline before calling `receive_status`: the socket's
five-second per-receive timeout is not a running task's lifetime limit. EOF/error
requires whole-cgroup cleanup; `ChildrenForked` never proves successful exec.

Receiver tests exercise ordered CLOEXEC descriptors, unknown credentials,
`MSG_CTRUNC`, complete descriptor cleanup on refusal, and malformed status
records. The receive-side `F_GETFD` readback now explicitly rejects syscall failure.
The fixed supervisor sender remains allocation-free after its seccomp filter;
the multi-descriptor offer sender is a separate root-side function.

The debug-only root fixture now calls the real `runner::serve`, including nonce
handshake, a single-use test `RootRegistry`, detached mounts, namespace assembly,
privilege drops, common and branch-specific filters, descriptor reordering and
`execveat`. Payload assertions check inherited prompt/attachment seals and offsets,
MCP vs egress separation, exact post-exec descriptor set, isolated namespaces,
fixed working directory and the explicitly rebuilt environment. A script with an
invalid shebang is executed through a copied, registered dash interpreter. PTY
assertions cover the controlling session and identical stdio device identities.
The fixture profile is deliberately test-only; it is not a signed vendor profile
or runtime-compatibility acceptance.

Failure cases cover a malformed ELF that passes the initial magic check but
fails exec, an exiting forwarder, supervisor privilege-drop failure injected by
an inherited test filter, cancellation and a double-forked child. They check
fixed error records, supervisor exit, the runtime barrier and pidfd death.
Cancellation is tested both after the payload is ready and immediately after
the children-forked receipt, while runtime startup may still be in flight.
A separate propagation test moves a still-shared detached mount through the
production `build_root`/`pivot`, adds late source submounts at two depths, and
compares against a shared-peer positive control. Source mount identity and empty
host directories must remain unchanged when the disposable namespaces exit.

Every fixture is enclosed in its own PID namespace with a fresh matching procfs
and a 256 MiB private tmpfs for all test files; namespace death also removes
those files after panic, timeout or _exit. The payload is copied from an opened
/proc/self/exe fd so test checkouts under /tmp continue to work.
An outer supervisor enforces a 45-second deadline; the test harness additionally
enforces 60 seconds. Killing fixture PID 1 kills its descendants, including those
that called setsid or changed uid. Credential-changing primitive fixtures also
reassert their parent-death signal. This replaces the earlier assumption that a
pre-drop parent-death signal survives an effective-identity change. Per-process
NOFILE is set to 4096 only when the existing hard limit allows it. No account,
host sysctl, host mount, unit or production directory is changed.

### Validation commands

Normal tests and cross checks (small crates only):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-run --tests --bins --features root-test-fixture --target x86_64-unknown-linux-gnu --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-run --target x86_64-pc-windows-gnu --offline
```

On each disposable root-capable Ubuntu 22.04/24.04 and Debian 12 VM:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --lib --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --features root-test-fixture --test root -- --ignored iso_root_ --test-threads=1
```

The 16 exact ignored test filters are:

- `iso_root_same_employee_pathname_socket_scm_rights_impossible`
- `iso_root_employee_cannot_read_gateway_data_dir`
- `iso_root_employee_cannot_reach_metadata`
- `iso_root_employee_cannot_ptrace_other_run`
- `iso_root_cyclic_fd_reorder_has_exact_whitelist`
- `iso_root_mount_tree_pty_and_proc_readbacks`
- `iso_root_detached_mount_private_propagation`
- `iso_root_full_launch_elf_capability_fd_handoff`
- `iso_root_full_launch_script_bypasses_shebang`
- `iso_root_full_launch_pty_control_terminal`
- `iso_root_full_launch_reports_exec_failure`
- `iso_root_full_launch_forwarder_failure_terminates_run`
- `iso_root_full_launch_cancellation_forwards_signal`
- `iso_root_full_launch_startup_cancellation_race`
- `iso_root_full_launch_reaps_double_fork_descendants`
- `iso_root_full_launch_supervisor_failure_blocks_runtime`

Missing kernel/root prerequisites fail rather than skip. Root tests are never
counted as passed by a cross check or by an ordinary test run that ignores them.
The script fixture additionally requires `/bin/dash` on those target distributions.

Remaining work is assigned explicitly:

| Owner | Remaining gate | Why it is outside this B2 patch |
| --- | --- | --- |
| B1/integrator | Wire `RootRegistry::take_run` and the sender into verified registry/transient units; verify root manifests, account registry, CAP_CHOWN/CAP_SETPCAP, NOFILE, device policy, cgroup cleanup and admission. | B1 owns host policy, registry, units and admission; this package provides both sides of its private bootstrap interface. |
| A/C/D/integrator | Exercise real CLI profiles, inherited bridge fd, forwarder and capability lifecycle with signed runtime manifests. | Test payloads and a test syscall envelope cannot certify real runtime compatibility or those packages' services. |
| Linux acceptance runner | Run the complete ignored suite and actual transient-unit launch on the three target distributions. | This session has macOS execution and cross-check evidence only. |
| Implementation reviewer | Daybreak security review of the integrated implementation. | Independent code review here is not a Daybreak or protected-mode release acceptance. |

No live-provider, protected-mode, release or customer acceptance is claimed.
No commit, merge, deployment, cargo fmt, gateway build or CLI build was performed.

### Round 2 observed checks

All commands used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0` in the assigned
worktree. These are current-session observations, separate from the archive.

| Command after that prefix | Observed result |
| --- | --- |
| `cargo test -p duduclaw-run --offline` | PASS on macOS: 18 passed, 0 failed; 16 root tests ignored. |
| `cargo test -p duduclaw-isolation --offline` | PASS on macOS: 29 passed, 0 failed; shared sources unchanged. |
| `cargo check -p duduclaw-run --lib --tests --target x86_64-unknown-linux-gnu --offline` | PASS; Linux-only unit tests compile but did not execute. |
| `cargo check -p duduclaw-run --tests --bins --features root-test-fixture --target x86_64-unknown-linux-gnu --offline` | PASS; complete Linux fixtures compile but did not execute. |
| `cargo check -p duduclaw-run --target x86_64-pc-windows-gnu --offline` | PASS; no Windows execution. |
| `cargo check -p duduclaw-run --release --bin duduclaw-run-root-fixture --features root-test-fixture --offline` | Expected refusal, exit 101: debug fixture cannot compile in release. |
| `cargo test -p duduclaw-run --test root --offline -- --ignored iso_root_full_launch_elf_capability_fd_handoff` | Expected refusal, exit 101: one test fails explicitly without the opt-in feature; no root execution occurred. |

Round 2 files: `src/lib.rs` exports the status module; `src/status.rs` validates
fixed receipts; `src/linux/bootstrap.rs` adds sender/decoder seams, nonce and fd
refusal tests, and fail-closed fcntl handling; `src/bin/root-fixture.rs` and
`tests/root.rs` provide bounded isolated test orchestration;
`src/bin/root-fixture/launch.rs` and `propagation.rs` implement the new acceptance
scenarios. Crate README, this note and the B2 changelog entry are updated.
The earlier wiki handoff is removed from the wiki tree and preserved below.
No dependency or shared-contract change was needed in round 2.

Independent code review covered the actual source. Fixed findings included the
credential-change parent-death handling, an unread status queue on POLLHUP and
negative F_GETFD handling; the runtime polling requirement is now explicit.
This review does not substitute for the requested Daybreak implementation review.

### Round 1 evidence archive

The following handoff was moved from the published wiki tree during round 2.
Its test counts and remaining-work list describe round 1 only; the round 2
section above will supersede that status. No earlier Linux execution is implied.

### Package B2 handoff — 2026-11-05

Status: **PARTIAL — compiling implementation, B1 integration and native acceptance pending.**
Branch: `feat/iso-b2-20261105`. No commit, merge, release or deployment was made.
Only the assigned worktree was changed. No cargo fmt, gateway/CLI/workspace build,
production data access or host configuration change was performed.

#### Files

Paths below are relative to the worktree.

| File | Change |
| --- | --- |
| `Cargo.toml` | Add workspace member `duduclaw-run`. |
| `Cargo.lock` | Add the new workspace package; existing dependency versions stay unchanged. |
| `crates/duduclaw-run/Cargo.toml` | Small library/binary; Linux-only libc/Landlock; explicit test fixture feature. |
| `crates/duduclaw-run/README.md` | Build/test instructions, disabled adapter and integration requirements. |
| `crates/duduclaw-run/src/lib.rs` | Portable policy modules and Linux-gated syscall modules. |
| `crates/duduclaw-run/src/main.rs` | Fail-closed entry point; non-Linux refuses; root registry adapter currently unavailable. |
| `crates/duduclaw-run/src/error.rs` | Fixed sanitized failures mapped to shared IPC error codes. |
| `crates/duduclaw-run/src/state.rs` | Ordered branch-aware state machine and refusal tests. |
| `crates/duduclaw-run/src/fd_plan.rs` | Dense fixed targets, source limits, rlimit formula and cyclic-source validation. |
| `crates/duduclaw-run/src/validation.rs` | Pure executable/mount/seal/snapshot/PTY matrix and aggregate input bounds. |
| `crates/duduclaw-run/src/seccomp.rs` | Decoded/tested x86_64/aarch64 BPF, common/supervisor/init/runtime policies and profile digest. |
| `crates/duduclaw-run/src/landlock.rs` | Strict ABI probing and filesystem permissions, including D8 restrictions. |
| `crates/duduclaw-run/src/linux/mod.rs` | Linux syscall boundary and checked error helpers. |
| `crates/duduclaw-run/src/linux/bootstrap.rs` | Root peer validation, nonce handshake, bounded SCM_RIGHTS and fixed status records. |
| `crates/duduclaw-run/src/linux/fd.rs` | statx/fstatfs observation, openat2, script identity check and actual close/reorder/readback. |
| `crates/duduclaw-run/src/linux/mounts.rs` | Private namespaces, fd-based filesystem assembly, PTY, pivot, proc and loopback. |
| `crates/duduclaw-run/src/linux/privileges.rs` | Securebits, bounding/capability drop, IDs/groups/NNP/dumpable readback. |
| `crates/duduclaw-run/src/linux/probe.rs` | Kernel/ABI/API readiness checks without host mount attachment. |
| `crates/duduclaw-run/src/linux/runner.rs` | Launch sequence, supervisor barrier, PID 1 reaping/signals, forwarder/runtime exec and late error reporting. |
| `crates/duduclaw-run/src/bin/root-fixture.rs` | Single-threaded root-only fixture, forbidden in release builds. |
| `crates/duduclaw-run/tests/root.rs` | Six explicit ignored root acceptance tests. |
| `docs/architecture/native-linux-isolation.md` | B2 integration interface, ordering clarifications and acceptance boundaries. |
| `docs/README.md` | Index the implementation note. |
| `CHANGELOG.md` | Self-contained zh-TW Unreleased development entry, without enabled-isolation claims. |
| Prior B2 handoff | Moved into this Package B2 archive in round 2. |

Public security feature pages in three languages were not changed: this package
does not enable or change existing installation security behavior. The assignment
explicitly limits those feature claims to enabled behavior.

#### Commands actually run

All cargo commands used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`. No formatter was run.

| Command (after that environment prefix) | Observed result |
| --- | --- |
| `cargo test -p duduclaw-run --offline` | PASS on macOS: 16 passed, 0 failed; six root tests ignored. |
| `cargo test -p duduclaw-isolation --offline` | PASS on macOS: 29 passed, 0 failed. |
| `cargo test -p duduclaw-run seccomp --offline` | PASS: 6 passed (delegated module check). |
| `cargo test -p duduclaw-run landlock --offline` | PASS: 3 passed (delegated module check). |
| `cargo check -p duduclaw-run --target x86_64-unknown-linux-gnu --offline` | PASS; initial run had three scoped unsafe warnings, subsequently annotated. |
| `cargo check -p duduclaw-run --tests --target x86_64-unknown-linux-gnu --offline` | PASS. |
| `cargo check -p duduclaw-run --tests --bins --features root-test-fixture --target x86_64-unknown-linux-gnu --offline` | PASS on final implementation, no warnings. This compiles Linux tests; it does not execute them. |
| `cargo check -p duduclaw-run --target x86_64-pc-windows-gnu --offline` | PASS. No Windows execution claimed. |
| `cargo test -p duduclaw-run --test root --offline -- --ignored iso_root_cyclic_fd_reorder_has_exact_whitelist` | Expected FAIL, exit 101: missing `root-test-fixture` explicitly refuses instead of falsely passing (delegated guard test). |
| `cargo check -p duduclaw-run --release --bin duduclaw-run-root-fixture --features root-test-fixture --offline` | Expected FAIL, exit 101: release fixture compile_error (delegated guard test). |

`git diff --check` passed. A whitespace/conflict-marker scan included the new,
untracked files. Independent code review and follow-up static review found and
closed the signal race, late exec-error reporting, forwarder-exit handling and
supervisor initialization race. This was not a Daybreak security acceptance.

#### Commands for the Linux builder

Run normal unit tests on native Linux first (includes Linux-only UAPI, bootstrap,
mount-policy, readiness-parser and shared-status tests):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --lib --offline
```

On a disposable root-capable Linux VM, build/run the opt-in fixture:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --features root-test-fixture --test root -- --ignored iso_root_ --test-threads=1
```

Exact root filters:

- `iso_root_same_employee_pathname_socket_scm_rights_impossible`
- `iso_root_employee_cannot_read_gateway_data_dir`
- `iso_root_employee_cannot_reach_metadata`
- `iso_root_employee_cannot_ptrace_other_run`
- `iso_root_cyclic_fd_reorder_has_exact_whitelist`
- `iso_root_mount_tree_pty_and_proc_readbacks`

These tests assert required root/kernel support, use disposable fixtures, and
fail instead of skipping missing prerequisites. They test primitives, including
real mount/PTY/proc construction; they do not invoke the entire `serve/launch`
with a real spawnd transient unit. The ptrace case deliberately leaves sibling
PIDs visible to test the seccomp rejection directly.

#### Precisely remaining

1. B1 must implement and wire `RootRegistry::take_run`; the executable intentionally
   remains `RegistryUnavailable`. B1 must adopt the documented nonce/offer/fd order,
   readback/status records and profile digest encoding. It must derive every
   trusted field from root policy and signed runtime manifests, never gateway
   overrides. No signed runtime profile was invented in this package.
2. B1 must verify actual unit attributes (including CAP_CHOWN, CAP_SETPCAP,
   LimitNOFILE and KillMode=control-group), admission, cgroup cleanup and account
   registry invariants. Any bootstrap error, EOF or timeout must kill the entire
   cgroup. Startup waits for the supervisor barrier, but death after readiness
   still relies on systemd/B1 cleanup as the approved design specifies.
3. Native Linux execution has not been performed. Run unit and six ignored tests
   on Ubuntu 22.04/5.15, Debian 12/6.1 and Ubuntu 24.04/6.8; test systemd device
   policies with the new devpts instance and real runtime fd inheritance.
4. Add full B1-to-trampoline integration tests with both ELF and script runtimes,
   PTY, capability fd handoff, supervisor-failure injection, exec failure,
   cancellation races, forwarder failure and namespace/cgroup teardown. Unit
   tests for the shared barrier do not substitute for this process-level evidence.
5. Complete signed runtime allowlist compatibility tests and the implementation
   Daybreak security review. No live-provider, protected-mode, release or customer
   acceptance is asserted.

#### CONTRACT CHANGE and deviations

**CONTRACT CHANGE: none to `duduclaw-isolation`**. No shared wire formats, canonical
capability bytes, hard limits or sources were changed; its 29 tests remain green.
B2 introduces a private root bootstrap interface and runtime-profile digest
encoding documented in the architecture note; B1 integration must align with them.

The supervisor performs capability/prctl readback before its final seccomp filter,
because the specified filter prohibits those readback syscalls. A shared anonymous
atomic status page reports post-fd-sweep exec errors without adding an inherited
fd. These resolve the design's ordering conflicts and are explicit review items.

The shared contract mandates sealed prompts, so the optional pipe prompt variant
in the design is not accepted. Common seccomp additionally denies ptrace and
process_vm calls and narrows socketpair to AF_UNIX. Unknown Landlock ABI >7 is
refused pending review. The launcher uses fixed cwd `/work`; B1's environment is
passed from scratch through execveat, never inherited from this process.

Dependencies are existing-lockfile `libc`, `landlock 0.4.5`, `sha2 0.10`, serde,
serde_json, thiserror and the shared contract. No heavy or new registry dependency
was added. Syscall assembly uses libc/raw Linux UAPI in the small Linux boundary;
Landlock uses the existing crate, and BPF construction is locally decoded in tests.
Implementation notes for the r7 design. These packages are not enabled by the
existing gateway. Building a package does not establish protected-mode readiness.

## Package C — egress proxy

`crates/duduclaw-egress` supplies the host daemon, namespace CONNECT forwarder,
and a separate DNS-only broker mode. Production OS operations are Linux-only;
address policy, CONNECT parsing, budgets and environment construction are portable.
The shared isolation contract is unchanged. Dependencies already present in the
workspace lockfile are reused: Tokio, serde/serde_json, nix 0.29 and libc.

### Guarantee and limits

The guarantee is **no connections to non-public IPs**, with destination ports
restricted by the root-selected profile. This is not content filtering. Tunnels
carry plain TCP bytes (normally end-to-end TLS); no SNI, HTTP Host or domain
allowlist is enforced. Public relays remain reachable. A public endpoint proxying
a private service is outside this IP-destination guarantee. AF_VSOCK is neither
representable as a destination nor enabled in the service address-family policy.

The policy follows the [IANA IPv4 registry](https://www.iana.org/assignments/iana-ipv4-special-registry/)
and [IANA IPv6 registry](https://www.iana.org/assignments/iana-ipv6-special-registry/)
(2025-10-09 registry revision). Non-global, multicast, broadcast, loopback,
link-local, private, CGNAT and documentation ranges are denied. The implementation
also denies all mapped/compatible IPv4, NAT64, 6to4 and Teredo prefixes. IPv6
outside currently allocated global unicast `2000::/3` is denied conservatively.
Registry changes require code and table-test review; there is no runtime download.
Standard Rust IP parsing is used; ambiguous decimal/octal/hex/short IPv4 spellings,
zone IDs, URLs, userinfo, non-ASCII DNS labels and invalid DNS syntax are refused.
International names must arrive as ASCII punycode.

Every tunnel resolves afresh, rejects the entire result if any answer is not
public, and connects numeric `SocketAddr`s from that checked set. A connect never
resolves the hostname again. All retries remain within the pinned set. DNS and
connect together have the profile's deadline, and the connected peer is checked.

### Integration seams for A and B1/B2

The named B1 seam is `controller::EgressController` (object safe). Its concrete
`RootEgressController::connect()` is called inside a Tokio runtime by root
spawnd; it accepts only the fixed root:root 0600 control socket, checks protected
ancestors and root peer credentials, and has no configurable alternate path.
`register(run, profile)` creates the CLOEXEC socketpair itself, sends its host
end with SCM_RIGHTS, and returns the forwarder end only after a correlated
`Registered` acknowledgement. `unregister(run)` waits for `Unregistered` after
host-side cleanup. The shared v1 messages and hard limits are unchanged.

B1 retains the controller for its lifetime and calls unregister on every run
exit/cancellation. An I/O error, bad/extra-fd ACK, wrong request ID/direction,
five-second timeout or cancellation of a polled operation drops the control
socket and permanently poisons that client. This revokes **all** its runs; B1
must stop their runtime units and must not interpret reconnect as restoring old
capabilities. A valid correlated `Refused` response keeps the channel usable.
Dropping a future before polling it has no effect. `UnavailableEgress` is the
default implementation and always denies; no permissive fallback is provided.
The daemon also authenticates the peer at its `serve_control` integration entry.


- Root starts the host binary through the supplied systemd socket and service
  templates. The service uid/gid must be `duduclaw-egress`, with zero capabilities.
  The daemon verifies uid/gid, `PR_SET_DUMPABLE=0`, `NoNewPrivileges`, configuration
  ownership and socket activation. After bootstrap/probes it installs a native
  x86_64/aarch64 seccomp BPF allowlist with TSYNC; other architectures fail closed.
  Unknown syscalls return EPERM and a mismatched syscall architecture kills the
  process. Socket creation permits only AF_UNIX/INET/INET6. exec, clone, namespace
  creation and socketpair are refused. The socket is root-owned at the contract's
  `/run/duduclaw-egress/control.sock`, `SOCK_SEQPACKET`, mode 0600. It authenticates
  the controller's uid **and** gid as zero; PID never authorizes a request.
- B1 keeps one control connection for its lifetime. `ipc::Envelope<EgressControl>`
  uses the unchanged length-prefixed v1 seqpacket format. `Register` carries one
  root-created unnamed AF_UNIX stream socketpair endpoint via SCM_RIGHTS.
  Unknown fields/cmsgs, extra/missing fds, truncation, wrong socket type, named
  sockets and non-root creation credentials fail closed. Received descriptors
  are CLOEXEC and closed on rejection, including data-plane ancillary fds.
- B1 waits for `Registered` before releasing the forwarder. Binding lives in the
  host run table; frame payloads cannot choose a run, employee, profile or resolver.
  Duplicate run IDs are rejected for the controller lifetime. Profiles are loaded
  once from root-owned `/etc/duduclaw/egress.json`; unknown IDs are refused.
- B1 sends `Unregister` when a run ends. The host cancels in-flight DNS/connects,
  waits for tunnel tasks and descriptors to close, then sends `Unregistered`.
  Control EOF terminates every run, drains cleanup and exits the daemon; systemd
  restarts it. A restart does not restore old registrations. B1 must consider
  all prior capabilities revoked and register fresh runs only after readiness.
- B2 gives only the forwarder its egress socket as fd 3. Invoke the same ELF as
  `duduclaw-egress forwarder <port> '<EgressLimits JSON>'`. The optional JSON is
  bounded to 2048 bytes and is the root profile's limits, not authority from an
  employee. Omitting it uses `budget::default_limits()`. The forwarder binds
  `127.0.0.1`, accepts only CONNECT, returns 405 for other methods including
  absolute-form HTTP, and never retries outside the socket. The host enforces
  its own profile independently. The helper supports at most 16 simultaneous
  clients and 4096 lifetime tunnels, even if a profile permits more.
- A/B1 call `environment::child_environment(root_allowlist, Some(port))` and apply
  the result after `Command::env_clear()`. The input must already be a trusted
  allowlist, never the inherited process environment. All case variations of
  `*_PROXY` and `PROXY` are removed, then only upper/lowercase HTTP_PROXY and
  HTTPS_PROXY are set. `None` produces an offline map with no proxy variables.
  This helper does not sanitize arbitrary other environment variables. Namespace
  isolation, runtime registration and the offline/no-egress decision belong to B.

### Root configuration

`Config` is a strict JSON object with `profiles` (1–128 contract EgressProfile
objects), `run_caps` (`total_bytes`, `total_connections`) and `max_runs` (1–128).
No field has an implicit permissive default. A profile's digest is a root-published
registry key; the proxy does not invent a new canonical profile encoding.
`total_connections` is 1–65536; it counts all admitted connect attempts, including
DNS or connect failure, and is never refunded by tunnel closure. The byte cap
counts payload in both directions before it is forwarded; protocol headers are
bounded separately. `max_tunnels` is additionally capped at 256 per run to bound
queues, and profiles below a 256-byte frame limit are rejected.

Per-run token buckets enforce aggregate frames/second and bytes/second (one
second of burst). Violations close the run; the implementation does not promise
traffic shaping. Bounded queues provide backpressure; queue overflow closes the
run. The controller is capped at 100 requests/second and 10,000 registrations
per connection. The forwarder uses its smaller lifetime/client bounds too.

Tunnel IDs must strictly increase on a connection. Client Data before Connected,
unknown IDs, duplicate Connect or wrong-direction frames are fatal. Closing is a
bounded state: crossed Close is idempotent; after a host TCP EOF, already-sent
client data may be drained and charged until client Close, but never forwarded.
Both actors use bidirectional activity for idle timeouts. Once a frame's first
byte arrives, a separate deadline bounds the entire frame (slowloris protection).

One structured terminal audit record contains only run/profile IDs, aggregate
payload bytes, attempt count and a fixed error code. No hostnames, IPs, headers,
payload, credentials, prompts or parser diagnostics are logged. Ordinary EOF and
explicit cancellation currently share the `unavailable` terminal code. The unit
bounds journal rate; host-wide journal retention remains the installer's duty.

### DNS routes and systemd deployment

The root profile explicitly selects one route. There is no automatic runtime
fallback and no libc/NSS hostname lookup for destinations:

1. `resolved_varlink`: fixed AF_UNIX
   `/run/systemd/resolve/io.systemd.Resolve`, `ResolveHostname` family 0, DNS-only,
   no search, no synthesized answers, no cache. Requests and replies are bounded;
   unsupported/error/partial replies fail closed. This uses the
   [systemd v249 interface](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-varlink.c).
2. `dedicated_dns_broker`: fixed AF_UNIX
   `/run/duduclaw-dns-broker/resolve.sock`. Install a distinct non-root
   `duduclaw-dns` account and run `duduclaw-egress dns-broker` in its own unit.
   Root-owned `/etc/duduclaw/dns-broker.json` contains only
   `{"upstream":"<literal IP>:53"}` (bracket IPv6). The broker accepts only the
   egress uid, creates its own A and AAAA queries, and connects only that numeric
   upstream on TCP 53. No UDP or arbitrary qtype/packet forwarding is exposed.
   Both address-family queries must succeed; CNAME/DNAME without a final address
   in the response is refused. At most 32 DNS requests run concurrently.

Both routes verify the fixed socket's protected ancestors and service/root peer
uid; the daemon probes every configured route before accepting registrations.
These probes verify socket availability, not upstream DNS health. Broker socket
mode is root:egress-group 0660 in a root-owned directory. Its daemon must run as
a different uid from egress. DNS replies are limited to 64 KiB and 64 addresses.

The templates under `crates/duduclaw-egress/systemd/` are **not installed by this
change**. The egress template has an empty capability set, NNP, strict filesystem
protection, a fixed syscall allowlist, resource limits and AF_UNIX/INET/INET6 only.
Its private read-only `/run` exposes the control directory and, by default, only
the resolved socket. A broker installation must replace that DNS bind with the
broker socket/directory in a root-owned drop-in. Installations deliberately using
both routes must bind both. The broker has its own `/run` projection and syscall
allowlist. The installer supplies its exact upstream IP allow rule; the shipped
broker template denies all IPs until that drop-in is present.

**R7-M2:** never add the upstream resolver to the egress service's IPAddressAllow.
Only the separate broker receives that exception. Egress's IP rules form a second
layer for direct socket destinations, including metadata. They cannot inspect a
public relay or tunnel content. B1 still must read back actual unit properties
and cgroup BPF attachment; missing second-layer enforcement is reported separately
from the primary code policy. Neither those readbacks nor namespace construction
are implemented by this package.

### Validation and remaining acceptance

Round 2 supplies the controller implementation and eight ignored root tests.
The macOS session executed 9 egress portable tests and 29 unchanged contract
tests: all passed. Linux x86_64 and Windows checks compile all test targets;
they do not execute Linux code. No gateway/CLI/workspace build was run. No
new third-party package/version or shared contract change was made in either round.
The general independent code review is not the required Daybreak security review.

Final round 2 checks: macOS tests **38 passed, 0 failed**; Linux x86_64 and
Windows GNU `--all-targets` checks **passed**; tracked and all 23 new-file
whitespace checks **passed**; `git diff -- crates/duduclaw-isolation` is empty.
The first Linux check caught three `Gid::is_root` API errors in the new client;
these were changed to explicit numeric gid checks before the passing rerun.
The compiled Linux test inventory is 35 ordinary and 8 ignored test functions;
this inventory is not an execution result.

Normal commands (run the first command again on the Linux builder to execute
Linux-only controller, broker, forwarder, seccomp and host tests):

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-egress --all-targets --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-egress --all-targets --offline
```

The ordinary broker tests use real local TCP and Unix sockets: assert exact A
and AAAA queries, preserve private answers for host policy rejection, and refuse
partial-family failure and caller-supplied routes/methods/flags. DNS codec tests
cover CNAME/DNAME with a final address, alias-only refusal, NODATA, compression
and truncation. Alias-only answers are intentionally not followed by additional
queries. These are fixture results, not real upstream acceptance. Varlink flags
and reply fields were rechecked against systemd v249's
[implementation](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-varlink.c)
and [flag definitions](https://github.com/systemd/systemd/blob/v249/src/resolve/resolved-def.h).

Run root tests as root, **only on disposable acceptance VMs**, with serial test
execution. Missing prerequisites fail the tests rather than silently skipping:

| Test filter | Prerequisites / assertions |
| --- | --- |
| `iso_root_controller_register_revoke_and_disconnect` | No installed services or external network; actual root SCM_RIGHTS, two registrations, duplicate refusal, ACK-after-EOF and controller disconnect revocation. |
| `iso_root_unregister_ack_follows_data_socket_eof` | No installed services; raw wire-client regression for unregister ordering. |
| `iso_root_seccomp_real_syscalls` | Separate subprocess installs the actual TSYNC filter; denies VSOCK/netlink/packet sockets, user namespace, socketpair and writable open, while existing stream I/O, TCP and Tokio timers work. Does not install a filter in the parent runner. |
| `iso_root_live_forwarding` | `ISO_EGRESS_ECHO_ADDR`: operator-controlled public numeric TCP echo address, arbitrary port; payload checked across any TCP fragmentation. |
| `iso_root_concurrent_live_tunnels_and_byte_cap` | Same echo fixture; two simultaneous TCP tunnels share the lifetime byte cap; over-budget payload is refused and closes the run. |
| `iso_root_resolved_varlink` | Live resolved Varlink socket/account and `ISO_EGRESS_DNS_NAME`, a public DNS fixture. Run separately for direct and CNAME names on resolved 249/252/255. |
| `iso_root_dedicated_dns_broker` | Installed broker, its fixed upstream TCP53 allow rule, both service accounts and `ISO_EGRESS_DNS_NAME`; child clears supplementary groups then drops to egress uid before using the protected broker socket. Put the compiled test executable under a path traversable by the egress account. |
| `iso_root_installed_daemon_registration_and_revocation` | Installed egress socket/service, root profiles and chosen live DNS route, **no spawnd controller**. First profile: at least 9 total attempts, 32 frames/s, a practical idle timeout (e.g. 30 s). Exercises actual systemd activation, service hardening, DNS startup probe, forbidden IP/translation families, duplicate registration and revocation. The test owns the sole controller and its disconnect ends the daemon; systemd may restart it. |

```sh
# Self-contained root tests, no service installation required:
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --offline -- --ignored iso_root_controller_register_revoke_and_disconnect --test-threads=1
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --test host --offline -- --ignored iso_root_unregister_ack_follows_data_socket_eof --test-threads=1
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --test linux_acceptance --offline -- --ignored iso_root_seccomp_real_syscalls --test-threads=1

# After the installer supplies service accounts, units, root config and DNS:
ISO_EGRESS_ECHO_ADDR=<public-operator-controlled-echo-IP:port> ISO_EGRESS_DNS_NAME=<public-DNS-fixture> CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-egress --offline -- --ignored iso_root_ --test-threads=1
```

The all-tests command requires **both** DNS routes to be installed; otherwise
select the individual applicable test filters. Install/test the unit templates
on Ubuntu 22.04, Ubuntu 24.04 and Debian 12, including private `/run` projections
and real cgroup BPF attachment readback (B1). The root tests have been written
and Linux-cross-compiled, **not executed in this macOS worktree**. Aarch64 Linux
has not been cross-compiled here. Real DNS CNAME behavior, sustained traffic,
runtime proxy compatibility and multi-package lifecycle remain acceptance gates.

### Package C handoff (round 2)

Implementation inside C's scope is present: host-bound public-IP TCP forwarding,
DNS pinning and two DNS routes, bounded CONNECT/data/control handling, aggregate
budgets, audit redaction, service templates/seccomp, environment helper, and the
root controller seam. There is no feature switch enabling this in the gateway.

Remaining integration ownership is explicit:

- A/B1 use `child_environment` with `env_clear`, install/publish trusted config,
  instantiate `EgressController`, and wire lifecycle cancellation and readiness.
- B2 passes the acknowledged endpoint only to the forwarder as fd 3, selects
  the loopback port and root limits, and creates the per-run namespaces.
- B1 installs accounts/units, selects the DNS bind and broker upstream IP rule,
  and reads back unit and cgroup BPF enforcement. C never grants the resolver
  an exception in the egress service itself.
- The coordinator executes the Linux/root matrix, obtains Daybreak review and
  completes multi-package/provider/product acceptance. No deployment, merge,
  release or customer acceptance is claimed by this implementation.

Round 2 changed `controller.rs` (new typed client/seam and lifecycle regressions),
`transport.rs` (bounded fd sending and peer validation), `daemon.rs` (authenticate
its public control entry), `lib.rs` (Linux controller export), `resolver.rs`
(TCP broker regressions), `dns_wire.rs` (alias/NODATA cases), `tests/host.rs`
(fragment-safe live echo), and new `tests/linux_acceptance.rs` (five root tests).
README, this note and the existing Unreleased entry were synchronized. The old
wiki handoff was moved here; no security-gap note remains in `wiki/`.

**CONTRACT CHANGE: none.** No commit was made. The existing worktree/branch belongs
to the coordinating session; this session did not create or remove a worktree.

### Round 1 evidence (historical, moved from wiki)

The first-round handoff recorded 8 portable egress and 29 contract tests passing,
x86_64 Linux and Windows all-target checks passing, and whitespace checks passing.
It listed 20 Linux library tests, 8 ordinary host integration tests and 2 ignored
root tests as compiled only. Those are historical counts, superseded by the
round 2 tests above. Its remaining gates (Linux execution, installed systemd/DNS,
A/B1/B2 wiring, Daybreak and multi-package acceptance) remain explicitly assigned
above; none is inferred from the earlier compile results.

The original package added `Cargo.toml` membership and only a package entry in
`Cargo.lock` (no dependency upgrades), the zh-TW Unreleased entry and docs index;
crate manifest/README; `policy`, `environment`, `http`, `budget`, `transport`,
`session`, `resolver`, `dns_wire`, `forwarder`, `daemon`, `seccomp`, `lib`, `main`;
four systemd templates; and `tests/host.rs`. Public security feature pages were
left unchanged because the assignment limits them to behavior actually enabled.
## Package E: disposable parse worker

Round 2 implementation, 2026-11-05. This section contains the former
`wiki/impl/native-linux-isolation-package-e-20261105.md` handoff and supersedes its
remaining-work list. No commit, installation, migration, deployment or live
protected-readiness acceptance was performed. The supplied worktree already
existed; this session did not create a worktree.

### Process and resource boundary

`duduclaw-parse-worker` is a workspace member. Its default library has no document
parser dependencies; `--features engine` builds the disposable executable. A/G
install the signed binary at `/usr/libexec/duduclaw/duduclaw-parse-worker` and the
socket/template supplied under `crates/duduclaw-parse-worker/systemd/`.
The root-owned system socket `/run/duduclaw-parse/parse.sock` is mode 0660, group
`duduclaw`; its parent is root-owned and not group-writable. The client checks
socket/parent ownership and root peer credentials. The gateway receives no
permission to submit arbitrary systemd properties or executable paths.

One socket-activated DynamicUser instance handles one seqpacket request. The
systemd instance name is an accepted-connection identifier, not request_id;
responses still bind request_id. The worker has PrivateNetwork, IPAddressDeny,
no capabilities, NNP, private IPC/devices, an empty read-only root with runtime
libraries projected, no application state and a bounded temporary filesystem.
It disables dumpability before receiving content, verifies its dynamic uid range,
then applies rlimits and a native x86_64/aarch64 syscall allowlist. Parsing cannot
open files, create/connect sockets, spawn processes, change namespaces or exec.

Limits: 512 MiB address space and cgroup memory, no swap, one task, one CPU,
64 fds, 30-second maximum request budget, 35-second service runtime, 5-second
startup and 1-second stop grace. Output file hard limit is 64 MiB; the process
narrows it to the actual request's output budget, or zero for no-output requests.
The socket caps active connections at four and has a backlog of four. Socket
buffers accommodate the 256 KiB frame. The process-local client admission is
acquired before allocating snapshots; connection admission precedes snapshotting.

A failed socket exchange applies a process-local cooldown of at most 41 seconds
from connection establishment, throttling repeated allocations. A socket can
still be in the accept backlog before service deadlines start, so cooldown expiry
is not proof of retirement, a synchronous cancellation receipt or a host-wide
reservation. Kernel queues and workers can retain a memfd until teardown. A/B must provide aggregate accounting
and verified retirement through the named launcher seam below.

### File protocol and output publication

A frame is the unchanged four-byte length plus bounded JSON, with ordered
SCM_RIGHTS descriptors. Inputs are authorized by the caller, copied into sealed
memfds and reopened read-only. Worker checks include fd count, size, regular
file type, access mode, tmpfs backing and seals. Output plans accept one empty,
unlinked, writable, sealable memfd, distinct from every input/output inode.
Truncated frames/ancillary data, unknown ancillary messages and mismatched fd
plans reject the request and close received descriptors.

For image/pack output, the worker drops all input and writable output descriptors
before replying. The client applies all four seals before reading size, SHA-256
and bytes. A retained writable mapping makes sealing fail; a retained ordinary fd
cannot mutate a sealed output. The hash is consistency evidence, not content
trust. Missing artifacts, mismatched digests, actual size over budget and incomplete
manifests are rejected. Pack destinations are revalidated by the caller and
published from a private staging directory, using Linux renameat2 NOREPLACE.
No ZIP parser or image decoder runs in the publishing gateway/CLI path.

### Implemented routes

| Surface | Behavior when isolation is required |
| --- | --- |
| Knowledge ingestion and direct extractor | PDF, DOCX, PPTX, XLSX/XLS, CSV, plain text, Markdown and HTML through worker; segment kind/label/page/sheet/line_rows/warnings preserved |
| Office preflight | Full caller DocumentLimits policy transmitted and checked in worker; public path and bytes entrypoints both route to it |
| Office artifact delivery | Magic/placeholder policy stays in caller; ZIP preflight runs in worker on the blocking pool |
| CSV and typed spreadsheet tools | Delimiter/header/offset/limit/sheet options, typed values, columns and truncation preserved; normal auditing/redaction remains in caller |
| Image dimensions, resize, screenshot masks | Bounded first-frame decoding; JPEG/PNG artifact bytes verified after sealing; no raw-image fallback on failure |
| CSS extraction and web search | Selector priority, element attributes, text/HTML/recursive JSON shapes; bounded results; no caller-side DOM parser |
| Expert ZIP import | Worker expands bytes and returns bounded entry manifest; caller validates names/ranges/collisions, stages and publishes without parsing ZIP |
| Legacy backup/portability import | Refused before archive parsing/staging; protected migration belongs to G's importer |
| Local ONNX/tokenizer, model bundle extraction and Kokoro model loading | Refused before parsing; existing Embedder/TtsProvider interfaces remain the A service integration points |
| Office-to-PDF preview | Legacy soffice spawn refused; A must provide isolated conversion execution |
| Signed updater archives | F owns these; untouched |

The worker engine shares the existing knowledge format-reader source through
`#[path]` with a local data-model facade, rather than depending on the knowledge
crate or duplicating its parser implementations. Expert ZIP filename decoding is
also shared, including GBK/Big5 and junk-entry reporting. The default client build
does not compile these sources.

Implementation ceilings narrow the contract: 64 MiB input/ZIP expansion (pack
extraction 50 MiB), 1,000 archive entries, depth 32, 8 MiB XML parts, 50,000 cells,
16,384-pixel image axes and 64 MiB decoded pixels. Per-text item remains 8 KiB,
aggregate text including metadata remains 128 KiB, frame remains 256 KiB.
A knowledge segment over 8 KiB is rejected, not silently split or truncated.
Complete XML scanning refuses a part/document beyond its configured scan budget;
it never treats an unchecked tail as safe. Table pagination remains explicitly
marked truncated. HTML bounds include recursion depth and node count. Basic
archive inspection rejects nested archives; pack extraction retains nested files
as bytes without recursively parsing them, preserving the importer's behavior.

### CONTRACT CHANGE

Additive changes in `duduclaw-isolation::worker`:

- Optional `ParseRequest.options`, omitted when absent; existing requests serialize
  with the same fields. Existing protocol version, canonical capability bytes,
  existing operation encodings and hard limits are unchanged.
- Operations for structured knowledge/table extraction, document policy preflight,
  image transforms, HTML extraction and pack expansion; closed bounded option
  enums and result types for those operations.
- `worker_media.rs` supplies bounded HTML query options.

Older workers reject new operation/option fields. There is no legacy parser
fallback. Matching version 1 alone does not prove support for the new adapters;
A/G must install the matching signed component set and run readiness probes.
Compatibility review of these additions is still required at integration.

### Named integration seams and remaining gates

- A/G call `client::require_isolation()` before admitting work, or set the
  root-owned gateway unit's `DUDUCLAW_ISOLATION_REQUIRED`. Any presence latches
  required mode, with no reset. The old protected-key mode does not automatically
  activate the new path. Install/verify signed binaries and units, and gate
  protected readiness on live probes; these are A/G responsibilities.
- A/B may install `Box<dyn client::WorkerLauncher>` once through
  `install_launcher`. `launch(ParseRequest, input_reservation_bytes)` returns a
  `WorkerSession`; its `exchange` accepts only the contract request and
  descriptors. The reservation is always the 64 MiB input ceiling because reader
  adapters discover the exact length during snapshotting; a zero initial input
  size is never a zero-byte reservation. The custom
  implementation owns host resource reservations and retains them through
  cancellation until unit/cgroup retirement is proven. Default is the fixed
  socket-activated launcher, never local execution. Results from an injected
  launcher are also bounded and validated by the client.
- D owns immutable request identity and authorized dirfd/openat2 resolution.
  E only consumes already-authorized files. Current legacy path adapters use
  NOFOLLOW/NONBLOCK plus regular-file checks; these are not substitutes for D's
  ancestor/mount/resource authorization.
- G replaces legacy backup/portability restore with the low-privilege importer.
  A supplies isolated model/inference/STT/TTS service execution and preconnected
  service fds. Parse operations LoadModel/LocalInference/TranscribeAudio/
  SynthesizeSpeech are explicitly unsupported by this document/media worker.
- Ubuntu 22.04, Ubuntu 24.04 and Debian 12 runtime acceptance, aarch64 runtime,
  gateway/CLI remote compilation, and Daybreak review remain unverified. The
  engineering subagent review is not a Daybreak approval. No full-product
  readiness, gateway load-test, cgroup OOM-killer or customer acceptance claim is
  made by passing small-crate tests or cross-compilation.

### Validation and remote commands

Normal developer checks (no gateway/CLI build on the shared Mac):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-parse-worker --all-targets --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-parse-worker --all-targets --features engine --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-parse-worker --no-default-features --offline
```

Remote builder (explicitly not run locally):

```sh
cargo check -p duduclaw-gateway -p duduclaw-cli
cargo test -p duduclaw-gateway parse_isolation_
cargo test -p duduclaw-cli parse_isolation_
cargo test -p duduclaw-knowledge parse_isolation_
cargo test -p duduclaw-gateway artifact_gate::tests
cargo test -p duduclaw-gateway computer_use_orchestrator::tests
cargo test -p duduclaw-parse-worker --features engine
```

Disposable root Linux VM, after installing the exact current binary and both
units (also requires the duduclaw group and cgroup v2):

```sh
cargo build -p duduclaw-parse-worker --features engine
install -D -m 0755 target/debug/duduclaw-parse-worker /usr/libexec/duduclaw/duduclaw-parse-worker
install -m 0644 crates/duduclaw-parse-worker/systemd/duduclaw-parse.socket /etc/systemd/system/
install -m 0644 crates/duduclaw-parse-worker/systemd/duduclaw-parse@.service /etc/systemd/system/
systemctl daemon-reload
systemctl start duduclaw-parse.socket
cargo test -p duduclaw-parse-worker --features engine -- --ignored iso_root_ --test-threads=1
```

The ignored tests cover text, malformed CFB, ZIP bomb, XML depth, oversized
results, real image/CSS and ZIP output, identity/capabilities/namespaces/hidden
paths/cgroup limits, disconnect and silent-client deadline cleanup, recovery,
and a disposable child exercising address-space allocation refusal, SSRF syscall
refusal and the production watchdog. The normal Linux suite covers SCM_RIGHTS,
fd cleanup/seals, output hashes, frames, deadlines and seccomp. Linux-specific
tests were only compiled on this Mac. Allocation refusal under RLIMIT_AS is not
a measured cgroup OOM-killer test; the systemd cleanup probe is not gateway
responsiveness under concurrent malicious load.

### Earlier round evidence retained

Round 1 recorded 19 macOS worker tests and 29 contract tests passing; Linux and
Windows all-target checks, client-only check and diff whitespace check passed.
It did not build gateway/CLI or run real systemd tests. Its missing Office/table/
image/CSS/archive adapters and parser-entry inventory are superseded above.
Earlier intermediate 15/18-test counts referred to older code. No new external
package version was added: the engine reuses locked zip, quick-xml, calamine,
pdf-extract, image, encoding_rs, scraper, ego-tree, csv, chrono and sha2; Linux uses
locked nix/libc. No new heavy model runtime dependency was introduced.


### Round 2 observed validation

| Command | Observed result on this Mac |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline` | 59 passed, 0 failed; Linux tests excluded by cfg |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline` | 32 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-parse-worker --all-targets --features engine --offline` | PASS; includes Linux transport, sandbox and seven ignored root tests |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-parse-worker --all-targets --features engine --offline` | PASS; invocation refuses on unsupported platforms |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-parse-worker --no-default-features --offline` | PASS; default client excludes document parser dependencies |
| `git diff --check` | PASS |

During implementation, the initial manifest used an absent workspace sha2 alias;
it was changed to the already-locked 0.10 version. The first Linux check ran before
the new archive module registration landed and failed with unresolved
`archive_adapter`; the complete tree passed subsequent checks. Earlier 49/58-test
results precede the workbook/archive/reservation tests; 59 is the final macOS worker count.
The root tests are present and compiled, not executed. Gateway/CLI/knowledge
consumer tests are written but not run locally under the delegated build limit.

Engineering review found and fixed the leftover public Office refusal and async
worker waits in screenshot/web callers. A subsequent review found no new
HIGH/CRITICAL in the reviewed diff. This is not a Daybreak security signoff.

### Full uncommitted package file inventory

This includes files carried over from round 1 as well as the round 2 completion.
The shared contract changes are explicitly listed above.

| File | Change |
| --- | --- |
| `CHANGELOG.md` | Unreleased user-facing entry. |
| `Cargo.lock` | Local worker package and edges to already-locked dependencies. |
| `Cargo.toml` | Register worker workspace member/dependency. |
| `crates/duduclaw-cli/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-cli/src/expert/safe_zip.rs` | Isolated expansion, manifest validation, private staging and no-replace publication. |
| `crates/duduclaw-cli/src/mcp/messaging.rs` | Bound search response size and move DOM parsing to worker/blocking pool. |
| `crates/duduclaw-cli/src/mcp/web.rs` | Run isolated CSS extraction on blocking pool. |
| `crates/duduclaw-cli/src/mcp_files.rs` | Typed CSV/spreadsheet worker adapter and row-shape tests. |
| `crates/duduclaw-cli/src/portability.rs` | Refuse legacy tar imports before parsing in required mode. |
| `crates/duduclaw-gateway/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-gateway/src/artifact_gate.rs` | Isolated Office container delivery preflight on blocking pool. |
| `crates/duduclaw-gateway/src/backup_restore.rs` | Refuse legacy archive restore before read/staging. |
| `crates/duduclaw-gateway/src/computer_use.rs` | Isolated mask adapter and async blocking-pool wrapper. |
| `crates/duduclaw-gateway/src/computer_use_orchestrator.rs` | Move screenshot mask pipeline to blocking pool. |
| `crates/duduclaw-gateway/src/computer_use_sessions/mod.rs` | Await masks and recheck live session. |
| `crates/duduclaw-gateway/src/computer_use_sessions/oversight.rs` | Recheck stop/authority after asynchronous masking. |
| `crates/duduclaw-gateway/src/computer_use_sessions/tests/oversight_cases.rs` | Stop-after-mask regression test. |
| `crates/duduclaw-gateway/src/document_limits.rs` | Full policy worker route, safe file opening, public-entry tests. |
| `crates/duduclaw-gateway/src/knowledge/extract_spawn.rs` | Structured fd-only knowledge extraction; no legacy process fallback. |
| `crates/duduclaw-gateway/src/knowledge/pipeline.rs` | Document mandatory custom policy worker preflight. |
| `crates/duduclaw-gateway/src/media.rs` | Isolated resizing; propagate failure rather than original-image fallback. |
| `crates/duduclaw-gateway/src/server.rs` | Block legacy soffice preview until A supplies isolated conversion. |
| `crates/duduclaw-gateway/src/tts_kokoro/mod.rs` | Refuse gateway-uid model load; TtsProvider integration seam. |
| `crates/duduclaw-gateway/src/web_extract.rs` | Bounded CSS/HTML/JSON worker adapters. |
| `crates/duduclaw-isolation/src/worker.rs` | Additive options/operations/bounded results and compatibility tests. |
| `crates/duduclaw-isolation/src/worker_media.rs` | Bounded HTML selector query types. |
| `crates/duduclaw-knowledge/Cargo.toml` | Client-only worker dependency. |
| `crates/duduclaw-knowledge/src/embed/bundle.rs` | Refuse in-process model tar unpacking. |
| `crates/duduclaw-knowledge/src/embed/local_onnx.rs` | Refuse model load and cached ONNX session use in required mode. |
| `crates/duduclaw-knowledge/src/extract/docx.rs` | Delegate truncation helper through shared extractor facade. |
| `crates/duduclaw-knowledge/src/extract/html.rs` | Delegate truncation helper through shared extractor facade. |
| `crates/duduclaw-knowledge/src/extract/mod.rs` | Direct extraction worker route and metadata conversion. |
| `crates/duduclaw-parse-worker/Cargo.toml` | Client/engine features; locked parser and syscall dependencies. |
| `crates/duduclaw-parse-worker/README.md` | Deployment inputs, startup and launcher seams. |
| `crates/duduclaw-parse-worker/src/adapter_engine.rs` | Structured parser adapters and policy/metadata tests. |
| `crates/duduclaw-parse-worker/src/adapters.rs` | Structured knowledge/table/Office policy client APIs. |
| `crates/duduclaw-parse-worker/src/archive_adapter.rs` | ZIP expansion manifest/output and shared filename decoding. |
| `crates/duduclaw-parse-worker/src/client.rs` | Admission, required latch, launcher/session seam and bounded calls. |
| `crates/duduclaw-parse-worker/src/engine_knowledge/mod.rs` | Engine-only facade over shared knowledge format-reader source. |
| `crates/duduclaw-parse-worker/src/lib.rs` | Public adapters and engine-only module boundaries. |
| `crates/duduclaw-parse-worker/src/main.rs` | Disposable fd service; sandbox, dispatch and drop-before-reply. |
| `crates/duduclaw-parse-worker/src/media_adapters.rs` | Images with output fd, HTML/CSS/search adapters and tests. |
| `crates/duduclaw-parse-worker/src/parsers.rs` | Bounded basic parser dispatch and image inspection. |
| `crates/duduclaw-parse-worker/src/parsers/archive.rs` | ZIP expansion/CRC/name/depth safety checks. |
| `crates/duduclaw-parse-worker/src/parsers/document.rs` | Bounded basic document and spreadsheet extraction. |
| `crates/duduclaw-parse-worker/src/parsers/tests.rs` | Malicious and valid basic parser fixtures. |
| `crates/duduclaw-parse-worker/src/policy.rs` | Narrower implementation limits. |
| `crates/duduclaw-parse-worker/src/response.rs` | Bound JSON serialization and closed error replies. |
| `crates/duduclaw-parse-worker/src/sandbox.rs` | Dynamic uid check, rlimits, seccomp and root resource tests. |
| `crates/duduclaw-parse-worker/src/transport.rs` | SCM_RIGHTS, seals, fd verification and output hash/readback. |
| `crates/duduclaw-parse-worker/systemd/duduclaw-parse.socket` | Fixed root socket, buffers and connection limits. |
| `crates/duduclaw-parse-worker/systemd/duduclaw-parse@.service` | DynamicUser/network/filesystem/syscall/resource sandbox. |
| `crates/duduclaw-parse-worker/tests/systemd.rs` | Six ignored real-systemd tests; one further ignored resource test is in sandbox.rs. |
| `docs/README.md` | Architecture note index. |
| `docs/architecture/native-linux-isolation.md` | Internal Package E implementation, seams, tests and handoff. |
| `docs/features/05-security-defense.md` | English unreleased behavior and acceptance boundaries. |
| `docs/features/ja-JP/05-security-defense.md` | Japanese behavior and acceptance boundaries. |
| `docs/features/zh-TW/05-security-defense.md` | Traditional Chinese behavior and acceptance boundaries. |
| `wiki/impl/native-linux-isolation-package-e-20261105.md` | Removed; relevant historical evidence and handoff moved into this Package E section. |
This is an implementation handoff, not evidence that protected employee isolation
or a production update is ready. Each implementation package owns its section.

## Package F

Status: **Round 2 implementation present; integration and native acceptance pending.**
The production Linux activator now switches `current`, stops/restarts fixed units,
checks genuine live readbacks, and advances the floor only after health. It uses
real socket/systemd adapters, not `UnavailableActivator`. Missing A/B/C/G seam
endpoints fail closed before publication. No customer service was enabled, no
commit was made, and no gateway/CLI/workspace build ran in this worktree.
Provisioning and cross-package wiring remain integration work; this source must
not replace an existing v1.79.3 installation by itself.

### Interfaces and trust

`duduclaw-updated` is a workspace library and executable. All OS adapters are
behind `cfg(target_os = "linux")`; the portable verifier builds on macOS and
Windows. No `duduclaw-isolation` schema, canonical capability bytes or hard limit
was changed. Update frames are the existing `UpdateRequest`/`UpdateResponse`
enums, encoded as JSON behind the shared four-byte length prefix, one seqpacket
per request. The endpoint is the contract's fixed `UPDATED_SOCKET`.

The server authenticates numeric UID/GID with `SO_PEERCRED`, accepts only the
registered gateway account or root (operator/nightly requester), and accepts no
ancillary data. Zero ancillary receive space causes Linux to close discarded
SCM_RIGHTS descriptors; `MSG_CTRUNC` and `MSG_TRUNC` reject the packet. Requests
have a five-second receive/send timeout and a 16-connection backlog. A single
worker performs updates while the server answers status. The latest 32 request
records are durable; an evicted or unknown ID is an unknown outcome, never proof
of failed installation. Apply requires an exact stable version; the gateway
chooses it from its recent update check and never forwards an asset URL.

The dashboard Apply and Status dispatch entries require Admin. The UI preserves
its request ID when the server response is lost, polls across gateway restarts,
and reports success only for `committed`. The CLI now requires `--version` and
submits the same RPC, retaining the existing AI-session operator guard. The
nightly timer retains its schedule but invokes the frozen launcher with
`--check-latest`. The unsigned GitHub index only selects a candidate: strict JSON,
`draft=false`, `prerelease=false`, stable tag, one MiB response ceiling. Missing
service/trust/state refuses; there is no legacy privileged install fallback.

Metadata encoding is compact UTF-8 JSON with lexicographically sorted object
keys, integer numbers, original array ordering, and no trailing newline.
`decode_canonical` round-trips the typed value to reject duplicate/unknown fields,
whitespace, alternate number/escape encodings, and ambiguity before signature use.
The metadata limit is 256 KiB. This is package F's signed-file representation;
it does not alter the shared wire contract.

The SSHSIG verifier follows upstream [PROTOCOL.sshsig](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.sshsig)
and [PROTOCOL.u2f](https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.u2f).
It requires version 1, SHA-512, empty reserved bytes, fixed role namespace, exact
public blob and algorithm, canonical base64, no trailing envelope bytes, and
strict Ed25519 verification. SK Ed25519 requires the role-specific application
and UP+UV flags; unsupported extension flags fail closed. The authenticated
counter is returned for diagnostics, not used as a monotonic authorization.
Root trust requires three distinct registered keys, at least one SK key, and two
distinct signatures. Rotation checks both old and new thresholds, adjacent root
version, expiry and permanent revocations. The target key is separate and must
be hardware-shaped. Synthetic SK vectors are parser/crypto evidence only, not
hardware evidence. SK ECDSA is unsupported and rejected.

The release signer is `scripts/sign-isolation-manifest.py`. It takes a manifest
template plus raw files named by artifact ID, recomputes size/SHA-256 through an
opened regular-file fd, and asks `ssh-keygen -Y sign` to use the fixed namespace.
Keys stay external. Outputs are `manifest.json` and
`manifest.json.signatures.json` (exactly one armor string for a release; a root
may have multiple). Publish them as `isolated-linux-<arch>-manifest.json` and
`isolated-linux-<arch>-manifest.json.signatures.json`; artifact assets are named
`isolated-linux-<arch>-<artifact-id>`, under the official `vX.Y.Z` release. The
plain Ed25519 fixtures were generated with system OpenSSH 10.5p1; no private key
is retained. Real FIDO2 fixture instructions are in the fixture README.

### Storage and process boundaries

All root anchors and their ancestors must be root-owned, without group/other
write access. Child operations use directory fds and `openat2` with BENEATH,
NO_SYMLINKS, NO_MAGICLINKS and NO_XDEV. A regular file must have one link, root
ownership, and no group/other write bits. Atomic snapshots use a new file,
file fsync, rename, then directory fsync. A root-owned flock serializes processes.
A persistence failure poisons the writer permanently, including after rename
may have landed. Subsequent status/error handling cannot save an older snapshot;
only a new process may read back the disk and recover. A regression test injects
a post-rename error and confirms the landed floor is never overwritten.

The downloader is `/usr/bin/curl`, executed only after dropping all groups and
all real/effective/saved IDs to `duduclaw-dl`, clearing the environment, setting
non-dumpable and NoNewPrivileges. It receives a fixed official HTTPS URL and no
credentials. Redirects stay HTTPS and are capped at three; transfers are bounded
by 305 seconds. Bytes stream through a pipe into a root-created staging inode;
the downloader never owns or opens an installation file. Hash and size checks
occur during copying and again on the resulting root-owned fd. Per-artifact
limit is 2 GiB; a generation totals at most 8 GiB. Downloaded archives are not
extracted. Artifact destinations come solely from `InstallTarget`.

The current accepted generation requires gateway, spawnd, trampoline, egress,
forwarder, MCP bridge, parser, importer and updater, all with protocol 1. Wrong
product/architecture/minimum version, incomplete generations, unknown protocols,
expired/future manifests and non-increasing versions/serials are rejected.
Runtime-entry publication and forward data migrations use the typed
`GenerationPreparation` seam; a missing/refusing endpoint blocks staging.

Root layout to be provisioned by G after bootstrap authentication:

| Location | Content |
| --- | --- |
| `/usr/libexec/duduclaw-updated-launch` | Frozen launcher binary; only bootstrap replaces it |
| `/etc/duduclaw-updated/trust.json` | `TrustBundle`: canonical root string, signatures, independently pinned digest |
| `/etc/duduclaw-updated/slot-{a,b}.json` | Accepted signed manifest, signature and acceptance timestamp |
| `/var/lib/duduclaw-updated/state.json` | `State`: floor, serial, digest, high-watermark, pending journal, selector, bounded request records |
| `/var/lib/duduclaw-updated/trust-ledger.json` | Signed root history, accepted releases, emergency intents and outcomes |
| `/opt/duduclaw-isolated/current` | Atomic relative symlink to one verified generation |
| `/var/lib/duduclaw-updated/update.lock` | Lifetime flock |
| `/opt/duduclaw-isolated/generations/<sha256>` | Immutable complete verified generation |
| `/opt/duduclaw-isolated/slots/{a,b}/duduclaw-updated` | Root updater A/B executable inodes |
| `/opt/duduclaw-isolated/pending-generation` | Bounded flat scratch stage, safely removed and recreated after interruption |

Provisioning must supply root-owned directories and initial state, gateway and
downloader system accounts, and the systemd unit. The executable refuses to
invent an initial floor or trust root. `verify_bootstrap_root` compares a compiled
`DUDUCLAW_BOOTSTRAP_ROOT_SHA256` against canonical bytes and verifies two root
signatures. An absent compiled pin refuses. G must still implement the independent
website/console hash comparison and both migration-package signatures before
provisioning; this function alone is not bootstrap acceptance. Never use curl
piped to a shell. No production trust material is included in this package.

### Durability and A/B recovery

`State` stores the committed version/serial/digest together in one atomic
snapshot, avoiding a split commit across a selector and a version-floor file.
A separate serial high-watermark survives abandoned trial boots, so a failed
serial cannot later authorize different bytes. Only the same pending digest can
retry its serial. Tests enumerate version, serial, expiry, architecture, product,
protocol and complete-generation failures.

`stage_inactive_slot` copies the verified generation's updater into a new inode
in the inactive slot, fsyncs the file, slot and parent directories, writes its
receipt, re-verifies it, then persists the trial selector. Repeating a pending or
already healthy trial returns that same slot; it never overwrites the known-good
fallback. The launcher persists `attempts=1` before `execveat` on the very fd it
hashed. A failed first root run causes the next launch to select the verified
previous slot, leave the committed floor unchanged, and retire an unactivated
pending attempt. A higher fixed release may then be accepted. Missing valid
active/fallback slots durably enter maintenance; the launcher does not guess a
stale other slot. Startup verifies its executable inode against the selected
signed slot, trust and journal access, accounts, and socket bind/listen before
marking a trial healthy. Slot receipt freshness is evaluated at acceptance time;
current root validity and revocations still apply. An update freeze does not
revoke an already accepted slot; new release admission still refuses it.

`GenerationActivator::activate_and_read_back` is the explicit A/B/G seam. It
must drain employee runs, publish a single current-generation selector, restart
fixed units, and compare genuine non-root product/version/build/protocol and
service-readiness results against the signed generation. It must coordinate
`stage_inactive_slot`, service restart and continuation after updater self-update.
Only a matching healthy updater slot can advance the journal to activated and
commit. `LinuxGenerationActivator` is now the daemon default; the unavailable
implementation remains only a refusal test fixture.

### Round 2 transaction and recovery

The durable order is request admission → download and hash → immutable generation
rename/fsync → trust acceptance receipt → `Prepared` → inactive updater slot
copy/receipt/fsync → trial selector → process exit 75 → systemd restart through
the frozen launcher → durable attempt → root self-test → healthy slot → drain
employee admission → stop the gateway and verify MainPID is zero → G migration /
B runtime publication → single `current` symlink rename/fsync → fixed unit
restarts → live health and executable readback → `Activated` → atomic floor,
serial, digest and `resume_required` commit → emergency-history completion if
needed → fresh health and admission resume → final public `Committed`.

`State::status` and idempotent Apply replay hide an internal committed record
while resume is outstanding. Failed/lost Resume stays visibly activated or
maintenance; a restart revalidates health and retries the same digest. The floor
is not reverted after a landed commit. Each clean daemon boot closes admission
and verifies the actual running generation, even when no pending update exists.
Unexpected/missing `current`, unreadable artifacts or mixed live versions keep
admission closed. `current` alone is not readiness evidence.

Queued/downloading requests resume automatically. A partial root scratch stage
is unlinked through its trusted dirfd and recreated; deletion is bounded and
rejects links, mounts, directories and special files. A crash after publishing
the immutable directory but before preparing state reuses and rehashes it.
Prepared/activated requests resume with their original digest, serial and
acceptance time. An elapsed release expiry does not invalidate a durably
accepted transaction; current root expiry and revocations still apply. A failed
first root trial retires its pending request, preserves the previous generation,
and does not lower the serial high-watermark. Unknown persistence outcomes poison
the writer so stale in-memory state cannot overwrite a possibly landed rename.

Before downloading, bounded GC keeps the effective and committed generations,
the candidate, and both slot-receipt generations. All other flat generation
directories are removed safely; interrupted GC is reentrant. Allocation checks
require the artifact total plus one maximum updater slot and 512 MiB free, and
at least 256 available inodes. Existing generations are never overwritten.

`Directory` root tests inject failures after temporary cleanup/create/write,
file fsync, rename, destination fsync and source fsync, then reopen the actual
snapshot. Portable tests inject failure after every production activation effect
and every normal/emergency durable state transition. These simulate process
interruptions; they are not physical power-loss evidence.

### Cross-package integration contract

The existing isolation update RPC is unchanged. The following internal F-owned
seam types are in `activation_contract.rs`; they are not exposed to employees or
the dashboard. Their exact wire representation is serde JSON in the existing
bounded four-byte-length seqpacket frame, with no ancillary descriptors.
Every request/response binds schema version 1, a fresh random nonce and an exact
manifest digest. Unknown fields, extra/missing components and refusal responses
fail closed. Adapters enforce bounded I/O/retry deadlines.

| Owner | Seam / obligation |
| --- | --- |
| B1 | `ActivationRuntime` on `/run/duduclaw-spawnd/update.sock`, root peer only. `Drain` durably closes admission and proves zero active runs. The barrier survives spawnd restart and is keyed by digest. `Resume` is idempotent after a lost reply; never implicitly resume on disconnect/restart. |
| B1/G | `GenerationPreparation::{preflight,apply}`: typed digest-bound migrations and runtime artifacts. Preflight makes no data changes. Apply is idempotent and recovers any interrupted publication/migration; updater first drains and stops the gateway. Missing reverse-migration support causes emergency rollback across **any** migration to be refused. |
| A | Live gateway readback at `/run/duduclaw-gateway-update/readback.sock`, peer UID/GID from the provisioned `duduclaw` account; exactly one gateway component. |
| C | Live egress readback at `/run/duduclaw-egress-update/readback.sock`, peer UID/GID from `duduclaw-egress`; exactly one egress component. |
| B1/B2/D/E/G | B1 supplies actual isolated readiness results for non-daemon artifacts. It cannot stand in for gateway/egress endpoints. Report product, version, protocol, actual loaded executable SHA-256, effective UID and readiness for every signed artifact except the updater slot, which has its independent root self-test. Root execution is allowed only for spawnd/trampoline. |
| G | Provision independent bootstrap pin/dual migration signatures, accounts, initial state/receipts, frozen launcher, slots, units, `current`, and trusted anchors. Keep install/generations traversable for service UIDs. Non-root readback daemons bind their own socket in a dedicated service-owned leaf under root-owned `/run`; verify root client credentials. |

`LinuxGenerationActivator` checks `current` is the committed/effective or pending
digest, verifies every artifact's bytes and signed mode, publishes the directory
0755 and `current` atomically, then restarts only `duduclaw-egress.service`,
`duduclaw-spawnd.service`, `duduclaw-gateway.service`. Before restarting, it checks
unit User/Group and non-root NoNewPrivileges settings. The live services must be
active, keep the same MainPID across authenticated readback, and `/proc/PID/exe`
must hash to the signed artifact both before and after that readback. The PID is
an observation, never an authorization principal. The manifest has no separate
build-string field; `build_sha256` is the exact signed executable identity.

The updater unit needs `CAP_SYS_PTRACE` to read non-dumpable service executables
and `CAP_DAC_OVERRIDE` to reach private service-owned health sockets, in addition
to its existing uid/gid/chown capabilities. It has ordering after the three fixed
units without a Requires dependency that would stop the updater during their
restart. Root VM tests cover the restricted capability set, a non-dumpable
non-root target and a mode-0600 seqpacket socket.

### Root trust administration and emergency rollback

`trust-ledger.json` preserves the bootstrap pin, adjacent dual-threshold root
rotations and accepted historical target identities. A root rotation also
persists freeze and permanent revocation. Historical installed generations remain
verifiable after an old root expires, but current root expiry/revocation still
refuses them. Histories are bounded: 32 roots, 256 accepted releases, 32 emergency
outcomes; exhaustion refuses rather than dropping security history. Atomic file
size is additionally limited to 1 MiB. No production signing keys are present.

A root operator stops the updater before using these fixed-inbox commands:

```sh
# CONFIG_ROOT/root-update.json: {"document":"<canonical signed root JSON>","signatures":["<armor>","<armor>"]}
/usr/libexec/duduclaw-updated-launch --rotate-root
# CONFIG_ROOT/emergency.json: same envelope, root-threshold emergency namespace
/usr/libexec/duduclaw-updated-launch --emergency
# Preparation only; start the service to continue the verified A/B transaction.
systemctl start duduclaw-updated.service
```

The root commands share the daemon's nonblocking update lock and cannot run
concurrently with an update. `--emergency` authorizes only an already accepted,
exact target digest, same product/architecture and an older version. It consumes
the emergency serial in trust state **before** writing the pending update state;
startup reconciles either snapshot boundary. The signed authorization and its
acceptance time are preserved. Expiry limits initial admission; the identical
accepted transaction can resume after that time, without accepting a new digest
or authorization. Current root checks remain live. A failed trial is recorded as
a consumed failed emergency, never replayed. A successful emergency changes the
effective generation and slot, while retaining the ordinary high version floor,
serial and highest attempted serial. Subsequent ordinary updates must exceed the
high floor **and** satisfy minimum-version against the actual running version.

### Remaining external gates and explicit refusals

- A/B/C/D/E/G must wire their named seams and G must provision the independently
  authenticated migration/bootstrap package. F neither invents readiness nor
  bypasses a missing endpoint. The shared isolation schemas were not changed.
- Pro's isolation asset publication URL/naming/authentication contract is not
  supplied by this checkout; its downloader route remains explicitly refused.
  The ordinary Pro/minisign pipeline is not a substitute for an isolation feed.
- Emergency rollback across any migration remains refused until G supplies a
  verified reverse plan. Forward migration/runtime publication uses the complete
  typed seam above; missing implementations refuse.
- Generate real hardware SK fixtures and verify GoTrust EdDSA. SK ECDSA remains
  unsupported/fail closed if the hardware requires it. Synthetic SK tests do not
  establish hardware compatibility.
- Run the ignored native Linux/systemd tests on Ubuntu 22.04/24.04 and Debian 12,
  then the integrated full update, reboot/power-cut, bootstrap and dashboard
  roundtrip. macOS tests and Linux cross-checks do not establish those outcomes.
- Remote gateway/CLI builds, web checks with dependencies installed, and the
  independent `gpt-daybreak-blue-latest` review remain required. Ordinary code
  review was performed; no Daybreak model was callable in this session.

### Validation commands

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline
python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v
python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v
# Root VM, systemd as PID 1, python3 and provisioned duduclaw-dl account:
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -- --ignored iso_root_ --test-threads=1
# Assigned remote builder only:
cargo test -p duduclaw-gateway protected_update_
cargo test -p duduclaw-gateway restart_pending_tests
cargo test -p duduclaw-cli service_protected_
# Real FIDO2 fixtures must first be created per tests/fixtures/README.md:
cargo test -p duduclaw-updated real_fido2_sk_ed25519_openssh_vector -- --ignored
# Only with web/node_modules present:
cd web
npx tsc -b
npx vitest run src/lib/protected-update.test.ts src/components/settings/sections/UpdateTab.test.tsx
```

### Round 2 local results and file inventory

| Check actually run | Result |
| --- | --- |
| `cargo test -p duduclaw-updated -p duduclaw-isolation --offline` with jobs 4, incremental 0 | 63 passed: 34 updater + 29 unchanged contracts; 0 failed; 1 ignored real-FIDO2 test |
| `cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline` with jobs 4, incremental 0 | Passed, including compilation of 11 ignored Linux root tests; 30 expected unsafe-boundary warnings in test build |
| `cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline` with jobs 4, incremental 0 | Passed; no native Windows execution |
| Python signer suite | 4 passed, including actual OpenSSH sign/verify |
| Python nightly helper suite | 12 passed, 1 skipped because systemd-analyze is absent on macOS |
| `git diff --check` | Passed |
| Web dependency check | `web/node_modules` absent; tsc/vitest not run |
| Native Linux / systemd / real FIDO2 / physical power cut | Not run on this macOS host |
| Independent ordinary code review | Identified startup/readback capability, cold-boot race, premature public success and emergency recovery issues; corrected and re-reviewed without remaining identified high/critical correctness blockers. This is not Daybreak acceptance. |

No external dependency, shared contract, gateway/CLI/web source or Cargo manifest
was changed in round 2. Their round 1 edits remain uncommitted. No `cargo fmt`,
commit, release, deployment or customer-machine operation was performed. This
session did not create a worktree, so the assigned worktree is retained for the
integrator.

| Round 2 file | Change |
| --- | --- |
| `crates/duduclaw-updated/src/activation_contract.rs` | New typed drain/preparation/readback seams, live identity validation, production orchestration crash matrix |
| `crates/duduclaw-updated/src/linux/activation.rs` | New real current publication, fixed-unit restart, authenticated health and executable readbacks, 3 root tests |
| `crates/duduclaw-updated/src/trust_store.rs` | New atomic trust history, rotation/freeze/revocation, emergency authorization and recovery tests |
| `crates/duduclaw-updated/src/linux/trust_admin.rs` | New root-only fixed-inbox administration and 2 root persistence tests |
| `crates/duduclaw-updated/src/linux/emergency.rs` | New exact-digest emergency preparation, cross-snapshot reconciliation and root commit-boundary test |
| `crates/duduclaw-updated/src/state.rs` | Accepted-time retries, actual emergency running version, monotonic floor, public Resume gate and crash tests |
| `crates/duduclaw-updated/src/linux/service.rs` | Connect real activator, self-restart continuation, all-boot health, scratch cleanup and generation GC |
| `crates/duduclaw-updated/src/linux/launcher.rs` | Revalidate historical accepted slots, resume trial staging, authenticated emergency fallback |
| `crates/duduclaw-updated/src/linux/files.rs` | Bounded safe directory cleanup/free-space checks, real filesystem persistence failpoints and same-fd exec tests |
| `crates/duduclaw-updated/src/linux/download.rs` | Explicit real/effective/saved uid/gid readback and downloader root acceptance test |
| `crates/duduclaw-updated/src/linux/mod.rs` | Export Linux activation, trust and emergency adapters |
| `crates/duduclaw-updated/src/lib.rs` | Export portable seam and trust ledger |
| `crates/duduclaw-updated/src/main.rs` | Root-only `--rotate-root` and `--emergency` commands |
| `crates/duduclaw-updated/README.md` | Document activation prerequisites and root test entry point |
| `scripts/linux-protected/updated/duduclaw-updated.service` | Fixed-service ordering and capabilities needed for authentic live readback |
| `scripts/linux-protected/auto-upgrade/README.md` | Update integration boundary description |
| `CHANGELOG.md` | User-facing round 2 behavior and remaining rollout gates |
| `docs/architecture/native-linux-isolation.md` | Current seams, recovery, commands, results, and archived round 1 handoff |
| `docs/features/05-security-defense.md` | English update behavior and acceptance boundary |
| `docs/features/zh-TW/05-security-defense.md` | Traditional Chinese update behavior and acceptance boundary |
| `docs/features/ja-JP/05-security-defense.md` | Japanese update behavior and acceptance boundary |
| `docs/guides/linux-protected-service.md` | English activation/recovery description |
| `docs/guides/zh-TW/linux-protected-service.md` | Traditional Chinese activation/recovery description |
| `docs/guides/ja-JP/linux-protected-service.md` | Japanese activation/recovery description |
| `wiki/impl/native-linux-isolation-package-f-2026-11-05.md` | Removed after moving original evidence below; no Package F security-gap handoff remains in wiki |

### Archived round 1 handoff (superseded by round 2)

The following original evidence was moved out of the published wiki. Its remaining-work list and counts describe round 1 only.

<details>
<summary>Round 1 evidence</summary>

### Package F implementation handoff — 2026-11-05

Status: **PARTIAL**. Work is confined to the assigned existing worktree and branch
`feat/iso-f-20261105`. No commit, branch switch, merge, release, deployment or
customer-machine action was performed. The new small crate compiles; the full
package is not ready to enable. Gateway/CLI/web compilation remains unverified.

#### Implemented

- Strict canonical metadata and Rust OpenSSH sshsig Ed25519/SK-Ed25519 verification,
  root 2/3 distinct-key thresholds, dual-threshold rotation verification,
  role/application/UP+UV rules, expiry/freeze/revocation and emergency verification.
- Official raw-artifact download under duduclaw-dl; root-owned bounded staging,
  openat2/dirfd checks, same-fd hash/exec, durable state, bounded RPC history,
  monotonic committed floor plus failed-attempt serial high-watermark.
- A/B selector/first-boot rollback state machine, frozen launcher, inactive-slot
  installation helper, and first real root-run self-tests before healthy marking.
- Dashboard Admin Apply/Status, retained request IDs on ambiguous results,
  localized progress and committed-only success; protected CLI requester and
  replacement nightly requester.
- Release manifest signer, real OpenSSH software-key fixtures (no private keys),
  unit tests, Linux privileged test entry point, and documentation.

#### Required remaining implementation

1. Replace `UnavailableActivator` with genuine drain/current-generation switch,
   fixed-unit restart and live product/version/build/protocol readbacks. Wire
   `stage_inactive_slot` and continuation after updater self-restart. Today the
   daemon verifies/stages a release, then reports Maintenance without switching
   current or committing the floor.
2. G provisioning: independently verified bootstrap program/package, BOTH migration
   signatures, pinned trust, initial state/slots/accounts and unit installation.
   `verify_bootstrap_root` only handles the compiled root fingerprint + threshold;
   no production root fingerprint or keys are provided.
3. Automatic recovery/cleanup for interrupted root `pending-generation` staging,
   prepared transactions, disk budgets and generation garbage collection. Current
   partial staging is retained and requires root inspection; it is not a completed
   crash-resume implementation for every filesystem point.
4. Root rotation/freeze/revocation persistence and feed/commands, accepted historic
   key material across target rotation; emergency installer with persisted serial
   and signed history. Current crypto APIs only verify these documents.
5. Runtime-entry publication, required data migrations and Pro release feed:
   current implementation explicitly refuses them.
6. Real FIDO2 fixtures and hardware verification; SK ECDSA support if needed by
   GoTrust. SK Ed25519 currently has synthetic crypto vectors only.
7. Linux root/systemd acceptance, all three supported distributions, actual
   downloader IDs and descriptor lifetime, reboot/power-cut A/B and update lifecycle.
8. Remote gateway/CLI/web builds and tests, then independent Daybreak review.
   Ordinary independent code review was performed, but no Daybreak model is
   available in this session; it is not claimed as security acceptance.

The exact interfaces, root filesystem layout and limits are recorded in
[the Package F architecture note](#package-f).

#### Commands actually run

| Command | Actual result |
| --- | --- |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated -p duduclaw-isolation --offline` | 49 passed (20 updater + 29 unchanged contract), 0 failed, 1 ignored real FIDO2 test; doc/bin suites 0 tests |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --offline` | Passed |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --tests --offline` | Passed after correcting one test-only Path::set_permissions compile error; 13 expected unsafe-boundary warnings |
| `CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-pc-windows-gnu -p duduclaw-updated --offline` | Passed; no linker or Windows native execution |
| `python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v` | 4 passed; includes actual ssh-keygen sign and verify |
| `python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v` | 12 passed, 1 skipped because systemd-analyze is unavailable on this Mac |
| `git diff --check` | Passed |
| Python JSON parse of `web/src/i18n/{zh-TW,en,ja-JP}.json` | 3 passed |
| Scan of new crate for `BEGIN .*PRIVATE KEY` | No matches; fixtures contain public keys and signatures only |

Earlier focused updater test runs also passed (11, then 19 active tests as state
coverage was added). Final totals above supersede those intermediate runs.
No cargo fmt, gateway/CLI/workspace build, dependency installation, native Linux
execution or web dependency installation was performed. `web/node_modules` is
absent, so the specifically authorized conditional tsc/vitest checks were skipped.

#### Builder commands and filters still required

```sh
### Linux: includes non-root seqpacket/artifact adapter tests that macOS cannot run.
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated
### Root-capable Linux: root dirfd/permissions/atomic replacement.
cargo test -p duduclaw-updated -- --ignored iso_root_
### Real hardware, only after creating the documented fixtures:
cargo test -p duduclaw-updated real_fido2_sk_ed25519_openssh_vector -- --ignored
cargo test -p duduclaw-gateway protected_update_
cargo test -p duduclaw-gateway restart_pending_tests
cargo test -p duduclaw-cli service_protected_
cd web
npx tsc -b
npx vitest run src/lib/protected-update.test.ts src/components/settings/sections/UpdateTab.test.tsx
```

The root test presently covers filesystem trust/atomic replacement only; the full
A/B reboot, downloader uid and native systemd integration harness is still needed.

#### CONTRACT CHANGE

None. No shared contract source was changed; all 29 contract tests remain green.
Package F defines the previously unspecified signed-file JSON representation in
its own metadata module. New crate dependencies were already locked packages:
serde, serde_json, thiserror, base64, sha2, ed25519-dalek, hex, nix, libc, tempfile.
Cargo.lock adds only the workspace crate and dependency edges, no external version
upgrade or new external package.

#### Review findings addressed

Independent readers reviewed actual code. Fixes include matching signer/server
signature-array formats; preserving unknown-outcome tracking; delaying healthy
until account/listener readiness; persisting maintenance even when boot refuses;
retaining failed serial high-watermark; idempotent healthy-slot staging; separating
update freeze from installed-slot authenticity; and poisoning all persistence
writes after an uncertain post-rename failure. A regression proves that even when
the new floor landed before fsync failed, later error handling cannot overwrite it
with the old in-memory floor. Policy refusals do not poison the writer.

#### Files changed or added

- `CHANGELOG.md` — Add a self-contained zh-TW partial implementation entry.
- `Cargo.lock` — Record the new local crate and dependency edges.
- `Cargo.toml` — Register the updater workspace member and dependency.
- `crates/duduclaw-cli/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-cli/src/service/protected_linux_install.rs` — Replace protected direct install with version-only daemon request.
- `crates/duduclaw-cli/src/service/protected_linux_tests.rs` — Stable-version and operator guard regression tests.
- `crates/duduclaw-gateway/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-gateway/src/handlers/dispatch_system.rs` — Admin authorization for protected update status.
- `crates/duduclaw-gateway/src/handlers/mod.rs` — Register the protected update handler module.
- `crates/duduclaw-gateway/src/handlers/protected_update_rpc.rs` — Admin dashboard updater request/status bridge and regression tests.
- `crates/duduclaw-gateway/src/handlers/system_config_rpc.rs` — Route protected apply requests to the root service.
- `crates/duduclaw-gateway/src/updater.rs` — Expose protected update channel and operator explanation.
- `crates/duduclaw-updated/Cargo.toml` — Declare internal updater dependencies (new crate reuses already locked external packages).
- `crates/duduclaw-updated/README.md` — Document purpose, usage constraints and validation requirements.
- `crates/duduclaw-updated/src/client.rs` — Fixed authenticated socket client and request-id binding.
- `crates/duduclaw-updated/src/lib.rs` — Portable module API, strict version parser and safe error codes.
- `crates/duduclaw-updated/src/linux/download.rs` — Fixed official HTTPS assets downloaded after uid/group drop.
- `crates/duduclaw-updated/src/linux/files.rs` — Root dirfd storage, atomic snapshots, fd hashing and root tests.
- `crates/duduclaw-updated/src/linux/launcher.rs` — Same-fd slot exec, trial persistence, staging and bootstrap pin helper.
- `crates/duduclaw-updated/src/linux/mod.rs` — Linux adapter exports and root process hardening.
- `crates/duduclaw-updated/src/linux/service.rs` — Authenticated root daemon, signed staging and unavailable activator seam.
- `crates/duduclaw-updated/src/linux/transport.rs` — Bounded seqpacket, credentials, ancillary rejection and tests.
- `crates/duduclaw-updated/src/main.rs` — Linux launcher/service/nightly entry points; other platforms refuse.
- `crates/duduclaw-updated/src/metadata.rs` — Canonical signed JSON encoding and ambiguity rejection.
- `crates/duduclaw-updated/src/sshsig.rs` — Strict SSHSIG Ed25519 and SK Ed25519 verification and vectors.
- `crates/duduclaw-updated/src/state.rs` — Durable floor/journal/slot/request state and failure matrix tests.
- `crates/duduclaw-updated/src/trust.rs` — Root threshold/rotation, release/installed-slot and emergency verification.
- `crates/duduclaw-updated/tests/fixtures/README.md` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-release.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-root.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1-wrong.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-1.pub` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-release.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-root.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2-wrong.sig` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/ed25519-2.pub` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `crates/duduclaw-updated/tests/fixtures/message.json` — Public OpenSSH fixture or hardware fixture reproduction instructions; no private key.
- `docs/README.md` — Index the new architecture handoff.
- `docs/architecture/native-linux-isolation.md` — Package F interfaces, semantics, boundaries and remaining work.
- `docs/features/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/features/ja-JP/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/features/zh-TW/05-security-defense.md` — Describe actual protected update routing without claiming isolation or delivery acceptance.
- `docs/guides/ja-JP/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `docs/guides/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `docs/guides/zh-TW/linux-protected-service.md` — Document new requester and development-only activation boundary; preserve legacy guidance.
- `scripts/linux-protected/auto-upgrade/README.md` — Document purpose, usage constraints and validation requirements.
- `scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade` — Replace legacy upgrade/download shell with fixed launcher requester.
- `scripts/linux-protected/auto-upgrade/duduclaw-auto-upgrade.service` — Remove legacy write paths; bound requester service.
- `scripts/linux-protected/updated/duduclaw-updated.service` — Development-only root daemon unit template.
- `scripts/sign-isolation-manifest.py` — Canonical manifest creation and offline ssh-keygen signing.
- `scripts/tests/test_protected_auto_upgrade.py` — Requester and source-trust installer regression tests.
- `scripts/tests/test_sign_isolation_manifest.py` — Real OpenSSH signing-tool tests.
- `web/src/components/settings/sections/UpdateTab.tsx` — Request tracking, polling, terminal states and safe retry UX.
- `web/src/i18n/en.json` — Localized protected update progress/result copy.
- `web/src/i18n/ja-JP.json` — Localized protected update progress/result copy.
- `web/src/i18n/zh-TW.json` — Localized protected update progress/result copy.
- `web/src/lib/api.ts` — Type protected apply/status RPC responses.
- `web/src/lib/protected-update.test.ts` — Regression coverage for states and lost replies.
- `web/src/lib/protected-update.ts` — Portable request tracking and ambiguous-outcome handling.
- `wiki/impl/native-linux-isolation-package-f-2026-11-05.md` — This evidence and remaining-work report.

</details>
This is an internal implementation record for design r7. Packages are delivered
separately and do not establish protected-mode readiness by merely compiling.

## Package H

`crates/duduclaw-isolation-storage` implements root-side, per-employee fixed-size
ext4 loop storage. It is a workspace library for package B1, not a gateway feature
flag, installer, daemon, mount namespace builder, or project-quota backend. No
existing installation is switched over by this change. Shared contract wire
formats, canonical bytes and hard limits are unchanged.

### B1 integration

1. Installation creates an empty root:root 0700 storage directory with root-owned,
   non-writable ancestors on ext4/XFS. B1 makes the relevant mount tree private.
2. `Storage::open(path)` verifies the anchor, takes a lifetime exclusive flock,
   validates bounded metadata and compares the full persisted backing device
   graph. Opening allows maintenance even with low free space/incomplete images.
3. B1 calls `readiness()` at startup, before admission and periodically during
   execution. Failure refuses new runs and B1 supplies the administrator notice.
   H has no background task/thread to monitor free space by itself.
4. After administrator confirmation and UID registry allocation, call
   `provision(EmployeeId, uid, Quota::try_from(&DiskQuota))`. The UID must already
   have passed B1's NSS/exclusions/account-lockdown checks. H currently accepts
   the contract's default 70000–99999 range only.
5. `attach(employee, noexec)` returns `MountAnchor { path, noexec }`. B1 must
   clone only `path/home` and `path/work` with `open_tree`, recursively make each
   detached clone private and set the required mount flags before handing mount
   fds to the trampoline. Image files and loop devices never enter that handover.
6. B1 revokes capabilities, drains the cgroup/UID and releases ALL detached mount
   clones before `resize` or `detach`. `destroy` requires the image to be unmounted and no
   configured loop referencing its inode. Neither method authorizes deletion or
   proves a run has stopped; B1 enforces those policy decisions.

The named B1 integration seam is `EmployeeStorage: Send` (object-safe, root-local,
not serializable), implemented by `Storage` on Linux and by the refusing facade
on other platforms. It covers readiness/provision/attach/detach/destroy/usage/resize.
There is no gateway-uid directory fallback or default-success implementation.
B1 must keep its per-employee admission lock throughout maintenance and must not
hand out clones while resize temporarily mounts the old filesystem.

`usage()` reads kernel `statvfs`, including inode counts, after checking mount
identity and flags. It requires an attached image. Quota is filesystem capacity,
including ext4 overhead, lost+found and the home/work directories; usable employee
space/inodes can therefore be smaller than the requested maximum.

### Admission and backing storage

The charged maximum of every image (Creating, Ready, Resizing, Deleting or Quarantined),
plus a candidate's capacity, plus `max(20 GiB, ceil(host capacity / 10))` must fit
host total capacity. Available host space must independently cover additional
allocation plus that reserve. Overflow, unreadable capacity and low space refuse
admission. Existing images stay charged until deletion and directory fsync have
completed. This is not a `du` polling guarantee.

`fstatfs` accepts only EXT4_SUPER_MAGIC/XFS_SUPER_MAGIC. From the backing filesystem
major:minor, sysfs traversal follows every partition parent and every `slaves`
edge, with bounded depth/node count and cycle/missing-node rejection. Physical
nvme/virtio/scsi/xen disks, md and partitions are recognized. Device mapper invokes
fixed `/usr/sbin/dmsetup table --major N --minor N` and permits only `linear`,
`crypt`, `striped`, `raid`; every dependency is still traversed. Unknown types,
including loop-backed host filesystems, are refused. Only target types and graph
edges are persisted: crypt keys/table arguments are never logged or stored.
Cloud hypervisor allocation is outside this guarantee, as in the design.

Provision reserves metadata before file creation, uses `fallocate` and verifies
`st_blocks * 512 >= size`. Images remain root:root 0600, single-link files resolved
from the anchor with `openat2(BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV)`. Formatting
uses root-owned `/usr/sbin/mkfs.ext4` with fixed argv, fixed 4096-byte blocks,
`-m 0`, `-N <inode ceiling>`, and
`-E lazy_itable_init=0,lazy_journal_init=0,nodiscard`. The image is inherited only
as fd 3; no shell or untrusted executable/config/argv is accepted. Helpers have
empty environment, fixed PATH/locale/cwd, bounded output and timeouts. They are
trusted root-side tools and must be shipped/maintained by the host administrator.
Only existing locked dependencies rustix 1.1.4 and nix 0.29.0 were enabled; no
new dependency versions or heavy dependency trees were added.

The bounded superblock preflight rejects bad magic, error state, excessive inode
rounding or incompatible capacity. `mkfs -N` can round: a result above the caller's
ceiling is refused, leaving a charged Creating record for explicit cleanup. A
dirty non-error image may replay its journal in the kernel. No automatic root
`fsck` runs on employee images.

Attachment checks existing loop devices by kernel backing `(device,inode)` rather
than parsing escaped `backing_file` path text. This also detects path aliases.
`LOOP_CTL_GET_FREE` + `LOOP_CONFIGURE` uses AUTOCLEAR; no SET_FD/SET_STATUS race.
The image flock and manager lock serialize cooperating writers. Host mounts use
`nosuid,nodev,errors=remount-ro` plus `noexec` when selected; mountinfo, loop backing
identity and statvfs/statfs flags/capacity are read back. A failed mount/readback
quarantines the image. Cleanup verifies identity separately so a verified
readonly/quarantined image can still be unmounted. There is no lazy unmount,
recursive deletion, root fsck, or automatic fallback.

### Crash/recovery and limits

The 4 MiB / 4096-image bounded, versioned registry uses exclusive temp create,
write, file fsync, rename and parent-directory fsync. A leftover temp file is
validated before replacement; it is never promoted over committed state. Missing
committed metadata with existing artifacts, unknown files, wrong ownership,
symlinks/hardlinks or replaced image identity refuse automatic reconstruction.
Any failed metadata publication poisons the current handle until it is reopened.
The root registry may preserve Creating/Deleting states after restart; readiness
refuses them and only explicit safe `destroy` releases their charge. Data recovery
is an operator decision. A quarantined entry conservatively blocks readiness for
the manager, including other employees, until cleaned up.

### Resize protocol

`resize` supports monotonic byte growth with a non-decreasing inode ceiling;
identical healthy quotas are an idempotent no-op. Shrink and inode-only changes
are explicitly refused before mutation (safe shrink requires content migration,
not file truncation). Existing host mounts or configured loops refuse growth.
Readiness and admission are rechecked, charging only the additional allocation
against free space but the full target against total capacity. The old verified
filesystem is temporarily mounted noexec in B1's private mount tree. Target quota
and `Resizing` are published durably BEFORE allocation. H uses fallocate, fsync,
`LOOP_SET_CAPACITY`, and `EXT4_IOC_RESIZE_FS`, then syncfs, mount/flags/quota and
bounded superblock readback. Finally it closes maintenance descriptors, unmounts
and publishes `Ready`. It never invokes resize2fs or fsck against employee data.

On any error after reservation the target stays fully charged and the entry is
quarantined; an interrupted process leaves `Resizing`. Both refuse readiness and
attach after reopen. No image truncation, automatic resume, or rollback to the old
charge occurs. If unmount cannot finish, the entry stays `Resizing` and charged.
Cleanup still uses verified image/loop identity and can detach these states.
An operator may recover data separately or explicitly destroy the image after
all clones are gone. Actual inode growth is kernel/ext4-group dependent; a target
inode ceiling that cannot contain the resulting capacity fails readback and
quarantines instead of publishing an over-quota mount.

Project quota is an optional alternative to this loop backend, not implemented.
Installation-specific custom UID ranges cannot be widened beyond the unchanged
shared contract defaults. These are explicit unsupported inputs, not success
stubs. Damaged-data recovery is an operator action; H does not interpret or repair
filesystem content. CPU/memory/PID/IO/PTY enforcement and runtime monitoring /
administrator notices are B1/B2 responsibilities; H supplies synchronous readiness
and kernel-enforced byte/inode ceilings, not a background daemon.

### Validation and remaining acceptance

Round 2 executed on macOS: storage pure/unit tests (16 passed, 0 failed),
unchanged shared contract tests (29 passed). Linux implementation AND its tests
pass cross-target check; Windows implementation/tests pass cross-target check.
Linux check emits six workspace `unsafe_code` warnings for the documented loop /
ext4 ioctls and async-safe pre-exec fd handover. These syscall-boundary warnings
are retained. One initial unit-test run failed because its shrink fixture was
below the minimum valid filesystem size (13 passed, 1 failed); the fixture was
corrected to test a valid shrink, and the full final run passed.

Run on an isolated Linux VM with root, loop devices, e2fsprogs and more than
20 GiB available on supported physical ext4/XFS backing. Prepare a fresh empty
root:root 0700 `ISO_STORAGE_TEST_ROOT` with root-owned, non-writable ancestors.
Never point it at production storage. `unshare` below makes the test mount tree
private without changing propagation in the host namespace. Run tests serially.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-isolation-storage --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --tests --target x86_64-pc-windows-gnu -p duduclaw-isolation-storage --offline
# As root, in a toolchain/build environment permitted to execute cargo:
unshare --mount --propagation private env ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline -- --ignored iso_root_ --test-threads=1
```

Eight privileged tests are ignored by default:

- `iso_root_provision_mount_usage_unmount_destroy`: formatting and real loop mount,
  inode/data ENOSPC without breaking host metadata writes, flags, UID ownership,
  shrink refusal, idempotence, readonly/quarantine detach and destruction.
- `iso_root_metadata_crash_safety`: interrupted write/fsync/rename/directory-fsync
  publication; recovery sees old or new complete metadata.
- `iso_root_incomplete_creation_never_attaches`: exclusive lock, reopen after an
  incomplete reservation, readiness refusal and explicit charged-state cleanup.
- `iso_root_resize_grows_preserves_data_and_rejects_live_mount`: kernel grow,
  live-mount refusal, data retention, readback, unmount, idempotence and reopen.
- `iso_root_resize_interruption_keeps_full_charge_and_refuses_attach`: unwind at
  reservation/allocation/kernel-grow checkpoints skips normal error handling;
  reopen keeps the full target, refuses attach and permits explicit cleanup.
- `iso_root_resize_error_quarantines_full_target`: normal error path, durable
  quarantine and target charge, reopen, refusal and safe cleanup.
- `iso_root_rejects_tmpfs_and_recursive_loop_backing`: actual unsupported tmpfs
  and ext4 on a loop device both refuse opening as host backing storage.
- `iso_root_rejects_image_alias_and_replacement`: direct identity checks reject
  symlink, hardlink and replacement inode (not masked by inventory failure).

These native tests are cross-compiled, NOT executed on Linux. Their interruption
hooks are software fault injection, not physical power-loss tests. Ubuntu 22.04 /
5.15, Debian 12 / 6.1 and Ubuntu 24.04 / 6.8 execution is the upstream VM gate.
Real thin-LV-under-crypt fixtures, live topology replacement, non-cooperating loop
races, physical power-loss and Daybreak security review remain acceptance work.
Pure fixtures cover recursive thin-under-crypt rejection, canonical topology /
target change detection and bounded graph refusal. B1 runtime wiring and monitoring
remain with its integrator against `EmployeeStorage`; H's side is implemented.
No release, live-provider, customer acceptance or active isolation is claimed.

Independent code review of round 2 found no confirmed critical/high blocker; it
caught an alias test initially masked by inventory rejection. The test now calls
the identity check directly with exact errors. The reviewer did not execute Linux
tests or perform the separate formal Daybreak security review. No shared wire
schema, canonical bytes, hard limits or dependencies changed in round 2.
No Package H handoff/security-gap notes were found under wiki/impl or wiki/reports;
all H handoff notes remain in this internal document.

Reference syscall semantics:
[loop(4)](https://www.man7.org/linux/man-pages/man4/loop.4.html),
[dmsetup(8)](https://man7.org/linux/man-pages/man8/dmsetup.8.html),
[Linux 5.15 loop UAPI](https://raw.githubusercontent.com/torvalds/linux/v5.15/include/uapi/linux/loop.h),
[ext4 resize ioctl](https://raw.githubusercontent.com/torvalds/linux/v5.15/fs/ext4/ioctl.c).

## Linux root acceptance record (2026-11-05)

Disposable GCP VMs (asia-east1-b, c2d-standard-4), test binaries built on a
separate Ubuntu 22.04 builder and copied to identical paths. Every `iso_root_`
test ran as root, one process per test, `--test-threads=1`. This record covers
the package-level root tests only; it is not a protected-mode, migration,
live-provider or release acceptance.

| VM | Kernel | systemd | Landlock ABI | Final result |
| --- | --- | --- | --- | --- |
| Ubuntu 22.04 | 5.15.0-1120-gcp (design target) | 249.11 | 1 | 66/66 pass |
| Ubuntu 22.04 | 6.8.0-1070-gcp (GCP image default, HWE) | 249.11 | 4 | 66/66 pass |
| Ubuntu 24.04 | 6.8.0-1071-gcp (design target) | 255.4 | 4 | 66/66 pass |
| Ubuntu 24.04 | 7.0.0-1014-gcp (GCP image default) | 255.4 | 8 | 50/66; all 16 B2 refused with `UnsupportedKernel` |
| Debian 12 | 6.1.0-54-cloud-amd64 | 252.39 | 2 | 66/66 pass |

Per package: B1 spawnd 13, B2 run 16, C egress 8, E parse worker 7, F updated 11,
H storage 11 root tests. C ran with a public operator-controlled TCP echo
fixture and both `example.com` and the CNAME name `www.github.com`.

Defects found by the root runs and fixed in this round:

- B1: account checks required `/usr/bin/userdbctl`, which no stock image ships
  (Ubuntu 22.04 has no package for it). spawnd now queries the userdb Varlink
  sockets directly, keeps provider attribution, refuses reserved uid/gid/name or
  alias claims from non-NSS providers (enumeration plus named and numeric
  lookups), and fails closed when no provider was completely enumerated.
- B1: on systemd 249 a second reference to the same unit from one D-Bus
  connection (AddRef at start, RefUnit during inspection), released while the unit
  was inactive, left PID 1 busy-looping and unresponsive to every D-Bus call until
  a reset. spawnd now holds at most one reference per unit per connection.
- B1: cleanup treated a cgroup removed before `inactive` as an error; it now
  retries and accepts absence only after an inactive readback.
- B1/C/F/gateway: `resources_ready` requires the gateway, spawnd, updated and
  egress units in `duduclaw-tcb.slice`; only spawnd had it. The egress and
  updated units and the generated gateway unit now set the slice.
- E: `TemporaryFileSystem=/` is overridden by `ProtectSystem=strict`, which
  `DynamicUser=yes` implies, so the worker saw the whole host root including
  `/etc/shadow`. The unit now uses an empty `RootDirectory` plus a read-only
  skeleton; `/tmp` and `/var/tmp` are the same bounded tmpfs (PrivateTmp's
  host-disk `/var/tmp` bind is replaced). `/sys` remains a read-only sysfs
  that systemd forces. The installer must create `/usr/lib/duduclaw/parse-root`
  and `/usr/lib/duduclaw/parse-root-tree` (see the crate README).
- F: the execveat fixture ran from `/run`, which Debian mounts `noexec`
  (test-only change; production staging is under `/opt/duduclaw-isolated`).
- H: LOOP_CONFIGURE kept the operation's flocked file description, so every
  later operation on an attached image returned Busy; detach did not wait for
  asynchronous AUTOCLEAR; online grow (and in-image discards) punch holes in the
  backing file, after which the image was refused as UnsafeBacking. The loop now
  gets its own identity-checked description, detach waits up to 3 s, and root
  re-allocates a sparse image after grow and on re-attach with capacity admission.
  Holes punched while mounted can still release host blocks until re-attach.
- C: the DNS broker refused NODATA reached through a CNAME (for example
  `www.github.com` AAAA), making IPv4-only aliased names unresolvable. A
  consistent alias chain with NOERROR, TC clear and a covering SOA is now an empty
  result for that family; both families empty still fails.
- Caller build: `duduclaw-inference` stopped being `Send` after the backend
  child migration (`LlamafileManager` now uses a tokio Mutex), and `duduclaw-cli`
  needed `recursion_limit = "512"`. Gateway and CLI with tests then type-check on
  Linux.

Design limitation confirmed, not changed: B2 refuses Landlock ABIs above 7
(`UnsupportedKernel` from the readiness probe). The current GCP Ubuntu 24.04
image boots kernel 7.0 with ABI 8, so every B2 launch test fails there with that
readiness error. Accepting newer ABIs needs an owner decision.

Daybreak security review of these fixes: three rounds; the reported MEDIUM and
LOW findings in the userdb client were fixed and the final pass reported no
finding. Not covered by any root test yet: live SSH/su/cron/at login attempts
against employee accounts, spawnd-to-B2 end-to-end launch through a real
transient unit, D8 queued admission across two runs, resource exhaustion (fork
bomb, PTY/fd exhaustion, unbounded output), btrfs/LVM-thin/dm-crypt backing
refusal, A/B update power-cut, migration (G) and MCP boundary (D).
