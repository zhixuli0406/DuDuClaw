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

The unit is `Type=notify` (`NotifyAccess=main`, `TimeoutStartSec=infinity`).
spawnd sends `READY=1` only after its publication journal
(`/var/lib/duduclaw-spawnd/publish.json`) is settled and `update.sock` is bound;
if neither roll-forward nor roll-back completes, it withholds `READY=1`, keeps
admission closed and keeps answering root on `update.sock` (status in
`/run/duduclaw-spawnd/update-status.json`). `duduclaw-egress.service` has
`Requires=` + `After=duduclaw-spawnd.service`. The gateway unit
(`duduclaw_core::root_key::recovery::protected_unit_text`) must add the same two
lines in the installer package; until then the gateway can start on a set that
spawnd is still recovering.

The native Linux service lifecycle and D-Bus fd handoff require root VM testing
on systemd 249, 252 and 255. No installation or activation has been performed by
this implementation change.

## License

Apache-2.0, as declared by the workspace.
