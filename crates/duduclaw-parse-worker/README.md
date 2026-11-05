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

Install the binary at `/usr/libexec/duduclaw/duduclaw-parse-worker` (root:root,
0755) and the socket/template under `systemd/` into `/etc/systemd/system/`
(root:root, 0644). Before enabling the socket, create **both** dedicated roots:

```sh
install -d -o root -g root -m 0755 /usr/lib/duduclaw/parse-root
install -d -o root -g root -m 0755 /usr/lib/duduclaw/parse-root-tree
```

Both paths must be real directories with root-owned, non-writable ancestors.
They must start empty and must never contain application data, configuration,
user homes or sockets. On reuse, reject unexpected contents; do not recursively
delete an existing directory. systemd may leave empty mountpoint directories in
the tree (`.tmp`, `dev`, `lib`, `lib64`, `proc`, `run`, `sys`, `tmp`, `usr`,
`var/tmp`) and base directories in the staging root. Do not copy `/etc`, `/var`, `/home` or `/root`
into either directory. Then run `systemctl daemon-reload` and
`systemctl enable --now duduclaw-parse.socket`. The root VM acceptance suite must
be rerun after installing these inputs; this README is not a live acceptance
receipt. An external installer must create these two directories as well as
installing the units and executable; this crate does not implement that installer.

`parse-root` is the `RootDirectory` staging mountpoint. A nonrecursive read-only
bind of `parse-root-tree` replaces its contents before the worker starts. This
also hides the base directories that systemd 249 creates in a bare
`RootDirectory`; `/var` is recreated only for the bounded `/var/tmp` mount. Nonrecursive `/usr`, `/lib` and optional `/lib64` binds
provide the fixed executable and runtime libraries. systemd resolves bind source
symlinks on the host, so merged-/usr distributions need no copied loader or
installer-created `lib` symlinks. The tree's bind destinations are directories.

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
`/sys` mounts. Module paths under the separate `/lib` and `/lib64` projections are hidden too, preserving
`ProtectKernelModules` across these aliases. `/run` is read-only and contains
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
through `install_launcher`. Protected callers use `retirement::RetiringLauncher`
when no launcher was explicitly installed. Boot may also install it explicitly.
It reserves one host-wide slot before snapshotting, using a fixed root-owned
`/run/duduclaw-parse/retirement.lease` file (regular, one link, mode 0660,
root:duduclaw). The installer must create it with exactly `clean\n` only after
proving that no previous parse worker or socket activation remains. The launcher
never creates or resets it. `systemd/duduclaw-parse-tmpfiles.conf` supplies the
create-only boot rule; install it as `/usr/lib/tmpfiles.d/duduclaw-parse.conf`.
It must not be used to repair a live dirty lease, and the installer still owns
the clean-install/no-existing-worker prerequisite. Its conservative slot covers the full input ceiling
and the fixed worker memory ceiling; a second process cannot acquire the flock.

Before receiving content, the worker answers a private nonce handshake with its
own pidfd. The gateway receives kernel SCM_CREDENTIALS, verifies the pidfd's PID
and liveness, and resolves the exact systemd instance, InvocationID, MainPID,
DynamicUser, PrivateNetwork, NoNewPrivileges, TasksMax and MemoryMax. The worker
closes that pidfd before parsing. Ordinary parse request/response schemas are
unchanged; the legacy non-protected socket launcher does not send the handshake.

The lease is fsynced dirty before connecting. A response does not retire it:
a separate bounded cleanup thread waits for the recorded unit to be inactive or
failed with its original InvocationID, its cgroup to be empty/removed, and its
pidfd to be exited. If PID 1 has already garbage-collected the instance, only an
explicit `GetUnit(exact_name)` `NoSuchUnit` reply together with an exited pidfd
and removal of the exact recorded cgroup directory can substitute for inactive.
The cgroup-v2 mount is verified before interpreting a missing directory; bus
errors or an unreadable/missing events file alone do not count. No `RefUnit` or
`UnrefUnit` is issued (systemd 249 duplicate-reference acceptance regression).
Only these full readbacks permit writing clean and releasing the slot. Unknown launch ownership, failed
readback, cleanup timeout or gateway crash leave dirty durably and refuse later
work. Root must reconcile the socket queue and all parse units/cgroups before
resetting it; automatic crash recovery is not implemented. This sacrifices
availability after uncertain failure instead of releasing an unproven budget.
The legacy socket launcher retains process-local quarantine only. Output memfds
are sealed before size and SHA-256 verification.

Round 3 adds `iso_root_e2e_worker_retirement_holds_host_slot_through_response_and_drop`
for the successful-response and pre-snapshot-drop paths. This test needs the
updated worker binary, template, system bus and initialized lease on a disposable
Linux VM; it has not been run on macOS.

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
