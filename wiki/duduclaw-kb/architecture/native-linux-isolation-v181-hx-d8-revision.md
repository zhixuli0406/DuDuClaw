# HX: in-run helper processes for execution-type tools (D8 revision, option A)

Status: design rev. 2 (2026-11-08, task HX1), base `feat/iso-v181-20261108`. Rev. 1's independent threat-model
review (Daybreak stand-in, GO with required changes F1–F17, `~/.baton/runs/20261108-183823-claude-18045-169.md`) is
folded into the body, tagged `[Fn]`; owner decisions Q1–Q5 are in §8. Line references are to this worktree;
anything not read in code is marked "(inferred, verify)".

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

**Design invariant.** Helper authority ⊆ calling runtime's authority:
- *Syscalls*: the signed helper filter F_H ⊆ the parent runtime profile, except a closed list of self-only calls
  with argument constraints (`prlimit64` with pid 0, `prctl` with allowlisted options; §2.4) [F3].
- *Network, identity, credentials, MCP channel, capability, lifetime*: nothing the run does not already have; a
  helper never outlives the run.
- *Filesystem*, with one stated exception [F15]: a helper may read/exec its own signed code at `/helpers/<id>` and
  read/write its private `/helper-tmp/<call>`. Every other helper path comes from a fixed `HelperFs` enum that is a
  subset of the runtime's Landlock list minus the forbidden paths (`/home/employee`, `/config`, `/run`, `/tmp`,
  `/dev/shm`, `/runtime`).
- *Not contained*: signals. Runtime and helpers share a UID, `kill` is in runtime profiles, and Landlock signal
  scoping needs ABI 6 (kernel 6.12; none of the H15 hosts). Each side can `SIGKILL`/`SIGSTOP` the other. This is an
  availability risk only and is handled by root-side enforcement (§2.5) and threat row T16 [F13].

**What keeps the runtime out of the launcher and helpers.** The primary guard is **Landlock domain scoping**: the
launcher is forked before init's `restrict_filesystem` (`runner.rs:377`), so it and its helpers live in a domain
that is a sibling of the runtime's, not nested in it. On every Landlock ABI the kernel refuses ptrace-class access
from a domain to a process outside it, which covers `/proc/<pid>/{mem,fd/*,environ,cwd,root}` and the
`hidepid=invisible` visibility check (`mounts.rs:225`), whatever the target's dumpability or the Yama setting.
Seccomp denial of `ptrace`, `process_vm_*`, `pidfd_getfd` (`seccomp.rs:101-121`) is the second layer;
non-dumpability is a third layer for the launcher only. Yama is **not** relied on: Ubuntu 22.04/24.04 set
`ptrace_scope=1` via sysctl.d, Debian 12 ships 0 (inferred, verify on the H15 host). `hidepid` alone does nothing
against the same UID.

## 2. Architecture

```
AI runtime ──fd3 MCP──▶ gateway (MCP boundary, capability, tool policy, transport-run == record.run)
                              │ SpawnRequest::HelperCall{run, employee_id, …} + sealed input memfd
                              ▼
                        spawnd (root) ──per-run control socketpair──▶ launcher (inside run, employee uid)
                          │   ▲  stdout/stderr pipe read ends                 │ fork + sandbox + execveat
                          │   └──────────── gateway reads, decodes ◀──────────┘ helper (fd1/fd2 = pipe write ends)
                          └── root, outside the unit: hx/fg, hx/rec limits, watchdog, cgroup.kill, memory.events
```

### 2.1 Who forks the launcher, and when
`child_init` today: drop identity/caps, NNP (`runner.rs:368-375`) → `restrict_filesystem` (:377) → common seccomp
(:379-380) → fork forwarder (:389-403) → fork runtime (:404-411) → wait for isolation receipts (:424) → init filter
(:443) → reap loop (:446-458).

HX inserts the launcher fork **after `PrivilegesVerified` (:375) and before `restrict_filesystem` (:377)**. A
regression test (`launcher_forked_before_runtime_landlock`) pins this: moving the fork after :377 would nest the
launcher in the runtime's domain and remove the primary guard. The launcher child, in order [F12]:
1. Fixes its fd table to an exact allowlist: 0/1/2 → `/dev/null` (it must not inherit the runtime's output fds),
   the control socket, the helper exec fds, the `hx/fg` and `hx/rec` `cgroup.procs` fds, the ready-pipe write end.
   Everything else is closed and the table is read back. It drops prompt, attachments, stdin, MCP, PTY slave and
   the egress socket exactly like the forwarder child (`runner.rs:394-396`).
2. Installs its Landlock: `/helpers` RX, `/helper-tmp` RW, plus the union of the bound helpers' `HelperFs` paths
   (`/usr` `/lib*` RX, `/etc` `/proc` RO, `/dev/null` `/dev/urandom`, `/work` RW). **Never** `/home/employee`,
   `/config`, `/run`, `/tmp`, `/dev/shm`. Helper Landlock stacks under this, so the union is an upper bound.
3. Installs common seccomp, then its self-filter F_L (§2.4).
4. Marks isolated bit 4 in the `FailurePage` (`runner.rs:272-310`), then **unmaps the page** (it is never exec'd,
   so the writable mapping would otherwise persist), closes the status fd and writes one byte to the ready pipe.
   Init waits on that pipe (bounded) before it evaluates `all_isolated` (:424), which closes the bit-4 race
   (`runner.rs:295-297`). `all_isolated` requires bit 4 when a launcher is planned. Then it serves the socket.

Init then continues as today; the runtime's `restrict_filesystem` list (:462-478) does **not** contain `/helpers`
or `/helper-tmp`, so the runtime cannot open, list or exec helper runtimes or helper scratch.

**Launcher form (Q1 = A).** The launcher is an in-process function of the trampoline binary
(`duduclaw-run/src/linux/launcher.rs::serve`), not an exec'd ELF, so it keeps the non-dumpable flag set at
`privileges::non_dumpable` (:373) and needs no extra signed artifact.

**OOM protection [F2].** Only the supervisor (outside the PID namespace) gets `oom_score_adj=-1000`, set while it
is still root. Init writes `0` to its own `oom_score_adj` before any further fork, so the forwarder, launcher and
runtime all start at 0 (readback in the receipt). If init is OOM-killed the PID namespace tears down and the run
fails closed. Helpers are bounded by their per-slot cgroup (§2.4), not by `oom_score_adj` (the launcher cannot
write `/proc/<pid>/oom_score_adj` once its Landlock makes `/proc` read-only).

### 2.2 Request channel
- **The runtime gets no launcher fd.** Requests reach the launcher only as authorized MCP calls relayed by the
  gateway and spawnd. A runtime-held socketpair was rejected: it would bypass capability, tool policy, approvals,
  revocation and audit, and make the launcher parse attacker bytes.
- spawnd creates one `SOCK_SEQPACKET|SOCK_CLOEXEC` socketpair per run (same as the bootstrap pair,
  `spawnd/src/native_trampoline.rs:131-133`), keeps one end in the run's entry, and passes the other in the
  bootstrap rights after the forwarder fds (`TrustedRun::expected_fd_count`, `runner.rs:50-53`, gains
  `1 + helper exec fds + helper mount fds + 4 cgroup fds` (§2.4); `MAX_BOOTSTRAP_FDS = 80`, `linux/bootstrap.rs:8`).
- Wire (new pure module `duduclaw-run/src/helper_wire.rs`, host-testable): frames ≤ 4 KiB, fixed binary layout,
  `DCH1` magic, close on any malformed frame.
  - spawnd → launcher: `Start{call: [u8;16], helper: u8 (index into the signed helper table in TrustedRun),
    slot: Fg|Rec, timeout_ms: u64}` + exactly 3 fds `[input memfd, stdout w, stderr w]` (recording adds no fd; its
    stdin pipe is launcher-created); `Stop{call}` (recording: write `stop\n` to helper stdin, the driver protocol at
    `recording_driver.cjs:11-18`); `Kill{call}`.
  - launcher → spawnd: `Started{call}`, `Ended{call, kind, code}`, `Draining{}`, `Error{code}`. **These frames are
    advisory** [F5]: spawnd uses them for latency, never as the source of truth for timeout, OOM or exit.
  - No names, paths, argv or env cross the wire; the helper is an index into root-signed data.
- **spawnd receive rules [F14].** Launcher → spawnd frames never carry fds. spawnd calls `recvmsg` with a minimal
  control buffer and `MSG_CMSG_CLOEXEC`; any cmsg (fds are closed at once), `MSG_CTRUNC`, `MSG_TRUNC`, unknown call
  id, a frame illegal for the call's state, or more than 64 frames/s closes the control channel; later calls get
  `helper_unavailable`. Before forwarding the gateway's input memfd, spawnd revalidates its seals
  (`F_SEAL_SEAL|SHRINK|GROW|WRITE`) and size, as `Expected::Prompt` does.
- **Call ids** are generated by spawnd per run (16 random bytes, deduplicated within the run) and returned in
  `HelperStarted` [F7].
- **Concurrency (Q4 = A).** One foreground slot (`hx/fg`) and one recording slot (`hx/rec`) per run. Further calls
  wait in a per-run spawnd FIFO of depth ≤ 4, each bounded by its own deadline, then `helper_busy`. Queued memfds
  and fds count against admission's `pending_input_bytes` and `input_descriptors` budgets
  (`admission.rs:203-207`). Queued calls are dropped (closing their fds) on run end, runtime exit (`Draining`),
  capability revocation and MCP cancel. If HX4 cannot deliver all of this, ship option B: `helper_busy` at once.

### 2.3 Validation against the signed helper-runtime manifest
- Helper runtimes stay what Package H already publishes: `RuntimeRecord`s with `RuntimeKind::Registered(id)`
  (`spawnd/src/installed.rs:96-110`) signed via `scripts/sign-isolation-manifest.py --kind runtime`
  (namespace `duduclaw-runtime-manifest-v1@dudustudio`, :20), bound by sha256 in the release manifest. Ids in code
  (these win over Package H's text): `duduclaw-program-v1` (`program_helper.rs:6`), `duduclaw-code-map-v1`
  (`code_map_helper.rs:3`), `duduclaw-office-<skill>-<script>-v1` (`isolation_execution_program.rs:51`),
  `duduclaw-browser-record-v1` (`recording_helper.rs:4`).
- New: the **parent** profile's signed `NativeRecord` (`installed.rs:187-192`) gains
  `launcher: Option<LauncherRecord { helpers: Vec<HelperBinding> }>`; `HelperBinding = { profile: ProfileId,
  seccomp: RuntimeProfile, seccomp_digest, memory_bytes, tasks, cpu_quota_percent, timeout_ms, filesystem:
  HelperFs }`. This list is the helper allowlist; it is per parent profile, signed, and never derived from IPC.
- Publication-time checks (in `installed.rs` bundle validation, :238-252, and `RootPolicy::from_verified_publication`,
  `policy.rs:41-69`), fail closed:
  1. the helper `RuntimeRecord` exists, is entitled to the employee, `!mcp && !pty_allowed`, and passes today's
     `valid_helper` rules (`isolation_execution_program.rs:57-84`);
  2. `F_H ⊆ parent` [F3]. The only syscalls allowed in F_H without being in the parent are the argument-constrained
     self-only set: `prlimit64` with pid == 0, `prctl` with options in {`PR_SET_NO_NEW_PRIVS`, `PR_SET_PDEATHSIG`,
     `PR_SET_DUMPABLE`(0), `PR_GET_*` read-only}. Runtime profiles are flat allowlists (`seccomp.rs:186`), so the
     subset check is computed on the decoded rule set;
  3. `memory_bytes`, `tasks`, `cpu` ≤ parent `ResourceLimits` minus a signed runtime reserve; `timeout_ms` ≤ 300 s
     and ≤ parent `runtime_ms − termination_grace_ms − RESULT_GRACE_MS` (`isolation_execution_program.rs:13`);
  4. helper `NetworkPolicy::Egress(p)` ⇒ parent `Egress(p)` with the same profile; `Offline` always allowed;
  5. at most 16 helper bindings per parent, helper mounts deduplicated by registry mount (fd budget);
  6. `HelperFs` is a fixed enum (`Work`, `WorkRecordings`, `SystemRo`); every variant maps to paths in the parent's
     Landlock list minus the forbidden paths of §1 [F15];
  7. a parent with a recording binding has `termination_grace_ms ≥ RECORDING_MIN_GRACE_MS` (proposed 12 s, sized in
     HX6 from the measured flush time) [F16].
- Per call, spawnd re-derives the helper with `RootPolicy::derive(employee, kind, false)` (`policy.rs:70-84`) from
  `entry.employee` (not from the request), maps it to the index in the run's `TrustedRun` helper table, and refuses
  if absent. The launcher re-verifies the exec fd identity against the registry mount (`verify_executable`,
  `runner.rs:170-177`) before every exec.

### 2.4 Helper sandbox
| Layer | Mechanism |
|---|---|
| Identity | Employee UID/GID inherited from init after `clear_capabilities` + securebits + NNP (`runner.rs:369-374`). No capability can be regained; the launcher never runs as root. |
| Seccomp | Launcher self-filter F_L = parent runtime profile ∪ L_restrict, where L_restrict = {`landlock_*`, `seccomp`(set filter), `prctl` (allowlisted options as in check 2), `prlimit64` (pid == 0), `setsid`, `setpgid(0,0)`, `wait4`/`waitid`, `timerfd_*`, `close_range`} — calls that only restrict the caller or supervise own children [F3]. The child installs F_H (signed helper profile, built with `seccomp::runtime`, `seccomp.rs:198-211`) immediately before `execveat`. Kernel stacking yields F_L ∩ F_H ⊆ parent (check 2); common refusals (namespaces, mount, ptrace, bpf, io_uring, `pidfd_getfd`; `seccomp.rs:98-133`) are embedded in both. |
| Namespaces | Shares the run's mount/PID/net/IPC namespaces. Per-helper namespaces are impossible after the common filter (`clone` namespace flags and `unshare`/`setns` denied, `seccomp.rs:14-15,114-118`), and a root launcher was rejected. Separation from the runtime is by Landlock domain, seccomp, process group and cgroup. |
| Filesystem | Child Landlock (stacked on the launcher's) from `HelperFs`: `/usr` `/lib*` RX, `/helpers/<id>` RX, `/etc` RO (CA store for recording), `/proc` RO, `/dev/null` `/dev/urandom`, `/helper-tmp/<call>` RW, and `/work` RW for program/code_map/office (`/work/recordings` for the recording parent process only, §4). **No** `/home/employee`, `/config`, `/run`, `/runtime`, `/tmp`, `/dev/shm` [F9]. `/work` is the calling run's storage-lease mount (`spawnd/src/storage/adapter.rs:152-176`); no new mount per call. |
| Helper runtimes | Each helper registry mount is attached read-only, `nosuid,nodev` at `/helpers/<id>` by `build_root` (`mounts.rs:139-192`, like `runtime` at :158). Helper binaries change their hard-coded `/runtime/...` paths (`program_helper.rs:31-37`, `office_helper.rs:76-91`, `recording_helper.rs:93,125`, code-map likewise) to the signed helper root carried in fixed argv. |
| Scratch [F9] | New tmpfs `/helper-tmp` (size from the parent's tmpfs budget), absent from the runtime's Landlock list. The launcher creates `/helper-tmp/<call>` 0700 and sets **every** helper scratch path, `TMPDIR` and `HOME` inside it: replaces `/tmp/recording` and `HOME=/tmp/recording` (`recording_helper.rs:101,127`), `/tmp/office` (`office_helper.rs:87`), and `TMPDIR=/tmp` (`program_helper.rs:222`, `recording_helper.rs:127`). After the call the launcher removes the directory with `openat(O_NOFOLLOW|O_DIRECTORY)` + `unlinkat` only. The runtime can no longer pre-create or swap `driver.cjs` or Office staging files. |
| cgroup [F6] (Q2 = A, mandatory) | Unit gets `Delegate=yes`; `ProtectControlGroups=yes` stays (`systemd.rs:68-69`), so nothing inside the run touches cgroupfs by path. **spawnd, as root outside the unit's mount namespace**, creates leaves under the unit cgroup after start: `rt-init/` (supervisor, init, forwarder, launcher; spawnd moves the supervisor there before enabling controllers), `rt-runtime/` (runtime and all its descendants), `hx/fg` and `hx/rec` (`memory.oom.group=1`). It passes root-opened fds in the bootstrap: `rt-runtime/cgroup.procs` (the runtime child writes `0` before exec), `rt-runtime/cgroup.kill` (init, F4), `hx/fg/cgroup.procs` and `hx/rec/cgroup.procs` (the helper child writes `0` before its Landlock/F_H/exec). Moves through root-opened fds rely on open-time credentials (kernel ≥ 5.16 or the CVE-2021-4197 backport). Per call, spawnd writes `memory.max`, `pids.max`, `cpu.max` from the binding into the slot before sending `Start`, refuses `Start` while the slot is still populated, and after **every** call, whatever the outcome, writes the slot's `cgroup.kill` and waits for recursive `cgroup.events populated 0` before returning `HelperEnded`. If any step fails, helpers are refused with `helper_cgroup_unavailable`. (Ordering of unit start vs bootstrap send, and that the supervisor is the only process at that point: inferred, verify in `native_trampoline.rs`.) |
| Rlimits | Set by the helper child with `prlimit64(0, …)`, hard = soft: `RLIMIT_CORE=0`, `RLIMIT_NOFILE` and `RLIMIT_FSIZE` from the binding [F3]. No `RLIMIT_AS` (breaks V8/Chromium); no `RLIMIT_NPROC` (counts the whole employee UID; `pids.max` covers it). |
| Process [F1] | `setsid`, `PR_SET_PDEATHSIG=SIGKILL`, then an exact `FdPlan` like the forwarder's (`runner.rs:519-533`): 0 = `/dev/null` (or the recording stdin pipe), 1/2 = the call's spawnd pipes, **3 = `/dev/null`**, 4 = sealed input memfd (checked by `helper_input_args`, `program_helper.rs:317-341`); everything else closed, table read back before exec. Helper Rust entries call `PR_SET_DUMPABLE 0` first. No MCP fd, control socket, status pipe, cgroup fd or egress socket reaches a helper. |
| Network | Offline helper: F_H omits `socket`, so `common_rules` turns every family into EPERM (`seccomp.rs:124-131`); the run netns has only loopback (`mounts.rs:517`). Egress helper (recording): `socket` allowed; reachability is the parent's forwarder on `127.0.0.1:<port>` (`policy.rs:93-107`), already enforced by `recording_helper.rs:96-100`; Landlock TCP pinning where ABI ≥ 4 (§4); the egress host keeps public-address-only policy. |

### 2.5 Results, timeout, run end, OOM
- **Result**: spawnd creates stdout/stderr pipes per call, passes write ends to the launcher, returns read ends to
  the gateway (`FdRole::StdoutRead/StderrRead`, `ipc.rs:184`). The gateway reads ≤ the helper record's
  `output_total_bytes` and decodes with today's strict decoders (`decode_result`,
  `isolation_execution_program.rs:157-164`: stderr must be empty, outcome `Exited(0)`). Files go to `/work`.
- **Timeout** = min(tool timeout, binding `timeout_ms`, run deadline − now − termination grace − 5 s). spawnd
  computes the run deadline from admission (unit start + `RuntimeMaxUSec`, `systemd.rs:90`) and puts the effective
  value in `Start`. If it is < 5 s the call is refused with `run_time_exhausted`; otherwise the response reports
  `effective_timeout_ms`. Enforcement is two-layer [F5]: the launcher's timerfd + `kill(-pgid, SIGKILL)` is the fast
  path; **spawnd's own watchdog** at `effective_timeout + 2 s` writes the slot's `cgroup.kill` as root from outside
  the run, so a runtime that `SIGSTOP`s the launcher cannot extend a helper.
- **OOM**: unit property `OOMPolicy=continue` (add to `systemd.rs:74-93` and to the start readback, :298-320).
  spawnd derives `OomKilled` from the slot's `memory.events` `oom_kill` delta, read as root [F5]; the launcher's
  `Ended.kind` is ignored for this. With `memory.oom.group=1` the whole helper tree dies together and the run
  continues.
- **Runtime exits [F4]**: (a) init reaps the runtime, then immediately writes the root-opened
  `rt-runtime/cgroup.kill` fd, so daemonized runtime descendants that still hold MCP fd 3 die before anything else.
  (b) Init sends `SIGTERM` to the launcher; from then on the launcher refuses `Start`, sends `Draining{}`, kills
  foreground helpers, and flushes a recording within F = `termination_grace_ms − 2 s`. spawnd independently watches
  `rt-runtime/cgroup.events`; `populated 0` moves the run to `Draining`, in which spawnd accepts only
  `HelperWait`/`HelperStop`/`HelperCancel` and drops the queue. `HelperCall` is accepted only in `Running`.
  (c) Init bounds its wait with `alarm`/`setitimer` (added to the init filter, `seccomp.rs:171-180`) at F + 2 s,
  then `SIGKILL`s the launcher's process group and exits with the runtime's stored status; a `SIGSTOP`ped launcher
  therefore cannot hold the run until `RuntimeMaxUSec`.
- **spawnd cancel / `RuntimeMaxUSec`**: systemd SIGTERMs the cgroup (`KillMode=control-group`, `systemd.rs:75`),
  the launcher drains as in (b), hard SIGKILL after `TimeoutStopUSec` (:92). Init exit kills every helper anyway.
- **Launcher death** is not fatal to the run (unlike the forwarder, `runner.rs:450-453`): spawnd sees control-socket
  EOF, kills both slots, and fails later calls with `helper_unavailable`.

## 3. MCP tool call path (protected mode)

1. Runtime sends `tools/call execute_program` on fd 3. The boundary runs the shared gates unchanged
   (`tool_policy::authorize_async`, capability, approvals; `execution.rs:92-97`).
2. New `ExecutionToolBridge::execute_in_run` replaces reserve/spawn (:99-158) for the five tools. **Transport
   binding [F7]**: the gateway requires that the run whose `McpGateway` fd the call arrived on equals `record.run`;
   delegated, Admin-RPC or other-channel contexts never issue a `HelperCall`. The plan factory
   (`isolation_execution_program.rs`) builds an `AuthorizedHelperPlan { run: record.run, employee_id, helper kind,
   sealed input, timeout }`. The check flips: today a child must differ from the caller (`run.0 ==
   context.record().run` refused, :206); now the target **must equal** the caller's capability run. Workspace: the
   record still needs `ResourceGrant::Workspace{employee-work}` (:91-93); the helper uses the run's own `/work`.
3. Re-authorize after preparation, then `verify_live` immediately before sending (same double check as :125-141).
4. `ExecutionBackend::run_helper(caller, plan) -> HelperOutcome + RunStdio` (new trait method,
   `duduclaw-isolation/src/execution.rs:217-242`; client in `duduclaw-execution/src/spawnd.rs:318`) sends
   `SpawnRequest::HelperCall`. spawnd checks `owned(run, connection)` (`admission.rs:162`; this only proves the
   gateway connection owns the run, which all employees share), **plus `entry.employee == employee_id`** [F7],
   state `Running`, helper index derived from `entry.employee`, deadline; no reservation, no admission entry.
5. The gateway awaits `HelperWait`, reads bounded output concurrently, decodes, and returns the normal MCP result.
   **The call is synchronous**; nothing is written to the job store for the three offline tools.
6. Run ends mid-call: `HelperEnded{RunEnded}` → result `{"status":"run_ended"}` (`isError: true`); no retry, no
   persisted result; files the helper already wrote stay in `/work`.
7. Capability revoked, principal generation changed, MCP `notifications/cancelled` for the request, or MCP
   connection closed mid-call: the gateway sends `HelperCancel{run, call}` (recording: `HelperStop`, §4) [F16, Q5];
   spawnd kills the slot cgroup; the call returns refused/uncertain like today's `guard.cancel()` path.
8. **Job keying [F8]**: in-run jobs are keyed `(run, call)` in a namespace separate from run handles. Their only
   stop/cancel primitives are `HelperStop`/`HelperCancel`. The legacy run-killing fallbacks `kill_authorized(run)`
   (`execution.rs:218`) and `backend.kill(run)` (:317) are unreachable for in-run jobs (typed: the in-run job type
   carries no `RunHandle` those functions accept), because `run` would be the caller's own run.
9. CLI tool timeout (Q5 = A): the signed MCP projection sets each runtime's MCP tool timeout ≥ 305 s (Codex default
   60 s; inferred, verify). It changes `McpAcceptance.launch_policy_digest` (`installed.rs:130-137`): re-run per runtime.

## 4. Recording (`browser_record_start/stop`)

- **Start** (Egress parent only, else `helper_parent_offline`): `HelperCall{slot: Rec}`; returns after `Started`
  with `{"status":"recording","id":<call id hex>}`. The spawnd call id (format of `recording_helper::valid_id`, :57)
  becomes the job key `(run, call)`. The gateway inserts a job row bound to (employee, actor, binding, principal
  generation) like `jobs.insert` today (`execution.rs:116`) and spawns a collector task owning the stdout read end.
- **Stop, same run**: `browser_record_stop {id}` → authorize as `stop_recording` does (:163-189) **with the second
  inverted check fixed** [F8]: today `run.0 == context.record().run → refused` (:180); an in-run job in the caller's
  own run is now the allowed case, resolved through the `(run, call)` table, never `parse_handle`. Then
  `HelperStop` → launcher writes `stop\n` → `HelperEnded` → collector decodes (`recording_helper::decode_result`,
  :59-81) → `jobs.finish` → result returned synchronously.
- **Run end without stop**: launcher flushes (§2.5); the collector stores the driver result plus
  `"ended_by":"run_end"`. Hard kill before flush → `{"status":"uncertain","reason":"run_killed"}`; artifacts are
  never fabricated (`/helper-tmp` staging is lost; `recording_io::publish` writes only on success,
  `recording_io.rs:30-35`).
- **Revocation [F16]**: capability revocation or a principal-generation change sends `HelperStop` for a live
  recording; the result is stored with `"ended_by":"revoked"`.
- **Stop in a later run**: the job row is terminal; `jobs.read` returns the stored result with no helper
  interaction. Admin RPCs `isolation.recording_status/stop` (`handlers/isolation_recording_rpc.rs:5-13`) keep
  working: live run → `HelperStop` for that `(run, call)` through spawnd; ended → stored result. Admin never issues
  `HelperCall`.
- **Browser containment [F10]**. `chromiumSandbox: true` (`recording_driver.cjs:34`) cannot start under the common
  filter (no user namespaces; setuid sandbox blocked by NNP/nosuid), so in practice the renderer runs with
  `--no-sandbox` and the helper sandbox is the containment. Required:
  - the Rust recording parent forks node/Chromium under a **nested Landlock** limited to `/helper-tmp/<call>` RW plus
    the read-only system paths: no `/work`, no `/helper-tmp` siblings;
  - Chromium flags `--disable-dev-shm-usage`; Playwright pipe transport (no DevTools TCP port); `--jitless`
    evaluated in HX6 (cost vs exposure);
  - where Landlock ABI ≥ 4 (Ubuntu 24.04 6.8; inferred, verify), the browser tree may TCP-connect only to the
    forwarder port and may not bind any TCP port. On older kernels (22.04 GA 5.15, Debian 12 6.1) the residual is
    loopback and abstract-UNIX reach to the runtime's local listeners; documented in threat row T5;
  - the Rust parent publishes into `/work/recordings/<id>/` (`openat2` `RESOLVE_BENEATH|NO_SYMLINKS`) only after the
    browser tree is dead (slot cgroup has no browser processes);
  - **HX6 is gated on Chromium actually launching** under F_L ∩ F_H and the nested Landlock on all three H15 hosts.
    If it cannot, recording ships disabled with readiness reason `helper_recording_unsupported`; no sandbox is
    weakened to make it start.
- **Storage**: result JSON in the gateway's `isolation-execution-jobs.db` (opened at boot,
  `isolation_mcp/boot.rs:463`; gateway uid, 0600 inferred, verify); artifacts `trace.zip`, `actions.json`,
  `network.json` (each ≤ 16 MiB, `recording_helper.rs:7`) under `/work/recordings/<id>/` in employee storage.
  **Integrity [F11]**: the runtime has RW on `/work` and can rewrite these files at any time, so the job row stores
  each artifact's sha256 from the decoded result; the D-H1 root work broker and `skill_from_recording` (H12d) verify
  it on every read and refuse with `artifact_modified` on mismatch.
- **Egress scope (Q3 = A)**: a recording reaches only what the parent run's egress profile allows. Enabling
  recording for an employee means granting that employee's runtime public-web egress; the admin UI states this.

## 5. Contract amendment

**D8 (new wording).** "At most one capability-holding run per employee. Inside that run, besides the runtime, the
trampoline may start (a) the egress forwarder and (b) one helper launcher that runs bounded helper processes. A
helper is started only by spawnd on a gateway request that arrived on the same run's MCP transport and names the
run's employee; holds no capability, MCP channel, credential or reservation; runs in the run's unit, a per-slot
cgroup, the run's UID and storage lease; never outlives the run; and its syscall and network authority is a subset
of the parent runtime profile and its filesystem authority a subset except its own signed code and private
scratch, fixed by root-signed policy." Also update: Package H D-H5 entry, `spawnd/README.md:5`,
`mcp-boundary/src/execution.rs:84-86` comment, `native-linux-isolation.md:7195-7212` (incl. "D8 means the
recording starts only after the caller run exits", :7203-7212).

**IPC** (`duduclaw-isolation/src/ipc.rs`, protocol stays v1 with new enum variants; gateway and spawnd ship in one
generation, `installed.rs:22-27` requires equal protocol versions):
- **Feature bit [F17]**: spawnd advertises `helper_calls` in its readiness/hello response. A gateway that does not
  see it never sends `HelperCall` and reports `helper_launcher_unavailable{reason: spawnd_feature_missing}`, so an
  old/new mix fails with a named reason instead of an opaque `InvalidRequest`.
- `SpawnRequest::HelperCall { run, employee_id, helper: RuntimeKind, timeout_ms, slot }` with fd `[PromptSealed]`
  → `SpawnResponse::HelperStarted { call, effective_timeout_ms }` + fds `[StdoutRead, StderrRead]`.
- `HelperWait { run, call }` → `HelperEnded { call, outcome: HelperOutcome }`; `HelperStop { run, call }`,
  `HelperCancel { run, call }` → `Ack`.
- `HelperOutcome = Exited(i32) | Signalled(u32) | TimedOut | OomKilled | RunEnded | StartFailed`, all determined by
  spawnd from root-side state (cgroup events, watchdog, `memory.events`) with launcher frames as hints.
- `ErrorCode` += `HelperBusy`, `HelperUnavailable`, `RunTimeExhausted`, `HelperCgroupUnavailable` (`ipc.rs:234`).
- `TrustedRun` (`runner.rs:28-48`) += `launcher: Option<LauncherPlan { helpers: Vec<HelperProgram> }>`,
  `helper_tmpfs_bytes` and the cgroup fd roles; `validate` (:54-79) checks each `HelperProgram` like
  `validate_program` (:82-99).

**Capability / grants**: no new MAC-covered grant type. The five tools keep their tool grants and the
`Workspace{employee-work}` grant; `tool_policy` gains route `Route::InRunHelper` (replacing `Route::Execution` for
these tools, `execution.rs:337-345`) whose invariant is `transport run == target run == record.run`.
`CapabilityHandover` (`isolation/src/execution.rs:101-104`) is unchanged: helpers never receive `mcp_bridge`.

**Signed policy / manifests**: `isolation-policy-v1` artifact `SignedPolicy.version` 1 → 2 (`installed.rs:199,243`)
with `NativeRecord.launcher`. `version` and the signing namespace stay inside the signed bytes, so a v2 document
can never be reinterpreted as v1 [F17]. A v1 reader refuses v2 (`deny_unknown_fields`); a v2 reader accepting a v1
document treats it as "no launcher" (helper tools report `helper_launcher_unavailable`). Runtime manifest kind
(`--kind runtime`) is unchanged; helper runtimes are re-signed with the new fixed argv (helper root, scratch under
`/helper-tmp`). Unit policy: `OOMPolicy=continue`, `Delegate=yes`, `ProtectControlGroups=yes` kept, all read back.

**Readiness reasons** (`duduclaw-gateway/src/isolation_readiness.rs`, banner F4): `helper_runtime_missing{runtime}`
(H16), `helper_launcher_unavailable{reason}`, `helper_parent_offline{runtime}`, `helper_not_narrowing{runtime}`
(publication checks 2, 3, 6), `helper_cgroup_unavailable` (no delegation, or host lacks the CVE-2021-4197 fix),
`helper_recording_unsupported` (§4). Per-call typed results: `helper_busy`, `run_time_exhausted`,
`helper_timed_out`, `helper_oom_killed`, `run_ended`, `helper_unavailable`, `artifact_modified`.

## 6. Threat model

| # | Attacker / threat | Mitigation | Residual |
|---|---|---|---|
| T1 | AI runtime forges or widens a helper request | No launcher fd in the runtime; only MCP → gateway gates → transport-run check → spawnd connection + employee check → index into signed table; argv/env/paths never on the wire. | None beyond the tool grant. |
| T2 | AI runtime inspects or hijacks the launcher (same UID) | **Landlock domain scoping** (launcher forked before :377, sibling domain): `/proc/<pid>/*` and ptrace-class access refused on every ABI. Second layer: `ptrace`, `process_vm_*`, `pidfd_getfd` denied (`seccomp.rs:101-121`). Third: launcher non-dumpable. Yama not relied on. spawnd parses launcher frames as untrusted (T10). | Signals: see T16. |
| T3 | AI runtime uses the helper as a confused deputy | F_L ∩ F_H ⊆ parent (arg-constrained self-only exceptions); Landlock subset except own code/scratch; offline = no `socket`; egress = parent forwarder only; checks 2–4, 6. | Interpreters become runnable via authorized tools only. Low. |
| T4 | AI runtime tampers with a running helper | Landlock domain scoping blocks `/proc/<pid>/mem` of every helper incl. dumpable interpreters; private `/helper-tmp/<call>`; strict result decoders. | Signals (T16) and same-UID writes to shared `/work` can corrupt its own helper's result; no authority gained. Low. |
| T5 | Malicious helper input (code, document, URL) | Helper sandbox; no `/home/employee`, `/config`, `/tmp`, `/dev/shm`, MCP fd, control socket; offline no network; Chromium in nested Landlock without `/work`, pipe transport, TCP pin on ABI ≥ 4. | Recording exfiltrates its own data through the parent egress (Q3). Pre-ABI-4 kernels: loopback/abstract-UNIX reach to runtime listeners. Medium. |
| T6 | Malicious helper runtime artifact | Signed runtime manifest + release sha256; exec fd identity on registry mount (`runner.rs:170-177`); interpreter digests re-checked (`program_helper.rs:287-310`). | Runs with helper authority (⊆ run). Low. |
| T7 | Resource exhaustion | Mandatory per-slot `hx/fg`, `hx/rec` limits written by root; cgroup.kill after every call; `memory.oom.group`; `OOMPolicy=continue`; timeout ≤ remaining run; spawnd watchdog; bounded pipes; 1+1 slots; queue ≤ 4 counted against admission budgets; disk on the employee quota. | Low. Hosts without open-time credentials refuse helpers. |
| T8 | Escape to the gateway UID | Only spawnd-created pipes reach the gateway; bounded strict decoders, stderr must be empty; artifacts only via the D-H1 broker (`openat2` no symlinks/xdev) with sha256 check. | Parser bugs in bounded JSON. Low. |
| T9 | Escape to root (spawnd) | Fixed 4 KiB frames; launcher → spawnd frames carry no fds; `MSG_CMSG_CLOEXEC`, close on any cmsg/truncation/unknown call/illegal state/rate; memfd seals and size revalidated. | Low. |
| T10 | Cross-employee / cross-run | Per-run control socket; `HelperCall.employee_id` must equal `entry.employee`, entitlement derived from `entry.employee`; transport run must equal `record.run`; per-run call ids; helper inside that run's unit, UID, storage lease. | None known. |
| T11 | D8 bypass | No reservation, unit, capability or MCP fd for helpers; PID-namespace teardown; recording ≤ run. | None. |
| T12 | Persistence after run end | Init exit kills the PID namespace; spawnd cleanup readback (unit inactive, cgroup empty, UID absent; `ipc.rs:293`) before releasing the storage lease. | None. |
| T13 | Helper inherits the control socket or other launcher fds [F1] | Exact helper `FdPlan`, fd 3 = `/dev/null`, readback; launcher fd allowlist [F12]. | None. |
| T14 | OOM-unkillable runtime via inherited `oom_score_adj` [F2] | Only the supervisor is protected; init resets to 0 before forking; readback. | None. |
| T15 | Runtime descendants act after the runtime exits [F4] | Init kills `rt-runtime` at once; spawnd `Draining` from `rt-runtime` `populated 0`; launcher refuses `Start` after SIGTERM; bounded init wait. | None. |
| T16 | Signals between runtime and helpers/launcher/init/forwarder [F13, F5] | Timeout, OOM and exit decided root-side by spawnd; launcher death non-fatal. Landlock signal and abstract-socket scoping used where ABI ≥ 6. | A compromised helper can `SIGTERM` init, `SIGKILL` the forwarder (run exits 125) or `SIGSTOP` the runtime: run availability only. **Needs owner acknowledgement at HX1 sign-off.** Low-Med. |
| T17 | Stopped launcher defeats the timeout [F5] | spawnd watchdog + root `cgroup.kill`; OOM from `memory.events`. | None. |
| T18 | Cancel of an in-run job kills the whole run [F8] | Jobs keyed `(run, call)`; only `HelperStop`/`HelperCancel`; legacy `kill_authorized(run)`/`backend.kill(run)` unreachable by type. | None. |
| T19 | Runtime pre-creates or swaps helper scratch in `/tmp` [F9] | All scratch, `TMPDIR`, `HOME` under `/helper-tmp/<call>`, which the runtime cannot reach; no-follow cleanup. | None. |
| T20 | Runtime rewrites published recording artifacts [F11] | sha256 in the job row, checked on every broker / `skill_from_recording` read. | Runtime can destroy (not forge) its own artifacts. Low. |

## 7. Tasks

Waves: HX1 → HX2 ∥ HX3a → HX3b → HX4 → HX5 → HX6. [SEC] = Daybreak review (HX3+HX4 together, then HX6). Root
tests are `#[ignore]` `iso_root_helper_*`, run on the H15 hosts.

- **HX1** design + contract amendment (this doc; D8 text in the files listed in §5), threat-model review folded in
  (F1–F17), owner decisions Q1–Q5 (§8). Open: owner acknowledgement of T16 residual.
- **HX2** [SEC] signed policy and plumbing: `installed.rs` (`LauncherRecord`, `HelperFs` enum, policy v2 with
  signed version, v1 → no launcher, checks 1–7), `policy.rs` (derive from `entry.employee`),
  `native_trampoline.rs` (helper exec/mount fds, control socketpair; cgroup leaves `rt-init`/`rt-runtime`/`hx/fg`/
  `hx/rec`, supervisor move, root-opened fds in bootstrap), host probe for the CVE-2021-4197 open-time-credentials
  fix, `bootstrap_mounts.rs` (helper mounts), `systemd.rs` (`OOMPolicy=continue`, `Delegate=yes`,
  `ProtectControlGroups=yes`, readback), signer templates. Tests: `publication_rejects_helper_syscall_outside_parent`,
  `publication_allows_prlimit64_pid0_only`, `publication_rejects_helperfs_outside_runtime_list`,
  `publication_rejects_short_grace_for_recording`, `egress_helper_requires_same_egress_parent`,
  `helper_table_fd_budget_enforced`, `v2_reader_v1_policy_means_no_launcher`, `v2_bytes_not_valid_as_v1`,
  `unit_policy_sets_oom_continue_delegate_protect_cgroups_and_reads_back`, `cgroup_leaves_created_outside_unit`,
  root `helpers_refused_without_open_time_credentials` (must pass on Ubuntu 22.04 5.15, else that host refuses).
- **HX3a** `helper_wire.rs` pure codec (exact fd counts, launcher frames fd-free, state table) and F_L/F_H builders
  in `seccomp.rs` (argument-constrained `prlimit64`/`prctl`) plus `alarm`/`setitimer` in the init filter, with
  decoded-BPF tests like `seccomp.rs:255-344`: `f_l_prlimit64_other_pid_denied`, `f_l_prctl_option_allowlist`,
  `init_filter_allows_alarm`, `wire_rejects_fd_on_launcher_frame`.
- **HX3b** [SEC] trampoline: `linux/launcher.rs`, `runner.rs` (fork before :377; launcher fd allowlist, Landlock,
  `FailurePage` unmap, ready pipe before `all_isolated`; helper exact `FdPlan`; hard = soft rlimits; supervisor-only
  OOM protection with init reset; runtime moves into `rt-runtime`, helpers into their slot; runtime-exit kill of
  `rt-runtime`; launcher refuses `Start` after SIGTERM; bounded init wait; scratch/`TMPDIR`/`HOME` under
  `/helper-tmp/<call>` with no-follow cleanup), `mounts.rs` (`/helpers`, `/helper-tmp`). Root tests:
  `launcher_forked_before_runtime_landlock`, `runtime_cannot_access_proc_of_helper` (Debian 12 with
  `ptrace_scope=0`), `launcher_fd_table_exact`, `launcher_landlock_excludes_home_config_run_tmp_shm`,
  `launcher_ready_before_all_isolated`, `helper_fd_table_exact`, `only_supervisor_oom_protected`,
  `helper_cannot_open_home_config_tmp_devshm`, `offline_helper_socket_eperm`, `runtime_cannot_exec_helpers`,
  `runtime_cannot_precreate_helper_scratch`, `runtime_descendants_killed_on_runtime_exit`,
  `launcher_refuses_start_after_sigterm`, `init_wait_bounded_with_stopped_launcher`, `helper_raise_rlimit_denied`.
- **HX4** [SEC] spawnd + IPC + client: `ipc.rs` variants + `helper_calls` feature bit, `server.rs` dispatch,
  `host.rs`/`admission.rs` (employee check, per-run call ids, slots, FIFO ≤ 4 with budget accounting and drops,
  `Running`/`Draining` from `rt-runtime` events, per-call limit writes, `cgroup.kill` + `populated 0` after every
  call, watchdog, OOM from `memory.events`, recvmsg rules, memfd revalidation), `audit.rs` events (run, employee,
  helper id, outcome, byte counts; no content), `duduclaw-execution/src/spawnd.rs` `run_helper`. Tests:
  `helper_call_never_occupies_d8_slot`, `helper_call_other_connection_refused`, `helper_call_wrong_employee_refused`,
  `helper_call_on_draining_run_refused`, `queued_call_counts_against_input_budgets`,
  `queue_dropped_on_run_end_revocation_cancel`, `helper_busy_after_queue_bound`, `deadline_refuses_below_five_seconds`,
  `control_channel_closed_on_cmsg_or_truncation`, `memfd_seals_revalidated`; root:
  `watchdog_kills_slot_with_stopped_launcher`, `oom_reported_from_memory_events_not_frame`,
  `fg_kill_leaves_rec_running`, `escaped_daemon_killed_after_call`.
- **HX5** gateway/MCP for execute_program/code_map/office_script: `mcp-boundary/src/execution.rs`
  (`execute_in_run`, transport-run check, `employee_id`, `(run, call)` job namespace, legacy fallbacks unreachable),
  `tool_policy.rs` (`Route::InRunHelper`), `isolation_execution_program.rs` (same-run plan, drop `collects_results`
  for the three), helper binaries' root path and `/helper-tmp` scratch (`program_helper.rs`, `code_map_helper.rs`,
  `office_helper.rs`), CLI tool-timeout ≥ 305 s projection + MCP acceptance re-run per runtime,
  `notifications/cancelled` and connection close → `HelperCancel`, feature-bit and readiness reasons,
  `mcp_isolation_parity_tests.rs:97-105`, `scripts/ci/check_isolation_tool_matrix.py`, docs + CHANGELOG. Tests:
  `execute_program_returns_result_synchronously`, `helper_target_must_equal_capability_run`,
  `transport_run_must_equal_record_run`, `admin_context_cannot_issue_helper_call`,
  `in_run_job_cancel_never_kills_run`, `revocation_mid_call_kills_helper`, `mcp_cancelled_maps_to_helper_cancel`,
  `run_end_mid_call_returns_run_ended`, `gateway_refuses_helper_without_spawnd_feature`,
  `cli_tool_timeout_projection_at_least_305s`.
- **HX6** [SEC] recording: in-run start/stop, `:180` inversion via `(run, call)`, gateway collector + job store with
  artifact sha256, launcher flush on SIGTERM, revocation → `HelperStop`, nested Landlock for node/Chromium,
  `--disable-dev-shm-usage`, pipe transport, TCP pinning on ABI ≥ 4, `/helper-tmp` scratch in
  `recording_helper.rs`, Admin RPC update, admin UI egress statement, H12d interplay. **Gate**: Chromium launches on
  all three H15 hosts, else `helper_recording_unsupported`. Tests: `stop_same_run_allowed`,
  `recording_stop_same_run_returns_artifacts`, `recording_flushed_at_run_end_then_later_stop_reads_store`,
  `recording_hard_kill_stores_uncertain`, `recording_refused_in_offline_parent`, `second_recording_same_run_ok`,
  `revocation_stops_recording`, `artifact_hash_mismatch_refused`, root `chromium_cannot_read_work`,
  `chromium_tcp_connect_only_forwarder_port` (ABI ≥ 4), `chromium_launches_under_helper_sandbox`.
- Acceptance additions to H15 (Ubuntu 22.04/24.04, Debian 12): synchronous execute_program within one Claude run;
  D8 second-run queueing still holds while a helper runs; recording stop after run end; record (not decisions) the
  Debian 12 `ptrace_scope` default, the 22.04 open-time-credentials probe, and the Landlock ABI of each kernel.

## 8. Decisions (owner, 2026-11-08)

- **Q1 Launcher form → A.** In-trampoline fork, no exec, stays non-dumpable, no new signed artifact. Conditions
  (F12): exact launcher fd allowlist with 0/1/2 → `/dev/null`, minimal Landlock (never `/home/employee`, `/config`,
  `/run`, `/tmp`, `/dev/shm`), `FailurePage` unmapped after marking, ready pipe awaited before `all_isolated`.
- **Q2 Helper memory/pids limit → A, mandatory.** Per-slot cgroups `hx/fg`, `hx/rec` created by spawnd outside the
  unit with `ProtectControlGroups=yes` kept (F6); cgroup kill after every call; spawnd watchdog (F5); fail closed
  with `helper_cgroup_unavailable`. The CVE-2021-4197 open-time-credentials fix must be proven on the Ubuntu 22.04
  5.15 kernel; if it is absent, helpers are refused on that host. Option B (no delegation) is not offered.
- **Q3 Recording egress → A.** Recording only within the parent run's egress profile. Enabling recording grants
  this employee's runtime public-web egress, and the admin UI must say so. Landlock TCP port pinning where
  ABI ≥ 4.
- **Q4 Parallel calls → A.** FIFO depth ≤ 4 per run; queued memfds/fds count against admission budgets; queued
  calls dropped on run end, runtime exit, capability revocation and MCP cancel. If HX4 cannot deliver these, ship
  B (`helper_busy` at once).
- **Q5 CLI tool timeout → A.** CLI MCP tool timeout ≥ 305 s via the signed projection; MCP acceptance re-run per
  runtime. MCP `notifications/cancelled` and connection close map to `HelperCancel`.
