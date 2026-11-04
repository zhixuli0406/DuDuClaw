# duduclaw-updated

Verifies officially signed Linux releases and coordinates protected updates without
allowing the gateway to choose a command, download URL or installation path.

The shared wire types live in `duduclaw-isolation`. The portable library verifies
sshsig metadata and models durable recovery. Linux adapters own the root state,
fixed socket, downloader privilege separation and verified file descriptors.
This package is not an installer: migration must provision its root trust and
accounts. The Linux activator publishes one generation, restarts fixed services
and compares authenticated live readiness and loaded-executable identity before
advancing the version floor. Named A/B/C/G drain, migration/runtime and readiness
seams must be implemented; missing endpoints fail closed. Interrupted updates
resume through the frozen A/B launcher. Root-only trust rotation and signed
emergency rollback preserve the normal monotonic floor.

## Validation

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --offline
```

On a provisioned root-capable Linux VM with systemd and python3:

```sh
cargo test -p duduclaw-updated -- --ignored iso_root_ --test-threads=1
```

See [implementation and acceptance boundaries](../../docs/architecture/native-linux-isolation.md).
Hardware sshsig fixtures require Homebrew OpenSSH and a FIDO2 key; synthetic
vectors do not establish hardware acceptance.

## License

Apache-2.0.
