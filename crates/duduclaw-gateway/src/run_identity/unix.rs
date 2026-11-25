//! Unique private socket directory. Never unlink an old path before binding.
use std::{ffi::CString, fs::File, io, os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt, path::Path};
use duduclaw_core::run_credential::discovery::DiscoveryDirectory;

pub(super) struct SocketDirectory {
    parent: File,
    directory: File,
    name: CString,
}

impl SocketDirectory {
    pub(super) fn bind(home: &Path, discovery: &DiscoveryDirectory)
        -> io::Result<(tokio::net::UnixListener, String, Self)>
    {
        let parent = discovery.try_clone_directory()?;
        cleanup_stale(home, &parent)?;
        let name = CString::new(format!("i-{:016x}", rand::random::<u64>())).unwrap();
        // SAFETY: pinned parent; single random component. EEXIST denies, never cleans up an unknown entry.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let directory = unsafe { File::from_raw_fd(fd) };
        let owned = Self { parent, directory, name };
        let metadata = owned.directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let path = std::path::absolute(home)?.join("run").join(owned.name.to_str().unwrap());
        // Validate the pathname used by bind against our pinned directory, both before and after bind.
        owned.check_path(&path)?;
        let socket = path.join("s");
        let listener = tokio::net::UnixListener::bind(&socket)?;
        owned.check_path(&path)?;
        let endpoint = format!("unix:v1:{}", socket.to_str().ok_or(io::ErrorKind::InvalidInput)?);
        Ok((listener, endpoint, owned))
    }

    fn check_path(&self, path: &Path) -> io::Result<()> {
        let expected = self.directory.metadata()?;
        let actual = std::fs::symlink_metadata(path)?;
        if !actual.is_dir() || actual.dev() != expected.dev() || actual.ino() != expected.ino() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}

// Called only while gateway.lock and the registry's listener lock are held.
// Enumerate names, then operate exclusively relative to pinned descriptors.
// Symlinks, foreign owners, broad modes, non-sockets and extra files are left alone.
fn cleanup_stale(home: &Path, parent: &File) -> io::Result<()> {
    let path = home.join("run");
    let actual = std::fs::symlink_metadata(&path)?;
    let pinned = parent.metadata()?;
    if !actual.is_dir() || actual.dev() != pinned.dev() || actual.ino() != pinned.ino() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    for entry in std::fs::read_dir(path)?.take(512) {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue; };
        let Some(suffix) = name.strip_prefix("i-") else { continue; };
        if suffix.len() != 16 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) { continue; }
        let name = CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
        let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { continue; }
        let directory = unsafe { File::from_raw_fd(fd) };
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 { continue; }
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe { libc::fstatat(directory.as_raw_fd(), c"s".as_ptr(), stat.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        if result == 0 {
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK || stat.st_uid != unsafe { libc::geteuid() } { continue; }
        } else if io::Error::last_os_error().kind() != io::ErrorKind::NotFound { continue; }
        // Drop unlinks only s and removes the directory only if still the same inode.
        drop(SocketDirectory { parent: parent.try_clone()?, directory, name });
    }
    Ok(())
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        // Only unlink the socket entry relative to the owned directory. Never follow a
        // symlink or recursively traverse anything (including after a crash/restart).
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), c"s".as_ptr(), 0); }
        let Ok(ours) = self.directory.metadata() else { return; };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstatat(self.parent.as_raw_fd(), self.name.as_ptr(), stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW) } != 0 { return; }
        let stat = unsafe { stat.assume_init() };
        if stat.st_dev as u64 == ours.dev() && stat.st_ino as u64 == ours.ino() {
            unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), libc::AT_REMOVEDIR); }
        }
    }
}
