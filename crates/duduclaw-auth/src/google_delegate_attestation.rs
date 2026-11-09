//! Admin attestation of a Google delegate subject (native Linux isolation
//! v1.81, SR4 item 1; rule D-5 (b) of the work breakdown).
//!
//! In protected mode a domain-wide-delegation employee (`agent.toml [google]
//! identity = "delegate:<S>"`) may act as the person S only for that person,
//! and only when an Admin attested — in their own MFA step-up session, audited
//! — WHICH users.db account S belongs to. Matching S against the login emails
//! alone is not enough: any staff member can set their own login email to any
//! unused address, so an email match proves nothing about who the mailbox
//! belongs to.
//!
//! `google_delegate_attestations(agent_id, subject, user_id,
//! email_at_attestation, credential_seq, attested_by, attested_at)` holds one
//! row per employee. `credential_seq` is the `credential_changes` high-water
//! mark read in the attesting transaction: ANY later `email` row of the
//! attested account (owner self-change, Admin change, raw writer — the log is
//! trigger-written) revokes the attestation at use time
//! ([`AttestationState::email_changed_since`]). There is no override; an
//! Admin attests again.
//!
//! Every attest / revoke writes an `auth_audit_log` row in its own
//! transaction ([`ATTESTED_ACTION`], [`REVOKED_ACTION`]).
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::db::UserDb;

/// `auth_audit_log` action of an attestation (detail: agent, subject, user id).
pub const ATTESTED_ACTION: &str = "google.delegate_subject_attested";
/// `auth_audit_log` action of a revoked attestation.
pub const REVOKED_ACTION: &str = "google.delegate_subject_attestation_revoked";

/// Refusal texts (end-user zh-TW; never an email of another account).
pub const NOT_ONE_PERSON: &str = "這個信箱目前不是恰好一位有效同仁的登入帳號，無法證明屬於誰。";

/// One stored attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateAttestation {
    pub agent_id: String,
    /// The attested subject, as normalized by the gateway.
    pub subject: String,
    /// The users.db account the Admin attested S belongs to.
    pub user_id: String,
    /// That account's login email when attested (as stored).
    pub email_at_attestation: String,
    /// `credential_changes` high-water mark of the attesting transaction.
    pub credential_seq: i64,
    pub attested_by: String,
    pub attested_at: String,
}

/// An attestation plus whether it has since been revoked by an email change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationState {
    pub attestation: DelegateAttestation,
    /// Any `email` change of the attested account after the attestation.
    pub email_changed_since: bool,
}
impl AttestationState {
    /// Still binding for `subject` and `user_id` (exact equality).
    pub fn binds(&self, subject: &str, user_id: &str) -> bool {
        !self.email_changed_since && self.attestation.subject == subject && self.attestation.user_id == user_id
            && !user_id.is_empty()
    }
}

/// Create the table. Idempotent; runs in `UserDb::init_tables` after the
/// credential change log exists.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS google_delegate_attestations (
            agent_id TEXT PRIMARY KEY,
            subject TEXT NOT NULL,
            user_id TEXT NOT NULL,
            email_at_attestation TEXT NOT NULL,
            credential_seq INTEGER NOT NULL,
            attested_by TEXT NOT NULL,
            attested_at TEXT NOT NULL);",
    )
    .map_err(|e| format!("google delegate attestation migration failed: {e}"))
}

fn audit(conn: &Connection, actor: &str, action: &str, agent: &str, detail: &serde_json::Value, now: &str)
    -> Result<(), String> {
    conn.execute(
        "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
        params![actor, action, agent, detail.to_string(), now],
    )
    .map(|_| ())
    .map_err(|e| format!("attestation audit: {e}"))
}

impl UserDb {
    /// Record that `subject` (already normalized by the caller) belongs to the
    /// ONE users.db account whose login email `email_matches` (the caller's
    /// normalizer, compared by exact equality). Inside one immediate
    /// transaction: exactly one account matches and it is Active, else
    /// [`NOT_ONE_PERSON`]; the credential-change high-water mark is read; the
    /// row is replaced; the audit row is written. `attested_by` is the
    /// verified Admin (the caller has checked role, MFA and step-up).
    pub fn attest_google_delegate(&self, agent_id: &str, subject: &str, email_matches: &dyn Fn(&str) -> bool,
        attested_by: &str) -> Result<DelegateAttestation, String> {
        if agent_id.is_empty() || subject.is_empty() || attested_by.is_empty() {
            return Err("attestation input is empty".into());
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("attestation: {e}"))?;
        let holders: Vec<(String, String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, email, status FROM users")
                .map_err(|e| format!("attestation: {e}"))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|e| format!("attestation: {e}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("attestation: {e}"))?
                .into_iter()
                .filter(|row: &(String, String, String)| email_matches(&row.1))
                .collect()
        };
        let [(user_id, email, status)] = holders.as_slice() else { return Err(NOT_ONE_PERSON.into()) };
        if status != "active" { return Err(NOT_ONE_PERSON.into()); }
        let seq = crate::credential_changes::high_water(&tx)?;
        tx.execute(
            "INSERT INTO google_delegate_attestations
                (agent_id, subject, user_id, email_at_attestation, credential_seq, attested_by, attested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(agent_id) DO UPDATE SET subject=excluded.subject, user_id=excluded.user_id,
                email_at_attestation=excluded.email_at_attestation, credential_seq=excluded.credential_seq,
                attested_by=excluded.attested_by, attested_at=excluded.attested_at",
            params![agent_id, subject, user_id, email, seq, attested_by, now],
        )
        .map_err(|e| format!("attestation: {e}"))?;
        audit(&tx, attested_by, ATTESTED_ACTION, agent_id,
            &serde_json::json!({"agent_id": agent_id, "subject": subject, "user_id": user_id}), &now)?;
        let attestation = DelegateAttestation {
            agent_id: agent_id.to_owned(), subject: subject.to_owned(), user_id: user_id.clone(),
            email_at_attestation: email.clone(), credential_seq: seq, attested_by: attested_by.to_owned(),
            attested_at: now,
        };
        tx.commit().map_err(|e| format!("attestation: {e}"))?;
        Ok(attestation)
    }

    /// Remove `agent_id`'s attestation (audited). `Ok(false)`: there was none.
    pub fn revoke_google_delegate_attestation(&self, agent_id: &str, actor: &str) -> Result<bool, String> {
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("attestation: {e}"))?;
        let removed: Option<(String, String)> = tx
            .query_row("SELECT subject, user_id FROM google_delegate_attestations WHERE agent_id = ?1",
                params![agent_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(|e| format!("attestation: {e}"))?;
        let Some((subject, user_id)) = removed else { return Ok(false) };
        tx.execute("DELETE FROM google_delegate_attestations WHERE agent_id = ?1", params![agent_id])
            .map_err(|e| format!("attestation: {e}"))?;
        audit(&tx, actor, REVOKED_ACTION, agent_id,
            &serde_json::json!({"agent_id": agent_id, "subject": subject, "user_id": user_id}), &now)?;
        tx.commit().map_err(|e| format!("attestation: {e}"))?;
        Ok(true)
    }

    /// `agent_id`'s attestation and whether the attested account's email
    /// changed since, read on one connection. A missing table or log is an
    /// error (callers refuse), never "no attestation".
    pub fn google_delegate_attestation(&self, agent_id: &str) -> Result<Option<AttestationState>, String> {
        let conn = self.conn();
        let attestation = conn
            .query_row(
                "SELECT agent_id, subject, user_id, email_at_attestation, credential_seq, attested_by, attested_at
                 FROM google_delegate_attestations WHERE agent_id = ?1",
                params![agent_id],
                |row| Ok(DelegateAttestation {
                    agent_id: row.get(0)?, subject: row.get(1)?, user_id: row.get(2)?,
                    email_at_attestation: row.get(3)?, credential_seq: row.get(4)?, attested_by: row.get(5)?,
                    attested_at: row.get(6)?,
                }),
            )
            .optional()
            .map_err(|e| format!("attestation: {e}"))?;
        let Some(attestation) = attestation else { return Ok(None) };
        let email_changed_since: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM credential_changes WHERE user_id = ?1 AND field = 'email' AND seq > ?2)",
                params![attestation.user_id, attestation.credential_seq],
                |row| row.get(0),
            )
            .map_err(|e| format!("attestation: {e}"))?;
        Ok(Some(AttestationState { attestation, email_changed_since }))
    }
}

#[cfg(test)]
mod tests {
    use crate::db::UserDb;
    use crate::models::{UserRole, UserStatus};

    fn db() -> (UserDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (UserDb::new(&dir.path().join("users.db")).unwrap(), dir)
    }
    fn lower(subject: &'static str) -> impl Fn(&str) -> bool {
        move |email: &str| email.trim().to_ascii_lowercase() == subject
    }

    #[test]
    fn sr4_attestation_binds_one_active_account_and_is_audited() {
        let (db, _dir) = db();
        let alice = db.create_user("Alice@Customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let state = db.google_delegate_attestation("bot").unwrap();
        assert!(state.is_none(), "no attestation ⇒ none (callers refuse)");
        let attested = db.attest_google_delegate("bot", "alice@customer.com", &lower("alice@customer.com"), "admin-id")
            .unwrap();
        assert_eq!((attested.user_id.as_str(), attested.email_at_attestation.as_str()), (alice.id.as_str(), "Alice@Customer.com"));
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("alice@customer.com", &alice.id));
        assert!(!state.binds("alice@customer.com", "someone-else"));
        assert!(!state.binds("other@customer.com", &alice.id));
        let rows = db.query_audit_log(None, Some(super::ATTESTED_ACTION), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.as_deref(), Some("bot"));
        // Nobody / an inactive account / two accounts: never attestable.
        assert_eq!(db.attest_google_delegate("bot", "ghost@customer.com", &lower("ghost@customer.com"), "admin-id")
            .unwrap_err(), super::NOT_ONE_PERSON);
        let bob = db.create_user("bob@customer.com", "Bob", "password1", UserRole::Employee).unwrap();
        db.set_user_status(&bob.id, UserStatus::Suspended).unwrap();
        assert!(db.attest_google_delegate("bot2", "bob@customer.com", &lower("bob@customer.com"), "admin-id").is_err());
        assert!(db.attest_google_delegate("bot2", "x", &|_: &str| true, "admin-id").is_err(), "ambiguous");
        // Revoke is audited and idempotent.
        assert!(db.revoke_google_delegate_attestation("bot", "admin-id").unwrap());
        assert!(!db.revoke_google_delegate_attestation("bot", "admin-id").unwrap());
        assert!(db.google_delegate_attestation("bot").unwrap().is_none());
        assert_eq!(db.query_audit_log(None, Some(super::REVOKED_ACTION), 10).unwrap().len(), 1);
    }

    #[test]
    fn sr4_any_email_change_after_attestation_revokes_it() {
        let (db, _dir) = db();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        // An email change BEFORE the attestation does not count.
        db.change_own_email(&alice.id, "alice2@customer.com").unwrap();
        db.change_own_email(&alice.id, "alice@customer.com").unwrap();
        db.attest_google_delegate("bot", "alice@customer.com", &lower("alice@customer.com"), "admin-id").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id));
        // Owner self-change (even back to the same address) revokes.
        db.change_own_email(&alice.id, "alice.new@customer.com").unwrap();
        db.change_own_email(&alice.id, "alice@customer.com").unwrap();
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.email_changed_since && !state.binds("alice@customer.com", &alice.id));
        // Re-attesting starts a new window; an Admin or raw change revokes too.
        db.attest_google_delegate("bot", "alice@customer.com", &lower("alice@customer.com"), "admin-id").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id));
        db.conn().execute("UPDATE users SET email='raw@customer.com' WHERE id=?1", [&alice.id]).unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().email_changed_since);
        // Another account's change never revokes Alice's attestation.
        db.conn().execute("UPDATE users SET email='alice@customer.com' WHERE id=?1", [&alice.id]).unwrap();
        db.attest_google_delegate("bot", "alice@customer.com", &lower("alice@customer.com"), "admin-id").unwrap();
        let carol = db.create_user("carol@customer.com", "Carol", "password1", UserRole::Employee).unwrap();
        db.change_own_email(&carol.id, "carol2@customer.com").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id));
    }
}
