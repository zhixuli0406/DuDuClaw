//! Opening `channel_ingress.db`: private file creation, schema, migrations.
//!
//! Migrations run only when `PRAGMA user_version` is below
//! [`SCHEMA_VERSION`], so an open of a current database writes nothing
//! (review I-MEDIUM-5). The gateway shares one store per home
//! ([`IngressStore::shared`]); the operator CLI opens with
//! [`IngressStore::open_current`], which never migrates.

use super::IngressStore;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Schema version written to `PRAGMA user_version` after migration.
pub(crate) const SCHEMA_VERSION: i64 = 3;

const BASE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ingress (
      seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
      channel TEXT NOT NULL, account TEXT NOT NULL, event_id TEXT NOT NULL,
      revision TEXT NOT NULL, authorization_revision TEXT NOT NULL, conversation TEXT NOT NULL,
      status TEXT NOT NULL DEFAULT 'ready', attempt INTEGER NOT NULL DEFAULT 0,
      received_at INTEGER NOT NULL, reason TEXT, lease_id TEXT, lease_until INTEGER,
      run_id TEXT, run_authorization_id TEXT, decision_fastlane INTEGER NOT NULL DEFAULT 0, decision_binding TEXT,
      UNIQUE(channel, account, event_id));
    CREATE TABLE IF NOT EXISTS ingress_payload (
      id TEXT PRIMARY KEY REFERENCES ingress(id), payload TEXT NOT NULL, expires_at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS ingress_ready ON ingress(status,seq);
    CREATE INDEX IF NOT EXISTS ingress_conversation ON ingress(channel,account,conversation,seq);
    CREATE TABLE IF NOT EXISTS ingress_attempts (
      operation_id TEXT PRIMARY KEY, ingress_id TEXT NOT NULL, ordinal INTEGER NOT NULL,
      status TEXT NOT NULL, reason TEXT, finished_at INTEGER NOT NULL,
      retry_authorization_id TEXT);
    CREATE TABLE IF NOT EXISTS ingress_retry_authorizations (
      authorization_id TEXT PRIMARY KEY, ingress_id TEXT NOT NULL,
      predecessor_operation_id TEXT NOT NULL, actor TEXT NOT NULL, note TEXT NOT NULL,
      at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS ingress_attempts_by_event ON ingress_attempts(ingress_id,ordinal);
    CREATE INDEX IF NOT EXISTS ingress_authorizations_by_event ON ingress_retry_authorizations(ingress_id);
    CREATE TRIGGER IF NOT EXISTS ingress_attempts_no_update BEFORE UPDATE ON ingress_attempts BEGIN SELECT
        RAISE(ABORT,'immutable ingress attempt'); END;
    CREATE TABLE IF NOT EXISTS ingress_resolutions (
      id INTEGER PRIMARY KEY AUTOINCREMENT, ingress_id TEXT NOT NULL,
      actor TEXT NOT NULL, action TEXT NOT NULL, note TEXT NOT NULL, at INTEGER NOT NULL);";

const RUN_TRIGGERS: &str = "CREATE TRIGGER IF NOT EXISTS ingress_run_insert BEFORE INSERT ON ingress
        WHEN NEW.run_id IS NULL OR (NEW.run_authorization_id IS NULL AND NEW.run_id!=NEW.id)
        OR (NEW.run_authorization_id IS NOT NULL AND (NEW.run_id!=NEW.run_authorization_id OR NOT EXISTS(SELECT 1
        FROM ingress_retry_authorizations a WHERE a.authorization_id=NEW.run_authorization_id
        AND a.ingress_id=NEW.id))) BEGIN SELECT RAISE(ABORT,'invalid ingress run binding'); END;
    CREATE TRIGGER IF NOT EXISTS ingress_run_update BEFORE UPDATE OF run_id,run_authorization_id ON ingress
        WHEN NEW.run_id IS NULL OR (NEW.run_authorization_id IS NULL AND NEW.run_id!=NEW.id)
        OR (NEW.run_authorization_id IS NOT NULL AND (NEW.run_id!=NEW.run_authorization_id OR NOT EXISTS(SELECT 1
        FROM ingress_retry_authorizations a WHERE a.authorization_id=NEW.run_authorization_id
        AND a.ingress_id=NEW.id))) BEGIN SELECT RAISE(ABORT,'invalid ingress run binding'); END;";

/// Version 2 (F2): back-off, run start, receipt side tables, retention.
/// New data lives in side tables so positional inserts into the original
/// attempt / authorization tables keep their shape.
const V2_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ingress_attempt_receipts (
      operation_id TEXT PRIMARY KEY, provider_receipt TEXT, delivered_via TEXT, progress_note TEXT);
    CREATE TABLE IF NOT EXISTS ingress_run_confirmations (
      authorization_id TEXT PRIMARY KEY, action TEXT NOT NULL, confirmed_duplicate_risk INTEGER NOT NULL,
      provider_receipt TEXT);
    CREATE TABLE IF NOT EXISTS ingress_alerts (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
    CREATE INDEX IF NOT EXISTS ingress_by_received ON ingress(status,received_at);
    DROP TRIGGER IF EXISTS ingress_attempts_no_delete;
    CREATE TRIGGER IF NOT EXISTS ingress_attempts_delete_guard BEFORE DELETE ON ingress_attempts
        WHEN EXISTS(SELECT 1 FROM ingress WHERE id=OLD.ingress_id) BEGIN SELECT
        RAISE(ABORT,'immutable ingress attempt'); END;";

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    stmt.query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn add_missing(conn: &Connection, table: &str, wanted: &[(&str, &str)]) -> Result<(), String> {
    let have = columns(conn, table)?;
    for (name, decl) in wanted {
        if !have.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"), [])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Version 3 (F5): alerts are queued and summarized per kind and window
/// (review N4), so a burst becomes one Activity Feed row, not hundreds.
const V3_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ingress_alert_queue (
      id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, reason TEXT NOT NULL,
      subject TEXT NOT NULL, at INTEGER NOT NULL);";

fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(BASE_SCHEMA).map_err(|e| e.to_string())?;
    add_missing(
        conn,
        "ingress",
        &[
            ("run_id", "TEXT"),
            ("run_authorization_id", "TEXT"),
            ("decision_binding", "TEXT"),
            ("decision_fastlane", "INTEGER NOT NULL DEFAULT 0"),
            ("retry_at", "INTEGER"),
            ("unavailable_count", "INTEGER NOT NULL DEFAULT 0"),
            ("run_started_at", "INTEGER"),
        ],
    )?;
    add_missing(
        conn,
        "ingress_resolutions",
        &[
            ("provider_receipt", "TEXT"),
            ("confirmed_duplicate_risk", "INTEGER"),
        ],
    )?;
    // Preview ledgers without an explicit active authorization cannot infer
    // one from time order. Preserve evidence and stop ambiguous old retries.
    conn.execute(
        "INSERT OR IGNORE INTO ingress_attempts(operation_id,ingress_id,ordinal,status,reason,finished_at,
            retry_authorization_id) SELECT lease_id,id,attempt,'uncertain','legacy_retry_run_unbound',
            strftime('%s','now'),NULL FROM ingress WHERE run_id IS NULL AND status='dispatching'
            AND lease_id IS NOT NULL AND EXISTS(SELECT 1 FROM ingress_retry_authorizations a
            WHERE a.ingress_id=ingress.id)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE ingress SET status=CASE WHEN status='dispatching' THEN 'uncertain' ELSE 'quarantined' END,
            reason='legacy_retry_run_unbound',lease_id=NULL,lease_until=NULL WHERE run_id IS NULL
            AND status IN ('ready','claimed','dispatching') AND EXISTS(SELECT 1
            FROM ingress_retry_authorizations a WHERE a.ingress_id=ingress.id)",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("UPDATE ingress SET run_id=id WHERE run_id IS NULL", [])
        .map_err(|e| e.to_string())?;
    conn.execute_batch(RUN_TRIGGERS)
        .map_err(|e| e.to_string())?;
    conn.execute_batch(V2_SCHEMA).map_err(|e| e.to_string())?;
    conn.execute_batch(V3_SCHEMA).map_err(|e| e.to_string())?;
    // Before F2 every `failed` row had crossed the dispatch boundary
    // (reply rejected, expired, authority changed before delivery): it is
    // "executed, not delivered", never a plain retry.
    conn.execute(
        "UPDATE ingress SET status='undelivered' WHERE status='failed'",
        [],
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
        .map_err(|e| e.to_string())
}

/// Consume the restore marker (if any): hold every `ready` / `claimed`
/// event, and every event that could otherwise take a plain `retry`
/// (`failed_before_dispatch`; `quarantined` because something could not be
/// read; review L6: the old device may already have retried it), as
/// `quarantined` / `restored_from_backup` in one transaction, then
/// delete the marker. Runs inside `open`, so no worker of this store can
/// claim before it. A marker that cannot be removed after the commit is
/// harmless: a second pass only holds rows that are waiting again, and
/// fails open would be the wrong direction, so the error is returned.
const HELD_AFTER_RESTORE: &str = "status IN ('ready','claimed','failed_before_dispatch')
    OR (status='quarantined' AND reason IN ('revalidation_unavailable','snapshot_unavailable'))";

fn hold_restored(conn: &Connection, home: &Path) -> Result<Vec<String>, String> {
    let marker = home.join(super::RESTORE_MARKER);
    match std::fs::symlink_metadata(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("restore marker: {e}")),
        Ok(_) => {}
    }
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let ids = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT id FROM ingress WHERE {HELD_AFTER_RESTORE} ORDER BY seq"
                ))
                .map_err(|e| e.to_string())?;
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        conn.execute(
            &format!(
                "UPDATE ingress SET status='quarantined',reason=?1,lease_id=NULL,lease_until=NULL,
                    retry_at=NULL WHERE {HELD_AFTER_RESTORE}"
            ),
            [super::RESTORED_REASON],
        )
        .map_err(|e| e.to_string())?;
        Ok::<_, String>(ids)
    })();
    match result {
        Ok(ids) => {
            conn.execute_batch("COMMIT;").map_err(|e| e.to_string())?;
            std::fs::remove_file(&marker).map_err(|e| format!("restore marker: {e}"))?;
            Ok(ids)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(e)
        }
    }
}

fn user_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// Resolve the database path, refusing a symlink at the file or sidecars.
fn db_path(home: &Path) -> Result<PathBuf, String> {
    // SQLite NOFOLLOW rejects symlinked parent components too (macOS /var).
    let path = std::fs::canonicalize(home)
        .map_err(|e| format!("ingress home: {e}"))?
        .join("channel_ingress.db");
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("ingress database symlink refused".into());
        }
    }
    Ok(path)
}

/// Create the database file privately when it does not exist yet. An
/// existing file is never opened and closed here: closing any descriptor on
/// it would drop every POSIX lock this process holds on it, letting another
/// process checkpoint and remove the WAL under live connections.
fn create_private(path: &Path) -> Result<(), String> {
    if path.exists() {
        return private_permissions(path);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match opts.open(path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("ingress file: {e}")),
    }
    private_permissions(path)
}

fn private_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn connect(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "PRAGMA busy_timeout=5000;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA secure_delete=ON;",
    )
    .map_err(|e| e.to_string())?;
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if p.exists() {
            private_permissions(&p)?;
        }
    }
    Ok(conn)
}

impl IngressStore {
    /// Open or create, migrating an older schema. The gateway's path.
    pub(crate) fn open(home: &Path) -> Result<Self, String> {
        let path = db_path(home)?;
        create_private(&path)?;
        let conn = connect(&path)?;
        if user_version(&conn)? < SCHEMA_VERSION {
            migrate(&conn)?;
        }
        let held = hold_restored(&conn, home)?;
        let store = Self::from_connection(conn);
        if !held.is_empty() {
            if let Ok(mut r) = store.restored.lock() {
                *r = held;
            }
        }
        Ok(store)
    }

    /// Open an existing, already-migrated database without writing schema.
    /// Used by the operator CLI: a database the gateway has not upgraded yet
    /// is refused rather than migrated from a second process.
    pub(crate) fn open_current(home: &Path) -> Result<Self, String> {
        let path = db_path(home)?;
        if !path.exists() {
            return Err("收件匣資料庫不存在（gateway 尚未收過 LINE 事件）".into());
        }
        let conn = connect(&path)?;
        if user_version(&conn)? != SCHEMA_VERSION {
            return Err("收件匣資料庫版本與這個程式不同；請先用同一版本啟動 gateway 一次".into());
        }
        Ok(Self::from_connection(conn))
    }

    /// The one store per home inside this process (the LINE worker and the
    /// admin RPCs share its connection and lock).
    pub(crate) fn shared(home: &Path) -> Result<Arc<Self>, String> {
        static STORES: OnceLock<Mutex<Vec<(PathBuf, Weak<IngressStore>)>>> = OnceLock::new();
        let key = std::fs::canonicalize(home).map_err(|e| format!("ingress home: {e}"))?;
        let mut stores = STORES
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "ingress registry poisoned".to_string())?;
        stores.retain(|(_, w)| w.strong_count() > 0);
        if let Some(live) = stores
            .iter()
            .find(|(p, _)| *p == key)
            .and_then(|(_, w)| w.upgrade())
        {
            return Ok(live);
        }
        let store = Arc::new(Self::open(&key)?);
        stores.push((key, Arc::downgrade(&store)));
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_database_open_writes_no_schema_and_old_failed_becomes_undelivered() {
        let dir = tempfile::tempdir().unwrap();
        assert!(IngressStore::open_current(dir.path()).is_err());
        {
            let path = dir.path().join("channel_ingress.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(BASE_SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO ingress(id,channel,account,event_id,revision,authorization_revision,conversation,
                    status,received_at) VALUES ('x','line','a','e','r','a','c','failed',1)",
                [],
            )
            .unwrap();
        }
        // Not yet migrated: the CLI path refuses instead of migrating.
        assert!(IngressStore::open_current(dir.path()).is_err());
        drop(IngressStore::open(dir.path()).unwrap());
        let conn = Connection::open(dir.path().join("channel_ingress.db")).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        let status: String = conn
            .query_row("SELECT status FROM ingress WHERE id='x'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "undelivered");
        let changes_before: i64 = conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        drop(IngressStore::open_current(dir.path()).unwrap());
        drop(IngressStore::open(dir.path()).unwrap());
        let changes_after: i64 = conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(changes_before, changes_after);
    }

    async fn seed(home: &Path, ids: &[&str]) {
        let s = IngressStore::open(home).unwrap();
        for id in ids {
            s.append(
                &[super::super::AcceptedEvent {
                    decision_fastlane: false,
                    decision_binding: None,
                    event_id: (*id).into(),
                    account: "a".into(),
                    revision: "r1".into(),
                    authorization_revision: "a1".into(),
                    conversation: format!("c-{id}"),
                    payload: "{}".into(),
                }],
                10,
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn restore_marker_holds_waiting_events_once() {
        let dir = tempfile::tempdir().unwrap();
        seed(
            dir.path(),
            &["waiting", "claimed", "done", "not-run", "unreadable"],
        )
        .await;
        {
            let s = IngressStore::open(dir.path()).unwrap();
            let done = s.claim(10).await.unwrap().unwrap();
            s.transition(&done, "claimed", "dispatching", None)
                .await
                .unwrap();
            s.transition(&done, "dispatching", "completed", None)
                .await
                .unwrap();
            let _held_claim = s.claim(10).await.unwrap().unwrap();
            // Review L6: events a plain `retry` could start again are held too.
            let not_run = s.claim(10).await.unwrap().unwrap();
            s.transition(
                &not_run,
                "claimed",
                "failed_before_dispatch",
                Some("late_reply_expired"),
            )
            .await
            .unwrap();
            let unreadable = s.claim(10).await.unwrap().unwrap();
            s.connection()
                .lock()
                .await
                .execute(
                    "UPDATE ingress SET status='quarantined',reason='revalidation_unavailable',
                        lease_id=NULL,lease_until=NULL WHERE id=?1",
                    [&unreadable.id],
                )
                .unwrap();
        }
        std::fs::write(dir.path().join(super::super::RESTORE_MARKER), "t").unwrap();
        let s = IngressStore::open(dir.path()).unwrap();
        assert!(!dir.path().join(super::super::RESTORE_MARKER).exists());
        assert_eq!(s.take_restored().len(), 4);
        assert!(s.take_restored().is_empty(), "announced once");
        let rows = s.list().await.unwrap();
        for row in &rows {
            if row.status == "completed" {
                continue;
            }
            assert_eq!(row.status, "quarantined", "{}", row.event_id);
            assert_eq!(row.reason.as_deref(), Some(super::super::RESTORED_REASON));
        }
        assert!(s.claim(11).await.unwrap().is_none());
        // Only once: a later open without a marker changes nothing.
        seed(dir.path(), &["after"]).await;
        let again = IngressStore::open(dir.path()).unwrap();
        assert!(again.take_restored().is_empty());
        assert_eq!(again.claim(12).await.unwrap().unwrap().event_id, "after");
    }

    #[tokio::test]
    async fn no_marker_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        seed(dir.path(), &["a"]).await;
        let s = IngressStore::open(dir.path()).unwrap();
        assert!(s.take_restored().is_empty());
        assert_eq!(s.list().await.unwrap()[0].status, "ready");
    }

    #[test]
    fn shared_store_is_one_instance_per_home() {
        let dir = tempfile::tempdir().unwrap();
        let a = IngressStore::shared(dir.path()).unwrap();
        let b = IngressStore::shared(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_database_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.db");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("channel_ingress.db")).unwrap();
        assert!(IngressStore::open(dir.path()).is_err());
    }
}
