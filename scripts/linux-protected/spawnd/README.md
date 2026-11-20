# Employee launcher installation support

These units stage the B1 launcher for a verified native Linux release. They do
not enable employee isolation by themselves. The migration/update installer must
first verify the complete signed release and publish `duduclaw-spawnd` and
`duduclaw-run` under `/usr/lib/duduclaw`, owned by root and not writable by any
other account.

The protected CLI installer installs these unit templates only when both fixed
binaries are already present in that root-managed directory. It creates the
`duduclaw-runsup` system account, state/log directories, and reloads systemd.
It deliberately does not start or enable the socket: the complete release's
migration/readiness gate owns activation. Existing installations without the new
binaries retain their existing installer behavior.

Before activation, packages A/B2/C/H/F/G must complete gateway integration,
trampoline bootstrap, egress registration, quota admission, signed generation
verification and migration confirmation. Host-specific IO limits and employee
slice limits are required at launch. The aggregate slice defaults are ceilings;
the installer must reserve adequate memory for the trusted-services slice and
its ancestors. Gateway and updater must also join that slice in their packages.

The socket is a root-owned `SOCK_SEQPACKET` endpoint, group-accessible only to the
gateway account. The daemon independently checks peer credentials. The service
does not write employee stdout/stderr to the journal. Normal account provisioning
happens before activation under G's approved operation. The service permits `/etc`
and the existing linger directory to be written only for root account quarantine
after interrupted provisioning; this expands the first-round read-only unit policy.
The registry singleton lock excludes concurrent provisioning.

The binary now uses the Linux production host with fail-closed missing adapters.
It writes `/run/duduclaw-spawnd/readiness.json` atomically as root:gateway-group
0640; `--check` validates ownership, boot identity and the one-second lifetime.
A running service alone is not a successful readiness check. BindsTo on each run
unit provides PID 1 shutdown cleanup, followed by startup residue reconciliation.

Run units (transient, `duduclaw-run-<run>.service`) deliberately set neither
`User=`/`Group=` nor `RestrictSUIDSGID=` (R7g): on systemd 255 an explicit
`User=root` loses `CAP_SETUID`, and `RestrictSUIDSGID=` makes `openat2` ENOSYS
for the trampoline. spawnd reads both back as unset and checks the started
process is root with exactly the trampoline capability set before it sends the
bootstrap; see `crates/duduclaw-spawnd/src/systemd.rs` (`UnitPolicy::new`) and
`docs/architecture/native-linux-isolation.md`. No static unit file mirrors the
run-unit policy, but the same `openat2` conflict applies to the static units:
`duduclaw-spawnd.service` and `duduclaw-isolation-ready.service` do not set
`RestrictSUIDSGID=` either, because spawnd (work broker, storage, cgroup
leaves, bootstrap mounts, trusted-file reader) and its `wait-consistent` waiter
(status file through the trusted-file reader) open paths with `openat2` and
RESOLVE_*. Both stay root services that create no set-ID files, with
`NoNewPrivileges=yes`. They keep `User=root`/`Group=root`: neither needs
`CAP_SETUID` (spawnd's bounding set excludes it; the waiter has none).
`duduclaw-updated.service` (scripts/linux-protected/updated) and
`duduclaw-auto-upgrade.service` follow the same rule and also drop the explicit
`User=root`, since the updater binary needs `CAP_SETUID` to drop its download
child to `duduclaw-dl`.

The daemon also binds the root-only update control socket
`/run/duduclaw-spawnd/update.sock` (root 0600) for `duduclaw-updated`. Its
`Apply` republishes the verified generation's root components, so the unit adds
`ReadWritePaths=-/usr/lib/duduclaw -/usr/libexec/duduclaw -/opt/duduclaw/bin`:
atomic replacement creates `<name>.next` inside each directory and renames it,
which needs directory write access, broader than the nine fixed files. Narrowing
it needs a separate narrow publisher helper (known residual, plan R6).
The drain barrier is kept in `/var/lib/duduclaw-spawnd/drain.json` across restarts.

The unit is `Type=notify` (`NotifyAccess=main`, `TimeoutStartSec=300`,
`TimeoutStopSec=90`; `duduclaw-updated` waits for its restarts against one
deadline derived from both). Every start re-verifies the installed set
(publication journal, or the generation `current` names) before binding
anything; `READY=1` then only means that `update.sock` is bound, and it is sent
whether or not the set is consistent, so `duduclaw-updated.service` (`Wants=` +
`After=duduclaw-spawnd.service` only) can always repair the host. On an
inconsistent set spawnd opens nothing else; a repairing `Apply` makes it exit
with status 82, which the unit restarts (`RestartForceExitStatus=82`,
`SuccessExitStatus=82`). A fully open spawnd whose set a request leaves
inconsistent (an `Apply` that failed after its journal said `publishing`)
closes admission, replies, writes its status and audit record, and exits with
status 83: a failure, restarted by `Restart=on-failure`; the new process's
start-up recovery completes or rolls back the publication. Its stderr goes to
the journal (fixed-vocabulary lines), and `systemctl status` shows the
`STATUS=` line.

Start rate limit: `StartLimitIntervalSec=300`/`StartLimitBurst=5` counts every
automatic restart (crashes, exit 83, and the exit-82 restart after a repair),
so crash loops stay limited. `duduclaw-updated` restarts spawnd after every
`Apply` (including every repairing one) and runs `systemctl reset-failed
duduclaw-spawnd.service` first, so a series of legitimate repairs does not
exhaust the budget. If the limit is hit anyway (`systemctl status` shows
`start-limit-hit`; the update endpoint is gone), an operator investigates the
journal, then runs `systemctl reset-failed duduclaw-spawnd.service` and
`systemctl start duduclaw-spawnd.service`.

`duduclaw-isolation-ready.service` (oneshot, `RemainAfterExit=yes`) runs
`duduclaw-spawnd wait-consistent` and completes only when the live spawnd
reports the verified, opened, consistent set in
`/run/duduclaw-spawnd/update-status.json` (owner = this boot, spawnd's current
`MainPID` and `InvocationID` as PID 1 reports them over the system bus; no
`/proc/<pid>` access, no capability). On a host that is not on the unified
cgroup v2 hierarchy it exits non-zero at once with a fixed message. The gate
lives exactly as long as one spawnd process:

| Unit | Directive | Target |
|---|---|---|
| `duduclaw-isolation-ready.service` | `BindsTo=` | `duduclaw-spawnd.service` |
| `duduclaw-isolation-ready.service` | `PartOf=` | `duduclaw-spawnd.service` |
| `duduclaw-isolation-ready.service` | `After=` | `duduclaw-spawnd.service` |
| `duduclaw-spawnd.service` | `Wants=` | `duduclaw-isolation-ready.service` |
| `duduclaw-egress.service` | `BindsTo=`, `After=` | `duduclaw-isolation-ready.service` |
| isolated gateway unit | `BindsTo=`, `After=` | `duduclaw-isolation-ready.service` |
| `duduclaw-updated.service` | `Wants=`, `After=` | `duduclaw-spawnd.service` (only) |

Whenever spawnd stops, fails or restarts, the gate stops and egress and the
gateway stop with it; every start of spawnd (manual or `Restart=`) starts the
gate again (`Wants=` is a `[Unit]` dependency: nothing has to be enabled), and
it completes only for the new process. After an explicit restart of spawnd,
egress and the gateway are restarted with the gate; after a crash or exit
82/83 they stay stopped until started again (egress by its socket, the gateway
by the updater or an operator), and then wait for the re-armed gate. The
updater is not bound to spawnd, so spawnd restarts never stop it.

Before every `Apply` the updater stops the gate (and with it egress and the
gateway), then `duduclaw-egress.socket`, egress and the gateway explicitly,
and requires PID 1 to report all four (`duduclaw_isolation::ids::ISOLATION_GATED_UNITS`)
`inactive`/`failed` with no pending job; otherwise it aborts before `Apply`.
spawnd refuses `Apply` unless its own system-bus readback of the same units
agrees, and an open spawnd that accepts an `Apply` reports `recovering` (status
file written before the publication journal) until it is restarted, so the
stopped gate is re-armed only by the restarted, re-verified spawnd. The
isolated-install gateway unit (written by the installer package, R3) must carry
`duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES` (the two
`BindsTo=`/`After=` lines). The single-binary gateway unit
(`duduclaw_core::root_key_recovery_unit`) does not (no spawnd on those hosts).

`duduclaw-isolation-selftest.service` (R7a; oneshot, `RemainAfterExit=no`, no
`[Install]`) runs `/usr/lib/duduclaw/duduclaw-run-selftest` as root in
`duduclaw-tcb.slice` with exactly the trampoline's `CapabilityBoundingSet=` and
`RestrictNamespaces=` (pinned against `TRAMPOLINE_CAPABILITIES` /
`TRAMPOLINE_NAMESPACES` by spawnd's unit-file test), `TimeoutStartSec=30` and
stdout/stderr to the journal. Root is the default identity: like run units it
sets neither `User=`/`Group=` nor `RestrictSUIDSGID=` (R7g; an explicit
`User=root` loses `CAP_SETUID`, `RestrictSUIDSGID=` makes `openat2` ENOSYS), and
spawnd reads back `User=""`, `Group=""`, `RestrictSUIDSGID=no` before starting
it. Every other sandbox directive it shares with run units has
`UnitPolicy::new`'s value; the deliberate differences are listed in the unit
file. Only spawnd starts it (by name, through PID 1),
after its start-up recovery, and only while PID 1 loads it from
`/etc/systemd/system/` without drop-ins. Its name is not of the run-unit shape,
so no run inventory counts it.
The protected installer installs this unit at `/etc/systemd/system/` (root:root 0644, exactly these bytes) before spawnd starts, and never enables or starts it.
The protected installer creates the `duduclaw-selftest` system account (no login, no home, own group, no supplementary groups) before spawnd starts.

`duduclaw-isolation-probe@.service` (R7c; template, oneshot,
`RemainAfterExit=no`, no `[Install]`) runs
`/usr/lib/duduclaw/duduclaw-isolation-probe %i` for one employee account and
sshd port (`duduclaw-isolation-probe@duduclaw-emp-<uid>.<port>.service`;
spawnd takes the port from its own `sshd -T` after that output already denies
the account) UNPRIVILEGED as the isolation test account: `User=` and
`Group=duduclaw-selftest`, `SupplementaryGroups=systemd-journal` (read access
to sshd's journal records), in `duduclaw-tcb.slice`, with an empty
`CapabilityBoundingSet=`, `IPAddressDeny=any` + `IPAddressAllow=localhost`,
`TimeoutStartSec=30` and stdout/stderr to the journal. A hostile peer that
exploits an ssh client bug therefore gets no root file or socket authority.
Like the self-test it sets no `RestrictSUIDSGID=` (measured on systemd 255);
the systemd 255 fact that made the self-test leave `User=` unset concerns an
explicit `User=root` only. The probe refuses to run as root. It proves
against the live sshd that a real public-key login and a real `-N -R`
forwarding session as the account are refused: the ssh client (fresh,
never-authorised ed25519 key; this binary as `ProxyCommand=… --relay`, so the
source port is known) must exit 255 with `Permission denied (publickey).`, and
the journal must hold sshd's (`sshd`, `sshd-session` or `sshd-auth`) root
record, from the session of that source port (same `_PID` as a record carrying
the port), saying the account is listed in `DenyUsers`. "NOT denied" needs
sshd's own root `Accepted publickey for <account> from 127.0.0.1 port <source
port> ssh2: ED25519 <fingerprint>` record for this run's key (fingerprint from
`ssh-keygen -l`); a client that got in without it is only "unverified". Pass:
exit 0 and one JSON line `{"version":1,"account":"…","port":N,"result":"denied"}`.
Failure: `{"code":"ssh_probe_failed","stage":"<name>"}` and the stage's exit
code (shared table `crates/duduclaw-isolation/src/probe_exit.rs`; 5 and 7
belonged to stages the root probe had and are never reused):

| Code | Stage | Meaning |
|---|---|---|
| 2 | `usage` | argv is not `<account>.<port>` (employee name rule, one `.`, port 1..=65535 without leading zero) or `--relay <src> <dst>` |
| 3 | `identity` | running as root, or no single `/etc/passwd` row in the employee uid range |
| 4 | `tools` | ssh, ssh-keygen, journalctl or the probe itself not a root-owned, non group/other-writable file |
| 6 | `workspace` | private 0700 directory under the unit's `/tmp` |
| 8 | `keygen` | ephemeral key generation or its `ssh-keygen -l` fingerprint |
| 9 / 12 | `login_inconclusive` / `forward_inconclusive` | neither refused nor accepted (connection refused, timeout, tool error) |
| 10 / 13 | `login_not_denied` / `forward_not_denied` | the account is reachable: client exit 0 (or the `-N` session stayed connected) AND sshd's matching root `Accepted publickey` record |
| 11 / 14 | `login_journal` / `forward_journal` | client refused, but no matching sshd DenyUsers journal record |
| 15 | `deadline` | the probe's 25 s deadline expired |
| 16 | `output` | the result line could not be written |
| 17 | `relay` | `--relay` mode only |
| 18 / 19 | `login_unverified` / `forward_unverified` | client exit 0 (or still connected) WITHOUT sshd's Accepted record (inconclusive) |

Host prerequisites where `/usr/sbin/sshd` exists: `/usr/bin/ssh`,
`/usr/bin/ssh-keygen`, `/usr/bin/journalctl`, the `duduclaw-selftest` account
and the `systemd-journal` group, sshd listening on 127.0.0.1 at the first
`Port` of its effective configuration, and sshd logging to the journal.
Hosts without sshd skip SSH checks entirely (spawnd never starts the unit
there). The protected installer
installs the template at `/etc/systemd/system/` (root:root 0644, exactly these
bytes) and never enables or starts it; rollback stops any running instance and
removes the file. No account is created for it (it reuses `duduclaw-selftest`).
Spawnd starts one instance per employee account through PID 1
(`Systemd::run_fixed_unit`, R7c part 2) and maps the exit code with
`duduclaw_isolation::probe_exit`; results are re-proven when sshd restarts and
at least every 6 hours, and allocation answers "pending" while the first proof
runs; see the spawnd README, "R7c: SSH denial from spawnd's own probe through
PID 1". sshd with `UsePAM no` is unsupported (sshd reports the expired account
before `DenyUsers`).

The native Linux service lifecycle and D-Bus fd handoff require root VM testing
on systemd 249, 252 and 255. No installation or activation has been performed by
this implementation change.

## License

Apache-2.0, as declared by the workspace.
