//! Symlink-safe file operations inside a directory another party can write.
//!
//! Shared by the Antigravity runtime (`<work_root>/.agents/mcp_config.json`,
//! written into an agent-writable tree) and the computer-use workspaces
//! (`<home>/computer_workspaces/<id>/data/`, mounted read-only into a
//! container). In both a planted symlink must never redirect the gateway's
//! read, write or delete outside the tree.
//!
//! Unix: every step after the trusted root is fd-relative (`openat` /
//! `mkdirat` / `renameat` / `unlinkat`) with `O_NOFOLLOW`, so a symlinked
//! intermediate directory or target is refused by the kernel and a swap
//! between "check" and "use" cannot redirect the operation. Temp files get a
//! random name and `O_CREAT|O_EXCL|O_NOFOLLOW`.
//!
//! Other platforms: best-effort path checks with `symlink_metadata` plus a
//! canonical-containment check (no fd-relative API in std).

use std::io;
use std::path::Path;

pub(crate) fn refused(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.into())
}

/// Reject anything that is not a single, plain path component.
fn check_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(refused(format!("invalid path component {name:?}")));
    }
    Ok(())
}

/// A fresh random temp name for `name` (always starts with `.`).
pub(crate) fn tmp_name(name: &str) -> String {
    format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple())
}

/// One step of [`SafeDir::write_bytes_atomic_with`], for fault injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteStep {
    /// After the temp file was created, before its bytes are written.
    Write,
    /// After the bytes were written, before `sync_all`.
    Sync,
    /// After `sync_all`, before the `renameat`.
    Rename,
    /// After the `renameat` and the directory `fsync`. The file has landed:
    /// an error returned here is ignored (it is a pause point for tests).
    AfterRename,
}

/// What a directory entry is (never followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Dir,
    /// A symlink, socket, fifo or device.
    Other,
}

/// One listed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryInfo {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Hard-link count (1 on platforms that do not report it).
    pub nlink: u64,
}

fn no_fault(_: WriteStep) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
mod imp {
    use super::{EntryInfo, EntryKind, WriteStep, check_name, refused, tmp_name};
    use nix::errno::Errno;
    use nix::fcntl::{OFlag, openat, renameat};
    use nix::sys::stat::{FileStat, Mode, SFlag, fstatat, mkdirat};
    use nix::unistd::{UnlinkatFlags, unlinkat};
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::path::Path;

    fn to_io(e: Errno) -> io::Error {
        io::Error::from_raw_os_error(e as i32)
    }

    fn kind(st: &FileStat) -> SFlag {
        SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits())
    }

    fn entry_kind(st: &FileStat) -> EntryKind {
        match kind(st) {
            SFlag::S_IFREG => EntryKind::File,
            SFlag::S_IFDIR => EntryKind::Dir,
            _ => EntryKind::Other,
        }
    }

    /// An open directory handle; every operation is relative to it.
    pub(crate) struct SafeDir {
        fd: OwnedFd,
    }

    impl SafeDir {
        /// Open a trusted root chosen by the gateway (symlinks in the root path
        /// itself are followed: the root is not agent-planted).
        pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
            let raw = openat(
                None,
                path,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(to_io)?;
            // SAFETY: `openat` just returned this fd and nothing else owns it.
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(raw) },
            })
        }

        fn stat(&self, name: &str) -> io::Result<Option<FileStat>> {
            match fstatat(
                Some(self.fd.as_raw_fd()),
                name,
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            ) {
                Ok(st) => Ok(Some(st)),
                Err(Errno::ENOENT) => Ok(None),
                Err(e) => Err(to_io(e)),
            }
        }

        /// What `name` is, without following it. `Ok(None)` when absent.
        pub(crate) fn entry(&self, name: &str) -> io::Result<Option<EntryInfo>> {
            check_name(name)?;
            Ok(self.stat(name)?.map(|st| EntryInfo {
                name: name.to_string(),
                kind: entry_kind(&st),
                size: st.st_size.max(0) as u64,
                nlink: st.st_nlink as u64,
            }))
        }

        /// Open child directory `name` without following a symlink. `create`
        /// makes it (0700) when absent. `Ok(None)` when absent and not created.
        pub(crate) fn child_dir(&self, name: &str, create: bool) -> io::Result<Option<SafeDir>> {
            check_name(name)?;
            match self.stat(name)? {
                None if create => match mkdirat(Some(self.fd.as_raw_fd()), name, Mode::S_IRWXU) {
                    // The new entry is durable only once its parent is synced.
                    Ok(()) => self.sync_dir(),
                    Err(Errno::EEXIST) => {}
                    Err(e) => return Err(to_io(e)),
                },
                None => return Ok(None),
                Some(st) if kind(&st) == SFlag::S_IFLNK => {
                    return Err(refused(format!(
                        "{name} is a symlink — refusing to follow it"
                    )));
                }
                Some(st) if kind(&st) != SFlag::S_IFDIR => {
                    return Err(refused(format!("{name} is not a directory")));
                }
                Some(_) => {}
            }
            let raw = openat(
                Some(self.fd.as_raw_fd()),
                name,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| match e {
                Errno::ELOOP | Errno::ENOTDIR => refused(format!(
                    "{name} is not a real directory — refusing to follow it"
                )),
                other => to_io(other),
            })?;
            // SAFETY: freshly returned fd, owned by nobody else.
            Ok(Some(SafeDir {
                fd: unsafe { OwnedFd::from_raw_fd(raw) },
            }))
        }

        /// Read regular file `name` as bytes, refusing one larger than
        /// `max_bytes`. `Ok(None)` when absent; a symlink or a non-regular
        /// file is refused.
        pub(crate) fn read_bytes(&self, name: &str, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
            self.read_bytes_checked(name, max_bytes, false)
        }

        /// [`Self::read_bytes`]; `single_link` also refuses a file with more
        /// than one hard link (checked on the opened descriptor).
        pub(crate) fn read_bytes_checked(
            &self,
            name: &str,
            max_bytes: u64,
            single_link: bool,
        ) -> io::Result<Option<Vec<u8>>> {
            check_name(name)?;
            match self.stat(name)? {
                None => return Ok(None),
                Some(st) if kind(&st) != SFlag::S_IFREG => {
                    return Err(refused(format!(
                        "{name} is not a regular file (symlink or special) — refusing to read it"
                    )));
                }
                Some(_) => {}
            }
            let raw = match openat(
                Some(self.fd.as_raw_fd()),
                name,
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(Errno::ENOENT) => return Ok(None),
                Err(Errno::ELOOP) => {
                    return Err(refused(format!(
                        "{name} is a symlink — refusing to read it"
                    )));
                }
                Err(e) => return Err(to_io(e)),
            };
            // SAFETY: freshly returned fd, owned by nobody else.
            let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(refused(format!("{name} is not a regular file")));
            }
            if single_link && std::os::unix::fs::MetadataExt::nlink(&meta) > 1 {
                return Err(refused(format!("{name} has several hard links")));
            }
            if meta.len() > max_bytes {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
            }
            let mut out = Vec::new();
            file.take(max_bytes.saturating_add(1))
                .read_to_end(&mut out)?;
            if out.len() as u64 > max_bytes {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
            }
            Ok(Some(out))
        }

        /// Read regular file `name` as UTF-8 text (no size cap). `Ok(None)`
        /// when absent; a symlink or a non-regular file is refused.
        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            match self.read_bytes(name, u64::MAX)? {
                None => Ok(None),
                Some(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8")),
            }
        }

        /// Replace regular file `name` atomically through temp file `tmp`
        /// (created with `O_EXCL|O_NOFOLLOW`), then `renameat`. A symlink or
        /// non-regular file at `name` is refused. New files are 0600; an
        /// existing file keeps its permission bits. `fault` runs before each
        /// [`WriteStep`]; an error from it aborts like a real failure. Any
        /// failure removes the temp file and leaves `name` untouched.
        pub(crate) fn write_bytes_atomic_with(
            &self,
            name: &str,
            contents: &[u8],
            tmp: &str,
            fault: &dyn Fn(WriteStep) -> io::Result<()>,
        ) -> io::Result<()> {
            check_name(name)?;
            check_name(tmp)?;
            let mode = match self.stat(name)? {
                None => 0o600,
                Some(st) if kind(&st) != SFlag::S_IFREG => {
                    return Err(refused(format!(
                        "{name} is not a regular file (symlink or special) — refusing to replace it"
                    )));
                }
                Some(st) => (st.st_mode as u32) & 0o7777,
            };
            let raw = openat(
                Some(self.fd.as_raw_fd()),
                tmp,
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(to_io)?;
            // SAFETY: freshly returned fd, owned by nobody else.
            let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            let written = (|| -> io::Result<()> {
                fault(WriteStep::Write)?;
                file.write_all(contents)?;
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
                fault(WriteStep::Sync)?;
                file.sync_all()?;
                fault(WriteStep::Rename)?;
                renameat(
                    Some(self.fd.as_raw_fd()),
                    tmp,
                    Some(self.fd.as_raw_fd()),
                    name,
                )
                .map_err(to_io)
            })();
            if written.is_err() {
                let _ = unlinkat(Some(self.fd.as_raw_fd()), tmp, UnlinkatFlags::NoRemoveDir);
                return written;
            }
            self.sync_dir();
            let _ = fault(WriteStep::AfterRename);
            Ok(())
        }

        /// `fsync` this directory so a rename or new entry in it survives a
        /// crash. The change already happened, so a failure is logged and
        /// not returned (returning it would report a landed write as failed).
        fn sync_dir(&self) {
            if let Err(e) = nix::unistd::fsync(self.fd.as_raw_fd()) {
                tracing::warn!(error = %e, "directory fsync failed after a rename or mkdir");
            }
        }

        /// [`Self::write_bytes_atomic_with`] with a fresh random temp name
        /// and no fault injection.
        pub(crate) fn write_file_atomic(&self, name: &str, contents: &str) -> io::Result<()> {
            self.write_bytes_atomic_with(
                name,
                contents.as_bytes(),
                &tmp_name(name),
                &super::no_fault,
            )
        }

        /// Remove regular file `name` (the directory entry, never a target).
        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            check_name(name)?;
            unlinkat(Some(self.fd.as_raw_fd()), name, UnlinkatFlags::NoRemoveDir).map_err(to_io)
        }

        /// Every entry of this directory except `.` and `..`, never followed.
        pub(crate) fn list(&self) -> io::Result<Vec<EntryInfo>> {
            // A second handle on the same directory for `fdopendir`, which
            // takes ownership of it (closed by `closedir`).
            let raw = openat(
                Some(self.fd.as_raw_fd()),
                ".",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(to_io)?;
            // SAFETY: freshly returned fd; ownership moves to the DIR stream.
            let owned = unsafe { OwnedFd::from_raw_fd(raw) };
            let dir = unsafe { libc::fdopendir(owned.into_raw_fd()) };
            if dir.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut names = Vec::new();
            loop {
                // SAFETY: `dir` is a valid open stream until `closedir` below.
                let ent = unsafe { libc::readdir(dir) };
                if ent.is_null() {
                    break;
                }
                // SAFETY: `d_name` is NUL-terminated per readdir(3).
                let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
                names.push(name.to_bytes().to_vec());
            }
            // SAFETY: `dir` came from `fdopendir` and is closed exactly once.
            unsafe { libc::closedir(dir) };
            let mut out = Vec::new();
            for raw_name in names {
                if raw_name == b"." || raw_name == b".." {
                    continue;
                }
                // A name that is not UTF-8 or not a plain component (it may
                // hold `\\`), or one that cannot be stat'ed, is reported as an
                // unnamed `Other` entry: it never fails the whole listing.
                let unnamed = EntryInfo {
                    name: String::new(),
                    kind: EntryKind::Other,
                    size: 0,
                    nlink: 1,
                };
                let Ok(name) = String::from_utf8(raw_name) else {
                    out.push(unnamed);
                    continue;
                };
                match self.entry(&name) {
                    Ok(Some(info)) => out.push(info),
                    Ok(None) => {}
                    Err(_) => out.push(unnamed),
                }
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{EntryInfo, EntryKind, WriteStep, check_name, refused, tmp_name};
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};

    /// Path-based fallback: checks with `symlink_metadata` before every use
    /// and verifies canonical containment in the root.
    pub(crate) struct SafeDir {
        path: PathBuf,
        root: PathBuf,
    }

    fn info(name: &str, m: &std::fs::Metadata) -> EntryInfo {
        let kind = if m.file_type().is_symlink() {
            EntryKind::Other
        } else if m.is_file() {
            EntryKind::File
        } else if m.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::Other
        };
        EntryInfo {
            name: name.to_string(),
            kind,
            size: m.len(),
            nlink: 1,
        }
    }

    impl SafeDir {
        pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
            let canon = path.canonicalize()?;
            if !canon.is_dir() {
                return Err(refused("root is not a directory"));
            }
            Ok(Self {
                path: canon.clone(),
                root: canon,
            })
        }

        fn meta(&self, name: &str) -> io::Result<Option<std::fs::Metadata>> {
            match std::fs::symlink_metadata(self.path.join(name)) {
                Ok(m) => Ok(Some(m)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }

        pub(crate) fn entry(&self, name: &str) -> io::Result<Option<EntryInfo>> {
            check_name(name)?;
            Ok(self.meta(name)?.map(|m| info(name, &m)))
        }

        pub(crate) fn child_dir(&self, name: &str, create: bool) -> io::Result<Option<SafeDir>> {
            check_name(name)?;
            let p = self.path.join(name);
            match self.meta(name)? {
                None if create => std::fs::create_dir(&p).or_else(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })?,
                None => return Ok(None),
                Some(m) if m.file_type().is_symlink() || !m.is_dir() => {
                    return Err(refused(format!("{name} is not a real directory")));
                }
                Some(_) => {}
            }
            let m = std::fs::symlink_metadata(&p)?;
            if m.file_type().is_symlink() || !m.is_dir() {
                return Err(refused(format!("{name} is not a real directory")));
            }
            let canon = p.canonicalize()?;
            if !canon.starts_with(&self.root) {
                return Err(refused(format!("{name} resolves outside the root")));
            }
            Ok(Some(SafeDir {
                path: canon,
                root: self.root.clone(),
            }))
        }

        pub(crate) fn read_bytes(&self, name: &str, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
            self.read_bytes_checked(name, max_bytes, false)
        }

        pub(crate) fn read_bytes_checked(
            &self,
            name: &str,
            max_bytes: u64,
            _single_link: bool,
        ) -> io::Result<Option<Vec<u8>>> {
            check_name(name)?;
            match self.meta(name)? {
                None => Ok(None),
                Some(m) if m.file_type().is_symlink() || !m.is_file() => {
                    Err(refused(format!("{name} is not a regular file")))
                }
                Some(m) if m.len() > max_bytes => {
                    Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"))
                }
                Some(_) => {
                    let mut out = Vec::new();
                    std::fs::File::open(self.path.join(name))?
                        .take(max_bytes.saturating_add(1))
                        .read_to_end(&mut out)?;
                    if out.len() as u64 > max_bytes {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
                    }
                    Ok(Some(out))
                }
            }
        }

        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            match self.read_bytes(name, u64::MAX)? {
                None => Ok(None),
                Some(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8")),
            }
        }

        pub(crate) fn write_bytes_atomic_with(
            &self,
            name: &str,
            contents: &[u8],
            tmp: &str,
            fault: &dyn Fn(WriteStep) -> io::Result<()>,
        ) -> io::Result<()> {
            check_name(name)?;
            check_name(tmp)?;
            let perms = match self.meta(name)? {
                None => None,
                Some(m) if m.file_type().is_symlink() || !m.is_file() => {
                    return Err(refused(format!("{name} is not a regular file")));
                }
                Some(m) => Some(m.permissions()),
            };
            let tmp = self.path.join(tmp);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            let res = (|| -> io::Result<()> {
                fault(WriteStep::Write)?;
                f.write_all(contents)?;
                if let Some(p) = perms {
                    std::fs::set_permissions(&tmp, p)?;
                }
                fault(WriteStep::Sync)?;
                f.sync_all()?;
                fault(WriteStep::Rename)?;
                std::fs::rename(&tmp, self.path.join(name))
            })();
            if res.is_err() {
                let _ = std::fs::remove_file(&tmp);
                return res;
            }
            let _ = fault(WriteStep::AfterRename);
            Ok(())
        }

        pub(crate) fn write_file_atomic(&self, name: &str, contents: &str) -> io::Result<()> {
            self.write_bytes_atomic_with(
                name,
                contents.as_bytes(),
                &tmp_name(name),
                &super::no_fault,
            )
        }

        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            check_name(name)?;
            std::fs::remove_file(self.path.join(name))
        }

        pub(crate) fn list(&self) -> io::Result<Vec<EntryInfo>> {
            let mut out = Vec::new();
            for entry in std::fs::read_dir(&self.path)? {
                let Ok(entry) = entry else { continue };
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    out.push(EntryInfo {
                        name: String::new(),
                        kind: EntryKind::Other,
                        size: 0,
                        nlink: 1,
                    });
                    continue;
                };
                match std::fs::symlink_metadata(entry.path()) {
                    Ok(m) => out.push(info(&name, &m)),
                    Err(_) => out.push(EntryInfo {
                        name: String::new(),
                        kind: EntryKind::Other,
                        size: 0,
                        nlink: 1,
                    }),
                }
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        }
    }
}

pub(crate) use imp::SafeDir;

impl SafeDir {
    /// Walk `components` from this directory, never following a symlink.
    /// `create` makes missing directories (0700). `Ok(None)` when a component
    /// is absent and `create` is false.
    pub(crate) fn open_path(
        &self,
        components: &[&str],
        create: bool,
    ) -> io::Result<Option<SafeDir>> {
        let mut current: Option<SafeDir> = None;
        for part in components {
            let next = match &current {
                None => self.child_dir(part, create)?,
                Some(dir) => dir.child_dir(part, create)?,
            };
            match next {
                Some(dir) => current = Some(dir),
                None => return Ok(None),
            }
        }
        match current {
            Some(dir) => Ok(Some(dir)),
            None => Err(refused("empty path")),
        }
    }
}

/// Convenience: is `path` itself a symlink (without following it)?
pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn symlinked_child_dir_is_refused_and_nothing_is_created_outside() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(".agents")).unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        assert!(dir.child_dir(".agents", true).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[test]
    fn symlinked_file_is_neither_read_nor_replaced() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.json");
        std::fs::write(&victim, "SECRET").unwrap();
        symlink(&victim, root.path().join("f.json")).unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        assert!(dir.read_file("f.json").is_err());
        assert!(dir.write_file_atomic("f.json", "{}").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "SECRET");
        assert!(is_symlink(&root.path().join("f.json")));
    }

    #[test]
    fn writes_are_atomic_new_files_0600_existing_mode_kept() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        dir.write_file_atomic("a.json", "1").unwrap();
        let p = root.path().join("a.json");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "1");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        dir.write_file_atomic("a.json", "2").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "2");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o644
        );
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_planted_predictable_temp_name_is_not_followed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim");
        std::fs::write(&victim, "KEEP").unwrap();
        // Plant symlinks at every name the old code would have used.
        for name in [
            format!("a.json.tmp-{}", std::process::id()),
            "a.json.tmp".to_string(),
            ".a.json.tmp".to_string(),
        ] {
            symlink(&victim, root.path().join(name)).unwrap();
        }
        let dir = SafeDir::open_root(root.path()).unwrap();
        dir.write_file_atomic("a.json", "new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "KEEP");
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.json")).unwrap(),
            "new"
        );
    }

    #[test]
    fn invalid_names_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        for bad in ["", ".", "..", "a/b", "../x"] {
            assert!(dir.read_file(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn each_injected_fault_keeps_the_old_file_and_leaves_no_temp() {
        let root = tempfile::tempdir().unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        dir.write_file_atomic("f", "old").unwrap();
        for step in [WriteStep::Write, WriteStep::Sync, WriteStep::Rename] {
            let fault = move |s: WriteStep| {
                if s == step {
                    Err(std::io::Error::from_raw_os_error(28)) // ENOSPC
                } else {
                    Ok(())
                }
            };
            assert!(
                dir.write_bytes_atomic_with("f", b"new", &tmp_name("f"), &fault)
                    .is_err()
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("f")).unwrap(),
                "old"
            );
            assert_eq!(
                std::fs::read_dir(root.path()).unwrap().count(),
                1,
                "{step:?}"
            );
        }
    }

    #[test]
    fn list_and_capped_read_never_follow_links() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("s"), "SECRET").unwrap();
        std::fs::write(root.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        symlink(outside.path().join("s"), root.path().join("link")).unwrap();
        let dir = SafeDir::open_root(root.path()).unwrap();
        let listed = dir.list().unwrap();
        let kinds: Vec<_> = listed.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("a.txt", EntryKind::File),
                ("link", EntryKind::Other),
                ("sub", EntryKind::Dir)
            ]
        );
        assert_eq!(dir.read_bytes("a.txt", 5).unwrap().unwrap(), b"hello");
        assert!(dir.read_bytes("a.txt", 4).is_err());
        assert!(dir.read_bytes("link", 100).is_err());
        assert!(dir.open_path(&["sub"], false).unwrap().is_some());
        assert!(dir.open_path(&["link"], false).is_err());
        assert!(dir.open_path(&["nope", "x"], false).unwrap().is_none());
    }
}
