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

The daemon also binds the root-only update control socket
`/run/duduclaw-spawnd/update.sock` (root 0600) for `duduclaw-updated`. Its
`Apply` republishes the verified generation's root components, so the unit adds
`ReadWritePaths=-/usr/lib/duduclaw -/usr/libexec/duduclaw -/opt/duduclaw/bin`:
atomic replacement creates `<name>.next` inside each directory and renames it,
which needs directory write access, broader than the eight fixed files. Narrowing
it needs a separate narrow publisher helper (known residual, plan R6).
The drain barrier is kept in `/var/lib/duduclaw-spawnd/drain.json` across restarts.

The unit is `Type=notify` (`NotifyAccess=main`, `TimeoutStartSec=300`). Every
start re-verifies the installed set (publication journal, or the generation
`current` names) before binding anything; `READY=1` then only means that
`update.sock` is bound, and it is sent whether or not the set is consistent, so
`duduclaw-updated.service` (`Wants=` + `After=duduclaw-spawnd.service` only) can
always repair the host. On an inconsistent set spawnd opens nothing else; a
repairing `Apply` makes it exit with status 82, which the unit restarts
(`RestartForceExitStatus=82`, `SuccessExitStatus=82`; crash loops stay limited
by `StartLimitIntervalSec=300`/`StartLimitBurst=5`). Its stderr goes to the
journal (fixed-vocabulary lines), and `systemctl status` shows the `STATUS=` line.

`duduclaw-isolation-ready.service` (oneshot, `RemainAfterExit=yes`,
`Requires=`/`After=duduclaw-spawnd.service`) runs `duduclaw-spawnd
wait-consistent` and completes only when the live spawnd reports the verified,
opened, consistent set in `/run/duduclaw-spawnd/update-status.json`.
`duduclaw-egress.service` has `Requires=` + `After=` on it. The isolated-install
gateway unit (written by the installer package, R3) must carry the same two
lines: `duduclaw_isolation::ids::ISOLATED_GATEWAY_UNIT_DEPENDENCIES`. The
single-binary gateway unit (`duduclaw_core::root_key_recovery_unit`) does not
(no spawnd on those hosts).

The native Linux service lifecycle and D-Bus fd handoff require root VM testing
on systemd 249, 252 and 255. No installation or activation has been performed by
this implementation change.

## License

Apache-2.0, as declared by the workspace.
