# duduclaw-isolation-storage

Root-side storage for native Linux employee isolation: fixed-size ext4 loop images,
fully allocated before attachment and bound to immutable employee IDs. This library is not wired into
the gateway; B1 must call its readiness and lifecycle methods under admission.

## Requirements

Linux 5.15+, root, ext4/XFS backing storage with a verifiable non-thin device
chain, loop-control, and root-owned e2fsprogs (`/usr/sbin/mkfs.ext4`). Device mapper
also requires `/usr/sbin/dmsetup`. The storage directory must already exist, be
root:root 0700, and have only root-owned, non-writable ancestors. It must be on a
private mount tree; B1 is responsible for namespace setup before opening it.

## Usage

`Storage::open(path)` takes an exclusive lifetime lock. `provision(employee, uid,
quota)` creates the image; identical requests are idempotent. `attach(employee,
noexec)` returns a root-only mount anchor, never an image or device handle.
`detach`, `destroy`, `usage`, and `readiness` recheck actual state. B1 must prevent
active runs and detached mount clones before detach/destroy; these methods do not
replace admission or cgroup cleanup. No Linux operation runs on other platforms:
`Storage::open` returns `Unsupported` there.

After unmounting, `detach` polls every 20 ms for up to 3 seconds for all loop
devices backed by the exact image device/inode to disappear, including on retries
when the host mount is already gone. A loop still pinned by a mount clone or open
device descriptor returns `Busy`; observation errors fail closed, and no loop is
forcibly cleared. Successful resize uses this same detach barrier before `Ready`.

B1 can use the object-safe, root-local `EmployeeStorage: Send` seam, implemented
by `Storage` on every platform (all methods refuse on non-Linux). It is not a
wire RPC, admission decision, or permission to hand a path to an employee.

Byte and inode limits use the shared `DiskQuota::LoopFilesystem`. Project quota
is explicitly unsupported. `resize(employee, target)` grows an unmounted image
with monotonic byte/inode ceilings. B1 must first drain runs and release all
mount clones. H checks capacity for the delta, attaches the old size in its private
mount tree, durably charges the full target as `Resizing`, fully allocates the
image, updates loop capacity, and calls the kernel ext4 grow ioctl. It reads back
mount limits and the superblock after `syncfs`, reallocation, file sync and an
allocation check; it unmounts and repeats the allocation barrier before `Ready`. Actual inode
capacity depends on ext4 block-group rounding; a target ceiling too small for
the result quarantines it. No resize2fs/fsck runs. Any failure after reservation
keeps the target charged and blocks attachment; process interruption leaves
`Resizing`, normal failures quarantine. Operator recovery or explicit destruction
is required; no automatic shrink/rollback can truncate data. Shrink and inode-only
changes are deliberately refused. Identical healthy quotas are a no-op.

### Loop discard and allocation recovery

Mounts explicitly use `nodiscard`, but this does not prevent ext4 kernel zeroout
from issuing `WRITE_ZEROES` without `REQ_NOUNMAP`. The loop driver can turn that
into holes in the host image, including during online growth. Setting only
`queue/discard_max_bytes=0` is not a portable protection: in upstream
[Linux 5.15 loop.c](https://github.com/torvalds/linux/blob/v5.15/drivers/block/loop.c),
`lo_fallocate` checks `blk_queue_discard`, a
[queue flag](https://github.com/torvalds/linux/blob/v5.15/include/linux/blkdev.h)
that the [sysfs limit write](https://github.com/torvalds/linux/blob/v5.15/block/blk-sysfs.c)
does not clear. In [6.1](https://github.com/torvalds/linux/blob/v6.1/drivers/block/loop.c)
and [6.8](https://github.com/torvalds/linux/blob/v6.8/drivers/block/loop.c), both
discard and write-zeroes pass through `lo_fallocate`'s numerical discard-limit
check instead. H therefore does not depend on queue tuning or kernel version
strings, and does not claim runtime allocation is immutable.

Before reusing a sparse image, root verifies ownership, permissions, link count,
device/inode and exact length, takes the image flock, and requires both the mount
and every matching loop to be gone. It checks the existing full logical charges
against total capacity and the missing allocation against available host space
plus the normal reserve, without charging the image twice. Mode-0 `fallocate`,
file sync and the allocation check then restore it before attach; a fresh capacity
check also verifies that host reserve remains after allocation. The shared
image-open path also provides this repair to readiness, idempotent provision and
resize preflight. A live or pinned sparse image returns `Busy`: B1 must drain it,
detach and retry. Identity/alias/length mismatches are still refused, never repaired.

The trade-off is that a mounted image may temporarily release host blocks. H
does not reserve those blocks independently while it is mounted; competing host
writes can still cause ENOSPC during employee activity. B1/operators must retain
host headroom. At reattachment, insufficient headroom returns `Capacity` without
mounting or permanently quarantining an otherwise unchanged image; it can be
retried after capacity is restored. Other refill failures also refuse attachment
and retain the full charge. This is allocation repair only, never filesystem
repair. During resize, every allocation/sync/verification/detach failure after
reservation quarantines the full target; interruptions keep `Resizing`, including
between kernel growth and reallocation. No root resize2fs/fsck is introduced.

The ignored `iso_root_runtime_hole_punch_refills_before_reattach` test deliberately
enables filesystem discard, punches a file inside the mounted image, checks that
live repair is refused, then detaches/reopens/attaches and checks full allocation
and retained/zeroed data. Resize tests also inspect allocation before any repair
on reopen and cover interruption after reallocation. These new root cases require
VM acceptance on the target kernels; compilation is not runtime verification.
`iso_root_resize_allocation_failure_quarantines_full_target` injects a length
change after growth to verify refusal and durable quarantine of the full target.

Metadata is bounded JSON, temp-write + fsync + rename + directory fsync. Pending
creation, resize, deletion and quarantine remain charged at their maximum capacity after
a crash. Incomplete creation is never automatically formatted or attached;
`destroy` can remove it after verifying it is unmounted. Corrupt images are
quarantined; no automatic root fsck runs. Unexpected files or mount/device state
fail closed. Metadata cannot recover employee data or replace a backup.

```sh
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo check -p duduclaw-isolation-storage --tests --target x86_64-unknown-linux-gnu --offline
# On an isolated root-capable ext4/XFS host only; ISO_STORAGE_TEST_ROOT must be
# an existing empty root:root 0700 directory on a PRIVATE mount tree:
unshare --mount --propagation private env ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test cargo test -p duduclaw-isolation-storage --offline -- --ignored iso_root_ --test-threads=1
```

See [implementation notes](../../docs/architecture/native-linux-isolation.md).

## License

Apache-2.0, as declared by the workspace.
