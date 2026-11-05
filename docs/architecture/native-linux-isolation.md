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
