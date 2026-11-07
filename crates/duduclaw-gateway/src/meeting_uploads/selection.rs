//! An authenticated dashboard user's explicit file/agent confirmation authorizes
//! one import for five minutes. This does not prove a Google Picker click;
//! import separately verifies the file's app authorization via Drive metadata.
//! Store only a digest of the random receipt; consuming it is an atomic DELETE.
use std::path::Path;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

pub(super) const TTL_SECONDS: i64 = 5 * 60;
const REFUSED: &str = "選檔確認已失效，請重新確認要匯入的檔案與員工。";

fn open(home: &Path) -> Result<Connection, String> {
    let c = Connection::open(home.join("meeting_jobs.db")).map_err(|_| REFUSED)?;
    c.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| REFUSED)?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS meeting_drive_selections (
        receipt_hash TEXT PRIMARY KEY, owner TEXT NOT NULL, agent TEXT NOT NULL,
        file_id TEXT NOT NULL, expires INTEGER NOT NULL
    ); CREATE INDEX IF NOT EXISTS meeting_drive_selection_owner ON meeting_drive_selections(owner);
    CREATE INDEX IF NOT EXISTS meeting_drive_selection_expiry ON meeting_drive_selections(expires);").map_err(|_| REFUSED)?;
    Ok(c)
}

pub(super) fn issue(home: &Path, owner: &str, agent: &str, file: &str) -> Result<String, String> {
    if owner.is_empty() || owner.len() > 256 || agent.is_empty() || agent.len() > 256
        || !super::drive::valid_file_id(file) { return Err(REFUSED.into()); }
    let mut c = open(home)?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_| REFUSED)?;
    let now = chrono::Utc::now().timestamp();
    tx.execute("DELETE FROM meeting_drive_selections WHERE expires<=?1", [now]).map_err(|_| REFUSED)?;
    // A new confirmation for this employee supersedes the previous receipt.
    tx.execute("DELETE FROM meeting_drive_selections WHERE owner=?1 AND agent=?2", params![owner, agent]).map_err(|_| REFUSED)?;
    let count: u64 = tx.query_row("SELECT COUNT(*) FROM meeting_drive_selections WHERE owner=?1", [owner], |r| r.get(0)).map_err(|_| REFUSED)?;
    if count >= 8 { return Err(REFUSED.into()); }
    let receipt = hex::encode(rand::random::<[u8; 32]>());
    let digest = hex::encode(Sha256::digest(receipt.as_bytes()));
    tx.execute("INSERT INTO meeting_drive_selections VALUES (?1,?2,?3,?4,?5)", params![digest, owner, agent, file, now + TTL_SECONDS]).map_err(|_| REFUSED)?;
    tx.commit().map_err(|_| REFUSED)?;
    Ok(receipt)
}

pub(super) fn consume(home: &Path, owner: &str, agent: &str, file: &str, receipt: Option<&str>) -> Result<(), String> {
    let receipt = receipt.filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or(REFUSED)?;
    let digest = hex::encode(Sha256::digest(receipt.as_bytes()));
    let c = open(home)?;
    let changed = c.execute("DELETE FROM meeting_drive_selections WHERE receipt_hash=?1 AND owner=?2 AND agent=?3 AND file_id=?4 AND expires>?5",
        params![digest, owner, agent, file, chrono::Utc::now().timestamp()]).map_err(|_| REFUSED)?;
    if changed != 1 { return Err(REFUSED.into()); }
    Ok(())
}

pub(super) fn purge_expired(home: &Path) -> Result<(), String> {
    open(home)?.execute("DELETE FROM meeting_drive_selections WHERE expires<=?1", [chrono::Utc::now().timestamp()]).map_err(|_| REFUSED)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const FILE: &str = "recording_123456";
    #[test]
    fn selection_receipts_fail_closed_for_missing_expired_replayed_and_wrong_bindings() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert!(consume(home, "owner", "alice", FILE, None).is_err());
        assert!(consume(home, "owner", "alice", FILE, Some("garbage")).is_err());
        let receipt = issue(home, "owner", "alice", FILE).unwrap();
        for (owner, agent, file) in [("other", "alice", FILE), ("owner", "bob", FILE), ("owner", "alice", "other_file_123456")] {
            assert!(consume(home, owner, agent, file, Some(&receipt)).is_err());
        }
        consume(home, "owner", "alice", FILE, Some(&receipt)).unwrap();
        assert!(consume(home, "owner", "alice", FILE, Some(&receipt)).is_err());
        let receipt = issue(home, "owner", "alice", FILE).unwrap();
        open(home).unwrap().execute("UPDATE meeting_drive_selections SET expires=?1", [chrono::Utc::now().timestamp()]).unwrap();
        assert!(consume(home, "owner", "alice", FILE, Some(&receipt)).is_err());
        purge_expired(home).unwrap();
        let remaining: u64 = open(home).unwrap().query_row("SELECT COUNT(*) FROM meeting_drive_selections", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0);
    }
    #[test]
    fn newer_confirmation_invalidates_previous_receipt_and_only_one_connection_can_consume() {
        let home = tempfile::tempdir().unwrap();
        let old = issue(home.path(), "owner", "alice", FILE).unwrap();
        let receipt = issue(home.path(), "owner", "alice", FILE).unwrap();
        assert!(consume(home.path(), "owner", "alice", FILE, Some(&old)).is_err());
        let path = home.path().to_path_buf();
        let results = std::thread::scope(|s| {
            let first = s.spawn(|| consume(&path, "owner", "alice", FILE, Some(&receipt)));
            let second = s.spawn(|| consume(&path, "owner", "alice", FILE, Some(&receipt)));
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    }
}

/// Per-dashboard-user sliding window, shared by all selection requests on a router.
/// Count authenticated attempts, including malformed requests, before provider work.
#[derive(Default)]
pub(super) struct RegistrationRate {
    hits: std::sync::Mutex<std::collections::HashMap<String, std::collections::VecDeque<std::time::Instant>>>,
}
impl RegistrationRate {
    pub(super) fn allow(&self, user: &str, now: std::time::Instant) -> bool {
        let window = std::time::Duration::from_secs(60);
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        hits.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < window));
        let queue = hits.entry(user.to_owned()).or_default();
        while queue.front().is_some_and(|t| now.duration_since(*t) >= window) {
            queue.pop_front();
        }
        if queue.len() >= 20 { return false; }
        queue.push_back(now);
        true
    }
}

#[cfg(test)]
#[test]
fn confirmation_rate_is_per_user_and_recovers_after_one_minute() {
    let rate = RegistrationRate::default();
    let now = std::time::Instant::now();
    for _ in 0..20 { assert!(rate.allow("alice", now)); }
    assert!(!rate.allow("alice", now));
    assert!(rate.allow("bob", now));
    assert!(!rate.allow("alice", now + std::time::Duration::from_secs(59)));
    assert!(rate.allow("alice", now + std::time::Duration::from_secs(60)));
}
