# Native Linux isolation implementation

This is an internal implementation record for design r7. Packages are delivered
separately and do not establish protected-mode readiness by merely compiling.

## Package H

`crates/duduclaw-isolation-storage` implements root-side, per-employee fixed-size
ext4 loop storage. It is a workspace library for package B1, not a gateway feature
flag, installer, daemon, mount namespace builder, or project-quota backend. No
existing installation is switched over by this change. Shared contract wire
formats, canonical bytes and hard limits are unchanged.

### B1 integration

1. Installation creates an empty root:root 0700 storage directory with root-owned,
   non-writable ancestors on ext4/XFS. B1 makes the relevant mount tree private.
2. `Storage::open(path)` verifies the anchor, takes a lifetime exclusive flock,
   validates bounded metadata and compares the full persisted backing device
   graph. Opening allows maintenance even with low free space/incomplete images.
3. B1 calls `readiness()` at startup, before admission and periodically during
   execution. Failure refuses new runs and B1 supplies the administrator notice.
   H has no background task/thread to monitor free space by itself.
4. After administrator confirmation and UID registry allocation, call
   `provision(EmployeeId, uid, Quota::try_from(&DiskQuota))`. The UID must already
   have passed B1's NSS/exclusions/account-lockdown checks. H currently accepts
   the contract's default 70000–99999 range only.
5. `attach(employee, noexec)` returns `MountAnchor { path, noexec }`. B1 must
   clone only `path/home` and `path/work` with `open_tree`, recursively make each
   detached clone private and set the required mount flags before handing mount
   fds to the trampoline. Image files and loop devices never enter that handover.
6. B1 revokes capabilities, drains the cgroup/UID and releases ALL detached mount
   clones before `resize` or `detach`. `destroy` requires the image to be unmounted and no
   configured loop referencing its inode. Neither method authorizes deletion or
   proves a run has stopped; B1 enforces those policy decisions.

The named B1 integration seam is `EmployeeStorage: Send` (object-safe, root-local,
not serializable), implemented by `Storage` on Linux and by the refusing facade
on other platforms. It covers readiness/provision/attach/detach/destroy/usage/resize.
There is no gateway-uid directory fallback or default-success implementation.
B1 must keep its per-employee admission lock throughout maintenance and must not
hand out clones while resize temporarily mounts the old filesystem.

`usage()` reads kernel `statvfs`, including inode counts, after checking mount
identity and flags. It requires an attached image. Quota is filesystem capacity,
including ext4 overhead, lost+found and the home/work directories; usable employee
space/inodes can therefore be smaller than the requested maximum.

### Admission and backing storage

The charged maximum of every image (Creating, Ready, Resizing, Deleting or Quarantined),
plus a candidate's capacity, plus `max(20 GiB, ceil(host capacity / 10))` must fit
host total capacity. Available host space must independently cover additional
allocation plus that reserve. Overflow, unreadable capacity and low space refuse
admission. Existing images stay charged until deletion and directory fsync have
completed. This is not a `du` polling guarantee.

`fstatfs` accepts only EXT4_SUPER_MAGIC/XFS_SUPER_MAGIC. From the backing filesystem
major:minor, sysfs traversal follows every partition parent and every `slaves`
edge, with bounded depth/node count and cycle/missing-node rejection. Physical
nvme/virtio/scsi/xen disks, md and partitions are recognized. Device mapper invokes
fixed `/usr/sbin/dmsetup table --major N --minor N` and permits only `linear`,
`crypt`, `striped`, `raid`; every dependency is still traversed. Unknown types,
including loop-backed host filesystems, are refused. Only target types and graph
edges are persisted: crypt keys/table arguments are never logged or stored.
Cloud hypervisor allocation is outside this guarantee, as in the design.

Provision reserves metadata before file creation, uses `fallocate` and verifies
`st_blocks * 512 >= size`. Images remain root:root 0600, single-link files resolved
from the anchor with `openat2(BENEATH|NO_SYMLINKS|NO_MAGICLINKS|NO_XDEV)`. Formatting
uses root-owned `/usr/sbin/mkfs.ext4` with fixed argv, fixed 4096-byte blocks,
`-m 0`, `-N <inode ceiling>`, and
`-E lazy_itable_init=0,lazy_journal_init=0,nodiscard`. The image is inherited only
as fd 3; no shell or untrusted executable/config/argv is accepted. Helpers have
empty environment, fixed PATH/locale/cwd, bounded output and timeouts. They are
trusted root-side tools and must be shipped/maintained by the host administrator.
Only existing locked dependencies rustix 1.1.4 and nix 0.29.0 were enabled; no
new dependency versions or heavy dependency trees were added.

The bounded superblock preflight rejects bad magic, error state, excessive inode
rounding or incompatible capacity. `mkfs -N` can round: a result above the caller's
ceiling is refused, leaving a charged Creating record for explicit cleanup. A
dirty non-error image may replay its journal in the kernel. No automatic root
`fsck` runs on employee images.

Attachment checks existing loop devices by kernel backing `(device,inode)` rather
than parsing escaped `backing_file` path text. This also detects path aliases.
`LOOP_CTL_GET_FREE` + `LOOP_CONFIGURE` uses AUTOCLEAR; no SET_FD/SET_STATUS race.
The image flock and manager lock serialize cooperating writers. Host mounts use
`nosuid,nodev,errors=remount-ro` plus `noexec` when selected; mountinfo, loop backing
identity and statvfs/statfs flags/capacity are read back. A failed mount/readback
quarantines the image. Cleanup verifies identity separately so a verified
readonly/quarantined image can still be unmounted. There is no lazy unmount,
recursive deletion, root fsck, or automatic fallback.

### Crash/recovery and limits

The 4 MiB / 4096-image bounded, versioned registry uses exclusive temp create,
write, file fsync, rename and parent-directory fsync. A leftover temp file is
validated before replacement; it is never promoted over committed state. Missing
committed metadata with existing artifacts, unknown files, wrong ownership,
symlinks/hardlinks or replaced image identity refuse automatic reconstruction.
Any failed metadata publication poisons the current handle until it is reopened.
The root registry may preserve Creating/Deleting states after restart; readiness
refuses them and only explicit safe `destroy` releases their charge. Data recovery
is an operator decision. A quarantined entry conservatively blocks readiness for
the manager, including other employees, until cleaned up.

### Resize protocol

`resize` supports monotonic byte growth with a non-decreasing inode ceiling;
identical healthy quotas are an idempotent no-op. Shrink and inode-only changes
are explicitly refused before mutation (safe shrink requires content migration,
not file truncation). Existing host mounts or configured loops refuse growth.
Readiness and admission are rechecked, charging only the additional allocation
against free space but the full target against total capacity. The old verified
filesystem is temporarily mounted noexec in B1's private mount tree. Target quota
and `Resizing` are published durably BEFORE allocation. H uses fallocate, fsync,
`LOOP_SET_CAPACITY`, and `EXT4_IOC_RESIZE_FS`, then syncfs, mount/flags/quota and
bounded superblock readback. Finally it closes maintenance descriptors, unmounts
and publishes `Ready`. It never invokes resize2fs or fsck against employee data.

On any error after reservation the target stays fully charged and the entry is
quarantined; an interrupted process leaves `Resizing`. Both refuse readiness and
attach after reopen. No image truncation, automatic resume, or rollback to the old
charge occurs. If unmount cannot finish, the entry stays `Resizing` and charged.
Cleanup still uses verified image/loop identity and can detach these states.
An operator may recover data separately or explicitly destroy the image after
all clones are gone. Actual inode growth is kernel/ext4-group dependent; a target
inode ceiling that cannot contain the resulting capacity fails readback and
quarantines instead of publishing an over-quota mount.

Project quota is an optional alternative to this loop backend, not implemented.
Installation-specific custom UID ranges cannot be widened beyond the unchanged
shared contract defaults. These are explicit unsupported inputs, not success
stubs. Damaged-data recovery is an operator action; H does not interpret or repair
filesystem content. CPU/memory/PID/IO/PTY enforcement and runtime monitoring /
administrator notices are B1/B2 responsibilities; H supplies synchronous readiness
and kernel-enforced byte/inode ceilings, not a background daemon.

### Validation and remaining acceptance

Round 2 executed on macOS: storage pure/unit tests (16 passed, 0 failed),
unchanged shared contract tests (29 passed). Linux implementation AND its tests
pass cross-target check; Windows implementation/tests pass cross-target check.
Linux check emits six workspace `unsafe_code` warnings for the documented loop /
ext4 ioctls and async-safe pre-exec fd handover. These syscall-boundary warnings
are retained. One initial unit-test run failed because its shrink fixture was
below the minimum valid filesystem size (13 passed, 1 failed); the fixture was
corrected to test a valid shrink, and the full final run passed.

Run on an isolated Linux VM with root, loop devices, e2fsprogs and more than
20 GiB available on supported physical ext4/XFS backing. Prepare a fresh empty
root:root 0700 `ISO_STORAGE_TEST_ROOT` with root-owned, non-writable ancestors.
Never point it at production storage. `unshare` below makes the test mount tree
private without changing propagation in the host namespace. Run tests serially.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --tests --target x86_64-unknown-linux-gnu -p duduclaw-isolation-storage --offline
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo check --tests --target x86_64-pc-windows-gnu -p duduclaw-isolation-storage --offline
# As root, in a toolchain/build environment permitted to execute cargo:
unshare --mount --propagation private env ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline -- --ignored iso_root_ --test-threads=1
```

Eight privileged tests are ignored by default:

- `iso_root_provision_mount_usage_unmount_destroy`: formatting and real loop mount,
  inode/data ENOSPC without breaking host metadata writes, flags, UID ownership,
  shrink refusal, idempotence, readonly/quarantine detach and destruction.
- `iso_root_metadata_crash_safety`: interrupted write/fsync/rename/directory-fsync
  publication; recovery sees old or new complete metadata.
- `iso_root_incomplete_creation_never_attaches`: exclusive lock, reopen after an
  incomplete reservation, readiness refusal and explicit charged-state cleanup.
- `iso_root_resize_grows_preserves_data_and_rejects_live_mount`: kernel grow,
  live-mount refusal, data retention, readback, unmount, idempotence and reopen.
- `iso_root_resize_interruption_keeps_full_charge_and_refuses_attach`: unwind at
  reservation/allocation/kernel-grow checkpoints skips normal error handling;
  reopen keeps the full target, refuses attach and permits explicit cleanup.
- `iso_root_resize_error_quarantines_full_target`: normal error path, durable
  quarantine and target charge, reopen, refusal and safe cleanup.
- `iso_root_rejects_tmpfs_and_recursive_loop_backing`: actual unsupported tmpfs
  and ext4 on a loop device both refuse opening as host backing storage.
- `iso_root_rejects_image_alias_and_replacement`: direct identity checks reject
  symlink, hardlink and replacement inode (not masked by inventory failure).

These native tests are cross-compiled, NOT executed on Linux. Their interruption
hooks are software fault injection, not physical power-loss tests. Ubuntu 22.04 /
5.15, Debian 12 / 6.1 and Ubuntu 24.04 / 6.8 execution is the upstream VM gate.
Real thin-LV-under-crypt fixtures, live topology replacement, non-cooperating loop
races, physical power-loss and Daybreak security review remain acceptance work.
Pure fixtures cover recursive thin-under-crypt rejection, canonical topology /
target change detection and bounded graph refusal. B1 runtime wiring and monitoring
remain with its integrator against `EmployeeStorage`; H's side is implemented.
No release, live-provider, customer acceptance or active isolation is claimed.

Independent code review of round 2 found no confirmed critical/high blocker; it
caught an alias test initially masked by inventory rejection. The test now calls
the identity check directly with exact errors. The reviewer did not execute Linux
tests or perform the separate formal Daybreak security review. No shared wire
schema, canonical bytes, hard limits or dependencies changed in round 2.
No Package H handoff/security-gap notes were found under wiki/impl or wiki/reports;
all H handoff notes remain in this internal document.

Reference syscall semantics:
[loop(4)](https://www.man7.org/linux/man-pages/man4/loop.4.html),
[dmsetup(8)](https://man7.org/linux/man-pages/man8/dmsetup.8.html),
[Linux 5.15 loop UAPI](https://raw.githubusercontent.com/torvalds/linux/v5.15/include/uapi/linux/loop.h),
[ext4 resize ioctl](https://raw.githubusercontent.com/torvalds/linux/v5.15/fs/ext4/ioctl.c).
