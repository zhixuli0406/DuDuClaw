//! Cross-process authority barrier for delegate:owner (D12).
//!
//! Lock order is intent -> resource -> application/SQLite locks. Readers take
//! intent exclusively only while acquiring a shared resource lease. Writers
//! hold intent shared until their exclusive resource lease ends: once any
//! writer has registered, new readers cannot overtake it. No lock file is ever
//! renamed/unlinked. Epoch publication precedes mutation (failed writes may
//! invalidate old calls, but can never resurrect them). No DB transaction is
//! held by a reader across network I/O. Out-of-band file/SQL writes are outside
//! this protocol. A sent request can still complete remotely after revocation.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use fs2::FileExt;

#[derive(Clone)]
pub struct AuthorityCoordinator { home: PathBuf }
pub struct SharedLease { _resource: File }
pub struct WriteLease { _resource: File, _intent: File }

fn open(home: &Path, name: &str) -> io::Result<File> {
    let path = home.join(name);
    if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(io::Error::other("invalid authority lock file"));
    }
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    if !file.metadata()?.is_file() { return Err(io::Error::other("invalid authority file")); }
    Ok(file)
}
fn wait(mut attempt: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let until = Instant::now() + Duration::from_secs(35);
    loop {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < until =>
                std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e),
        }
    }
}
impl AuthorityCoordinator {
    pub fn new(home: &Path) -> Self { Self { home: home.to_owned() } }
    /// Call on the blocking pool when used by an async handler.
    pub fn write(&self) -> io::Result<WriteLease> {
        let intent = open(&self.home, ".google-authority.intent")?;
        wait(|| FileExt::try_lock_shared(&intent))?;
        let resource = open(&self.home, ".google-authority.resource")?;
        wait(|| FileExt::try_lock_exclusive(&resource))?;
        let mut epoch = open(&self.home, ".google-authority.epoch")?;
        // Random durable generations prevent ABA, including revoke/re-auth.
        epoch.seek(SeekFrom::Start(0))?;
        epoch.write_all(uuid::Uuid::new_v4().simple().to_string().as_bytes())?;
        epoch.set_len(32)?;
        epoch.sync_all()?;
        #[cfg(unix)] File::open(&self.home)?.sync_all()?;
        Ok(WriteLease { _resource: resource, _intent: intent })
    }
    pub fn read(&self) -> io::Result<SharedLease> {
        let intent = open(&self.home, ".google-authority.intent")?;
        let resource = open(&self.home, ".google-authority.resource")?;
        wait(|| {
            FileExt::try_lock_exclusive(&intent)?;
            let result = FileExt::try_lock_shared(&resource);
            FileExt::unlock(&intent)?;
            result
        })?;
        Ok(SharedLease { _resource: resource })
    }
    pub async fn shared(&self) -> io::Result<SharedLease> {
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || coordinator.read()).await.map_err(io::Error::other)?
    }
    pub fn initialize(&self) -> io::Result<()> {
        match std::fs::symlink_metadata(self.home.join(".google-authority.epoch")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => { drop(self.write()?); Ok(()) },
            Err(e) => Err(e),
            Ok(_) => self.epoch().map(|_| ()),
        }
    }
    pub fn epoch(&self) -> io::Result<String> {
        let path = self.home.join(".google-authority.epoch");
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(io::Error::other("invalid authority epoch file"));
        }
        let mut opts = OpenOptions::new();
        opts.read(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts.open(path)?;
        let mut value = String::new();
        (&mut file).take(33).read_to_string(&mut value)?;
        if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::other("missing or corrupt authority epoch"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_write_retires_epoch_including_failed_mutation() {
        let home = tempfile::tempdir().unwrap();
        let c = AuthorityCoordinator::new(home.path());
        assert!(c.epoch().is_err());
        drop(c.write().unwrap());
        let a = c.epoch().unwrap();
        drop(c.write().unwrap());
        assert_ne!(a, c.epoch().unwrap());
    }
    #[test]
    fn writer_waits_for_headers_and_blocks_new_readers() {
        let home = tempfile::tempdir().unwrap();
        let c = AuthorityCoordinator::new(home.path());
        drop(c.write().unwrap());
        let old = c.epoch().unwrap();
        let headers = c.read().unwrap();
        let intent = open(home.path(), ".google-authority.intent").unwrap();
        FileExt::lock_shared(&intent).unwrap();
        let reader_intent = open(home.path(), ".google-authority.intent").unwrap();
        assert!(FileExt::try_lock_exclusive(&reader_intent).is_err());
        let writer_resource = open(home.path(), ".google-authority.resource").unwrap();
        assert!(FileExt::try_lock_exclusive(&writer_resource).is_err());
        drop(headers);
        FileExt::try_lock_exclusive(&writer_resource).unwrap();
        drop(writer_resource);
        drop(intent);
        drop(c.write().unwrap());
        assert_ne!(old, c.epoch().unwrap(), "post-header results must be discarded");
    }
    #[test]
    fn cross_process_writer_child() {
        let Some(home) = std::env::var_os("DUDUCLAW_AUTHORITY_TEST_HOME") else { return; };
        let home = PathBuf::from(home);
        let intent = open(&home, ".google-authority.intent").unwrap();
        FileExt::lock_shared(&intent).unwrap();
        std::fs::write(home.join("writer-registered"), b"ready").unwrap();
        drop(AuthorityCoordinator::new(&home).write().unwrap());
    }
    #[test]
    fn cross_process_writer_cannot_commit_before_response_headers() {
        let home = tempfile::tempdir().unwrap();
        let c = AuthorityCoordinator::new(home.path());
        c.initialize().unwrap();
        let epoch = c.epoch().unwrap();
        let headers = c.read().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "authority_coordinator::tests::cross_process_writer_child"])
            .env("DUDUCLAW_AUTHORITY_TEST_HOME", home.path())
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !home.path().join("writer-registered").exists() {
            if Instant::now() >= deadline {
                let _ = child.kill(); let _ = child.wait();
                panic!("child never registered writer intent");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(epoch, c.epoch().unwrap());
        let reader = open(home.path(), ".google-authority.intent").unwrap();
        assert!(FileExt::try_lock_exclusive(&reader).is_err());
        drop(headers);
        assert!(child.wait().unwrap().success());
        assert_ne!(epoch, c.epoch().unwrap());
    }

}
