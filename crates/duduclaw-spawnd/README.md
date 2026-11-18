# duduclaw-spawnd

Root-owned launch coordination for the planned native Linux employee isolation.
The daemon accepts only the gateway account on the fixed seqpacket socket and
serializes capability-holding runs for each employee. It never executes employee
code itself or falls back to the gateway identity.

This package is an integration component, not a complete protected installation.
The production Host now installs the native B1/B2 transport adapter: it prepares
root-authorized descriptors, sends a sealed, nonce/run-bound manifest to
`duduclaw-run`, and consumes the bootstrap status protocol. It also includes root
account recovery, durable UID/run/allocation journals, real systemd transient
units, detached mount/executable preparation, bounded lifecycle jobs and an
independent intrusion monitor. A missing trusted runtime publication, verified
installation/readiness producer, DNS evidence or production storage setup still
refuses launches; installing the units does not enable employee isolation.

Protected Linux TEAM gateway boot selects SpawndBackend and applies the composite
admission gate to subprocesses and API-only model calls. Every required component
must be ready. The dashboard remains available while missing integrations keep
employee execution closed. This wiring is not production acceptance.
See [Package B1](../../docs/architecture/native-linux-isolation.md#package-b1)
for adapter contracts and remaining cross-package acceptance.

### Run-unit identity and set-id policy (R7g)

`UnitPolicy::new` sets neither `User=`/`Group=` nor `RestrictSUIDSGID=` on the
transient run unit. Measured on systemd 255: an explicit `User=root` with
`NoNewPrivileges=` and any seccomp restriction removes `CAP_SETUID` from the
trampoline's permitted/effective sets, and `RestrictSUIDSGID=` makes every
`openat2` ENOSYS. `Systemd::start` therefore reads back `User`/`Group` as empty
and `RestrictSUIDSGID` as false (`RUN_UNIT_DEFAULTED`), then, before the
bootstrap offer, requires `/proc/<MainPID>/status` to show uid/gid 0 and
`CapPrm` = `CapEff` = `CapBnd` = `TRAMPOLINE_CAPABILITIES`, no ambient
capability and no_new_privs, for a process in the unit's own cgroup. Any
mismatch stops the unit. Set-id files stay inert (nosuid writable mounts,
no_new_privs, no employee capability) and the trampoline's common seccomp
filter refuses set-id modes in the chmod/mknod families. Root test:
`iso_root_run_unit_policy_keeps_setuid_and_openat2`.

The static `duduclaw-spawnd.service` and `duduclaw-isolation-ready.service`
(`scripts/linux-protected/spawnd`) do not set `RestrictSUIDSGID=` for the same
`openat2` reason: spawnd's work broker, storage adapter, cgroup leaves,
bootstrap mounts and the trusted-file reader (`duduclaw_updated::linux::files`,
also used by `wait-consistent`) all open with `openat2` and RESOLVE_*. They keep
`User=root` (neither needs `CAP_SETUID`) and `NoNewPrivileges=yes`; pinned by
`update_units_reaching_openat2_set_no_restrict_suid_sgid`.

### Native CLI home on older kernels

Codex/Grok homes first try composing the persistent private clone beneath the
detached tmpfs. Linux 6.8 `do_move_mount()` requires the target in the caller's
mount namespace; Linux 6.15 permits eligible anonymous targets via
`may_use_mount()`. Only `EINVAL` from that composition selects the compatibility
path; kernel version strings are never consulted and other errors fail closed.

The compatibility worker unshares `CLONE_FS | CLONE_NEWNS`, makes its namespace
recursively private, and attaches the home over its own `/tmp` before adding
`data` and the root-owned config directories/links. It returns a recursive private
detached clone and exits, destroying the temporary namespace on success or error.
No host scratch directory is created, no original persistent mount or propagation
is changed, and no employee process enters the composition namespace. B2 receives
only the complete tree through the existing descriptor/readback contract. Cache
ownership, external Codex refusal and the prohibition on gateway env copying stay
unchanged. A missing/non-directory/symlink `/tmp` or denied namespace operation
refuses the launch.

The root test below exercises automatic selection and a forced `EINVAL` path for
both runtimes, including config/cache ownership, mount flags, persistent data,
unchanged source propagation and attachment in a subsequent namespace. Run on
each supported kernel; compilation and portable selection tests are not root
acceptance:

```sh
cargo test -p duduclaw-spawnd --lib native_home::tests::iso_root_native_home_projects_root_configs_and_refuses_external_codex -- --ignored --exact --test-threads=1 --nocapture
```

The related-call audit found no other detached-target composition in
`duduclaw-spawnd`, `duduclaw-run`, `duduclaw-isolation-storage` or
`duduclaw-execution`. Spawnd's bootstrap/storage `open_tree` sites only clone;
run's root is attached before its input, system, certificate, tmpfs and device
submounts, and proc is attached after pivot. Run's probes use invalid descriptors
and its root fixtures only clone. The latter two crates contain neither syscall.

## Build and verification

Only Linux runs the service. The policy and admission tests are portable.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --target x86_64-unknown-linux-gnu --offline
```

Privileged tests are opt-in with the `iso_root_` prefix; run them only in a
disposable Linux VM. The service accepts no paths, argv, environment, uid or
systemd properties from its caller. `--check` exits unsuccessfully if any required
integration/readback is missing. It prints the root-owned readiness evidence from
`/run/duduclaw-spawnd/readiness.json`; boot identity and a one-second expiry prevent
stale evidence from authorizing a launch. It never treats process liveness as readiness.

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd -- --ignored iso_root_ --test-threads=1
```

Those privileged tests are compiled but have not been run in this worktree.
They require a disposable root Linux VM and the installation prerequisites named
in each ignore reason. The account test modifies login ACLs; the systemd resource
test requires the complete trusted service hierarchy.

Every run unit carries `RestrictNamespaces=mnt ipc uts net pid`
(`systemd::TRAMPOLINE_NAMESPACES`). Under it systemd's seccomp filter returns
ENOSYS for `clone3`, so `duduclaw-run` creates its PID-namespace init and HX
helpers with legacy `clone(CLONE_PIDFD | SIGCHLD)` and never probes `clone3`.

## Egress control client (v1.83 R7e)

spawnd is the only client of `/run/duduclaw-egress/control.sock`, which
`duduclaw-egress.socket` creates through PID 1 as `root:root 0600` in a
root-owned 0755 directory. Because PID 1 created and listens on it, neither the
socket file owner nor `SO_PEERCRED` identifies the service (both are root).
`Seqpacket::connect_fixed_egress` therefore requires exactly that endpoint shape
(every ancestor a root-owned directory without group/other write, the socket
`root:root` mode 0600 and not a symlink), enables `SO_PASSCRED` before
connecting, and returns an `EgressSeqpacket` whose only receive path requires,
for every frame, exactly one kernel-attached `SCM_CREDENTIALS` naming the
`duduclaw-egress` uid/gid with a non-zero pid. A frame without credentials, with
several, with another account's or with truncated control data is refused, the
connection is shut down and the cached client dropped; the next call reconnects.
The egress account is never uid 0. The egress service is unchanged and still
requires a root peer. The generic `Seqpacket::recv` (gateway, update control)
keeps refusing any credentials message.

## Update control endpoint (v1.83 R6)

`update_control.rs` serves `/run/duduclaw-spawnd/update.sock` (root 0600 in the
root-owned `/run/duduclaw-spawnd`) for `duduclaw-updated`'s activation client:
only a uid 0 / gid 0 `SO_PEERCRED` peer is answered; one `SOCK_SEQPACKET` frame
of the `activation_contract` types per connection, no ancillary data, unknown
fields refused, 5 s timeouts. Every refusal is the single `Refused` outcome.

- `Prepare` refuses a manifest with data migrations (their executor, G's, is
  not part of this release) and checks the fixed destinations. Before the
  generation is staged (the updater's preflight precedes its download) that is
  all; once staged, the manifest must hash to the generation digest, equal the
  request's preparation, and every artifact must pass `verify_fd`.
- `Drain{g}` writes `/var/lib/duduclaw-spawnd/drain.json` durably (temporary,
  fsync, rename, directory fsync) **before** the in-memory barrier closes, then
  replies with the admitted runs counted by an admission iteration that began
  after the closure. New `StartRun`s are refused (`not_ready`); queued and
  running runs are kept. The updater retries until the count is zero. An
  unhealthy admission iteration (its tick failed) is `Refused`, whatever the count.
- `Apply` requires the barrier closed for exactly that generation in memory and
  on disk, and zero admitted runs in a fresh healthy iteration. It verifies the
  whole generation first (manifest digest, signature against the trust ledger,
  every artifact from its descriptor), then writes the publication journal (see
  below), then copies spawnd, run, run self-test (R7a), SSH denial probe (R7c), parse worker, `/usr/lib/duduclaw/current/*`,
  egress and gateway from the generation directory, hashing each file from its
  descriptor while copying, to `<name>.next`, fsync, rename, directory fsync;
  finally every destination file and directory is synced again and the journal
  says `published`. While a journal of another generation is `publishing`,
  `Apply` is refused except for that journal's previous generation (the roll-back).
- `Readback` reports spawnd itself (a live-process report:
  `sha256(/proc/self/exe)`, liveness of the admission loop, euid 0) and a FILE
  MEASUREMENT of every other non-daemon component: the SHA-256, by descriptor,
  of every file the component is executed (the policy: read) from, opened through
  the trusted reader (no symlinks on the way, root:root, not group/other-writable,
  one link, regular file, the signed size, at most 768 MiB). Measured at the
  fixed destination: run, run self-test (R7a), SSH denial probe (R7c) in
  `/usr/lib/duduclaw`; importer and migration in `/usr/lib/duduclaw/current`
  (the migration pipeline, `duduclaw-import@`, `duduclaw-migrate-expire@`), all
  mode 0755. The parse worker at BOTH the copy its unit executes,
  `/usr/lib/duduclaw/parse-root-tree/usr/libexec/duduclaw/duduclaw-parse-worker`
  (`RootDirectory=` + `BindReadOnlyPaths=…parse-root-tree:/`), and the host copy
  `/usr/libexec/duduclaw/duduclaw-parse-worker` the tree builder copies from;
  ready only when both hold the signed bytes. The runtime tree is built by the
  installer only and an `Apply` does not rebuild it, so an update that changes
  the parse worker fails closed at readback until it does.
  Measured in the committed `generations/<hex>` directory of the generation
  being read back (never through `current`; mounted at `/runtime` for runs):
  the MCP bridge and the egress forwarder (0755) and every runtime entry (0755;
  the signed `isolation-policy-v1` 0444). The signed mode must be the one
  `publication` allows for that kind. Bounded: every `open`, `fstat` and `read`
  of a measured file (including the identity re-check of a cached digest) runs
  on one dedicated measuring thread (`spawnd-update-measure`, started by the
  first `Readback`, alive as long as the endpoint); the request only reads the
  thread's result table and waits for it at most 2 s (a hard condvar timeout,
  well inside the client's 5 s socket timeout), then answers with what is
  finished. At most one measuring pass is in flight: a `Readback` of the
  generation in progress waits on that pass, one of another generation replaces
  the pending job and cancels the current pass at its next check (before every
  file and every 64 KiB chunk). A pass is not cut short by a request's wait, so
  the updater's retries (every 200 ms for 30 s) see more and more of it. A
  syscall that never returns (failing disk, stalled storage) blocks only the
  measuring thread: every `Readback` still answers within 2 s with the affected
  artifact and every later one not measured, so the update fails closed when
  the client gives up; that thread is neither killed nor replaced (no second
  measuring thread is ever started while one exists). A manifest whose measured
  files exceed 4 GiB in total is not measured at all (decided before anything
  is opened, logged once), and completed digests are cached per generation
  (dropped when another generation is measured, at most 256) with the file's
  `fstat` identity (device, inode, size, mode, mtime, ctime; equal before and
  after hashing) so a later pass re-hashes only what changed or did not finish.
  A record is `ready` only when the measured digest is the
  artifact's and spawnd's euid is 0; product, version and protocol are then the
  manifest's facts of exactly those bytes and `effective_uid` is the measurer's
  (0). A file that cannot be opened, verified or measured in time reports
  `ready: false`; nothing is guessed.
- Read-back rule (all three daemons): a component's read-back `ready` means it
  is serving with a complete identity (spawnd: a recent healthy admission-loop
  iteration; gateway/egress: their read-back endpoint serving). Whole-host
  readiness (spawnd's evidence, the gateway's `isolation_readiness`) is never an
  input of the updater's verification: both need the updater's probe answer,
  which it gives only after its start-up verification, so using them there is a
  circular wait. Whole-host readiness is enforced at admission instead (spawnd's
  `Readiness::check`, the gateway's `check_execution` before every run).
  Evidence: whenever the set of false checks changes, spawnd prints one stderr
  line, `evidence incomplete: <readiness.json names>` with the first failing
  resources sub-condition as `resources_verified(<name>)` (`ResourceCheck`,
  e.g. `employees_slice_cgroup`), or `evidence complete`.
- `Resume{g}` requires that no journal exists or that it says `published` for
  `g`; it removes the journal, then the record, durably, then reopens admission.
  It is the only way to lift the barrier. At start the record is read strictly
  before the first admission; an unreadable record keeps admission closed until
  a new `Drain`. A process that accepted a publication (`Apply` past its gate
  check) refuses every `Resume` and changes nothing, and refuses an `Apply` of
  any other generation; `Prepare`, `Drain`, `Readback` and a retried `Apply` of
  the same generation are still served. Only the restarted, re-verified spawnd
  resumes.

### Publication journal and start-up recovery

Each file rename is atomic; the set of eight is not. `Apply` therefore keeps
`/var/lib/duduclaw-spawnd/publish.json` (root 0600; format
`duduclaw_updated::publish_journal`, schema 1, fixed across versions, strict
parsing: bounded, unknown field or schema refused, canonical bytes only):
`{schema, generation, manifest_sha256, previous, state}`. `previous` is the
generation installed before (`current` when no journal exists; the journal's own
`previous` on a retried `Apply`). Written `publishing` (temporary, fsync, rename,
directory fsync, read back) before the first destination is touched; rewritten
`published` after all eight are in place and synced. Removed only by the
`Resume` of its generation, i.e. after the updater committed (or rolled back to)
it, so until then the previous generation stays known, and the updater's
generation pruning keeps both directories named by the journal.

At EVERY start, before any socket is bound (update, control, allocation) and
before admission (`update_control::recover`, holding the lifetime flock
`/var/lib/duduclaw-spawnd/update.lock`), the installed set is verified against
the trust ledger including its revocation fence (`verify_recorded` =
`TrustLedger::verify_installed`):

- A journal, `publishing` or `published`: its generation is fully re-verified
  (manifest digest equal to the journal's, signature, every artifact by
  descriptor) and published again with the idempotent procedure (files already
  holding the right bytes are kept, an altered one is replaced; journaled
  `publishing` → `published` like any `Apply`). If that fails (directory
  missing or corrupt, signer revoked), the journal's `previous` generation is
  verified and published with the same procedure (the new journal names the
  failed generation as its fallback).
- No journal: the generation the `current` selector names is verified the same
  way, and every one of the eight destinations must already hold its bytes
  (read-only: without a journal there is no recorded intent to complete). An
  isolated install always has a selector from its first boot on:
  `duduclaw-bootstrap install` (`trust_init::publish_at`) publishes the initial
  `current` and records its manifest in the trust ledger, and the installer
  places the eight components, so a fresh install before its first update is
  verified exactly like any other host. No selector is not consistent.
- Otherwise (neither generation verifies, the journal is unreadable or of an
  unknown schema, the selector is absent or unreadable, the selected generation
  does not verify or a destination differs) the set is NOT consistent:
  admission is closed (`recovery_incomplete`; no `Resume` lifts it).
  An unreadable journal must be inspected and removed by an operator.

Then (`update_control::start`): `update.sock` is bound and `READY=1` sent,
ALWAYS. `READY=1` means only "the root update endpoint is available".

- Consistent: the allocation socket (`LinuxHost::open`), the control listener
  and admission are opened; then the status says `consistent` for this process.
- Not consistent: nothing else is initialised (only audit, the status file and
  `update.sock`). `Drain` closes and records the barrier; it answers zero
  admitted runs only when the workload census is complete and idle (below),
  otherwise it is refused. `Resume` is refused. A successful `Apply` (same
  census precondition) repairs the set: spawnd sends the reply, writes the
  status (`recovering`) and exits with status 82 (`RESTART_AFTER_REPAIR_EXIT`);
  the unit restarts exactly that status (`RestartForceExitStatus=82`,
  `SuccessExitStatus=82`), and the new process runs the full verification
  before opening anything. There is no in-process switch.
- Opened, then a request leaves the set not consistent (an `Apply` that failed
  after the journal said `publishing`, or a journal that no longer reads):
  before the reply, admission is closed (`recovery_incomplete`) and every
  further request is refused; after the reply the status is written
  (`inconsistent`), an `update_set_inconsistent` audit record is written
  synchronously (acknowledged by the audit worker, which also flushes every
  record queued before it), and spawnd exits with status 83
  (`EXIT_SET_INCONSISTENT`). `Restart=on-failure` restarts it (counted against
  the start rate limit), and the start-up recovery completes or rolls back the
  publication. The consistency gate and its dependents stop with the process.

Workload census (`Effects::workloads`, production `system_census`): PID 1's
`ListUnits` over the system bus, counting every `duduclaw-run-<id>.service`
(exact name) in a state other than `inactive`/`failed`, and
`/sys/fs/cgroup/duduclaw.slice/duduclaw-employees.slice/cgroup.events`, which
must say `populated 0` or be absent (every run unit lives below it). The slice
itself is started with spawnd (`Wants=`/`After=duduclaw-employees.slice` in the
unit): it exists from install and after every reboot with zero employees, its
cgroup is realised before spawnd's mount namespace is built (so the
`-`-prefixed cgroup `ReadWritePaths=` entry is bound), and `resources_verified`
requires it (`employees_slice_cgroup` while it is not started). The cgroup
root must be the unified hierarchy (`cgroup.controllers`). Nothing comes from
this process's memory, so runs started by an earlier spawnd process whose
asynchronous teardown is still in progress are seen. A census that cannot be
completed refuses `Drain` (without an admission loop) and `Apply` (always, in
addition to the admission loop's own zero count).

The barrier restored from a record and the start-up verification are audited
(`update_barrier_restored`, `publication_recovery` with the action and the
journal's generations, or the selected generation; nothing for a verified
start without a journal). These records are written synchronously: each is
acknowledged by the audit worker after its file and directory fsync (at most
5 s) before the start-up status is reported; one that is dropped, fails or is
not acknowledged in time makes every later `STATUS=` line of the process end
in `; audit_write_failed`.

Operator status: `/run/duduclaw-spawnd/update-status.json` (root 0600, atomic
replace, written at start, once everything is open and after every request on
`update.sock`; the wire contract has no status request). Schema 3. Fields:
`set` (`consistent`, `recovering`, `inconsistent`), `closed_reason` (with
`inconsistent`: `publication_incomplete`, `journal_unreadable`,
`selector_absent`, `selector_unreadable`, `selected_unverified`), `owner`
(`boot_id`, `pid`, `invocation_id` = the service start's `$INVOCATION_ID`;
`null` outside a systemd start),
`admission_closed`, `barrier` (`open`, `drained`, `unreadable_record`,
`recovery_incomplete`) and `barrier_generation`, `journal` (`absent`,
`publishing`, `published`, `unreadable`), `journal_generation`,
`journal_previous`, `publication_consistent`, `startup_barrier` /
`startup_barrier_generation` and `startup_recovery` (`no_journal`,
`reverified`, `rolled_forward`, `rolled_back`, `incomplete`,
`journal_unreadable`, `selector_absent`, `selector_unreadable`,
`selected_unverified`). Digests are lowercase hex. The same state is sent as one
`STATUS=` line (`consistent`, `recovering`, `inconsistent: <closed_reason>`)
whenever it changes, and logged to stderr (the unit routes stderr to the
journal); a failed status-file write appends `; status_file_write_failed`, a
synchronously written audit record that did not land `; audit_write_failed`.

### Unit ordering (`duduclaw-isolation-ready.service`)

- `duduclaw-spawnd.service`: `Type=notify`, `TimeoutStartSec=300` (bounds the
  verification before `READY=1`), `TimeoutStopSec=90`;
  `Wants=duduclaw-isolation-ready.service`, so every start of spawnd (boot,
  manual, `Restart=`) starts the gate again (a `[Unit]` dependency; nothing is
  enabled). Exit 82 (repair) and 83 (inconsistent set) are restarted; see the
  scripts README for the start rate limit and `systemctl reset-failed`.
- `duduclaw-isolation-ready.service` (next to the spawnd unit): oneshot,
  `RemainAfterExit=yes`, `BindsTo=`/`PartOf=`/`After=duduclaw-spawnd.service`
  (stopped whenever spawnd stops, fails or restarts; restarted with it), runs
  `duduclaw-spawnd wait-consistent`, `TimeoutStartSec=infinity` (the only
  unit without a start timeout), no network, read-only filesystem, no
  capabilities. The waiter exits 0 only when the status file says
  `consistent`, its `owner.boot_id` is the current boot, and `owner.pid` and
  `owner.invocation_id` are exactly the `MainPID` and `InvocationID` PID 1
  reports for the active `duduclaw-spawnd.service` (system bus, read-only
  properties); a missing, unreadable, stale (earlier process, invocation or
  boot) or inconsistent status keeps it waiting. It never opens `/proc/<pid>`
  (so `hidepid=` and spawnd's non-dumpable flag do not matter) and needs no
  capability; the trade-off is a dependency on the system bus (spawnd is
  ordered after `dbus.service`; while the bus is down the waiter keeps
  waiting). On a host whose `/sys/fs/cgroup` is not the unified cgroup v2
  hierarchy it exits 1 at once with a fixed message (the isolated install
  requires cgroup v2: plan S1 HostPreflight).
- `duduclaw-egress.service`: `BindsTo=`/`After=duduclaw-isolation-ready.service`.
- Isolated-install gateway unit (written by the installer package, R3): must
  contain `duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES`
  (`BindsTo=duduclaw-isolation-ready.service` and
  `After=duduclaw-isolation-ready.service`), so it stops whenever the gate
  drops. The single-binary gateway unit is unchanged (no spawnd on those hosts).
- `duduclaw-updated.service`: `Wants=`/`After=duduclaw-spawnd.service` only, so
  it starts on an inconsistent set and can repair it, and spawnd restarts never
  stop or restart it. Not `Requires=`/`BindsTo=`/`PartOf=`: the updater itself
  restarts spawnd during an activation (`systemctl reset-failed`, then a
  blocking `systemctl restart` of spawnd, egress and the gateway, each waiting
  for its own job's `done` result, then checking every unit for no pending job
  and `active` under a new `InvocationID`, all against one absolute deadline,
  `RESTART_DEADLINE` = stop 90 s + start 300 s + 60 s), which those would
  propagate to the updater. The spawnd process that accepted the `Apply`
  refuses `Resume`: only the restarted, re-verified spawnd reopens admission.

The unit grants spawnd write access to `/usr/lib/duduclaw`, `/usr/libexec/duduclaw`
and `/opt/duduclaw/bin` for this publication: an atomic replacement creates
`<name>.next` in the destination directory and renames it over `<name>`, which
needs write access to the directory, not only to the file. This is broader than
the eight fixed file names; narrowing it requires moving publication into a
separate, narrowly privileged publisher helper (known residual, plan R6).

## Package H adapter

`storage::LoopStorage::open(storage_directory, journal_directory)` implements the
B1 storage seam with `duduclaw-isolation-storage::EmployeeStorage`. Both paths are
root installation inputs, not gateway request fields. The journal directory is a
separate root:root 0700 directory outside H's image directory (H rejects unknown
files). A lifetime flock protects the B1 journal; H separately locks its images.
The daemon now calls `Components::installed()`: it attempts the real H adapter at
fixed root installation paths `/var/lib/duduclaw-spawnd/images` (H images) and
`/var/lib/duduclaw-spawnd/storage` (B1 journal). There is no environment, config or
RPC override. The installer creates both root:root 0700 directories on
H-supported ext4/XFS backing with the required capacity reserve. On every start,
`duduclaw-spawnd.service` uses `MountFlags=private` to make the tree private in
spawnd's mount namespace; employee images are mounted only there. The installer
creates no host mount. Boot opens verified existing anchors and their metadata;
it does not create those directories or provision an employee image.
Any failure retains UnavailableStorage. The generic `Components::default()` stays
refusing for explicit/incomplete compositions. Installation evidence and root
runtime publication remain missing, so this does not enable TEAM execution.

Provision and destroy bind the complete proposal digest to the operation id.
Employee/UID tombstones remain after destruction. Run attachment intent and a
fresh generation token are fsynced before H mounts anything; after restart B1 can
recover that exact run even when attach failed before returning a receipt. A
stale receipt cannot detach a newer generation. Unknown images, missing Ready
images, unleased mounts and lost/invalid ledgers refuse readiness. Incomplete
maintenance remains fenced. The same approved operation can recover a completed
H image's temporary mount; H Creating/Quarantined images are not automatically
resumed and require explicit destruction or operator data recovery. Any failed journal publication poisons the current adapter.

H's `images()` reports kernel-verified image/mount identities for reconciliation.
Before reporting usage the adapter calls H readiness and reads real `statvfs`;
when idle it briefly mounts and detaches under a durable maintenance phase.
**CONTRACT CHANGE (I1b round 5):** StartRun optionally carries a bounded
`workspace` selector and one trailing directory fd after its existing prompt
and attachments (66 descriptors maximum; the 64 attachment cap is unchanged).
The fd pins D's AuthorizedWorkspace directory while queued. Root never mounts
that supplied fd: after H attachment it reopens only `employee-work` or
`.forks/<32 lowercase hex>` plus a validated relative cwd under the H work
mount using openat2 BENEATH/NO_SYMLINKS/NO_MAGICLINKS/NO_XDEV. Employee UID,
H device and authorization device/inode must all match. The reopened directory
becomes the existing detached `/work` mount, narrowing a nonempty cwd to that
subdirectory; the B2 bootstrap schema remains unchanged. Missing selectors retain
the original work mount. Old roots reject new fields; mixed versions fail closed.
Native root mount/queue acceptance is still required.

Both home and workspace are conservatively noexec. Only private detached clones
of their directories reach B1/B2, never images or block devices. B1 still owns the
unit/cgroup/UID-empty proof and must release every clone before detach/destroy.

The ignored `iso_root_storage_adapter_reopen_generation_and_proposal_binding`
test uses real loop storage and checks reopening, stale-token refusal and proposal
binding. It needs the same `ISO_STORAGE_TEST_ROOT`, private mount tree, ext4/XFS
backing and host capacity reserve as package H's root tests. It has not been run
in this worktree; it is not an end-to-end employee launch test.

### Package H work broker wiring (H4)

The control socket now answers the four `ipc::Opcode::WORK` ops; spawnd
advertises `SpawndFeatures::WORK_BROKER` (readiness report and every
`Reserved`, the one feature set shared with HX4's `helper_calls`) so a gateway
never sends them to an older spawnd (which closes the connection on an unknown
`op`).

- **Asynchronous replies.** `OpenWorkFile`, `WorkAnchor`, `StopEmployeeRuns`
  and `EmployeeIdle` are queued as coordinator tickets; the event loop never
  blocks. The connection gets no other reply until the deferred one is sent,
  always within `BROKER_REPLY_MS` (4 s, below the client's 5 s); a late host
  result (and its fd) is dropped and the caller sees
  `workspace_broker_unavailable`.
- **OpenWorkFile.** Coordinator: owning connection (`NotFound`), the run's own
  employee (`Unauthorized`), admission `Running` and no pending stop
  (`NotReady`), caps of 4 outstanding snapshots per run and 16 overall
  (`resource_limit`). Lifecycle thread, same worker turn: H2 `check_work_path`,
  `LiveRunView` from the core's own `LiveRun` (not closed, trampoline Running,
  unit present, the run fence not raised) and `verify_lease` against the
  durable lease inventory. Copy: on a separate `spawnd-work-broker` thread
  (bounded FIFO, one copy at a time, so a 10 s H2 copy never stalls polls,
  fences or cleanup); the fence is re-read before and after the copy, and the
  coordinator re-checks Running before the single `WorkFileRead` fd is sent.
- **Broker root.** `Core::start` takes `bootstrap_mounts::work_broker_root`
  (detached RO/nosuid/nodev/noexec clone of the whole H work tree) BEFORE
  `select_workspace` narrows `/work`. `cleanup_run` releases it first and
  waits (`NotReady`) while any copy still holds a dup, before the trampoline
  close and any `Storage::detach`. A failed clone disables the broker for that
  run only.
- **WorkAnchor.** Registry account Ready and allocation journal Ready, then
  H3's `Storage::work_anchor`; a missing/invalid anchor is
  `workspace_broker_unavailable`, an unknown/retired employee `not_found`.
- **StopEmployeeRuns (F6 S13).** Cancels every non-terminal run of the
  employee through the per-run cancel path (any connection), refuses new
  reservations and opens for it while pending, and confirms only after every
  run is terminal (Finished = complete cleanup readback) and a fresh root
  `EmployeeIdle` readback is all zero; otherwise `confirmed = false` after
  `STOP_WAIT_MS` (3.5 s). Unknown employee ⇒ `not_found`.
- **EmployeeIdle.** Counts open root runs + `runs.json` records (by employee
  or uid) + non-terminal admission entries, durable leases, processes whose
  uid OR gid fields match (S14), and `duduclaw-run-*` units mapped to the
  employee or to nobody. Any unreadable source is an error, never zero.
- **Anchor backfill.** On start, after reconcile, every allocation Ready in both
  the journal and the registry gets its missing anchor (`work_anchor::backfill`,
  idempotent, never repairs); `work_anchor_backfilled` /
  `work_anchor_backfill_failed` audit events.
- **Audit.** `work_file_opened` (employee, run, source device/inode/size),
  `work_file_refused` (wire code), `employee_runs_stopped` (count, confirmed);
  never a path, file name or content (`audit::Detail`).

Root acceptance: `tests/work_broker_chain.rs`
(`iso_root_work_broker_chain_backfill_snapshot_and_detach`, ignored; root VM,
`ISO_STORAGE_TEST_ROOT`, `DUDUCLAW_ISO_DISPOSABLE=1`, whole binary under
`unshare --mount --propagation private`; see `tests/README.md`). Not run in this worktree.

### Root retire (F6l, F6 design r5 §3.12)

The allocation socket speaks schema 1 (unchanged v1.80 semantics) and schema 2.
A schema-2 reply carries `error` (`not_found`, `inconsistent`, `not_ready`,
`invalid_request`, `isolation_unavailable`; the 4 s worker timeout is
`isolation_unavailable`, never `not_found`), `phase` (ReadBack answers every
phase with the stored receipt) and `idle` (ReadBack of a Ready entry, from the
H4 `EmployeeIdle` sources read in the same worker turn; unreadable ⇒ omitted).

- **Journal schema 3** (`allocations.json`): `Phase` adds `retiring`/`retired`,
  entries add `retire` (the retire proposal binding) and `retired_at`; schemas
  1–2 are read (they can never carry those), 3 is written. A Retired entry no
  longer holds its label; `Allocate` and `Disable` refuse Retiring/Retired; a
  retire binding equal to any allocation or other retire binding fails decode.
- **Missing journal (S5a)**: `Allocator::open` refuses a missing journal while
  the registry holds a non-Tombstone account or storage reports an image or a
  live ledger entry (`Storage::traces`). `NotFound` (S5b) is answered only when
  the journal, the registry (any state) and storage show no trace of the
  expected employee and nothing untracked exists; otherwise `inconsistent`.
- **`Action::Retire`** (schema 2, `EmployeeRetire` item in Applying, approver
  id): retire-only validation (never creates an entry); target binding, label,
  employee and UID must match; a Retired replay is answered before the host
  gate; host busy ⇒ `not_ready`; a Ready entry must read idle (runs, units,
  leases, uid **or** gid processes; a process with no run is audited
  `intruder`); `Retiring` is saved before any side effect; account →
  Tombstone, `recover_locked`, `storage.destroy` with the `Retire` operation
  kind, no image/lease left, then `Retired` and the root audit event `retired`
  (UID, label, last usage, quota, digest prefix, requester, approver, early).
  The UID stays burned (never recycled); the passwd/group entry stays locked.
  Every failure after `Retiring` is resumed by the gateway with the same request.
- Residual (S14): SysV IPC objects and `/tmp`, `/dev/shm` files of the UID are
  not swept. The journal stays capped at 4096 entries for the host lifetime
  (Retired entries are not compacted yet).

Root tests (`allocation_retire_tests.rs`, `iso_root_retire_*`, injected account
and storage effects on a real `/run` registry; the same scenarios also run
portably in `retire_scenarios_with_in_memory_registry`). Not run in this
worktree:

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib iso_root_retire_ -- --ignored --test-threads=1
```

### In-run helper calls (HX4, D8 revision option A)

The four `ipc::Opcode::HELPER` ops are answered when `helper_calls` is
advertised (trampoline launcher support, a publication with launcher plans,
slot cgroups not known to lack open-time credentials); otherwise `HelperCall`
answers `helper_unavailable` / `helper_cgroup_unavailable`.

- **Coordinator** (`admission_helpers.rs`, pure): `HelperCall` only from the
  run's owning connection (`not_found`), for the run's own employee
  (`unauthorized`, F7), while admission is `Running`, no employee stop is
  pending and the runtime has not exited (`not_ready`, F4), with ≥ 5 s of run
  time left (`run_time_exhausted`). Call ids are 16 CSPRNG bytes drawn by the
  server, never zero or reused within the run. One foreground and one
  recording slot; a busy slot queues the call in a per-run FIFO (≤ 4 calls for
  both slots, ≤ 3 s each, then `helper_busy`), and queued/starting input
  memfds count against `pending_input_bytes`/`input_descriptors` together with
  StartRun inputs. The queue is dropped (memfds closed) on run end, drain
  (cancel, revocation, intrusion, timeouts), runtime exit, launcher loss, an
  abandoned ticket (disconnect) and `HelperCancel`. A helper call never creates
  an admission entry, so it never occupies the D8 slot or the host run budget.
  `HelperCall` is a deferred reply (`HelperStarted` + `[StdoutRead,
  StderrRead]`); `HelperWait` is a bounded poll (`not_ready`, then
  `HelperEnded` once; `run_ended` for calls live when the run left Running);
  `HelperStop`/`HelperCancel` reach only the call (F8) — no path to the run's
  revoke/kill. `HelperWait`/`HelperStop`/`HelperCancel` carry `employee_id`
  and are checked like `HelperCall` (owning connection and run employee,
  `unauthorized`); a stop/cancel the worker mailbox refuses is retried every
  tick until delivered or the call ended root-side (security fix round).
- **Root engine** (`helper_host.rs`, lifecycle thread, one per Running run):
  the launcher link (control socket end, slot cgroups, run helper table) moves
  out of the trampoline once (`Trampoline::take_helper_link`). Per call: the
  helper is re-derived from the run's own employee (`RootPolicy::derive_helper`,
  `runtime_denied` if no longer entitled, or if its kind, profile or binding
  digest differs from the run table's after a reload), slot must match the binding, the
  memfd is revalidated (tmpfs, seals, size, CLOEXEC; `invalid_fds`), effective
  timeout = min(tool, binding, run deadline − termination grace − 5 s), slot
  must be empty, the signed binding's `memory.max`/`pids.max`/`cpu.max` are
  written and read back, then `Start` with `[memfd, stdout w, stderr w]`.
  Outcome is root-side (F5): own watchdog at effective + 2 s writes the slot's
  `cgroup.kill`; OOM from the `memory.events oom_kill` delta; after every call
  the slot is killed, drained (`populated 0`) and closed again before the end
  is reported. Launcher frames are recvmsg'd with `MSG_CMSG_CLOEXEC` and a
  one-fd control buffer (F14): any fd/cmsg, truncation, illegal frame, > 64
  frames/s or EOF closes the channel, kills both slots and makes later calls
  `helper_unavailable`. `rt-runtime` `populated 0` (after populated) or the
  launcher's `Draining` reports Draining (F4). Run cleanup shuts the engine
  down (live calls `run_ended`) before the trampoline is closed.
- **Audit**: `helper_started` / `helper_ended` (call id, signed helper index,
  slot, input bytes, effective timeout / outcome), `helper_refused` (code),
  `helper_channel_closed` (code). No argv, path, input or output content.

Root tests (`iso_root_helper_watchdog_kills_slot_with_stopped_launcher`,
`_oom_reported_from_memory_events_not_frame`, `_fg_kill_leaves_rec_running`,
`_escaped_daemon_killed_after_call`) are not written yet: they need a real
launcher run (follow-up); the unprivileged halves are in `helper_host_tests.rs`.

## Landlock evidence and logging

B2's probe returns the detected kernel ABI and the selected ruleset ABI without
printing them to employee stderr. The B1 readiness interface carries both fields;
when a trusted installation producer returns verified evidence, the daemon logs
the pair once per daemon process to stderr and publishes it as `EvidenceReport.landlock`
(`detected_abi`, `selected_abi`). The supplied service routes stderr to the
journal (rate-limited); readiness remains the retained operator evidence. The selected ABI cannot exceed 7.
Neither kernel probing nor an arbitrary configuration flag makes this readiness
evidence available. The bootstrap receipt's selected ABI alone is not an
installation self-test or an operator readiness report.

### R7a: trampoline facts from spawnd's own self-test

The installed adapter (`installed::install`) gives `NativeTrampoline` a
`selftest::SelfTestInstallation`; no installer receipt supplies these facts any
more. `trampoline_verified` requires both `/usr/lib/duduclaw/duduclaw-run` and
`/usr/lib/duduclaw/duduclaw-run-selftest` to match the committed signed
generation's `Trampoline` / `TrampolineSelftest` artifacts (sha256, size and
mode, by descriptor; the check moved here from the receipt reader).
`landlock_self_test` and `namespace_self_test` are the cached pass of the fixed
unit `duduclaw-isolation-selftest.service`, which spawnd starts through PID 1
(`Systemd::run_fixed_unit`: only the members of `systemd::FixedUnit`, this unit
or an R7c probe instance; PID 1 must load it from
`/etc/systemd/system/duduclaw-isolation-selftest.service` with no drop-in, no
pending daemon-reload, not transient and not already running; PID 1 must
report the run units' `NoNewPrivileges`, `TRAMPOLINE_CAPABILITIES`, no ambient
capability and `TRAMPOLINE_NAMESPACES`, and `User`/`Group` empty and
`RestrictSUIDSGID` false (`RUN_UNIT_DEFAULTED`), every mismatch named as
`invalid unit policy: fixed unit readback <Key>: …`; the journal is never
parsed). `run_fixed_unit` holds a `Unit.Ref` on the unit from before the
pre-start readback until it returns, released on every path including errors
and the deadline, and refuses without starting if the reference cannot be
taken: unreferenced, PID 1 collects the finished oneshot and every outcome
field reads 0. A run passes only on a new `InvocationID`, `ActiveState=inactive`,
`Result=success`, `ExecMainCode=1` (exited), `ExecMainStatus=0`, a non-zero
`ExecMainPID`, and `ExecStart`'s single pinned record agreeing with that pid
and exit, started after the record read before the start and exited after it
started; anything else fails. The ABI pair is the live probe.
The outcome is cached under (boot id, committed generation, detected/selected
ABI, both artifact identities): a pass is reused for that key, any key change
runs the unit again, a fail is reused for 60 s before one retry. The run happens
on a background thread first requested by the first readiness refresh after
start-up recovery, so it never delays the sockets, the update endpoint or the
refresh loop (a run in progress reads as "not yet passed"). The protected
installer creates the `duduclaw-selftest` system account and installs the unit
file (`protected_linux_layout.rs`) before it starts spawnd; without them this
readiness fails closed.

### R7c: SSH denial from spawnd's own probe through PID 1

There is no installer receipt any more: `installed.rs` reads nothing under
`/run/duduclaw-isolation-evidence`. `/usr/lib/duduclaw/duduclaw-isolation-probe`
(signed target `IsolationProbe`, root:root 0755, built from
`crates/duduclaw-isolation/src/bin/probe.rs`) and the template unit
`duduclaw-isolation-probe@.service` (embedded here as
`systemd::ISOLATION_PROBE_UNIT_TEXT`, executable `systemd::PROBE_EXECUTABLE`,
names `duduclaw_isolation::ids::ISOLATION_PROBE_UNIT_PREFIX` /
`ISOLATION_PROBE_UNIT_TEMPLATE`) prove per account against the live sshd that
a public-key login and a `-N -R` forwarding session are refused (client
`Permission denied (publickey).` plus sshd's root `DenyUsers` journal record
for the probe's own source port). The probe runs UNPRIVILEGED as the isolation
test account (`duduclaw_isolation::ids::SELFTEST_USER`, `duduclaw-selftest`)
with the supplementary group `systemd-journal` (journal read access) and no
capability; it refuses to run as root. "NOT denied" (exit 10/13) is reported
only with sshd's own root `Accepted publickey for <account> from 127.0.0.1 port
<source port> ssh2: ED25519 <fingerprint of this run's ephemeral key>` record;
a client that succeeded (or stayed connected) without it is
`login_unverified`/`forward_unverified` (18/19), inconclusive, never a
tombstone: something else answering on an unbound sshd port cannot burn a
uid. The probe accepts journal records from `sshd`, `sshd-session` and
`sshd-auth`; the DenyUsers record (no port) is bound to the source port by
`_PID` equality with a record that carries it (OpenSSH ≥ 10 split-process
correlation is not verified on a real host; the supported distributions ship
≤ 9.6). The exit-code table is `duduclaw_isolation::probe_exit` (shared by the
probe and spawnd; also in `scripts/linux-protected/spawnd/README.md`).

Fixed-unit set. `Systemd::run_fixed_unit` accepts exactly the members of
`systemd::FixedUnit`: the self-test unit (unchanged) and
`duduclaw-isolation-probe@<account>.<port>.service` (`FixedUnit::Probe {
account, port }`) for an account name passing `employee_account_name_valid`
and a port 1..=65535 in canonical decimal (`duduclaw_isolation::ids::probe_instance`:
exactly one `.`). The port is the first `port` of spawnd's own `sshd -T` for
the account (shared parser). Each member fixes its fragment file (the
instance's is the template `/etc/systemd/system/duduclaw-isolation-probe@.service`;
`Id` is the instance), the shipped bytes, the exact `ExecStart` argv
(`[PROBE_EXECUTABLE, "<account>.<port>"]`) and the readback (`systemd::probe_unit_readback`):
`Type=oneshot`, `Slice=duduclaw-tcb.slice`, `NoNewPrivileges`, empty
`CapabilityBoundingSet` and `AmbientCapabilities`, `User` = `Group` =
`duduclaw-selftest`, `SupplementaryGroups` = exactly [`systemd-journal`] (D-Bus
`as`), `RestrictSUIDSGID` false,
`IPAddressDeny` = {`::/0`, `0.0.0.0/0`}, `IPAddressAllow` = {`127.0.0.0/8`,
`::1/128`}, `RestrictAddressFamilies` allow-list {AF_UNIX, AF_INET, AF_INET6},
`PrivateTmp`, `ProtectSystem=strict` (lists compared as exact multisets, every
mismatch named with PID 1's actual value). A drop-in, transient unit, pending
daemon-reload, other fragment or other argv refuses before any start. The
`Unit.Ref` pin and the outcome binding are the self-test's; for the probe the
exit status is trusted only when bound to THIS invocation
(`FixedUnitOutcome::exit_status_of_this_start`: new InvocationID, exited,
`ExecStart` record pid/status of this start, `inactive`/`success` for 0,
`failed`/`exit-code` otherwise) and mapped by `ssh_evidence::unit_outcome`:
0 ⇒ Denied, 10/13 (login/forwarding NOT denied, with sshd's Accepted record)
⇒ Reachable, anything else (other codes, timeout, refused start, PID 1 error)
⇒ Inconclusive with the stage for the log. A failed instance is reset.

Evidence (`ssh_evidence`). Results live in memory only, keyed by boot id,
committed signed generation, account name and uid, SHA-256 of
`sshd -T -C user=<account>,host=localhost,addr=127.0.0.1` (which `verify_login`
reads anyway) and the port taken from it, the installed probe binary verified
against the generation's `IsolationProbe` artifact (digest, size, mode), the
installed template unit's digest, and the live sshd listener's identity
(`ssh_evidence::ListenerIdentity`): unit `Id`, `MainPID`,
`ExecMainStartTimestampMonotonic` and the `ExecStart` argv (path must be
`/usr/sbin/sshd`) of the one active `ssh.service`/`sshd.service`, read from
PID 1 over the worker's D-Bus connection (spawnd cannot read
`/proc/<sshd>/exe`: sshd's capabilities are not a subset of spawnd's bounded
set, so the kernel's ptrace access check refuses). Any change (an sshd
restart, `-f`/`-o` in its command line), and every spawnd restart, re-probes.
A Denied result is current for at most `DENIED_TTL` (6 h) with the listener
identity in its key, `DENIED_TTL_NO_IDENTITY` (10 min) without; the worker
re-probes it in the background after 5/6 of that, so a healthy host shows no
gap, and an expired result is "pending". Reachable results do not expire in
the process. One worker
thread (`duduclaw-ssh-probe`, its own D-Bus connection, started by the first
lookup during start-up recovery) re-measures the host part every 2 s and runs
queued probes one at a time, each bounded by the unit deadline (40 s); the
250 ms refresh never starts a unit. `verify_login` keeps every earlier check
(linger, `user@` unit, sessions, `sshd -t`) and then, in this order, requires
its OWN `sshd -T` to deny the account (`ssh_effective_denied`'s shared parser)
and name a port (otherwise refused: `accounts::SSH_PORT_UNKNOWN`) before it
looks up the current key: Denied passes; Reachable and
Inconclusive refuse; no current result asks the worker and refuses as "probe pending"
(`io::ErrorKind::WouldBlock`, `accounts::SshRefusal::Pending`; the gateway sees
`NotReady`). An inconclusive result is retried after 60 s. Hosts without
`/usr/sbin/sshd` skip all of this as before.

Per-employee, not host-wide. `Registry::verify` in the readiness refresh and
start-up recovery walks every account, and before R7c one account's failing
SSH check failed it, which turned off `registry_verified`, `accounts_locked`,
`ssh_live_negative_checks`, `cron_at_acl_and_jobs_verified` and
`external_iam_reservation_verified` for the whole host (and a failed start-up
reconcile is never re-evaluated). Those walks now use
`LinuxAccounts::background`: host-level checks still refuse, but an account's
probe state only requests a probe. The probe result is enforced per employee
at admission (`Core::ready`, `LinuxAccounts::admission`), so after a restart
each employee becomes runnable as soon as its own probe reports Denied.

Allocation (`registry::Allocations`). Allocation never waits for a probe on
the lifecycle thread: a new account is reserved and created, its probe is
queued ahead of background requests (`AccountHost::login_denied`), and the
allocation answers "probe pending" at once (wire `NotReady`), leaving the row
`Reserved` and remembered as resumable. The next allocation of the same
employee asks again: Denied ⇒ `recover_locked` re-lock and no-process
readback, `verify_locked`, Ready; Reachable ⇒ Tombstone; still pending or
inconclusive ⇒ pending again (the request is re-queued if the worker dropped
it; an inconclusive result is re-run at most every 10 s,
`ssh_evidence::FORCED_RETRY_MS`). Every other failure as before ⇒ Tombstone.
At most `registry::MAX_PENDING_RESERVATIONS` (4) reservations may be pending at
once; a further new employee is refused with `registry::PendingLimit`
(resource limit, wire `NotReady`, nothing reserved) until one resolves. A
reservation still unresolved `registry::PENDING_RESERVATION_TTL_MS` (10 min)
after it was made (a resume does not extend it) is retired by the lifecycle
thread's maintenance like a failed allocation: Tombstone (saved durably before
it is forgotten), then `recover_locked`; at most 4 per turn, never waiting for
a probe. The employee keeps its single registry row (one row per employee is
unchanged) and is from then on a tombstoned employee like any other: its next
allocation is refused as every Tombstone is, and its slot under the cap is
free for other employees. A
pending reservation is not a host-wide verification failure. It is remembered
in memory only: after a restart `reconcile` tombstones every `Reserved` row as
before.

Unsupported hosts (refused, host prerequisites): passwd/group NSS modules other
than `files`/`systemd` (LDAP/SSSD, Google Compute Engine OS Login, `compat`,
bracketed action clauses; `accounts::nss_supported`, no IAM reservation
producer exists); sshd present without `/usr/bin/ssh`, `/usr/bin/ssh-keygen` or
`/usr/bin/journalctl`, or not listening on loopback, or not logging to the
journal; no `duduclaw-selftest` account or no `systemd-journal` group (the
probe unit's identity); sshd with `UsePAM no` (sshd then reports the expired account before
`DenyUsers`, so the probe cannot find the DenyUsers record and stays
inconclusive).

Account creation no longer passes `--key CREATE_MAIL_SPOOL=no` (shadow 4.13
refuses it); a mail spool at `/var/mail/<name>` or `/var/spool/mail/<name>`
after useradd refuses the account (rolled back, never deleted by spawnd).

### R7b: DNS route evidence

`dns_route_verified` no longer comes from an installer receipt (nothing
produced one, so it could never become true). Signed isolation policy v4
(domain `duduclaw-isolation-policy-v4`) is v3 plus the required
`dns_probe_name`: a hostname AI employees need anyway (for example the model
API host), chosen by the vendor at signing time. The one rule
(`duduclaw_isolation::egress::validate_dns_probe_name`, mirrored by
`scripts/sign-isolation-manifest.py`): ASCII lower case, 1–253 bytes, at least
two labels of 1–63 bytes of `[a-z0-9-]` not starting or ending with `-`, no
trailing dot, a last label that is not all digits, and neither `localhost` nor
a name under `.local`, `.internal` or `.localhost`. v1–v3 documents refuse the
key; `installed::signed_policy_dns_probe_name` reads it next to
`signed_policy_egress_profiles`.

`dns_evidence` runs a background worker (started, like the self-test, by the
first readiness refresh after start-up recovery; never a precondition of
spawnd opening, since the egress unit waits for spawnd's readiness gate). It
sends `EgressControl::ProbeDns { name }` over the existing, single egress
control connection (credential-checked like every egress frame; the worker
never connects, the lifecycle thread does) and passes only on
`DnsProbed { route, resolved: true, public_addresses >= 1 }` with `route` equal
to the one route of every profile in the root-owned `/etc/duduclaw/egress.json`
(read with the installed-document ownership checks; spawnd has no other source
of the configured route). A pass is fresh for 60 s and re-probed after 45 s; a
failure is retried after 5 s. No result yet, a stale result, a policy older
than v4 (nothing is sent), an unreadable configuration, no connection, a
`Refused` answer or a failed exchange all read as not verified. The refresh
path only loads an atomic snapshot. A probe holds the control connection for
one exchange (egress's 3 s deadline), so a run registration waits at most that
long behind it. `egress_verified` is unchanged; `egress_second_layer` has its
own readback (next section).

### R7b: egress second layer from PID 1

`egress_second_layer` (display-only: `Readiness::check` does not include it)
no longer comes from the receipt. `egress_layer` runs a background worker with
the DNS evidence's snapshot and cadence (fresh for 60 s, re-read after 45 s,
retried after 5 s; the refresh path only loads the snapshot) and its own D-Bus
connection to PID 1 (`Systemd::egress_second_layer`, read-only properties):

- `duduclaw-egress.service` must be loaded (`GetUnit`; never loaded by spawnd)
  from exactly `/etc/systemd/system/duduclaw-egress.service` with no drop-in (a
  drop-in could add `IPAddressAllow=any`), no pending daemon-reload, not
  transient (`systemd::fixed_unit_load_mismatch`, the self-test unit's rule
  generalised to a unit name), and `active`.
- Its `IPAddressDeny` and `IPAddressAllow` (`a(iayu)`: family 2/10, address
  bytes, prefix length) must each equal, as an exact multiset (PID 1 reports
  them in hash order), the sets parsed from the shipped unit text spawnd embeds
  (`systemd::EGRESS_UNIT_TEXT`, `crates/duduclaw-egress/systemd/duduclaw-egress.service`:
  22 deny and 9 allow prefixes). The parser (`systemd::unit_ip_rules`) is
  strict: every assignment in `[Service]`, literal `address/prefix` tokens only
  (no `any`, no bare address, no host bits beyond the prefix), no duplicates, no
  empty assignment; a parse failure is `unavailable`.
- When `/etc/duduclaw/egress.json` selects `dedicated_dns_broker`,
  `duduclaw-dns-broker.service` must also be loaded from its own file with no
  drop-in (it may be idle: it is socket-activated), `IPAddressDeny` exactly
  `::/0` and `0.0.0.0/0` (`any`), and `IPAddressAllow` non-empty with every
  entry a single unicast, non-loopback host (/32 or /128). Nothing in the
  protected installer writes that allow line today, so a broker host reads
  `unavailable`.

Anything else (not loaded, not active, a property unreadable, a mismatch) is
`unavailable`, never a readiness failure; the reason is logged when it changes,
not on every read. The egress unit starts only after spawnd's readiness gate,
so the value is `unavailable` until the first successful readback. This
verifies what PID 1 has configured for the unit, not that the cgroup BPF
filter is attached in the kernel (reading the attached programs needs a
privilege spawnd does not hold).

### R7b: file-owner inventory at UID allocation

The receipt's `unowned_uids`/`owner_inventory_digest` and
`InstallationEvidence::no_existing_file_owner` are gone. `Registry::reserve`
asks the account host once per allocation, before trying any candidate, for
the occupied ids (`AccountHost::occupied_ids` → `owner_inventory::scan_installed`);
the per-candidate `getent passwd|group` checks are unchanged, and so are the
tombstone and retire rules. The scan covers only spawnd's own state directories
that can hold employee-owned files (`owner_inventory::INVENTORY_ROOTS`):
`/var/lib/duduclaw-spawnd/{work-anchors,images,storage,projections,etc-state}`.
spawnd cannot see the host's `/home`, `/root` or `/tmp` by design, so files
there are outside it. Every uid and gid inside the employee range that owns an
entry (roots included) is collected; a candidate found as either is skipped
(an employee's gid is its uid). Each root is opened from `/` without symlinks
through root-owned, not group/other-writable components; entries are read
through directory descriptors and inspected with `fstatat(AT_SYMLINK_NOFOLLOW)`
(a symlink is recorded, never followed); subdirectories are entered only with
`openat2(RESOLVE_BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV)`, so a mounted
employee image or any other mount is recorded by the owner seen from its
parent and never entered. Bounds: 200,000 entries in total, 12 levels below a
root. Exceeding one, or any error other than a root that does not exist yet
(fresh install: empty), fails the inventory: the allocation is refused (and
logged) with nothing reserved or tombstoned.

```bash
# Disposable root VM:
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib systemd::linux::tests::iso_root_egress_second_layer_readback_matches_real_systemd -- --ignored --exact --test-threads=1
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib owner_inventory::tests::iso_root_uid_inventory_finds_foreign_owned_files_and_skips_mounts -- --ignored --exact --test-threads=1
```

## UserDB account verification

Account checks enumerate Varlink providers directly; `userdbctl` is not required.
Every provider also receives named `GetUserRecord`/`GetGroupRecord` lookups
(`userName`/`groupName`) and numeric lookups (`uid`/`gid`) for the account being
checked or created, all registry employees when that list is available, and local
employee user/group entries. Each request includes the socket's `service`.
This catches providers that synthesize reserved aliases only on named lookup,
as permitted by the [user-record specification](https://github.com/systemd/systemd/blob/main/docs/USER_RECORD.md).
Only `io.systemd.UserDatabase.NoRecordFound` means no hit. Every targeted hit
must be an exact local NSS echo matching the queried name or ID; non-NSS hits,
other errors, timeouts and malformed replies are refused using the same strict
canonical parser and provider validation as enumeration.
Each record retains its socket provider and complete JSON until validation, with
duplicate JSON keys refused. Before merging identity pairs, any non-NSS provider
(including PID1's DynamicUser) touching a reserved UID/GID, a `duduclaw-emp-*`
name or alias, or a local employee account/group name is refused; unrelated
non-NSS identities remain allowed. Employee membership mappings are also protected.
`aliases` must be an array of strings wherever encountered, including top-level,
`status.*`, `perMachine[*]` and `binding.*` objects in user and group records.
The user-record specification defines aliases at top-level and in `status.*`;
the other locations and group records are checked conservatively.
NSS echoes touching this namespace must match local `/etc/passwd`, `/etc/group` and
`/etc/shadow` identity, home, shell, account expiry, password state and the
`members`/`memberOf` group mappings in these records. The separate `GetMemberships`
API is not queried. Employee accounts/groups have no aliases, so an NSS echo
carrying an alias field (even an empty array) is refused. Relevant incomplete
records and unsupported security attributes fail closed.
Recovery compares the current local state so a matching half-created account can
still be locked down; the normal admission checks continue to require lockdown.

An absent `/run/systemd/userdb` is allowed. An existing directory requires at
least one provider to complete both user and group enumeration and all targeted
lookups, including explicit
`NoRecordFound` replies. A DynamicUser-only installation satisfies this condition;
an empty or Multiplexer-only directory is refused. There is no Multiplexer
fallback. The 5-second total deadline, 8 MiB/65,536-record budgets, 256-entry limit,
fixed alias restrictions, root-owned socket and root peer checks remain enforced.
Enumeration and all targeted lookups across providers share those total budgets;
DynamicUser may answer non-dynamic employee lookups with `NoRecordFound`.
The NSS shadow mapping follows systemd's
[user-record-nss.c](https://github.com/systemd/systemd/blob/v252/src/shared/user-record-nss.c);
`service` can appear under `status[machine-id]` in
[v249](https://github.com/systemd/systemd/blob/v249/src/userdb/userwork.c),
[v252](https://github.com/systemd/systemd/blob/v252/src/userdb/userwork.c) and
[v255](https://github.com/systemd/systemd/blob/v255/src/userdb/userwork.c), so it is
checked as metadata, never used to override the queried provider.

These review fixes have portable regression tests. The earlier root-account
acceptance on Ubuntu 22.04/24.04 and Debian 12 must be repeated with this patch;
compilation and fixture tests do not establish live-provider acceptance.

### I1 round 3: installed publication consumers

`Components::installed` now consumes root publication through `installed.rs`.
F must have a verified committed generation containing an immutable mode-0444
`RuntimeEntry { registry_id: "isolation-policy-v1" }` artifact. Its strict JSON
contains `version: 1`, the bounded runtime table and architecture-specific native
profiles (versions 2–4 add their signed `domain`; v3 adds per-record
`turn_limit`; v4, domain `duduclaw-isolation-policy-v4`, adds the required
`dns_probe_name` — see "R7b: DNS route evidence" below). The root-only `/var/lib/duduclaw-spawnd/publication.json` contains
`version`, the same `generation` digest and administrator-confirmed employee
entitlements. Paths are fixed, not supplied by a gateway request. This is a new
internal root publication schema; the shared IPC/capability contract is unchanged.

The verified generation directory is the runtime anchor. The installer must also
create root-owned `etc-state` (0700), `projections` and `snapshots` directories
under `/var/lib/duduclaw-spawnd`. Runtime entries must name the exact signed
artifact filenames visible in the generation. Publication emits a root-owned
0640 gateway projection at `/run/duduclaw-spawnd/publication.json`, bounded to
64 KiB and one second of monotonic validity. `GatewayPublication::read(gid)` checks
ownership/mode, boot ID, freshness and root-policy validity. No root private
state or executable path is disclosed through the projection.

spawnd reads no installer receipt (R7c removed the last reader of
`/run/duduclaw-isolation-evidence/installation.json`): the trampoline facts come
from spawnd's own self-test (R7a), the DNS route from its probe through egress
(R7b), file owners from its own scan at allocation (R7b), SSH denial from the
probe unit through PID 1 (R7c, above), and hosts with external NSS modules
are refused (R7c). The installer no longer refuses at S6 for a missing receipt.

R7d, start-up reconciliation retry (`host.rs`, `StartupReconcile`): the
reconciliation needs F's committed generation (the publication anchors), and F
answers only after its recovery reads the live gateway, egress and spawnd units
back; those start after spawnd is open. The first attempt runs in
`LinuxHost::open`. While it has never succeeded in this process and the core is
not poisoned, `Core::refresh` attempts it again at most every 2 s; the first
success latches and is never repeated, and a poisoned core is never recovered
by a retry. A retry asks F first (the anchors, read-only) and returns before
any side effect while F does not answer. Repeating the reconciliation is safe:
no run exists while `startup_reconciled` is false (`Core::ready` →
`Readiness::check`), and `Allocate` is answered `NotReady` until the latch,
because `RegistryStore::reconcile` tombstones every `Reserved` row.

Root caches only a fully verified immutable F generation and its
manifest. Every consumption still makes an authenticated nonce probe and checks
generation, product, exact binary version and all eight protocols; failed probes
clear the cache, and generation changes trigger full artifact verification again.
Freshness starts before the probe. A slow cold verification may warm the cache
but cannot publish readiness until a later fresh probe succeeds. Per-run runtime,
bridge and forwarder fd hashing is unchanged. Each `InstalledPublication`
instance pins the first verified generation for its entire daemon lifetime.
Load, mount anchors, MCP preparation/readback and native-profile selection all
refuse a different generation; F activation must restart B1. They compare the
complete supplied runtime against the signed record (including executable,
arguments, resource/time limits, network and MCP/PTY flags), and MCP preparation
and readback rebuild and compare the bootstrap plan before using its descriptors.
Native-profile selection revalidates the signed launch-policy acceptance. Thus a
new native policy cannot be mixed into an older plan between preparation stages.
The gateway publication is limited
to 64 KiB after encoding; larger runtime/entitlement sets refuse rather than
truncate, even when individual table bounds would permit them.

### R8d: who writes the host entitlements

The installer writes `/var/lib/duduclaw-spawnd/publication.json` once, with
an empty entitlement list. After that only root writes it, on the allocation
worker, through the existing atomic root writer (`host_state::atomic_json`,
root:root 0600: temp file, fsync, rename, directory fsync); `entitlement.rs`:

- `Entitle` (allocation socket, request/response schema 4 only; schemas 1–3
  cannot carry it): `{ item, entitlement: { profiles, max_turns? } }` for the
  item's employee. The allocation entry must be `Ready` (not disabled), else
  `not_allocated`. The whole candidate publication (this employee's one record
  replaced) passes the production check (`RootPolicy::from_verified_publication`
  against the committed generation's signed runtimes) and its gateway
  projection, sized with the longest boot id and timestamp, fits 64 KiB, before
  anything is written. Refusals write nothing: `not_signed` (a profile outside
  the signed policy), `invalid_entitlement` (empty, over 128 or duplicated
  profiles; `max_turns` outside 1..=1000 or below a signed step minimum),
  `capacity` (over 30,000 records or the 64 KiB projection). An identical
  record is answered without a write. The reply carries the stored receipt
  and the entitlement now in force, which the gateway compares with what it
  asked for. A write whose outcome on disk is unknown poisons the allocator
  (the journal's rule); one that provably left the old file in force does not.
- `Disable` and `Retire` remove the employee's record first, before their
  state change (absent ⇒ no write), so a crash between the two steps resumes
  cleanly.
- Generation rollover, in `InstalledPublication::load` only (root's lifecycle
  thread; the gateway-side reader never migrates): a version-1 publication of
  another generation keeps, per record, only the profiles the new signed
  policy still has, drops records left empty, drops a `max_turns` that no
  longer narrows every signed step range validly, is stamped with the new
  generation and written atomically, then validated as usual. Before that
  write it must pass the same check as `Entitle` (production validation, 64
  KiB projection): when the new policy leaves less room, trailing records
  (file order) are dropped one at a time until it fits — those employees are
  not entitled (their runs are refused) until re-entitled, so an update that
  would do this should be avoided by keeping the signed runtime list small —
  and any other failure, or an empty list that still does not fit, refuses
  with nothing written. A failed write
  refuses the publication (fail closed). Unreadable content, another version
  or unknown fields are refused as before, never migrated. The gateway's
  start-up reconcile re-sends every live employee's entitlement afterwards.

Trust statement: the gateway account is the authority for entitlements
exactly as it is for account allocation; root enforces signed-policy
membership, Ready allocation, capacity.

Capacity: the projection is `299 + R + 222·n` bytes in the worst case for `n`
one-profile records (envelope 300 bytes including the longest boot id,
timestamp and generation; 221 bytes per record plus a separator; `R` = the
serialized signed runtimes), so `n ≤ ⌊(65 237 − R) / 222⌋` — 293 with no
runtimes, fewer with a real signed policy. Each further profile adds up to
130 bytes to its record.

Linux builder checks and focused tests:

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --all-targets
cargo test -p duduclaw-spawnd installed::tests -- --test-threads=1
# Disposable root VM; receipt transport only, not employee/runtime acceptance:
cargo test -p duduclaw-spawnd iso_root_e2e_publication_receipt_mode_and_symlink_refusal -- --ignored --test-threads=1
```

For an MCP-enabled installed runtime, the signed runtime record also requires
`mcp_acceptance`. It binds exact runtime/version, runtime and bridge hashes, D's
projection digest, fd 3, successful real-runtime initialization/parent exec/MCP
child exec/exclusive-config tests and a transcript digest. `launch_policy_digest`
is SHA-256 of the JSON object with domain
`duduclaw-runtime-mcp-acceptance-launch-v1`, `runtime` (the full `RuntimeRecord`
with `mcp_acceptance: null`) and `native` (the exact native profile). Changing
arguments, limits, sandbox profile or forwarder invalidates acceptance.

Before preparing detached mounts, the installed publication atomically writes
D's exact configuration bytes as root-owned mode-0444 files in a 0755 employee
projection. After mount preparation it verifies the exact file set and hashes,
read-only flags, mount ID, fixed MCP argument suffix and bridge ELF hash from the
actual descriptors delivered to B2. Only flat `RunConfig` destinations are
supported here; RuntimeHome/WorkingDirectory projections still refuse.
A root-owned 0640 `/run/duduclaw-spawnd/mcp-<runhex>.json` receipt binds those
observations to generation, employee and run. `McpRunReceipt::read` enforces boot,
run, mode and a 60-second consumption deadline; cleanup removes the receipt.
Fixture publications retain their explicit independent launch contract through
the default trait methods and are not proof of installed MCP acceptance.

### Installed publication root acceptance (not a Claude turn test)

The three `installed::root_acceptance::iso_root_e2e_installed_publication_*`
tests consume real installed F/B1 inputs. They create no acceptance metadata,
accounts, signed manifests or run receipts. Each requires root and
`DUDUCLAW_ISO_DISPOSABLE=1`; a missing daemon, signed artifact, confirmed employee,
fresh measurement or runtime acceptance causes a test failure. They verify only
the root publication and receipt-consumption layer. They do not prove
A→B1→systemd→B2 execution, a real Claude model turn, a D tool result, D8 queueing,
or fresh login attacks.

The VM must have the compiled-version F service running with a committed signed
generation; B1 must continuously publish current root/gateway reports, with at
least one confirmed MCP-enabled Claude entitlement. The signed runtime policy
must contain real exact-runtime MCP inheritance acceptance. The second test
starts the installed probe instance for every entitled account through PID 1
(R7c: the signed probe binary, the template unit, files/systemd NSS only) and
the installed `duduclaw-isolation-selftest.service` through
PID 1 (R7a), which needs the unit file and the `duduclaw-selftest` account, and
(R7b) requires a policy v4 `dns_probe_name` and one DNS route in
`/etc/duduclaw/egress.json` (the probe itself needs spawnd's own control
connection and is covered by egress's `iso_root_egress_dns_probe_through_control_socket`). No test
synthesizes those prerequisites. For the third test, an externally started real
Claude run must remain journaled and its MCP receipt must be less than 60 seconds
old. Set `ISO_INSTALLED_RUN_ID` to that actual 32-character lowercase hex run ID.

Build on the Linux builder:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib --no-run
```

Run each test separately on the disposable installed root VM (commands below are
intended for a root shell; they have not been executed by this development pass):

```sh
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_signed_policy_and_gateway_consumer -- --ignored --exact --test-threads=1 --nocapture
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_live_install_evidence -- --ignored --exact --test-threads=1 --nocapture
# Export ISO_INSTALLED_RUN_ID from the actual newly started run before this command.
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::root_acceptance::iso_root_e2e_installed_publication_actual_run_mcp_receipt -- --ignored --exact --test-threads=1 --nocapture
```

The acceptance harness can copy the emitted library test executable using its
existing `exes.txt` / `push.sh` convention. Its `runall.sh` filter
`iso_root_e2e_installed_publication_` selects exactly these three tests; the root
shell must also export the real `ISO_INSTALLED_RUN_ID` for the third test. Verify
that each result says `1 passed`, rather than treating zero selected tests as a
pass. The separate `iso_root_e2e_publication_receipt_mode_and_symlink_refusal`
fixture tests receipt filesystem rejection only and does not replace these tests.

### Real installed account denials (separate from Claude/D8 acceptance)

`installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials`
executes actual localhost SSH public-key login and remote-forwarding attempts,
`su`, a user-manager transient unit, crontab installation and `at` scheduling.
Every family must have a successful positive control. Missing binaries, missing
journals, unreachable services, timeouts, a failed UID transition or unrecognized
errors fail the test. It does not create accounts, keys, SSH policies, signed
runtime acceptance or installed authority records.

Use a disposable, already installed VM with an actual confirmed employee in the
root publication/UID registry. Keep its expired/locked password, `nologin`,
`/nonexistent`, no-linger and scheduler denial policies intact. The dedicated
control account must be an ordinary local login user (UID 1000–59999) with a
working user manager and permission to use cron, at and SSH remote forwarding.
Its crontab must contain exactly this single harmless line, including newline:
`# duduclaw isolation acceptance control`. The employee must have no crontab.
Both `cron.service` and `atd.service` must be active. `/usr/bin/setpriv` starts
scheduler/user-manager attempts under the exact UID/GID with supplementary groups
cleared, bypassing PAM so expiration cannot hide which tool was actually tested;
a UID marker proves execution. `su` separately tests the real PAM/login route.

The SSH fixture is deliberately explicit:

- The actual SSH daemon listens at `127.0.0.1:22`; this test never changes it.
- `/run/duduclaw-iso-account-test` and its `authorized_keys` directory are
  root-owned and traversable by sshd. Effective `AuthorizedKeysFile` must be
  exactly `/run/duduclaw-iso-account-test/authorized_keys/%u` for both users.
  Pre-provision one identical dedicated test public key in each root-owned
  `<username>` file. Thus absence of an employee key cannot explain a pass.
- Set `ISO_ACCOUNT_SSH_KEY` to that key's root-private, unencrypted test private
  key file, and `ISO_ACCOUNT_SSH_KNOWN_HOSTS` to a root-owned known-hosts file
  containing the independently verified localhost host key. No `ssh-keyscan`
  trust-on-first-use bypass or interactive credential prompt is used.
- Root must be able to read the current-boot sshd journal. Each negative attempt
  uses a fresh loopback source port through `/usr/bin/nc`; acceptance requires
  both the client authentication refusal and root-owned journal messages from
  the same sshd PID naming that employee/source port and explicitly rejecting it
  because of `DenyUsers`. Offline `sshd -T` plus exit 255 alone is insufficient.
  An sshd configuration/logging variant that cannot supply those precise records
  fails the test instead of being treated as a denial.

Required tools are `timeout`, OpenSSH client/server/keygen, `nc`, `journalctl`,
`su`, `setpriv`, `systemd-run`, `systemctl`, `crontab`, `at`, `atq` and `atrm` at
the fixed system paths in the test. Every command has a 12-second TERM deadline
and two-second KILL grace. SSH masters close even after unexpected success;
crontab uses only a comment-only table and removes an unexpectedly accepted
employee table; `at` schedules only `/usr/bin/true` without mail, verifies the new
job's owner and immediately removes that job before asserting the result.
The test does not remove existing employee/control jobs. If the active UID
monitor interrupts an attempt before its exact evidence is observed, the test
fails; it never disables that monitor or counts an arbitrary kill as success.

On the Linux builder:

```sh
CARGO_BUILD_JOBS=12 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --lib --no-run
cargo test -p duduclaw-spawnd --lib installed::account_root_acceptance::ssh_policy_evidence_requires_same_root_session_and_source_port -- --exact
```

On the disposable installed VM, from a root shell after preparing the fixture:

```sh
export ISO_ACCOUNT_CONTROL_USER=iso-control
export ISO_ACCOUNT_SSH_KEY=/run/duduclaw-iso-account-test/client_key
export ISO_ACCOUNT_SSH_KNOWN_HOSTS=/run/duduclaw-iso-account-test/known_hosts
DUDUCLAW_ISO_DISPOSABLE=1 cargo test -p duduclaw-spawnd --lib installed::account_root_acceptance::iso_root_e2e_installed_account_real_denials -- --ignored --exact --test-threads=1 --nocapture
```

For the existing copied-binary acceptance harness, export the same three fixture
variables in its root shell and use `runall.sh <outdir>
'iso_root_e2e_installed_account_real_denials'`. This command must select one test
and report `1 passed`; it is not evidence of a Claude turn, MCP tool call or D8
admission. This development pass type-checked the test but did not execute these
root attacks or provision the fixture.

## Isolation policy generator (R7d, `duduclaw-isolation-policy`)

Every generation must carry the signed isolation policy (manifest artifact
`isolation-policy-v1`, `runtime_entry`, generation file
`runtime-isolation-policy-v1`, mode 0444, at most 1 MiB); the installer's S5 and
spawnd refuse a generation without a valid one. `duduclaw-isolation-policy`
produces that document from a small reviewed source file and checks it with
spawnd's own code (`src/installed_policy_tool.rs` is a child module of
`installed`, so it calls the production readers and checks, never copies).

It is an offline vendor/test tool behind the non-default feature `policy-tool`;
`scripts/package-linux-release.py` refuses the feature and the binary name in a
release build (`refuse_test_signer_build`, `check_release_binary_set`). It is
**Linux only** (spawnd's policy readers are): on other hosts the binary only
prints that. Any Linux architecture can build for either one (`--arch`).

```sh
cargo build --locked --release -p duduclaw-spawnd --features policy-tool --bin duduclaw-isolation-policy
duduclaw-isolation-policy build  --source <policy-source.json> --artifacts-dir <dir> \
    --arch <x86_64|aarch64> --out <new policy.json> [--manifest <manifest.json>]
duduclaw-isolation-policy verify --policy <policy.json> --artifacts-dir <dir> \
    [--manifest <signed manifest.json>] [--arch <x86_64|aarch64>]
```

`--artifacts-dir` holds the real files under their installed generation names
(`duduclaw_updated::publication::artifact_name`): `runtime-<registry_id>` for
runtime entries (`runtime-api-worker-v1` = the built `duduclaw-api-worker`,
`runtime-native-adapter-v1` = `duduclaw-runtime-adapter`, `runtime-vendor-codex`,
`runtime-vendor-grok`), `duduclaw-egress-forwarder` (= the `duduclaw-egress` ELF)
and `duduclaw-mcp-bridge`; other files are ignored, symbolic links refused.
`scripts/package-linux-release.py`'s `stage_policy_inputs` writes such a directory
from a binary directory and a template. Without `--manifest` the cross-check
runs against a manifest synthesised from those files (runtime ELF entries 0755,
as the packager signs them); with `--manifest` against that manifest, and every
file of `--artifacts-dir` it lists must have the same sha256 and size. `verify`
with `--manifest` also requires the manifest's `isolation-policy-v1` entry
(0444, ≤ 1 MiB) to be exactly the `--policy` bytes; without `--manifest`, a
`runtime-isolation-policy-v1` in `--artifacts-dir` must be. `build` writes the
policy (compact JSON, mode 0444, refusing an existing `--out`) only after every
check passed; both commands exit 1 on a refusal (the message names the rule)
and 2 on a usage error; `verify` prints a short summary (runtimes, kinds,
entries, digest prefixes, network, MCP, MCP acceptance present or not).

### Source format (`schema: "duduclaw-isolation-policy-source-v1"`)

JSON, unknown fields refused everywhere. Top level:

| Field | Meaning |
| --- | --- |
| `schema` | exactly `duduclaw-isolation-policy-source-v1` |
| `comment` | optional free text, never copied into the policy |
| `dns_probe_name` | policy v4 probe name (`egress::validate_dns_probe_name`) |
| `forwarder_seccomp` | `{"x86_64": <file>, "aarch64": <file>}` syscall allow-lists of the egress forwarder; required exactly when a runtime uses egress (only the `--arch` one must exist) |
| `runtimes` | 1–128 runtime entries |

Each runtime:

| Field | Meaning |
| --- | --- |
| `label` | 1–64 of `[a-z0-9-]`, unique; the profile id is derived from it |
| `comment` | optional free text |
| `runtime` | runtime kind as in the policy: `{"kind": "open_ai_compat"}`, `claude`, `codex`, `grok`, `{"kind": "registered", "name": …}` |
| `executable` | generation file name of its ELF in `--artifacts-dir`, e.g. `runtime-api-worker-v1` |
| `pinned_version` | signed version string (not empty, not `latest`/`*`) |
| `fixed_args` | `"default"` = exactly what the gateway requires for the kind (`open_ai_compat`: `--duduclaw-protocol openai-compat-v1 --mcp-config /config/mcp.json`; Codex/Grok: the native adapter protocol; Claude: `gateway_plan::claude_arguments`, ranged when `turn_limit` is set; `generic-cli-v1.<id>`); requires `mcp: true`. Otherwise an explicit list |
| `model` | Claude with `"default"` only: the `--model` value |
| `resources`, `timeouts` | exactly the policy's `resources`/`timeouts` objects |
| `network` | `{"kind": "offline"}` or `{"kind": "egress", "profile": "<egress label>"}` (egress labels must not equal a runtime label) |
| `mcp`, `pty_allowed`, `queue_class`, `turn_limit` | as in the policy (`turn_limit` optional, Claude MCP only) |
| `native` | `seccomp` (`{"x86_64": <file>, "aarch64": <file>}`), `tmpfs_bytes`, `tmpfs_inodes`, `proc_subset_pid`, optional `launcher` (a `LauncherRecord`, copied verbatim; helper `profile`s are derived ids, printed by `build`) |
| `mcp_acceptance` | optional, copied verbatim from an acceptance record. Without it the policy loads, but spawnd refuses every MCP run of that runtime at MCP preparation (`prepare_mcp_projection`) |

Syscall allow-list files (paths relative to the source file): one **decimal
syscall number** or an inclusive range `N-M` per line, `#` comments, blank lines
ignored; syscall names are not accepted. At most 512 unique numbers.

### What the tool computes

- Executable, vendor (`runtime-vendor-codex`/`-grok`, Codex/Grok only), forwarder
  and bridge digests: sha256 of the real files' bytes.
- Runtime and forwarder seccomp digests: `RuntimeProfile::digest(arch)`
  (`crates/duduclaw-run/src/seccomp.rs`, `"duduclaw-runtime-seccomp-v1\0"` +
  audit arch + count + sorted numbers), the digest `seccomp::runtime` checks.
- Profile ids (runtime and egress): nothing in the product defines how they are
  chosen, so the tool derives `sha256("duduclaw-isolation-profile-v1\0" ++ label)`
  (`installed_policy_tool::profile_id`).
- Forwarder record: entry `duduclaw-egress-forwarder`, argv
  `["/runtime/duduclaw-egress-forwarder", "forwarder", "18080"]`; 18080 is
  `policy::RUNTIME_PROXY_PORT`, the proxy port spawnd plans for every egress run
  (`HTTP_PROXY=http://127.0.0.1:18080`, `host.rs`).
- The document: policy v4 (`duduclaw-isolation-policy-v4`).

### Checks run on the output (all production code)

`installed::parse_policy` and `signed_policy` (installer reader); per runtime
`systemd::UnitPolicy::new`, `policy::validate_turn_limit`,
`RootPolicy::from_verified_publication`; `validate_runtime_artifacts`;
`seccomp::runtime` per native record; `validate_launchers`; the complete `bundle`
sequence (`verified_policy`); `load`'s `GatewayPublication::policy` +
`with_launchers`; `RootPolicy::derive` per runtime (and with a PTY when
`pty_allowed`, i.e. `LaunchPolicy::validate`); the installer S5 readers
`signed_policy_egress_profiles` (at least one egress profile) and
`signed_policy_dns_probe_name`; duduclaw-run `RootConfig::validate`; the
forwarder rules of `native_profile` (`forwarder_artifact_matches`) and the
trampoline (`forwarder_argv0`, forwarder present exactly for egress), the egress
`forwarder <port> [limits]` argv shape and the runner's `seccomp::runtime` for the
forwarder; with `mcp_acceptance`, `McpAcceptance::validate_launch` and
`validate` against the manifest's MCP bridge.

Not covered: producing MCP acceptance, reviewed production syscall lists,
vendor (Codex/Grok) runtimes in the TEST path, Script executables (the tool
emits ELF entries only). The TEST bundle path (`scripts/isolation-test-keys.sh
bundle`) and its fixtures are described in
`scripts/live-test/linux-protected/README.md`.

## License

Apache-2.0, as declared by the workspace.
