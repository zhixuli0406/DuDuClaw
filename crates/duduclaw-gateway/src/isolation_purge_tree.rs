//! Descriptor-relative removal of one quarantine tree (F6 design r5 §3.8,
//! task F6i; child module of `isolation_purge`, Unix only).
//!
//! The tree is removed through descriptors held from the private quarantine
//! root down, exactly like the seal walk reads it (`isolation_quarantine::walk`):
//! every child is `fstatat(AT_SYMLINK_NOFOLLOW)`ed relative to its parent's
//! descriptor, a directory is opened `O_NOFOLLOW | O_DIRECTORY` relative to it
//! and must be the inode just stat'ed, and every entry is removed with
//! `unlinkat` on that parent. A symlink is unlinked, never followed (its
//! target is untouched); nothing on another filesystem (a mount inside the
//! tree) is entered or removed. Names are raw bytes (no UTF-8 assumption).
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
/// the tree's filesystem; nothing on another one is entered or removed.
fn empty_dir(dir: &OwnedFd, dev: u64, euid: u32, depth: usize, removed: &mut Removed) -> Result<(), TreeError> {
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
            empty_dir(&child, dev, euid, depth + 1, removed)?;
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
    match expect {
        Expect::Root(root) if identity(&st) != root => Err(TreeError::Refused("root_replaced")),
        Expect::Nothing => Err(TreeError::Refused("unexpected_tree")),
        _ => Ok(true),
    }
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
    let mut removed = Removed::default();
    empty_dir(&root, dev, euid, 0, &mut removed)?;
    drop(root);
    unlink(parent, name.as_bytes(), true)?;
    // The removal is durable only once the quarantine root is synced.
    nix::unistd::fsync(parent.as_raw_fd()).map_err(|_| TreeError::Io)?;
    Ok(Some(removed))
}
