//! Admission limits include work after request cancellation and response streaming.
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock}, time::{Duration, Instant}};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub(crate) const MAX_DOWNLOAD: u64 = 20 * 1024 * 1024;
pub(crate) const MAX_PREVIEW: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_MESSAGE: usize = 1024 * 1024;
pub(crate) const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENTS: usize = 32;
pub(crate) const MAX_REFERENCES: usize = 256;
const WINDOW: Duration = Duration::from_secs(60);
struct UserLimit { slots: Arc<Semaphore>, since: Instant, spent: u64 }
pub(super) struct Limits { global: Arc<Semaphore>, users: Mutex<HashMap<(PathBuf, String), UserLimit>> }
impl Limits {
    pub(super) fn new() -> Self { Self { global: Arc::new(Semaphore::new(4)), users: Mutex::new(HashMap::new()) } }
    pub(super) fn acquire(&self, home: &Path, user: &str, cost: u64, budget: u64) -> Result<Permit, String> {
        let global = self.global.clone().try_acquire_owned().map_err(|_| "attachment service busy")?;
        let mut users = self.users.lock().map_err(|_| "attachment service busy")?;
        users.retain(|_, v| v.since.elapsed() < WINDOW || v.slots.available_permits() < 2);
        let key = (home.to_path_buf(), user.to_owned());
        if !users.contains_key(&key) && users.len() >= 4096 { return Err("attachment service busy".into()); }
        let entry = users.entry(key).or_insert_with(|| UserLimit { slots: Arc::new(Semaphore::new(2)), since: Instant::now(), spent: 0 });
        if entry.since.elapsed() >= WINDOW { entry.since = Instant::now(); entry.spent = 0; }
        if entry.spent.saturating_add(cost) > budget { return Err("attachment rate exceeded".into()); }
        let user = entry.slots.clone().try_acquire_owned().map_err(|_| "attachment service busy")?;
        entry.spent += cost;
        Ok(Permit { _global: global, _user: user })
    }
}
pub(crate) struct Permit { _global: OwnedSemaphorePermit, _user: OwnedSemaphorePermit }
pub(crate) fn history(home: &Path, user: &str) -> Result<Permit, String> {
    static LIMITS: OnceLock<Limits> = OnceLock::new();
    LIMITS.get_or_init(Limits::new).acquire(home, user, 1, 30)
}
pub(crate) fn download(home: &Path, user: &str, max: u64) -> Result<Permit, String> {
    static LIMITS: OnceLock<Limits> = OnceLock::new();
    // Reserve the maximum before disk I/O; failed requests also consume budget.
    LIMITS.get_or_init(Limits::new).acquire(home, user, max, 120 * 1024 * 1024)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_attachment_limits_isolate_users_and_release_concurrency() {
        let limits = Limits::new();
        let a = limits.acquire(Path::new("/test"), "alice", 1, 3).unwrap();
        let b = limits.acquire(Path::new("/test"), "alice", 1, 3).unwrap();
        assert!(limits.acquire(Path::new("/test"), "alice", 1, 3).is_err());
        assert!(limits.acquire(Path::new("/test"), "bob", 1, 3).is_ok());
        drop(a); drop(b);
        assert!(limits.acquire(Path::new("/test"), "alice", 1, 3).is_ok());
        assert!(limits.acquire(Path::new("/test"), "alice", 1, 3).is_err());
    }
}
