# duduclaw-parse-worker

Parses untrusted documents in a disposable Linux service instead of the gateway
account. A systemd socket starts one DynamicUser service per connection. Requests
carry sealed input file descriptors and the shared isolation worker contract;
responses are bounded structured documents/tables/HTML, image dimensions, archive
manifests and verified output artifacts, or closed errors.

The default library contains only the client, policy and transport. Parser
dependencies are behind `engine`, required by the binary. These files are
deployment inputs, not an installer. Do not grant the gateway permission to create
arbitrary systemd units.

## Installation (root, systemd 249 or newer)

After verifying the release signature, install the manifest-pinned binary at
`/usr/libexec/duduclaw/duduclaw-parse-worker` (root:root, 0755) and the supplied
socket/template into `/etc/systemd/system/` (root:root, 0644). The protected Linux
installer now invokes its embedded tree builder after authenticating the selected
generation's recorded manifest under the updater lock. Missing trust/state,
pending updates, or a worker that differs from the signed artifact refuse tree
preparation. The managed reinstall path prepares a missing tree, verifies an
existing one, then runs readiness without replacing signed service units.

For standalone staging after release verification, the root installer must build
a **new minimal runtime tree** before enabling the socket:

```sh
install -d -o root -g root -m 0755 /usr/lib/duduclaw/parse-root
python3 crates/duduclaw-parse-worker/scripts/build-runtime-tree.py \
  --worker-sha256 "$VERIFIED_MANIFEST_WORKER_SHA256"
```

`VERIFIED_MANIFEST_WORKER_SHA256` must come from the already-verified release
manifest. The helper is an installation input; it does not authenticate releases
or activate services. It requires root-owned, non-writable inputs/ancestors,
checks the worker digest, resolves its ELF interpreter and exact dependency
closure with the trusted loader's `--list` mode under a clean environment, and
copies only those files. `/usr/bin/readelf` is required on the install host.
Unresolved dependencies, unexpected loader output, unsupported interpreters,
non-system library paths and an existing destination in standalone build mode are refused. Supported
interpreters are the Linux x86_64 and aarch64 glibc loaders; static ELF is accepted.

The tree has no host `/usr`, `/lib` or `/lib64` binds. It contains the fixed worker
path and individual runtime library files; merged-/usr symlinks are resolved at
installation and copies are placed at the required in-root paths. `/usr/local`
and unrelated `/usr/lib/duduclaw` components remain hidden. Installer mode builds
and fsyncs a private staging tree, verifies it, then atomically publishes over a
missing or empty destination. A populated tree is reused only after a bounded
inventory and content comparison against the verified worker and exact current
library closure. Unknown entries, symlinks, hard links, changed files, unsafe
ownership/modes and nested mounts refuse reuse; failed staging stays inactive.
Readiness distinguishes this populated tree from the empty `parse-root` staging
skeleton and revalidates it against signed state.

Replacing an existing tree for a new worker or runtime library version still
requires the signed generation coordinator to close admission, stop workers and
publish a new checked tree. The installer deliberately refuses a mismatch and
preserves the old tree. First bootstrap before recorded state exists, automatic
update-time replacement, and root-VM startup/roundtrips remain integration and
acceptance requirements.

`parse-root` is the `RootDirectory` staging mountpoint. A nonrecursive read-only
bind of `parse-root-tree` replaces its contents before the worker starts, hiding
base directories that systemd 249 creates in a bare `RootDirectory`. The staging
root must be root-owned with non-writable ancestors. Only empty mountpoints may
be added by systemd; never put configuration, user data, homes or sockets there.

`DynamicUser` forces `ProtectSystem=strict` and `PrivateTmp` in systemd 249.
Its read-only `/` rule overrides a tmpfs at `/`, and its private `/tmp` rule
overrides a tmpfs at `/tmp`. The unit therefore mounts the bounded tmpfs at
`/.tmp`, then binds it to both `/tmp` and `/var/tmp` through the staging path before
pivoting root, overriding both implicit PrivateTmp host-disk mounts. All three
names refer to the same per-instance 8 MiB / 1024-inode filesystem; mode 0700
remains root-only, since parsing uses passed memfds. `/var` exists only as the
mountpoint for the bounded `/var/tmp`, with no other entries. `/sys` remains a
read-only sysfs projection forced by systemd kernel protections; all mounts
below it must also be read-only. Every host-disk filesystem projection must be
read-only. `InaccessiblePaths` cannot suppress these implicit `/var/tmp` and
`/sys` mounts. Module paths are absent from the minimal runtime tree and remain explicitly hidden. `/run` is read-only and contains
only systemd's empty `/run/systemd/incoming` mount-propagation endpoint; systemd
249 requires this mount even for a private root. No host runtime sockets are
projected. `/proc` retains the process visibility restrictions, and `/dev` uses
`PrivateDevices`. Explicit `MountAPIVFS=no` suppresses systemd 255's automatic
host os-release projection; the process and kernel protection settings still
force API VFS mounts. All existing syscall, capability and resource limits stay.
The v255 distinction is in
[exec-invoke.c](https://github.com/systemd/systemd/blob/v255/src/core/exec-invoke.c)
and [namespace.c](https://github.com/systemd/systemd/blob/v255/src/core/namespace.c).

Compatibility was checked against upstream systemd v249
[mount setup](https://github.com/systemd/systemd/blob/v249/src/core/namespace.c),
[base directory creation](https://github.com/systemd/systemd/blob/v249/src/shared/base-filesystem.c),
[DynamicUser implications](https://github.com/systemd/systemd/blob/v249/src/core/unit.c)
and [execution order](https://github.com/systemd/systemd/blob/v249/src/core/execute.c).
Dynamic UID/GID allocation and supplementary-group/NSS lookups happen before
mount namespace setup. The worker itself checks numeric credentials and does not
need passwd/NSS lookups, so no `/etc` projection is needed for DynamicUser startup.
Actual startup and parser roundtrips on systemd 249/252/255 remain root-VM checks.

The A/G integration seam is `client::require_isolation()` before accepting any
work; the root-owned gateway unit may also set `DUDUCLAW_ISOLATION_REQUIRED=1`.
Presence of that variable (even empty or malformed) requires isolation. The latch
cannot be reset. This is not a protected-readiness check or a migration switch.
Unsupported operations fail closed; legacy installations retain their old path.

The named A/B lifecycle seam is `WorkerLauncher` / `WorkerSession`, installed once
through `install_launcher`. Protected gateway installation requires
`supports_live_authority()`, and protected calls use `exchange_checked`; its
default implementation refuses rather than invoking a legacy unchecked session.
The fixed socket session rechecks the retained caller authority after poll and
immediately before sending or receiving descriptors. Silent peers are polled in
at most 50 ms intervals so revocation also ends blocked transport waits. Legacy
unprotected calls retain their original exchange API.

The default fixed socket launcher has process-local admission and failure
quarantine, but does not provide host-wide cancellation receipts. Closing a
revoked transport does not prove that systemd has retired the worker. Output
memfds are sealed before size and SHA-256 verification.

## Development

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-parse-worker --features engine --offline
cargo build -p duduclaw-parse-worker --features engine
```

Privileged acceptance tests have the prefix `iso_root_` and are ignored by default.
Run them on a disposable Linux system with the supplied units installed. See
`docs/architecture/native-linux-isolation.md` for coverage and remaining seams.

## License

Apache-2.0.
