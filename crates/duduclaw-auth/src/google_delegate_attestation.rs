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
//! email_at_attestation, credential_seq, attested_by, attested_at,
//! employee_id, employee_incarnation)` holds one row per employee label.
//! `credential_seq` is the `credential_changes` high-water mark read in the
//! attesting transaction: ANY later `email` row of the attested account
//! (owner self-change, Admin change, raw writer — the log is trigger-written)
//! revokes the attestation at use time
//! ([`AttestationState::email_changed_since`]). There is no override; an
//! Admin attests again.
//!
//! SR4 round 3:
//! - N1: the Admin attests the person they were SHOWN. The request names the
//!   holder's user id; the attesting transaction refuses
//!   ([`HOLDER_MISMATCH`], audited `attest_holder_mismatch`) unless the single
//!   Active holder of the address is exactly that account. When the holder's
//!   current address was set by the account itself
//!   ([`email_self_set`]: the latest `email` row of the account is
//!   owner-authenticated), the Admin must also acknowledge that explicitly
//!   (`acknowledge_self_set`, [`SELF_SET_UNACKNOWLEDGED`]); the
//!   acknowledgement is audited ([`SELF_SET_ACKNOWLEDGED_ACTION`]).
//! - N2: an attestation binds the employee's immutable registry id and
//!   incarnation as well as its label. A retired label recreated under the
//!   same id (a new `EmployeeId`), or a restore (a new incarnation), needs a
//!   fresh attestation. Rows written before this column existed have no
//!   employee id and never bind ([`AttestationState::binds`]). The Shared
//!   tombstone / purge hook removes the label's row
//!   ([`revoke_in_transaction`]).
//!
//! Every attest / revoke / refused attest writes an `auth_audit_log` row in
//! its own transaction ([`ATTESTED_ACTION`], [`REVOKED_ACTION`],
//! [`REFUSED_ACTION`]).
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::db::UserDb;

/// `auth_audit_log` action of an attestation (detail: agent, subject, user id).
pub const ATTESTED_ACTION: &str = "google.delegate_subject_attested";
/// `auth_audit_log` action of a revoked attestation.
pub const REVOKED_ACTION: &str = "google.delegate_subject_attestation_revoked";
/// `auth_audit_log` action of a refused attestation (detail `reason`).
pub const REFUSED_ACTION: &str = "google.delegate_subject_attest_refused";
/// `auth_audit_log` action recording that the Admin acknowledged the holder
/// set this address on their own account (SR4 round 3, N1 (3)).
pub const SELF_SET_ACKNOWLEDGED_ACTION: &str = "google.delegate_subject_self_set_acknowledged";
/// Refusal reason code of [`HOLDER_MISMATCH`] (audit detail `reason`).
pub const HOLDER_MISMATCH_CODE: &str = "attest_holder_mismatch";

/// Refusal texts (end-user zh-TW; never an email of another account).
pub const NOT_ONE_PERSON: &str = "這個信箱目前不是恰好一位有效同仁的登入帳號，無法證明屬於誰。";
/// `attest_holder_mismatch`: the address is no longer held by the person the
/// Admin was shown.
pub const HOLDER_MISMATCH: &str = "這個信箱目前的登入帳號不是畫面上列出的同仁（可能剛被變更），未確認。請重新整理後再確認。";
/// The holder set this address themself and the Admin did not acknowledge it.
pub const SELF_SET_UNACKNOWLEDGED: &str = "這個信箱是該同仁自己改成的登入信箱。請先另外確認這真的是他本人的信箱，並勾選對應的確認項目。";

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
    /// SR4 round 3 (N2): the employee's immutable registry id (hex) when
    /// attested. `None` for rows written before the column existed: those
    /// never bind.
    pub employee_id: Option<String>,
    /// SR4 round 3 (N2): the employee incarnation when attested.
    pub employee_incarnation: Option<u32>,
}

/// Who and which employee an Admin attests (SR4 round 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttestationRequest<'a> {
    pub agent_id: &'a str,
    /// Hex of the employee's immutable registry id (`EmployeeId`).
    pub employee_id: &'a str,
    pub employee_incarnation: u32,
    /// Already normalized by the caller.
    pub subject: &'a str,
    /// The holder the Admin was shown; must be the single Active holder.
    pub user_id: &'a str,
    /// Required when the holder set this address on their own account.
    pub acknowledge_self_set: bool,
}

/// A successful attestation plus what the Admin confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attested {
    pub attestation: DelegateAttestation,
    /// The holder's display name (for the confirmation copy).
    pub display_name: String,
    /// The holder set the current address on their own account.
    pub email_self_set: bool,
}

/// The current holder of a delegate subject, as shown to an Admin before an
/// attestation (SR4 round 3, N1 (2)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateHolder {
    pub user_id: String,
    pub display_name: String,
    /// Account creation time as stored (RFC 3339).
    pub created_at: String,
    /// The latest `email` change of the account was made by the account itself
    /// (owner-authenticated). An account created with the address and never
    /// changed is `false`.
    pub email_self_set: bool,
}

/// An attestation plus whether it has since been revoked by an email change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationState {
    pub attestation: DelegateAttestation,
    /// Any `email` change of the attested account after the attestation.
    pub email_changed_since: bool,
}
impl AttestationState {
    /// Still binding for `subject`, `user_id` and exactly this employee
    /// incarnation (exact equality). A row without an employee id (written
    /// before SR4 round 3) never binds.
    pub fn binds(&self, subject: &str, user_id: &str, employee_id: &str, incarnation: u32) -> bool {
        !self.email_changed_since && self.attestation.subject == subject && self.attestation.user_id == user_id
            && !user_id.is_empty() && !employee_id.is_empty()
            && self.attestation.employee_id.as_deref() == Some(employee_id)
            && self.attestation.employee_incarnation == Some(incarnation)
    }
}

/// Create the table, or add the SR4 round 3 employee columns to an existing
/// one (existing rows keep NULL: never binding). Idempotent; runs in
/// `UserDb::init_tables` after the credential change log exists.
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let failed = |e: rusqlite::Error| format!("google delegate attestation migration failed: {e}");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS google_delegate_attestations (
            agent_id TEXT PRIMARY KEY,
            subject TEXT NOT NULL,
            user_id TEXT NOT NULL,
            email_at_attestation TEXT NOT NULL,
            credential_seq INTEGER NOT NULL,
            attested_by TEXT NOT NULL,
            attested_at TEXT NOT NULL,
            employee_id TEXT,
            employee_incarnation INTEGER);",
    )
    .map_err(failed)?;
    let columns: Vec<String> = {
        let mut statement = conn.prepare("PRAGMA table_info(google_delegate_attestations)").map_err(failed)?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1)).map_err(failed)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(failed)?
    };
    for (column, kind) in [("employee_id", "TEXT"), ("employee_incarnation", "INTEGER")] {
        if !columns.iter().any(|existing| existing == column) {
            conn.execute_batch(&format!("ALTER TABLE google_delegate_attestations ADD COLUMN {column} {kind};"))
                .map_err(failed)?;
        }
    }
    Ok(())
}

fn audit(conn: &Connection, actor: Option<&str>, action: &str, agent: &str, detail: &serde_json::Value, now: &str)
    -> Result<(), String> {
    conn.execute(
        "INSERT INTO auth_audit_log (user_id, action, target, detail, ip, timestamp) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
        params![actor, action, agent, detail.to_string(), now],
    )
    .map(|_| ())
    .map_err(|e| format!("attestation audit: {e}"))
}

/// SR4 round 3 (N1): the latest `email` change of `user_id` was made by the
/// account itself in its own authenticated session. No change ⇒ `false`. A
/// missing log is an error (callers refuse).
pub fn email_self_set(conn: &Connection, user_id: &str) -> Result<bool, String> {
    let latest: Option<(Option<String>, i64)> = conn
        .query_row(
            "SELECT actor_user_id, owner_authenticated FROM credential_changes
             WHERE user_id = ?1 AND field = 'email' ORDER BY seq DESC LIMIT 1",
            params![user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("attestation: {e}"))?;
    Ok(latest.is_some_and(|(actor, owner)| owner == 1 && actor.as_deref() == Some(user_id)))
}

/// SR4 round 3 (N2): remove `agent`'s attestation inside a caller-owned
/// users.db transaction (the Shared tombstone / purge hook), audited with no
/// actor and `reason`. A database without the table has nothing to remove.
pub fn revoke_in_transaction(conn: &Connection, agent: &str, reason: &str) -> Result<bool, String> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='google_delegate_attestations')",
            [], |row| row.get(0))
        .map_err(|e| format!("attestation: {e}"))?;
    if !exists { return Ok(false); }
    let removed: Option<(String, String)> = conn
        .query_row("SELECT subject, user_id FROM google_delegate_attestations WHERE agent_id = ?1",
            params![agent], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .map_err(|e| format!("attestation: {e}"))?;
    let Some((subject, user_id)) = removed else { return Ok(false) };
    conn.execute("DELETE FROM google_delegate_attestations WHERE agent_id = ?1", params![agent])
        .map_err(|e| format!("attestation: {e}"))?;
    audit(conn, None, REVOKED_ACTION, agent,
        &serde_json::json!({"agent_id": agent, "subject": subject, "user_id": user_id, "reason": reason}),
        &Utc::now().to_rfc3339())?;
    Ok(true)
}

impl UserDb {
    /// Record that `request.subject` (already normalized by the caller)
    /// belongs to the ONE users.db account whose login email `email_matches`
    /// (the caller's normalizer, compared by exact equality), for exactly
    /// `request`'s employee incarnation. Inside one immediate transaction:
    /// exactly one account matches and it is Active, else [`NOT_ONE_PERSON`];
    /// it is the account the Admin was shown (`request.user_id`), else
    /// [`HOLDER_MISMATCH`] (the refusal is audited); a self-set address needs
    /// `acknowledge_self_set`, else [`SELF_SET_UNACKNOWLEDGED`]; the
    /// credential-change high-water mark is read; the row is replaced; the
    /// audit rows are written. `attested_by` is the verified Admin (the caller
    /// has checked role, MFA and step-up).
    pub fn attest_google_delegate(&self, request: &AttestationRequest<'_>, email_matches: &dyn Fn(&str) -> bool,
        attested_by: &str) -> Result<Attested, String> {
        let AttestationRequest { agent_id, employee_id, employee_incarnation, subject, user_id: shown,
            acknowledge_self_set } = *request;
        if agent_id.is_empty() || subject.is_empty() || attested_by.is_empty() || shown.is_empty()
            || employee_id.is_empty() {
            return Err("attestation input is empty".into());
        }
        let now = Utc::now().to_rfc3339();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| format!("attestation: {e}"))?;
        let holders: Vec<(String, String, String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, email, status, display_name FROM users")
                .map_err(|e| format!("attestation: {e}"))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .map_err(|e| format!("attestation: {e}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("attestation: {e}"))?
                .into_iter()
                .filter(|row: &(String, String, String, String)| email_matches(&row.1))
                .collect()
        };
        let [(user_id, email, status, display_name)] = holders.as_slice() else { return Err(NOT_ONE_PERSON.into()) };
        if status != "active" { return Err(NOT_ONE_PERSON.into()); }
        if user_id != shown {
            // N1 (1): the address is held by someone other than the person the
            // Admin was shown. Refused and audited; nothing else changes.
            audit(&tx, Some(attested_by), REFUSED_ACTION, agent_id, &serde_json::json!({
                "agent_id": agent_id, "subject": subject, "reason": HOLDER_MISMATCH_CODE,
                "requested_user_id": shown, "holder_user_id": user_id,
            }), &now)?;
            tx.commit().map_err(|e| format!("attestation: {e}"))?;
            return Err(HOLDER_MISMATCH.into());
        }
        let self_set = email_self_set(&tx, user_id)?;
        if self_set && !acknowledge_self_set { return Err(SELF_SET_UNACKNOWLEDGED.into()); }
        let seq = crate::credential_changes::high_water(&tx)?;
        tx.execute(
            "INSERT INTO google_delegate_attestations
                (agent_id, subject, user_id, email_at_attestation, credential_seq, attested_by, attested_at,
                 employee_id, employee_incarnation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(agent_id) DO UPDATE SET subject=excluded.subject, user_id=excluded.user_id,
                email_at_attestation=excluded.email_at_attestation, credential_seq=excluded.credential_seq,
                attested_by=excluded.attested_by, attested_at=excluded.attested_at,
                employee_id=excluded.employee_id, employee_incarnation=excluded.employee_incarnation",
            params![agent_id, subject, user_id, email, seq, attested_by, now, employee_id,
                i64::from(employee_incarnation)],
        )
        .map_err(|e| format!("attestation: {e}"))?;
        let detail = serde_json::json!({"agent_id": agent_id, "subject": subject, "user_id": user_id,
            "employee_id": employee_id, "employee_incarnation": employee_incarnation,
            "email_self_set": self_set, "acknowledged_self_set": self_set && acknowledge_self_set});
        audit(&tx, Some(attested_by), ATTESTED_ACTION, agent_id, &detail, &now)?;
        if self_set {
            // N1 (3): the Admin acknowledged attesting a self-set address.
            audit(&tx, Some(attested_by), SELF_SET_ACKNOWLEDGED_ACTION, agent_id, &detail, &now)?;
        }
        let attested = Attested {
            attestation: DelegateAttestation {
                agent_id: agent_id.to_owned(), subject: subject.to_owned(), user_id: user_id.clone(),
                email_at_attestation: email.clone(), credential_seq: seq, attested_by: attested_by.to_owned(),
                attested_at: now, employee_id: Some(employee_id.to_owned()),
                employee_incarnation: Some(employee_incarnation),
            },
            display_name: display_name.clone(),
            email_self_set: self_set,
        };
        tx.commit().map_err(|e| format!("attestation: {e}"))?;
        Ok(attested)
    }

    /// SR4 round 3 (N1 (2)): what an Admin is shown about the holder of a
    /// delegate subject before attesting. `Ok(None)`: no such account.
    pub fn google_delegate_holder(&self, user_id: &str) -> Result<Option<DelegateHolder>, String> {
        let conn = self.conn();
        let row: Option<(String, String)> = conn
            .query_row("SELECT display_name, created_at FROM users WHERE id = ?1", params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(|e| format!("attestation: {e}"))?;
        let Some((display_name, created_at)) = row else { return Ok(None) };
        let email_self_set = email_self_set(&conn, user_id)?;
        Ok(Some(DelegateHolder { user_id: user_id.to_owned(), display_name, created_at, email_self_set }))
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
        audit(&tx, Some(actor), REVOKED_ACTION, agent_id,
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
                "SELECT agent_id, subject, user_id, email_at_attestation, credential_seq, attested_by, attested_at,
                        employee_id, employee_incarnation
                 FROM google_delegate_attestations WHERE agent_id = ?1",
                params![agent_id],
                |row| Ok(DelegateAttestation {
                    agent_id: row.get(0)?, subject: row.get(1)?, user_id: row.get(2)?,
                    email_at_attestation: row.get(3)?, credential_seq: row.get(4)?, attested_by: row.get(5)?,
                    attested_at: row.get(6)?, employee_id: row.get(7)?,
                    // An out-of-range incarnation never binds.
                    employee_incarnation: row.get::<_, Option<i64>>(8)?.and_then(|value| u32::try_from(value).ok()),
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
    use super::AttestationRequest;
    use crate::db::UserDb;
    use crate::models::{UserRole, UserStatus};

    const EMPLOYEE: &str = "01010101010101010101010101010101";

    fn db() -> (UserDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (UserDb::new(&dir.path().join("users.db")).unwrap(), dir)
    }
    fn lower(subject: &'static str) -> impl Fn(&str) -> bool {
        move |email: &str| email.trim().to_ascii_lowercase() == subject
    }
    fn request<'a>(agent: &'a str, subject: &'a str, user_id: &'a str) -> AttestationRequest<'a> {
        AttestationRequest { agent_id: agent, employee_id: EMPLOYEE, employee_incarnation: 0, subject, user_id,
            acknowledge_self_set: false }
    }
    fn attest(db: &UserDb, agent: &str, subject: &'static str, user_id: &str) -> Result<super::Attested, String> {
        db.attest_google_delegate(&request(agent, subject, user_id), &lower(subject), "admin-id")
    }

    #[test]
    fn sr4_attestation_binds_one_active_account_and_is_audited() {
        let (db, _dir) = db();
        let alice = db.create_user("Alice@Customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let state = db.google_delegate_attestation("bot").unwrap();
        assert!(state.is_none(), "no attestation ⇒ none (callers refuse)");
        let attested = attest(&db, "bot", "alice@customer.com", &alice.id).unwrap();
        assert_eq!((attested.attestation.user_id.as_str(), attested.attestation.email_at_attestation.as_str()),
            (alice.id.as_str(), "Alice@Customer.com"));
        assert_eq!(attested.display_name, "Alice");
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        assert!(!state.binds("alice@customer.com", "someone-else", EMPLOYEE, 0));
        assert!(!state.binds("other@customer.com", &alice.id, EMPLOYEE, 0));
        let rows = db.query_audit_log(None, Some(super::ATTESTED_ACTION), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target.as_deref(), Some("bot"));
        // Nobody / an inactive account / two accounts: never attestable.
        assert_eq!(attest(&db, "bot", "ghost@customer.com", &alice.id).unwrap_err(), super::NOT_ONE_PERSON);
        let bob = db.create_user("bob@customer.com", "Bob", "password1", UserRole::Employee).unwrap();
        db.set_user_status(&bob.id, UserStatus::Suspended).unwrap();
        assert!(attest(&db, "bot2", "bob@customer.com", &bob.id).is_err());
        assert!(db.attest_google_delegate(&request("bot2", "x", &bob.id), &|_: &str| true, "admin-id").is_err(),
            "ambiguous");
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
        // An email change BEFORE the attestation does not count (a self-set
        // address needs the acknowledgement, N1 (3)).
        db.change_own_email(&alice.id, "alice2@customer.com").unwrap();
        db.change_own_email(&alice.id, "alice@customer.com").unwrap();
        let acknowledged = AttestationRequest { acknowledge_self_set: true, ..request("bot", "alice@customer.com", &alice.id) };
        db.attest_google_delegate(&acknowledged, &lower("alice@customer.com"), "admin-id").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        // Owner self-change (even back to the same address) revokes.
        db.change_own_email(&alice.id, "alice.new@customer.com").unwrap();
        db.change_own_email(&alice.id, "alice@customer.com").unwrap();
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.email_changed_since && !state.binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        // Re-attesting starts a new window; an Admin or raw change revokes too.
        db.attest_google_delegate(&acknowledged, &lower("alice@customer.com"), "admin-id").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        db.conn().execute("UPDATE users SET email='raw@customer.com' WHERE id=?1", [&alice.id]).unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().email_changed_since);
        // Another account's change never revokes Alice's attestation. The raw
        // write is unattributed, so the address is no longer self-set.
        db.conn().execute("UPDATE users SET email='alice@customer.com' WHERE id=?1", [&alice.id]).unwrap();
        attest(&db, "bot", "alice@customer.com", &alice.id).unwrap();
        let carol = db.create_user("carol@customer.com", "Carol", "password1", UserRole::Employee).unwrap();
        db.change_own_email(&carol.id, "carol2@customer.com").unwrap();
        assert!(db.google_delegate_attestation("bot").unwrap().unwrap().binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
    }

    /// SR4 round 3 (N1 (1)): the Admin attests the person they were shown;
    /// anyone else holding the address refuses, audited.
    #[test]
    fn sr4r3_attestation_refuses_a_holder_other_than_the_one_shown() {
        let (db, _dir) = db();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        let mallory = db.create_user("mallory@customer.com", "Mallory", "password1", UserRole::Employee).unwrap();
        assert_eq!(attest(&db, "bot", "alice@customer.com", &mallory.id).unwrap_err(), super::HOLDER_MISMATCH);
        assert!(db.google_delegate_attestation("bot").unwrap().is_none(), "nothing written");
        let refused = db.query_audit_log(None, Some(super::REFUSED_ACTION), 10).unwrap();
        assert_eq!(refused.len(), 1);
        assert!(refused[0].detail.as_deref().unwrap_or_default().contains(super::HOLDER_MISMATCH_CODE));
        assert!(attest(&db, "bot", "alice@customer.com", "").is_err(), "a user id is required");
        assert!(attest(&db, "bot", "alice@customer.com", &alice.id).is_ok());
    }

    /// SR4 round 3 (N1 (2)/(3)): the holder view says whether the account set
    /// the address itself; such an address needs an explicit acknowledgement,
    /// which is audited.
    #[test]
    fn sr4r3_self_set_address_needs_acknowledgement() {
        let (db, _dir) = db();
        let mallory = db.create_user("mallory@customer.com", "Mallory", "password1", UserRole::Employee).unwrap();
        let holder = db.google_delegate_holder(&mallory.id).unwrap().unwrap();
        assert_eq!((holder.display_name.as_str(), holder.email_self_set), ("Mallory", false));
        assert!(!holder.created_at.is_empty());
        assert!(db.google_delegate_holder("nobody").unwrap().is_none());
        // Mallory sets her own login email to the CEO's address.
        db.change_own_email(&mallory.id, "ceo@customer.com").unwrap();
        assert!(db.google_delegate_holder(&mallory.id).unwrap().unwrap().email_self_set);
        assert_eq!(attest(&db, "bot", "ceo@customer.com", &mallory.id).unwrap_err(), super::SELF_SET_UNACKNOWLEDGED);
        assert!(db.google_delegate_attestation("bot").unwrap().is_none());
        let acknowledged = AttestationRequest { acknowledge_self_set: true, ..request("bot", "ceo@customer.com", &mallory.id) };
        let attested = db.attest_google_delegate(&acknowledged, &lower("ceo@customer.com"), "admin-id").unwrap();
        assert!(attested.email_self_set);
        let rows = db.query_audit_log(None, Some(super::SELF_SET_ACKNOWLEDGED_ACTION), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].detail.as_deref().unwrap_or_default().contains("\"acknowledged_self_set\":true"));
        // An Admin change is not a self change.
        let bob = db.create_user("bob@customer.com", "Bob", "password1", UserRole::Employee).unwrap();
        db.change_email("admin-id", &bob.id, "bob2@customer.com").unwrap();
        assert!(!db.google_delegate_holder(&bob.id).unwrap().unwrap().email_self_set);
    }

    /// SR4 round 3 (N2): an attestation binds the employee id and
    /// incarnation; a legacy row without them never binds; the Shared
    /// tombstone hook removes it.
    #[test]
    fn sr4r3_attestation_binds_employee_incarnation() {
        let (db, _dir) = db();
        let alice = db.create_user("alice@customer.com", "Alice", "password1", UserRole::Employee).unwrap();
        attest(&db, "bot", "alice@customer.com", &alice.id).unwrap();
        let state = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert!(state.binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        assert!(!state.binds("alice@customer.com", &alice.id, EMPLOYEE, 1), "a restored incarnation");
        assert!(!state.binds("alice@customer.com", &alice.id, "02020202020202020202020202020202", 0),
            "a recreated employee under the same label");
        assert!(!state.binds("alice@customer.com", &alice.id, "", 0));
        // A row written before SR4 round 3 (no employee) is kept but never binds.
        db.conn().execute("UPDATE google_delegate_attestations SET employee_id=NULL, employee_incarnation=NULL", [])
            .unwrap();
        let legacy = db.google_delegate_attestation("bot").unwrap().unwrap();
        assert_eq!(legacy.attestation.employee_id, None);
        assert!(!legacy.binds("alice@customer.com", &alice.id, EMPLOYEE, 0));
        // The Shared tombstone users.db hook revokes it, audited.
        attest(&db, "bot", "alice@customer.com", &alice.id).unwrap();
        {
            let mut conn = db.conn();
            let tx = conn.transaction().unwrap();
            crate::db::remove_shared_bindings_in_transaction(&tx, "bot").unwrap();
            tx.commit().unwrap();
        }
        assert!(db.google_delegate_attestation("bot").unwrap().is_none());
        let revoked = db.query_audit_log(None, Some(super::REVOKED_ACTION), 10).unwrap();
        assert!(revoked.iter().any(|row| row.detail.as_deref().unwrap_or_default().contains("employee_retired")));
    }

    /// SR4 round 3 (N2): the migration adds the employee columns to a table
    /// created before them and keeps its rows (not binding).
    #[test]
    fn sr4r3_migration_keeps_rows_without_employee() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE google_delegate_attestations (agent_id TEXT PRIMARY KEY, subject TEXT NOT NULL,
                user_id TEXT NOT NULL, email_at_attestation TEXT NOT NULL, credential_seq INTEGER NOT NULL,
                attested_by TEXT NOT NULL, attested_at TEXT NOT NULL);
             INSERT INTO google_delegate_attestations VALUES ('bot','a@customer.com','u','a@customer.com',0,'admin','t');",
        ).unwrap();
        super::migrate(&conn).unwrap();
        super::migrate(&conn).unwrap();
        let (employee, incarnation): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT employee_id, employee_incarnation FROM google_delegate_attestations WHERE agent_id='bot'", [],
            |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!((employee, incarnation), (None, None));
    }
}
