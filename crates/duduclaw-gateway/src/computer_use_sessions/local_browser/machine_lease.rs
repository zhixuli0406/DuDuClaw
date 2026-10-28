//! Machine-wide local-browser lease. A browser launch is dirty until the
//! owned process tree is proved gone; dropping a file lock is not that proof.
use std::{
    fs::File,
    path::{Path, PathBuf},
};
pub(crate) struct MachineLease {
    file: File,
    path: PathBuf,
}
pub(crate) fn path() -> PathBuf {
    #[cfg(all(test, target_os = "macos"))]
    if let Some(path) = super::native_fixture::machine_lease_path() {
        return path;
    }
    std::env::temp_dir().join(format!("duduclaw-local-browser-{}.lock", unsafe {
        libc::geteuid()
    }))
}
impl MachineLease {
    pub(crate) fn acquire(path: &Path) -> Result<Self, String> {
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "local machine lease unavailable")?;
        let meta = file
            .metadata()
            .map_err(|_| "local machine lease unavailable")?;
        if !meta.is_file()
            || meta.nlink() != 1
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err("unsafe local machine lease".into());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("local machine busy".into());
        }
        if file
            .metadata()
            .map_err(|_| "local machine lease unavailable")?
            .len()
            != 0
        {
            return Err("local_machine_cleanup_unverified".into());
        }
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }
    fn begin_with_directory_sync(
        &self,
        sync: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<(), String> {
        if self
            .file
            .metadata()
            .map_err(|_| "local machine lease unavailable")?
            .len()
            != 0
        {
            return Err("local_machine_cleanup_unverified".into());
        }
        use std::io::{Seek, SeekFrom, Write};
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.write_all(b"browser-exit-unverified-v1\n"))
            .and_then(|_| file.sync_all())
            .and_then(|_| sync())
            .map_err(|_| "local_machine_barrier_unavailable".into())
    }
    pub(crate) fn begin_launch(&self) -> Result<(), String> {
        self.begin_with_directory_sync(|| {
            use std::os::unix::fs::OpenOptionsExt;
            let parent = self
                .path
                .parent()
                .ok_or_else(|| std::io::Error::other("machine lease parent unavailable"))?;
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(parent)?
                .sync_all()
        })
    }
    pub(crate) fn acknowledge_exit(&self) -> Result<(), String> {
        self.file
            .set_len(0)
            .and_then(|_| self.file.sync_all())
            .map_err(|_| "local_machine_barrier_unavailable".into())
    }
}

/// Read-only: do not create a lease on an unused installation.
pub(crate) fn status(path: &Path) -> &'static str {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    };
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "unused",
        Err(_) => return "unavailable",
    };
    let Ok(meta) = file.metadata() else {
        return "unavailable";
    };
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return "unavailable";
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } != 0 {
        return "busy";
    }
    let Ok(meta) = file.metadata() else {
        return "unavailable";
    };
    if meta.len() == 0 {
        "available"
    } else {
        "cleanup_unverified"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dropping_a_dirty_lease_never_releases_the_cleanup_barrier() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("machine.lock");
        let lease = MachineLease::acquire(&path).unwrap();
        lease.begin_launch().unwrap();
        drop(lease);
        assert!(
            MachineLease::acquire(&path).is_err(),
            "unknown browser exit must block a new launch across gateway lifetimes"
        );
        assert_eq!(status(&path), "cleanup_unverified");
    }
    #[test]
    fn only_an_explicit_verified_exit_reopens_the_machine_and_live_leases_stay_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("machine.lock");
        let lease = MachineLease::acquire(&path).unwrap();
        assert!(MachineLease::acquire(&path).is_err());
        lease.begin_launch().unwrap();
        lease.acknowledge_exit().unwrap();
        drop(lease);
        assert!(MachineLease::acquire(&path).is_ok());
    }
    #[test]
    fn directory_sync_failure_refuses_launch_and_preserves_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("machine.lock");
        let lease = MachineLease::acquire(&path).unwrap();
        assert!(
            lease
                .begin_with_directory_sync(|| Err(std::io::Error::other(
                    "injected directory sync failure"
                )))
                .is_err()
        );
        drop(lease);
        assert!(MachineLease::acquire(&path).is_err());
    }
    #[test]
    fn doctor_inspection_never_creates_a_machine_lease() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("unused.lock");
        assert_eq!(status(&path), "unused");
        assert!(!path.exists());
    }
}
