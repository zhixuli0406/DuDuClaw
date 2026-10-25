//! A home-wide, cross-process OAuth grant fence. Only nonblocking file-lock
//! attempts run on the async worker; contention yields with a bounded wait.
//! The lease covers exchange/probe/publication or revoke decision/HTTP.
//!
//! A timed-out/cancelled revoke may still be running at Google. Its durable
//! uncertainty marker prevents a subsequent grant from racing that request.
use fs2::FileExt;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
pub const UNCERTAIN: &str =
    "上次 Google 授權撤銷尚未確認完成；新的連結已暫停，請管理員確認 Google 授權狀態。";

pub struct Lease {
    _file: std::fs::File,
    home: PathBuf,
}

fn marker(home: &Path) -> PathBuf {
    home.join("locks/google-grant-revoke-uncertain.json")
}

impl Lease {
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn begin_revoke(&self) -> Result<(), String> {
        // Without directory durability support, never issue a provider write
        // whose uncertainty barrier might disappear after power loss.
        #[cfg(not(unix))]
        return Err("此平台無法確認撤銷守門已持久化，本次僅中斷本機連線。".into());
        #[cfg(unix)]
        self.begin_revoke_unix()
    }

    #[cfg(unix)]
    fn begin_revoke_unix(&self) -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(marker(&self.home))
            .map_err(|_| UNCERTAIN.to_owned())?;
        let body = serde_json::json!({"version":1,"operation_id":uuid::Uuid::new_v4(),"started_at":chrono::Utc::now().to_rfc3339()});
        file.write_all(body.to_string().as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| UNCERTAIN.to_owned())?;
        sync_dir(&self.home)?;
        Ok(())
    }

    pub fn confirm_revoke(&self) -> Result<(), String> {
        std::fs::remove_file(marker(&self.home)).map_err(|_| UNCERTAIN.to_owned())?;
        sync_dir(&self.home)
    }
}

fn sync_dir(home: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::fs::File::open(home.join("locks"))
            .and_then(|file| file.sync_all())
            .map_err(|_| UNCERTAIN.to_owned())?;
        // Persist the locks dentry itself on the first use of a fresh home.
        // Syncing only the child directory cannot make its parent entry durable.
        #[cfg(test)]
        if failing_home_sync().lock().unwrap().contains(home) {
            return Err(UNCERTAIN.into());
        }
        std::fs::File::open(home)
            .and_then(|file| file.sync_all())
            .map_err(|_| UNCERTAIN.to_owned())?;
    }
    Ok(())
}

#[cfg(test)]
fn failing_home_sync() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static HOMES: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    HOMES.get_or_init(Default::default)
}

#[cfg(test)]
pub(super) fn fail_home_sync(home: &Path, fail: bool) {
    let home = std::fs::canonicalize(home).unwrap();
    if fail {
        failing_home_sync().lock().unwrap().insert(home);
    } else {
        failing_home_sync().lock().unwrap().remove(&home);
    }
}

/// Early metadata-only refusal. This is not a lease and never proves an
/// effect safe: the later effect must acquire its real grant lease again.
pub(crate) fn check_metadata_barrier(home: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(marker(home)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(UNCERTAIN.into()),
    }
}

pub async fn acquire(home: &Path) -> Result<Lease, String> {
    let canonical =
        std::fs::canonicalize(home).map_err(|_| "無法確認 Google 連線的資料目錄。".to_owned())?;
    let home = canonical.as_path();
    let directory = home.join("locks");
    std::fs::create_dir_all(&directory)
        .map_err(|_| "無法協調 Google 連線，這次沒有變更 Google 授權。".to_owned())?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(directory.join("google-grants.lock"))
        .map_err(|_| "無法協調 Google 連線。".to_owned())?;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => match std::fs::symlink_metadata(marker(home)) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Lease {
                        _file: file,
                        home: canonical,
                    });
                }
                _ => return Err(UNCERTAIN.into()),
            },
            Err(error)
                if error.raw_os_error().is_some_and(|code| {
                    Some(code) == fs2::lock_contended_error().raw_os_error()
                }) => {}
            Err(_) => return Err("無法協調 Google 連線。".into()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Google 連線正在更新或撤銷中，請稍後重試。".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[cfg(unix)]
    async fn first_locks_directory_requires_successful_home_sync_before_revoke() {
        let home = tempfile::tempdir().unwrap();
        assert!(!home.path().join("locks").exists());
        let lease = acquire(home.path()).await.unwrap();
        fail_home_sync(home.path(), true);
        assert!(lease.begin_revoke().is_err());
        fail_home_sync(home.path(), false);
        assert!(marker(lease.home()).exists());
        drop(lease);
        assert!(acquire(home.path()).await.is_err());
    }

    #[tokio::test]
    #[cfg(not(unix))]
    async fn unsupported_directory_durability_never_allows_provider_revoke() {
        let home = tempfile::tempdir().unwrap();
        let lease = acquire(home.path()).await.unwrap();
        assert!(lease.begin_revoke().is_err());
        assert!(!marker(lease.home()).exists());
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn grant_cannot_publish_during_revoke_and_cancel_is_durably_closed() {
        let home = tempfile::tempdir().unwrap();
        let lease = acquire(home.path()).await.unwrap();
        lease.begin_revoke().unwrap();
        let other_home = home.path().to_path_buf();
        let waiting = tokio::spawn(async move { acquire(&other_home).await });
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!waiting.is_finished());
        lease.confirm_revoke().unwrap();
        drop(lease);
        drop(waiting.await.unwrap().unwrap());
        let uncertain = acquire(home.path()).await.unwrap();
        uncertain.begin_revoke().unwrap();
        drop(uncertain);
        assert!(acquire(home.path()).await.is_err());
        assert!(marker(home.path()).exists());
    }
}
