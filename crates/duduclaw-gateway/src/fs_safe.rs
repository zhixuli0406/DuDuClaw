//! Symlink-safe file operations inside a directory another party can write.
//!
//! Shared by the Grok runtime (`<work_root>/.grok/config.toml`,
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
//! Windows: open children relative to pinned directory handles with NtCreateFile;
//! enumerate, rename and delete by handle. Reject reparse points on opened handles.
//! Directory handles deny delete sharing; write sharing is needed by native rename.
//! Other platforms retain best-effort path checks and canonical containment.

use std::io;

#[path = "fs_safe/sqlite.rs"]
pub(crate) mod sqlite;

/// Test homes must satisfy the same ownership/mode contract as production.
/// tempfile's default directory mode is 0777 & !umask, not necessarily 0700.
#[cfg(test)]
pub(crate) fn private_test_home() -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    // Resolve only the OS-selected temporary root (e.g. macOS /var), never
    // a production database path or an untrusted path supplied by a caller.
    builder.tempdir_in(std::env::temp_dir().canonicalize()?)
}

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
    #[cfg(windows)]
    {
        let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ').to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| stem.strip_prefix(prefix).is_some_and(|suffix|
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")));
        if name.contains([':', '<', '>', '"', '|', '?', '*']) || name.chars().any(|c| c.is_ascii_control())
            || name.ends_with(['.', ' ']) || device {
            return Err(refused(format!("invalid Windows path component {name:?}")));
        }
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

    #[cfg(target_os = "macos")]
    fn macos_system_path(path: &Path) -> io::Result<std::borrow::Cow<'_, Path>> {
        use std::os::unix::fs::MetadataExt;
        for (alias, target) in [("/var", "/private/var"), ("/tmp", "/private/tmp"), ("/etc", "/private/etc")] {
            if let Ok(tail) = path.strip_prefix(alias) {
                let metadata = std::fs::symlink_metadata(alias)?;
                if metadata.file_type().is_symlink() && metadata.uid() == 0
                    && Path::new("/").join(std::fs::read_link(alias)?) == Path::new(target) {
                    return Ok(std::borrow::Cow::Owned(Path::new(target).join(tail)));
                }
                break;
            }
        }
        Ok(std::borrow::Cow::Borrowed(path))
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

        /// Walk from the filesystem root without resolving any symlink. Unlike
        /// open_root, this is for paths whose parent components are untrusted.
        pub(crate) fn open_nofollow(path: &Path, create: bool) -> io::Result<Self> {
            use std::path::Component;
            let absolute = if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) };
            // Resolve only macOS root-owned system aliases; the target and
            // entire suffix still use O_NOFOLLOW, never untrusted symlinks.
            #[cfg(target_os = "macos")]
            let absolute = macos_system_path(&absolute)?;
            let mut dir = Self::open_root(Path::new("/"))?;
            for component in absolute.components() {
                match component {
                    Component::RootDir => {}
                    Component::Normal(name) => {
                        let name = name.to_str().ok_or_else(|| refused("non-UTF-8 directory"))?;
                        dir = dir.child_dir(name, create)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
                    }
                    _ => return Err(refused("invalid directory component")),
                }
            }
            Ok(dir)
        }

        pub(crate) fn metadata(&self) -> io::Result<std::fs::Metadata> {
            // Duplicate the descriptor; never reopen the directory by name.
            std::fs::File::from(self.fd.try_clone()?).metadata()
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
            let Some(file) = self.open_read_checked(name, single_link)? else { return Ok(None) };
            let meta = file.metadata()?;
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

        /// Return the already-open, no-follow descriptor. Callers must stream
        /// this handle, never reopen a checked path.
        pub(crate) fn open_read_checked(&self, name: &str, single_link: bool) -> io::Result<Option<std::fs::File>> {
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
            Ok(Some(file))
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

        /// Create a new 0600 file relative to this pinned directory. The caller
        /// owns the descriptor and may hand it to an asynchronous writer.
        pub(crate) fn open_create_new(&self, name: &str) -> io::Result<std::fs::File> {
            check_name(name)?;
            let raw = openat(
                Some(self.fd.as_raw_fd()), name,
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::S_IRUSR | Mode::S_IWUSR,
            ).map_err(to_io)?;
            // SAFETY: openat returned a fresh descriptor owned only here.
            Ok(unsafe { std::fs::File::from_raw_fd(raw) })
        }

        /// Create only: never overwrite an operator file or follow a link.
        pub(crate) fn create_file_new(&self, name: &str, contents: &[u8]) -> io::Result<bool> {
            let mut file = match self.open_create_new(name) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    return match self.entry(name)? {
                        Some(entry) if entry.kind == EntryKind::File && entry.nlink == 1 => Ok(false),
                        _ => Err(refused("existing file is not a regular, single-link file")),
                    };
                }
                Err(e) => return Err(e),
            };
            file.write_all(contents)?;
            file.sync_all()?;
            self.sync_dir();
            Ok(true)
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

#[cfg(windows)]
mod imp {
    use super::{EntryInfo, EntryKind, WriteStep, check_name, refused, tmp_name};
    use std::fs::{File, Metadata, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::windows::{fs::{MetadataExt, OpenOptionsExt}, io::{AsRawHandle, FromRawHandle}};
    use std::path::{Component, Path, PathBuf, Prefix};
    use std::sync::{Arc, Mutex};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{NtCreateFile, NtSetInformationFile,
        FILE_CREATE, FILE_OPEN, FILE_OPEN_IF, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FILE_RENAME_INFORMATION, FileRenameInformation};
    use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, UNICODE_STRING, ERROR_NO_MORE_FILES};
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle,
        GetFileInformationByHandleEx, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_ID_BOTH_DIR_INFO, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
        FILE_DISPOSITION_INFO, FileDispositionInfo, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, DELETE, SYNCHRONIZE};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    // Every child access is a single-component NtCreateFile relative to an
    // already opened directory handle. Enumeration and rename use handles too;
    // pathname checks or share flags are not the reparse-race boundary.
    pub(crate) struct SafeDir {
        dir: Arc<File>,
        ancestors: Vec<Arc<File>>,
        enumeration: Mutex<()>,
    }

    fn nt_result(status: i32) -> io::Result<()> {
        if status < 0 {
            return Err(io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32));
        }
        Ok(())
    }

    fn link_count(file: &File) -> io::Result<u64> {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: live handle and writable output buffer; read only after success.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::from(unsafe { info.assume_init() }.nNumberOfLinks))
    }

    fn real_file(file: &File, directory: bool, single_link: bool) -> io::Result<Metadata> {
        let meta = file.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || meta.is_dir() != directory {
            return Err(refused("reparse point or wrong file type"));
        }
        if !directory && !meta.is_file() { return Err(refused("regular file required")); }
        if single_link && link_count(file)? != 1 { return Err(refused("single-link file required")); }
        Ok(meta)
    }

    fn delete(file: &File) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: live handle with DELETE access and a correctly sized input buffer.
        if unsafe { SetFileInformationByHandle(file.as_raw_handle(), FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(), std::mem::size_of_val(&disposition) as u32) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    impl SafeDir {
        pub(crate) fn open_root(path: &Path) -> io::Result<Self> {
            Self::open_nofollow(&path.canonicalize()?, false)
        }

        pub(crate) fn open_nofollow(path: &Path, create: bool) -> io::Result<Self> {
            let absolute = if path.is_absolute() { path.to_owned() } else { std::env::current_dir()?.join(path) };
            let mut parts = absolute.components();
            let prefix = match parts.next() {
                Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => prefix,
                _ => return Err(refused("directory must be on a local drive")),
            };
            if parts.next() != Some(Component::RootDir) { return Err(refused("directory must be absolute")); }
            let mut root = PathBuf::from(prefix.as_os_str());
            root.push(std::path::MAIN_SEPARATOR_STR);
            // Only the drive root is opened by path. Keep no delete sharing.
            // Native rename opens its target directory with FILE_WRITE_DATA, so
            // denying write sharing would break replacement (FILE_RENAME_INFORMATION
            // RootDirectory contract). Handle-relative access is the race boundary.
            let file = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).open(root)?;
            real_file(&file, true, false)?;
            let mut dir = Self { dir: Arc::new(file), ancestors: Vec::new(), enumeration: Mutex::new(()) };
            for part in parts {
                let Component::Normal(name) = part else { return Err(refused("invalid directory component")) };
                let name = name.to_str().ok_or_else(|| refused("non-UTF-8 directory"))?;
                dir = dir.child_dir(name, create)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            }
            Ok(dir)
        }

        pub(crate) fn metadata(&self) -> io::Result<Metadata> { self.dir.metadata() }

        pub(super) fn directory_file(&self) -> &File { &self.dir }

        fn relative(&self, value: &str, access: u32, disposition: u32, options: u32) -> io::Result<File> {
            self.relative_secure(value, access, disposition, options, None)
        }

        // The descriptor is borrowed only for this synchronous NtCreateFile call.
        pub(super) fn relative_secure(&self, value: &str, access: u32, disposition: u32,
            options: u32, security: Option<&windows_sys::Win32::Security::SECURITY_DESCRIPTOR>) -> io::Result<File> {
            check_name(value)?;
            real_file(&self.dir, true, false)?;
            let mut wide: Vec<u16> = value.encode_utf16().collect();
            let length = u16::try_from(wide.len() * 2).map_err(|_| refused("name too long"))?;
            let name = UNICODE_STRING { Length: length, MaximumLength: length, Buffer: wide.as_mut_ptr() };
            let attributes = OBJECT_ATTRIBUTES {
                Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
                RootDirectory: self.dir.as_raw_handle(), ObjectName: &name,
                // entry() must inspect the final reparse point itself. Typed opens
                // also use OBJ_DONT_REPARSE, as in skill_loader::safe_dir.
                Attributes: 0x40 | if options == 0 { 0 } else { 0x1000 },
                SecurityDescriptor: security.map_or(std::ptr::null(), |sd| std::ptr::from_ref(sd).cast()), SecurityQualityOfService: std::ptr::null(),
            };
            let mut handle = std::ptr::null_mut();
            let mut status_block = std::mem::MaybeUninit::<IO_STATUS_BLOCK>::zeroed();
            // SAFETY: live parent, single-component counted UTF-16 name and live
            // output buffers. FILE_OPEN_REPARSE_POINT never follows the leaf.
            let status = unsafe { NtCreateFile(&mut handle, access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                &attributes, status_block.as_mut_ptr(), std::ptr::null(), 0,
                FILE_SHARE_READ | FILE_SHARE_WRITE, // Native rename needs a target-directory writer; never share DELETE.
                disposition, FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT | options,
                std::ptr::null(), 0) };
            nt_result(status)?;
            // SAFETY: NtCreateFile returned a new, uniquely owned successful handle.
            Ok(unsafe { File::from_raw_handle(handle) })
        }

        pub(crate) fn entry(&self, name: &str) -> io::Result<Option<EntryInfo>> {
            let file = match self.relative(name, FILE_READ_ATTRIBUTES, FILE_OPEN, 0) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            let meta = file.metadata()?;
            let kind = if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 { EntryKind::Other }
                else if meta.is_file() { EntryKind::File }
                else if meta.is_dir() { EntryKind::Dir } else { EntryKind::Other };
            Ok(Some(EntryInfo { name: name.to_string(), kind, size: meta.len(), nlink: link_count(&file)? }))
        }

        pub(crate) fn child_dir(&self, name: &str, create: bool) -> io::Result<Option<Self>> {
            let file = match self.relative(name, FILE_GENERIC_READ,
                if create { FILE_OPEN_IF } else { FILE_OPEN }, FILE_DIRECTORY_FILE) {
                Ok(file) => file,
                Err(e) if !create && e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            real_file(&file, true, false)?;
            let mut ancestors = self.ancestors.clone();
            ancestors.push(self.dir.clone());
            Ok(Some(Self { dir: Arc::new(file), ancestors, enumeration: Mutex::new(()) }))
        }

        pub(crate) fn open_read_checked(&self, name: &str, single_link: bool) -> io::Result<Option<File>> {
            let file = match self.relative(name, FILE_GENERIC_READ, FILE_OPEN, FILE_NON_DIRECTORY_FILE) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            real_file(&file, false, single_link)?;
            Ok(Some(file))
        }

        pub(crate) fn read_bytes(&self, name: &str, max_bytes: u64) -> io::Result<Option<Vec<u8>>> {
            self.read_bytes_checked(name, max_bytes, false)
        }

        pub(crate) fn read_bytes_checked(&self, name: &str, max_bytes: u64, single_link: bool) -> io::Result<Option<Vec<u8>>> {
            let Some(file) = self.open_read_checked(name, single_link)? else { return Ok(None) };
            if file.metadata()?.len() > max_bytes { return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large")); }
            let mut out = Vec::new();
            file.take(max_bytes.saturating_add(1)).read_to_end(&mut out)?;
            if out.len() as u64 > max_bytes { return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large")); }
            Ok(Some(out))
        }

        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            self.read_bytes(name, u64::MAX)?.map(|bytes| String::from_utf8(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))).transpose()
        }

        pub(crate) fn open_create_new(&self, name: &str) -> io::Result<File> {
            let file = self.relative(name, FILE_GENERIC_WRITE, FILE_CREATE, FILE_NON_DIRECTORY_FILE)?;
            real_file(&file, false, true)?;
            Ok(file)
        }

        pub(crate) fn create_file_new(&self, name: &str, contents: &[u8]) -> io::Result<bool> {
            let mut file = match self.open_create_new(name) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    return match self.entry(name)? {
                        Some(entry) if entry.kind == EntryKind::File && entry.nlink == 1 => Ok(false),
                        _ => Err(refused("existing file is not a regular, single-link file")),
                    };
                }
                Err(e) => return Err(e),
            };
            file.write_all(contents)?;
            file.sync_all()?;
            Ok(true)
        }

        fn target_permissions(&self, name: &str) -> io::Result<Option<std::fs::Permissions>> {
            let file = match self.relative(name, FILE_READ_ATTRIBUTES, FILE_OPEN, FILE_NON_DIRECTORY_FILE) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            Ok(Some(real_file(&file, false, false)?.permissions()))
        }

        fn rename(&self, file: &File, name: &str) -> io::Result<()> {
            check_name(name)?;
            real_file(&self.dir, true, false)?;
            let wide: Vec<u16> = name.encode_utf16().collect();
            let name_bytes = u32::try_from(wide.len() * 2).map_err(|_| refused("name too long"))?;
            let size = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name_bytes as usize;
            // u64 storage supplies native alignment and room for the variable name.
            let mut storage = vec![0u64; size.div_ceil(8)];
            let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
            // SAFETY: aligned, zeroed buffer large enough for the header and name;
            // the source and target parent handles stay live for the synchronous call.
            unsafe {
                (*rename).Anonymous.ReplaceIfExists = 1;
                (*rename).RootDirectory = self.dir.as_raw_handle();
                (*rename).FileNameLength = name_bytes;
                let target = storage.as_mut_ptr().cast::<u8>()
                    .add(std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName)).cast::<u16>();
                std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
                let mut status = std::mem::MaybeUninit::<IO_STATUS_BLOCK>::zeroed();
                nt_result(NtSetInformationFile(file.as_raw_handle(), status.as_mut_ptr(),
                    rename.cast(), size as u32, FileRenameInformation))
            }
        }

        pub(crate) fn write_bytes_atomic_with(&self, name: &str, contents: &[u8], tmp: &str,
            fault: &dyn Fn(WriteStep) -> io::Result<()>) -> io::Result<()> {
            check_name(name)?;
            check_name(tmp)?;
            let perms = self.target_permissions(name)?;
            let mut file = self.relative(tmp, FILE_GENERIC_WRITE | DELETE, FILE_CREATE, FILE_NON_DIRECTORY_FILE)?;
            let res = (|| -> io::Result<()> {
                real_file(&file, false, true)?;
                fault(WriteStep::Write)?;
                file.write_all(contents)?;
                if let Some(perms) = perms { file.set_permissions(perms)?; }
                fault(WriteStep::Sync)?;
                file.sync_all()?;
                fault(WriteStep::Rename)?;
                // Repeat the no-follow target check immediately before replacement.
                // A later leaf swap cannot redirect the handle-relative rename.
                self.target_permissions(name)?;
                self.rename(&file, name)
            })();
            if res.is_err() {
                // A copied read-only attribute must not prevent deleting our temp.
                if let Ok(meta) = file.metadata() {
                    let mut perms = meta.permissions();
                    if perms.readonly() {
                        perms.set_readonly(false);
                        let _ = file.set_permissions(perms);
                    }
                }
                let _ = delete(&file);
                return res;
            }
            let _ = fault(WriteStep::AfterRename);
            Ok(())
        }

        pub(crate) fn write_file_atomic(&self, name: &str, contents: &str) -> io::Result<()> {
            self.write_bytes_atomic_with(name, contents.as_bytes(), &tmp_name(name), &super::no_fault)
        }

        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            let file = self.relative(name, FILE_READ_ATTRIBUTES | DELETE, FILE_OPEN, FILE_NON_DIRECTORY_FILE)?;
            real_file(&file, false, false)?;
            delete(&file)
        }

        pub(crate) fn list(&self) -> io::Result<Vec<EntryInfo>> {
            // Restart/continuation share a handle cursor, so serialize each full scan.
            let _guard = self.enumeration.lock().map_err(|_| io::Error::other("directory enumeration poisoned"))?;
            real_file(&self.dir, true, false)?;
            let mut storage = vec![0u64; 8192];
            let bytes_len = storage.len() * 8;
            let mut out = Vec::new();
            let mut first = true;
            loop {
                storage.fill(0);
                // SAFETY: live directory and aligned writable enumeration buffer.
                let ok = unsafe { GetFileInformationByHandleEx(self.dir.as_raw_handle(),
                    if first { FileIdBothDirectoryRestartInfo } else { FileIdBothDirectoryInfo },
                    storage.as_mut_ptr().cast(), bytes_len as u32) };
                first = false;
                if ok == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) { break; }
                    return Err(error);
                }
                let mut offset = 0;
                loop {
                    let header = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
                    if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > bytes_len {
                        return Err(io::Error::other("invalid directory enumeration"));
                    }
                    // SAFETY: aligned offset and checked header/name bounds in this buffer.
                    let entry = unsafe { &*(storage.as_ptr().cast::<u8>().add(offset).cast::<FILE_ID_BOTH_DIR_INFO>()) };
                    let length = entry.FileNameLength as usize;
                    if length % 2 != 0 || offset + header + length > bytes_len {
                        return Err(io::Error::other("invalid directory name length"));
                    }
                    let wide = unsafe { std::slice::from_raw_parts(
                        storage.as_ptr().cast::<u8>().add(offset + header).cast::<u16>(), length / 2) };
                    let value = String::from_utf16(wide);
                    if !matches!(value.as_deref(), Ok("." | "..")) {
                        let unnamed = EntryInfo { name: String::new(), kind: EntryKind::Other, size: 0, nlink: 1 };
                        match value {
                            Ok(name) if check_name(&name).is_ok() => {
                                if entry.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                                    out.push(EntryInfo { name, ..unnamed });
                                } else {
                                    match self.entry(&name) {
                                        Ok(Some(info)) => out.push(info),
                                        Ok(None) => {},
                                        Err(_) => out.push(unnamed),
                                    }
                                }
                            }
                            _ => out.push(unnamed),
                        }
                    }
                    let next = entry.NextEntryOffset as usize;
                    if next == 0 { break; }
                    if next < header + length || next % 8 != 0 || offset + next >= bytes_len {
                        return Err(io::Error::other("invalid directory entry offset"));
                    }
                    offset += next;
                }
            }
            out.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(out)
        }
    }
}

#[cfg(not(any(unix, windows)))]
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

        pub(crate) fn metadata(&self) -> io::Result<std::fs::Metadata> { std::fs::metadata(&self.path) }

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
            single_link: bool,
        ) -> io::Result<Option<Vec<u8>>> {
            let file = {
                let _ = single_link;
                check_name(name)?;
                match self.meta(name)? {
                    None => return Ok(None),
                    Some(m) if m.file_type().is_symlink() || !m.is_file() => return Err(refused(format!("{name} is not a regular file"))),
                    Some(_) => std::fs::File::open(self.path.join(name))?,
                }
            };
            if file.metadata()?.len() > max_bytes {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
            }
            let mut out = Vec::new();
            file.take(max_bytes.saturating_add(1)).read_to_end(&mut out)?;
            if out.len() as u64 > max_bytes {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "file too large"));
            }
            Ok(Some(out))
        }

        pub(crate) fn open_nofollow(_path: &Path, _create: bool) -> io::Result<Self> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "secure attachment directories require no-follow handles"))
        }

        pub(crate) fn open_read_checked(&self, _name: &str, _single_link: bool) -> io::Result<Option<std::fs::File>> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "secure attachment reads require no-follow directory handles"))
        }

        pub(crate) fn read_file(&self, name: &str) -> io::Result<Option<String>> {
            match self.read_bytes(name, u64::MAX)? {
                None => Ok(None),
                Some(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8")),
            }
        }

        pub(crate) fn open_create_new(&self, _name: &str) -> io::Result<std::fs::File> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "secure attachment creation requires no-follow directory handles"))
        }

        /// Refuse creation where fd-relative no-follow operations are unavailable.
        pub(crate) fn create_file_new(&self, name: &str, contents: &[u8]) -> io::Result<bool> {
            let mut file = match self.open_create_new(name) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    return match self.entry(name)? {
                        Some(entry) if entry.kind == EntryKind::File && entry.nlink == 1 => Ok(false),
                        _ => Err(refused("existing file is not a regular, single-link file")),
                    };
                }
                Err(e) => return Err(e),
            };
            file.write_all(contents)?;
            file.sync_all()?;
            Ok(true)
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
                    f.set_permissions(p)?;
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
                match self.entry(&name) {
                    Ok(Some(info)) => out.push(info),
                    Ok(None) => {},
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

#[cfg(all(test, windows))]
#[path = "fs_safe/windows_tests.rs"]
mod windows_tests;

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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_aliases_keep_the_suffix_nofollow() {
        for parent in [std::env::temp_dir(), std::path::PathBuf::from("/tmp")] {
            let temp = tempfile::tempdir_in(parent).unwrap();
            let root = temp.path();
            let dir = SafeDir::open_nofollow(root, false).unwrap();
            let real = dir.child_dir("real", true).unwrap().unwrap();
            real.child_dir("nested", true).unwrap().unwrap();
            assert!(SafeDir::open_nofollow(&root.join("real"), false).is_ok());
            symlink("real", root.join("linked")).unwrap();
            assert!(SafeDir::open_nofollow(&root.join("linked"), false).is_err());
            assert!(SafeDir::open_nofollow(&root.join("linked/nested"), false).is_err());
        }
    }

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
        assert!(std::fs::symlink_metadata(root.path().join("f.json")).unwrap().file_type().is_symlink());
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

#[cfg(all(test, unix))]
mod attachment_dir_tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn attachment_creation_pins_every_directory_before_name_swap() {
        let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let outside = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let path = root.path().join("agents/sales/attachments");
        let directory = SafeDir::open_nofollow(&path, true).unwrap();
        let original = root.path().join("old-agent");
        std::fs::rename(path.parent().unwrap(), &original).unwrap();
        symlink(outside.path(), path.parent().unwrap()).unwrap();
        assert!(directory.create_file_new("upload.txt", b"pinned").unwrap());
        assert_eq!(std::fs::read(original.join("attachments/upload.txt")).unwrap(), b"pinned");
        assert_eq!(std::fs::metadata(original.join("attachments/upload.txt")).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        assert!(SafeDir::open_nofollow(&path, true).is_err());
    }

    #[test]
    fn attachment_creation_rejects_symlink_in_each_parent_without_creating_children() {
        for part in ["agents", "agents/sales", "agents/sales/attachments"] {
            let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let outside = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let planted = root.path().join(part);
            std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
            symlink(outside.path(), &planted).unwrap();
            assert!(SafeDir::open_nofollow(&root.path().join("agents/sales/attachments"), true).is_err());
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none(), "{part}");
        }
    }
}
