# duduclaw-spawnd

Root-owned launch coordination for the planned native Linux employee isolation.
The daemon accepts only the gateway account on the fixed seqpacket socket and
serializes capability-holding runs for each employee. It never executes employee
code itself or falls back to the gateway identity.

This package is an integration component, not a complete protected installation.
The B2 trampoline, C egress and H storage adapters must supply verified readiness,
sealed bootstrap descriptors and cleanup evidence. The shipped binary refuses
launches until those adapters are connected; installing its units does not enable
employee isolation. The production Host includes root account recovery, durable
UID/run journals, real systemd transient units, detached mount/executable
preparation, bounded lifecycle jobs and an independent intrusion monitor.
See [Package B1](../../docs/architecture/native-linux-isolation.md#package-b1)
for adapter contracts and remaining cross-package acceptance.

## Build and verification

Only Linux runs the service. The policy and admission tests are portable.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-spawnd --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check -p duduclaw-spawnd --target x86_64-unknown-linux-gnu --offline
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

## License

Apache-2.0, as declared by the workspace.
