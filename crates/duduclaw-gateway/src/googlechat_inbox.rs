//! Durable notification handoff. Only resource names are stored, never Chat text.
//! A crash during a turn is uncertain and must not automatically repeat effects.
use std::{path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock, Weak}};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const MAX_MESSAGE_AGE_SECS: i64 = 7 * 24 * 60 * 60;
pub(crate) const CLOCK_SKEW_SECS: i64 = 300;
// A future-dated resource can remain admissible beyond its local received_at
// age. Cover that skew plus one more skew interval as a safety margin.
pub(crate) const RETENTION_SECS: i64 = MAX_MESSAGE_AGE_SECS + 2 * CLOCK_SKEW_SECS;
const MAX_ROWS: i64 = 10_000;
const MAX_PENDING: i64 = 100;
const MAX_STREAM_ROWS: i64 = 1_000;
const MAX_STREAM_PENDING: i64 = 10;
const MAX_PAYLOAD: usize = 2048;
pub(crate) struct Inbox { conn: Mutex<Connection> }
pub(crate) struct Pending { pub id: String, pub payload: String }
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission { Inserted, Duplicate, Full }

impl Inbox {
    pub(crate) fn shared(home: &Path) -> Result<Arc<Self>, String> {
        static STORES: OnceLock<Mutex<Vec<(PathBuf, Weak<Inbox>)>>> = OnceLock::new();
        let home = home.canonicalize().map_err(|_| "無法開啟 Google Chat 收件匣目錄")?;
        let mut stores = STORES.get_or_init(Default::default).lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?;
        stores.retain(|(_, s)| s.strong_count() > 0);
        if let Some(store) = stores.iter().find(|(p, _)| p == &home).and_then(|(_, s)| s.upgrade()) { return Ok(store); }
        let store = Arc::new(Self::open(&home)?);
        stores.push((home, Arc::downgrade(&store)));
        Ok(store)
    }

    fn open(home: &Path) -> Result<Self, String> {
        let path = home.canonicalize().map_err(|_| "無法開啟 Google Chat 收件匣目錄")?.join("googlechat_inbox.db");
        prepare_private(&path, true)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            prepare_private(&PathBuf::from(format!("{}{suffix}", path.display())), false)?;
        }
        let conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .map_err(|_| "無法開啟 Google Chat 耐久收件匣")?;
        // 64 MiB database ceiling (4 KiB pages); checkpoints bound WAL growth.
        conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA page_size=4096;
            PRAGMA max_page_count=16384; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            PRAGMA secure_delete=ON; PRAGMA wal_autocheckpoint=64; PRAGMA journal_size_limit=1048576;
            CREATE TABLE IF NOT EXISTS notifications (
                stream TEXT NOT NULL, event_key TEXT NOT NULL, payload TEXT,
                status TEXT NOT NULL, received_at INTEGER NOT NULL,
                PRIMARY KEY(stream,event_key));
            CREATE INDEX IF NOT EXISTS notifications_pending ON notifications(stream,status,received_at);
            CREATE INDEX IF NOT EXISTS notifications_age ON notifications(received_at);
            UPDATE notifications SET status='pending' WHERE status='queued';
            UPDATE notifications SET status='uncertain',payload=NULL WHERE status='running';")
            .map_err(|_| "Google Chat 收件匣初始化失敗")?;
        for suffix in ["", "-wal", "-shm"] {
            prepare_private(&PathBuf::from(format!("{}{suffix}", path.display())), false)?;
        }
        let store = Self { conn: Mutex::new(conn) };
        store.prune(chrono::Utc::now().timestamp())?;
        Ok(store)
    }

    pub(crate) fn prune(&self, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?;
        // Never evict a live turn to make room: that could permit a replay.
        conn.execute("DELETE FROM notifications WHERE received_at < ?1 AND status NOT IN ('queued','running')",
            [now - RETENTION_SECS]).map_err(|_| "Google Chat 收件匣清理失敗")?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").map_err(|_| "Google Chat 收件匣同步失敗".into())
    }

    /// Recovery references that can no longer be verified have caused no
    /// side effects and must not consume the long-lived replay budget.
    pub(crate) fn discard_pending(&self, stream: &str, id: &str) -> Result<(), String> {
        self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?.execute(
            "DELETE FROM notifications WHERE stream=?1 AND event_key=?2 AND status='pending'", params![stream,id])
            .map(|_| ()).map_err(|_| "Google Chat 無效通知清理失敗".into())
    }

    pub(crate) fn contains(&self, stream: &str, id: &str) -> Result<bool, String> {
        self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?.query_row(
            "SELECT 1 FROM notifications WHERE stream=?1 AND event_key=?2", params![stream,id], |_| Ok(())).optional()
            .map(|v| v.is_some()).map_err(|_| "無法讀取 Google Chat 收件匣".into())
    }

    /// Per-stream quotas reserve capacity for other apps under global caps.
    /// Neither cap evicts unexpired dedupe evidence. Capacity
    /// refusal is an intentional ACK/drop; database errors are not ACKed.
    pub(crate) fn persist(&self, stream: &str, id: &str, payload: &str) -> Result<Admission, String> {
        if stream.len() > 1024 || id.len() > 1024 || payload.len() > MAX_PAYLOAD {
            return Err("Google Chat 通知識別資料過大".into());
        }
        let write = || -> rusqlite::Result<Admission> {
            let mut conn = self.conn.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let exists = tx.query_row("SELECT 1 FROM notifications WHERE stream=?1 AND event_key=?2",
                params![stream,id], |_| Ok(())).optional()?.is_some();
            if exists { return Ok(Admission::Duplicate); }
            let (stream_rows, stream_pending): (i64, i64) = tx.query_row(
                "SELECT count(*),coalesce(sum(status IN ('pending','queued','running')),0) FROM notifications WHERE stream=?1", [stream],
                |r| Ok((r.get(0)?, r.get(1)?)))?;
            if stream_rows >= MAX_STREAM_ROWS || stream_pending >= MAX_STREAM_PENDING { return Ok(Admission::Full); }
            let (rows, pending): (i64, i64) = tx.query_row(
                "SELECT count(*),coalesce(sum(status IN ('pending','queued','running')),0) FROM notifications", [],
                |r| Ok((r.get(0)?, r.get(1)?)))?;
            if rows >= MAX_ROWS || pending >= MAX_PENDING { return Ok(Admission::Full); }
            tx.execute("INSERT INTO notifications(stream,event_key,payload,status,received_at) VALUES (?1,?2,?3,'pending',?4)",
                params![stream,id,payload,chrono::Utc::now().timestamp()])?;
            tx.commit()?;
            Ok(Admission::Inserted)
        };
        write().map_err(|_| "Google Chat 收件匣寫入失敗；尚未 ACK".into())
    }

    pub(crate) fn pending(&self, stream: &str) -> Result<Vec<Pending>, String> {
        let conn = self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?;
        let mut query = conn.prepare("SELECT event_key,payload FROM notifications WHERE stream=?1 AND status='pending' ORDER BY received_at,rowid LIMIT 100")
            .map_err(|_| "無法查詢 Google Chat 待處理事件")?;
        query.query_map([stream], |r| Ok(Pending { id: r.get(0)?, payload: r.get(1)? }))
            .map_err(|_| "無法讀取 Google Chat 待處理事件")?.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|_| "Google Chat 待處理事件損毀".into())
    }

    pub(crate) fn transition(&self, stream: &str, id: &str, from: &str, to: &str) -> Result<bool, String> {
        self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?.execute(
            "UPDATE notifications SET status=?4,payload=CASE WHEN ?4 IN ('done','dropped','uncertain') THEN NULL ELSE payload END WHERE stream=?1 AND event_key=?2 AND status=?3",
            params![stream,id,from,to]).map(|n| n == 1).map_err(|_| "Google Chat 收件匣狀態寫入失敗".into())
    }

    pub(crate) fn diagnostic(&self, stream: &str) -> Result<Option<String>, String> {
        self.conn.lock().map_err(|_| "Google Chat 收件匣鎖定失敗")?.query_row(
            "SELECT 'Google Chat 上次處理中斷；為避免重複執行，不會自動重跑，識別資料保留七天。' FROM notifications WHERE stream=?1 AND status='uncertain' LIMIT 1",
            [stream], |r| r.get(0)).optional().map_err(|_| "無法讀取 Google Chat 診斷".into())
    }
}

fn prepare_private(path: &Path, create: bool) -> Result<(), String> {
    #[cfg(not(unix))]
    { let _ = (path, create); Err("Google Chat Pub/Sub 收件匣目前需要 Unix 檔案權限防護".into()) }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.is_file() || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
                    return Err("Google Chat 收件匣拒絕連結、非一般檔案或其他擁有者".into());
                }
                // Do not open/close an existing SQLite file (POSIX lock lifetime).
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| "無法限制 Google Chat 收件匣權限")?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if create {
                    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                        .custom_flags(libc::O_NOFOLLOW).open(path).map_err(|_| "無法建立私有 Google Chat 收件匣")?;
                }
            }
            Err(_) => return Err("無法檢查 Google Chat 收件匣權限".into()),
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod googlechat_inbox_tests {
    use super::*;
    #[test]
    fn recovery_replays_queued_but_never_uncertain_resource() {
        let home = tempfile::tempdir().unwrap();
        let s = Inbox::open(home.path()).unwrap();
        assert_eq!(s.persist("app", "MESSAGE:spaces/A/messages/one", "reference").unwrap(), Admission::Inserted);
        s.transition("app", "MESSAGE:spaces/A/messages/one", "pending", "queued").unwrap();
        drop(s);
        let s = Inbox::open(home.path()).unwrap();
        assert_eq!(s.pending("app").unwrap().len(), 1);
        s.transition("app", "MESSAGE:spaces/A/messages/one", "pending", "running").unwrap();
        drop(s);
        let s = Inbox::open(home.path()).unwrap();
        assert!(s.pending("app").unwrap().is_empty());
        assert!(s.diagnostic("app").unwrap().is_some());
        // Pub/Sub messageId is deliberately absent from the dedupe key.
        assert_eq!(s.persist("app", "MESSAGE:spaces/A/messages/one", "new-delivery").unwrap(), Admission::Duplicate);
        let payload: Option<String> = s.conn.lock().unwrap().query_row("SELECT payload FROM notifications", [], |r| r.get(0)).unwrap();
        assert!(payload.is_none());
    }
    #[test]
    fn dedupe_survives_future_clock_skew_through_last_admissible_second() {
        let home = tempfile::tempdir().unwrap();
        let s = Inbox::open(home.path()).unwrap();
        let received = chrono::Utc::now().timestamp();
        let created = received + CLOCK_SKEW_SECS;
        s.persist("app", "skewed", "reference").unwrap();
        s.transition("app", "skewed", "pending", "done").unwrap();
        s.conn.lock().unwrap().execute("UPDATE notifications SET received_at=?1", [received]).unwrap();
        // The old seven-day retention would delete this row one second later,
        // although the trusted resource is still within its acceptance window.
        for now in [received + MAX_MESSAGE_AGE_SECS + 1, created + MAX_MESSAGE_AGE_SECS,
            received + RETENTION_SECS] {
            s.prune(now).unwrap();
            assert_eq!(s.persist("app", "skewed", "reference").unwrap(), Admission::Duplicate);
        }
        s.prune(received + RETENTION_SECS + 1).unwrap();
        assert!(!s.contains("app", "skewed").unwrap());
    }
    #[test]
    fn retention_and_capacity_keep_unexpired_dedupe_evidence() {
        let home = tempfile::tempdir().unwrap();
        let s = Inbox::open(home.path()).unwrap();
        let now = chrono::Utc::now().timestamp();
        for id in ["old", "current", "running"] { s.persist("app", id, "reference").unwrap(); }
        s.transition("app", "old", "pending", "done").unwrap();
        s.transition("app", "running", "pending", "running").unwrap();
        s.conn.lock().unwrap().execute("UPDATE notifications SET received_at=?1 WHERE event_key IN ('old','running')", [now - RETENTION_SECS - 1]).unwrap();
        s.prune(now).unwrap();
        assert!(!s.contains("app", "old").unwrap());
        assert!(s.contains("app", "current").unwrap());
        assert!(s.contains("app", "running").unwrap());
        {
            let mut conn = s.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            for n in 0..MAX_STREAM_ROWS - 2 {
                tx.execute("INSERT INTO notifications(stream,event_key,status,received_at) VALUES ('app',?1,'done',?2)",
                    params![format!("id-{n}"),now]).unwrap();
            }
            tx.commit().unwrap();
        }
        assert_eq!(s.persist("app", "over-cap", "reference").unwrap(), Admission::Full);
        assert_eq!(s.persist("app", "current", "reference").unwrap(), Admission::Duplicate);
    }
    #[test]
    fn row_capacity_isolated_per_stream_preserves_dedupe() {
        let home = tempfile::tempdir().unwrap();
        let s = Inbox::open(home.path()).unwrap();
        for n in 0..MAX_STREAM_ROWS {
            let id = format!("id-{n}");
            assert_eq!(s.persist("app", &id, "reference").unwrap(), Admission::Inserted);
            s.transition("app", &id, "pending", "done").unwrap();
        }
        assert_eq!(s.persist("app", "over-cap", "reference").unwrap(), Admission::Full);
        assert_eq!(s.persist("app", "id-0", "reference").unwrap(), Admission::Duplicate);
        assert_eq!(s.persist("other-app", "new", "reference").unwrap(), Admission::Inserted);
        assert!(s.contains("app", "id-0").unwrap());
    }
    #[test]
    fn pending_capacity_isolated_per_stream_and_failed_recovery_releases_it() {
        let home = tempfile::tempdir().unwrap();
        let s = Inbox::open(home.path()).unwrap();
        for n in 0..MAX_STREAM_PENDING {
            let id = format!("id-{n}");
            assert_eq!(s.persist("app", &id, "reference").unwrap(), Admission::Inserted);
            // All three active states count against the same stream quota.
            if n % 3 != 0 { s.transition("app", &id, "pending", "queued").unwrap(); }
            if n % 3 == 2 { s.transition("app", &id, "queued", "running").unwrap(); }
        }
        assert_eq!(s.persist("app", "over-cap", "reference").unwrap(), Admission::Full);
        assert_eq!(s.persist("app", "id-0", "reference").unwrap(), Admission::Duplicate);
        assert_eq!(s.persist("other-app", "new", "reference").unwrap(), Admission::Inserted);
        s.discard_pending("app", "id-0").unwrap();
        assert_eq!(s.persist("app", "replacement", "reference").unwrap(), Admission::Inserted);
        s.transition("other-app", "new", "pending", "running").unwrap();
        s.discard_pending("other-app", "new").unwrap();
        assert!(s.contains("other-app", "new").unwrap(), "never delete a started turn's replay evidence");
    }
    #[test]
    fn global_row_and_pending_caps_still_apply_across_streams() {
        for (limit, status) in [(MAX_ROWS, "done"), (MAX_PENDING, "pending")] {
            let home = tempfile::tempdir().unwrap();
            let s = Inbox::open(home.path()).unwrap();
            {
                let mut conn = s.conn.lock().unwrap();
                let tx = conn.transaction().unwrap();
                let per_stream = if status == "done" { MAX_STREAM_ROWS } else { MAX_STREAM_PENDING };
                for n in 0..limit {
                    tx.execute("INSERT INTO notifications(stream,event_key,status,received_at) VALUES (?1,?2,?3,?4)",
                        params![format!("app-{}", n / per_stream),format!("id-{n}"),status,chrono::Utc::now().timestamp()]).unwrap();
                }
                tx.commit().unwrap();
            }
            assert_eq!(s.persist("fresh-app", "new", "reference").unwrap(), Admission::Full);
            assert_eq!(s.persist("app-0", "id-0", "reference").unwrap(), Admission::Duplicate);
        }
    }
    #[test]
    fn private_database_rejects_links_and_hardens_existing_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("googlechat_inbox.db");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let store = Inbox::open(home.path()).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            assert_eq!(std::fs::metadata(format!("{}{suffix}", path.display())).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(store);
        let other = home.path().join("other");
        std::fs::hard_link(&path, &other).unwrap();
        assert!(Inbox::open(home.path()).is_err());
        std::fs::remove_file(other).unwrap();
        symlink(&path, home.path().join("googlechat_inbox.db-wal")).unwrap();
        assert!(Inbox::open(home.path()).is_err());
    }
}
