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
