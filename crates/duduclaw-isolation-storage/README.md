# duduclaw-isolation-storage

Root-side storage for native Linux employee isolation: fixed-size, fully allocated
ext4 loop images bound to immutable employee IDs. This library is not wired into
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

B1 can use the object-safe, root-local `EmployeeStorage: Send` seam, implemented
by `Storage` on every platform (all methods refuse on non-Linux). It is not a
wire RPC, admission decision, or permission to hand a path to an employee.

Byte and inode limits use the shared `DiskQuota::LoopFilesystem`. Project quota
is explicitly unsupported. `resize(employee, target)` grows an unmounted image
with monotonic byte/inode ceilings. B1 must first drain runs and release all
mount clones. H checks capacity for the delta, attaches the old size in its private
mount tree, durably charges the full target as `Resizing`, fully allocates the
image, updates loop capacity, and calls the kernel ext4 grow ioctl. It reads back
mount limits and the superblock, unmounts, then publishes `Ready`. Actual inode
capacity depends on ext4 block-group rounding; a target ceiling too small for
the result quarantines it. No resize2fs/fsck runs. Any failure after reservation
keeps the target charged and blocks attachment; process interruption leaves
`Resizing`, normal failures quarantine. Operator recovery or explicit destruction
is required; no automatic shrink/rollback can truncate data. Shrink and inode-only
changes are deliberately refused. Identical healthy quotas are a no-op.

Metadata is bounded JSON, temp-write + fsync + rename + directory fsync. Pending
creation, resize, deletion and quarantine remain charged at their maximum capacity after
a crash. Incomplete creation is never automatically formatted or attached;
`destroy` can remove it after verifying it is unmounted. Corrupt images are
quarantined; no automatic root fsck runs. Unexpected files or mount/device state
fail closed. Metadata cannot recover employee data or replace a backup.

```sh
CARGO_BUILD_JOBS=4 CARGO_INCREMENTAL=0 cargo test -p duduclaw-isolation-storage --offline
cargo check --target x86_64-unknown-linux-gnu -p duduclaw-isolation-storage --offline
# On an isolated root-capable ext4/XFS host only; ISO_STORAGE_TEST_ROOT must be
# an existing empty root:root 0700 directory on a PRIVATE mount tree:
unshare --mount --propagation private env ISO_STORAGE_TEST_ROOT=/var/lib/iso-storage-test cargo test -p duduclaw-isolation-storage --offline -- --ignored iso_root_ --test-threads=1
```

See [implementation notes](../../docs/architecture/native-linux-isolation.md).

## License

Apache-2.0, as declared by the workspace.
