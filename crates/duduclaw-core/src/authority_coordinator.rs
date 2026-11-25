//! Cross-process authority barrier for delegate:owner (D12).
//!
//! Lock order is seat admission -> intent -> resource -> SQLite. Readers take
//! intent exclusively only while acquiring a shared resource lease. Writers
//! hold intent shared until their exclusive resource lease ends: once any
//! writer has registered, new readers cannot overtake it. No lock file is ever
//! renamed/unlinked. Epoch publication precedes mutation (failed writes may
//! invalidate old calls, but can never resurrect them). No DB transaction is
//! held by a reader across network I/O. Out-of-band file/SQL writes are outside
//! this protocol. A sent request can still complete remotely after revocation.
//! The private namespace excludes other accounts; it is not a sandbox against
//! another process running as the same OS user.
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::sync::{Arc, OnceLock};
mod namespace;
use namespace::{Namespace, INTENT, RESOURCE, EPOCH};
use fs2::FileExt;

#[derive(Clone)]
pub struct AuthorityCoordinator {
    home: PathBuf,
    #[cfg(unix)]
    trusted_home: Option<crate::trusted_home::TrustedHome>,
    namespace: Arc<OnceLock<Result<Arc<Namespace>, String>>>,
}
pub struct SharedLease { _resource: File, _namespace: Arc<Namespace> }
pub struct WriteLease { _resource: File, _intent: File, _namespace: Arc<Namespace> }

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
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_owned(), #[cfg(unix)] trusted_home: None, namespace: Arc::new(OnceLock::new()) }
    }
    #[cfg(unix)]
    pub fn from_trusted_home(home: &crate::trusted_home::TrustedHome) -> Self {
        Self { home: home.path().to_owned(), trusted_home: Some(home.clone()), namespace: Arc::new(OnceLock::new()) }
    }
    fn namespace(&self) -> io::Result<Arc<Namespace>> {
        self.namespace.get_or_init(|| {
            #[cfg(unix)]
            let result = match &self.trusted_home {
                Some(home) => Namespace::open_trusted(home),
                None => Namespace::open(&self.home),
            };
            #[cfg(not(unix))]
            let result = Namespace::open(&self.home);
            result.map(Arc::new).map_err(|e| e.to_string())
        }).as_ref().cloned().map_err(|e| io::Error::other(e.clone()))
    }
    /// Call on the blocking pool when used by an async handler.
    ///
    /// Acquiring this lease immediately and durably retires pending authority,
    /// even if the later write fails or rolls back. It is not a read/init lock:
    /// perform read-only migration/no-op checks BEFORE calling, recheck under
    /// the lease, then begin SQLite and hold the lease through commit/rollback.
    /// Publishing only after commit is unsafe without a recovery protocol:
    /// SQLite/file commit could succeed but durable epoch publication fail.
    /// Keeping publication first also covers writers spanning multiple stores.
    pub fn write(&self) -> io::Result<WriteLease> {
        let namespace = self.namespace()?;
        let intent = namespace.file(INTENT)?;
        wait(|| FileExt::try_lock_shared(&intent))?;
        let resource = namespace.file(RESOURCE)?;
        wait(|| FileExt::try_lock_exclusive(&resource))?;
        let mut epoch = namespace.file(EPOCH)?;
        // Random durable generations prevent ABA, including revoke/re-auth.
        epoch.seek(SeekFrom::Start(0))?;
        epoch.write_all(uuid::Uuid::new_v4().simple().to_string().as_bytes())?;
        epoch.set_len(32)?;
        epoch.sync_all()?;
        namespace.sync()?;
        Ok(WriteLease { _resource: resource, _intent: intent, _namespace: namespace })
    }
    pub fn read(&self) -> io::Result<SharedLease> {
        let namespace = self.namespace()?;
        let intent = namespace.file(INTENT)?;
        let resource = namespace.file(RESOURCE)?;
        wait(|| {
            FileExt::try_lock_exclusive(&intent)?;
            let result = FileExt::try_lock_shared(&resource);
            FileExt::unlock(&intent)?;
            result
        })?;
        Ok(SharedLease { _resource: resource, _namespace: namespace })
    }
    pub async fn shared(&self) -> io::Result<SharedLease> {
        let coordinator = self.clone();
        tokio::task::spawn_blocking(move || coordinator.read()).await.map_err(io::Error::other)?
    }
    pub fn initialize(&self) -> io::Result<()> {
        let namespace = self.namespace()?;
        let lease = self.read()?;
        let empty_new = namespace.epoch_created && namespace.file(EPOCH)?.metadata()?.len() == 0;
        if !empty_new { return self.epoch().map(|_| ()); }
        drop(lease);
        // Concurrent initializers serialize through the writer barrier. An
        // extra initial generation can only invalidate, never restore a call.
        drop(self.write()?);
        Ok(())
    }
    pub fn epoch(&self) -> io::Result<String> {
        let mut file = self.namespace()?.file(EPOCH)?;
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
        let intent = c.namespace().unwrap().file(INTENT).unwrap();
        FileExt::lock_shared(&intent).unwrap();
        let reader_intent = c.namespace().unwrap().file(INTENT).unwrap();
        assert!(FileExt::try_lock_exclusive(&reader_intent).is_err());
        let writer_resource = c.namespace().unwrap().file(RESOURCE).unwrap();
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
        let c = AuthorityCoordinator::new(&home);
        let intent = c.namespace().unwrap().file(INTENT).unwrap();
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
        let reader = c.namespace().unwrap().file(INTENT).unwrap();
        assert!(FileExt::try_lock_exclusive(&reader).is_err());
        drop(headers);
        assert!(child.wait().unwrap().success());
        assert_ne!(epoch, c.epoch().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn regular_file_replacement_cannot_split_a_live_authority_lease() {
        use std::os::unix::fs::OpenOptionsExt;
        for name in [INTENT, RESOURCE, EPOCH] {
            let home = tempfile::tempdir().unwrap();
            let c = AuthorityCoordinator::new(home.path());
            c.initialize().unwrap();
            let headers = c.read().unwrap();
            let path = home.path().join(namespace::DIRECTORY).join(name);
            std::fs::rename(&path, path.with_extension("old")).unwrap();
            // Even a private, regular replacement cannot change this
            // coordinator's pinned lock/epoch identity.
            std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .open(&path).unwrap();
            assert!(c.namespace().unwrap().file(name).is_err(), "{name}");
            drop(headers);
            assert!(c.write().is_err(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn writable_home_refuses_replacement_even_for_a_new_coordinator() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let home = tempfile::tempdir().unwrap();
        let c = AuthorityCoordinator::new(home.path());
        c.initialize().unwrap();
        let headers = c.read().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
        let path = home.path().join(namespace::DIRECTORY).join(RESOURCE);
        std::fs::rename(&path, path.with_extension("old")).unwrap();
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).unwrap();
        assert!(AuthorityCoordinator::new(home.path()).write().is_err());
        assert!(c.read().is_err());
        drop(headers);
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_authority_files_are_refused_on_startup_and_reopen() {
        for name in [INTENT, RESOURCE, EPOCH] {
            let home = tempfile::tempdir().unwrap();
            let c = AuthorityCoordinator::new(home.path());
            c.initialize().unwrap();
            let path = home.path().join(namespace::DIRECTORY).join(name);
            std::fs::hard_link(path, home.path().join("alias")).unwrap();
            assert!(c.namespace().unwrap().file(name).is_err(), "{name}");
            assert!(AuthorityCoordinator::new(home.path()).initialize().is_err(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_home_directory_and_file_permissions_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        for mode in [0o770, 0o707, 0o777] {
            let home = tempfile::tempdir().unwrap();
            std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(AuthorityCoordinator::new(home.path()).initialize().is_err());
            assert!(!home.path().join(namespace::DIRECTORY).exists());
        }
        let home = tempfile::tempdir().unwrap();
        let c = AuthorityCoordinator::new(home.path());
        c.initialize().unwrap();
        for name in [INTENT, RESOURCE, EPOCH] {
            let path = home.path().join(namespace::DIRECTORY).join(name);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(c.namespace().unwrap().file(name).is_err());
            assert!(AuthorityCoordinator::new(home.path()).initialize().is_err());
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::set_permissions(home.path().join(namespace::DIRECTORY), std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(c.read().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_home_and_replaced_directory_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link = parent.path().join("home");
        std::os::unix::fs::symlink(home.path(), &link).unwrap();
        assert!(AuthorityCoordinator::new(&link).initialize().is_err());
        let c = AuthorityCoordinator::new(home.path());
        c.initialize().unwrap();
        let directory = home.path().join(namespace::DIRECTORY);
        std::fs::rename(&directory, home.path().join("old-authority")).unwrap();
        AuthorityCoordinator::new(home.path()).initialize().unwrap();
        assert!(c.read().is_err());
        assert!(c.write().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn trusted_home_pin_survives_owned_links_and_refuses_replacement_before_open() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("real");
        std::fs::create_dir(&path).unwrap();
        let alias = parent.path().join("alias");
        std::os::unix::fs::symlink("real", &alias).unwrap();
        let pin = crate::trusted_home::TrustedHome::resolve(&alias).unwrap();
        let c = AuthorityCoordinator::from_trusted_home(&pin);
        c.initialize().unwrap();
        assert_eq!(c.epoch().unwrap(), AuthorityCoordinator::new(&path).epoch().unwrap());
        let before_open = AuthorityCoordinator::from_trusted_home(&pin);
        std::fs::rename(&path, parent.path().join("old")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(before_open.write().is_err());
        assert!(c.read().is_err());
        assert!(!path.join(namespace::DIRECTORY).exists());
    }

}
