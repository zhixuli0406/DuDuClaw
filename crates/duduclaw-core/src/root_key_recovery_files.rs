//! Pinned advisory state directory. These checks prevent path redirection, not
//! same-UID forgery: the root-owned systemd unit enforces the restart ceiling.
use std::io;
use std::path::Path;
use serde::Serialize;

pub(super) struct StateDir {
    #[cfg(unix)]
    file: std::fs::File,
}

#[cfg(unix)]
impl StateDir {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
        Self::from_file(file)
    }

    fn from_file(file: std::fs::File) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7022 != 0 {
            return Err(io::Error::other("invalid recovery state directory owner or mode"));
        }
        Ok(Self { file })
    }

    pub(super) fn open_home(home: &Path, create: bool) -> io::Result<Self> {
        use std::os::fd::AsRawFd;
        // The configured home's ancestor path remains an installation trust
        // assumption. Reject a symlink at home itself, then pin state via its fd.
        let home = Self::open(home)?;
        if create && unsafe { libc::mkdirat(home.file.as_raw_fd(), c"state".as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) { return Err(error); }
        }
        Self::from_file(home.open_at("state", libc::O_RDONLY | libc::O_DIRECTORY, 0)?)
    }

    fn name(name: &str) -> io::Result<std::ffi::CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(io::Error::other("invalid recovery state name"));
        }
        std::ffi::CString::new(name).map_err(io::Error::other)
    }

    fn open_at(&self, name: &str, flags: i32, mode: libc::mode_t) -> io::Result<std::fs::File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let name = Self::name(name)?;
        // The same pinned directory is used for reads, lock, rename and fsync.
        // O_NONBLOCK avoids hanging on a hostile FIFO before fstat rejects it.
        let fd = unsafe { libc::openat(self.file.as_raw_fd(), name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, mode as libc::c_uint) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: openat returned a new descriptor owned by this call.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn validate_file(file: &std::fs::File) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1 {
            return Err(io::Error::other("invalid recovery state file type, owner, mode or link count"));
        }
        Ok(())
    }

    pub(super) fn budget_lock(&self) -> io::Result<std::fs::File> {
        use fs2::FileExt;
        let file = self.open_at("root-key-recovery.lock", libc::O_RDWR | libc::O_CREAT, 0o600)?;
        Self::validate_file(&file)?;
        file.try_lock_exclusive()?;
        Ok(file)
    }

    pub(super) fn read_json<T: serde::de::DeserializeOwned>(&self, name: &str) -> io::Result<T> {
        self.read_json_bounded(name, 16384)
    }

    pub(super) fn read_json_bounded<T: serde::de::DeserializeOwned>(&self, name: &str, limit: u64) -> io::Result<T> {
        use std::io::Read;
        let file = self.open_at(name, libc::O_RDONLY, 0)?;
        Self::validate_file(&file)?;
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit { return Err(io::Error::other("recovery state file too large")); }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }

    pub(super) fn write_json(&self, name: &str, value: &impl Serialize) -> io::Result<()> {
        self.write_json_bounded(name, value, 16384)
    }

    pub(super) fn write_json_bounded(&self, name: &str, value: &impl Serialize, limit: u64) -> io::Result<()> {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        // Refuse suspicious existing files, including hard links and symlinks.
        // A same-UID writer may still replace these; they never grant authority.
        match self.open_at(name, libc::O_RDONLY, 0) {
            Ok(file) => Self::validate_file(&file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
        if bytes.len() as u64 > limit { return Err(io::Error::other("recovery state file too large")); }
        let temp_name = format!(".root-key-{}.tmp", uuid::Uuid::new_v4());
        let temp = Self::name(&temp_name)?;
        let target = Self::name(name)?;
        let result = (|| {
            let mut file = self.open_at(&temp_name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)?;
            Self::validate_file(&file)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            if unsafe { libc::renameat(self.file.as_raw_fd(), temp.as_ptr(), self.file.as_raw_fd(), target.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            self.file.sync_all()
        })();
        if result.is_err() { unsafe { libc::unlinkat(self.file.as_raw_fd(), temp.as_ptr(), 0); } }
        result
    }
}

// Native recovery is Linux-only. Do not silently use weaker path operations on
// other platforms; pure counter/config/diagnostic tests remain portable.
#[cfg(not(unix))]
impl StateDir {
    pub(super) fn open_home(_: &Path, _: bool) -> io::Result<Self> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn budget_lock(&self) -> io::Result<std::fs::File> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn read_json_bounded<T: serde::de::DeserializeOwned>(&self, _: &str, _: u64) -> io::Result<T> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn write_json_bounded(&self, _: &str, _: &impl Serialize, _: u64) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn read_json<T: serde::de::DeserializeOwned>(&self, _: &str) -> io::Result<T> { Err(io::ErrorKind::Unsupported.into()) }
    pub(super) fn write_json(&self, _: &str, _: &impl Serialize) -> io::Result<()> { Err(io::ErrorKind::Unsupported.into()) }
}
