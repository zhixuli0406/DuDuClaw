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

## Inactive bootstrap publication

`duduclaw-updated --bootstrap-stage` is a root-only fixed-inbox operation. The
release archive also includes the same ELF as `duduclaw-updated-launch`; the
installed frozen path is `/usr/libexec/duduclaw-updated-launch`. The verifier must
be built with the independently published `DUDUCLAW_BOOTSTRAP_ROOT_SHA256` root
fingerprint. A build without that pin refuses; there is no flag or runtime
environment override. Before executing it as root, independently authenticate
the verifier itself according to the approved transition procedure.

The installer creates root-owned, non-writable ancestors for `/etc/duduclaw-updated`,
`/var/lib/duduclaw-updated`, `/opt/duduclaw-isolated` and its `generations` directory.
Place these root-owned regular files in `/etc/duduclaw-updated/bootstrap`:

```text
root.json
root.json.signatures.json
manifest.json
manifest.json.signatures.json
artifacts/<each signed artifact id>
```

The optional offline packaging flags `--bootstrap-root <root.json>` and
`--bootstrap-root-signatures <root.json.signatures.json>` assemble that inbox
alongside the signed raw release assets. They also require the existing
`--manifest-template` and `--signing-key` arguments. Assembly checks hashes and
layout; the Rust verifier remains responsible for root threshold, target
signature, expiry, protocol, architecture, modes and complete component coverage.
Inputs are pinned by descriptors through verification and copying. A successful
stage publishes one immutable generation with atomic rename and fsync; replay
rechecks all bytes instead of trusting an earlier receipt.

This operation refuses an existing updater state, trust bundle or `current`
selector. It does not write those objects, provision A/B slot receipts, copy
executables to service paths, start a service, or execute staged content. It is
**not** the v1.79.3 transition authorizer: legacy/new-root dual signatures and live
activation are still required before an installer may select or run the generation.
The `bootstrap_staged:<digest>` result must never be reported as readiness.

## Validation

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-updated --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --target x86_64-unknown-linux-gnu -p duduclaw-updated --offline
```

On a provisioned root-capable Linux VM with systemd and python3:

```sh
cargo test -p duduclaw-updated -- --ignored iso_root_ --test-threads=1
```

For only the inactive publisher's real filesystem tests (corrupt/missing/link
inputs, interrupted copy, idempotent readback and no selector/commit writes):

```sh
sudo cargo test -p duduclaw-updated iso_root_bootstrap_ -- --ignored --test-threads=1
```

See [implementation and acceptance boundaries](../../docs/architecture/native-linux-isolation.md).
Hardware sshsig fixtures require Homebrew OpenSSH and a FIDO2 key; synthetic
vectors do not establish hardware acceptance.

## License

Apache-2.0.
