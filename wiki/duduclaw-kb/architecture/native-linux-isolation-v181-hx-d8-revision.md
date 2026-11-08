# HX: in-run helper processes for execution-type tools (D8 revision, option A)

Status: design for review, 2026-11-08 (task HX1). Base `feat/iso-v181-20261108`. Needs an independent security
review (Daybreak) and owner confirmation before HX2–HX6 code. Line references are to this worktree; anything not
read in code is marked "(inferred, verify)".

## 1. Problem and constraints

- **D8** (spawnd admission): at most one capability-holding run per employee. A second same-employee run queues;
  a same-employee wait returns `WouldDeadlock` (`duduclaw-spawnd/src/admission.rs:249-255`; spawnd README:5).
- Package H D-H5 proposed child runs that start after the parent exits (accepted/pending job, today's bridge at
  `duduclaw-mcp-boundary/src/execution.rs:84-160`, result `"status":"accepted","completion":"pending"` at :151).
  **Owner rejected D-H5**: `execute_program`, `code_map`, `office_script`, `browser_record_start/stop` must return
  results synchronously in protected mode.
- **Owner option A**: the tool runs as a bounded helper process inside the calling run, started by an in-run
  helper launcher that the trampoline init forks (template: egress forwarder branch,
  `duduclaw-run/src/linux/runner.rs:389-403`). D8 is preserved; the MCP call blocks until the helper exits;
  helper timeout ≤ remaining run time; `OOMPolicy=continue`; the helper allowlist only narrows the parent profile;
  egress helpers need an Egress parent; a recording lives at most as long as the calling run, is flushed at run end,
  and a later stop returns the stored result. Pending-job paths for the three offline tools are dropped.

Design invariant used throughout: **helper authority ⊆ calling runtime's authority** (syscalls, files, network,
identity). Helpers gain no capability, credential, MCP channel or lifetime the run does not already have. This is
what makes sharing the run's UID and namespaces acceptable.

## 2. Architecture

```
AI runtime ──fd3 MCP──▶ gateway (MCP boundary, capability, tool policy)
                              │ SpawnRequest::HelperCall{run = record.run, …} + sealed input memfd
                              ▼
                        spawnd (root) ──per-run control socketpair──▶ launcher (inside run, employee uid)
                              ▲  stdout/stderr pipe read ends                 │ fork + sandbox + execveat
                              └──────────── gateway reads, decodes ◀──────────┘ helper (fd1/fd2 = pipe write ends)
```

### 2.1 Who forks the launcher, and when
`child_init` today: drop identity/caps, NNP (`runner.rs:368-375`) → `restrict_filesystem` (:377) → common seccomp
(:379-380) → fork forwarder (:389-403) → fork runtime (:404-411) → wait for isolation receipts (:424) → init filter
(:443) → reap loop (:446-458).

HX inserts the launcher fork **after `PrivilegesVerified` (:375) and before `restrict_filesystem` (:377)**, so the
launcher and the runtime get different Landlock domains:
- The launcher child installs its own Landlock (run paths + `/helpers` RX + `/helper-tmp` RW), then common seccomp,
  then its self-filter F_L (§2.4), marks isolated bit 4 in the `FailurePage` (`runner.rs:272-310`; `all_isolated`
  must require bit 4 when a launcher is planned), closes the status fd, and serves.
- Init continues as today; the runtime's `restrict_filesystem` list (:462-478) does **not** contain `/helpers` or
  `/helper-tmp`, so the runtime cannot open, list or exec helper runtimes or helper scratch.
- Recommended (open question Q1): the launcher is an in-process function of the trampoline binary
  (`duduclaw-run/src/linux/launcher.rs::serve`), not an exec'd ELF. Without `execve` it stays non-dumpable
  (`privileges::non_dumpable`, :373), so a same-UID runtime cannot open `/proc/<launcher>/mem`; the forwarder, by
  contrast, is exec'd and becomes dumpable again (inferred, verify; Yama `ptrace_scope` is the only extra guard).
- Before dropping privileges (still root in the trampoline), set `oom_score_adj=-1000` on init and the supervisor
  so `OOMPolicy=continue` cannot leave a run without its PID 1 (inferred, verify the supervisor shares the cgroup).

The launcher holds only: its control socket, the helper runtime exec fds, and (option Q2-A) a pre-opened
`hx/cgroup.procs` fd. It drops prompt, attachments, stdin, MCP, PTY slave and the egress socket exactly like the
forwarder child (`runner.rs:394-396`).

### 2.2 Request channel
- **The runtime gets no launcher fd.** Requests reach the launcher only as authorized MCP calls relayed by the
  gateway and spawnd. A runtime-held socketpair was rejected: it would bypass capability, tool policy, approvals,
  revocation and audit, and make the launcher parse attacker bytes.
- spawnd creates one `SOCK_SEQPACKET|SOCK_CLOEXEC` socketpair per run (same as the bootstrap pair,
  `spawnd/src/native_trampoline.rs:131-133`), keeps one end in the run's `Pending` entry, and passes the other in
  the bootstrap rights after the forwarder fds (`TrustedRun::expected_fd_count`, `runner.rs:50-53`, gains
  `1 + helper exec fds + helper mount fds`; `MAX_BOOTSTRAP_FDS = 80`, `linux/bootstrap.rs:8`).
- Wire (new pure module `duduclaw-run/src/helper_wire.rs`, host-testable): frames ≤ 4 KiB, fixed binary layout,
  `DCH1` magic, ≤ 4 fds per frame via SCM_RIGHTS, close on any malformed frame.
  - spawnd → launcher: `Start{call: [u8;16], helper: u8 (index into the signed helper table in TrustedRun),
    timeout_ms: u64, mode: Foreground|Recording}` + fds `[input memfd, stdout w, stderr w]`; `Stop{call}` (recording:
    write `stop\n` to helper stdin, the driver protocol at `recording_driver.cjs:11-18`); `Kill{call}`.
  - launcher → spawnd: `Started{call}`, `Ended{call, kind: Exited|Signalled|TimedOut|OomKilled|StartFailed, code}`,
    `Busy{call}`, `Error{code}`.
  - No names, paths, argv or env cross the wire; the helper is an index into root-signed data.
- Concurrency: **one foreground helper + one recording per run**; further calls wait in a spawnd FIFO of depth 4,
  each bounded by its own deadline, then `HelperBusy` (open question Q4).

### 2.3 Validation against the signed helper-runtime manifest
- Helper runtimes stay what Package H already publishes: `RuntimeRecord`s with `RuntimeKind::Registered(id)`
  (`spawnd/src/installed.rs:96-110`) signed via `scripts/sign-isolation-manifest.py --kind runtime`
  (namespace `duduclaw-runtime-manifest-v1@dudustudio`, :20) and bound by sha256 in the release manifest. Exact ids
  in code: `duduclaw-program-v1` (`program_helper.rs:6`), `duduclaw-code-map-v1` (`code_map_helper.rs:3`),
  `duduclaw-office-<skill>-<script>-v1` (`isolation_execution_program.rs:51`), `duduclaw-browser-record-v1`
  (`recording_helper.rs:4`). (Package H's text says `code-map-v1` / `browser-record-v1`; the code names win.)
- New: the **parent** profile's signed `NativeRecord` (`installed.rs:187-192`) gains
  `launcher: Option<LauncherRecord { helpers: Vec<HelperBinding> }>`; `HelperBinding = { profile: ProfileId,
  seccomp: RuntimeProfile, seccomp_digest, memory_bytes, tasks, cpu_quota_percent, timeout_ms, filesystem:
  HelperFs }`. This list is the helper allowlist; it is per parent profile, signed, and never derived from IPC.
- Publication-time checks (in `installed.rs` bundle validation, :238-252, and `RootPolicy::from_verified_publication`,
  `policy.rs:41-69`), fail closed:
  1. the helper `RuntimeRecord` exists, is entitled to the employee, `!mcp && !pty_allowed`, and passes today's
     `valid_helper` rules (`isolation_execution_program.rs:57-84`);
  2. `seccomp(helper) ⊆ seccomp(parent) ∪ L_restrict` (§2.4);
  3. `memory_bytes`, `tasks`, `cpu` ≤ parent `ResourceLimits` minus a signed runtime reserve; `timeout_ms` ≤ parent
     `runtime_ms − termination_grace_ms − RESULT_GRACE_MS` (`isolation_execution_program.rs:13`);
  4. helper `NetworkPolicy::Egress(p)` ⇒ parent `Egress(p)` with the same profile; `Offline` always allowed;
  5. at most 16 helper bindings per parent, helper mounts deduplicated by registry mount (fd budget).
- Per call, spawnd re-derives the helper with `RootPolicy::derive(employee, kind, false)` (`policy.rs:70-84`), maps
  it to the index in the run's `TrustedRun` helper table, and refuses if absent. The launcher re-verifies the exec fd
  identity against the registry mount (`verify_executable`, `runner.rs:170-177`) before every exec.

### 2.4 Helper sandbox
| Layer | Mechanism |
|---|---|
| Identity | Employee UID/GID inherited from init after `clear_capabilities` + securebits + NNP (`runner.rs:369-374`). No capability can be regained; the launcher never runs as root. |
| Seccomp | Launcher self-filter F_L = parent runtime profile ∪ L_restrict, where L_restrict = {`landlock_*`, `seccomp`, `prctl`, `prlimit64`, `setpgid`, `pidfd_open`, `pidfd_send_signal`, `waitid`, `timerfd_*`} (calls that only restrict or supervise). The child installs F_H (signed helper profile, built with `seccomp::runtime`, `seccomp.rs:198-211`) immediately before `execveat`. Kernel stacking yields F_L ∩ F_H, so the helper can never exceed the parent; common refusals (namespaces, mount, ptrace, bpf, io_uring, `pidfd_getfd`; `seccomp.rs:98-133`) are embedded in both. |
| Namespaces | Shares the run's mount/PID/net/IPC namespaces. Per-helper namespaces are impossible after the common filter (`clone` namespace flags and `unshare`/`setns` denied, `seccomp.rs:14-15,114-118`), and a root launcher was rejected. Separation from the runtime is by Landlock, seccomp, process group and cgroup. |
| Filesystem | Child Landlock (stacked on the launcher's): `/usr` `/lib*` RX, `/helpers/<id>` RX, `/etc` RO (CA store for recording), `/proc` RO, `/dev` (null/urandom), `/helper-tmp/<call>` RW, and `/work` RW for program/code_map/office (`/work/recordings` only for recording). **Never** `/home/employee` (runtime credentials, inferred, verify), `/config` (MCP projection), `/run`, `/runtime`. `/work` is the calling run's storage-lease mount (`spawnd/src/storage/adapter.rs:152-176`); no new mount is made per call. |
| Helper runtimes | Each helper registry mount is attached read-only, `nosuid,nodev` at `/helpers/<id>` by `build_root` (`mounts.rs:139-192`, like `runtime` at :158). Helper binaries change their hard-coded `/runtime/...` paths (`program_helper.rs:31-37`, `office_helper.rs:76-91`, `recording_helper.rs:93,125`, code-map likewise) to the signed helper root carried in fixed argv. |
| Scratch | New tmpfs `/helper-tmp` (size from the parent's tmpfs budget), absent from the runtime's Landlock list; the launcher creates `/helper-tmp/<call>` 0700 and removes it after the call. Replaces the fixed `/tmp/recording` (`recording_helper.rs:101`), which a same-UID runtime could pre-create and which blocks a second recording in one run. |
| cgroup | Q2-A (recommended): unit gets `Delegate=yes`; the root trampoline creates leaf cgroups `rt/` (supervisor, init, runtime, forwarder, launcher) and `hx/` with `memory.max`, `pids.max`, `cpu.max` from the binding, and opens `hx/cgroup.procs` before dropping privileges; the launcher moves each helper in through that fd (open-time credentials, kernel ≥ 5.16 or the CVE-2021-4197 backport; inferred, verify on Ubuntu 22.04 5.15) and uses `hx/cgroup.kill` to reap escaped descendants. Always also: helper `oom_score_adj=1000`. |
| Rlimits | `RLIMIT_CORE=0`, `RLIMIT_NOFILE` and `RLIMIT_FSIZE` from the binding. No `RLIMIT_AS` (breaks V8/Chromium); no `RLIMIT_NPROC` (counts the whole employee UID). |
| Process | `setsid`, `PR_SET_PDEATHSIG=SIGKILL`, `close_range(5, ∞)` before exec; fds 0 (`/dev/null`, or the recording stdin pipe), 1/2 (spawnd pipes), 4 (sealed input memfd, checked by `helper_input_args`, `program_helper.rs:317-341`). Helper Rust entries call `PR_SET_DUMPABLE 0` first. No MCP fd, no control socket, no status pipe, no egress socket. |
| Network | Offline helper: F_H omits `socket`, so `common_rules` turns every family into EPERM (`seccomp.rs:124-131`); the run netns has only loopback (`mounts.rs:517`). Egress helper (recording): `socket` allowed; reachability is the parent's forwarder on `127.0.0.1:<port>` (`policy.rs:93-107`), already enforced by `recording_helper.rs:96-100`; the egress host keeps public-address-only policy. |

### 2.5 Results, timeout, run end, OOM
- **Result**: spawnd creates stdout/stderr pipes per call, passes write ends to the launcher, returns read ends to
  the gateway (`FdRole::StdoutRead/StderrRead`, `ipc.rs:184`). The gateway reads ≤ the helper record's
  `output_total_bytes` and decodes with today's strict decoders (`decode_result`,
  `isolation_execution_program.rs:157-164`: stderr must be empty, outcome `Exited(0)`). Files go to `/work`.
- **Timeout** = min(tool timeout, binding `timeout_ms`, run deadline − now − termination grace − 5 s). spawnd
  computes the run deadline from admission (unit start + `RuntimeMaxUSec`, `systemd.rs:90`) and puts the effective
  value in `Start`; the launcher enforces it with a timerfd and `kill(-pgid, SIGKILL)` (+ `hx/cgroup.kill`). If the
  effective value is < 5 s the call is refused with `run_time_exhausted` instead of starting; otherwise the
  response reports `effective_timeout_ms`.
- **Run end**: (a) runtime exits → init (`runner.rs:446-458`) sends `SIGTERM` to the launcher (init filter already
  allows `kill`/`wait4`, `seccomp.rs:156-166`) and keeps reaping until the launcher exits, then exits with the
  runtime's stored status; the launcher kills foreground helpers at once and flushes a recording within
  F = `termination_grace_ms − 2 s` before `SIGKILL`. (b) spawnd cancel / `RuntimeMaxUSec` → systemd SIGTERMs the
  cgroup (`KillMode=control-group`, `systemd.rs:75`), the launcher sees it directly, same flush; hard SIGKILL after
  `TimeoutStopUSec` (:92). (c) Init exit tears down the PID namespace, killing every helper regardless.
- **Launcher death** is not fatal to the run (unlike the forwarder, `runner.rs:450-453`): spawnd sees control-socket
  EOF and fails later calls with `helper_unavailable`; PDEATHSIG and `hx/cgroup.kill` remove orphans.
- **OOM**: unit property `OOMPolicy=continue` (add to `systemd.rs:74-93` and to the start readback, :298-320).
  A helper OOM is reported as `Ended{OomKilled}` (from `memory.events` of `hx/` under Q2-A, otherwise SIGKILL +
  `oom_score_adj` heuristic) and the run continues.

## 3. MCP tool call path (protected mode)

1. Runtime sends `tools/call execute_program` on fd 3. The boundary runs the shared gates unchanged
   (`tool_policy::authorize_async`, capability, approvals; `execution.rs:92-97`).
2. New `ExecutionToolBridge::execute_in_run` replaces reserve/spawn (:99-158) for the five tools. The plan factory
   (`isolation_execution_program.rs`) builds an `AuthorizedHelperPlan { run: record.run, helper kind, sealed input,
   timeout }`. The check flips: today a child must differ from the caller (`run.0 == context.record().run` refused,
   :206); now the target **must equal** the caller's capability run. Workspace: the record still needs
   `ResourceGrant::Workspace{employee-work}` (:91-93); the helper uses the run's own `/work`, so no
   `AuthorizedWorkspace` delivery is needed.
3. Re-authorize after preparation, then `verify_live` immediately before sending (same double check as :125-141).
4. `ExecutionBackend::run_helper(caller, plan) -> HelperOutcome + RunStdio` (new trait method,
   `duduclaw-isolation/src/execution.rs:217-242`; client in `duduclaw-execution/src/spawnd.rs:318`) sends
   `SpawnRequest::HelperCall`. spawnd checks `owned(run, connection)` (`admission.rs:249`), state `Running`, employee
   match, helper index, deadline; no reservation, no admission-queue entry, no second unit.
5. The gateway awaits `HelperWait`, reads bounded output concurrently, decodes, and returns the normal MCP result.
   **The call is synchronous**; nothing is written to the job store for the three offline tools.
6. Run ends mid-call: `Ended{kind: RunEnded}` → result `{"status":"run_ended"}` (`isError: true`); no retry, no
   persisted result; files the helper already wrote stay in `/work`.
7. Capability revoked or MCP connection closed mid-call: the gateway sends `HelperCancel`; the launcher kills the
   helper; the call returns refused/uncertain like today's `guard.cancel()` path.
8. Native CLI tool timeouts must be ≥ the longest helper timeout + grace (Codex `tool_timeout_sec` default is 60 s,
   Claude's MCP tool timeout likewise configurable; inferred, verify). This lives in the signed MCP projection, so
   it changes `McpAcceptance.launch_policy_digest` (`installed.rs:130-137`) and needs new acceptance (Q5).

## 4. Recording (`browser_record_start/stop`)

- **Start** (Egress parent only, else `helper_parent_offline`): `HelperCall{mode: Recording}`; returns after
  `Started` with `{"status":"recording","id":<call id hex>}`. The call id is a gateway-random 16-byte value
  (format of `recording_helper::valid_id`, :57) and becomes the job key. The gateway inserts a job row bound to
  (employee, actor, binding, principal generation) like `jobs.insert` today (`execution.rs:116`) and spawns a
  collector task that owns the stdout read end.
- **Stop, same run**: `browser_record_stop {id}` → authorize exactly as `stop_recording` does (:163-189), then
  `HelperStop` → launcher writes `stop\n` → wait `Ended` → collector decodes (`recording_helper::decode_result`,
  :59-81) → `jobs.finish` → return the result synchronously.
- **Run end without stop**: launcher flushes (§2.5); the collector stores the driver result plus
  `"ended_by":"run_end"`. Hard kill before flush → `{"status":"uncertain","reason":"run_killed"}`; artifacts are
  never fabricated (the `/helper-tmp` staging is lost; `recording_io::publish` writes only on success,
  `recording_io.rs:30-35`).
- **Stop in a later run**: the job row is terminal; `jobs.read` returns the stored result with no helper
  interaction. Admin RPCs `isolation.recording_status/stop` (`handlers/isolation_recording_rpc.rs:5-13`) keep
  working: live run → `HelperStop` through spawnd; ended → stored result.
- **Storage**: result JSON in the gateway's `isolation-execution-jobs.db` (opened at boot, `isolation_mcp/boot.rs:463`;
  gateway uid, 0600 inferred, verify); artifacts `trace.zip`, `actions.json`, `network.json` (each ≤ 16 MiB,
  `recording_helper.rs:7`) under `/work/recordings/<id>/` in employee storage (employee uid, 0700 dirs / 0600 files,
  inferred, verify). The gateway reads artifacts only through the D-H1 root work broker.
- The old text "D8 means the recording starts only after the caller run exits"
  (`docs/architecture/native-linux-isolation.md:7203-7212`) is replaced.
- Feasibility risk: the driver launches Chromium with `chromiumSandbox: true` (`recording_driver.cjs:34`); Chromium's
  layer-1 sandbox needs user/PID namespaces, which the common filter denies (inferred, verify that the existing
  child-run path ever launched it). HX6 must settle this before acceptance; `--no-sandbox` would leave only the
  helper sandbox around the renderer.

## 5. Contract amendment

**D8 (new wording).** "At most one capability-holding run per employee. Inside that run, besides the runtime, the
trampoline may start (a) the egress forwarder and (b) one helper launcher that runs bounded helper processes. A
helper is started only by spawnd on a gateway request bound to the same run; holds no capability, MCP channel,
credential or reservation; runs in the run's unit, cgroup, UID and storage lease; never outlives the run; and its
syscall, filesystem and network authority is a subset of the parent runtime profile, fixed by root-signed policy."
Also update: Package H D-H5 entry, `spawnd/README.md:5`, `mcp-boundary/src/execution.rs:84-86` comment,
`native-linux-isolation.md:7195-7212`.

**IPC** (`duduclaw-isolation/src/ipc.rs`, protocol stays v1 with new enum variants; gateway and spawnd ship in one
generation, `installed.rs:22-27` requires equal protocol versions):
- `SpawnRequest::HelperCall { run, call: HelperCallId, helper: RuntimeKind, timeout_ms, mode }` with fd
  `[PromptSealed]` → `SpawnResponse::HelperStarted { call, effective_timeout_ms }` + fds `[StdoutRead, StderrRead]`.
- `HelperWait { run, call }` → `HelperEnded { call, outcome: HelperOutcome }`; `HelperStop { run, call }`,
  `HelperCancel { run, call }` → `Ack`.
- `HelperOutcome = Exited(i32) | Signalled(u32) | TimedOut | OomKilled | RunEnded | StartFailed`.
- `ErrorCode` += `HelperBusy`, `HelperUnavailable`, `RunTimeExhausted` (`ipc.rs:234`).
- `TrustedRun` (`runner.rs:28-48`) += `launcher: Option<LauncherPlan { helpers: Vec<HelperProgram> }>` and
  `helper_tmpfs_bytes`; `validate` (:54-79) checks each `HelperProgram` like `validate_program` (:82-99).

**Capability / grants**: no new MAC-covered grant type. The five tools keep their tool grants and the
`Workspace{employee-work}` grant; `tool_policy` gains route `Route::InRunHelper` (replacing `Route::Execution` for
these tools, `execution.rs:337-345`) whose invariant is `target run == record.run`. `CapabilityHandover`
(`isolation/src/execution.rs:101-104`) is unchanged: helpers never receive `mcp_bridge`.

**Signed policy / manifests**: `isolation-policy-v1` artifact `SignedPolicy.version` 1 → 2 (`installed.rs:199,243`)
with `NativeRecord.launcher`; `deny_unknown_fields` means v1 readers refuse v2 and vice versa (one generation).
Runtime manifest kind (`--kind runtime`) is unchanged; helper runtimes are re-signed with the new fixed argv
(helper root). Unit policy: `OOMPolicy=continue`, `Delegate=yes` under Q2-A.

**Readiness reasons** (`duduclaw-gateway/src/isolation_readiness.rs`, banner F4): `helper_runtime_missing{runtime}`
(H16), `helper_launcher_unavailable`, `helper_parent_offline{runtime}`, `helper_not_narrowing{runtime}` (publication
check 2–3 failed), `helper_cgroup_unavailable` (Q2-A). Per-call typed results: `helper_busy`, `run_time_exhausted`,
`helper_timed_out`, `helper_oom_killed`, `run_ended`, `helper_unavailable`.

## 6. Threat model

| Attacker / threat | Mitigation | Residual |
|---|---|---|
| AI runtime forges or widens a helper request | No launcher fd in the runtime; only MCP → gateway gates → spawnd `owned(run, connection)` → index into signed table; argv/env/paths never on the wire. | None beyond what the tool grant allows. |
| AI runtime hijacks the launcher (same UID) | Launcher not exec'd, stays non-dumpable; `ptrace`, `process_vm_*`, `pidfd_getfd` denied (`seccomp.rs:98-109`); `hidepid=invisible` (`mounts.rs:225`). spawnd still parses launcher frames as untrusted. | Runtime can SIGKILL/SIGSTOP the launcher or its helpers: self-DoS only (`helper_unavailable`). Low. |
| AI runtime uses the helper as a confused deputy | F_L ∩ F_H ⊆ parent; Landlock subset; offline = no `socket`; egress = parent forwarder only; publication check 2–4. | Helper interpreters (python/node/chromium) become runnable via authorized tools only. Low. |
| AI runtime tampers with a running helper | Helper `PR_SET_DUMPABLE 0`; private `/helper-tmp`; strict result decoders. | Same-UID writes to `/work`, signals, and `/proc/<pid>/mem` of interpreter children when Yama is off can corrupt its own helper's result; no authority gained. Low. |
| Malicious helper input (code, document, URL) | Helper sandbox; no `/home/employee`, `/config`, MCP fd, control socket, status pipe; offline no network; recording writes only `/work/recordings`. | Recording: compromised renderer can exfiltrate its own recording data through the parent egress profile. Chromium sandbox status open (§4). Medium. |
| Malicious helper runtime artifact | Signed runtime manifest + release sha256; exec fd identity on registry mount (`runner.rs:170-177`); interpreter digests re-checked (`program_helper.rs:287-310`). | A signed-but-malicious artifact runs with helper authority (⊆ run). Low. |
| Resource exhaustion | `hx/` memory/pids/cpu (Q2-A), `oom_score_adj=1000`, `OOMPolicy=continue`, timeout ≤ remaining run, bounded pipes, 1+1 concurrency, queue ≤ 4, disk counted on the employee quota. | Without Q2-A a helper fork bomb can use the run's `TasksMax` until timeout and escaped descendants live until run end. Medium (Low with Q2-A). |
| Escape to the gateway UID | Only spawnd-created pipes reach the gateway; bounded strict decoders, stderr must be empty; artifacts read only via the D-H1 broker (`openat2` no symlinks/xdev). | Parser bugs in bounded JSON. Low. |
| Escape to root (spawnd) | Fixed 4 KiB frames, ≤ 4 fds, close on malformed; fds from the launcher are never accepted (direction is spawnd → launcher only). | Low. |
| Cross-employee | Per-run control socket in spawnd's per-run entry; run ↔ employee ↔ connection check; helper inside that run's unit, UID, storage lease; no shared helper pool. | None known. |
| D8 bypass | No reservation, no unit, no capability, no MCP fd for helpers; PID-namespace teardown at run end; recording ≤ run. | None. |
| Persistence after run end | Init exit kills the PID namespace; spawnd cleanup readback (unit inactive, cgroup empty, UID absent; `ipc.rs:293`) before releasing the storage lease. | None. |

## 7. Tasks

Waves: HX1 → HX2 ∥ HX3a → HX3b → HX4 → HX5 → HX6. [SEC] = Daybreak review (HX3+HX4 together, then HX6).

- **HX1** design + contract amendment (this doc; D8 text in the files listed in §5), Daybreak threat-model review,
  owner sign-off on Q1–Q5. Acceptance: review findings resolved or accepted by the owner.
- **HX2** [SEC] signed policy and plumbing: `installed.rs` (`LauncherRecord`, policy v2, checks 1–5),
  `policy.rs`, `native_trampoline.rs` (helper exec/mount fds, control socketpair in `Pending`), `bootstrap_mounts.rs`
  (helper mounts), `systemd.rs` (`OOMPolicy=continue`, `Delegate=yes`, readback), `scripts/sign-isolation-manifest.py`
  templates. Tests: `publication_rejects_helper_wider_than_parent`, `egress_helper_requires_same_egress_parent`,
  `helper_table_fd_budget_enforced`, `unit_policy_sets_oom_continue_and_reads_back`.
- **HX3** [SEC] trampoline: (a) `helper_wire.rs` pure codec + F_L/F_H builders in `seccomp.rs` with decoded-BPF
  tests like `seccomp.rs:255-344`; (b) `linux/launcher.rs`, `runner.rs` (launcher fork before :377, bit 4, run-end
  SIGTERM/flush in the reap loop, non-fatal launcher death, `oom_score_adj`), `mounts.rs` (`/helpers`,
  `/helper-tmp`, cgroup leaves), runtime Landlock unchanged except it never lists the new paths. Root tests
  (`#[ignore]`, `iso_root_helper_*`): helper cannot open `/home/employee` or `/config`; offline helper `socket` →
  EPERM; runtime cannot exec `/helpers/*`; helper killed at timeout and at run end; helper OOM leaves the run alive;
  escaped grandchild removed by `cgroup.kill`.
- **HX4** [SEC] spawnd + IPC + client: `ipc.rs` variants, `server.rs` dispatch, `host.rs`/`admission.rs` per-run
  helper slot and FIFO, deadline from admission, `audit.rs` events (run, employee, helper id, outcome, byte counts;
  no content), `duduclaw-execution/src/spawnd.rs` `run_helper`. Tests: `helper_call_never_occupies_d8_slot`,
  `helper_call_other_connection_refused`, `helper_call_on_ended_run_refused`, `helper_busy_after_queue_bound`,
  `deadline_refuses_below_five_seconds`.
- **HX5** gateway/MCP for execute_program/code_map/office_script: `mcp-boundary/src/execution.rs`
  (`execute_in_run`, `Route::InRunHelper`), `tool_policy.rs`, `isolation_execution_program.rs` (same-run plan, drop
  `collects_results` for the three), helper binaries' root path (`program_helper.rs`, `code_map_helper.rs`,
  `office_helper.rs`), native CLI tool-timeout projection, readiness reasons, `mcp_isolation_parity_tests.rs:97-105`
  and `scripts/ci/check_isolation_tool_matrix.py`, docs + CHANGELOG. Tests:
  `execute_program_returns_result_synchronously`, `helper_target_must_equal_capability_run`,
  `revocation_mid_call_kills_helper`, `run_end_mid_call_returns_run_ended`.
- **HX6** [SEC] recording: start/stop in-run, gateway collector + job-store persistence, launcher flush on SIGTERM,
  `/helper-tmp` staging, Admin RPC update, Chromium sandbox decision, interplay with H12d `skill_from_recording`.
  Tests: `recording_stop_same_run_returns_artifacts`, `recording_flushed_at_run_end_then_later_stop_reads_store`,
  `recording_hard_kill_stores_uncertain`, `recording_refused_in_offline_parent`, `second_recording_same_run_ok`.
- Acceptance additions to H15 (Ubuntu 22.04/24.04, Debian 12): synchronous execute_program within one Claude run;
  D8 second-run queueing still holds while a helper runs; recording stop after run end.

## 8. Open questions for the owner

- **Q1 Launcher form.** A: in-process function of the trampoline (no exec, stays non-dumpable, no new signed
  artifact). B: separate `duduclaw-helper-launcher` ELF like the forwarder (exec resets dumpability; relies on
  `PR_SET_DUMPABLE` at entry and Yama). **Recommend A**; the name "helper launcher" stays for the role.
- **Q2 Helper memory/pids limit.** A: `Delegate=yes` + root-created `rt/`/`hx/` leaf cgroups, helper moved in via a
  pre-opened `cgroup.procs` fd, `cgroup.kill` cleanup; refuse helpers (`helper_cgroup_unavailable`) where this
  does not work. B: no delegation; only `oom_score_adj=1000` + `OOMPolicy=continue` + timeout. **Recommend A**
  (B leaves fork bombs and escaped descendants bounded only by the run); needs a 22.04 kernel check first.
- **Q3 Recording egress scope.** A: recording reaches only what the parent run's egress profile allows; employees
  whose runtime profile is vendor-API-only get `helper_parent_offline`/no recording until a public-web parent
  profile is published for them. B: a helper-specific egress registration, which widens the run's network authority
  and breaks "only narrows". **Recommend A** (matches the owner's "egress helpers need an Egress parent"); decide
  which parent profiles get public-web egress.
- **Q4 Parallel tool calls from one run** (CLIs may issue several at once). A: FIFO depth 4, each waiting within its
  own deadline, then `helper_busy`. B: refuse any second call immediately. **Recommend A.**
- **Q5 CLI tool timeout.** A: raise the projected MCP tool timeout to ≥ 305 s (new signed MCP acceptance per runtime).
  B: cap helper timeouts at each CLI's default (Codex 60 s; inferred, verify). **Recommend A.**
