//! Process-local live authority for delayed isolated reminders. Disk carries
//! only a fail-closed marker. Restart never promotes one to host authority.
use std::{collections::HashMap, path::{Path, PathBuf}, sync::{Mutex, OnceLock}};
use duduclaw_mcp_boundary::io_authority::CapturedAuthority;

use sha2::{Digest, Sha256};
type Key = (PathBuf, String);
struct Entry { authority: CapturedAuthority, digest: [u8; 32] }
fn registry() -> &'static Mutex<HashMap<Key, Entry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<Key, Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}
fn key(home: &Path, id: &str) -> Result<Key, String> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err("invalid reminder identity".into());
    }
    Ok((home.canonicalize().map_err(|e| e.to_string())?, id.to_owned()))
}
/// Register before the marked JSONL record is published. Failure or process
/// loss can only leave an unusable marked record, never an unmarked fallback.
pub(super) fn bind(home: &Path, id: &str, payload: &[u8], authority: &CapturedAuthority) -> Result<bool, String> {
    if !authority.is_scoped() { return Ok(false); }
    authority.verify().map_err(|e| e.to_string())?;
    let key = key(home, id)?;
    let mut entries = registry().lock().map_err(|_| "reminder authority unavailable")?;
    entries.retain(|_, entry| entry.authority.verify().is_ok());
    if entries.len() >= 4096 || entries.contains_key(&key) {
        return Err("reminder authority registry is full or identity already exists".into());
    }
    entries.insert(key, Entry { authority: authority.clone(), digest: Sha256::digest(payload).into() });
    Ok(true)
}
pub(super) fn lookup(home: &Path, id: &str, payload: &[u8]) -> Result<CapturedAuthority, String> {
    let key = key(home, id)?;
    let mut entries = registry().lock().map_err(|_| "reminder authority unavailable")?;
    let Some(entry) = entries.get(&key) else {
        return Err("isolated reminder authority lost; recreate the reminder under a live authorization".into());
    };
    if entry.digest != <[u8; 32]>::from(Sha256::digest(payload)) {
        entries.remove(&key);
        return Err("isolated reminder contents changed".into());
    }
    let authority = entry.authority.clone();
    if let Err(error) = authority.verify() {
        entries.remove(&key);
        return Err(error.to_string());
    }
    Ok(authority)
}
pub(super) fn forget(home: &Path, id: &str) {
    if let Ok(key) = key(home, id) {
        if let Ok(mut entries) = registry().lock() { entries.remove(&key); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use duduclaw_mcp_boundary::io_authority;
    #[tokio::test]
    async fn delayed_authority_is_bound_to_home_and_id_and_never_falls_back_after_loss() {
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let authority = io_authority::scope(crate::isolation_io_test_support::context(live.clone()), async { io_authority::capture() }).await;
        assert!(bind(home.path(), "one", b"original", &authority).unwrap());
        assert!(lookup(home.path(), "one", b"original").is_ok());
        assert!(lookup(other.path(), "one", b"original").is_err());
        assert!(lookup(home.path(), "different", b"original").is_err());
        assert!(bind(home.path(), "one", b"original", &authority).is_err());
        live.store(false, Ordering::SeqCst);
        assert!(lookup(home.path(), "one", b"original").is_err());
        live.store(true, Ordering::SeqCst);
        assert!(lookup(home.path(), "one", b"original").is_err(), "lost entries cannot revive as host work");
        assert!(bind(home.path(), "one", b"original", &authority).unwrap());
        assert!(lookup(home.path(), "one", b"changed-recipient").is_err());
        assert!(lookup(home.path(), "one", b"original").is_err(), "changed data cannot recover the earlier authority");
        assert!(bind(home.path(), "one", b"original", &authority).unwrap());
        forget(home.path(), "one");
        assert!(lookup(home.path(), "one", b"original").is_err());
        assert!(!bind(home.path(), "host", b"original", &io_authority::capture()).unwrap());
    }
}
