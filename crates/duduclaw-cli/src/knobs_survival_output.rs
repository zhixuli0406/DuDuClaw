//! Publish only the explicitly requested report artifact.
//! Unix publication is anchored to one verified directory FD throughout.

use duduclaw_core::error::{DuDuClawError, Result};
use std::path::Path;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        io::{AsRawFd, FromRawFd},
    };
    use std::{
        ffi::CString,
        fs::{File, Metadata, OpenOptions},
        io::Write,
        path::PathBuf,
    };

    pub(crate) struct OutputTarget {
        directory: File,
        canonical_parent: PathBuf,
        directory_identity: Metadata,
        name: CString,
    }

    fn same_file(a: &Metadata, b: &Metadata) -> bool {
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    fn refused() -> DuDuClawError {
        DuDuClawError::Config("survival output must be a regular single-link artifact outside the history database and its sidecars".into())
    }

    fn checked_file(directory: &File, name: &CString) -> Result<Option<File>> {
        // NONBLOCK prevents a replaced FIFO from hanging before metadata
        // validation; NOFOLLOW rejects an output symlink without opening it.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(refused());
        }
        // SAFETY: openat returned one owned file descriptor.
        let file = unsafe { File::from_raw_fd(descriptor) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(refused());
        }
        Ok(Some(file))
    }

    struct Stage<'a> {
        directory: &'a File,
        name: CString,
        file: File,
    }
    impl Drop for Stage<'_> {
        fn drop(&mut self) {
            // This name belongs to our stage in the anchored directory. A
            // successful rename removes it; cleanup then sees ENOENT.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }

    impl OutputTarget {
        pub(crate) fn prepare(home: &Path, path: &Path) -> Result<Self> {
            let name = path.file_name().ok_or_else(refused)?;
            // These names are reserved in every directory: home may be
            // rebound between preparation, snapshot capture and publication.
            if name.to_str().is_some_and(|name| {
                [
                    "tasks.db",
                    "tasks.db-wal",
                    "tasks.db-shm",
                    "tasks.db-journal",
                ]
                .iter()
                .any(|reserved| name.eq_ignore_ascii_case(reserved))
            }) {
                return Err(refused());
            }
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let canonical_parent = std::fs::canonicalize(parent)?;
            let directory_identity = std::fs::metadata(&canonical_parent)?;
            if !directory_identity.is_dir() {
                return Err(refused());
            }
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY);
            let directory = options.open(&canonical_parent)?;
            if !same_file(&directory_identity, &directory.metadata()?) {
                return Err(refused());
            }
            let home_identity = std::fs::metadata(home)?;
            let reserved = [
                "tasks.db",
                "tasks.db-wal",
                "tasks.db-shm",
                "tasks.db-journal",
            ];
            if same_file(&home_identity, &directory_identity)
                && name.to_str().is_some_and(|name| {
                    reserved
                        .iter()
                        .any(|reserved| name.eq_ignore_ascii_case(reserved))
                })
            {
                return Err(refused());
            }
            let name = CString::new(name.as_bytes()).map_err(|_| refused())?;
            let target = Self {
                directory,
                canonical_parent,
                directory_identity,
                name,
            };
            if let Some(existing) = checked_file(&target.directory, &target.name)? {
                let identity = existing.metadata()?;
                for input in reserved {
                    if let Ok(metadata) = std::fs::metadata(home.join(input)) {
                        if same_file(&identity, &metadata) {
                            return Err(refused());
                        }
                    }
                }
            }
            Ok(target)
        }

        pub(crate) fn publish(&self, body: &str) -> Result<()> {
            if body.len() > 16 * 1024 * 1024 {
                return Err(DuDuClawError::Config(
                    "survival output artifact byte limit exceeded".into(),
                ));
            }
            if !same_file(
                &self.directory_identity,
                &std::fs::metadata(&self.canonical_parent)?,
            ) {
                return Err(refused());
            }
            checked_file(&self.directory, &self.name)?;
            let name = CString::new(format!(
                ".dudu-survival-{:032x}.tmp",
                rand::random::<u128>()
            ))
            .expect("ASCII stage name");
            let descriptor = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: openat created one new descriptor in our pinned parent.
            let file = unsafe { File::from_raw_fd(descriptor) };
            let mut stage = Stage {
                directory: &self.directory,
                name,
                file,
            };
            stage
                .file
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            stage.file.write_all(body.as_bytes())?;
            stage.file.flush()?;
            let identity = stage.file.metadata()?;
            let staged = checked_file(&self.directory, &stage.name)?.ok_or_else(refused)?;
            if !same_file(&identity, &staged.metadata()?) || identity.nlink() != 1 {
                return Err(refused());
            }
            checked_file(&self.directory, &self.name)?;
            if !same_file(
                &self.directory_identity,
                &std::fs::metadata(&self.canonical_parent)?,
            ) {
                return Err(refused());
            }
            // Both names use the same already-verified directory FD. A path
            // or parent symlink replacement cannot redirect this rename.
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    stage.name.as_ptr(),
                    self.directory.as_raw_fd(),
                    self.name.as_ptr(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let published = checked_file(&self.directory, &self.name)?.ok_or_else(refused)?;
            if !same_file(&identity, &published.metadata()?)
                || !same_file(
                    &self.directory_identity,
                    &std::fs::metadata(&self.canonical_parent)?,
                )
            {
                return Err(refused());
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
pub(super) use unix::OutputTarget;

#[cfg(not(unix))]
pub(super) struct OutputTarget;
#[cfg(not(unix))]
impl OutputTarget {
    pub(super) fn prepare(_home: &Path, _path: &Path) -> Result<Self> {
        Err(DuDuClawError::Config("anchored survival artifact publication is unsupported on this platform; use stdout instead of --output".into()))
    }
    pub(super) fn publish(&self, _body: &str) -> Result<()> {
        Err(DuDuClawError::Config("anchored survival artifact publication is unsupported on this platform; use stdout instead of --output".into()))
    }
}
