# Native Linux isolation implementation

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
