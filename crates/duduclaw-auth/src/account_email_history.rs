//! Every email address an account ever held (native Linux isolation v1.81,
//! SR5 item 4).
//!
//! The Google tenant source admits a designated service mailbox for any
//! actor (rule D-5). A service mailbox must therefore never be an address a
//! person ever logged in with: not today's address only (a colleague's
//! account whose email was changed, or an account since removed, would
//! otherwise leave its old mailbox designatable).
//!
//! `account_email_history(email, user_id, first_seen)` is append-only and is
//! written by SQLite triggers on `users` (INSERT, and UPDATE OF email — old and
//! new address), so every writer — present, future, or one that bypasses
//! `UserDb` — records the address in the statement that sets it, inside the
//! same transaction. [`migrate`] seeds it once with every current address.
//! Addresses are stored as written; callers compare with their own normalizer.
//!
//! Residual (documented): an address that was never a users.db email (a
//! colleague with no DuDuClaw account at all) cannot be known here; the
//! tenant settings RPC is Admin + MFA + fresh step-up and audited for that
//! reason.
use rusqlite::{Connection, TransactionBehavior};

const STAMP: &str = "strftime('%Y-%m-%dT%H:%M:%SZ','now')";

fn record_sql(email: &str, user: &str) -> String {
    // `WHERE NOT EXISTS` instead of `INSERT OR IGNORE`: the conflict clause of
    // the firing statement would override the trigger's own.
    format!(
        "INSERT INTO account_email_history(email,user_id,first_seen) SELECT {email},{user},{STAMP} \
         WHERE {email} IS NOT NULL AND NOT EXISTS(SELECT 1 FROM account_email_history WHERE email={email} AND user_id={user});"
    )
}

fn schema_sql() -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS account_email_history (
            email TEXT NOT NULL,
            user_id TEXT NOT NULL,
            first_seen TEXT NOT NULL,
            PRIMARY KEY (email, user_id));
         CREATE TRIGGER IF NOT EXISTS account_email_history_no_delete BEFORE DELETE ON account_email_history
         BEGIN SELECT RAISE(ABORT, 'account email history is append-only'); END;
         CREATE TRIGGER IF NOT EXISTS account_email_history_no_update BEFORE UPDATE ON account_email_history
         BEGIN SELECT RAISE(ABORT, 'account email history is append-only'); END;
         CREATE TRIGGER IF NOT EXISTS account_email_history_user_insert AFTER INSERT ON users
         BEGIN {insert} END;
         CREATE TRIGGER IF NOT EXISTS account_email_history_user_email AFTER UPDATE OF email ON users
         WHEN OLD.email IS NOT NEW.email BEGIN {old} {new} END;
         INSERT INTO account_email_history(email,user_id,first_seen)
            SELECT u.email,u.id,u.created_at FROM users u
            WHERE NOT EXISTS(SELECT 1 FROM account_email_history h WHERE h.email=u.email AND h.user_id=u.id);",
        insert = record_sql("NEW.email", "NEW.id"),
        old = record_sql("OLD.email", "OLD.id"),
        new = record_sql("NEW.email", "NEW.id"),
    )
}

/// Create the table and its triggers and seed it with every current
/// address. Idempotent; runs in `UserDb::init_tables` after `users` exists.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    tx.execute_batch(&schema_sql()).map_err(|e| format!("account email history migration failed: {e}"))?;
    tx.commit().map_err(|e| e.to_string())
}

/// Every address any account ever held (distinct, as written). A missing
/// table is an error, never "no addresses".
pub fn all_emails(conn: &Connection) -> Result<Vec<String>, String> {
    let mut statement = conn
        .prepare("SELECT DISTINCT email FROM account_email_history ORDER BY email")
        .map_err(|e| format!("account email history: {e}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("account email history: {e}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("account email history: {e}"))
}

#[cfg(test)]
mod tests {
    use crate::db::UserDb;
    use crate::models::UserRole;

    fn db() -> (UserDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (UserDb::new(&dir.path().join("users.db")).unwrap(), dir)
    }

    #[test]
    fn sr5_every_address_an_account_ever_held_is_kept() {
        let (db, _dir) = db();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        assert_eq!(db.account_email_history().unwrap(), vec!["alice@customer.com".to_string()]);
        // A changed address keeps the old one.
        db.change_email("admin-actor", &alice.id, "alice.new@customer.com").unwrap();
        let history = db.account_email_history().unwrap();
        assert!(history.contains(&"alice@customer.com".to_string()) && history.contains(&"alice.new@customer.com".to_string()));
        // A raw writer that bypasses UserDb is recorded by the trigger too.
        db.conn().execute("UPDATE users SET email='raw@customer.com' WHERE id=?1", [&alice.id]).unwrap();
        assert!(db.account_email_history().unwrap().contains(&"raw@customer.com".to_string()));
        // Removing the account never removes its addresses; the log is append-only.
        db.conn().execute("DELETE FROM users WHERE id=?1", [&alice.id]).unwrap();
        assert_eq!(db.account_email_history().unwrap().len(), 3);
        assert!(db.conn().execute("DELETE FROM account_email_history", []).is_err());
        assert!(db.conn().execute("UPDATE account_email_history SET email='x@y.z'", []).is_err());
    }

    #[test]
    fn sr5_email_change_is_audited_in_the_same_transaction() {
        let (db, _dir) = db();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        db.change_own_email(&alice.id, "alice2@customer.com").unwrap();
        db.change_email("admin-actor", &alice.id, "alice3@customer.com").unwrap();
        let rows = db.query_audit_log(None, Some(crate::db::EMAIL_CHANGED_ACTION), 10).unwrap();
        assert_eq!(rows.len(), 2);
        let details: Vec<serde_json::Value> =
            rows.iter().map(|row| serde_json::from_str(row.detail.as_deref().unwrap()).unwrap()).collect();
        assert!(details.iter().any(|d| d["old_email"] == "alice@customer.com" && d["new_email"] == "alice2@customer.com"
            && d["by_owner"] == true));
        assert!(details.iter().any(|d| d["old_email"] == "alice2@customer.com" && d["new_email"] == "alice3@customer.com"
            && d["by_owner"] == false));
        assert!(rows.iter().all(|row| row.target.as_deref() == Some(alice.id.as_str())));
    }

    #[test]
    fn sr5_migration_seeds_existing_accounts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("users.db");
        {
            let db = UserDb::new(&path).unwrap();
            db.create_user("old@customer.com", "Old", "password1", UserRole::Employee).unwrap();
        }
        // An upgraded store whose history table predates nothing: drop it
        // (triggers first) and let the next open seed it again.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("DROP TRIGGER account_email_history_no_delete; DROP TRIGGER account_email_history_no_update;
                DROP TRIGGER account_email_history_user_insert; DROP TRIGGER account_email_history_user_email;
                DROP TABLE account_email_history;").unwrap();
        }
        let db = UserDb::new(&path).unwrap();
        assert_eq!(db.account_email_history().unwrap(), vec!["old@customer.com".to_string()]);
    }
}
