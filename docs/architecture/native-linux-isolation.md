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

Kernel readiness checks require Linux 5.15+, actual Landlock ABI >=1, seccomp
KILL_PROCESS, clone3, openat2, statx mount IDs, close_range and detached fs-context
mount APIs including recursive mount_setattr/NOSYMFOLLOW. I1 selects the exact
ABI-7 filesystem policy on kernels reporting newer ABIs; missing ABI 1 or any
enforcement failure still refuses execution. Launch still verifies every actual namespace, mount
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
process_vm calls and narrows socketpair to AF_UNIX. The earlier refusal of Landlock ABI >7 is superseded by the I1 compatibility
policy below. The launcher uses fixed cwd `/work`; B1's environment is
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

Historical limitation at the time of this root acceptance: B2 refused Landlock
ABIs above 7 (`UnsupportedKernel`). I1 changes that policy to use the ABI-7
ruleset on newer kernels. The ABI-8 row above remains the observed historical
result; the changed code still needs a new native acceptance run.

Daybreak security review of these fixes: three rounds; the reported MEDIUM and
LOW findings in the userdb client were fixed and the final pass reported no
finding. Not covered by any root test yet: live SSH/su/cron/at login attempts
against employee accounts, spawnd-to-B2 end-to-end launch through a real
transient unit, D8 queued admission across two runs, resource exhaustion (fork
bomb, PTY/fd exhaustion, unbounded output), btrfs/LVM-thin/dm-crypt backing
refusal, A/B update power-cut, migration (G) and MCP boundary (D).
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
## Package G — migration from v1.79.3

### Migration security correction (2026-11-05)

The migration security patch separates `ContentArchived` (authenticated streams
persisted, no content claim) from `ContentImported` after administrator decisions.
Root resumes confirmed employees in batches, binding all product writes and blob
copies to proposal operation/digest and a live confirmed allocation. Per-record
materialization, blob-copy and readback receipts are durable; a crash repeats the
same identity and cannot rebind to another allocation. The dashboard reports
rejected employees as not migrated. Rejected plaintext archive payloads are
securely deleted before their terminal receipts; originals remain in the encrypted
snapshot until expiry. Details and the additional signed-platform allocation
protocol are in [the migration README](../../crates/duduclaw-migration/README.md#post-confirmation-content-and-recovery).

Cleanup requires all proposals and all accepted records to be terminal even past
the deadline. With a verified snapshot it removes `legacy` and extracted streams
early, retaining encrypted recovery until expiry. Key readback atomically removes
`fresh-keys`, retaining only non-secret digests and receipts. Legacy archive-only
Complete journals are corrected, while old manifests lacking the v2 inventory
seal refuse cleanup because their attachment/work-product inventory is incomplete.

Authority adapters now commit intents before external calls and CAS receipts
outside any SQLite writer transaction. One 30-second deadline covers the async
bridge and nested synchronous product file-lock waits, including publication and
compensation after async calls return.

This is component implementation, not native deployment acceptance. The signed
`duduclaw-migration-platform` executable and production Employee allocation backend
are still absent from this worktree; the new materialization path requires their
live stopped-process, UID, mount and quota proof. Gateway/CLI were not built in
this security task, and real Linux root materialization remains unverified.
The prior round notes below are historical evidence, not this patch's test results.

### Round 3b status (2026-11-05)

Phase 1 completed: inspected the inherited tracked diff and untracked Package G
files on `feat/iso-g-20261105`; no reset or deletion of the interrupted work was
needed. The requested offline test command passed: importer 35 (31 unit + 4
integration), migration 34, isolation 29; **98 passed, 0 failed**. `git diff
--check` also passed. No gateway/CLI build or commit was performed.

Phase 2 completed within its 25-minute budget (about 01:09–01:24 UTC). The
inherited launcher/startup code was retained and hardened; this round did not
replace the missing F/B/H integration. **110 portable tests passed, 0 failed**:
importer 41 (37 unit + 4 integration), migration 40 (38 unit + 2 integration),
isolation 29. The exact command was:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline
```

Both passed; the second is Linux **type checking**, not native execution. Existing
unsafe-code/dead-code warnings remain. Logs:
`/private/tmp/iso-g-round3b-final.log` and
`/private/tmp/iso-g-round3b-linux-final.log`. No gateway/CLI build, `cargo fmt`,
commit, native service start or production migration was performed.

Round 3b changes and evidence:

| Files (relative to repository root) | Change |
| --- | --- |
| `crates/duduclaw-migration/src/import_manifest.rs`; gateway `src/handlers/migration_authority.rs` | Extracted the actual inert proposal-staging function into the small migration crate; gateway delegates to it. Rejects mixed migrations and duplicate source records before staging. |
| `crates/duduclaw-migration/tests/authority_extraction.rs`; migration `Cargo.toml`; `Cargo.lock` | Real extractor → serialized proposals → actual staging store → reopen/replay integration test for all 17 categories, including planted Admin/password and stdio MCP. No live account/config/employee files are created; unconfirmed apply never reaches the applier. Importer is a test-only dependency. |
| `crates/duduclaw-import/src/authority_schedules.rs`; `src/lib.rs`; `src/authority.rs`; migration `src/root_pipeline.rs` | Exact-schema workflow/autopilot readers and bounded reminders JSONL reader; proposals are per-record inert metadata with mandatory re-entry, never old actions, acceptance, grants, leases or delivery routes. Root inventory and WAL transport route to these readers. |
| `crates/duduclaw-import/scripts/capture_schedule_schemas.py`; `src/autopilot-v1793.sql`; `src/workflow-v1793.sql`; gateway `src/migration_import.rs` | Reproducible schema catalogs and remote gateway catalog-drift assertions against the normal store constructors. Catalog generation succeeded; gateway assertions are written but not run locally. |
| `crates/duduclaw-import/src/authority_config.rs`; `src/authority.rs` | Employee action rules/trusted-read hints become separate inactive Policy proposals requiring re-entry; malformed shapes are withheld. MCP URL fragments join userinfo/query redaction. Focused canary tests pass. |
| `crates/duduclaw-migration/src/import_profile.rs`; `src/import_launcher.rs`; `src/lib.rs`; importer `systemd/duduclaw-import@.service` | Portable sandbox readback checks; add arm64 `mkdirat`; test reference-unit parity and reject widened/missing properties. Accept only the explicit syscall list plus the reviewed fixed systemd v249/v252/v255 implicit defaults, never arbitrary groups. Actual DynamicUser launch is still unverified. |
| `crates/duduclaw-migration/src/import_manifest.rs`; gateway `src/migration_startup.rs` | Shared pure boot decision with portable tests: explicit import requests cannot fall back to normal startup, and an unfinished manifest for this home blocks ordinary boot. Gateway/CLI startup process tests still require the remote builder. |
| `CHANGELOG.md`; importer `README.md`; security-defense feature docs in all three languages; this document | Updated unreleased behavior and evidence boundaries. |

The scheduling readers deliberately do not restore executable workflows or
reminder delivery. Full category **extraction coverage** is tested; full category
**apply/readback** is not complete. Remaining priority 4/5 work is unchanged:
F/B/H installed platform adapter and employee/principal allocation, confirmed
content materialization, Personal fresh login, Connector and non-Telegram
activation, non-time schedules, org/action-rules/cached-config apply/readback,
first-admin issuer, and final completion/report integration. Without the platform
adapter, `run` still refuses before taking the old service offline. The exact
remote test commands below remain required.

Independent static review found and resolved an arm64 syscall omission, URL
fragment credential leakage, and systemd implicit-default readback compatibility.
The default expansion was checked against official [v249 transient-unit
code](https://github.com/systemd/systemd/blob/v249/src/core/dbus-execute.c) and
[v255 syscall definitions](https://github.com/systemd/systemd/blob/v255/src/shared/seccomp-util.c).
The final reviewer reported no remaining finding in that correction. This is not
formal Daybreak review or native systemd acceptance. During development the new
users fixture omitted a required `bound_at` column; the fixture was fixed before
the final passing run. Shared isolation wire formats/hard limits are unchanged.

The older round-3 notes below describe inherited code and historical results,
not additional round-3b acceptance evidence. In particular, their unexecuted
gateway Admin/MCP test is distinct from the new portable integration test above.

Status: **PARTIAL — round 3 adds runnable G components, but the requested full
migration and all authority apply categories are NOT complete.** No commit,
release, deployment or production migration was performed. This worktree still
lacks the F/B/H platform adapter and several normal product apply/readback paths;
`run` therefore still refuses before taking the old service offline on an install
without that adapter. This is not end-to-end migration acceptance.

### Round 3 implementation and remaining gates (2026-11-05)

Implemented in this worktree:

- A real `systemd-run` DynamicUser importer launcher. Root verifies systemd
  properties, MainPID, actual UID, capabilities, NoNewPrivileges, seccomp and a
  distinct network namespace before passing one or two read-only regular-file
  descriptors through SCM_RIGHTS. The 1 GiB cgroup memory limit accounts for the
  main/WAL copies in tmpfs. Actual home/quarantine paths are hidden and unsupported
  ProtectHome layouts are rejected before outage. Output is bounded and withheld until process
  exit/readback. Verified parser rejection is separate from fatal sandbox/IPC
  failure. Native systemd execution has not been run here.
- Exact path inventory under a pinned quarantined dirfd, bounded traversal,
  no-follow source opens, immutable source IDs, root-owned staged streams and
  source-rejection reports. CLI cache remains excluded except the exact Claude
  hook settings file. Unrecognized sources remain in recovery; attachments and
  work-product publication still require the confirmed B/H allocation path.
- An exclusive gateway startup branch before normal config/bootstrap/listeners.
  The CLI branch precedes file logging, OTLP and redaction initialization.
  It forces and verifies non-dumpable/NNP, consumes systemd FD 3 and checks a
  root-owned import manifest. Ordinary boot refuses an unfinished import plan.
  Root-peer authorization plus a per-message SCM_CREDENTIALS handshake verifies
  the actual gateway acceptor despite systemd owning the listening socket.
- Unconfirmed employees' content is archived with source IDs and durable stream
  receipts, without creating employee directories, UIDs, disks or active
  memory/wiki entries. A root-only status reply reports exact stream, record,
  pending and expired-approval counts. It is **archive acceptance**, not completed
  materialization into confirmed employees' live content namespaces.
- Worker extraction for known config/org/agent TOML, MCP JSON, instruction files,
  skill text, hook/settings JSON, roles/departments/bindings/connectors/data-source
  JSON, scheduling JSON, wiki scope policy, exact v1.79.3 users/tasks/approvals/cron
  schemas and the sessions-with-channel-settings variant. Actual remote MCP,
  personal/OAuth stores, channel bindings, empty departments and old `.soul_*`
  baselines are accounted for. Encrypted stores and unsupported authority
  artifacts produce explicit re-entry obligations instead of becoming authority.
  Users exclude password/MFA/session/token material; tasks emit inactive schedule
  proposals. Unknown schema/version, extra views/triggers and quick_check failure
  reject the entire source. Unknown config sections and secret-bearing values
  require re-entry; the worker does not put original credentials in proposals.
- Actual stdio MCP apply/readback/disable, with normal scanning and locked atomic
  config writes, baseline compare and an operation/digest receipt in the same
  artifact. Remote MCP, old environment credentials and remote bridges refuse.
  Profile instruction changes call the ordinary review/save/history/baseline path
  and reload the live registry. Neither path launches the proposed command.
- Atomic Shared ACL binding receipts in the same transaction as the ordinary auth
  binding writer. Fresh active login is required. Gateway application additionally
  requires actual Shared-principal allocation readback from B/D/H; an Applied
  Employee proposal or `shared:` label alone is insufficient. This gate currently
  refuses because the registry adapter is absent.
- Root pipeline composition through content import/confirmation readback. A
  waiting administrator is a successful `AwaitingAdministrator` result; completion
  still requires fresh zero-outstanding readback. The CLI uses the composed
  integration, not the early-stop adapter.
- Skill and hook adapters use the normal scanner/loader and guard merge paths.
  Prepared inode receipts survive publication without hard links in live paths.
  Profile history binds the operation/digest. Department publication uses
  `RENAME_NOREPLACE`, inode and operation xattr ownership. Equal normal writes
  are not adopted or removed by compensation.
- Config/Budget/policy ownership is recorded from the normal writer's fsynced
  temporary file before rename. The ordinary product-policy/MFA/Google source
  guard remains in the write path; migration refuses Google/resolver source
  changes. Compensation checks ownership again inside the normal file lock.
- Telegram channel bindings, pure time cron and read-only database grants have
  normal writers and actual router/scheduler/registry readbacks. They remain
  unavailable without the real Shared-principal backend. Ordinary binding writes
  now read/mutate/write under the same lock; unrelated operations retain receipts.
- Retrying a failed apply first authenticates the session and binding, reconciles
  partial effects, then performs fresh validation. A failed fresh validation
  leaves the item Failed and its prior partial authority disabled.

#### Authority coverage

These are code paths, not native acceptance results. Every extracted item stays
inert until confirmation; a written adapter that still requires B/D/H proof is
not an available production migration path.

| Category | Source and apply boundary |
| --- | --- |
| User | Exact `users.db`; fresh invitation-only account, new password/MFA required |
| Role | Built-in Admin/Manager/Employee role travels with User; product has no mutable custom-role registry |
| Department | Real department directories, org/user metadata; normal creation with owned-directory compensation |
| Employee | `agent.toml` pending only; confirmed immutable UID/quota allocation awaits B/H |
| EmployeeInstructions | Normal review/save/history for SOUL/IDENTITY/INSTRUCTIONS/CONTRACT; old baselines are not trusted |
| UserAgentBinding | Shared ACL adapter requires fresh login and real Shared proof; Personal reclaim remains missing |
| ChannelBinding | Actual channel store; Telegram adapter requires Shared proof; other channels require re-entry |
| LegacyApproval | Exact approvals DB and JSON; always ExpiredRejected, never reactivated |
| McpServer | Normal stdio writer/scanner, empty env supported; credential/remote records require reconnection |
| Skill | Flat Markdown or canonical SKILL.md via normal scanner/loader; extra bundle artifacts/shared skills require re-entry |
| Hook | Strict command-hook structure, normal mandatory guards, owned atomic publication; never executes during apply |
| Policy | Delegation plus explicit per-use container/computer_use/provenance readers; org/action_rules/other policies remain unsupported for apply |
| Connector | Actual personal/OAuth/website stores are inventoried; provider-specific fresh connection/apply is missing |
| DataSourceAuthorization | Separate read-only grants from old agent capabilities; normal validator/writer/registry, requires Shared proof; source definitions/secrets require re-entry |
| Budget | Positive daily/monthly caps become separate proposals; unlimited/unknown settings require re-entry |
| ConfigurationSection | Explicit safe container/computer_use/provenance sections; other cached sections need their own reload/readback adapters |
| Schedule | Actual cron DB/JSONL pure time records, normal store and live cache; Shared proof required; workflows/goals/responsibilities/command triggers remain re-entry obligations |

**Still missing (do not report Package G complete):**

1. `InstalledPlatform` is a fixed, bounded, root-owned adapter protocol, but its
   expected `duduclaw-migration-platform` executable is **not implemented or
   installed in this worktree**. F/B/H must supply signed preflight, live final
   ownership/quota/protocol checks and fresh service activation. Thus the normal
   CLI `run` remains unavailable without that integration; substituting this seam
   did not by itself satisfy the requested end-to-end flow.
2. Employee allocation and Shared/Personal principal integration; provider-specific
   Connector activation; non-Telegram channel admission; non-time scheduling
   admission; org/action_rules and other cached configuration/policy reloads.
   Shared ACL/channel/cron/database-grant adapters await immutable principal
   readback. Personal fresh-login and archive materialization remain open.
3. Historical authority schema variants beyond the explicit catalogs, external
   OAuth revoke/reconnect and per-secret rotation inventory. Exact approvals DB
   extraction is now implemented and expires every old approval. Original
   credential retention is not wired from the extractor: secret-bearing proposals
   require re-entry; a notice is not a restored provider connection.
4. First-admin bootstrap issuer/claim-route integration, final completion report,
   installed expiry timers, cross-filesystem quarantine and file-tree recovery.
5. Remote gateway/CLI compile/tests, actual v1.79.3 root migration on the three
   Linux distributions and formal Daybreak review.

Shared `duduclaw-isolation` wire formats and hard limits are unchanged. Package G
has a private manifest/handshake protocol and additive private proposal fields.
The importer adds the already-present workspace `toml` dependency and gateway
adds importer as a test-only dependency; no heavy new package was introduced.

Round 3 local results and exact remote commands are recorded below under
**Round 3 validation**. The following round-2 implementation notes are historical
background; this section supersedes their launcher/startup and support status.

### Round 3 validation

Executed locally on macOS, with two build jobs and incremental compilation off:

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth --offline` | 136 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-isolation --offline` | Earlier 33 migration + 29 isolation passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration --offline` | Final 34 passed, 0 failed, including the new partial-compensation retry case |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-import --offline` | Final 31 unit + 4 integration passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | Passed; unsafe-code lint warnings remain |
| `git diff --check` | Passed |

The 234 distinct final passing tests above do **not** include gateway tests. In
particular, the planted Admin/MCP test and the import-only startup tests have been
written but have not been executed here. The Linux cross-check is type checking,
not native execution. No gateway/CLI/workspace build, `cargo fmt`, remote build,
root systemd test, service deployment or production migration was run. The web
dependencies are absent (`web/node_modules`), so TypeScript/Vitest were not run.

Local logs: `/tmp/iso-g-round3-auth-full.log`,
`/tmp/iso-g-round3-migration.log`, `/tmp/iso-g-round3-migration-final.log`,
`/tmp/iso-g-round3-linux-final.log`; final importer output is in the delegated
test transcript (the earlier `/tmp/iso-g-round3-import-final.log` predates the
additional sources). During development an importer fixture INSERT and the new
retry test's error-comparison assertion failed; both were fixed and their suites
then passed. Independent static reviews closed the import-boot, secret/schema,
Shared proof, normal-write ownership and retry findings. This is not a formal
Daybreak review and does not replace gateway compilation.

Run on the remote Linux builder (not on the shared 16 GB Mac):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo build -p duduclaw-import -p duduclaw-migration
cargo test -p duduclaw-auth migration_
cargo test -p duduclaw-migration
cargo test -p duduclaw-import
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-cli --test migration_import_startup
cargo test -p duduclaw-gateway migration_confirmation_
cargo test -p duduclaw-gateway migration_decisions_
cargo test -p duduclaw-gateway migration_authority_ -- --test-threads=1
cargo test -p duduclaw-gateway migration_import_
cargo test -p duduclaw-gateway --lib agent_binding
cargo test -p duduclaw-gateway --lib departments
cargo test -p duduclaw-gateway --lib db_source_grants
cargo test -p duduclaw-gateway --lib cron_store
cargo test -p duduclaw-gateway --lib cron_scheduler
cargo test -p duduclaw-gateway --lib config_commit
cargo test -p duduclaw-gateway --lib home_publication
```

The authority filter includes these required regression cases:

```sh
cargo test -p duduclaw-gateway migration_authority_planted_admin_and_mcp_inactive_until_individual_confirmation
cargo test -p duduclaw-gateway migration_authority_profile_uses_normal_review_writer_and_live_registry
cargo test -p duduclaw-gateway migration_authority_task_schema_matches_fresh_store
cargo test -p duduclaw-gateway migration_authority_shared_label_never_substitutes_for_principal_readback
cargo test -p duduclaw-gateway migration_authority_same_content_replacement_is_not_owned_or_compensated
cargo test -p duduclaw-gateway migration_authority_config_refuses_equal_normal_publication_and_compensation
cargo test -p duduclaw-gateway migration_authority_config_receipt_survives_restart_and_rejects_google_source_change
cargo test -p duduclaw-gateway migration_import_schema_catalog_matches_approval_channel_and_cron_stores
```

Run privileged tests only in a disposable native Linux VM. The importer must
already be installed as a root-owned executable with root-owned non-writable
ancestors outside `/home`; its sandbox hides `/home`. Optional default protected
paths may be absent; the pipeline also hides the actual fresh home and quarantine.
Do not run these against a customer installation.
Repeat the native matrix on Ubuntu 22.04, Ubuntu 24.04 and Debian 12:

```sh
sudo env DUDUCLAW_IMPORT_TEST_BINARY=/usr/lib/duduclaw/current/duduclaw-import cargo test -p duduclaw-migration -- --ignored iso_root_ --test-threads=1
sudo cargo test -p duduclaw-gateway -- --ignored iso_root_migration_import_ --test-threads=1
```

The migration root filter covers DynamicUser FD transport, SQLite+WAL transport,
parser rejection versus isolation failure, actual acceptor credentials behind a
root-created socket, snapshot/rotation resume, pidfd stopping, transient unit
inventory and blob-copy constraints. These tests do not replace a signed F/B/H
installation plus full legacy-upgrade acceptance.

When web dependencies are available:

```sh
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

Round 3 file responsibilities (rounds 1–2 remain uncommitted in this worktree):

| Files | Change |
| --- | --- |
| `crates/duduclaw-migration/src/import_launcher.rs` | Verified DynamicUser launch, bounded output and read-only descriptor handoff |
| `crates/duduclaw-migration/src/root_sources.rs` | Pinned no-follow source inventory and exact source routing |
| `crates/duduclaw-migration/src/root_pipeline.rs` | Root import coordinator and explicit installed platform boundary |
| `crates/duduclaw-migration/src/import_manifest.rs` | Private bounded root manifest with inert authority proposals |
| `crates/duduclaw-migration/src/import_receiver.rs` | Live acceptor credential handshake and staged relay |
| `crates/duduclaw-migration/src/{root_host,native_stop,journal,lib}.rs` | Composed pipeline access, pending-admin outcome and evidence/error plumbing |
| `crates/duduclaw-migration/src/bin/duduclaw-migrate.rs` | Select composed installed integration |
| `crates/duduclaw-import/src/{authority,lib,linux,main,sqlite}.rs` | Authority proposal extraction, FD worker mode, parser exit status and defensive SQLite handling |
| `crates/duduclaw-import/src/*-v1793.sql`, `scripts/capture_*_schema.py` | Reviewed exact authority database schema fixtures |
| `crates/duduclaw-import/tests/linux_binary.rs`, `systemd/duduclaw-import@.service` | Worker regression and sandbox template updates |
| `crates/duduclaw-gateway/src/migration_startup.rs` | Exclusive hardened import-only boot |
| `crates/duduclaw-gateway/src/migration_import.rs` | Inert archive imports, status/readback and durable receipts |
| `crates/duduclaw-gateway/src/{lib,server}.rs`, `crates/duduclaw-cli/src/lib.rs` | Early startup branch before ordinary initialization |
| `crates/duduclaw-cli/tests/migration_import_startup.rs` | Real CLI subprocess refusal before logging/configuration writes |
| `crates/duduclaw-gateway/src/handlers/migration_authority*.rs` | Inert staging, normal MCP/profile writers, Shared proof and planted legacy tests |
| `crates/duduclaw-gateway/src/handlers/{migration_confirmations_rpc,mod}.rs` | Live context wiring and honest per-item availability |
| `crates/duduclaw-auth/src/{migration,db}.rs` | Atomic Shared binding receipts using normal binding semantics |
| `Cargo.lock`, importer/gateway `Cargo.toml` | Existing TOML dependency and test-only importer dependency |
| Migration/importer `README.md`, this handoff | Current implementation, limits and exact verification commands |
| `docs/features/*/05-security-defense.md`, `docs/guides/*/linux-protected-service.md`, English equivalents, `CHANGELOG.md` | Three-language evidence-bounded behavior notes |

### Root execution and recovery (historical round 2)

`duduclaw-migration::root_host::RootMigration` validates root-owned ancestors,
locks the installation across migration IDs, pins an immutable plan, and advances
the durable journal only after stage-specific readback. `RootMigrationIntegration`
is the named F/B/A/D/H integration seam. `preflight` defaults to refusal; normal
`run` refuses before stopping services if downstream integration is missing.

The explicit maintenance command `prepare --plan <root-owned-plan> --stop-after-keys`
stops at `KeysRotated` and leaves the installation OFFLINE. It is not a full
migration. It inventories installed and loaded/transient services, follows
activation triggers, masks units, locks the old account, disables linger and
user-manager sessions, checks cron/at ACLs and jobs, and kills/rechecks processes
through pidfds. Unit names support exact systemd hexadecimal escapes and reject
leading option syntax. Commands have deadlines and bounded captured output.

Quarantine uses same-filesystem RENAME_NOREPLACE and a root-only directory.
Snapshot walking uses pinned descriptors and openat2 with no symlink, magic-link
or mount traversal. Special files are metadata only; file identity, byte/file/depth
limits and bounded xattrs are checked. AEAD chunks bind migration/sequence/length;
terminal authentication prevents truncation and a failed writer cannot reuse a
nonce. Publication fsyncs and verifies decryption before publishing. Fresh home
creation and root/identity/JWT/internal-MCP key publication use actual product
formats and resumable root-owned staging. Fresh configuration disables dispatch
and host-credential inheritance and requires MFA.

`status`, `export-records` and `expire` are implemented. `new-setup-token` has a
root-terminal/stage-gated `RootBootstrapIssuer` seam, not a working issuer: this
worktree has no setup-token claim route. The adapter must use the gateway's
already-open fresh UserDb and return the actual product URL; root never opens
gateway-owned SQLite/WAL and never prints a fabricated link. Recovery exports inert
JSONL into a new root-owned directory, not a restored runnable installation.
Expiry uses the immutable deadline, verifies the snapshot before removing
plaintext, traverses without following links, then removes the encrypted archive
and its key. The supplied daily systemd expiry templates are not installed.
Cross-filesystem quarantine and reconstruction of a regular-file recovery tree
are still unsupported and refuse.

### Import worker and actual gateway writes

`duduclaw-import` is a non-root Linux fd worker. It refuses missing read-only
regular source descriptors, missing NoNewPrivileges and unknown input formats;
unrelated inherited descriptors are closed. The DynamicUser/no-network/resource
profile exists, but a real launch/property-readback/SCM_RIGHTS integration is still
required before trusting worker output.

Supported formats: inert UTF-8 content, attachments/work-product hashes, exact
fresh v1.79.3 sessions and memory SQLite schemas. The memory catalog is reconstructed
from reviewed engine schema SQL; its equality test against a real fresh engine is
written for the remote gateway builder and has not yet been run. Unknown legacy
schema variants reject the whole DB. SQLite parses only a private DB/WAL copy,
with defensive/trusted-schema/extension/trigger/view restrictions, a read allowlist,
query deadline, per-value/op limits and a global hard heap cap that only narrows.
No raw SQLite errors, schema names or input content enter diagnostic messages.

`import_receiver::StagedImport` checks the entire bounded stream and successful
worker exit before making records available. Migration, employee, source, kind,
sequence, record count and stream digest are host-bound. The root relay verifies
the destination gateway UID and checks completion readback; the gateway connection
handler requires root SO_PEERCRED. `ImportModeGate` must prove no public dashboard,
channels or employee execution are running and resolve the new immutable employee.
Its unavailable implementation refuses before opening destination databases.

`GatewayImportWriter` uses a durable content ledger for restart retries. Memory
writes use the normal engine API with origin trust 0 and import lineage; imported
content cannot supersede new higher-trust facts. Wiki writes use the normal wiki
API with old frontmatter quoted as inert JSON. SessionManager imports quoted
historical user content into archived sessions with message and receipt in one
transaction; system/tool roles, summaries, live channel bindings and wakeups are
not revived. Task/audit content stays in an inert archive outside live actions.

`import_blobs::copy_blob` implements root fd-relative content copy, a path
allowlist, O_PATH classification, no links/xattrs/mount traversal, quota/count/depth
limits, source identity and hash checks, private staging, fchown, atomic publication
and destination readback. B/H must provide confirmed allocation, durable quota and
root copy receipts; gateway refuses blob completion without them.

### Dashboard authentication and decisions

The Admin-only review RPC rechecks the live account and credential epoch. Password
plus enrolled TOTP establishes a five-minute proof tied to the verified dashboard
session. CSRF is random, persistent and session-bound. TOTP replay and password
attempt limits survive restart; demotion, credential changes, expiry and clock
rollback refuse authorization. No client-provided MFA/admin flags are accepted.

The page now supports individual approval/rejection, bulk approval of low-risk
proposals, atomic bulk rejection of untouched proposals, retry/resume after a
crash and credential replacement. Both edition nav lists retain the 1.80.0 badge.
Untrusted values are rendered as plain text, full high-risk fields are scrollable,
and fingerprints are masked. Unsupported items can be rejected but not approved.
The UI reloads readback state after mutations; an approval response is never
presented as proof that application succeeded.

Confirmation transactions bind operation ID, revision and complete normalized
proposal digest. Approved payloads cannot change. Stage/publish/readback/disable
steps remain durable and repeatable; failure is committed before compensation.
At most 200 employee proposals and 200 decisions per request are accepted. Current
bulk approval is conservatively limited to Department labels. Migration remains
incomplete until root completion readback, all proposals and secret obligations
are settled.

### Actual authority writers and secrets

`GatewayAuthority` implements normal Department and invitation-only User writes,
delegation-policy configuration and agent budgets. The invitation and operation
receipt are atomic; a duplicate email is never adopted as an idempotency receipt.
Compensation suspends the same account and fences its credential epoch; retry
cannot resurrect an account already activated with a password.

Configuration staging is encrypted, commits use config_commit compare-and-swap,
and compensation refuses to overwrite concurrent edits. Budget and delegation
are read from disk on each normal gate. Other configuration requires
`MigrationConfigReadback` to reload and verify effective runtime state. Without
that adapter it is not approvable and cannot become Applied merely because a file
matches. `MigrationAuthorityBackend` is the explicit per-category seam for the
remaining authority types; no default implementation grants authority.

Secret values stay outside proposal/audit/export schemas in encrypted staging.
Fingerprints use the same protected HMAC key when proposing and rotating. The
rotation path writes through the normal configuration writer, verifies effective
readback, and only then records the new fingerprint. The root proposal producer
still needs to connect fingerprint_secret/stage_legacy_secret; a client assertion
cannot clear rotation obligations. `GatewayAccountReadback` is now wired into the RPC for the narrow `accounts`
configuration proposal. It accepts one explicit API-key account using the product
provider allowlist, normal account-entry builder and encrypted credential writer.
Host credential inheritance must be explicitly false; OAuth, `[api]` fallback,
remote secret references and ambiguous pool IDs refuse. Readback invalidates and
rebuilds the normal rotator and checks exact account/provider/priority/budget and
selected plaintext key internally. Compensation reloads and verifies removal.
Rotation only changes the encrypted key field, preserving later account edits.
It makes no provider network call, does not prove provider-side validity, does not
revoke the old key at its provider, and does not terminate already-running work
that holds an older rotator reference, including an existing Discovery run. Two-provider real-rotator tests are written
but await the remote gateway builder.

### Remaining work and acceptance boundaries

- Signed installer/F integration for preflight and the later migration stages;
  actual DynamicUser launcher, source FD transport and sandbox-property readback.
- Import-only gateway boot/listener and authoritative migration manifest;
  external OAuth revocation/reconnect report and manual rotation inventory.
- Legacy TOML/JSON authority extraction and unsupported historical SQLite catalogs.
- Concrete Employee, Role, EmployeeInstructions, user/channel bindings, MCP,
  Skill, Hook, Connector, DataSourceAuthorization and Schedule backends; some old
  proposal shapes lack a target employee/artifact/source handle and cannot safely
  infer one. B/H confirmed UID/disk allocation and blob receipt integration.
- Personal owner fresh-login binding, first-admin service startup integration,
  final isolation/protocol/ownership/quota readback and completion reporting.
- Cross-filesystem quarantine, file-tree recovery and installed expiry timer.
- Three-distribution root acceptance, actual v1.79.3 upgrade, gateway/CLI build,
  web TypeScript/Vitest/browser checks and formal Daybreak security review.

Dependencies added to these small crates already existed in Cargo.lock (including
chrono/tempfile/libc/sha2); no heavy new dependency or shared contract change.

### Round 2 validation and handoff

Actual local results (2026-11-05):

| Command | Observed result |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth --offline` | 135 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline` | migration 31, importer 10, isolation 29; 70 passed, 0 failed |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | PASS; type check only, not Linux execution; syscall unsafe lint warnings remain |
| Migration translation JSON and key parity | PASS, 85 matching keys in three languages |
| `git diff --check` | PASS |

Total: **205 distinct local tests passed**. Earlier implementation errors (a
password-hash field lookup, Linux read/type inference and FTS schema authorizer
restrictions) were corrected before these final checks. No gateway/CLI/workspace
build was run, as requested. `web/node_modules` does not exist, so TypeScript,
Vitest and browser checks were not run and dependencies were not installed.

Independent code review found and prompted fixes for ineffective config readback,
missing transient units, raw SQLite diagnostics, pre-validation relay reads,
SQLite heap limits and escaped unit names. Final static re-review confirmed that
credential rotation preserves later budget/priority/label/tag edits, partial-write
errors are described accurately, and zero-quota blob replay only accepts a verified
existing artifact. Gateway tests cover same-value retry after a committed secret
write with failed runtime readback; they are still remote-only, not executed here.
This is not a formal Daybreak review.

Remote builder commands (not run locally):

```sh
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-gateway migration_confirmation_
cargo test -p duduclaw-gateway migration_decisions_
cargo test -p duduclaw-gateway migration_authority_ -- --test-threads=1
cargo test -p duduclaw-gateway migration_import_
# Includes exact current MemoryEngine schema versus importer fixture comparison.
# The native tests below require disposable Linux VMs, not a production host.
cargo test -p duduclaw-migration root_host::
cargo test -p duduclaw-import --test linux_binary -- --nocapture
cargo test -p duduclaw-migration -- --ignored iso_root_
cargo test -p duduclaw-gateway -- --ignored iso_root_migration_import_
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

The root test suite includes quarantine/snapshot/key-rotation resume and lock,
symlink/plan-change refusal, pidfd stop, transient delayed-restart inventory,
root blob-copy rejection/readback and the prior snapshot walker test. Run it on
Ubuntu 22.04, Ubuntu 24.04 and Debian 12 before claiming Linux acceptance.

Round 2 file responsibilities (in addition to the historical round 1 list):

| Files | Change |
| --- | --- |
| `crates/duduclaw-migration/src/root_host.rs` | Root plan, lock, journal runner, quarantine, snapshot, fresh keys, expiry and export |
| `crates/duduclaw-migration/src/native_stop.rs` | Actual service/activation/account/job/process lockdown and readback |
| `crates/duduclaw-migration/src/root_bootstrap.rs` | Root-terminal bootstrap issuer seam and safe output boundary |
| `crates/duduclaw-migration/src/bin/duduclaw-migrate.rs` | Root CLI run, explicit offline prepare, status, export, expiry, bootstrap entry |
| `crates/duduclaw-migration/systemd/duduclaw-migrate-expire@.service` | Uninstalled expiry service template |
| `crates/duduclaw-migration/systemd/duduclaw-migrate-expire@.timer` | Uninstalled daily expiry timer template |
| `crates/duduclaw-migration/src/import_receiver.rs` | Host-bound full-stream staging and root relay |
| `crates/duduclaw-migration/src/import_blobs.rs` | Root bounded no-follow file copier and readback |
| `crates/duduclaw-migration/src/confirmation.rs` | Existing-store writes, atomic bulk reject, authorization and rotation status |
| `crates/duduclaw-migration/src/confirmation_tests.rs` | Bulk rejection, missing database and existing decision regression coverage |
| `crates/duduclaw-migration/src/lib.rs` | New modules and unchanged contract re-exports |
| `crates/duduclaw-migration/Cargo.toml` | Existing-lockfile small dependencies |
| `crates/duduclaw-migration/README.md` | Real command usage and remaining integration boundaries |
| `crates/duduclaw-import/src/lib.rs` | Memory SQLite and bounded attachment/work-product source formats |
| `crates/duduclaw-import/src/sqlite.rs` | Exact memory catalog, heap cap and strict extraction |
| `crates/duduclaw-import/src/memory-v1793.sql` | Trusted fresh-engine schema fixture |
| `crates/duduclaw-import/scripts/capture_memory_schema.py` | Reproducible schema extraction from reviewed source |
| `crates/duduclaw-import/tests/content.rs` | Blob hash/binding rejection tests |
| `crates/duduclaw-import/tests/linux_binary.rs` | Non-root fd/NNP failure checks |
| `crates/duduclaw-import/Cargo.toml` | Existing sha2 dependency |
| `crates/duduclaw-import/README.md` | Supported formats and launcher boundary |
| `crates/duduclaw-gateway/src/migration_import.rs` | Root connection gate, real content writes and archive receipts |
| `crates/duduclaw-gateway/src/lib.rs` | Gateway import module registration |
| `crates/duduclaw-gateway/src/handlers/migration_confirmations_rpc.rs` | Live authentication, decisions, apply/resume and rotation RPC wiring |
| `crates/duduclaw-gateway/src/handlers/migration_authority.rs` | Durable product writers, compensation and encrypted secret handling |
| `crates/duduclaw-gateway/src/handlers/migration_authority_accounts.rs` | Encrypted account writes and effective rotator readback |
| `crates/duduclaw-gateway/src/handlers/mod.rs` | Protected namespace dispatch and module registration |
| `crates/duduclaw-auth/src/mfa.rs` | Persistent migration proof/attempt schema |
| `crates/duduclaw-auth/src/mfa/migration.rs` | Password/TOTP/session/epoch/CSRF authorization |
| `crates/duduclaw-auth/src/mfa/tests.rs` | Expiry, replay, demotion and restart rate-limit tests |
| `crates/duduclaw-auth/src/migration.rs` | Atomic invitation account/idempotency and compensation |
| `crates/duduclaw-auth/src/lib.rs` | Auth migration module export |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Cross-version refusal points to verified migration tooling |
| `web/src/lib/migration-confirmations.ts` | Typed decisions, step-up and credential replacement calls |
| `web/src/pages/MigrationConfirmationsPage.tsx` | Actionable authenticated individual/bulk/retry/rotation controls |
| `web/src/pages/MigrationConfirmationsPage.test.tsx` | Decision, authentication, rejection, masking and readback regressions |
| `web/src/i18n/{zh-TW,en,ja-JP}.json` | User-facing confirmation translations |
| `docs/features/{,zh-TW/,ja-JP/}05-security-defense.md` | Actual enabled authentication/confirmation behavior |
| `docs/guides/{,zh-TW/,ja-JP/}linux-protected-service.md` | Review workflow and migration acceptance limits |
| `CHANGELOG.md` | Self-contained unreleased user-facing behavior |
| `Cargo.lock` | Resolve only already-present dependency versions |
| `docs/architecture/native-linux-isolation.md` | Current implementation, tests, missing work and historical handoff |
| `wiki/reports/native-linux-isolation-package-g-2026-11-05.md` | Removed; its full historical text moved below, not left in published wiki |

### Historical round 1 handoff (superseded by round 2 below)

#### Package G 交付紀錄（2026-11-05）

狀態：**PARTIAL**。只修改交辦 worktree，沒有 commit、沒有建立或移除 worktree、沒有操作既有安裝或客戶資料。共用 contract 無變更。

本輪交付可測試的遷移元件、內容匯入器、首位管理員原子領取、唯讀確認頁與舊安裝器的跨版本拒絕。這不等於完成 Package G，也不能執行完整 v1.79.3 遷移。詳細介面與限制見 [實作說明](../../docs/architecture/native-linux-isolation.md)。

##### 實際驗證

| 指令 | 結果 |
| --- | --- |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration -p duduclaw-import -p duduclaw-isolation --offline` | PASS：migration 25、import 7、contract 29，共 61 passed / 0 failed；Linux 專屬測試在 macOS 未執行 |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth otp::tests --offline` | PASS：18 passed / 0 failed，包含兩條獨立 SQLite 連線搶同一 token |
| `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration -p duduclaw-import --offline` | PASS：Linux Rust 原始碼及測試型別檢查，未連結或執行 Linux binary；使用 SQLite 既有 bindings，未編譯 bundled SQLite C |
| `cargo check --target x86_64-unknown-linux-gnu -p duduclaw-import --offline`（初次原配置） | FAIL：缺 `x86_64-linux-gnu-gcc`，停在 bundled SQLite C build；不算 Rust 原始碼失敗，也不算 Linux 實機驗收 |
| `git diff --check` | PASS |
| 三語 JSON 解析及 key 集合比較 | PASS |

合計實際執行 79 個不重複測試通過。早期 migration 測試因 journal 測試使用 enum 不支援的 struct update 語法失敗，修正後已由上述完整指令重跑通過。Importer 保留有說明的 unsafe syscall/SQLite FFI，workspace `unsafe_code=warn` 會印警告。

未跑 gateway／CLI／workspace build（依交辦資源限制）；未跑 web TypeScript／Vitest（`web/node_modules` 不存在，未安裝依賴）；未跑 Linux 原生、Windows、設計畫布或瀏覽器驗收。Linux 交叉型別檢查不能替代上述驗證。

##### 審查

獨立 code-reviewer 看過實際變更；指出的缺 version-floor 放行、proposal 總量上限及 inherited fd 的 unsafe precondition 已修正並複查。archive I/O 錯誤後禁止再次加密以避免 nonce 重用，另有回歸測試。沒有正式 `gpt-daybreak-blue-latest` 安全審查，不能視為 Daybreak GO。

##### 精確未完成項目

1. 尚無可操作的 `duduclaw-migrate` root 命令及 installer 遷移接線。`HostAdapter` 沒有 production implementation：root flock、停止舊 gateway、mask/timer/socket/cron/at/user-manager 封鎖、清空舊 uid 程序與讀回待做。舊 installer 目前只會拒絕跨入 v1.80。
2. 快照走訪／AEAD 元件已有，但 fsync＋原子發布＋解密驗證整合、人工安全還原、期限清理及跨檔案系統複製待做。quarantine helper 只有同檔案系統 `RENAME_NOREPLACE`，呼叫端須先停止舊環境並封存父目錄權限。
3. 新 gateway 空資料目錄、root／identity／MCP／JWT 金鑰實際輪替、外部 OAuth 撤銷、手動輪替報告與管理員 setup link root-only 輸出尚未接線；journal 只檢查可信 adapter 的證據。
4. Importer 有 systemd profile，但 root 啟動／fd 交付／實際 sandbox 屬性讀回未接線。只支援 inert text 與精確 fresh v1.79.3 sessions.db schema；memory.db、其他歷史 schema、TOML／JSON 權威提案抽取、附件與工作產出複製待做。
5. root-only gateway import endpoint、完整 stream 暫存到 worker 成功結束、host-bound migration/employee/source handle 檢查、正常 content API／最低信任 lineage 寫入未接線。Importer stdout 不可直接當成功完整匯入。
6. 真實 session CSRF／step-up provider、每一類權威提案的正常寫入驗證／stage／publish／readback／disable adapter、secret vault／rotation verifier 未接線。因此 UI 只讀，所有 mutation RPC 拒絕；目前只有 Department 可批次核准的 engine 策略也較設計保守。
7. 舊員工名稱對 immutable id、確認後 UID／loop 配置與容量檢查、舊使用者邀請、Personal owner 重新登入綁定、部分套用資源 tombstone／清除等，都仍是必須實作的 product adapters。
8. 三個 Linux 發行版的原生／root 測試、完整 legacy planted authority 端到端遷移、gateway／CLI build、web build/tests/視覺驗收與正式 Daybreak 審查待做。

##### 下一位執行者的驗證指令

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-migration --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-import --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-auth otp::tests --offline
#### Linux 非 root，實際驗證缺少來源 fd 的拒絕（root 會在更早的身分檢查拒絕）
cargo test -p duduclaw-import --test linux_binary -- --nocapture
#### Linux root，可拋棄測試機
cargo test -p duduclaw-migration -- --ignored iso_root_
#### 遠端 builder：本地明確未跑
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-gateway migration_confirmation_methods_are_exact
cd web
npx tsc -b
npx vitest run src/pages/MigrationConfirmationsPage.test.tsx src/components/layout/nav-model.test.ts src/components/FirstRunGate.test.tsx
```

##### 依賴及契約

無 CONTRACT CHANGE。兩個新 workspace crates 使用 lockfile 已有的 rusqlite、serde/json、thiserror、sha2、hmac、chacha20poly1305、rand、rustix、libc、tempfile；沒有新增重型套件。SQLite worker 新增既有 rusqlite 的 hooks/limits/modern_sqlite feature。

##### 檔案清單

- `CHANGELOG.md`：未發行功能、認證與舊升級拒絕紀錄。
- `Cargo.lock`：登錄新 crates 的既有依賴。
- `Cargo.toml`：加入兩個新 workspace members。
- `crates/duduclaw-auth/src/db.rs`：setup token 用途／嘗試次數 schema 與舊 token 撤銷。
- `crates/duduclaw-auth/src/otp.rs`：原子 claim、期限／速率／用途區分及 18 項測試。
- `crates/duduclaw-cli/src/service/protected_linux_install.rs`：阻止缺失紀錄或舊版直接跨入 v1.80。
- `crates/duduclaw-gateway/Cargo.toml`：加入 migration engine 依賴。
- `crates/duduclaw-gateway/src/handlers/migration_confirmations_rpc.rs`：Admin／live session 唯讀清單及 mutation 拒絕。
- `crates/duduclaw-gateway/src/handlers/mod.rs`：攔截精確 migration RPC 名稱，禁止 extension 接管。
- `crates/duduclaw-import/Cargo.toml`：內容匯入 crate／binary 與 Linux 依賴。
- `crates/duduclaw-import/README.md`：fd 協定、支援 schema 與整合限制。
- `crates/duduclaw-import/src/lib.rs`：有界請求、內容類型與 contract JSONL 輸出。
- `crates/duduclaw-import/src/linux.rs`：來源 fd 與 worker 身分／NNP／dumpable 檢查。
- `crates/duduclaw-import/src/main.rs`：固定 fd 啟動、額外 fd 清理、非 Linux 拒絕。
- `crates/duduclaw-import/src/session-v1793.sql`：精確可信 sessions schema。
- `crates/duduclaw-import/src/sqlite.rs`：私有 DB/WAL 副本、SQLite 防禦與 schema 驗證。
- `crates/duduclaw-import/systemd/duduclaw-import@.service`：未安裝的 DynamicUser 無網路限制 profile。
- `crates/duduclaw-import/tests/content.rs`：內容-only／限額／未知輸入拒絕測試。
- `crates/duduclaw-import/tests/linux_binary.rs`：非 root Linux 缺 fd 子程序拒絕測試。
- `crates/duduclaw-migration/Cargo.toml`：遷移元件 crate 依賴。
- `crates/duduclaw-migration/README.md`：元件使用及尚未啟用界線。
- `crates/duduclaw-migration/src/archive.rs`：分塊 AEAD、結尾驗證與失敗後禁止 nonce 重用。
- `crates/duduclaw-migration/src/confirmation.rs`：原子核准、冪等套用／補償／輪替及容量限制。
- `crates/duduclaw-migration/src/confirmation_tests.rs`：17 項核准、故障、重試與秘密測試。
- `crates/duduclaw-migration/src/journal.rs`：durable intent／readback／receipt 及逐階段中斷測試。
- `crates/duduclaw-migration/src/lib.rs`：模組與不含秘密的錯誤類型。
- `crates/duduclaw-migration/src/snapshot.rs`：Linux no-follow 有界 walker、digest 與同檔案系統隔離。
- `docs/README.md`：新增架構文件索引。
- `docs/architecture/native-linux-isolation.md`：Package G 實作與缺口的自成段說明。
- `docs/features/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/features/ja-JP/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/features/zh-TW/05-security-defense.md`：說明已啟用的 setup token 修正與遷移只讀界線。
- `docs/guides/ja-JP/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `docs/guides/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `docs/guides/zh-TW/linux-protected-service.md`：說明舊安裝器拒絕跨版與尚未接線的遷移。
- `web/src/App.tsx`：註冊管理員遷移確認頁。
- `web/src/components/FirstRunGate.test.tsx`：零員工導向回歸測試。
- `web/src/components/FirstRunGate.tsx`：未建立員工時仍可開遷移與帳號安全頁。
- `web/src/components/layout/nav-model.test.ts`：兩種導覽可見性及 badge 測試。
- `web/src/components/layout/nav-model.ts`：兩種 edition 共用導覽加入 newIn 1.80.0。
- `web/src/i18n/en.json`：遷移頁與欄位名稱翻譯。
- `web/src/i18n/ja-JP.json`：遷移頁與欄位名稱翻譯。
- `web/src/i18n/zh-TW.json`：遷移頁與欄位名稱翻譯。
- `web/src/lib/migration-confirmations.ts`：typed RPC request 與選取規則。
- `web/src/pages/MigrationConfirmationsPage.test.tsx`：高風險／未就緒／遮罩／讀回失敗測試。
- `web/src/pages/MigrationConfirmationsPage.tsx`：純文字檢視／秘密遮罩／唯讀拒絕 UI。
- `wiki/reports/native-linux-isolation-package-g-2026-11-05.md`：交付證據、完整剩餘工作與遠端驗證指令。

## Package I1 — runtime integration (2026-11-05)

**Current handoff: see "Round 2" below. The following round-1 record is historical
and is superseded where round 2 describes changed behavior.**

### Round 1 — historical integration subset

Status: **PARTIAL — development wiring, not a runnable protected TEAM install.**
This worktree remains uncommitted. No gateway/CLI/workspace build, installation,
account mutation, migration or native-root test was performed in this session.
The existing subprocess fence remains fail-closed. There is no new local-backend
fallback. The remaining integration must not treat this diagnostic report or
small-crate tests as full-product acceptance.

### Composite diagnosis and operator access

`isolation_readiness::initialize` runs after boot edition resolution and on live
edition resolution, so Personal-to-TEAM activation also latches requirements.
It applies only to Linux `native-systemd` TEAM and enables both the existing
execution requirement and E's isolated-parser requirement. A later downgrade
cannot clear a latch. Other installations receive no new latch from this module;
the older core native-systemd fence is not weakened.

`system.status.isolation_readiness` reports eight fixed component names. B1's
root-owned, bounded, boot-bound and expiring `EvidenceReport` is read afresh.
All B1 primary checks must pass before launch-service, sandbox, egress and storage
are shown ready. Missing, malformed, stale or unreadable evidence yields
unavailable. D, E, F and runtime adapters remain `pending_integration`: neither
service liveness nor the presence of key files can substitute for their missing
trusted factories/readbacks. The composite therefore stays unready in this
subset. A once-only operator error and a non-dismissible dashboard banner explain
that protected employee processes cannot start. Admins can inspect fixed status
labels and refresh; no host paths, PIDs, secret bytes or raw errors are exposed.

This is **diagnosis, not a complete TEAM admission gate**. Missing
`ProtectedLaunch` still blocks protected subprocess launches; API/in-process work
needs the remaining explicit TEAM admission wiring. The dashboard stays running.
G's existing `/app/system/migration` route, Admin navigation with `newIn: 1.80.0`,
and FirstRunGate exception were verified by source inspection and retained;
this does not establish migration apply/readback acceptance.

### B1 to H storage adapter

`storage::LoopStorage::open(storage_directory, journal_directory)` now implements
B1's storage trait using H's `EmployeeStorage`. It is ready for trusted root
composition; `Components::default()` deliberately still refuses storage because
F/G have not supplied installed anchors and the other production adapters.
The root-local journal binds each approved operation ID to its full proposal
digest, employee, immutable UID and quota. It preserves employee/UID tombstones,
and fsyncs a fresh run/generation attachment intent before H attachment. Only
private, noexec detached home/work mounts leave this adapter. H's new read-only
`images()` inventory compares actual mounts against image/device identity.
Unknown images, missing Ready images, unleased mounts, incomplete maintenance
and invalid/lost journals cannot become readiness success.

A stale receipt cannot detach a newer run. An attach intent that never mounted
has no observed device (0:0); it is accepted only by cleanup identity checks,
never by launch/device checks. B1 still proves unit inactivity, empty cgroup and
absence of employee-UID processes before calling detach. No account allocation
or proposal approval is implemented by this adapter.

H usage requires an attached image. Idle preflight usage therefore uses a
journaled maintenance attach/statvfs/detach cycle, not cached usage or `du`.
Interrupted temporary mounts of completed images can be recovered with the same
approved operation. H Creating/Quarantined images do **not** automatically resume
formatting; they require explicitly authorized destruction or operator recovery.
Readiness stays false until resolved. The production factory and G's approved
employee allocation RPC still need to call this adapter.

### Landlock policy and design §15 compatibility note

The requested I1 policy selects `min(detected ABI, 7)`. ABI 0, an unavailable
Landlock API and any failed enforcement still refuse execution. ABI >7 produces
exactly the known ABI-7 filesystem rights; no new rights are invented or claimed
as handled. Persistent socket/FIFO/device creation stays denied. `restrict`
re-probes the kernel and compares the resulting policy before applying the
hard-required ruleset. This follows the kernel's explicit handled-rights
[forward/backward compatibility model](https://docs.kernel.org/userspace-api/landlock.html).

The probe exposes both `landlock_abi` (actual kernel evidence) and
`landlock_ruleset_abi` (selected policy), logging both once per process to stderr.
During a launch this is the bounded employee stderr pipe, not a B1 boot journal;
B1 still needs to publish/log the probe evidence through its readiness adapter.
The unchanged 12-byte privilege receipt continues to carry 1–7, now explicitly
the selected ruleset ABI. It precedes enforcement and cannot establish exec or
full isolation. No shared wire layout or capability canonical bytes changed.

For the design §15 table: Linux 5.15/6.1/6.8 keep their detected ABI 1/2/4
policies; a kernel reporting ABI 8 or later selects ABI 7. The external design
file was not edited because this assignment restricts writes to this worktree.
The handoff described this policy as pending owner confirmation; implementing
it here is not owner release approval. Historical ABI-8 failures in the earlier
acceptance table remain historical facts; the modified code requires re-testing.

### Remaining work, in dependency order

1. F/G verified root publication, installed anchors, account-lockdown evidence
   and B1 production `Components` composition. Connect `LoopStorage` only with
   those trusted installation inputs; no default paths or successful stubs.
2. Reconcile B1's sealed `BootstrapPlan` with B2's `RootRegistry/TrustedRun`:
   descriptor order, non-PTY stdin contract, executable/mount identities,
   digest-bound runtime and forwarder seccomp profiles, forwarder ELF and
   root tmpfs/devpts policy. B2's binary still uses `UnavailableRegistry`.
3. Use C's authenticated `EgressController` for root-selected run registrations,
   revocation and DNS evidence; carry B2 probe/kernel ABI evidence into B1's
   trusted report and preserve cancellation/readback barriers.
4. Construct gateway `SpawndBackend` with full composite admission, D live
   authority/key material/metadata/resource resolvers and `ProtectedLaunch`;
   inject real `ExecutionPlanFactory`, worker plans and a host-budgeted
   `WorkerLauncher` with proven retirement. Update every CLI/runtime's published
   MCP projection and verify inherited socketpair fd behavior. Do not invent
   factories that claim all tools are supported without execution.
5. Complete E/F/D live readiness, including updater loaded generation and worker
   unit/retirement evidence. Apply the composite gate to API/in-process TEAM
   execution too. The diagnostic-only module must not serve as authorization.
6. Wire G's F/B1/H platform and authority adapters: only Admin-confirmed proposals
   allocate UIDs/disks, with operation/digest-bound apply, readback and compensation.
   Existing migration UI reachability is not completion of those adapters.
7. Add and execute genuine `iso_root_e2e_*` tests for the full A→B1→systemd→B2
   chain, real MCP tool call through D, same-employee queued D8 admission with a
   double-fork leftover, `WouldDeadlock`, and real SSH public-key/su/user-manager/
   crontab/at failures. No placeholder E2E test was added: the production chain
   needed to exercise it is still missing.
8. Remote gateway/CLI compilation, frontend type-check/tests, the three Linux
   distributions (including ABI 8+), native Windows/macOS regression where
   applicable, and formal Daybreak security review remain required.

### Validation and acceptance handoff

Actual commands and counts are recorded below. Cross-target
checking is compilation/type checking, never native execution. No tests matching
zero cases count as a test pass.

Remote gateway/frontend checks (not run locally):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway isolation_readiness::tests
cd web
npx tsc -b
npx vitest run src/components/IsolationReadinessBanner.test.tsx
```

There are 5 new gateway cases and 7 UI cases. `web/node_modules` is absent in
this worktree, so frontend dependencies were not installed. Local build limits
prohibit compiling gateway/CLI; the above remains an explicit unverified gate.

Root builder commands, only on a disposable Linux VM:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-run --features root-test-fixture --offline -- --ignored iso_root_ --test-threads=1
unshare --mount --propagation private env DUDUCLAW_ISO_DISPOSABLE=1 ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test cargo test -p duduclaw-spawnd --offline -- --ignored iso_root_storage_adapter_reopen_generation_and_proposal_binding --test-threads=1
```

Reuse the existing acceptance `build.sh`/`push.sh` artifact inventory and
`runall.sh`: one test process per `--ignored --exact <name> --test-threads=1`,
retaining its exit status and requiring **exactly one passed case**. Include B2's
new `iso_root_landlock_probe_reports_detected_and_selected_abi` library test as
well as all 16 existing fixture tests. The new B1 storage root test also needs
`unshare --mount --propagation private`, which the old `runall.sh` automatically
uses only for package H: extend its package/name condition or run the B1 test
with the explicit command above. Do not run it with shared host propagation.
These are package tests; they are not the requested full-chain E2E suite.

### Actual I1 checks

All Cargo commands below used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`.

| Exact command (after the two environment assignments) | Observed result |
| --- | --- |
| `cargo test -p duduclaw-run --offline` | 19 passed, 0 failed; 16 root cases ignored on macOS |
| `cargo check --tests --features root-test-fixture --target x86_64-unknown-linux-gnu -p duduclaw-run --offline` | Passed; Linux-only tests and fixture type-checked, not executed |
| `cargo test -p duduclaw-isolation --offline` | 32 passed, 0 failed; shared canonical/wire contract regressions |
| `cargo test -p duduclaw-spawnd storage_adapter --offline` | Intermediate: 3 passed, 0 failed |
| `cargo test -p duduclaw-spawnd storage --offline` | Final storage subset: 6 passed, 0 failed, 67 filtered |
| `cargo test -p duduclaw-isolation-storage --offline` | 17 passed, 0 failed |
| `cargo check -p duduclaw-spawnd --tests --target x86_64-unknown-linux-gnu --offline` | Passed; includes new real-loop root test type checking |
| `cargo check --tests --features duduclaw-run/root-test-fixture --target x86_64-unknown-linux-gnu -p duduclaw-run -p duduclaw-spawnd -p duduclaw-isolation-storage --offline` | Final combined Linux type check passed; unsafe-code warnings remain, including adapter syscall sites |
| `cargo check --tests --target x86_64-pc-windows-gnu -p duduclaw-run -p duduclaw-spawnd -p duduclaw-isolation-storage --offline` | Passed; Windows type check only, not native execution; deprecated/dead-code warnings remain |
| `cargo metadata --no-deps --format-version 1 --offline` | Passed; metadata only, not a gateway build |
| `git diff --check` | Passed |

The four final macOS test selections total **74 passed, 0 failed**; the earlier
3-case filter is a subset and is not counted twice. Locale JSON parsing and all
14 new keys in each of the three languages were checked. Independent static
review covered B2, gateway/web and B1/H. The live-edition latch, misleading
privilege-receipt wording, fabricated unknown-device sentinel and invalid-input
journal poisoning findings were corrected. No unresolved Critical/High/Medium
was reported in that code review. It was **not** formal Daybreak review.

**CONTRACT CHANGE:** none to `duduclaw-isolation` wire formats, canonical bytes or
hard limits. Additive internal APIs are H's `ImageStatus`/`EmployeeStorage::images`,
B1's cleanup-only `Attachment::check_identity`, and B2's selected-ABI readiness
field. Gateway status gains an optional diagnostic payload. Dependency edges
reuse existing workspace crates; no new third-party dependency or version.

### I1 file-by-file handoff

| File | Change |
| --- | --- |
| `Cargo.lock` | Record gateway→spawnd and spawnd→storage workspace dependency edges. |
| `crates/duduclaw-gateway/Cargo.toml` | Linux-only spawnd evidence dependency. |
| `crates/duduclaw-gateway/src/isolation_readiness.rs` | New fail-closed diagnostic snapshot, narrow-only latches and 5 tests. |
| `crates/duduclaw-gateway/src/server.rs` | Initialize protected TEAM requirements after boot edition resolution. |
| `crates/duduclaw-gateway/src/handlers/lifecycle.rs` | Also initialize after live edition promotion; never clear latches. |
| `crates/duduclaw-gateway/src/handlers/system_status.rs` | Add operator-safe optional isolation status. |
| `crates/duduclaw-gateway/src/lib.rs` | Export diagnostic module. |
| `crates/duduclaw-isolation-storage/src/lib.rs` | Add refusing-by-default root-local image inventory API. |
| `crates/duduclaw-isolation-storage/src/linux.rs` | Implement verified image/mount inventory and extend root assertions. |
| `crates/duduclaw-isolation-storage/README.md` | Explain inventory and B1 responsibilities. |
| `crates/duduclaw-spawnd/Cargo.toml` | Reuse H workspace crate. |
| `crates/duduclaw-spawnd/src/storage.rs` | Export real adapter and distinguish cleanup identity from launch device proof. |
| `crates/duduclaw-spawnd/src/storage/adapter.rs` | New durable B1→H adapter, safe detached mounts and ignored root lifecycle test. |
| `crates/duduclaw-spawnd/src/storage/adapter_state.rs` | New bounded operation/run journal reconciliation and portable tests. |
| `crates/duduclaw-spawnd/src/host.rs` | Permit cleanup of an identity-bound attach intent without inventing a device. |
| `crates/duduclaw-spawnd/README.md` | Adapter prerequisites, failure/recovery boundaries and root acceptance instructions. |
| `crates/duduclaw-run/src/landlock.rs` | Clamp newer detected ABIs to exact known ABI-7 policy and test rights. |
| `crates/duduclaw-run/src/linux/probe.rs` | Preserve detected/selected ABI evidence, once-per-process logging and tests. |
| `crates/duduclaw-run/src/linux/runner.rs` | Carry selected ABI into the existing bounded privilege receipt. |
| `crates/duduclaw-run/src/status.rs` | Clarify that the receipt is not enforcement/exec success. |
| `crates/duduclaw-run/README.md` | Current ABI compatibility, evidence and integration limits. |
| `web/src/components/IsolationReadinessBanner.tsx` | Persistent warning, Admin details and refresh. |
| `web/src/components/IsolationReadinessBanner.test.tsx` | Seven presentation/state regressions, not run locally. |
| `web/src/components/layout/MainLayout.tsx` | Mount the warning in the dashboard shell. |
| `web/src/lib/api.ts` | Optional typed composite status. |
| `web/src/i18n/zh-TW.json` | Traditional Chinese operator wording. |
| `web/src/i18n/en.json` | Matching English wording. |
| `web/src/i18n/ja-JP.json` | Matching Japanese wording. |
| `docs/features/05-security-defense.md` | Enabled diagnosis/parser latch and remaining admission boundaries. |
| `docs/features/zh-TW/05-security-defense.md` | Matching Traditional Chinese documentation. |
| `docs/features/ja-JP/05-security-defense.md` | Matching Japanese documentation. |
| `docs/architecture/native-linux-isolation.md` | This evidence-bounded handoff and corrected ABI history/current policy. |
| `CHANGELOG.md` | Self-contained unreleased diagnosis and Landlock compatibility entries. |

### Round 2 — historical integration handoff (2026-11-05)

**PARTIAL / NOT READY FOR DEPLOYMENT.** Work remains uncommitted in the assigned
worktree. This round changes TEAM admission from diagnostics into refusal at
execution entry points, and adds real transport/lifecycle implementations. It
still does not produce a runnable protected TEAM installation: the authoritative
installation and runtime producers below are missing. No protected employee was
executed here. No gateway/CLI/workspace build, root test, deployment, customer
acceptance or formal Daybreak review was performed.

| Requested priority | Current implementation | Remaining closure |
| --- | --- | --- |
| Gateway A backend and B1→B2 | Boot connects one `SpawndBackend::connect_with_admission`, installs it as the protected selection; production Host uses `NativeTrampoline`; the binary consumes sealed plans through `SealedRegistry`. | Installed root publication, runtime profiles, binary/namespace/Landlock installation evidence and root anchors are absent. Native B1 PTY remains refused. |
| D/A/E boot and runtime shim | Concrete `ProtectedMcpLaunch`; boot owner assembles Boundary, A execution plans, E services and binary-owned CLI dispatcher when all trusted inputs exist. | No production `BootSources` producer, fresh component-readback producer, root CLI projection/inheritance acceptance producer or runtime call-site `prepare_launch`/`with_context` wiring. |
| Composite TEAM admission | API and subprocess entry points refuse unless every component is ready; retained A backend handles and model/tool-loop rounds recheck. F now has a real expiring health probe. | D/E/runtime-adapter states intentionally remain pending; constructed services are not readiness evidence. |
| F/G adapters | Dashboard update path verified; new F authenticated readiness. G confirmed allocation goes through the B1 lifecycle owner to account/H operations and readback. | Missing installation evidence and unprovisioned H anchors still refuse real allocation; new-employee product creation and complete generation-activation endpoints remain absent. |
| `iso_root_e2e_*` | Added real sealed transport and B2 fixture tests, plus a G durable binding/compensation fixture. | None executed here. These are narrower than the requested full A→B1→systemd→B2/D-tool/D8/real-login negative suite, which remains unwritten. |

#### Execution, admission and boot ownership

`isolation_readiness::connect_backend` connects at trusted boot even while another
component is unready. `selection` holds one backend; protected `select` and the
command adapter no longer create unrelated per-run connections. The boot-only
accessor permits composition before readiness, but the installed backend itself
checks the callback before/after reservation, before spawn, after awaited input
preparation and after StartRun. A late failed check closes the control connection
so B1 cancels its runs. Cleanup is not blocked by readiness.

A retains the actual authenticated reservation expiry, employee, run and random
control-connection identity, plus the validated TakeIo receipt. D uses these
rather than synthesized IDs. `ProtectedMcpLaunch` is single-use: trusted
policy/grant, root-verified runtime projection, issue, actual MCP socket delivery,
activation and revocation. Error, explicit revoke and Drop revoke the capability
and abort the connection task. Native argv/env are never converted into root
policy or accepted as runtime publication.

The gateway now calls `isolation_mcp::boot_protected` after backend connection.
The binary registers a concrete dispatcher factory before `cmd_run_server` starts
the gateway. It uses the existing internal-key authenticator inside the gateway,
retains the original scope checks and requires complete dispatcher services; no
key is placed in runtime files/fds and no principal is manufactured from a slug.
The owner retains Boundary/sweep and E services. `prepare_launch` re-reads G's
confirmed root mapping and D's durable binding before creating the launch.
`isolation-worker-integration` is enabled in gateway defaults; no E engine runs
inside gateway. Missing producers are logged explicitly, and boot construction
never flips composite readiness to true. This is composition support, not a
claim that runtime entry points already scope every call through it.

The process-wide TEAM gate is checked before human-context shortcuts in
`autonomous_runtime`, at `run_agent_prompt_inner`, the Claude runner, MoA and the
operator direct tool-loop. RecordingProvider, ReasoningTap and LocalChatProvider
check again for each complete/stream call; the SSE fallback rechecks too. These
checks return a nonretryable configuration/request refusal. Existing explicit
Admin maintenance/model-probe paths retain their existing separate authority.
Non-protected installations and non-Linux platforms receive no new TEAM latch;
LocalBackend behavior remains unchanged. A live TEAM promotion latches refusal;
a restart is still needed to obtain missing boot composition inputs.

#### B1 production adapter and B2 final isolation receipt

`native_trampoline::NativeTrampoline` is now the production Host's adapter. It
accepts only a root `Publication::native_profile` (not deserializable gateway
policy), verifies the existing sealed BootstrapPlan bytes, and converts actual
mount/executable observations into B2's exact descriptor order:

`home, work, projection, runtime, etc, ELF, [script], prompt, attachments,
[stdin], [MCP], [egress, forwarder ELF]`.

The optional forwarder is opened relative to the detached runtime mount and
hash-verified. Runtime/forwarder seccomp allowlists and digests, snapshot mount
identity, tmpfs limits and proc policy must come from the verified publication.
`bind_unit` validates the checked StartedUnit/run association and cgroup, receives
the root-authenticated fresh challenge, checks cancellation, then sends the
nonce/run-bound root memfd plus rights. The unit barrier is not released before
the monitor owns the cgroup. The sender drops its copies after handoff. Failed
start/transport still follows B1's existing revoke/kill/D8 cleanup path.

B2's production binary no longer uses `UnavailableRegistry`. `SealedRegistry`
consumes one offer, verifies root ownership, bounded regular tmpfs file, seals,
nonce/run/domain and trusted-plan validation; no pathname or environment override
is accepted. Malformed first offers cannot retry. Non-PTY stdin is now an exact
read-only CLOEXEC pipe; PTY+stdin is refused. The private fd contract remains
bounded by 80 bootstrap descriptors and the original 72-source shuffle limit.

A CLOEXEC notification pipe plus shared atomic final-stage bits separates
isolation completion from a mere fork or pipe EOF. Runtime and forwarder set their
bits only after Landlock, final fd readback and final seccomp installation. EOF
without those bits, or any recorded failure, refuses. B1 needs **all four**
EmployeeReady, SupervisorReady, IsolationReady and ChildrenForked receipts before
Running; ChildrenForked follows init's dedicated filter. Duplicate/error/unexpected
PTY receipts refuse. IsolationReady is not proof that exec succeeded or that the
process remains alive. Terminal status always takes precedence when drained.
An already-exited short runtime preserves its actual exit result without issuing
a readiness receipt; an early dead forwarder fails the run.

The native B1 adapter currently refuses PTY: returned-master provenance and
actual PTY activity tracking still need their production integration. It does
not allocate a host PTY or invent activity each poll. Non-PTY activity timestamps
use one shared monotonic epoch clock with the coordinator and are captured during
preparation, avoiding boot-relative/epoch confusion and future asynchronous
receipt timestamps.

Landlock probe logging was moved out of the employee stderr pipe. `probe()` only
returns actual detected/selected evidence. A trusted trampoline readiness
producer must provide both ABIs; B1 emits them once to daemon stderr and
publishes `EvidenceReport.landlock`. The supplied service currently discards
stderr; a bounded retained daemon-log destination remains an installer task. Claimed trampoline readiness without valid
ABI evidence now refuses; selected ABI must equal `min(detected, 7)`. The actual
installation/self-test producer is still missing, so the default does not report
this evidence as ready. The round-1 stderr behavior above is historical.

#### F and confirmed G allocation

F's private probe uses the existing fixed root socket, fresh nonce, strict schema,
peer credentials and no ancillary rights. It refuses work-in-progress, poisoned,
maintenance, pending or resume-required state and verifies the running updater
slot plus signed committed manifest/current selector. Full artifact verification
occurs during clean recovery; the once-per-second probe avoids rehashing up to
8 GiB under the service lock. The root-only `verified_committed_generation` API
can re-verify all artifact bytes, but its release manifest is not a substitute
for NSS/SSH evidence or B1 runtime/mount policy.

Gateway probes in one background thread. Evidence expires two seconds after the
RPC **started**, not after it completed; failure clears the cache immediately.
Admission does not block on F's five-second socket timeout. Version and the entire
protocol vector are checked. B1 does not yet publish a loaded generation digest,
so cross-component generation equality remains unverified. The existing Admin
`system.apply_update`/status dashboard client was checked; only Committed is
success. No update was requested or installed in this round.

G's private allocation protocol binds operation ID, full normalized proposal
digest, label and quota. Root generates a random immutable EmployeeId before
account effects; it never hashes a label. The same B1 lifecycle worker owns the
allocator, RegistryStore flock and H provider. Only Applying confirmations can
allocate; Pending/Approved payloads refuse. Successful retries return the same
identity; altered bindings refuse. Every partially created UID/reservation is
added to the independent monitor before replying, including failure cases.

Compensation persists Disabling, permanently tombstones the UID, proves no
remaining process, destroys/readbacks H state, then persists Disabled. A proven
never-provisioned image is a no-op only after complete inventory. Disabled
operations cannot resurrect UIDs. A failed employee retry completes compensation
and asks for a new proposal; a new operation may reuse the product label with a
new EmployeeId/UID. This is deliberately narrower than transparent same-operation
recovery and is a remaining product limitation.

The dashboard confirmation adapter currently supports employees already written
through the normal product flow, with complete settings exactly equal to the
live registry config and live Admin access. It does not copy imported authority
into a fresh employee. The confirmed mapping is stored separately and resolution
requires an Applied confirmation plus live root ReadBack. It proves employee/UID/
quota only, not D principal/session/policy revisions. New-employee creation and
partial imported-setting re-entry are not integrated.

#### Remaining work before any runnable/acceptance claim

1. Root installer must provision the fixed H image/journal anchors described below;
   provide actual UID-range/no-existing-owner/SSH public-key and forwarding
   evidence, not config booleans. Missing anchors and account evidence still deny.
2. F/G must publish the root runtime/entitlement registry, mount anchors,
   architecture-specific seccomp/forwarder profiles and actual trampoline
   installation/self-test evidence. F's current signed artifact list alone does
   not encode these policies. The C DNS readiness producer is still missing.
3. Supply gateway BootSources from live principal/session/binding/revision,
   trusted execution plans, root runtime projection and acceptance receipts,
   E host input budgets plus proven worker cgroup retirement, turn authority and
   fresh composite readbacks. Merely opening E's socket is insufficient.
4. Wire each runtime call site to `prepare_launch` and execution context; publish
   Claude/Codex/Grok/generic/openai-compat configs with D's fixed shim and prove
   fd-3 inheritance. Appending arguments inside A does not modify B1's fixed
   root argv because StartRun intentionally carries no argv/env.
5. Implement/validate native B1 PTY, complete G fresh employee creation/recovery,
   and close F's drain/prepare/apply/readback/resume generation endpoints.
6. Write and run the full gateway A→B1→systemd→B2 suite with a real D tool call,
   D8 second-run admission including a double-fork leftover, same-employee
   WouldDeadlock and actual ssh public-key/ssh forwarding/su/user-manager/cron/at
   rejection. Do not relabel the narrow fixtures below as that acceptance.
7. Compile/test gateway and CLI remotely; run frontend checks when dependencies
   exist; run root fixtures on disposable Ubuntu 22.04/24.04 and Debian 12,
   including newer Landlock ABI. Formal Daybreak review remains a separate gate.

#### Round-2 validation and remote filters

Local commands use `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`. Cross checks do not
link or execute Linux/Windows code; zero-test binaries/doc-tests are not counted.

| Command / selection | Observed result |
| --- | --- |
| `cargo test -p duduclaw-spawnd -p duduclaw-run -p duduclaw-execution -p duduclaw-isolation --features duduclaw-run/root-test-fixture --offline` | macOS: 73 + 20 + 32 + 32 = **157 passed**, 0 failed; root fixture tests ignored |
| `cargo test -p duduclaw-mcp-boundary --features execution-integration --offline` | macOS: **59 passed** (58 library + 1 bridge); Linux lifecycle not executed |
| `cargo test --offline -p duduclaw-updated -p duduclaw-migration --lib` | macOS: **35 + 41 passed**, 0 failed; one hardware-key fixture ignored |
| Combined Linux check of execution, mcp-boundary, spawnd, run, updated, migration with all targets, binding-store/execution-integration/root-test-fixture features | Passed (final 3.89 s); exact command below. |
| `cargo check -p duduclaw-run -p duduclaw-spawnd -p duduclaw-execution -p duduclaw-migration -p duduclaw-updated --tests --target x86_64-pc-windows-gnu --offline` with the same SQLite metadata workaround | Passed (11.26 s); type check only, no Windows execution/linking. |
| Standalone boot harness including actual new boot module, boundary service and E adapter, with unrelated gateway dependencies stubbed | Linux check including tests and no-default-feature check passed; not a gateway/CLI package build. |
| Standalone readiness harness including the actual readiness module, with only core latch names stubbed on macOS | **7 passed**, 0 failed; verifies pure admission/expiry logic, not actual gateway entry points. |

The non-overlapping real-crate local suites above total **292 passed, 0 failed**.
The additional seven pure readiness-harness passes are reported separately.
The final B2 rerun had 20 passed and 19 ignored root fixture tests. The earlier
round-1 checks and repeated subsets are not added again. The new spawnd→migration
edge pulls existing bundled SQLite into the graph: the first Linux cross check
failed because `x86_64-linux-gnu-gcc` is absent. Subsequent **type checks only** used
`LIBSQLITE3_SYS_USE_PKG_CONFIG=1 PKG_CONFIG_ALLOW_CROSS=1`; this is not evidence that
the resulting Linux/Windows binaries link. No new third-party version was added.

```sh
LIBSQLITE3_SYS_USE_PKG_CONFIG=1 PKG_CONFIG_ALLOW_CROSS=1 CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-execution -p duduclaw-mcp-boundary -p duduclaw-spawnd -p duduclaw-run -p duduclaw-updated -p duduclaw-migration --all-targets --features duduclaw-run/root-test-fixture,duduclaw-mcp-boundary/execution-integration,duduclaw-mcp-boundary/binding-store --target x86_64-unknown-linux-gnu --offline
```

Remote gateway/CLI checks (not executed locally):

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo check -p duduclaw-gateway -p duduclaw-cli --tests
cargo test -p duduclaw-gateway isolation_readiness::tests -- --test-threads=1
cargo test -p duduclaw-gateway isolation_mcp::boot::tests -- --test-threads=1
cargo test -p duduclaw-gateway isolation_execution -- --test-threads=1
cargo test -p duduclaw-gateway migration_authority -- --test-threads=1
cargo test -p duduclaw-gateway protected_update -- --test-threads=1
cargo test -p duduclaw-cli mcp_isolation -- --test-threads=1
cargo test -p duduclaw-execution reservation_metadata_
cargo test -p duduclaw-execution mcp_receipt_requires_actual_delivery_and_live_control
cargo test -p duduclaw-execution retained_backend_checks_admission_before_connecting_to_root
cargo test -p duduclaw-spawnd native_trampoline::tests
cargo test -p duduclaw-run eof_without_both_final_isolation_receipts_never_admits
cargo test -p duduclaw-run sealed_transport_test_plan_obeys_production_policy_validation
cargo test -p duduclaw-migration allocation::tests
cargo test -p duduclaw-updated readiness::tests
cargo test -p duduclaw-mcp-boundary --features binding-store,execution-integration
```

Check that each requested selection matches tests; counts are not inferred from
exit zero. The admission wrappers also require actual API/CLI-path tests on the
remote gateway, not only the pure component-state tests above.

On a disposable root Linux acceptance machine:

```sh
cargo test -p duduclaw-run --lib iso_root_e2e_sealed_registry -- --ignored --test-threads=1
cargo test -p duduclaw-run --features root-test-fixture --test root iso_root_e2e_sealed_bootstrap -- --ignored --test-threads=1
cargo test -p duduclaw-run --features root-test-fixture --test root iso_root_full_launch -- --ignored --test-threads=1
cargo test -p duduclaw-spawnd iso_root_e2e_confirmed_allocation_binding_restart_and_compensation -- --ignored --test-threads=1
unshare --mount --propagation private env DUDUCLAW_ISO_DISPOSABLE=1 ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test cargo test -p duduclaw-spawnd iso_root_storage_adapter_reopen_generation_and_proposal_binding -- --ignored --test-threads=1
```

The sealed-registry filter has three real handshake/descriptor tests. The sealed
bootstrap filter has three real B2 employee uid/namespace/fd, teardown and fast-exit
fixtures. Including G, there are seven newly added e2e-named root tests.
Existing full-launch scenarios now all use the production sealed registry,
including expected exec failure. The G e2e-named fixture uses real root journal/
registry persistence but **injected account/storage effects**. None proves
production systemd/D8 or a real D tool call. Reuse the acceptance scripts'
artifact inventory and one-process-per-exact-test convention; for B1/H storage
add the explicit private mount namespace shown above. Do not run storage tests
with shared host propagation or on a non-disposable host.

`web/node_modules` is absent: no dependency installation, tsc or vitest was run.
Three locale JSON files parse; all 14 readiness keys match and are nonempty.
Remote frontend commands remain `npx tsc -b` and
`npx vitest run src/components/IsolationReadinessBanner.test.tsx` in `web`.

#### Round-2 file map

| Files | Change |
| --- | --- |
| `execution/src/selection.rs`, `command.rs` | Shared boot-installed protected backend and no per-run reconnect/fallback. |
| `execution/src/spawnd.rs`, `spawnd/transport.rs` | Actual reservation/control/MCP metadata; retained-handle admission checks and tests. |
| `mcp-boundary/src/protected_launch.rs`, `lib.rs`, `Cargo.toml` | Concrete A/D lifecycle behind execution-integration. |
| `gateway/src/isolation_readiness.rs`, `server.rs`, `Cargo.toml` | Composite gate, A boot connection, F background evidence and E integration feature. |
| `gateway/src/autonomous_runtime.rs`, `runtime_dispatch.rs`, `claude_runner.rs`, `direct_api.rs`, `channel_reply/direct_api.rs`, `thinking/llm_tap.rs`, `local_llm.rs` | Initial and per-round API/tool-loop TEAM admission. |
| `gateway/src/isolation_mcp.rs`, `isolation_mcp/boot.rs` | Boot owner, explicit missing sources, assembly and confirmed launch helper. |
| `cli/src/mcp_isolation_boot.rs`, `lib.rs` | Existing authenticated CLI dispatcher factory registered before server boot. |
| `run/src/linux/sealed_registry.rs`, `sealed_registry_root_tests.rs`, `bootstrap.rs`, `main.rs`, `linux/mod.rs` | Production one-shot sealed plan transport and root tests. |
| `run/src/linux/runner.rs`, `fd.rs`, `mounts.rs`, `fd_plan.rs`, `seccomp.rs`, `status.rs` | Trusted-plan serialization, non-PTY stdin, final isolation barrier and exact fd preservation. |
| `run/src/linux/probe.rs`, `spawnd/src/bootstrap.rs`, `host_state.rs`, `host.rs` | ABI evidence belongs to root readiness/logging rather than employee output. |
| `run/src/bin/root-fixture.rs`, `root-fixture/launch.rs`, `run/tests/root.rs` | Actual fixture launches use production sealed transport and assert isolation/terminal boundaries. |
| `spawnd/src/native_trampoline.rs`, `bootstrap_mounts.rs`, `bootstrap.rs`, `host.rs`, `lib.rs`, `main.rs`, `server.rs` | B1/B2 adapter, typed root profile seam, cancellation/stage checks and shared clock. |
| `updated/src/readiness.rs`, `client.rs`, `linux/service.rs`, `lib.rs` | Authenticated private health protocol and verified generation API. |
| `migration/src/allocation.rs`, `allocation/transport.rs`, `confirmation.rs`, `lib.rs` | Bound allocation protocol, client, receipt checks and safe failed-employee retry behavior. |
| `spawnd/src/allocation.rs`, `admission.rs`, `host_worker.rs`, `host.rs`, `lib.rs`, `storage/adapter.rs` | Single-owner root allocator, durable identity/compensation and proven absent-image no-op. |
| `gateway/src/handlers/migration_authority_employee.rs`, `migration_authority.rs`, `migration_confirmations_rpc.rs`, `mod.rs` | Confirmed employee adapter and read-only root-verified mapping resolver. |
| `spawnd/Cargo.toml`, `Cargo.lock` | Existing run/migration dependency edges; SQLite cross-compilation limitation above. |
| `run/README.md`, `spawnd/README.md`, security feature docs in all three languages, `CHANGELOG.md`, three locale files | Actual gate/transport behavior, operator wording and explicit remaining limits. |

All abbreviated crate paths above are below `crates/duduclaw-.../`. Round-1 files
not changed again remain described in the preceding table. Review found and
corrected retained-backend admission bypass, EOF false readiness, incomplete
init-stage evidence, fast-exit outcome loss, clock-domain mismatch, G compensation/
monitor/retry issues and API-only MoA/tool-loop omissions. This is ordinary
independent code review, **not** the required formal Daybreak security sign-off.

**CONTRACT CHANGE:** no change to `duduclaw-isolation` wire formats, capability
canonical bytes or hard limits. New protocols are private F health and G/B1
allocation; B1/B2 adds a sealed root manifest descriptor and private status kind 7
(`IsolationReady`). Internal root readiness adds actual/selected ABI evidence;
missing evidence refuses. No commit, merge, release or installation occurred.

Evidence logs retained locally for this round: `/tmp/iso-i1-r2-final-local-tests.log`,
`/tmp/iso-i1-r2-run-final-tests.log`, `/tmp/iso-i1-r2-final-linux-check.log`,
`/tmp/iso-i1-r2-final-windows-check.log`, `/tmp/iso-i1-r2-boundary-test.log`,
`/tmp/iso-i1-r2-fg-tests.log`, `/tmp/iso-i1-r2-boot-typecheck-tests.log`, and
`/tmp/iso-i1-r2-readiness-harness.log`. The harness sources are under
`/tmp/iso-i1-r2-boot-check` and `/tmp/iso-i1-r2-readiness-check`; their stubs are
explicit and must not be mistaken for production implementations. `git diff
--check` passed after final integration.

Final production storage closure: `main` now uses `Components::installed()`, which
attempts the real H adapter at fixed `/var/lib/duduclaw-spawnd/images` and
`/var/lib/duduclaw-spawnd/storage`. This resolves the earlier unspecified-layout
seam; it does not create/mount anchors or allocate an employee disk at startup.
Both anchors must already be root:root 0700; H still requires private propagation,
supported backing, capacity reserve and complete inventory. Opening may create
only the existing adapters' lock/metadata files. Failure retains the refusing
UnavailableStorage implementation. The remaining installer, account evidence and
runtime publication gates are unchanged. Independent review passed, and final
Linux spawnd all-targets type-check passed (`/tmp/iso-i1-r2-installed-storage-check.log`).


### Round 3 — historical integration handoff (2026-11-05)

**PARTIAL / NOT READY: the requested protected Claude employee turn has not been
implemented or demonstrated end to end.** This round supplies real installed
publication consumers, post-StartRun MCP projection receipts and worker retirement.
It does not fill missing authority with a synthetic principal, fixed policy
revision or permissive tool factory. Consequently protected TEAM admission still
refuses work, even if all service units exist. Rounds 1–3 remain uncommitted in the
assigned worktree. No gateway/CLI/workspace build was run locally.

#### Ordered progress and remaining work

| Requested stage | Round-3 change | What still prevents completion |
| --- | --- | --- |
| Root publication / BootSources | Production B1 uses `InstalledPublication` and measured installation receipt consumers. Boot can obtain the runtime registry from F's verified generation and installs `RetiringLauncher`. | No installer/signing harness currently produces the required policy artifact, account/kernel/DNS receipts and exact-runtime MCP acceptance transcripts. Boot still lacks live authority, durable bindings, execution/worker plans and turn authority. |
| Runtime contexts and D shim | `InstalledRuntimeRegistry` validates current root publication and actual per-run mount receipt. `ProtectedMcpLaunch` defers mount-receipt validation until after TakeIo; Claude uses `/config/mcp.json`. | No CLI runtime call site yet calls `prepare_launch` / `with_context`. Claude/OpenAI RunConfig publication is supported; Codex/Grok home/project overlays and generic Registered runtimes refuse. No actual runtime's fd-3 inheritance or exclusive config was tested. |
| E retirement | A host-wide dirty lease, authenticated worker pidfd, exact systemd invocation and cgroup retirement hold the input/memory slot through response and cleanup. | Root execution of the new worker test, a linked Linux parser-engine binary, installation and crash reconciliation remain outstanding; engine all-targets cross type-check now passes. Construction is not readiness evidence. |
| PTY | Inspected B1/B2/A ownership and idle-timeout paths; retained refusal. | Need authenticated returned-master provenance and an activity mechanism that observes real bytes without stealing them from A. NativeTrampoline still refuses PTY. |
| Full root e2e | Added narrower publication/retirement checks; commands below distinguish them. | The full A→B1→systemd→B2 employee UID→Claude MCP child→D real-tool test and D8 double-fork leftover test remain unwritten/unexecuted. A real login/scheduler denial test is now written and type-checked, but has not been executed. |

The authority gap is concrete: the product has authenticated human contexts and
fresh `users.db` access checks, and G can resolve a confirmed immutable EmployeeId,
but there is no production writer for D's principal generations, lifetime
employee/tenant bindings or complete policy revision. `BindingStore::new`,
`register_principal` and `create_employee` are currently used by tests, not the
product administration flow. An ACL generation is not a revision of all employee
policy, and an employee slug is not an authenticated tenant. The execution and
worker plan traits likewise have no production per-tool implementations. These
must be implemented from trusted administration/dispatch state before wiring a
runtime context; a deny-all placeholder would not satisfy the requested turn.

#### Installed publication and per-run MCP readback

B1 consumes `/var/lib/duduclaw-spawnd/publication.json` for generation-bound host
entitlements, and the signed F `RuntimeEntry` artifact `isolation-policy-v1` for
runtime/native profiles. Root installation measurements live under
`/run/duduclaw-isolation-evidence`; they bind boot, generation, inventory and
bounded freshness. Account readiness requires real positive/negative SSH evidence
and the other account controls; no receipt is inferred from a unit's existence.
The exact strict schemas and limits are documented in the spawnd README.

F is nonce-probed every time a committed generation is consumed. Only the expensive
full verification of the same immutable signed generation is cached; failures
clear the cache and generation changes reverify it. A cold verification that
exceeds the one-second freshness budget only warms the cache; a subsequent fresh
probe is required to admit it. Per-launch executable/bridge/forwarder hashes remain.

B1 atomically publishes the gateway-readable root-owned 0640
`/run/duduclaw-spawnd/publication.json`. MCP acceptance binds runtime version,
runtime/bridge/config digests, native launch policy, fd 3, successful initialize,
parent/child exec, exclusive config and a nonzero transcript digest. This is a
consumer of measured acceptance, not an implementation that generates it. Runtime
scripts refuse because the present receipt cannot attest interpreter plus script
with one executable digest. Multiple profiles for a runtime also refuse at the
gateway registration lookup instead of guessing which policy applies.

Before StartRun, the host checks exact projection bytes and adds only D-owned fixed
arguments to the in-process plan. Those arguments do not travel in StartRun: the
signed root runtime registry must already contain them. B1 publishes root-owned
0444 config files, verifies the detached mount's recursive readonly state, ancestry,
exact contents and bridge digest, and writes a boot/run/employee/generation-bound
`mcp-<run>.json` receipt with a real mount ID. D reads that receipt only after the
actual TakeIo handoff. A missing/mismatched/stale receipt revokes the capability.
The root receipt TTL is 60 seconds; the general publication TTL is at most one
second. The projection has a 64 KiB limit. No gateway pathname is used inside the
employee namespace.

#### Worker lifecycle and acceptance boundary

The root-created `/run/duduclaw-parse/retirement.lease` begins with `clean\n` only
after the installer proves no old worker or queued activation. A create-only
systemd tmpfiles rule is provided. The launcher holds a cross-process flock and
fsyncs `dirty\n` before connecting, reserving one full input/memory slot. It never
creates or repairs the lease. A private pre-content nonce handshake carries the
worker's own pidfd with kernel SCM_CREDENTIALS; the gateway checks the PID and exact
systemd invocation plus sandbox/resource properties. The pidfd is closed in the
worker before parsing; normal parse protocol bytes are unchanged.

A successful response is not retirement. Cleanup requires the recorded process to
exit and its cgroup to be empty/removed, with the recorded unit/invocation lifecycle
validated. Unknown ownership, readback errors, timeout or gateway crash leave the
lease dirty. Root must reconcile workers and queued activation before resetting
it; automatic crash repair is not implemented. See the parse-worker README for
the final systemd lifecycle detail and the root test prerequisites.

#### PTY blocker

B2 can create a private devpts PTY in its own fixture, but the production status
handoff currently gives B1 a master fd without a namespace/mount provenance receipt.
B1's output relay handles pipes; merely duplicating the master cannot observe
activity without consuming bytes intended for A. Updating an idle timestamp on
each poll would disable the timeout, and replacing the master with a stream would
fail A's real-PTY validation. Completing this needs a private authenticated activity
channel or an equivalent design plus provenance checks, followed by the full-chain
PTY acceptance. Existing B2 PTY tests are not that evidence.

#### Remote build and root commands

Remote execution was attempted, not completed. The existing builder wrapper's
gcloud access failed because this sandbox cannot write its credentials database;
a direct SSH attempt to the inventoried builder address was denied with
`Operation not permitted`. No remote package or root test result is claimed.
Use a separate round-3 directory so another acceptance session's binaries remain
unchanged. The builder name was inventoried; its historical IP is not a current
reachability assertion.

```sh
GS=/Users/lizhixu/Project/commercial-wt-v180-linux/scripts/v180-gcp-builder/gssh.sh
rsync -az -e "$GS" --exclude target/ --exclude node_modules/ --exclude .git --exclude artifacts/ --exclude web/dist/ \
  /Users/lizhixu/Project/DuDuClaw-wt-iso-i1/ duduclaw-v180-builder-4:wt/iso-i1-r3/
"$GS" duduclaw-v180-builder-4 'cd ~/wt/iso-i1-r3 && . ~/.cargo/env && CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo check -p duduclaw-gateway -p duduclaw-cli --tests'
```

The gateway build must use the real dashboard asset build required by the project;
a stub asset directory is at most type-check scaffolding, never acceptance.
On the Linux builder, after syncing the final sources:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo check -p duduclaw-parse-worker --features engine --all-targets
cargo test -p duduclaw-gateway isolation_mcp::boot::tests -- --test-threads=1
cargo test -p duduclaw-gateway isolation_readiness::tests -- --test-threads=1
cargo test -p duduclaw-cli mcp_isolation -- --test-threads=1
cargo test -p duduclaw-spawnd installed::tests -- --test-threads=1
cargo test -p duduclaw-spawnd gateway_mcp::tests -- --test-threads=1
cargo test -p duduclaw-execution live_status_ -- --test-threads=1
cargo test -p duduclaw-execution live_snapshot_ -- --test-threads=1
cargo test -p duduclaw-mcp-boundary --features binding-store,execution-integration
cargo test -p duduclaw-parse-worker retirement -- --test-threads=1
```

Require actual nonzero matching test counts. Compile root test executables on the
builder using the existing acceptance `build.sh` JSON artifact inventory convention,
then copy those executables and matching installed binaries to the disposable VM.
The original acceptance scripts are under the task's `scratchpad/accept`; adjust
both build and runner roots consistently to `~/wt/iso-i1-r3` / `~/wt/iso-i1-r3-out`.
Do not reuse old `exes.txt` paths from another build. The runner uses one process per
exact ignored test, `DUDUCLAW_ISO_DISPOSABLE=1`, `--test-threads=1 --nocapture`, and
requires `test result: ok. 1 passed`; a zero-test exit is not a pass.

For the new E test, install the matching engine binary, socket/template and
create-only tmpfiles configuration first, and initialize a clean lease only after
proving that there are no old workers or pending activations:

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-parse-worker --features engine --test systemd \
  iso_root_e2e_worker_retirement_holds_host_slot_through_response_and_drop \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::tests::iso_root_e2e_publication_receipt_mode_and_symlink_refusal \
  -- --ignored --exact --test-threads=1 --nocapture
```

The E test exercises response/drop budget retention and actual retirement. The
publication fixture checks root mode/symlink rules. Neither is a Claude turn or
a substitute for the missing full-chain test. Root-installed publication readback
commands and final local validation results are recorded below after final review.

#### Round-3 file map

| Files (under `crates/duduclaw-*`) | Change |
| --- | --- |
| `spawnd/src/installed.rs`, `installed_root_tests.rs`, `installed_account_root_tests.rs`, `host.rs`, `bootstrap_mounts.rs`, `policy.rs`, `lib.rs`, `Cargo.toml`, README | Signed policy / measured installation consumers, root projections and per-run readback. |
| `spawnd/src/gateway_mcp.rs` | Gateway reader binding F generation, actual run and accepted runtime bytes. |
| `mcp-boundary/src/protected_launch.rs`, `config.rs`, `config_tests.rs` | Verify mount receipt after TakeIo; select Claude's in-namespace config path. |
| `gateway/src/isolation_mcp.rs`, `isolation_mcp/boot.rs`, `isolation_readiness.rs` | Consume fresh F generation for registry, install retiring E launcher, keep explicit missing authorities. |
| `execution/src/spawnd.rs`, `spawnd/transport.rs` | Fresh original-control-connection run snapshot for D and concurrency regression work. |
| `parse-worker/src/{retirement,retirement_hello}.rs`, `client.rs`, `lib.rs`, `main.rs`, `Cargo.toml`, unit/tmpfiles files, `tests/systemd.rs`, README | Cross-process budget lease, worker identity handshake and retirement lifecycle. |
| `scripts/ci/privileged_effects.json` | Exact account-test host-command and four I1 thread inventory entries; no whole-file exemptions. |
| `Cargo.lock`, security docs in three languages, `CHANGELOG.md`, this handoff | Existing zbus dependency edge, behavior/limits and exact validation instructions. |

**CONTRACT CHANGE:** no shared isolation wire formats, canonical capability bytes
or hard limits changed. Added schemas are private root publication/readback and
E's opt-in pre-request handshake. zbus 5.19 was already in Cargo.lock; no new
third-party version was introduced. No commit, deployment, installation, release
or formal Daybreak review occurred.


The installed-publication tests below require the real signed F generation,
measured installation receipts, confirmed root account/entitlement and exact CLI
MCP-child acceptance. They create none of those prerequisites. Missing evidence
fails the test. The last test additionally requires an already-running real Claude
run whose root receipt is still younger than 60 seconds; it does not start that run.

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_signed_policy_and_gateway_consumer \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_live_install_evidence \
  -- --ignored --exact --test-threads=1 --nocapture
# Set ISO_INSTALLED_RUN_ID to the actual 32-lowercase-hex run ID from the root journal.
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_actual_run_mcp_receipt \
  -- --ignored --exact --test-threads=1 --nocapture
```

E no longer uses `RefUnit`/`UnrefUnit`. If PID 1 has garbage-collected the unit,
only explicit exact-name `NoSuchUnit`, an exited original pidfd, a still-mounted
cgroup-v2 hierarchy and disappearance of the original cgroup directory permit
release. A missing `cgroup.events` file in an existing directory is insufficient.
If the unit still exists, its InvocationID must match and state must be inactive
or failed with an empty/removed cgroup. Property/GC races retry for at most 40
seconds; none releases a dirty lease on its own.

A's new `live_snapshot` reads fresh root Status on the original authenticated
control connection. Independent review caught normal async Status polling racing
a synchronous D watch. All transactions now share one bounded gate, and async
packet I/O executes in `spawn_blocking`, so a synchronous verifier on a
current-thread Tokio runtime cannot stall the future needed to release the gate.
Each read sends its own request ID and consumes its own response; there is no
cached allow or second control connection. The sync wait+I/O budget remains
250 ms. Cancellation, protocol errors and timeout shutdown the original socket
before releasing the gate. The new regression is
`live_snapshot_serializes_with_wait_without_revoking_a_live_run`; Linux execution
of it remains pending. No production `LiveAuthority` yet consumes this snapshot.


Independent review also found that repeated bundle reads could combine an old
BootstrapPlan with a newer acceptance/native profile when profile and version were
unchanged. B1 now pins one F generation for the daemon lifetime; every publication,
anchor, prepare, record and native-profile lookup refuses another generation until
F restarts B1. It compares every `PublishedRuntime` field, reconstructs the exact
BootstrapPlan and rechecks native launch acceptance. Tests change argv, resource
limits, timeouts, network and executable digest without changing profile/version,
and verify that all refuse. The reviewer rechecked both HIGH findings and found
no remaining HIGH/CRITICAL in the reviewed diff. This is ordinary independent
review, not the requested formal Daybreak sign-off or proof of runtime acceptance.

#### Round-3 validation record

Local commands used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`; no `cargo fmt` or
local gateway/CLI/workspace build was run. Test totals below exclude repeat runs,
ignored Linux root tests, zero-test binaries and earlier rounds.

| Selection | Observed result |
| --- | --- |
| macOS `cargo test -p duduclaw-mcp-boundary --features execution-integration,binding-store --offline` | 65 library + 1 bridge = **66 passed**, 0 failed. |
| macOS `cargo test -p duduclaw-spawnd -p duduclaw-execution -p duduclaw-parse-worker --offline` | **73 + 32 + 7 = 112 passed**, 0 failed. spawnd rerun after generation pin also passed 73. |
| Linux `cargo check -p duduclaw-parse-worker --features engine --all-targets --target x86_64-unknown-linux-gnu --offline` | Passed, 13.70 s. Includes the changed worker binary's code; no Linux link or execution. |
| Combined Linux all-target check of spawnd/execution/mcp-boundary/parse-worker with binding-store, execution-integration and engine | Passed, 8.68 s; the final spawnd all-targets check including real account attempts also passed (0.37 s warm cache). |
| Combined Windows all-target check of those four crates with binding-store and execution-integration | Passed, 5.85 s; no Windows link or execution, no parser engine feature. |
| Standalone harness including actual gateway boot/boundary/worker modules with unrelated gateway APIs stubbed | Linux all-target check passed, 3.57 s; no-worker-feature variant passed, 1.84 s. This is not the gateway or CLI package. |

The distinct macOS suites total **178 passed, 0 failed**. New Linux-only concurrency,
root publication, handshake and retirement tests were type-checked, not executed.
The first standalone harness attempt lacked the workspace lock and failed offline
resolution of the already-locked yanked async-executor version; copying Cargo.lock
resolved it. No dependency versions were changed to work around that error.
Cross checks use `LIBSQLITE3_SYS_USE_PKG_CONFIG=1 PKG_CONFIG_ALLOW_CROSS=1` because
this machine lacks the target C linker; this workaround proves Rust types only.

Exact combined check (run locally; not a Linux runtime result):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 LIBSQLITE3_SYS_USE_PKG_CONFIG=1 PKG_CONFIG_ALLOW_CROSS=1 \
  cargo check -p duduclaw-spawnd -p duduclaw-execution -p duduclaw-mcp-boundary -p duduclaw-parse-worker \
  --features duduclaw-mcp-boundary/execution-integration,duduclaw-mcp-boundary/binding-store,duduclaw-parse-worker/engine \
  --all-targets --target x86_64-unknown-linux-gnu --offline
```

Logs are `/private/tmp/iso-i1-r3-boundary-tests.log`,
`iso-i1-r3-component-tests.log`, `iso-i1-r3-spawnd-final-tests.log`,
`iso-i1-r3-worker-engine-check.log`, `iso-i1-r3-final-linux-check.log`,
`iso-i1-r3-final-windows-check.log`, `iso-i1-r3-boot-check.log`,
`iso-i1-r3-boot-no-worker-check.log`,
`iso-i1-r3-status-serialization-check.log`, and
`iso-i1-e-retirement-check.log` in the same temporary directory. Harness sources
are `/private/tmp/iso-i1-r3-boot-check`. The earlier
`iso-i1-r3-stage3-check.log` records the intermediate post-retirement Linux check.


#### Authority producer investigation for the next implementation pass

Use `UserDb::live_user_context`, not only `effective_access`: the former includes
credential epoch and forced-password-change state. `duduclaw-auth/src/org.rs`
`load_access` reads active user, direct/team access and ACL generation in one
transaction. Existing auth writers advance that ACL generation, but they do not
write D's `iso_principals`; client-only logout is not server-side revocation.

No existing field was found that can honestly serve as an immutable Shared tenant
identity. `OrgStore` stores reporting lines/departments; license customer and
subscription IDs describe billing/license records; the Teams connector's tenant
ID belongs to Microsoft Teams. G's Employee proposal and allocation Receipt do
not contain D lifetime Personal/Shared binding. Its separate UserAgentBinding
proposal's `owner_id` is an email resolved to a user UUID and `shared:<AccessLevel>`
is an ACL, not a tenant binding. `EmployeeAuthority::readback_shared_employee`
currently proves only confirmed root UID/quota, despite its name.

The next implementation therefore needs a durable installation/tenant identity
and explicit administration/migration confirmation of each lifetime employee
binding, plus live synchronization/revocation of D principals. Suitable existing
policy write points are `agent_files/writer.rs::commit_locked_with_lifecycle`,
`handlers/config_commit.rs::commit_rendered_inner`, `OrgStore::mutate` and UserDb's
transactional ACL writers. `autonomy_gate::lifecycle_barrier_required` currently
covers status becoming non-active, not all tool/permission/scope changes. A new
monotonic isolation revision must fence old runs/capabilities before these writes,
read back successful bytes and publish the next revision; after-commit events or
content hashes alone do not prevent an A→B→A policy from reviving old authority.
These are missing product integrations, not a request for new credentials and not
something that can be solved by merely starting the existing units.


#### Real account denial acceptance (written, not executed)

`installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials`
executes actual SSH public-key login/forwarding, su login, user-manager transient
unit, crontab installation and at scheduling attempts. It requires an already
confirmed installed employee and a dedicated unprivileged control account. Every
positive control must succeed; missing tools, unreachable SSH, an unstarted user
manager or arbitrary nonzero status are not passing negative evidence. Commands
have a 12-second deadline plus a two-second kill grace. Cron probes contain only
one harmless comment; at probes schedule `/usr/bin/true` and remove/read back the
newly created job. Unexpected SSH/cron/at success is cleaned up before test failure.

The fixture requires root-owned SSH key/known-host files and the same public key
in the fixed root-owned authorized-key file for the control and employee accounts.
Disk `sshd -T` alone is not enough: each real negative SSH attempt selects a fresh
source port and requires current-boot root journal entries for the same sshd PID,
port and employee plus an exact `DenyUsers` policy rejection. This prevents a
missing key or stale/different live daemon configuration from being mistaken for
lockdown. The dedicated control's public-key login and remote forwarding must
both succeed. Scheduler and user-manager tools run with `setpriv` at the real
employee UID/GID and cleared groups, checking an actual UID marker, so PAM's
expired-account refusal cannot substitute for testing the tool itself. The su
probe intentionally exercises the login/PAM path.

The full fixture prerequisites are in `crates/duduclaw-spawnd/README.md`; no setup,
key creation, installed SSH change or scheduler request was performed locally.
Export `ISO_ACCOUNT_CONTROL_USER`, `ISO_ACCOUNT_SSH_KEY` and
`ISO_ACCOUNT_SSH_KNOWN_HOSTS` to the documented dedicated fixture values, then run
from a root shell on the disposable VM:

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials \
  -- --ignored --exact --test-threads=1 --nocapture
cargo test -p duduclaw-spawnd ssh_policy_evidence_requires_same_root_session_and_source_port
```

The latter is a pure log-binding regression test, not an SSH attempt. This new
suite closes the previously unwritten account-attempt test source, not its Linux
execution gate, and does not cover the missing Claude turn or D8 chain.


Final account-test Linux check passed; log:
`/private/tmp/iso-i1-r3-account-final-check.log`. The new test and its log-binding
regression have been type-checked only. Final `git diff --check` passed.

The repository spawn-inventory check is **not green**:

```sh
python3 scripts/ci/check_spawn_inventory.py
```

It reports **34 unclassified functions**, each confirmed to exist in HEAD with
identical observed API counts. This round explicitly registered the new account
fixture's host process and four I1 threads (A's concurrency test, E retirement,
the F probe and B1 allocation server); the latter two were inherited from rounds
1–2. Production threads are classified `not_process`, not employee-execution
exemptions. No whole-file exclusion was added. The baseline comparison is
`/private/tmp/iso-i1-spawn-inventory-baseline.json`; final checker output is
`/private/tmp/iso-i1-r3-final-spawn-inventory.log`. The four I1 threads are now classified, while the 34 unrelated baseline entries
remain for their owning packages. Independent review checked all five new
inventory entries against their exact methods. Inventory coverage does not
establish isolation acceptance.

### Round 4 — historical integration handoff (2026-11-05)

**Status: partial authority integration, not an executable protected TEAM release.**
Work remained in the requested dependency order. The production Shared authority
adapter now uses existing product records; complete BootSources, runtime entry
contexts, PTY, and Claude/D8 full-chain tests are still unfinished. No commit,
merge, release, local gateway/CLI/agent build, workspace build, or `cargo fmt` was
performed. Existing rounds 1–3 remain uncommitted in this same supplied worktree;
this session created no additional worktree. No contract wire format, canonical
bytes, or hard limit changed.

#### Authority and cancellation changes

- `users.db/auth_meta` now holds a stable installation tenant UUID and monotonic
  isolation policy epoch, initialized together by the existing UserDb migration.
  No new database, table, synthetic background user, or parallel ACL store was
  introduced. A missing single key, corrupt UUID, or corrupt epoch is refused;
  only a legacy database missing both keys gets the initial pair.
- The read-only auth snapshot uses the existing transactional active-user,
  direct/team/manager access and ACL-generation calculation. Human runs need a
  completed login, no forced password change, Operator-or-higher employee access,
  and the credential epoch authenticated at ingress. Background runs explicitly
  have no human; their tenant and ACL generation remain live-checked.
- G's existing `confirmed_employees` allocation index now pins the tenant beside
  its receipt. Every lookup rechecks the Applied confirmation and root UID/quota
  receipt. An active legacy row without a tenant cannot be silently backfilled;
  disable it and approve a fresh employee allocation. A disabled employee ID
  cannot be resurrected or transferred. Compensation may disable an unpinned
  row without assigning it a tenant.
- `GatewayAuthority` rechecks the live employee registry, auth snapshot, complete
  strict policy digest, tenant pin and A's original authenticated control/run
  status. Boot occurs after UserDb and MethodHandler's registry initialization.
  Both the registry and current effective policy file must say Active. Registry
  `try_read` contention refuses validation and may revoke a connection during a
  rescan; it does not reuse a cached allow. Personal lifetime bindings and
  connector-to-tenant grants still have no production adapter and are refused.
- Epoch-local employee/run maps are bounded to 4096; they are associations, not
  durable authority. Prepared policy revisions are unique local counters, not
  truncated hashes. Verification compares the full digest and durable epoch;
  the epoch is read again after digest/root Status I/O. Run registration occurs
  after reservation and before D issues a capability. Revocation and cancellation
  clean up a late blocking registration through RAII and a cancellation latch.
- Core org/preset/bootstrap writers, dashboard config/agent/contract writes,
  Google config commits, custom redaction writes and changed resolved-preset
  artifacts fence the existing users.db epoch before publication. Restoring A
  after A→B→A cannot restore the old epoch. A failed write may conservatively
  revoke extra runs. The CLI org-sync and preset bind/unbind/install-builtin
  entries run existing auth migration after their operator gates, so a legitimate
  standalone command can upgrade a legacy store before gateway startup. Other
  onboard/scaffold/expert creation flows have not had equivalent legacy-entry
  coverage added. Direct external edits do not get an epoch bump; the live digest
  catches a persistent change, but transient external A→B→A is not covered.
- Policy fencing rejects a symlink database leaf. It resolves the trusted parent
  first because SQLite NOFOLLOW also rejects macOS's ordinary `/var` ancestor
  symlink. Missing databases stay missing; missing/corrupt metadata is not repaired
  by the fence. Tests cover both parent and leaf cases.
- A shared bounded blocking verifier (16 jobs; 2-second permit wait; 10-second
  result deadline) keeps root/SQLite reads off Tokio's reactor. Cancelled jobs
  retain their permit until completion. Each connection allows one authority
  read at a time; its watcher is polled independently of request deadlines and
  EOF. Pending D grants are revoked if a cancelled blocking result is discarded.
  Resource checks and host turn snapshots use the same off-reactor path. The
  connector client's remaining synchronous checks are not all converted; the
  production connector grant adapter currently refuses those routes.

These are source and unit-level properties. Full prepared-run A→B→A/racing-root-
Status execution tests, actual authenticated ingress capture and formal Daybreak
security sign-off remain pending. The independent ordinary review found and
rechecked fixes for tenant transfer, watcher starvation and legacy standalone
CLI migration; its final read found no new HIGH/CRITICAL in the reviewed scope.

#### Remaining work, in required order

1. Finish production authority integration: ingress/dispatch must construct
   `LaunchInput` with the actual authenticated credential epoch, principal,
   bounded tool/resource grants and `HostTurn` (turn ID, approval session,
   metadata, principal context). Do not reconstruct missing values from employee
   payloads or environment. Personal bindings, connector ownership and the
   utility-runner association remain absent. Audit remaining policy writers and
   exercise complete run revocation across real writes and root Status races.
2. Complete BootSources with real `ExecutionPlanFactory` and `WorkerPlanFactory`.
   Only fixtures and the resource-checking wrapper implement these traits today.
   The CLI's `require_complete_services` must remain enforced: partial factories,
   `ListingPlans`, or fabricated support flags cannot establish readiness.
3. After boot services compile, wire Claude, Codex, Grok, generic CLI and
   openai-compat separately through `prepare_launch` and the scoped execution
   context. Their MCP config must use D's root-published shim, preserving the
   exact per-runtime fd-3 acceptance and sealed projection. No runtime call site
   was connected in this round.
4. Pass PTY through A→B1→B2 with its descriptor roles, status/resize, cancellation
   and teardown. No PTY-chain implementation was added this round.
5. Write the real ignored Claude and D8 `iso_root_e2e_*` tests: one authenticated
   D tool call; actual process uid/namespaces/cgroup/fd evidence; a second run
   waiting for an empty first cgroup including double-fork residue; self-dispatch
   returning WouldDeadlock. **These full-chain tests still do not exist.** The
   existing publication/receipt/account tests below are narrower and cannot be
   reported as substitutes.

The missing execution routes (22), after the CLI overrides and documented host
refusals, are `create_task`, `spawn_agent`, `spawn_ephemeral`, `execute_program`,
`office_script`, `fork_run`, `inspect_branches`, `diff_branches`, `merge_or_select`,
`terminate_branch`, `fork_cost`, `discovery_catalog`, `discovery_list`,
`discovery_tree`, `discovery_artifact`, `discovery_cancel`, `browser_record_start`,
`browser_record_stop`, `code_map`, `hardware_info`, `model_search`, `model_recommend`.
The missing worker routes (18) are `skill_extract`, `shared_skill_adopt`,
`skill_from_recording`, `skill_security_scan`, `skill_graduate`,
`skill_synthesis_run`, `skill_hub_install`, `skill_curator_status`,
`transcribe_audio`, `meeting_transcribe`, `meeting_upload_link`,
`meeting_transcribe_status`, `meeting_transcribe_cancel`, `synthesize_speech`,
`knowledge_add_source`, `web_fetch_cached`, `web_extract`, `route_query`.

For execution, reuse `GatewayPublication::read(gateway_gid)?.policy()?.derive(...)`
for root policy, but first provide an actual tool→registered-runtime/helper-input
mapping and extract the original handler's no-effect authorization/plan API.
Native command argv or invented runtime IDs are not root authority. For workers,
reuse `install_worker_services` and E's actual fd parsers, then implement
handler authorization → authorized File → ParseRequest → existing result/write
APIs. Status/cancel tools cannot be represented by fabricated parser input.
TranscribeAudio/SynthesizeSpeech/LoadModel/LocalInference wire enum variants do
not establish parser implementations; the inspected E engine does not implement
those operations. Detailed source pointers are also preserved in
`/private/tmp/iso-i1-r4-factory-prerequisites.md`.

#### Round-4 file handoff

Paths below are under `crates/` unless stated otherwise; inherited round-1–3
changes outside this list were preserved.

| File | Round-4 change |
| --- | --- |
| `duduclaw-auth/src/isolation.rs` | Existing users.db initialization and live read-only authority snapshot. |
| `duduclaw-auth/src/isolation_tests.rs` | Tenant, human ACL/epoch, corruption and legacy-migration regressions. |
| `duduclaw-auth/src/db.rs` | Initialize tenant/epoch through normal schema migration. |
| `duduclaw-auth/src/org.rs` | Reuse effective-access calculation within the caller's transaction. |
| `duduclaw-auth/src/lib.rs` | Export authority snapshot API. |
| `duduclaw-core/src/isolation_policy.rs` | Durable pre-write epoch fence, with no database creation. |
| `duduclaw-core/src/isolation_policy_tests.rs` | Writer ABA, corruption, no-create and symlink regressions. |
| `duduclaw-core/src/org_store.rs` | Fence org publication under its writer lock. |
| `duduclaw-core/src/preset.rs` | Fence preset bindings and installed operator preset writes. |
| `duduclaw-core/src/config.rs` | Fence minimal config publication. |
| `duduclaw-core/src/lib.rs` | Export policy fence. |
| `duduclaw-core/Cargo.toml`, root `Cargo.lock` | Reuse already-locked workspace rusqlite; no dependency version change. |
| `duduclaw-agent/src/registry.rs` | Fence changed/removing resolved preset artifact; unchanged bytes do not bump. |
| `duduclaw-cli/src/isolation_policy_init.rs` | Operator-only callers' existing-auth migration helper and tests. |
| `duduclaw-cli/src/preset_cmd.rs` | Migration preparation and install-builtin operator guard. |
| `duduclaw-cli/src/lib.rs` | Register helper; prepare non-dry-run org sync. |
| `duduclaw-cli/src/mcp_isolation.rs` | Async resource checks and owned turn resolver. |
| `duduclaw-cli/src/mcp_isolation_identity.rs` | Adjust turn-scope callers/tests. |
| `duduclaw-cli/src/mcp_isolation_resources.rs` | Offload connector/resource reads from reactor. |
| `duduclaw-gateway/src/approval/policy_snapshot.rs` | Strict parsed/active authority digest and regressions; legacy digest unchanged. |
| `duduclaw-gateway/src/approval/binding.rs`, `approval/mod.rs` | Export strict revision entry. |
| `duduclaw-gateway/src/handlers/migration_authority_employee.rs` | Tenant pin on existing allocation index; live root readback and tests. |
| `duduclaw-gateway/src/handlers/config_commit.rs` | Fence known authority targets before publish, with writer regression. |
| `duduclaw-gateway/src/google_identity/home_source.rs` | Fence both actual Home config commit paths. |
| `duduclaw-gateway/src/redaction_custom_rules.rs` | Fence custom redaction publication/deletion. |
| `duduclaw-gateway/src/isolation_mcp/authority.rs` | Production Shared authority, bounded run associations, live epoch/status checks. |
| `duduclaw-gateway/src/isolation_mcp/launch.rs` | Cancel-safe authority registration around ProtectedMcpLaunch. |
| `duduclaw-gateway/src/isolation_mcp/boot.rs` | Construct authority from existing UserDb/registry; still refuse missing factories. |
| `duduclaw-gateway/src/isolation_mcp.rs` | Bounded async sweep, exports and non-Linux boot signature. |
| `duduclaw-gateway/src/server.rs` | Boot authority after existing auth and employee registry initialization. |
| `duduclaw-gateway/src/isolation_egress.rs` | One host turn snapshot off reactor; regression source added. |
| `duduclaw-gateway/src/isolation_resources.rs`, `isolation_connectors.rs` | Consume BindingAuthority rather than a second durable BindingStore. |
| `duduclaw-mcp-boundary/src/bindings.rs` | BindingAuthority abstraction; preserve fixture store adapter. |
| `duduclaw-mcp-boundary/src/authority.rs`, `authority_async_tests.rs` | Bounded verifier, per-connection serialization and Linux regressions. |
| `duduclaw-mcp-boundary/src/server.rs` | Independent watcher/deadline/EOF polling and fresh dispatch checks. |
| `duduclaw-mcp-boundary/src/protected_launch.rs` | Cancellation-safe pending grants and off-reactor issue/activation. |
| `duduclaw-mcp-boundary/src/tool_policy.rs`, `tool_policy_tests.rs` | Async resource authorization and current-thread regression. |
| `duduclaw-mcp-boundary/src/factories.rs`, `worker_execution.rs` | Await live resource authorization around effects. |
| `CHANGELOG.md`, this document | Current status, evidence limits and next exact commands. |

#### Round-4 validation actually performed

All local Cargo commands used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`. Gateway,
CLI and agent packages were not built. Standalone harnesses use actual changed
source files through `#[path]`, with unrelated gateway APIs stubbed; they are not
full-package validation. No Linux root test was executed this round.

| Command/selection | Result |
| --- | --- |
| `cargo test -p duduclaw-mcp-boundary --features binding-store,execution-integration --offline` | **66 library + 1 bridge passed**, 0 failed; Linux-only tests excluded on macOS. |
| `cargo test -p duduclaw-auth isolation_authority --lib` | **8 passed**, 0 failed, with the default macOS TMPDIR. |
| `cargo test -p duduclaw-auth org::tests --lib` | **30 passed**, 0 failed. |
| `cargo test -p duduclaw-core isolation_policy --lib --offline` | **7 passed**, 0 failed. |
| `cargo test -p duduclaw-core org_store --lib` | **19 passed**, 0 failed. |
| `cargo test -p duduclaw-core preset::tests --lib` | **23 passed**, 0 failed, 1 pre-existing ignored. |
| `cargo test -p duduclaw-core config::tests --lib` | **13 passed**, 0 failed. |
| Standalone actual policy_snapshot source | **6 passed**, 0 failed; payload hashing helper is stubbed. |
| Standalone actual G employee authority source | **2 passed**, 0 failed; no root ReadBack executed. |
| Standalone actual CLI policy-init helper | **3 passed**, 0 failed; no CLI crate build. |
| Linux all-target core/auth/boundary check | Passed, with boundary binding-store/execution-integration. |
| Windows all-target boundary check | Passed; no Windows execution or link, core/auth not checked on Windows this round. |
| Linux actual boot/authority/launch source harness | Passed with worker feature and without worker feature; unrelated gateway services are stubs. |

Intermediate failures were not hidden: the initial Linux harness needed an
available target C compiler for aws-lc (then passed using the existing Zig
wrapper), and SQLite NOFOLLOW initially rejected `/var` ancestors (6 auth tests
passed, 2 failed). Canonicalizing only the trusted parent fixed the actual code;
the default-temp auth rerun passed all 8 and the explicit parent/leaf regression
passed. No lockfile version workaround was used.

Logs: `/private/tmp/iso-i1-r4-resource-authority-tests.log`,
`iso-i1-policy-legacy-auth-final.log`, `iso-i1-r4-auth-org-tests.log`, `iso-i1-r4-core-final-tests.log`,
`iso-i1-policy-epoch-org.log`, `iso-i1-policy-epoch-preset.log`,
`iso-i1-policy-epoch-config.log`, `iso-i1-r4-policy-final-tests.log`,
`iso-i1-r4-employee-check.log`, `iso-i1-cli-policy-helper-harness.log`,
`iso-i1-r4-final-linux-check.log`, `iso-i1-r4-final-windows-check.log`,
`iso-i1-r4-live-registry-check.log`, `iso-i1-r4-boot-no-worker-check.log`.
Harness directories: `/private/tmp/iso-i1-r4-boot-check`,
`/private/tmp/iso-i1-r4-employee-check`,
`/private/tmp/iso-i1-cli-policy-helper-harness`.

The existing spawn checker still reports the same 34 inherited unclassified
functions in this worktree (`/private/tmp/iso-i1-r4-spawn-inventory-before.log`).
Per the owner, I2 already fixed these on the integration branch; this round did
not redo or edit their inventory entries. No new process-launch site was added.
The new `verify_blocking` call runs bounded host verification on Tokio's blocking
pool; it does not launch an employee process and is not a scanner process site.
Final `git diff --check` passed. Full gateway/CLI compilation remains the remote
builder's next check; the local compiling state is limited to the small crates
and explicitly scoped source harnesses above.

#### Exact remote compile and regression commands (not run here)

Use the Linux builder for the actual packages before root acceptance. The
standalone harness does not replace these checks. Preserve the acceptance
builder's normal feature flags and lower jobs if sharing its memory budget.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-gateway -p duduclaw-cli --all-targets
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib isolation_mcp::authority::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib approval::policy_snapshot::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib handlers::migration_authority::employee::isolation_tenant_tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib \
  handlers::config_commit::tests::isolation_policy_replacement_fences_all_authority_targets_before_publish \
  -- --exact --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib \
  isolation_egress::tests::isolated_turn_snapshot_runs_once_off_the_reactor_thread -- --exact
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-cli --lib isolation_policy_init::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-cli --lib mcp_isolation_identity
```

These boundary tests are unprivileged Linux regressions, **not ignored root
acceptance**. Require nonzero matched tests (the async module contains three):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary \
  --features binding-store,execution-integration authority::async_tests -- --nocapture
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary \
  --features binding-store,execution-integration \
  protected_launch::tests::pending_guard_drop_revokes_issued_capability -- --exact
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary \
  --features binding-store,execution-integration \
  protected_launch::tests::cancelled_blocking_handoff_drops_pending_capability -- --exact
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-mcp-boundary \
  --features binding-store,execution-integration \
  tool_policy::tests::async_resource_authority_is_fresh_and_off_the_reactor_thread -- --exact
```

The three async test names are
`authority::async_tests::slow_authority_does_not_starve_request_deadline_or_delay_revocation`,
`authority::async_tests::slow_authority_idle_eof_revokes_without_waiting_for_blocking_job`,
and `authority::async_tests::cancelled_verification_retains_connection_slot_until_work_finishes`.

#### Exact existing root e2e commands and acceptance-runner convention

Only run on the provisioned disposable Linux acceptance VM, from a root shell.
The installed signed publication, live units, storage, exact-runtime acceptance
and confirmed employee prerequisites in the spawnd README remain mandatory.
The third command needs `ISO_INSTALLED_RUN_ID` from an **already real run**; it
reads its receipt and does not launch Claude. The fourth needs the dedicated
control account, root-owned SSH key/known-host fixtures and actual services
specified in `crates/duduclaw-spawnd/README.md`.

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_signed_policy_and_gateway_consumer \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_live_install_evidence \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::root_acceptance::iso_root_e2e_installed_publication_actual_run_mcp_receipt \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib \
  installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials \
  -- --ignored --exact --test-threads=1 --nocapture
```

For prebuilt executables, use the supplied `accept/build.sh`/push convention and
place `exes.txt` at `/home/lizhixu/wt/iso-accept/exes.txt`. On the VM, with
`ISO_EGRESS_ECHO_ADDR` and the fixture variables above exported, the supplied
`runall.sh` can select exactly these existing four tests:

```sh
bash runall.sh /home/lizhixu/wt/iso-accept/i1-r4 \
  '^installed::(root_acceptance::iso_root_e2e_installed_publication_(signed_policy_and_gateway_consumer|live_install_evidence|actual_run_mcp_receipt)|account_root_acceptance::iso_root_e2e_installed_account_real_denials)$'
```

Require exactly four summary rows, each with `test result: ok. 1 passed` in its
raw log. The runner supplies the disposable marker, storage root, serial exact
selection and 600-second timeout. Zero selected tests, receipt-only checks and
account denials do not satisfy the absent Claude/D8 full-chain gate. There is no
honest exact full-chain filter to provide until those tests are implemented.

### Round 5 — current integration handoff (2026-11-05)

**PARTIAL: production Claude end-to-end execution is not closed.** The existing
rounds 1–4 changes remain uncommitted. This round added the root-published Claude
main plan, the protected launch consumer in both Claude streaming paths, explicit
refusals for the other four runtimes, and two ignored root-chain tests. It did
not complete the production MCP tool factories or authenticated ingress. The
composite gate still refuses protected TEAM work. No gateway-UID fallback was
added, no readiness field was changed to Ready, and no tool-completeness check
was weakened. Neither new root test was executed.

The owner's requested ordering was Claude first, then the other runtimes, then
PTY. Claude's complete production path remains the first outstanding item. The
other runtimes received only the requested fail-closed entry fences; PTY was not
started. There is no contract wire-format/canonical-byte/hard-limit change, no
new dependency version, no commit, and no local gateway/CLI/workspace build or
`cargo fmt`. Work stayed in the supplied worktree; no worktree was created.

#### What changed in this round

Paths in this table are relative to `crates/` unless stated otherwise. Entries
name this round's edits only, not all inherited dirty files.

| File | Round-5 change |
| --- | --- |
| `duduclaw-spawnd/src/gateway_plan.rs` (new) | `InstalledMainPlanFactory`, fresh generation-pinned root policy read, exact signed Claude input protocol, pure plan tests. This is a main-process factory, **not** D's MCP `ExecutionPlanFactory`. |
| `duduclaw-spawnd/src/lib.rs` | Expose the Linux main-plan module, with pure tests available on the development host. |
| `duduclaw-gateway/src/isolation_mcp/boot.rs` | Fill and retain `main_plans` from verified generation/real gateway GID; report `MainPlans` when absent; derive a Claude plan only for the confirmed employee under the same gateway home. Existing E `RetiringLauncher` injection remains intact. |
| `duduclaw-gateway/src/isolation_mcp/runtime.rs` (new) | One-shot host `RuntimeAuthorization`, protected A/D launch context, concurrent bounded stdin delivery, cancellation ownership, five source-level tests. |
| `duduclaw-gateway/src/isolation_mcp.rs` | Linux-only runtime module and host-only authorization scope export. |
| `duduclaw-gateway/src/claude_runner.rs` | Protected Claude consumer in the shared stream reader; skip legacy MCP/hook preparation and native credential/command path; refuse unsupported execution options. Local stream callers and cancellation test retain their existing path. |
| `duduclaw-gateway/src/channel_reply/cli_env.rs` | Protected Claude consumer retaining the channel stream parser, thinking, usage attribution and trajectory handling; skip legacy credential/MCP/hook/system-temp preparation; observe input completion before success. |
| `duduclaw-gateway/src/runtime/isolation_gate.rs` (new) | Named terminal operator-facing refusal for unfinished runtimes; two pure source tests. |
| `duduclaw-gateway/src/runtime/{mod,codex,grok,generic_cli,openai_compat}.rs` | Register the gate; refuse all seven execution/streaming entry points before native credentials, configuration, network or PTY work. |
| `duduclaw-spawnd/tests/claude_chain.rs` (new) | Real installed A/B1/B2/Claude/shim/D transport acceptance with a test-only D callback and authority. |
| `duduclaw-spawnd/tests/d8_chain.rs` (new) | Real double-fork residue, queueing, self-wait refusal, process identity and independent unit/cgroup retirement observations. |
| `duduclaw-spawnd/src/bin/d8-fixture.rs` (new) | Explicit-feature, debug-only ELF that must be separately signed into the disposable acceptance generation. |
| `duduclaw-spawnd/Cargo.toml` | Linux test dependencies using existing versions, fixture feature and binary target. |
| `Cargo.lock` (repository root) | Record the spawnd test dependency edges to execution/Tokio using existing locked versions; retain inherited lockfile changes. |
| `duduclaw-spawnd/tests/README.md` (new) | Exact signed prerequisites, root commands, copied-executable convention and evidence limits. |
| `scripts/ci/privileged_effects.json` (repository root) | Classify the three new constructor/backend/fork sites; leave the 34 inherited I2 inventory findings untouched. |
| `docs/features/{,zh-TW/,ja-JP/}05-security-defense.md`, `CHANGELOG.md` (repository root) | Document the actual runtime refusal behavior and incomplete production status. |
| `docs/architecture/native-linux-isolation.md` (repository root) | This handoff; Round 4 retained as historical evidence. |

`InstalledMainPlanFactory::claude` rereads root publication for each plan and
requires an exact generation and employee entitlement. The signed Claude argv
must equal `gateway_plan::claude_arguments(model)`: text stdin, stream-json
output, system prompt at `/proc/self/fd/5`, built-in tools disabled with
`--tools ""`, only D MCP tools auto-approved, and strict `/config/mcp.json`.
D appends its three fixed MCP arguments exactly once. Runtime resources,
network and timeouts come from root publication. Native `Command` arguments
and environment are never treated as authority. Model changes, extra flags,
wrong employee, unsupported execution classes and input budgets are refused.
The initial scope is non-PTY Claude, not the no-tool judge profile.

The user prompt is sealed in fd 4 and delivered unchanged on stdin with EOF;
the system prompt is a separate sealed attachment at fd 5. The gateway drains
input concurrently with output, limits delivery to 30 seconds, kills the run
on incomplete input, and observes writer completion before returning success.
Cancellation drops both the managed child and writer. The root test checks an
independent system nonce, so a user-prompt echo cannot impersonate fd 5 delivery.

`RuntimeAuthorization` has no serde/environment/RPC constructor. An initiating
host scope is consumed once, does not inherit into a new Tokio task, and cannot
be replaced or revived by a nested scope. Both employee directory and gateway
home must match. Missing authority, unsupported credential/env overrides,
reasoning effort, explicit legacy MCP config, or channel session resume refuse
with a terminal spawn-gate message. **There is currently no production ingress
that constructs this scope.** The export is a consumer seam, not completed
principal propagation. The root test uses its own explicitly marked authority.

#### Remaining work, in dependency order

1. **Production tool factories:** `BootSources.execution_plans` and
   `worker_plans` still have no production producers. D's execution factory is
   for MCP-triggered helpers, distinct from the new Claude main plan. The
   existing dispatcher completeness check covers 22 A execution tools and 18 E
   worker tools. Their real handler-to-plan adapters, registered helper runtime
   publications and typed results must be implemented together. Do not return
   unconditional `supports`, fabricate a no-op plan, narrow the advertised
   inventory just to pass boot, or skip `require_complete_services`. E's actual
   retirement-aware `WorkerLauncher` is already injected; the missing part is
   its product operation/plan mapping, not another socket launcher.
2. **Authenticated ingress:** construct the authorization from the original
   verified channel/web user or approved background trigger and carry it across
   the actual task/spawn boundaries. Populate fresh policy grants/resource
   roots, `WaitOrigin`, caller, turn and authenticated epoch; do not infer an
   anonymous background principal from an absent user. Scope only the target
   employee invocation. Preserve approvals, delegation identity, cancellation,
   budget attribution and terminal error handling.
3. **Composite readiness:** replace `PendingIntegration` for parse worker, D
   boundary and runtime adapters only after the corresponding real producers,
   fresh service evidence and supported runtime policy exist. Current code
   deliberately keeps these pending. A constructed main-plan object is not a
   ready product or root acceptance receipt.
4. **Production Claude acceptance:** run the new transport test with the exact
   signed Claude input protocol, then exercise actual gateway boot, real ingress
   and the original CLI product dispatcher with one real product tool. The
   transport test's live-status test authority and nonce callback cannot close
   that second acceptance requirement. Provider credentials/routing must be
   provisioned through the approved isolation design; no gateway credential
   environment is copied into the run.
5. **Other runtimes:** reuse the protected launch path and signed runtime
   configuration. Codex/Grok additionally need accepted home/config projections;
   generic CLI needs its registered protocol, and openai-compat needs an
   isolated API worker path. Their current explicit refusals are intentional.
   Non-protected callers retain existing behavior.
6. **PTY:** carry allocation, fd transport, terminal size, resize/input/output,
   idle timeout, cancellation and retirement through A/B1/B2, with native tests.
   No PTY integration or PTY acceptance was added this round.
7. **Acceptance/review:** actual package compilation on the owner's remote
   builder, both new root tests, complete product-path tests and formal Daybreak
   security review remain required. Ordinary independent review found no new
   CRITICAL/HIGH issue after fixes, but it was not Daybreak/security signoff.

The tool-factory gap requires operation routing work, not just filling two
constructors. A's `ExecutionToolBridge::execute` reserves `context.employee`,
accepts only `EmployeeTool + Registered + !mcp + !pty`, and returns an accepted
run handle. It cannot directly preserve a cross-employee delegation, store
query or cancellation API. E's `WorkerToolBridge::validate_plan` requires an
actual input `File` and parser result; job-control APIs have no such input.
Do not manufacture a file or child process to satisfy these shapes.

| Existing product operations | Concrete integration seam still missing |
| --- | --- |
| `mcp/bus_tasks.rs::handle_create_task`, `mcp/spawn.rs::handle_spawn_agent` / `handle_spawn_ephemeral` | Target-employee dispatch with original delegation, target ACL and background admission; a same-employee helper plan changes semantics. |
| `mcp_fork.rs` run/inspect/diff/merge/terminate/cost; `mcp/discovery.rs::handle_discovery_query` | Split actual branch launches from authorized store/artifact reads, workspace promotion/CAS and existing-run cancellation. Preserve original handler authorization and record gates. |
| `mcp/exec.rs` program/Office execution; `mcp_recording.rs` record start/stop; `mcp_memory_handlers.rs` code map; `mcp/inference.rs` hardware/model operations | Actual signed helper protocols, authorized workspace inputs and typed results; preserve skill/script/path/network policies and existing-run handle ownership. |
| Skill lifecycle/hub/curator/recording-distill tools | Separate fetch, approval, parsing and publication; an archive parse result alone does not implement install or curator semantics. |
| `mcp/meeting_tools.rs` upload/status/cancel; `mcp/media_tools.rs` transcription/TTS; `mcp/inference.rs` route query | Keep authenticated job control/provider credentials/pure routing on their appropriate host paths; isolate untrusted media processing. These are not all file-parser operations. |
| `mcp/knowledge_client.rs` source ingestion; `mcp/web.rs` fetch/cache/extract | Reuse E knowledge/HTML parsing with host-owned descriptors while retaining collection/source authority, egress/SSRF/rate/cache policy and indexing/publication results. |

Already reusable: root `GatewayPublication`/`RootPolicy::derive` for registered
runtime entitlement, `RetiringLauncher`/`WorkerInvoke` for real worker lifetime
and fd exchange, E's knowledge/table/document/archive adapters, `ResourceRoots`
and `ResourceCheckedPlans`, and the original product dispatcher/approval gates.
Selecting a correct authenticated gateway-control route is distinct from
deleting a tool from completeness. That routing design and its regressions must
be completed before these producer slots can truthfully close.

#### Validation actually run

| Check | Result and evidence |
| --- | --- |
| `cargo test -p duduclaw-spawnd --lib gateway_plan --offline` with 2 jobs/incremental off | **2 passed**, 0 failed; `/private/tmp/iso-i1-r5-main-plan-tests.log`. |
| Actual `isolation_mcp/runtime.rs` in standalone source harness | **5 passed**, 0 failed; `/private/tmp/iso-i1-r5-runtime-tests.log`. Real core/execution/boundary dependencies, stub gateway boot/autonomy and spawn-error facade. This is not a gateway package build or product-path test. |
| Actual `runtime/isolation_gate.rs` in standalone rustc harness | **2 passed**, 0 failed. Execution latch/error facade stubbed; pure refusal cases only. |
| Actual boot/authority/launch/runtime sources in Linux source harness, `--all-targets`, worker feature on and off | **Both checks passed**; `/private/tmp/iso-i1-r5-boot-tests-check.log` and `iso-i1-r5-boot-no-worker-check.log`. Unrelated gateway services are stubs. |
| Actual spawnd Claude/D8 tests and fixture, Linux cross-check | **Passed**, not executed; `/private/tmp/iso-i1-r5-chain-check.log`. |
| Privileged-effect inventory | Still **34 inherited unclassified sites**; normalized site sets equal Round 4, no new unclassified site. This is not a passing full scanner result. `/private/tmp/iso-i1-r5-spawn-final.log`. |
| `git diff --check` | Passed. |
| Local gateway/CLI/workspace build; root execution | **Not run** per build limit / absent signed disposable acceptance setup. No actual Claude tool-call or D8 root pass is claimed. |

The compiling state is bounded to the small crate/test targets and source
harnesses above. The changed complete gateway modules, full package feature
matrix and Windows/macOS gateway builds still need the remote build. The local
Claude `--help` was inspected for the input/tools flags, but that does not prove
the acceptance machine's signed CLI version or provider execution.

Exact local commands used (source harnesses are temporary diagnostics):

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib gateway_plan --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
  CARGO_TARGET_DIR="$PWD/target" cargo test \
  --manifest-path /private/tmp/iso-i1-r5-runtime-tests/Cargo.toml --offline
rustc --edition=2021 --test /private/tmp/iso-i1-r5-runtime-gate-harness.rs \
  -o /private/tmp/iso-i1-r5-runtime-gate-harness
/private/tmp/iso-i1-r5-runtime-gate-harness
```

For each of the following cross-check invocations, set the environment below.
`/private/tmp` Zig caches are necessary on the restricted development host.

```sh
export CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0
export CC_x86_64_unknown_linux_gnu=/private/tmp/iso-i1-zig-cc
export AR_x86_64_unknown_linux_gnu=/private/tmp/iso-zig-ar
export ZIG_GLOBAL_CACHE_DIR=/private/tmp/iso-i1-zig-global
export ZIG_LOCAL_CACHE_DIR=/private/tmp/iso-i1-zig-local
export CARGO_TARGET_DIR=/private/tmp/iso-i1-r3-target
cargo check --manifest-path /private/tmp/iso-i1-r5-boot-check/Cargo.toml \
  --all-targets --target x86_64-unknown-linux-gnu --offline
cargo check --manifest-path /private/tmp/iso-i1-r5-boot-check/Cargo.toml \
  --all-targets --no-default-features --target x86_64-unknown-linux-gnu --offline
cargo check -p duduclaw-spawnd --test claude_chain --test d8_chain \
  --bin duduclaw-spawnd-d8-fixture --features root-test-fixture \
  --target x86_64-unknown-linux-gnu --offline
```

#### Exact remote compile and regression commands (not run here)

Run on the Linux builder, not this development Mac. Keep the Round-4 regression
commands as well. Require nonzero matches for each test filter; the new runtime
modules contain five and two tests respectively.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-gateway -p duduclaw-cli --all-targets
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib gateway_plan
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib isolation_mcp::runtime::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib runtime::isolation_gate::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib isolation_mcp::boot::tests
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib \
  channel_reply::cli_env::thinking_tests::l1_stub_claude_thinking_flows_by_policy -- --exact
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-gateway --lib \
  claude_runner::process_tree_cancellation_tests::webchat_claude_streaming_cancel_kills_grandchild -- --exact
```

#### New root-chain commands and evidence boundary

Detailed prerequisites and cleanup semantics are in
[`crates/duduclaw-spawnd/tests/README.md`](../../crates/duduclaw-spawnd/tests/README.md).
These tests require an already provisioned **disposable** Linux host with the
signed generation, idle confirmed employee, real gateway account, H image,
live B1/F/egress services, exact accepted Claude/shim ELF and actual provider
routing. The D8 debug fixture must be separately signed as
`Registered("iso-d8-fixture-v1")`; no acceptance or signature is synthesized.

The Claude test re-execs its driver as the real gateway UID with empty groups.
It uses the production main-plan factory and runtime registry through
A → spawnd → systemd → duduclaw-run → real Claude → real shim → D. Root verifies
UID/GID, namespaces, cgroup, executable digests, MCP socket and sealed fd 4/5
while a tool call is held. A test-only D echo callback checks independent user
and system nonces and returns an unpredictable response that must appear in the
Claude stream. It **does not** construct production gateway BootOwner or execute
a real product tool handler.

D8 uses a genuine double fork adopted by namespace PID 1. With init held by
pidfd and the main process exited, the orphan remains alive and a second run
must remain queued, have no I/O and refuse self-wait with `WouldDeadlock`.
After resuming init, observation of the second process requires the first unit
inactive and its cgroup empty via independent systemd readback, plus no employee
UID processes outside the second cgroup. This is a bounded repeated observation,
not a continuous trace of every teardown/admission transition. Panic cleanup
resumes held init before retiring only the test's recorded units.

Build on the acceptance builder (12 jobs follows its existing `build.sh`;
reduce if that machine is shared):

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd \
  --features root-test-fixture --test claude_chain --test d8_chain --no-run
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo build -p duduclaw-spawnd \
  --features root-test-fixture --bin duduclaw-spawnd-d8-fixture
```

Run from root on the prepared disposable host; **not run in this round**:

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --test claude_chain \
  iso_root_e2e_claude_installed_shim_tool_transport \
  -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd \
  --features root-test-fixture --test d8_chain \
  iso_root_e2e_d8_double_fork_residue_blocks_second_run \
  -- --ignored --exact --test-threads=1 --nocapture
```

For the supplied acceptance `build.sh`/`push.sh` convention, add
`--features root-test-fixture` to the spawnd build, collect both test executables
and the fixture in `exes.txt`, and preserve their absolute paths on the VM. The
fixture digest check uses `CARGO_BIN_EXE_duduclaw-spawnd-d8-fixture`; changing its
path breaks the test. With the runner's existing `ISO_EGRESS_ECHO_ADDR` and other
required environment set:

```sh
bash runall.sh /var/tmp/iso-i1-r5-chain \
  'iso_root_e2e_(claude_installed_shim_tool_transport|d8_double_fork_residue_blocks_second_run)$'
```

Require exactly two outer summary rows, each with `test result: ok. 1 passed`.
Internal self-exec helpers intentionally have no `iso_root_` prefix and must not
be selected independently. Passing these transport/retirement tests would still
leave production factory/ingress acceptance and the other runtimes/PTY open.
## Package I2 — installer and release integration (2026-11-05)

### Round 2 handoff (2026-11-05)

Status: **PARTIAL — inventory CI is repaired; authenticated inactive publication,
installer migration routing, first-admin claim, local readiness checks and
coordinated removal are implemented. Signed initial activation, G's platform
adapter and I1 composite readiness remain implementation blockers.** This section
supersedes the historical round-1 status below. No commit, cargo fmt, local
CLI/gateway/workspace build, release publication or live service mutation was
performed. The supplied worktree is retained for the integration owner.

#### Completed changes and enforcement boundaries

0. **Spawn inventory was repaired first.** All 34 previously unclassified sites
   now have written per-site reasons: 16 test-only, nine task/thread operations
   (not process launches), eight privileged-service launches and one existing
   ExecutionBackend adapter. `privileged_service` is restricted to explicit
   trusted service identities; it cannot exempt employee, operator or gateway
   execution. Root spawnd/trampoline, updater and migration-import launches stay
   outside the employee ExecutionBackend for their documented design §4.1 roles.
   The installer command runner's bounded capture uses `spawn`, kills/reaps on
   failure, and has the matching registry count. Final guard: **489 functions,
   zero migrations pending**. This proves coverage, not runtime isolation.
1. **F publication and packaging.** The archive now also contains a separate
   regular-file frozen `duduclaw-updated-launch`. F's parser filename is aligned
   with E (`duduclaw-parse-worker`). The migration executable is a required signed
   component. Optional offline `--bootstrap-root` / `--bootstrap-root-signatures`
   inputs produce the fixed bootstrap inbox alongside F's signed manifest and
   artifacts; signing keys remain refused in CI.

   `duduclaw-updated --bootstrap-stage` reads the root-owned fixed inbox
   `/etc/duduclaw-updated/bootstrap`. It checks the compiled
   `DUDUCLAW_BOOTSTRAP_ROOT_SHA256` pin, bootstrap root threshold signatures, F
   target signature, exact ten-component set, architecture/protocol, modes and
   sizes. It opens all artifact FDs, validates and copies from those same FDs,
   rehashes copies, fsyncs and atomically publishes an inactive generation under
   `/opt/duduclaw-isolated/generations/<digest>`. Retry revalidates existing bytes.
   An absent compiled pin refuses. This path **does not select `current`, create
   trust/slot receipts or updater state/floors, authorize the legacy dual-signature
   transition, publish mixed fixed prefixes, or execute/start any artifact**.
   It is authenticated staging, not a finished bootstrap installer.
2. **G and first administrator.** Protected install/upgrade from v1.79.3 (or an
   unknown floor) to v1.80+ enters G's durable transaction using the fixed,
   root-managed plan. Invalid paths, missing plan/platform or non-terminal output
   refuse; there is no legacy copy/apply fallback. G preflights its installed
   adapter before stopping the old service. Existing F-managed installations
   bypass legacy-floor migration and a reinstall only reads readiness, avoiding
   overwriting newer signed unit templates.

   G's `GatewayBootstrapIssuer` now uses a bounded, root-peer-only Unix socket
   owned by the gateway, with matching migration identity. The gateway requires
   an authenticated completed import manifest and its held singleton lock.
   Only the gateway accesses its UserDb. The local setup page receives the token
   through the URL fragment, has no remote assets and uses a restrictive CSP;
   POST requires a loopback peer and exact same-origin Host/Origin. Its real auth
   transaction validates email/password, consumes the one-use token and creates
   the first Admin with the chosen password atomically. Empty-user/recovery,
   token expiry/replay and existing failure/rate limits remain enforced.
   Already-claimed installer retries do not issue another first-admin token.
   Migration mode skips both default-admin creation and the old pending-account
   landing behavior.

   G now writes the durable root-owned
   `/etc/duduclaw/migration-import.json` boot fence after platform preflight and
   before the old service stop. Completion is written durably before `/run`.
   Startup requires matching durable/runtime manifests, rejects a runtime-only
   manifest and refuses incomplete import; a reboot clearing `/run` cannot
   restore the default-admin path. This does not implement the still-missing
   signed `duduclaw-migration-platform` or other G authority/materialization
   adapters, so actual end-to-end migration currently refuses at preflight.
3. **Readiness.** `service readiness --protected-linux` (also the `ops service`
   alias) is root/operator gated and read-only. It prints every group's result:
   executable ownership/modes/links, service identities, directories/parser roots,
   unit files/live loaded state, and composite isolation. Static IDs, distinct
   identities, locked passwords, auxiliary expiry, no supplementary groups or
   linger, G's root:root 0711 parent and gateway:gateway 0700 child home,
   exact unit bytes/fragment paths, no pending daemon reload and no
   unverified drop-ins are checked. Installer/reinstaller and a successful G
   handoff run the summary. `CompositeReadiness` is the explicit I1 integration
   seam; `MissingComposite` always fails, so no false readiness or enablement is
   reported. This is not the runtime probe/capability/storage/egress acceptance.
4. **Coordinated uninstall.** Isolation detection and a persistent retry marker
   route to the new coordinator. It inventories owned units, rejects unknown
   DuDuClaw unit names, durably backs up unit files and atomically masks them in
   `/etc/systemd/system` (a runtime mask would lose to an existing `/etc` unit).
   After daemon-reload and mask readback it stops admissions/timers, gateway and
   workers, proves inactive units, empty cgroups and no relevant processes or
   mounts, then removes the fixed installation binaries/helpers. Failures before
   quiescence never reach removal. Unit-template masks are verified on disk,
   since systemd cannot load an uninstantiated template for state readback.

   It preserves service accounts, employee allocations/tombstones, data, keys,
   version floors, updater state/trust/generations/selectors, durable masks and
   unit backups. Repeated removal uses the retained marker and never falls into
   legacy uninstall. No purge, lazy unmount or forced process termination is
   hidden in removal. Mount checking covers the fixed installation/agent roots;
   **H's arbitrary configured storage roots still need an authoritative inventory
   adapter before complete storage-aware uninstall acceptance**.

Shared `duduclaw-isolation` wire formats, canonical bytes and limits were not
changed. The bootstrap exchange is a new private G/gateway protocol, not a shared
contract revision. CLI adds an existing workspace migration dependency (Linux
only); no third-party dependency was added.

#### Round 2 observed validation

Cargo runs below used `CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0`; Linux target checks
also used `LIBSQLITE3_SYS_USE_PKG_CONFIG=1`. Neither CLI nor gateway crate was built.
Counts describe each final suite once; subset reruns are not added to totals.

| Command (environment prefixes above apply to Cargo) | Observed result |
| --- | --- |
| `python3 scripts/ci/check_spawn_inventory.py --require-migrated` | 489 classified, zero pending |
| `python3 -m unittest discover -s scripts/ci -p 'test_spawn_inventory.py' -v` | 17 passed |
| `rustc --edition=2021 --test scripts/tests/protected_layout_harness.rs -o /private/tmp/iso-i2-layout-tests` then `/private/tmp/iso-i2-layout-tests` | 12 passed; actual layout/lifecycle code, including fake-root migrated-home ownership/modes/links, durable mask/backup/retry/refusal and fail-before-removal checks |
| `cargo test -p duduclaw-updated --offline` | 37 passed, one ignored on macOS |
| `cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-updated --offline` | Passed, including Linux-only bootstrap tests; not executed on Linux |
| `cargo test -p duduclaw-migration --offline` | 44 passed (42 unit, two integration) |
| `cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-migration --offline` | Passed |
| `cargo test -p duduclaw-auth otp::tests --offline` | 19 passed |
| `cargo test --manifest-path /private/tmp/iso-i2-bootstrap-harness/Cargo.toml --offline` | Three passed: actual page/origin tests and actual HTTP claim handler with real UserDb; AppState/import-manifest seams are harness stubs |
| `cargo check --tests --target x86_64-unknown-linux-gnu --manifest-path /private/tmp/iso-i2-bootstrap-harness/Cargo.toml --offline` | Passed: actual full installer/layout/lifecycle/migration/readiness/uninstall modules and gateway's new Linux socket/start code; real core/G/F/isolation libraries |
| `python3 -m unittest discover -s scripts/tests -p 'test_package_linux_release.py' -v` | 17 passed |
| `python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v` | 20 passed, one skipped (macOS lacks systemd-analyze) |
| `python3 -m unittest discover -s scripts/tests -p 'test_release_*.py' -v` | 75 passed |
| `python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v` | Four passed |
| `bash -n scripts/release.sh`; `sh -n scripts/linux-protected/auto-upgrade/install.sh`; `git diff --check` | Passed |
| Three-language documentation link/fence/marker checks | Three guides, nine links passed |

The standalone Linux harness stubs only installer parent constants/options/unit
text/account validation, legacy copy and gateway version lookup; its gateway
side also stubs AppState/manifest and Linux UserDb types. An initial Linux auth
cross-check failed because aws-lc requires the absent `x86_64-linux-gnu-gcc`;
using the explicitly scoped UserDb type stub is **not** full gateway/CLI compile
acceptance. The macOS HTTP test uses real auth. Temporary harness/evidence is
under `/private/tmp/iso-i2-bootstrap-harness` and `/private/tmp/iso-i2-*.log`.
No native root systemd, socket, browser or release-binary test was run.

Independent ordinary code review identified and drove fixes for persistent-mask
precedence, managed reinstall overwrites, repeat-uninstall legacy fallback,
volatile reboot fencing, singleton-lock ownership and readiness matching G's
root-owned parent/private gateway home. The final ordinary review found no
unresolved HIGH/CRITICAL in the reviewed changes. Ordinary review and
fixture tests do not constitute formal `gpt-daybreak-blue-latest` security signoff;
that review has not been obtained.

#### Remaining implementation and remote gates

- Bind real offline bootstrap keys/pin and legacy dual signatures; implement the
  authenticated initial activation transaction, trust bundle, A/B receipts and
  updater state/floor. Resolve F `/opt/duduclaw-isolated/current` versus G
  `/usr/lib/duduclaw/current` and B/E/legacy fixed prefixes without unsigned
  copying or symlink shortcuts. Do not activate a mixed generation.
- Supply the signed G platform adapter and runtime/storage/authority materializers,
  then verify quarantine/import, pending confirmations and first-admin setup.
- Wire I1 composite readiness and verified DNS/unit configuration; no current
  readiness command can return overall success with `MissingComposite`.
- Connect H's complete storage inventory to removal; run real fresh-install,
  retry/reinstall and removal acceptance. The fixed-path coordinator exists but
  does not prove arbitrary configured storage is unmounted.
- Build actual GNU x86_64/aarch64 releases, verify glibc and real offline signatures,
  obtain formal security review and run the supported native kernel matrix.
  GCP kernel 7.0 / Landlock ABI 8 remains refused; I2 did not widen support.

Exact remote commands (not local build or acceptance evidence):

```sh
cargo check -p duduclaw-cli -p duduclaw-gateway --tests
cargo test -p duduclaw-cli service_protected_
cargo test -p duduclaw-cli spawnd_install_tests
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
cargo test -p duduclaw-gateway migration_setup_
cargo test -p duduclaw-gateway migration_import_boot_
cargo test -p duduclaw-auth otp::tests
cargo test -p duduclaw-migration
cargo test -p duduclaw-updated
# Root, disposable native VM only; inspect each fixture's prerequisites first:
cargo test -p duduclaw-updated iso_root_bootstrap_ -- --ignored --test-threads=1
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-cli iso_root_install_support_staging_and_idempotent_readback -- --ignored --test-threads=1
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-cli iso_root_install_uninstall_preserves_recovery_and_is_repeatable -- --ignored --test-threads=1
# Build web/dist first; no signing keys in CI:
bash scripts/release.sh linux-assets --target x86_64-unknown-linux-gnu --output-dir dist-linux-x64
bash scripts/release.sh linux-assets --target aarch64-unknown-linux-gnu --output-dir dist-linux-arm64
```

Live first-admin acceptance must include non-root issuer refusal, held-lock
checks, wrong-origin/non-loopback/replay denial, reboot with `/run` cleared,
already-claimed installer retry and the actual SSH-tunnel setup page.

#### Round 2 file inventory (round 1 remains uncommitted)

| File | Round 2 responsibility |
| --- | --- |
| `scripts/ci/check_spawn_inventory.py` | Validate narrowly scoped privileged-service classifications |
| `scripts/ci/privileged_effects.json` | Classify 34 missing sites and updated bounded installer spawn |
| `scripts/ci/test_spawn_inventory.py` | Reject privileged exemptions for gateway/employee identities |
| `scripts/package-linux-release.py` | Include frozen launcher and optional verified bootstrap inbox inputs |
| `scripts/tests/test_package_linux_release.py` | Bootstrap input, signing and artifact inventory regressions |
| `crates/duduclaw-updated/src/publication.rs` | Shared required artifact names and complete signed-set validation |
| `crates/duduclaw-updated/src/linux/bootstrap.rs` | Root-pinned, same-FD inactive publication and ignored native fixtures |
| `crates/duduclaw-updated/src/linux/service.rs` | Correct parser artifact name through shared mapping |
| `crates/duduclaw-updated/src/linux/mod.rs` | Expose bootstrap staging |
| `crates/duduclaw-updated/src/main.rs` | Fixed root-only bootstrap-stage entry point |
| `crates/duduclaw-updated/src/lib.rs` | Export publication policy |
| `crates/duduclaw-updated/README.md` | Document staging and outstanding activation boundary |
| `crates/duduclaw-cli/src/service/protected_linux_layout.rs` | Match G's root-owned parent/private gateway home and test ownership/mode/link refusals |
| `crates/duduclaw-cli/src/service/protected_linux_lifecycle.rs` | Removal inventory/order/quiescence policy and durable fake-root-tested masks |
| `crates/duduclaw-cli/src/service/protected_linux_uninstall.rs` | Native coordinated removal, retained recovery state and ignored VM fixture |
| `crates/duduclaw-cli/src/service/protected_linux_readiness.rs` | Five-group local readiness and explicit missing composite seam |
| `crates/duduclaw-cli/src/service/protected_linux_migration.rs` | Fixed-plan installer handoff to G and one-time admin link |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Bounded command capture, managed routing, readiness and removal wiring |
| `crates/duduclaw-cli/src/service/protected_linux.rs` | Register lifecycle/migration/readiness entry points |
| `crates/duduclaw-cli/src/service/protected_linux_tests.rs` | Readiness command option/operator-gate regressions |
| `crates/duduclaw-cli/src/lib.rs` | CLI readiness parser and dispatch |
| `crates/duduclaw-cli/Cargo.toml`, `Cargo.lock` | Existing migration crate as Linux-only CLI dependency |
| `scripts/tests/protected_layout_harness.rs` | Compile actual layout and lifecycle modules without CLI build |
| `crates/duduclaw-auth/src/otp.rs` | Transactional token claim with chosen-password first Admin and tests |
| `crates/duduclaw-migration/src/bootstrap_protocol.rs` | Bounded private bootstrap request/reply and boot-fence location |
| `crates/duduclaw-migration/src/root_bootstrap.rs` | Peer-checked gateway issuer and already-claimed handling |
| `crates/duduclaw-migration/src/root_pipeline.rs` | Durable pre-stop/completion boot fence |
| `crates/duduclaw-migration/src/import_manifest.rs` | Durable/runtime consistency selection and reboot tests |
| `crates/duduclaw-migration/src/lib.rs` | Export private bootstrap protocol |
| `crates/duduclaw-migration/src/bin/duduclaw-migrate.rs` | Use real gateway bootstrap issuer |
| `crates/duduclaw-migration/README.md` | Explain implemented issuer/claim and platform gap |
| `crates/duduclaw-gateway/src/server_migration_bootstrap.rs` | Root-peer socket, local setup route and actual auth claim |
| `crates/duduclaw-gateway/src/migration_setup.html` | Local three-language first-admin form and fragment token |
| `crates/duduclaw-gateway/src/migration_startup.rs` | Read durable boot fence and reject inconsistent/incomplete imports |
| `crates/duduclaw-gateway/src/server.rs` | Migration startup, default-admin exclusion and route/listener wiring |
| `docs/guides/linux-protected-service.md`, `docs/guides/zh-TW/linux-protected-service.md`, `docs/guides/ja-JP/linux-protected-service.md` | Matching staging/migration/readiness/removal behavior and limitations |
| `docs/features/05-security-defense.md`, `docs/features/zh-TW/05-security-defense.md`, `docs/features/ja-JP/05-security-defense.md` | Matching evidence-bounded security behavior |
| `CHANGELOG.md` | User-facing round-2 changes without completed isolation claim |
| `docs/architecture/native-linux-isolation.md` | Current handoff, evidence and remaining integration gates |

### Round 1 historical handoff (superseded above)

Status: **PARTIAL — release packaging and installation prerequisites are implemented;
full fresh installation, migration activation and removal are not implemented.**
This is a coherent staging-only subset within the delegated time/build limits.
The existing worktree/branch was retained; no commit, formatter, release, service
operation, other-worktree write or production-home access was performed.

### Implemented behavior

- Both GNU Linux release targets build nine explicit production binaries: gateway,
  spawnd, trampoline, egress, updater, parser with `engine`, importer, migration
  command and MCP bridge. The archive also includes a separate regular-file
  egress-forwarder copy of the egress ELF and the Python SDK. Root test fixtures
  are never selected. Every binary must be nonempty and strictly below 200 MiB;
  all actual release binaries go through the existing glibc baseline checker.
  Archive names, SHA-256 output and workflow minisign/cosign steps are retained.
- `scripts/release.sh linux-assets --target <triple>` shares the workflow helper.
  A single frozen input set feeds the tarball and optional F raw assets. With
  `--manifest-template <reviewed JSON> --signing-key <offline target key>`, the
  helper delegates canonical signing to F's existing script. Missing keys print
  an explicit skip; CI refuses a key before reading it. This does not generate
  a bootstrap root, release policy template or migration dual signatures.
- The old optional B1 support hook now checks the complete fixed-path publication
  before staging 15 units, 15 root-owned directories and four auxiliary static
  accounts, plus checking the gateway account. It **does not publish binaries**.
  `protected_linux_layout.rs` records the paths expected by the current package
  templates. Every executable and ancestor is checked without following links;
  executable files require root:root, exact 0755, one link and nonzero size.
  These checks do not authenticate a release; signed bootstrap must publish first.
- System IDs must be nonzero and strictly below explicit `SYS_UID_MAX` and
  `SYS_GID_MAX`, whose values must be below DynamicUser's range. New accounts
  use a capped max-minus-one allocation, no subordinate ranges, and auxiliary
  accounts have locked/expired passwords, nologin, no home or supplementary
  groups, and disabled linger. Service UID/GID pairs must be distinct. Existing
  incompatible accounts refuse; this hook does not silently repurpose them.
- The two parser roots start root:root 0755 and empty. Reinstall accepts only the
  bounded empty mountpoint skeleton that systemd can leave; files, symlinks,
  unexpected descendants, writable directories and different devices refuse.
  It never recursively removes unexpected contents. SSH/cron/at employee policy
  remains with spawnd; no second implementation was added here.
- Support units are staged only and are not enabled. No DNS profile, runtime
  publication, accepted-generation receipt, first-admin URL or readiness success
  is invented. Existing v1.80 fresh/cross-version migration guards remain intact.
  A legacy reinstall now refuses when F's state/current exists, before modifying
  that managed generation. Legacy uninstall refuses any published isolation
  component or F state/current before removing anything; pure legacy removal is
  unchanged. Full coordinated removal is still outstanding.
- Nightly requests already used F's frozen launcher. The timer installer now
  checks its root ownership, ancestors and executable/single-link status before
  installation. Exact updater `active` state is checked before installation and
  again before enabling. It does
  not request an update while checking. Active is a timer prerequisite, not
  composite isolation or update-success evidence.
- Three-language guides now describe the planned architecture, real migration
  command interface and confirmation page, explicit implementation gaps, and
  the existing package-level kernel matrix. Kernel 7.0 / Landlock ABI 8 remains
  refused. No new supported-kernel claim was added by I2.

### Validation in this worktree

No CLI/gateway/workspace Cargo build or test was run, as explicitly prohibited.
The standalone Rust harness compiles the **actual std-only layout module**, not
an alternative implementation; it does not type-check the full Linux installer.
The ignored root fixture was written, not executed or compiled as part of CLI.

| Actual command | Result |
| --- | --- |
| `rustc --edition=2021 --test scripts/tests/protected_layout_harness.rs -o /private/tmp/iso-i2-layout-tests` then `/private/tmp/iso-i2-layout-tests` | 6 passed, 0 failed; real temporary-filesystem ownership/mode/link/incomplete-publication/skeleton tests plus policy/template checks |
| `python3 -m unittest discover -s scripts/tests -p 'test_package_linux_release.py' -v` | 12 passed, 0 failed; both archive inventories, exact 200 MiB boundary, offline OpenSSH signing and CI refusal |
| `python3 -m unittest discover -s scripts/tests -p 'test_protected_auto_upgrade.py' -v` | 20 passed, 0 failed, 1 skipped because macOS has no systemd-analyze |
| `python3 -m unittest discover -s scripts/tests -p 'test_release_*.py' -v` | 75 passed, 0 failed (packaging agent) |
| `python3 -m unittest discover -s scripts/tests -p 'test_sign_isolation_manifest.py' -v` | 4 passed, 0 failed (packaging agent) |
| `bash -n scripts/release.sh`; `sh -n scripts/linux-protected/auto-upgrade/install.sh`; `git diff --check` | Passed |
| Existing `/private/tmp/iso-a-syntax/target/debug/iso-a-syntax` on the three changed installer Rust modules | 3 files parsed, 0 syntax errors; not type checking |
| Three-guide link/marker/fence checks | Passed: 3 guides, 9 relative links (documentation agent) |
| `python3 scripts/ci/check_spawn_inventory.py --require-migrated` | FAILED: 34 unclassified symbols; reconstructing changed-file sites from HEAD reproduced the identical 34 failures, so no I2 regression or passing whole-project inventory is claimed |

Initial standalone test compilation directly at crate root failed because the
module has `pub(super)` visibility; the checked-in parent harness fixes that.
One intermediate macOS test failed because setting setuid did not retain that bit
in this environment; the setuid filesystem assertion is now Linux-only, while
non-executable/group-write/ownership/link checks run on macOS too. A parallel fixture initially collided on a coarse timestamp; an atomic sequence now
separates temporary roots. No production permission check was weakened. Independent ordinary code review drove fixes for
gateway allocation at the inclusive useradd limit, parser-root reuse, signed-asset
name collisions and relative CARGO_TARGET_DIR resolution. This is not a formal
Daybreak security review. No new dependency or shared isolation contract change.

### Required next steps (implementation, not just acceptance)

1. Supply independently authenticated bootstrap publication of every binary and
   F's trust bundle, slot receipts, initial state and generation. Resolve current
   package path mismatches: F publishes `/opt/duduclaw-isolated/current`, whereas
   G/import templates expect `/usr/lib/duduclaw/current`; spawnd, parser, egress
   and the old gateway still use separate fixed prefixes. F's ParseWorker target
   currently installs `duduclaw-work`, whereas E executes `duduclaw-parse-worker`.
   The layout refuses symlink ancestors, so simply linking those prefixes is not
   an implementation. Do not activate a mixed generation.
2. Implement G's `duduclaw-migration-platform`, signed preflight/final readbacks,
   employee allocation/materialization, first-admin issuer/claim route and the
   remaining G authority adapters. Wire installer → quarantine → import → pending
   confirmation. I2 intentionally kept the v1.80 refusal instead of copying legacy
   databases or using an unsigned in-place switch.
3. Complete signed unit activation and root-selected DNS configuration/drop-ins,
   storage provisioning and the composite readiness checker/summary. The current
   hook stages files only; a daemon's `active` state is insufficient.
4. Implement coordinated uninstall: stop admissions/timers/workers, prove empty
   cgroups and no live mounts, remove the owned installation set and preserve
   permanent identity mappings, data, root keys, trust and recovery material.
5. Run remote CLI/gateway compilation, actual GNU x86_64/aarch64 release builds,
   glibc checks, real offline hardware signatures, fresh install/reinstall/removal
   and v1.79.3 migration on the supported kernel matrix. The new ignored test
   proves staging only, not complete fresh-install acceptance. Obtain formal
   implementation security review. Reconcile the existing spawn registry failures
   in the integration owner’s scope before CI can be green.

Remote commands (not executed locally):

```sh
cargo check -p duduclaw-cli -p duduclaw-gateway --tests
cargo test -p duduclaw-cli service_protected_
cargo test -p duduclaw-cli spawnd_install_tests
cargo test -p duduclaw-cli legacy_protected_upgrade_cannot_bypass_migration
# Root, disposable native VM, complete trusted binary publication already present:
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-cli iso_root_install_support_staging_and_idempotent_readback -- --ignored --test-threads=1
# Build web/dist first. No signing keys in CI:
bash scripts/release.sh linux-assets --target x86_64-unknown-linux-gnu --output-dir dist-linux-x64
bash scripts/release.sh linux-assets --target aarch64-unknown-linux-gnu --output-dir dist-linux-arm64
```

### Changed-file inventory

| File | Change |
| --- | --- |
| `.github/workflows/release.yml` | Build/package all Linux components through one helper; preserve archive signing |
| `.github/workflows/release-cache-warm.yml` | Warm the matching production binary build set |
| `scripts/release.sh` | Add explicit local `linux-assets` entry point without publishing |
| `scripts/package-linux-release.py` | Build, freeze, validate, archive and optionally offline-sign Linux assets |
| `scripts/tests/test_package_linux_release.py` | Packaging, missing-input, size, signing and target-directory regressions |
| `scripts/linux-protected/auto-upgrade/install.sh` | Refuse timer installation without trusted frozen launcher and active updater |
| `scripts/linux-protected/auto-upgrade/README.md` | Explain timer prerequisites and evidence boundary |
| `scripts/tests/test_protected_auto_upgrade.py` | Fake-root prerequisites and stopped-during-install regressions |
| `crates/duduclaw-cli/src/service/protected_linux.rs` | Register the shared fixed-layout module |
| `crates/duduclaw-cli/src/service/protected_linux_install.rs` | Complete published-support staging, static accounts, parser-root reuse and legacy-operation refusal |
| `crates/duduclaw-cli/src/service/protected_linux_layout.rs` | Fixed binary/unit/directory inventory, range and filesystem validation, six tests |
| `scripts/tests/protected_layout_harness.rs` | Compile actual installer policy without building CLI/gateway |
| `docs/guides/linux-protected-service.md` | Rewrite English architecture, migration, readiness and recovery guide |
| `docs/guides/zh-TW/linux-protected-service.md` | Matching Traditional Chinese guide |
| `docs/guides/ja-JP/linux-protected-service.md` | Matching Japanese guide |
| `docs/features/05-security-defense.md` | Align English native isolation state and limitations |
| `docs/features/zh-TW/05-security-defense.md` | Align Traditional Chinese security description |
| `docs/features/ja-JP/05-security-defense.md` | Align Japanese security description |
| `CHANGELOG.md` | User-facing v1.80 preparation and changed timer/legacy-operation behavior |
| `docs/architecture/native-linux-isolation.md` | This evidence, exact implementation gaps and remote handoff |
