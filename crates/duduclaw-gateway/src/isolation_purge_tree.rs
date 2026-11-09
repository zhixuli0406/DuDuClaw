//! Descriptor-relative removal of one quarantine tree (F6 design r5 §3.8,
//! task F6i; child module of `isolation_purge`, Unix only).
//!
//! The tree is removed through descriptors held from the private quarantine
//! root down, exactly like the seal walk reads it (`isolation_quarantine::walk`):
//! every child is `fstatat(AT_SYMLINK_NOFOLLOW)`ed relative to its parent's
//! descriptor, a directory is opened `O_NOFOLLOW | O_DIRECTORY` relative to it
//! and must be the inode just stat'ed, and every entry is removed with
//! `unlinkat` on that parent. A symlink is unlinked, never followed (its
//! target is untouched); nothing on another filesystem or another mount (a
//! mount inside the tree, including a same-device bind mount: Linux
//! `STATX_MNT_ID` per opened directory) is entered or removed. The opened
//! root descriptor must be the recorded root itself. Names are raw bytes
//! (no UTF-8 assumption).
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

use nix::errno::Errno;
use nix::fcntl::{openat, AtFlags, OFlag};
use nix::sys::stat::{fstatat, FileStat, Mode, SFlag};
use nix::unistd::{unlinkat, UnlinkatFlags};

use super::Removed;

/// Deepest directory nesting removed; deeper refuses (retried after an
/// operator looks), so the recursion and the open descriptors stay bounded.
pub(crate) const MAX_DEPTH: usize = 128;
/// Most entries one removal visits (a same-uid writer cannot keep it busy forever).
pub(crate) const MAX_ENTRIES: u64 = 4_000_000;

/// What the row's quarantine name must hold for the removal to proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Expect {
    /// The directory moved at the tombstone, by its recorded `(dev, ino)`.
    Root((u64, u64)),
    /// Any real directory on the quarantine filesystem (an `invalid` row,
    /// whose root may itself be the reason it is invalid).
    AnyDir,
    /// Nothing (an `absent` row): anything present is foreign.
    Nothing,
}

/// Why a removal stopped. Nothing is ever followed, whatever the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreeError {
    /// What sits at the row's name is not what the row recorded (a symlink, a
    /// file, another inode, another filesystem): refused, alerted, operator only.
    Refused(&'static str),
    /// I/O failure, a race, or a bound reached: the purge stays `purging`
    /// and the next call (or boot) retries.
    Io,
}

fn kind(st: &FileStat) -> SFlag { SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits()) }

#[allow(clippy::unnecessary_cast)]
fn identity(st: &FileStat) -> (u64, u64) { (st.st_dev as u64, st.st_ino as u64) }

fn stat_child(dir: &OwnedFd, name: &[u8]) -> Result<Option<FileStat>, TreeError> {
    match fstatat(Some(dir.as_raw_fd()), std::ffi::OsStr::from_bytes(name), AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(st) => Ok(Some(st)),
        Err(Errno::ENOENT) => Ok(None),
        Err(_) => Err(TreeError::Io),
    }
}

fn open_dir(dir: &OwnedFd, name: &[u8]) -> Result<OwnedFd, TreeError> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
    let raw = openat(Some(dir.as_raw_fd()), std::ffi::OsStr::from_bytes(name), flags, Mode::empty()).map_err(|_| TreeError::Io)?;
    // SAFETY: freshly returned descriptor, owned only here.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// F6 b3 N3: the kernel mount id of `name` under `dir` (empty `name`: of
/// `dir` itself), never following a final symlink. A same-device bind mount
/// inside the tree has the device of the tree but another mount id. Linux
/// (`statx` `STATX_MNT_ID`, 5.8+, which protected mode already requires —
/// `duduclaw-run` probe): a kernel that cannot report it is `Io` (fail
/// closed, never "same mount"). Elsewhere `None`: no same-device bind
/// mounts there, the device check stands alone.
#[cfg(target_os = "linux")]
fn mount_id_at(dir: &OwnedFd, name: &[u8]) -> Result<Option<u64>, TreeError> {
    let path = std::ffi::CString::new(name).map_err(|_| TreeError::Io)?;
    let flags = libc::AT_SYMLINK_NOFOLLOW | if name.is_empty() { libc::AT_EMPTY_PATH } else { 0 };
    // SAFETY: an all-zero `statx` (plain integers) is a valid value.
    let mut st: libc::statx = unsafe { std::mem::zeroed() };
    // Raw syscall: the glibc `statx` wrapper (2.28) is newer than the release
    // toolchain's glibc. The descriptor is held for the call.
    duduclaw_core::linux_syscalls::statx(dir.as_raw_fd(), &path, flags, libc::STATX_MNT_ID, &mut st)
        .map_err(|_| TreeError::Io)?;
    if st.stx_mask & libc::STATX_MNT_ID == 0 { return Err(TreeError::Io); }
    Ok(Some(st.stx_mnt_id))
}

#[cfg(not(target_os = "linux"))]
fn mount_id_at(_: &OwnedFd, _: &[u8]) -> Result<Option<u64>, TreeError> { Ok(None) }

/// F6 b3 N2: the directory opened for the root is the one the row recorded
/// (not only the inode of the second stat — both could be a swap).
fn check_opened_root(expect: Expect, opened: (u64, u64)) -> Result<(), TreeError> {
    match expect {
        Expect::Root(root) if opened != root => Err(TreeError::Refused("root_replaced")),
        Expect::Nothing => Err(TreeError::Refused("unexpected_tree")),
        _ => Ok(()),
    }
}

fn fd_identity(fd: &OwnedFd) -> Result<(u64, u64, u32, u32), TreeError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::File::from(fd.try_clone().map_err(|_| TreeError::Io)?).metadata().map_err(|_| TreeError::Io)?;
    Ok((meta.dev(), meta.ino(), meta.uid(), meta.mode()))
}

/// Raw names of one directory (never `.`/`..`), read through a second handle
/// of the held descriptor.
fn names(dir: &OwnedFd) -> io::Result<Vec<Vec<u8>>> {
    let raw = openat(Some(dir.as_raw_fd()), ".", OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC, Mode::empty())
        .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
    // SAFETY: freshly returned descriptor; ownership moves to the DIR stream.
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let stream = unsafe { libc::fdopendir(owned.into_raw_fd()) };
    if stream.is_null() { return Err(io::Error::last_os_error()); }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` is a valid open stream until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() { break; }
        // SAFETY: `d_name` is NUL-terminated per readdir(3).
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes().to_vec();
        if name != b"." && name != b".." { names.push(name); }
    }
    // SAFETY: `stream` came from `fdopendir` and is closed exactly once.
    unsafe { libc::closedir(stream) };
    Ok(names)
}

fn unlink(dir: &OwnedFd, name: &[u8], directory: bool) -> Result<(), TreeError> {
    let flag = if directory { UnlinkatFlags::RemoveDir } else { UnlinkatFlags::NoRemoveDir };
    match unlinkat(Some(dir.as_raw_fd()), std::ffi::OsStr::from_bytes(name), flag) {
        Ok(()) | Err(Errno::ENOENT) => Ok(()),
        Err(_) => Err(TreeError::Io),
    }
}

/// A directory of ours that lacks `u+rwx` (e.g. a read-only `SKILLS/`) is
/// made 0700 through its descriptor so its entries can be unlinked; a
/// directory of another account is left alone (the unlink then fails, `Io`).
fn make_removable(fd: &OwnedFd, euid: u32) -> Result<(), TreeError> {
    use std::os::unix::fs::PermissionsExt;
    let (_, _, uid, mode) = fd_identity(fd)?;
    if uid != euid || mode & 0o700 == 0o700 { return Ok(()); }
    std::fs::File::from(fd.try_clone().map_err(|_| TreeError::Io)?)
        .set_permissions(std::fs::Permissions::from_mode(0o700)).map_err(|_| TreeError::Io)
}

/// Remove every entry under `dir` (the directory itself stays). `dev` is
/// the tree's filesystem and `mount` its mount id; nothing on another
/// filesystem or another mount (a same-device bind mount, N3) is entered
/// or removed.
fn empty_dir(dir: &OwnedFd, dev: u64, mount: Option<u64>, euid: u32, depth: usize, removed: &mut Removed) -> Result<(), TreeError> {
    if depth >= MAX_DEPTH { return Err(TreeError::Io); }
    make_removable(dir, euid)?;
    for name in names(dir).map_err(|_| TreeError::Io)? {
        let Some(st) = stat_child(dir, &name)? else { continue };
        let (entry_dev, entry_ino) = identity(&st);
        if entry_dev != dev { return Err(TreeError::Refused("other_filesystem")); }
        removed.entries = removed.entries.saturating_add(1);
        if removed.entries > MAX_ENTRIES { return Err(TreeError::Io); }
        if kind(&st) == SFlag::S_IFDIR {
            // A directory swapped for a symlink after the stat fails the
            // O_NOFOLLOW open (retried; the link is then unlinked, never followed).
            let child = open_dir(dir, &name)?;
            let (child_dev, child_ino, _, _) = fd_identity(&child)?;
            if (child_dev, child_ino) != (entry_dev, entry_ino) { return Err(TreeError::Io); }
            // Checked on the opened descriptor, before anything under it is touched.
            if mount_id_at(&child, b"")? != mount { return Err(TreeError::Refused("other_mount")); }
            empty_dir(&child, dev, mount, euid, depth + 1, removed)?;
            drop(child);
            unlink(dir, &name, true)?;
        } else {
            // Regular files, symlinks (the link itself) and special files.
            if kind(&st) == SFlag::S_IFREG {
                #[allow(clippy::unnecessary_cast)]
                let size = st.st_size.max(0) as u64;
                removed.file_bytes = removed.file_bytes.saturating_add(size);
            }
            unlink(dir, &name, false)?;
        }
    }
    Ok(())
}

/// Check what `name` under the held quarantine root `parent` is, without
/// changing anything: `Ok(true)` = the expected tree is there, `Ok(false)` =
/// nothing is there; anything else is `Refused`.
pub(crate) fn inspect(parent: &OwnedFd, name: &str, expect: Expect) -> Result<bool, TreeError> {
    let Some(st) = stat_child(parent, name.as_bytes())? else { return Ok(false) };
    if kind(&st) != SFlag::S_IFDIR { return Err(TreeError::Refused("not_a_directory")); }
    let (parent_dev, _, _, _) = fd_identity(parent)?;
    if identity(&st).0 != parent_dev { return Err(TreeError::Refused("other_filesystem")); }
    if mount_id_at(parent, name.as_bytes())? != mount_id_at(parent, b"")? { return Err(TreeError::Refused("other_mount")); }
    check_opened_root(expect, identity(&st)).map(|()| true)
}

/// Remove the tree `name` under the held quarantine root `parent`, then the
/// name itself. `Ok(None)`: nothing was there (already removed; idempotent
/// after a crash). The root directory is not counted in [`Removed`] (the
/// stat manifest does not list it either).
pub(crate) fn remove(parent: &OwnedFd, name: &str, expect: Expect, euid: u32) -> Result<Option<Removed>, TreeError> {
    if !inspect(parent, name, expect)? { return Ok(None); }
    let st = stat_child(parent, name.as_bytes())?.ok_or(TreeError::Io)?;
    let root = open_dir(parent, name.as_bytes())?;
    let (dev, ino, _, _) = fd_identity(&root)?;
    if (dev, ino) != identity(&st) { return Err(TreeError::Io); }
    // N2: the opened descriptor itself is the recorded root, on the root's mount (N3).
    check_opened_root(expect, (dev, ino))?;
    let mount = mount_id_at(&root, b"")?;
    if mount != mount_id_at(parent, b"")? { return Err(TreeError::Refused("other_mount")); }
    let mut removed = Removed::default();
    empty_dir(&root, dev, mount, euid, 0, &mut removed)?;
    drop(root);
    unlink(parent, name.as_bytes(), true)?;
    // The removal is durable only once the quarantine root is synced.
    nix::unistd::fsync(parent.as_raw_fd()).map_err(|_| TreeError::Io)?;
    Ok(Some(removed))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStrExt as _;

    fn held(path: &std::path::Path) -> OwnedFd {
        let raw = openat(None, path, OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC, Mode::empty()).unwrap();
        // SAFETY: freshly returned descriptor, owned only here.
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    /// N2: the opened root descriptor is compared to the recorded root, not
    /// only to the second stat (a swap between the two is refused).
    #[test]
    fn f6b3_opened_root_must_be_recorded_root() {
        assert_eq!(check_opened_root(Expect::Root((1, 2)), (1, 3)), Err(TreeError::Refused("root_replaced")));
        assert_eq!(check_opened_root(Expect::Root((1, 2)), (2, 2)), Err(TreeError::Refused("root_replaced")));
        assert_eq!(check_opened_root(Expect::Root((1, 2)), (1, 2)), Ok(()));
        assert_eq!(check_opened_root(Expect::AnyDir, (5, 6)), Ok(()));
        assert_eq!(check_opened_root(Expect::Nothing, (5, 6)), Err(TreeError::Refused("unexpected_tree")));
        // Through `remove`: a root other than the recorded one is refused, nothing removed.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("row/sub")).unwrap();
        std::fs::write(dir.path().join("row/sub/keep.txt"), "keep").unwrap();
        let parent = held(dir.path());
        assert_eq!(remove(&parent, "row", Expect::Root((0, 0)), nix::unistd::geteuid().as_raw()),
            Err(TreeError::Refused("root_replaced")));
        assert!(dir.path().join("row/sub/keep.txt").exists());
    }

    /// N3: one tree on one mount: every directory reports the same mount id
    /// (Linux), so a plain tree is removed; the bind-mount refusal is the
    /// root-only test below.
    #[test]
    fn f6b3_same_mount_tree_removed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("row/a/b")).unwrap();
        std::fs::write(dir.path().join("row/a/b/f.txt"), "x").unwrap();
        let parent = held(dir.path());
        let inner = held(&dir.path().join("row/a/b"));
        assert_eq!(mount_id_at(&inner, b"").unwrap(), mount_id_at(&parent, b"").unwrap());
        assert_eq!(mount_id_at(&parent, b"row").unwrap(), mount_id_at(&parent, b"").unwrap());
        #[cfg(target_os = "linux")]
        assert!(mount_id_at(&parent, b"").unwrap().is_some(), "Linux reports a mount id");
        drop(inner);
        let removed = remove(&parent, "row", Expect::AnyDir, nix::unistd::geteuid().as_raw()).unwrap().unwrap();
        assert_eq!(removed.entries, 3);
        assert!(!dir.path().join("row").exists());
    }

    /// N3: a same-device bind mount inside the tree is never entered: the
    /// purge refuses before touching the bind source.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root Linux (bind mount); isolated temporary directories only"]
    fn f6b3_bind_mount_inside_tree_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();
        std::fs::create_dir_all(dir.path().join("row/mnt")).unwrap();
        let target = dir.path().join("row/mnt");
        let source_c = std::ffi::CString::new(outside.as_os_str().as_bytes()).unwrap();
        let target_c = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-terminated paths of this test's own directories.
        let mounted = unsafe { libc::mount(source_c.as_ptr(), target_c.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) };
        assert_eq!(mounted, 0, "bind mount");
        let parent = held(dir.path());
        let result = remove(&parent, "row", Expect::AnyDir, nix::unistd::geteuid().as_raw());
        // SAFETY: the mount made above.
        unsafe { libc::umount2(target_c.as_ptr(), libc::MNT_DETACH) };
        assert_eq!(result, Err(TreeError::Refused("other_mount")));
        assert_eq!(std::fs::read_to_string(outside.join("keep.txt")).unwrap(), "keep", "the bind source is untouched");
    }
}
