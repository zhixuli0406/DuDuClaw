//! Only migrations that change Google authority enter the writer barrier.
//! Schema-only initialization may run on every open; credential backfills must
//! use this path, with a read-only predicate that also recognizes legacy data.
use rusqlite::{Connection, TransactionBehavior};
use crate::UserDb;

pub(crate) fn schema_object_exists(conn: &Connection, kind: &str, name: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type=?1 AND name=?2)",
        [kind, name], |r| r.get(0)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::authority_coordinator::AuthorityCoordinator;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn current_schema_reopens_preserve_authority_epoch() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        drop(UserDb::new(&path).unwrap());
        let coordinator = AuthorityCoordinator::new(home.path());
        let epoch = coordinator.epoch().unwrap();
        // Also prove request-time opens need no exclusive authority lease.
        let _reader = coordinator.read().unwrap();
        for _ in 0..2 {
            drop(UserDb::new(&path).unwrap());
            assert_eq!(epoch, coordinator.epoch().unwrap());
        }
    }

    #[test]
    fn legacy_credential_backfill_rotates_epoch_only_once() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        let user = db.create_user("legacy@c.test", "Legacy", "fixture-password", crate::UserRole::Employee).unwrap();
        db.conn().execute("INSERT INTO mfa_credentials(user_id,secret_enc,last_step) VALUES(?1,'fixture',0)",
            [&user.id]).unwrap();
        assert_eq!(db.get_user(&user.id).unwrap().unwrap().credential_epoch, 0);
        let coordinator = AuthorityCoordinator::new(home.path());
        let before = coordinator.epoch().unwrap();
        let reopened = UserDb::new(&path).unwrap();
        assert_eq!(reopened.get_user(&user.id).unwrap().unwrap().credential_epoch, 1);
        let after = coordinator.epoch().unwrap();
        assert_ne!(before, after);
        drop(UserDb::new(&path).unwrap());
        assert_eq!(after, coordinator.epoch().unwrap());
    }

    #[test]
    fn legacy_session_migration_revokes_and_rotates_epoch() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        db.conn().execute_batch("ALTER TABLE remembered_families DROP COLUMN generation;
            INSERT INTO remembered_families(id,user_id,epoch,login_session,label,created_at,last_used_at,expires_at)
                VALUES('family','legacy',0,'session','device',0,0,9999999999);
            INSERT INTO remembered_tokens(hash,family) VALUES('hash','family');").unwrap();
        let coordinator = AuthorityCoordinator::new(home.path());
        let before = coordinator.epoch().unwrap();
        let reopened = UserDb::new(&path).unwrap();
        let conn = reopened.conn();
        assert!(crate::user_row::has_column(&conn, "remembered_families", "generation").unwrap());
        assert_eq!(conn.query_row("SELECT revoked FROM remembered_families", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM remembered_tokens", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let after = coordinator.epoch().unwrap();
        assert_ne!(before, after);
        drop(conn);
        drop(UserDb::new(&path).unwrap());
        assert_eq!(after, coordinator.epoch().unwrap());
    }

    #[test]
    fn real_schema_migration_waits_for_readers_and_holds_lease_through_commit() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        db.conn().execute_batch("DROP TABLE google_identity_consents;").unwrap();
        let coordinator = AuthorityCoordinator::new(home.path());
        let before = coordinator.epoch().unwrap();
        let reader = coordinator.read().unwrap();
        let (preflight_tx, preflight_rx) = mpsc::channel();
        let (mutated_tx, mutated_rx) = mpsc::channel();
        let (commit_tx, commit_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let conn = db.conn();
            db.migrate_google_authority_if_needed(&conn, |conn| {
                let needed = crate::google_identity::migration_needed(conn)?;
                preflight_tx.send(()).unwrap();
                Ok(needed)
            }, |conn| {
                assert!(!conn.is_autocommit());
                crate::google_identity::migrate(conn)?;
                mutated_tx.send(()).unwrap();
                commit_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            }).unwrap();
        });
        preflight_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(mutated_rx.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(before, coordinator.epoch().unwrap());
        drop(reader);
        mutated_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let observer = Connection::open(&path).unwrap();
        assert!(!schema_object_exists(&observer, "table", "google_identity_consents").unwrap());
        let (read_tx, read_rx) = mpsc::channel();
        let next = coordinator.clone();
        let next_reader = std::thread::spawn(move || {
            let _lease = next.read().unwrap();
            read_tx.send(()).unwrap();
        });
        assert!(matches!(read_rx.recv_timeout(Duration::from_millis(100)), Err(mpsc::RecvTimeoutError::Timeout)));
        commit_tx.send(()).unwrap();
        writer.join().unwrap();
        read_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        next_reader.join().unwrap();
        assert!(schema_object_exists(&observer, "table", "google_identity_consents").unwrap());
        assert_ne!(before, coordinator.epoch().unwrap());
        drop(UserDb::new(&path).unwrap());
        let after = coordinator.epoch().unwrap();
        drop(UserDb::new(&path).unwrap());
        assert_eq!(after, coordinator.epoch().unwrap());
    }
}

impl UserDb {
    /// Never call inside a SQLite transaction: authority precedes SQLite in
    /// the lock order. The predicate must be read-only and match every change
    /// made by the migration. Update both together when adding a migration.
    pub(crate) fn migrate_google_authority_if_needed(
        &self,
        conn: &Connection,
        needed: impl Fn(&Connection) -> Result<bool, String>,
        migrate: impl FnOnce(&Connection) -> Result<(), String>,
    ) -> Result<(), String> {
        if !conn.is_autocommit() { return Err("authority migration requires autocommit".into()); }
        if !needed(conn)? { return Ok(()); }
        let _authority = self.google_authority_write()?;
        // Another opener may have completed the migration while we waited.
        if !needed(conn)? { return Ok(()); }
        let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        // Recheck after SQLite's reservation as well; no partial backfill is
        // visible and an error rolls back before releasing the authority lease.
        if needed(&tx)? { migrate(&tx)?; }
        tx.commit().map_err(|e| e.to_string())
    }
}
