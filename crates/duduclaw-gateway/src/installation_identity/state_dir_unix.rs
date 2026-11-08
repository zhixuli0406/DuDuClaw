//! Unix descriptor-relative private state directory (`<home>/state`).
//!
//! As with other gateway state stores, ancestors of the configured home are
//! trusted. Home itself and `state/` are pinned with `O_NOFOLLOW` descriptors;
//! every file is opened relative to the pinned directory, must be owned by the
//! effective uid, mode 0600, a regular file with exactly one hard link and, on
//! macOS, carry no extended ACL entries.
use std::{io, path::Path};
use zeroize::Zeroizing;
use super::MAX_STATE_BYTES;

pub struct StateDir {
    directory: std::fs::File,
}

impl StateDir {
    pub fn open(home: &Path) -> io::Result<Self> {
        use std::os::{fd::{AsRawFd, FromRawFd}, unix::fs::OpenOptionsExt};
        let home = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(home)?;
        validate(&home, true)?;
        if unsafe { libc::mkdirat(home.as_raw_fd(), c"state".as_ptr(), 0o700) } != 0
            && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { libc::openat(home.as_raw_fd(), c"state".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: openat returned a fresh, owned descriptor.
        let directory = unsafe { std::fs::File::from_raw_fd(fd) };
        validate(&directory, true)?;
        home.sync_all()?;
        Ok(Self { directory })
    }

    /// Open (creating if needed) a private, non-replaceable lock file.
    pub fn lock(&self, name: &str) -> io::Result<std::fs::File> {
        let lock = open_at(&self.directory, name, libc::O_RDWR | libc::O_CREAT)?;
        validate(&lock, false)?;
        Ok(lock)
    }

    /// Bounded read. `Ok(None)` only for `NotFound`; any other error (symlink,
    /// permissions, hard link, FIFO, ACL) is returned so callers fail closed.
    pub fn read(&self, name: &str) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
        use std::io::Read;
        let file = match open_at(&self.directory, name, libc::O_RDONLY) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        validate(&file, false)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_STATE_BYTES + 1));
        file.take(MAX_STATE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STATE_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        Ok(Some(bytes))
    }

    /// Write a private temporary file and rename it over `name`.
    pub fn write_atomic(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::{io::Write, os::fd::AsRawFd};
        if bytes.len() > MAX_STATE_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        match open_at(&self.directory, name, libc::O_RDONLY) {
            Ok(file) => validate(&file, false)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let temp_name = format!(".state-{}.tmp", uuid::Uuid::new_v4());
        let temp = c_name(&temp_name)?;
        let target = c_name(name)?;
        let result = (|| {
            let mut file = open_at(&self.directory, &temp_name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
            validate(&file, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            if unsafe { libc::renameat(self.directory.as_raw_fd(), temp.as_ptr(), self.directory.as_raw_fd(), target.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.directory.sync_all()
        })();
        if result.is_err() { unsafe { libc::unlinkat(self.directory.as_raw_fd(), temp.as_ptr(), 0); } }
        result
    }

    /// `O_CREAT|O_EXCL` creation. An existing entry (including a dangling
    /// symlink) returns `AlreadyExists` and is never replaced. A failed write
    /// removes the file this call created; callers hold an exclusive lock.
    pub fn create_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        use std::{io::Write, os::fd::AsRawFd};
        if bytes.len() > MAX_STATE_BYTES { return Err(io::ErrorKind::InvalidData.into()); }
        let target = c_name(name)?;
        let mut file = open_at(&self.directory, name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
        let result = (|| {
            validate(&file, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.directory.sync_all()
        })();
        if result.is_err() {
            drop(file);
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), target.as_ptr(), 0); }
        }
        result
    }
}

pub(super) fn validate(file: &std::fs::File, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    #[cfg(target_os = "macos")]
    reject_macos_acl(file)?;
    let meta = file.metadata()?;
    if meta.uid() != unsafe { libc::geteuid() }
        || if directory { !meta.is_dir() || meta.mode() & 0o7022 != 0 }
            else { !meta.is_file() || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1 } {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

fn c_name(name: &str) -> io::Result<std::ffi::CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    std::ffi::CString::new(name).map_err(io::Error::other)
}

fn open_at(directory: &std::fs::File, name: &str, flags: i32) -> io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let name = c_name(name)?;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
        flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0o600 as libc::c_uint) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    // SAFETY: new descriptor, transferred once.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
fn reject_macos_acl(file: &std::fs::File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // Darwin's extended ACL API is in libc, but not exposed by the libc crate.
    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
        fn acl_get_entry(acl: *mut libc::c_void, id: libc::c_int, entry: *mut *mut libc::c_void) -> libc::c_int;
        fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    // Darwin acl_get_fd returns the extended ACL, or ENOENT when none exists.
    let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) { return Ok(()) }
        return Err(error);
    }
    let mut entry = std::ptr::null_mut();
    let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
    let error = std::io::Error::last_os_error();
    unsafe { acl_free(acl); }
    if result == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,
            "private state must have no extended ACL entries"));
    }
    // Darwin reports EINVAL when the independent, valid ACL has no first entry.
    if result == -1 && error.raw_os_error() == Some(libc::EINVAL) { return Ok(()) }
    Err(error)
}
