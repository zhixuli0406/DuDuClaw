//! RM-136: `users.mfa_reset` — an Administrator resets another account's
//! second factor.
//!
//! The rules (second factor for an Administrator target, 7-day qualifying
//! Admin, fail closed) live in `duduclaw_auth::mfa::admin_reset` and run in
//! the reset transaction; this adapter only picks the proof from the
//! parameters, delivers the actor's own email confirmation code, records
//! exactly one outcome per attempt and notifies the affected person.
//!
//! **Audit (M5).** Every attempt — from the very first gate in
//! `handle_mfa_rpc` on — is owned by one [`ResetAttempt`] guard with a unique
//! attempt id. Its outcome row goes into the users.db audit outbox: in the
//! reset transaction itself when the auth layer decides, otherwise through a
//! short dedicated insert; if even that fails (users.db unavailable) the
//! event is appended to the hash-chained security audit directly (no outbox
//! row exists then). The guard records `unavailable` for any path that ends
//! without an outcome (its `Drop`), then drains the outbox
//! ([`drain_reset_audit_outbox`]); the gateway also drains at start.
//! Delivery is exactly once per `attempt_id` (protocol in
//! `duduclaw_auth::mfa::reset_outbox`; [`SecurityAudit`] is its sink). The
//! email-code start's held row is the attempt's one durable state: when its
//! release fails, the release is retried (queued for every later drain of
//! this process) and the outcome is never written a second way; if the
//! process dies first, the start-up drain delivers the held row itself
//! (`interrupted: true`). Never a secret in a row or event.
//!
//! Parameters (all with `user_id`, the target):
//! - `{code}` — fresh TOTP code or unused recovery code;
//! - `{password}` — current password; staff targets only;
//! - `{password, send_email_code: true}` — actor without an authenticator
//!   app: mail a confirmation code to the actor, answer `{reset_token}`;
//! - `{reset_token, email_code}` — finish that confirmation.
use super::*;
use duduclaw_auth::mfa::{AdminResetProof, AdminResetRefusal, ResetAuditRow};

/// Security audit event type of every `users.mfa_reset` attempt.
pub(crate) const RESET_AUDIT_EVENT: &str = "mfa.admin_reset";
const MAX_LOGGED_ID_CHARS: usize = 128;

/// What the parameters ask for.
enum Request {
    Factor(String),
    Password(String),
    EmailBegin(String),
    EmailCode { reset_token: String, code: String },
}

impl Request {
    fn parse(params: &Value) -> Self {
        let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);
        if let Some(reset_token) = text("reset_token") {
            return Self::EmailCode { reset_token, code: text("email_code").unwrap_or_default() };
        }
        if let Some(password) = text("password") {
            return if params.get("send_email_code").and_then(Value::as_bool) == Some(true) {
                Self::EmailBegin(password)
            } else {
                Self::Password(password)
            };
        }
        Self::Factor(text("code").unwrap_or_default())
    }
    fn kind(&self) -> &'static str {
        match self { Self::Factor(_) => "factor", Self::Password(_) => "password", Self::EmailBegin(_) => "email_code_begin",
            Self::EmailCode { .. } => "email_code" }
    }
}

enum Done {
    Reset { target_admin: bool },
    EmailSent(duduclaw_auth::mfa::AdminResetEmail),
}

/// The security audit event of one outbox row.
fn audit_event(row: &ResetAuditRow) -> duduclaw_security::audit::AuditEvent {
    let bounded = |id: &str| duduclaw_core::truncate_chars(id, MAX_LOGGED_ID_CHARS);
    duduclaw_security::audit::AuditEvent::new(
        RESET_AUDIT_EVENT, "dashboard", duduclaw_security::audit::Severity::Warning,
        json!({"attempt_id": row.attempt_id, "actor_user_id": bounded(&row.actor_user_id),
            "target_user_id": row.target_user_id.as_deref().map(bounded), "proof": row.proof,
            "outcome": row.outcome, "reason": row.reason, "target_admin": row.target_admin,
            "attempted_at": row.created_at, "interrupted": row.held}),
    )
}

fn notify(event: &duduclaw_security::audit::AuditEvent) {
    crate::security_autopilot::emit_security_autopilot_event(&event.severity, &event.event_type,
        Some(event.agent_id.as_str()), crate::security_autopilot::SOURCE_AUDIT);
}

/// Append one row that has no outbox row to the hash-chained security audit
/// (strict: an error means it was not written), then notify the security bus.
fn append_row(home: &std::path::Path, row: &ResetAuditRow) -> Result<(), String> {
    let event = audit_event(row);
    duduclaw_security::audit::try_append_audit_event(home, &event)?;
    notify(&event);
    Ok(())
}

/// The hash-chained security audit as the outbox's exactly-once sink: the
/// position is the log length, and `append_once` checks (under the audit
/// writer lock) only the records after it for this attempt's event.
struct SecurityAudit<'a>(&'a std::path::Path);

impl duduclaw_auth::mfa::ResetAuditSink for SecurityAudit<'_> {
    fn position(&mut self) -> std::result::Result<u64, String> {
        duduclaw_security::audit::audit_log_position(self.0)
    }

    fn append_once(&mut self, row: &ResetAuditRow, since: u64) -> std::result::Result<bool, String> {
        let event = audit_event(row);
        let attempt_id = row.attempt_id.as_str();
        let present = |line: &Value| line["event_type"] == RESET_AUDIT_EVENT && line["details"]["attempt_id"] == attempt_id;
        let appended = duduclaw_security::audit::try_append_audit_event_once(self.0, &event, since, &present)?;
        if appended { notify(&event); }
        Ok(appended)
    }
}

static DRAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A held-row release that failed, retried by every later drain of the same
/// users.db in this process.
struct PendingRelease {
    db: std::sync::Arc<duduclaw_auth::UserDb>,
    attempt_id: String,
    outcome: &'static str,
    reason: &'static str,
}

/// Bound of [`PENDING_RELEASES`] (one entry per held outbox row at most).
const MAX_PENDING_RELEASES: usize = duduclaw_auth::mfa::RESET_OUTBOX_MAX_ROWS as usize;
static PENDING_RELEASES: std::sync::Mutex<Vec<PendingRelease>> = std::sync::Mutex::new(Vec::new());

/// Retry the failed releases of `db`'s held rows. A release that answers
/// `Ok(false)` found its row already being delivered: that record stands.
fn retry_releases(db: &duduclaw_auth::UserDb) {
    let mut pending = PENDING_RELEASES.lock().unwrap_or_else(|e| e.into_inner());
    pending.retain(|release| !std::ptr::eq(std::sync::Arc::as_ptr(&release.db), db)
        || db.mfa_reset_outbox_release(&release.attempt_id, release.outcome, release.reason).is_err());
}

/// Deliver undelivered `users.mfa_reset` outbox rows to the security audit,
/// exactly once each, after retrying failed held-row releases.
/// `include_held` only at gateway start (a held row then belongs to a dead
/// process). Serialized within the process. Returns the rows delivered.
pub(crate) fn drain_reset_audit_outbox(home: &std::path::Path, db: &duduclaw_auth::UserDb, include_held: bool) -> usize {
    let _serial = DRAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    retry_releases(db);
    match db.mfa_reset_outbox_drain(include_held, &mut SecurityAudit(home)) {
        Ok(delivered) => delivered,
        Err(error) => {
            tracing::warn!(%error, "users.mfa_reset audit delivery pending; retried after the next attempt and at gateway start");
            0
        }
    }
}

/// The one owner of a `users.mfa_reset` attempt's audit record (module docs).
pub(crate) struct ResetAttempt {
    home: std::path::PathBuf,
    db: Option<std::sync::Arc<duduclaw_auth::UserDb>>,
    attempt_id: String,
    actor: String,
    target: Option<String>,
    proof: &'static str,
    recorded: bool,
}

impl ResetAttempt {
    /// Start an attempt before any gate runs. `db` is `None` when users.db is
    /// not available (the outcome then goes straight to the security audit).
    pub(crate) fn begin(home: &std::path::Path, db: Option<std::sync::Arc<duduclaw_auth::UserDb>>,
        ctx: &UserContext, params: &Value) -> Self {
        let target = params.get("user_id").and_then(Value::as_str).filter(|t| !t.is_empty())
            .map(|t| duduclaw_core::truncate_chars(t, MAX_LOGGED_ID_CHARS));
        Self {
            home: home.to_path_buf(), db, attempt_id: uuid::Uuid::new_v4().simple().to_string(),
            actor: ctx.user_id.clone(), target, proof: Request::parse(params).kind(), recorded: false,
        }
    }

    pub(crate) fn attempt_id(&self) -> &str { &self.attempt_id }

    fn row(&self, outcome: &str, reason: &str, target_admin: Option<bool>) -> ResetAuditRow {
        ResetAuditRow::new(&self.attempt_id, &self.actor, self.target.as_deref(), self.proof, outcome, reason, target_admin)
    }

    /// Record the outcome decided here: outbox insert, else direct append.
    fn record(&mut self, outcome: &str, reason: &str, target_admin: Option<bool>) {
        if self.recorded { return; }
        self.recorded = true;
        let row = self.row(outcome, reason, target_admin);
        if let Some(db) = &self.db
            && db.mfa_reset_outbox_record(&row).is_ok() {
            return;
        }
        if let Err(error) = append_row(&self.home, &row) {
            tracing::error!(%error, attempt_id = %self.attempt_id, "users.mfa_reset attempt could not be audited");
        }
    }

    /// The auth layer committed this attempt's outbox row.
    fn committed(&mut self) { self.recorded = true; }

    /// Finish the held email-start row (committed with the code) with its
    /// delivery outcome. The held row stays the attempt's one durable state:
    /// a failed release is queued and retried by every later drain (this
    /// guard's own `Drop` first), never recorded a second way. `Ok(false)`
    /// means a start-up drain already began delivering the held row; that
    /// single record stands.
    fn release(&mut self, outcome: &'static str, reason: &'static str) {
        self.recorded = true;
        let Some(db) = &self.db else { return };
        match db.mfa_reset_outbox_release(&self.attempt_id, outcome, reason) {
            Ok(true) => {}
            Ok(false) => tracing::warn!(attempt_id = %self.attempt_id,
                "users.mfa_reset held audit row already delivered as interrupted; final outcome not recorded"),
            Err(error) => {
                tracing::warn!(%error, attempt_id = %self.attempt_id, "users.mfa_reset audit release pending; retried");
                let mut pending = PENDING_RELEASES.lock().unwrap_or_else(|e| e.into_inner());
                if pending.len() < MAX_PENDING_RELEASES {
                    pending.push(PendingRelease { db: db.clone(), attempt_id: self.attempt_id.clone(), outcome, reason });
                } else {
                    tracing::error!(attempt_id = %self.attempt_id,
                        "users.mfa_reset release queue full; the held row is delivered as interrupted at gateway start");
                }
            }
        }
    }

    /// Record a refusal decided in the gateway and answer it.
    pub(crate) fn refuse(mut self, refusal: AdminResetRefusal) -> WsFrame {
        self.record("refused", refusal.reason(), None);
        refused(refusal)
    }
}

impl Drop for ResetAttempt {
    fn drop(&mut self) {
        // No path may end without an outcome (panics and forgotten branches).
        self.record("refused", AdminResetRefusal::Unavailable.reason(), None);
        if let Some(db) = &self.db {
            drain_reset_audit_outbox(&self.home, db, false);
        }
    }
}

/// A refusal with its stable code and the plain zh-TW sentence.
pub(crate) fn refused(refusal: AdminResetRefusal) -> WsFrame {
    WsFrame::Response { id: String::new(), ok: false, payload: None,
        error: Some(json!({"code": refusal.code(), "message": refusal.message()})) }
}

fn password_refusal(error: &str) -> AdminResetRefusal {
    if error.contains("嘗試次數過多") { AdminResetRefusal::RateLimited } else { AdminResetRefusal::FactorFailed }
}

impl MethodHandler {
    /// `users.mfa_reset`: every gate (users.db, policy store, dashboard
    /// session, rate limit) and the reset itself, all under one
    /// [`ResetAttempt`] guard.
    pub(super) async fn handle_mfa_reset_rpc(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = self.user_db.read().await.clone();
        let attempt = ResetAttempt::begin(&self.home_dir, db.clone(), ctx, &params);
        if acl::require_role(ctx, UserRole::Admin).is_err() { return attempt.refuse(AdminResetRefusal::ActorNotAdmin); }
        let Some(db) = db else { return attempt.refuse(AdminResetRefusal::Unavailable) };
        let personal = self.resolve_edition_profile().await.is_personal();
        if crate::mfa::enforced(&self.home_dir, personal).is_err() {
            return attempt.refuse(AdminResetRefusal::Unavailable);
        }
        let session = if params.get("password").is_some() || params.get("reset_token").is_some() {
            match crate::google_personal::session_for(&ctx.user_id) {
                Ok(session) => Some(session),
                Err(_) => return attempt.refuse(AdminResetRefusal::InvalidRequest),
            }
        } else { None };
        if !crate::server::check_login_rate_limit(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), &ctx.email) {
            return attempt.refuse(AdminResetRefusal::RateLimited);
        }
        self.handle_mfa_admin_reset(attempt, db, params, ctx, session).await
    }

    /// The reset after the gates of [`Self::handle_mfa_reset_rpc`].
    async fn handle_mfa_admin_reset(&self, mut attempt: ResetAttempt, db: std::sync::Arc<duduclaw_auth::UserDb>,
        params: Value, ctx: &UserContext, session: Option<String>) -> WsFrame {
        let actor = ctx.user_id.clone();
        let request = Request::parse(&params);
        let proof_kind = request.kind();
        let home = self.home_dir.clone();
        let Some(target) = params.get("user_id").and_then(Value::as_str).filter(|t| !t.is_empty()).map(str::to_owned) else {
            return attempt.refuse(AdminResetRefusal::InvalidRequest);
        };
        let needs_session = !matches!(request, Request::Factor(_));
        let session = match (needs_session, session) {
            (true, Some(session)) => session,
            (true, None) => return attempt.refuse(AdminResetRefusal::InvalidRequest),
            (false, _) => String::new(),
        };
        let attempt_id = attempt.attempt_id().to_owned();
        // `bool`: the auth layer committed this attempt's outbox row.
        let work = {
            let (db, home, actor, target) = (db.clone(), home.clone(), actor.clone(), target.clone());
            tokio::task::spawn_blocking(move || -> (Result<Done, AdminResetRefusal>, bool) {
                let now = chrono::Utc::now().timestamp();
                let cipher = crate::mfa::Cipher(&home);
                let checked = |proof: AdminResetProof<'_>| {
                    let (result, recorded) = db.mfa_admin_reset_recorded(&attempt_id, proof_kind, &actor, &target, proof, &cipher, now);
                    (result.map(|outcome| Done::Reset { target_admin: outcome.target_admin }), recorded)
                };
                match &request {
                    Request::Factor(code) => checked(AdminResetProof::Factor(code)),
                    Request::Password(password) => {
                        if actor == target { return (Err(AdminResetRefusal::SelfReset), false); }
                        match db.mfa_factor_password_challenge(&actor, password, &session, now) {
                            Ok(token) => checked(AdminResetProof::Password { reauth_token: &token, session: &session }),
                            Err(error) => (Err(password_refusal(&error)), false),
                        }
                    }
                    Request::EmailBegin(password) => match db.mfa_admin_reset_email_begin_recorded(&attempt_id, proof_kind,
                        &actor, &target, password, &session, crate::mfa_email::available(&home), now) {
                        // The held row was committed with the code.
                        Ok(mail) => (Ok(Done::EmailSent(mail)), true),
                        Err(refusal) => (Err(refusal), false),
                    },
                    Request::EmailCode { reset_token, code } =>
                        checked(AdminResetProof::EmailCode { reset_token, session: &session, code }),
                }
            }).await
        };
        match work {
            Ok((Ok(Done::Reset { target_admin }), recorded)) => {
                if recorded { attempt.committed() } else { attempt.record("allowed", "allowed", Some(target_admin)) }
                crate::mfa_email::notify_reset(&home, &db, &target);
                WsFrame::ok_response("", json!({"reset": true}))
            }
            Ok((Ok(Done::EmailSent(mail)), _)) => {
                if crate::mfa_email::send_reset_code(&home, &db, &actor, &mail).await.is_err() {
                    attempt.release("refused", AdminResetRefusal::EmailUnavailable.reason());
                    return refused(AdminResetRefusal::EmailUnavailable);
                }
                attempt.release("challenge_sent", "challenge_sent");
                WsFrame::ok_response("", json!({"email_code_sent": true, "reset_token": mail.reset_token,
                    "expires_in": duduclaw_auth::mfa::admin_reset::RESET_EMAIL_TTL}))
            }
            Ok((Err(refusal), true)) => {
                attempt.committed();
                refused(refusal)
            }
            Ok((Err(refusal), false)) => attempt.refuse(refusal),
            Err(_) => attempt.refuse(AdminResetRefusal::Unavailable),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    /// The users.mfa_reset outbox rows still undelivered (tests).
    pub(crate) fn pending(db: &duduclaw_auth::UserDb) -> Vec<duduclaw_auth::mfa::ResetAuditRow> {
        db.mfa_reset_outbox_pending(true).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(home: &std::path::Path, attempt_id: &str) -> Vec<duduclaw_security::audit::AuditEvent> {
        duduclaw_security::audit::read_recent_events(home, 1000).into_iter()
            .filter(|e| e.event_type == RESET_AUDIT_EVENT && e.details["attempt_id"] == attempt_id).collect()
    }

    fn sql(home: &std::path::Path, statement: &str) {
        rusqlite::Connection::open(home.join("users.db")).unwrap().execute_batch(statement).unwrap();
    }

    #[test]
    fn held_release_failure_is_retried_and_delivered_once_with_the_final_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let db = std::sync::Arc::new(duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap());
        let mut attempt = ResetAttempt {
            home: home.to_path_buf(), db: Some(db.clone()), attempt_id: uuid::Uuid::new_v4().simple().to_string(),
            actor: "actor-1".into(), target: Some("target-1".into()), proof: "email_code_begin", recorded: false,
        };
        let attempt_id = attempt.attempt_id.clone();
        // The held row the email-code start commits together with the code.
        let mut held = attempt.row("challenge_sent", "challenge_sent", None);
        held.held = true;
        db.mfa_reset_outbox_record(&held).unwrap();
        // The mail failed and so does the release.
        sql(home, "CREATE TRIGGER release_fails BEFORE UPDATE OF held ON mfa_reset_audit_outbox
            BEGIN SELECT RAISE(ABORT, 'disk full'); END;");
        attempt.release("refused", AdminResetRefusal::EmailUnavailable.reason());
        // End of the request: the retry still fails; nothing is written a
        // second way and the held row is kept unchanged.
        drop(attempt);
        assert!(events(home, &attempt_id).is_empty());
        let rows = test_support::pending(&db);
        assert_eq!((rows.len(), rows[0].held, rows[0].outcome.as_str()), (1, true, "challenge_sent"));
        // The store recovers; the next drain (here the start-up one) retries
        // the release first and delivers exactly one record, the final one.
        sql(home, "DROP TRIGGER release_fails;");
        assert_eq!(drain_reset_audit_outbox(home, &db, true), 1);
        assert_eq!(drain_reset_audit_outbox(home, &db, true), 0);
        let delivered = events(home, &attempt_id);
        assert_eq!(delivered.len(), 1);
        assert_eq!((&delivered[0].details["outcome"], &delivered[0].details["reason"], &delivered[0].details["interrupted"]),
            (&json!("refused"), &json!("email_unavailable"), &json!(false)));
        assert!(test_support::pending(&db).is_empty());
    }

    #[test]
    fn crash_after_the_audit_append_replays_without_a_second_record() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let row = ResetAuditRow::new(&uuid::Uuid::new_v4().simple().to_string(), "actor-1", Some("target-1"), "factor",
            "allowed", "allowed", Some(false));
        db.mfa_reset_outbox_record(&row).unwrap();
        append_row(home, &ResetAuditRow::new("unrelated", "actor-1", None, "factor", "refused", "rate_limited", None)).unwrap();
        // The append succeeds, the delete does not (what a crash right after
        // the append leaves on disk).
        sql(home, "CREATE TRIGGER crash_before_delete BEFORE DELETE ON mfa_reset_audit_outbox
            BEGIN SELECT RAISE(ABORT, 'crashed'); END;");
        assert_eq!(drain_reset_audit_outbox(home, &db, false), 0);
        assert_eq!(events(home, &row.attempt_id).len(), 1);
        // Restart: the start-up drain finds the record after the row's marker.
        sql(home, "DROP TRIGGER crash_before_delete;");
        assert_eq!(drain_reset_audit_outbox(home, &db, true), 1);
        assert_eq!(events(home, &row.attempt_id).len(), 1, "exactly one record per attempt id");
        assert!(test_support::pending(&db).is_empty());
        assert!(duduclaw_security::audit_chain::verify_chain(&home.join("security_audit.jsonl")).unwrap().is_intact());
    }
}
