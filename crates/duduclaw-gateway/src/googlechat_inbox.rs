//! Durable notification handoff. Only resource names are stored, never Chat text.
//! A crash during a turn is uncertain and must not automatically repeat effects.
use std::{collections::HashSet, path::{Path, PathBuf}, sync::{Arc, Mutex, OnceLock, Weak}};
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
pub(crate) struct Inbox { conn: Mutex<Connection>, home: PathBuf }
pub(crate) struct Pending { pub id: String, pub payload: String }
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission { Inserted, Duplicate, Full }

/// Resource references only; app_email pins the sender across app reconfiguration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingPlaceholder {
    pub message_name: String,
    pub space: String,
    pub app_agent: Option<String>,
    pub app_email: String,
    pub employee_id: String,
    pub session_id: String,
    pub thread_name: Option<String>,
    pub created_at: i64,
    pub state: String,
}

/// Accept only canonical Chat resource components, before constructing URLs.
pub(crate) fn parse_message_resource_name(name: &str) -> Option<(&str, &str)> {
    let mut parts = name.split('/');
    let (Some("spaces"), Some(space), Some("messages"), Some(message), None) =
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else { return None; };
    let valid = |id: &str| (1..=128).contains(&id.len()) && !matches!(id, "." | "..")
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    (valid(space) && valid(message)).then_some((space, message))
}

impl PendingPlaceholder {
    pub(crate) fn valid(&self) -> bool {
        parse_message_resource_name(&self.message_name).is_some_and(|(space, message)| {
            self.space == format!("spaces/{space}")
                && message.strip_prefix("client-").is_some_and(|id| {
                    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
        })
            && self.app_agent.as_ref().is_none_or(|s| s.len() <= 128)
            && self.employee_id.len() <= 128 && self.app_email.len() <= 320
            && self.session_id.len() <= 512 && self.thread_name.as_ref().is_none_or(|s| s.len() <= 512)
            && matches!(self.state.as_str(), "pending" | "cleanup_only")
    }
}

// Home is part of the key: independent gateways/test homes cannot hide each
// other's rows. All registrations in the process share this set.
fn active() -> &'static Mutex<HashSet<(PathBuf, String)>> {
    static ACTIVE: OnceLock<Mutex<HashSet<(PathBuf, String)>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

/// Drop releases liveness, preserving evidence for same-process recovery.
pub(crate) struct PlaceholderRegistration {
    store: Arc<Inbox>,
    name: String,
    retired: std::sync::atomic::AtomicBool,
}
impl PlaceholderRegistration {
    pub(crate) fn new(store: Arc<Inbox>, row: &PendingPlaceholder) -> Result<Self, String> {
        // Publish liveness atomically with persistence from the sweep's view.
        let mut active = active().lock().map_err(|_| "placeholder registry unavailable")?;
        store.insert_placeholder(row)?;
        active.insert((store.home.clone(), row.message_name.clone()));
        Ok(Self { store, name: row.message_name.clone(), retired: false.into() })
    }

    pub(crate) fn home(&self) -> &Path { &self.store.home }

    pub(crate) fn cleanup_only(&self) -> Result<(), String> {
        self.set_state("cleanup_only")
    }

    pub(crate) fn set_state(&self, state: &str) -> Result<(), String> {
        self.store.placeholder_access(|conn| conn.execute(
            "UPDATE pending_placeholders SET state=?2 WHERE message_name=?1", params![self.name, state]).map(|_| ()))
    }

    pub(crate) fn retire(&self) {
        if self.retired.load(std::sync::atomic::Ordering::Relaxed) { return; }
        if self.store.remove_placeholder(&self.name).is_err() {
            tracing::warn!(failed = 1, "Google Chat placeholder retirement failed");
        } else {
            self.retired.store(true, std::sync::atomic::Ordering::Relaxed);
            active().lock().unwrap_or_else(|e| e.into_inner()).remove(&(self.store.home.clone(), self.name.clone()));
        }
    }
}
impl Drop for PlaceholderRegistration {
    fn drop(&mut self) {
        active().lock().unwrap_or_else(|e| e.into_inner()).remove(&(self.store.home.clone(), self.name.clone()));
    }
}

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
        #[cfg(unix)]
        prepare_private(&path, true)?;
        #[cfg(unix)]
        for suffix in ["-wal", "-shm", "-journal"] {
            prepare_private(&PathBuf::from(format!("{}{suffix}", path.display())), false)?;
        }
        #[cfg(unix)]
        let mut conn = Connection::open_with_flags(&path, rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .map_err(|_| "無法開啟 Google Chat 耐久收件匣")?;
        // HTTP Google Chat is also supported on Windows. Reuse the shared
        // private SQLite opener there (including its documented home-ACL trust).
        #[cfg(not(unix))]
        let mut conn = duduclaw_core::private_sqlite::open(home, "googlechat_inbox.db", duduclaw_core::private_sqlite::Access::Create)
            .map_err(|_| "無法開啟 Google Chat 耐久收件匣")?
            .ok_or("無法開啟 Google Chat 耐久收件匣")?;
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
            CREATE TABLE IF NOT EXISTS pending_placeholders (
                message_name TEXT PRIMARY KEY NOT NULL, space TEXT NOT NULL,
                app_agent TEXT, app_email TEXT NOT NULL, employee_id TEXT NOT NULL,
                session_id TEXT NOT NULL, thread_name TEXT, created_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS pending_placeholders_age ON pending_placeholders(created_at);
            UPDATE notifications SET status='pending' WHERE status='queued';
            UPDATE notifications SET status='uncertain',payload=NULL WHERE status='running';")
            .map_err(|_| "Google Chat 收件匣初始化失敗")?;
        #[cfg(unix)]
        for suffix in ["", "-wal", "-shm"] {
            prepare_private(&PathBuf::from(format!("{}{suffix}", path.display())), false)?;
        }
        // v1 databases already have the table. Never rebuild/drop recovery rows.
        migrate_placeholder_state(&mut conn)?;
        let store = Self { conn: Mutex::new(conn), home: home.canonicalize().map_err(|_| "placeholder home unavailable")? };
        store.prune(chrono::Utc::now().timestamp())?;
        Ok(store)
    }

    // Placeholder maintenance must not spend its sweep budget waiting for
    // another SQLite writer or this process's notification operations.
    fn placeholder_access<T>(&self, work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, String> {
        let conn = self.conn.try_lock().map_err(|_| "placeholder store busy")?;
        conn.busy_timeout(std::time::Duration::ZERO).map_err(|_| "placeholder store unavailable")?;
        let result = work(&conn);
        conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| "placeholder store unavailable")?;
        result.map_err(|_| "placeholder store operation failed".into())
    }

    pub(crate) fn insert_placeholder(&self, row: &PendingPlaceholder) -> Result<(), String> {
        if !row.valid() { return Err("invalid placeholder reference".into()); }
        self.placeholder_access(|conn| conn.execute(
            "INSERT INTO pending_placeholders(message_name,space,app_agent,app_email,employee_id,session_id,thread_name,created_at,state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![row.message_name,row.space,row.app_agent,row.app_email,row.employee_id,row.session_id,row.thread_name,row.created_at,row.state]).map(|_| ()))
    }

    pub(crate) fn remove_placeholder(&self, name: &str) -> Result<(), String> {
        self.placeholder_access(|conn| conn.execute(
            "DELETE FROM pending_placeholders WHERE message_name=?1", [name]).map(|_| ()))
    }

    #[cfg(test)]
    pub(crate) fn stale_placeholders(&self, now: i64) -> Result<Vec<PendingPlaceholder>, String> {
        self.read_placeholders(now.saturating_sub(5), false).map(|rows| rows.into_iter().filter_map(|(_, row)| row).collect())
    }

    pub(crate) fn placeholder_active(&self, name: &str) -> bool {
        active().lock().map(|a| a.contains(&(self.home.clone(), name.to_owned()))).unwrap_or(true)
    }

    pub(crate) fn inactive_placeholders_before(&self, cutoff: i64) -> Result<Vec<(i64, Option<PendingPlaceholder>)>, String> {
        self.read_placeholders(cutoff, true)
    }

    fn read_placeholders(&self, cutoff: i64, exclude_active: bool) -> Result<Vec<(i64, Option<PendingPlaceholder>)>, String> {
        let active = active().lock().map_err(|_| "placeholder registry unavailable")?;
        let names: Vec<_> = active.iter().filter(|(home, _)| exclude_active && home == &self.home).map(|(_, name)| name).collect();
        let names = serde_json::to_string(&names).map_err(|_| "placeholder registry unavailable")?;
        self.placeholder_access(|conn| {
            let mut query = conn.prepare("SELECT rowid,message_name,space,app_agent,app_email,employee_id,session_id,thread_name,created_at,state FROM pending_placeholders WHERE created_at < ?1 AND message_name NOT IN (SELECT value FROM json_each(?2)) ORDER BY created_at,message_name LIMIT 200")?;
            let rows = query.query_map(params![cutoff, names], |r| {
                let rowid: i64 = r.get(0)?;
                // Validate borrowed SQLite values before allocating any stored
                // string. Even corrupt legacy rows cannot allocate huge fields.
                for (column, limit, optional) in [(1, 273, false), (2, 135, false), (3, 128, true),
                    (4, 320, false), (5, 128, false), (6, 512, false), (7, 512, true), (9, 12, false)] {
                    match r.get_ref(column)? {
                        rusqlite::types::ValueRef::Null if optional => {},
                        rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= limit && std::str::from_utf8(bytes).is_ok() => {},
                        _ => return Ok((rowid, None)),
                    }
                }
                let Ok(created_at) = r.get(8) else { return Ok((rowid, None)); };
                Ok((rowid, Some(PendingPlaceholder {
                    message_name: r.get(1)?, space: r.get(2)?, app_agent: r.get(3)?, app_email: r.get(4)?,
                    employee_id: r.get(5)?, session_id: r.get(6)?, thread_name: r.get(7)?, created_at, state: r.get(9)?,
                })))
            })?;
            rows.collect()
        })
    }

    pub(crate) fn remove_placeholder_row(&self, rowid: i64) -> Result<(), String> {
        self.placeholder_access(|conn| conn.execute("DELETE FROM pending_placeholders WHERE rowid=?1", [rowid]).map(|_| ()))
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

fn migrate_placeholder_state(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "Google Chat placeholder migration failed".to_owned())?;
    if !placeholder_state_column(&tx)? {
        if let Err(error) = tx.execute_batch("ALTER TABLE pending_placeholders ADD COLUMN state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','cleanup_only'));") {
            let duplicate_column = error.to_string().to_ascii_lowercase().contains("duplicate column name");
            if !duplicate_column || !placeholder_state_column(&tx)? {
                return Err("Google Chat placeholder migration failed".to_owned());
            }
        }
    }
    if !placeholder_state_column(&tx)? {
        return Err("Google Chat placeholder migration failed".to_owned());
    }
    tx.commit().map_err(|_| "Google Chat placeholder migration failed".to_owned())
}

fn placeholder_state_column(conn: &Connection) -> Result<bool, String> {
    let mut query = conn.prepare("PRAGMA table_info(pending_placeholders)")
        .map_err(|_| "Google Chat placeholder migration failed")?;
    let columns = query.query_map([], |row| Ok((
        row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
        row.get::<_, Option<String>>(4)?,
    )))
        .map_err(|_| "Google Chat placeholder migration failed")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| "Google Chat placeholder migration failed")?;
    match columns.iter().find(|(name, _, _, _)| name == "state") {
        None => Ok(false),
        Some((_, column_type, not_null, default)) => Ok(
            column_type.eq_ignore_ascii_case("TEXT") && *not_null == 1
                && default.as_deref() == Some("'pending'"),
        ),
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
    fn placeholder() -> PendingPlaceholder {
        PendingPlaceholder {
            message_name: format!("spaces/A/messages/client-{}", "a".repeat(32)), space: "spaces/A".into(),
            app_agent: Some("sales".into()), app_email: "sales@test.invalid".into(), employee_id: "sales".into(),
            session_id: "session".into(), thread_name: None, created_at: 1, state: "pending".into(),
        }
    }

    #[test]
    fn message_resource_name_accepts_only_canonical_bounded_components() {
        for name in [format!("spaces/A/messages/client-{}", "a".repeat(32)), "spaces/a_.-Z/messages/m_.-9".into()] {
            assert!(parse_message_resource_name(&name).is_some(), "{name}");
        }
        let max = "a".repeat(128);
        assert!(parse_message_resource_name(&format!("spaces/{max}/messages/{max}")).is_some());
        for id in ["", ".", "..", "a?x=y", "a#fragment", "%2e%2e", "a/b", "空白", "a b", &"a".repeat(129)] {
            assert!(parse_message_resource_name(&format!("spaces/{id}/messages/m")).is_none(), "space {id}");
            assert!(parse_message_resource_name(&format!("spaces/A/messages/{id}")).is_none(), "message {id}");
        }
        for name in ["spaces/A/messages/m/extra", "/spaces/A/messages/m", "spaces/A/messages/m/", "spaces/A/message/m", "spaces/A/messages"] {
            assert!(parse_message_resource_name(name).is_none(), "{name}");
        }
    }

    #[test]
    fn placeholder_insert_bounds_every_field() {
        let home = tempfile::tempdir().unwrap();
        let store = Inbox::open(home.path()).unwrap();
        let mut boundary = placeholder();
        boundary.app_agent = Some("a".repeat(128)); boundary.employee_id = "a".repeat(128);
        boundary.app_email = "e".repeat(320); boundary.session_id = "s".repeat(512);
        boundary.thread_name = Some("t".repeat(512));
        store.insert_placeholder(&boundary).unwrap();
        store.remove_placeholder(&boundary.message_name).unwrap();
        for field in 0..8 {
            let mut row = placeholder();
            match field {
                0 => row.message_name = "spaces/../messages/m".into(),
                1 => row.space = "spaces/B".into(),
                2 => row.app_agent = Some("a".repeat(129)),
                3 => row.employee_id = "a".repeat(129),
                4 => row.app_email = "e".repeat(321),
                5 => row.session_id = "s".repeat(513),
                6 => row.thread_name = Some("t".repeat(513)),
                _ => row.state = "unknown".into(),
            }
            assert!(store.insert_placeholder(&row).is_err(), "field {field}");
        }
    }

    #[test]
    fn placeholder_v1_migration_is_additive_and_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let conn = Connection::open(home.path().join("googlechat_inbox.db")).unwrap();
        conn.execute_batch(include_str!("../../duduclaw-import/src/googlechat-placeholders-v183.sql")).unwrap();
        conn.execute(&format!("INSERT INTO pending_placeholders VALUES ('spaces/A/messages/client-{}','spaces/A','sales','sales@test.invalid','sales','session',NULL,1)", "a".repeat(32)), []).unwrap();
        drop(conn);
        for _ in 0..2 {
            let store = Inbox::open(home.path()).unwrap();
            assert_eq!(store.stale_placeholders(100).unwrap(), vec![placeholder()]);
            assert_eq!(store.conn.lock().unwrap().query_row(
                "SELECT count(*) FROM pragma_table_info('pending_placeholders') WHERE name='state'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        }
    }

    #[test]
    fn placeholder_v1_migration_is_safe_across_concurrent_connections() {
        use std::sync::{Arc, Barrier};
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("googlechat_inbox.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../../duduclaw-import/src/googlechat-placeholders-v183.sql")).unwrap();
        drop(conn);

        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2).map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut conn = Connection::open(path).unwrap();
                barrier.wait();
                migrate_placeholder_state(&mut conn).unwrap();
            })
        }).collect();
        for handle in handles { handle.join().unwrap(); }

        let conn = Connection::open(path).unwrap();
        assert!(placeholder_state_column(&conn).unwrap());
        let state_columns: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info('pending_placeholders') WHERE name='state'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(state_columns, 1);
    }

    #[test]
    fn placeholders_survive_reopen_and_retire_without_changing_inbound_dedupe() {
        let home = tempfile::tempdir().unwrap();
        let row = PendingPlaceholder {
            message_name: format!("spaces/A/messages/client-{}", "a".repeat(32)), space: "spaces/A".into(),
            app_agent: Some("sales".into()), app_email: "sales@test.invalid".into(), employee_id: "sales".into(),
            session_id: "googlechat:spaces/A:sales".into(), thread_name: Some("spaces/A/threads/T".into()),
            created_at: chrono::Utc::now().timestamp() - 10, state: "pending".into(),
        };
        let store = Inbox::open(home.path()).unwrap();
        store.insert_placeholder(&row).unwrap();
        store.persist("app", "MESSAGE:spaces/A/messages/inbound", "reference").unwrap();
        store.transition("app", "MESSAGE:spaces/A/messages/inbound", "pending", "running").unwrap();
        drop(store);
        let store = Inbox::open(home.path()).unwrap();
        assert_eq!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap(), vec![row.clone()]);
        store.remove_placeholder(&row.message_name).unwrap();
        assert!(store.stale_placeholders(chrono::Utc::now().timestamp()).unwrap().is_empty());
        assert!(store.diagnostic("app").unwrap().is_some());
        assert_eq!(store.persist("app", "MESSAGE:spaces/A/messages/inbound", "reference").unwrap(), Admission::Duplicate);
    }

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
