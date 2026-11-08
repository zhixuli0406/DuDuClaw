//! Owner credential change log (native Linux isolation v1.81, F6 design
//! §3.4 "Personal adds", owner decision Q3, task F6c).
//!
//! `credential_changes(seq, user_id, field, actor_user_id, owner_authenticated,
//! at)` records every change of an account's password, email address and
//! second-factor set (TOTP, passkeys, email factor). A Personal restore
//! compares the tombstone-time principal snapshot with the live account and
//! refuses when anything changed, **unless** every change since the
//! tombstone has a row here with `actor_user_id == owner` and
//! `owner_authenticated = 1` (the owner changed it in their own
//! authenticated session). There is no override.
//!
//! Like `admin_eligibility_log`, rows are appended by SQLite triggers, so every
//! writer (present, future, or one that bypasses `UserDb`) logs in the
//! statement that makes the change. A row starts unattributed
//! (`actor_user_id NULL`, `owner_authenticated 0`), which the restore treats
//! as "changed by someone else" (fail closed). A writer that knows who acted
//! stamps its rows once, inside its own transaction, after reading
//! [`high_water`]: [`stamp_owner`] for the owner's own authenticated
//! self-service change, [`stamp_actor`] for anyone else (an Admin reset).
//!
//! The Google link lives outside users.db (`person_credentials`), so it is
//! not logged here; a changed Google `sub` always refuses a restore.
//!
//! Wiring the stamps into the self-service password, email and MFA writers is
//! F6d's (the RPC layer knows the authenticated session); until then every
//! change reads unattributed and a changed snapshot refuses the restore.
use rusqlite::{params, Connection, TransactionBehavior};

const NOW: &str = "CAST(strftime('%s','now') AS INTEGER)";

fn append_sql(id: &str, field: &str) -> String {
    format!("INSERT INTO credential_changes(user_id,field,actor_user_id,owner_authenticated,at) \
        VALUES({id},'{field}',NULL,0,{NOW});")
}

fn schema_sql() -> String {
    let mut sql = String::from(
        "CREATE TABLE IF NOT EXISTS credential_changes (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id TEXT NOT NULL,
            field TEXT NOT NULL CHECK (field IN ('password', 'email', 'totp', 'passkey', 'email_factor')),
            actor_user_id TEXT,
            owner_authenticated INTEGER NOT NULL DEFAULT 0 CHECK (owner_authenticated IN (0, 1)),
            at INTEGER NOT NULL,
            CHECK (owner_authenticated = 0 OR actor_user_id = user_id));
         CREATE INDEX IF NOT EXISTS idx_credential_changes_user ON credential_changes(user_id, at);
         CREATE TRIGGER IF NOT EXISTS credential_changes_no_delete BEFORE DELETE ON credential_changes
         BEGIN SELECT RAISE(ABORT, 'credential change log is append-only'); END;
         CREATE TRIGGER IF NOT EXISTS credential_changes_stamp_once BEFORE UPDATE ON credential_changes
         WHEN OLD.actor_user_id IS NOT NULL OR NEW.actor_user_id IS NULL OR NEW.seq IS NOT OLD.seq
            OR NEW.user_id IS NOT OLD.user_id OR NEW.field IS NOT OLD.field OR NEW.at IS NOT OLD.at
         BEGIN SELECT RAISE(ABORT, 'credential change log is append-only'); END;\n",
    );
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS credential_changes_password AFTER UPDATE OF password_hash ON users
         WHEN OLD.password_hash IS NOT NEW.password_hash BEGIN {} END;
         CREATE TRIGGER IF NOT EXISTS credential_changes_email AFTER UPDATE OF email ON users
         WHEN OLD.email IS NOT NEW.email BEGIN {} END;\n",
        append_sql("NEW.id", "password"), append_sql("NEW.id", "email"),
    ));
    for (table, field) in [("mfa_credentials", "totp"), ("mfa_passkeys", "passkey"), ("mfa_email_credentials", "email_factor")] {
        sql.push_str(&format!(
            "CREATE TRIGGER IF NOT EXISTS credential_changes_{table}_insert AFTER INSERT ON {table} BEGIN {} END;
             CREATE TRIGGER IF NOT EXISTS credential_changes_{table}_delete AFTER DELETE ON {table} BEGIN {} END;\n",
            append_sql("NEW.user_id", field), append_sql("OLD.user_id", field),
        ));
    }
    sql.push_str(&format!(
        "CREATE TRIGGER IF NOT EXISTS credential_changes_mfa_email_credentials_update
         AFTER UPDATE OF email ON mfa_email_credentials WHEN OLD.email IS NOT NEW.email BEGIN {} END;\n",
        append_sql("NEW.user_id", "email_factor"),
    ));
    sql
}

/// Create the log and its triggers. Idempotent; runs in `UserDb::init_tables`
/// after the MFA tables exist.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    tx.execute_batch(&schema_sql()).map_err(|e| format!("credential change log migration failed: {e}"))?;
    tx.commit().map_err(|e| e.to_string())
}

/// Highest `seq` so far (0 for an empty log). Read inside the writer's
/// transaction before the change, then stamp after it.
pub fn high_water(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT COALESCE(MAX(seq),0) FROM credential_changes", [], |r| r.get(0)).map_err(|e| e.to_string())
}

/// The owner changed their own credential in an owner-authenticated session:
/// attribute the unattributed rows after `since` **of that user only**.
pub fn stamp_owner(conn: &Connection, since: i64, owner: &str) -> Result<(), String> {
    conn.execute("UPDATE credential_changes SET actor_user_id=?2,owner_authenticated=1
        WHERE seq>?1 AND actor_user_id IS NULL AND user_id=?2", params![since, owner])
        .map(|_| ()).map_err(|e| e.to_string())
}

/// Someone other than the account holder (an Admin reset, a migration) made
/// the change: attribute the unattributed rows after `since` to `actor`, never
/// as owner-authenticated.
pub fn stamp_actor(conn: &Connection, since: i64, actor: &str) -> Result<(), String> {
    conn.execute("UPDATE credential_changes SET actor_user_id=?2,owner_authenticated=0
        WHERE seq>?1 AND actor_user_id IS NULL", params![since, actor])
        .map(|_| ()).map_err(|e| e.to_string())
}

/// One log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialChange {
    pub seq: i64,
    pub user_id: String,
    pub field: String,
    pub actor_user_id: Option<String>,
    pub owner_authenticated: bool,
    pub at: i64,
}

impl CredentialChange {
    /// The account holder made this change in their own authenticated session.
    pub fn by_owner(&self) -> bool {
        self.owner_authenticated && self.actor_user_id.as_deref() == Some(self.user_id.as_str())
    }
}

/// Changes of `user` with `at >= since`, oldest first, at most `limit` (an
/// over-long answer is an error, never a truncated history). A missing table
/// is an error, never "no changes".
pub fn changes_since(conn: &Connection, user: &str, since: i64, limit: usize) -> Result<Vec<CredentialChange>, String> {
    let bound = i64::try_from(limit.saturating_add(1)).map_err(|e| e.to_string())?;
    let mut statement = conn.prepare(
        "SELECT seq,user_id,field,actor_user_id,owner_authenticated,at FROM credential_changes
         WHERE user_id=?1 AND at>=?2 ORDER BY seq LIMIT ?3").map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![user, since, bound], |r| Ok(CredentialChange {
        seq: r.get(0)?, user_id: r.get(1)?, field: r.get(2)?, actor_user_id: r.get(3)?,
        owner_authenticated: r.get::<_, i64>(4)? == 1, at: r.get(5)?,
    })).map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    if rows.len() > limit { return Err("credential change log longer than the read limit".into()); }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use crate::{UserDb, UserRole};

    fn db() -> (tempfile::TempDir, UserDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("users.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn credential_changes_logged_by_trigger_and_stamped_once() {
        let (_dir, db) = db();
        let alice = db.create_user("alice@example.test", "Alice", "password1", UserRole::Employee).unwrap().id;
        let conn = db.conn();
        let since = super::high_water(&conn).unwrap();
        conn.execute("UPDATE users SET password_hash='x' WHERE id=?1", [&alice]).unwrap();
        conn.execute("UPDATE users SET display_name='A' WHERE id=?1", [&alice]).unwrap();
        let rows = super::changes_since(&conn, &alice, 0, 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.field.as_str()).collect::<Vec<_>>(), ["password"]);
        assert!(!rows[0].by_owner());
        super::stamp_owner(&conn, since, &alice).unwrap();
        let rows = super::changes_since(&conn, &alice, 0, 10).unwrap();
        assert!(rows[0].by_owner());
        // Append-only: no restamp, no delete.
        assert!(conn.execute("UPDATE credential_changes SET actor_user_id='other'", []).is_err());
        assert!(conn.execute("DELETE FROM credential_changes", []).is_err());
        // An owner stamp never claims another account's rows.
        let next = super::high_water(&conn).unwrap();
        conn.execute("UPDATE users SET email='new@example.test' WHERE id=?1", [&alice]).unwrap();
        super::stamp_owner(&conn, next, "someone-else").unwrap();
        assert!(!super::changes_since(&conn, &alice, 0, 10).unwrap()[1].by_owner());
    }
}
