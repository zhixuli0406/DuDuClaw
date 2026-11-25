//! Pin the authority namespace before opening any advisory lock.
use std::{fs::File, io, path::Path};

pub(super) const DIRECTORY: &str = ".google-authority";
pub(super) const INTENT: &str = "intent";
pub(super) const RESOURCE: &str = "resource";
pub(super) const EPOCH: &str = "epoch";
const NAMES: [&str; 3] = [INTENT, RESOURCE, EPOCH];

fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "unsafe or replaced authority namespace")
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{ffi::CString, fs::{Metadata, OpenOptions}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::PathBuf};

    pub(super) struct Directory { home: File, directory: File, path: PathBuf, trusted: Option<crate::trusted_home::TrustedHome> }

    fn validate(metadata: &Metadata, directory: bool, home: bool) -> io::Result<()> {
        // SAFETY: geteuid has no pointer arguments or preconditions.
        if metadata.uid() != unsafe { libc::geteuid() }
            || if directory { !metadata.is_dir() } else { !metadata.is_file() }
            || if home { metadata.mode() & 0o022 != 0 }
               else { metadata.mode() & 0o7777 != if directory { 0o700 } else { 0o600 } }
            || (!directory && metadata.nlink() != 1) {
            return Err(denied());
        }
        Ok(())
    }

    pub(super) fn same(a: &File, b: &File) -> io::Result<()> {
        let a = a.metadata()?;
        let b = b.metadata()?;
        if a.dev() != b.dev() || a.ino() != b.ino() { return Err(denied()); }
        Ok(())
    }

    fn open_at(directory: &File, name: &str, flags: libc::c_int) -> io::Result<File> {
        let name = CString::new(name).map_err(|_| denied())?;
        // SAFETY: directory is live, name is NUL-terminated, and the returned
        // descriptor is owned exactly once. O_NONBLOCK refuses planted FIFOs
        // without waiting for a peer; metadata is checked before using it.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK, 0o600 as libc::c_uint) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn open_home(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
        validate(&file.metadata()?, true, true)?;
        Ok(file)
    }

    impl Directory {
        pub(super) fn open(path: &Path) -> io::Result<Self> {
            Self::open_inner(path, open_home(path)?, None)
        }
        pub(super) fn open_trusted(trusted: &crate::trusted_home::TrustedHome) -> io::Result<Self> {
            trusted.verify()?;
            Self::open_inner(trusted.path(), trusted.directory().try_clone()?, Some(trusted.clone()))
        }
        fn open_inner(path: &Path, home: File, trusted: Option<crate::trusted_home::TrustedHome>) -> io::Result<Self> {
            let name = CString::new(DIRECTORY).map_err(|_| denied())?;
            // SAFETY: home is a pinned directory fd and name is one component.
            if unsafe { libc::mkdirat(home.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists { return Err(error); }
            }
            let directory = open_at(&home, DIRECTORY, libc::O_RDONLY | libc::O_DIRECTORY)?;
            validate(&directory.metadata()?, true, false)?;
            home.sync_all()?;
            Ok(Self { home, directory, path: path.to_owned(), trusted })
        }
        pub(super) fn file(&self, name: &str, create: bool) -> io::Result<File> {
            if let Some(home) = &self.trusted { home.verify()?; }
            same(&self.home, &open_home(&self.path)?)?;
            let current = open_at(&self.home, DIRECTORY, libc::O_RDONLY | libc::O_DIRECTORY)?;
            validate(&current.metadata()?, true, false)?;
            same(&self.directory, &current)?;
            let file = open_at(&self.directory, name, libc::O_RDWR
                | if create { libc::O_CREAT | libc::O_EXCL } else { 0 })?;
            validate(&file.metadata()?, false, false)?;
            Ok(file)
        }
        pub(super) fn sync(&self) -> io::Result<()> { self.directory.sync_all() }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::windows_private_state::SecureDirectory;

    pub(super) struct Directory(SecureDirectory);
    impl Directory {
        pub(super) fn open(home: &Path) -> io::Result<Self> {
            SecureDirectory::open(home, DIRECTORY).map(Self)
        }
        pub(super) fn file(&self, name: &str, create: bool) -> io::Result<File> {
            self.0.open_authority_file(name, create)
        }
        pub(super) fn sync(&self) -> io::Result<()> { Ok(()) }
    }
    // SecureDirectory pins every ancestor and each anchor file excludes delete
    // sharing. ACL, reparse-point, type and link-count checks run on every open.
    pub(super) fn same(_: &File, _: &File) -> io::Result<()> { Ok(()) }
}

pub(super) struct Namespace {
    directory: platform::Directory,
    anchors: [File; 3],
    pub(super) epoch_created: bool,
}
impl Namespace {
    pub(super) fn open(home: &Path) -> io::Result<Self> {
        let directory = platform::Directory::open(home)?;
        Self::from_directory(directory)
    }
    #[cfg(unix)]
    pub(super) fn open_trusted(home: &crate::trusted_home::TrustedHome) -> io::Result<Self> {
        Self::from_directory(platform::Directory::open_trusted(home)?)
    }
    fn from_directory(directory: platform::Directory) -> io::Result<Self> {
        let mut epoch_created = false;
        let mut open = |name| match directory.file(name, true) {
            Ok(file) => { if name == EPOCH { epoch_created = true; } Ok(file) },
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => directory.file(name, false),
            Err(e) => Err(e),
        };
        let anchors = [open(INTENT)?, open(RESOURCE)?, open(EPOCH)?];
        directory.sync()?;
        Ok(Self { directory, anchors, epoch_created })
    }
    pub(super) fn file(&self, name: &str) -> io::Result<File> {
        let index = NAMES.iter().position(|n| *n == name).ok_or_else(denied)?;
        let file = self.directory.file(name, false)?;
        platform::same(&self.anchors[index], &file)?;
        Ok(file)
    }
    pub(super) fn sync(&self) -> io::Result<()> { self.directory.sync() }
}
