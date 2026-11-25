//! RM-136: an Administrator resets another account's second factor
//! (`users.mfa_reset`).
//!
//! One transaction decides and applies. Rules, all enforced here (not in the
//! dashboard):
//! - the actor is an active Admin and not the target; the target is active;
//! - **Administrator targets** — role `admin` now, or an Admin at any moment
//!   of the last 7 days according to `admin_eligibility_log`, so demote →
//!   reset → promote cannot skip these rules:
//!   - the actor presents a **fresh second factor** in this call: a TOTP code
//!     or an unused recovery code ([`AdminResetProof::Factor`]), or — only for
//!     an actor with no authenticator app (passkey-only or email-only) — the
//!     current password **plus** a one-time code mailed to the actor's own
//!     address ([`AdminResetProof::EmailCode`], started by
//!     [`UserDb::mfa_admin_reset_email_begin`]). The password alone
//!     ([`AdminResetProof::Password`]) is refused
//!     ([`AdminResetRefusal::SecondFactorRequired`]);
//!   - the actor is a *qualifying* Admin: eligible continuously for the whole
//!     7-day cooling window of the F6c2 rule
//!     ([`crate::admin_approval_rule::check_qualified_admin`]). A recently
//!     created or promoted Admin, one whose own factor was reset in the
//!     window, or any actor while the log is younger than 7 days, is refused
//!     ([`AdminResetRefusal::Cooling`]); an unreadable log refuses too. This
//!     rule is checked only once the second factor was verified: a caller
//!     without one is told a second factor is required, never its cooling
//!     state (the email-code start does not check it either);
//! - **other targets** keep the existing confirmation: password or a fresh
//!   factor, no cooling window, so a single-Admin company can still help its
//!   staff.
//!
//! Why the email path exists: `check_factor` verifies only TOTP and recovery
//! codes, and a first passkey enrollment issues no recovery codes, so a
//! passkey-only Administrator could otherwise not satisfy the second-factor
//! rule at all. Fail closed: any store error refuses.
//!
//! Audit (M5): every attempt's outcome row goes into the users.db audit
//! outbox (`reset_outbox`) in the reset transaction itself, so a committed
//! reset can never lack its record; the gateway delivers the rows to the
//! hash-chained security audit.
use super::*;

/// Lifetime of a reset confirmation email code (seconds); the password
/// capability it is bound to lives just as long.
pub const RESET_EMAIL_TTL: i64 = TTL;
const PURPOSE_PREFIX: &str = "mfa_reset:";
const MAX_ID_CHARS: usize = 128;

/// What the acting Administrator presents.
#[derive(Debug, Clone, Copy)]
pub enum AdminResetProof<'a> {
    /// A fresh TOTP code (six digits) or an unused recovery code.
    Factor(&'a str),
    /// A password capability from `mfa_factor_password_challenge` for this
    /// dashboard session. Never enough for an Administrator target.
    Password { reauth_token: &'a str, session: &'a str },
    /// The capability and code [`UserDb::mfa_admin_reset_email_begin`] issued.
    EmailCode { reset_token: &'a str, session: &'a str, code: &'a str },
}

impl AdminResetProof<'_> {
    /// Stable proof name for audit records (never the secret).
    pub fn kind(&self) -> &'static str {
        match self { Self::Factor(_) => "factor", Self::Password { .. } => "password", Self::EmailCode { .. } => "email_code" }
    }
}

/// Why a reset was refused. Closed set; [`Self::code`] is the wire code,
/// [`Self::reason`] the audit reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminResetRefusal {
    InvalidRequest,
    SelfReset,
    ActorNotAdmin,
    TargetUnavailable,
    /// Administrator target confirmed with the password only.
    SecondFactorRequired,
    /// The email-code path was asked for by an actor that has an authenticator app.
    UseAuthenticator,
    /// The actor has no second factor of its own.
    NoSecondFactor,
    /// Wrong, expired, reused or locked-out proof.
    FactorFailed,
    EmailUnavailable,
    RateLimited,
    /// The actor has not been a qualifying Admin for the whole 7-day window.
    Cooling,
    AdminLogUnavailable,
    Unavailable,
}

impl AdminResetRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "MFA_RESET_INVALID",
            Self::SelfReset => "MFA_RESET_SELF",
            Self::ActorNotAdmin => "MFA_RESET_NOT_ADMIN",
            Self::TargetUnavailable => "MFA_RESET_TARGET_UNAVAILABLE",
            Self::SecondFactorRequired => "MFA_RESET_SECOND_FACTOR_REQUIRED",
            Self::UseAuthenticator => "MFA_RESET_USE_AUTHENTICATOR",
            Self::NoSecondFactor => "MFA_RESET_NO_SECOND_FACTOR",
            Self::FactorFailed => "MFA_RESET_FACTOR_FAILED",
            Self::EmailUnavailable => "MFA_RESET_EMAIL_UNAVAILABLE",
            Self::RateLimited => "MFA_RESET_RATE_LIMITED",
            Self::Cooling => "MFA_RESET_COOLING",
            Self::AdminLogUnavailable => "MFA_RESET_ADMIN_LOG_UNAVAILABLE",
            Self::Unavailable => "MFA_RESET_UNAVAILABLE",
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::SelfReset => "self_reset",
            Self::ActorNotAdmin => "actor_not_admin",
            Self::TargetUnavailable => "target_unavailable",
            Self::SecondFactorRequired => "second_factor_required",
            Self::UseAuthenticator => "use_authenticator",
            Self::NoSecondFactor => "no_second_factor",
            Self::FactorFailed => "factor_failed",
            Self::EmailUnavailable => "email_unavailable",
            Self::RateLimited => "rate_limited",
            Self::Cooling => "cooling_window",
            Self::AdminLogUnavailable => "admin_log_unavailable",
            Self::Unavailable => "unavailable",
        }
    }
    /// Plain zh-TW sentence for people (the dashboard shows its own translation).
    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "請重新選擇要重設的帳號。",
            Self::SelfReset => "不能重設自己的兩步驟驗證。",
            Self::ActorNotAdmin => "需要目前的管理員權限。",
            Self::TargetUnavailable => "找不到這個帳號，或帳號已停用。",
            Self::SecondFactorRequired => "重設管理員的兩步驟驗證時，必須用你自己的驗證碼確認，只輸入密碼不夠。",
            Self::UseAuthenticator => "你已設定驗證器 App，請改用驗證器的驗證碼確認。",
            Self::NoSecondFactor => "請先替自己的帳號設定兩步驟驗證，再重設其他管理員。",
            Self::FactorFailed => "驗證失敗或已過期，請重新輸入。",
            Self::EmailUnavailable => "目前無法寄出確認碼，請改用驗證器的驗證碼或備援碼。",
            Self::RateLimited => "寄送太頻繁，請稍後再試。",
            Self::Cooling => "身為管理員並已設定兩步驟驗證未連續滿 7 天的帳號，不能重設其他管理員的兩步驟驗證。",
            Self::AdminLogUnavailable => "無法確認管理員資格，暫時不能重設其他管理員。",
            Self::Unavailable => "雙因素驗證服務無法使用，請稍後再試。",
        }
    }
}

/// A completed reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminResetOutcome {
    /// The target was treated as an Administrator (second factor + 7 days).
    pub target_admin: bool,
}

/// Private delivery data of [`UserDb::mfa_admin_reset_email_begin`]. Never
/// put `code` in an RPC response, log or audit record.
pub struct AdminResetEmail {
    pub reset_token: String,
    pub email: String,
    pub code: String,
}

type Refused<T> = std::result::Result<T, AdminResetRefusal>;

fn store<E>(_: E) -> AdminResetRefusal { AdminResetRefusal::Unavailable }

fn purpose(target: &str) -> String { format!("{PURPOSE_PREFIX}{target}") }

fn code_hash(token: &str, purpose: &str, code: &str) -> String {
    // The capability is stored only as a hash: no offline six-digit dictionary.
    token_hash(&format!("mfa-reset-email:{token}:{purpose}:{code}"))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_ID_CHARS && !id.chars().any(char::is_control)
}

fn has_totp(tx: &Connection, user: &str) -> Refused<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM mfa_credentials WHERE user_id=?1)", [user], |r| r.get(0)).map_err(store)
}

/// Every rule that does not depend on the proof, except the qualifying-Admin
/// rule ([`qualifying`]). Returns whether the target is treated as an
/// Administrator.
fn preconditions(tx: &Connection, actor: &str, target: &str, now: i64) -> Refused<bool> {
    use AdminResetRefusal::*;
    if !valid_id(actor) || !valid_id(target) || now <= 0 { return Err(InvalidRequest); }
    if actor == target { return Err(SelfReset); }
    let account = |id: &str| -> Refused<Option<(String, String)>> {
        tx.query_row("SELECT role,status FROM users WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(store)
    };
    match account(actor)? {
        Some((role, status)) if role == UserRole::Admin.to_string() && status == "active" => {}
        _ => return Err(ActorNotAdmin),
    }
    let target_role = match account(target)? {
        Some((role, status)) if status == "active" => role,
        _ => return Err(TargetUnavailable),
    };
    // Unknown history is treated as an Administrator (stricter).
    let target_admin = target_role == UserRole::Admin.to_string()
        || crate::admin_approval_rule::admin_within_window(tx, target, now).unwrap_or(true);
    Ok(target_admin)
}

/// The actor is a qualifying Admin (7-day window), for an Administrator
/// target. Checked only after the actor's second factor was verified, so a
/// caller without one learns only that a second factor is required, never
/// its cooling state.
fn qualifying(tx: &Connection, actor: &str, now: i64) -> Refused<()> {
    use crate::admin_approval_rule::{ApprovalRefusal, NotQualified};
    use AdminResetRefusal::*;
    match crate::admin_approval_rule::check_qualified_admin(tx, actor, now) {
        Ok(()) => Ok(()),
        Err(ApprovalRefusal::ApproverNotQualified(NotQualified::NotEligibleNow)) => Err(NoSecondFactor),
        Err(ApprovalRefusal::ApproverNotQualified(_)) => Err(Cooling),
        Err(ApprovalRefusal::AdminLogUnavailable) => Err(AdminLogUnavailable),
        Err(_) => Err(Unavailable),
    }
}

/// The reset itself, after every check passed.
fn apply(tx: &Connection, actor: &str, target: &str) -> Refused<()> {
    let since = crate::admin_eligibility::high_water(tx).map_err(store)?;
    clear(tx, target).map_err(store)?;
    crate::admin_eligibility::stamp_actor(tx, since, actor).map_err(store)?;
    audit(tx, target, "mfa.reset", Some(actor)).map_err(store)
}

/// A refusal and whether the work done to reach it (proof attempt counters,
/// consumed codes, `mfa.verification_failed`) must be committed: `true` for a
/// failed proof and for any refusal after the proof was verified.
type Decided = std::result::Result<AdminResetOutcome, (AdminResetRefusal, bool)>;

/// Decide and apply one reset on `tx` (a savepoint of the reset transaction).
fn decide(tx: &Connection, actor: &str, target: &str, proof: AdminResetProof<'_>, cipher: &dyn MfaCipher,
    now: i64) -> Decided {
    use AdminResetRefusal::*;
    let none = |refusal: AdminResetRefusal| (refusal, false);
    let target_admin = preconditions(tx, actor, target, now).map_err(none)?;
    let verified = match proof {
        AdminResetProof::Factor(code) => {
            if code.is_empty() || code.len() > 64 { return Err((FactorFailed, false)); }
            check_factor(tx, actor, code, code.len() != 6, cipher, now).is_ok()
        }
        AdminResetProof::Password { reauth_token, session } => {
            if target_admin { return Err((SecondFactorRequired, false)); }
            check_reauth(tx, actor, reauth_token, session, now).is_ok()
        }
        AdminResetProof::EmailCode { reset_token, session, code } => {
            if has_totp(tx, actor).map_err(none)? { return Err((UseAuthenticator, false)); }
            if check_reauth(tx, actor, reset_token, session, now).is_err() { return Err((FactorFailed, false)); }
            let failures = owner_factor::lockout(tx, actor).map_err(|_| (Unavailable, false))?;
            if owner_factor::locked(failures, now) { false } else {
                let purpose = purpose(target);
                let row: Option<(String, i64)> = tx.query_row("SELECT c.code_hash,c.expires_at
                    FROM mfa_restore_email_codes c JOIN users u ON u.id=c.user_id
                    WHERE c.token_hash=?1 AND c.user_id=?2 AND c.purpose=?3 AND c.email=u.email AND c.epoch=u.credential_epoch",
                    params![token_hash(reset_token), actor, purpose], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional().map_err(|_| (Unavailable, false))?;
                // Any presentation consumes the code.
                tx.execute("DELETE FROM mfa_restore_email_codes WHERE token_hash=?1 AND user_id=?2",
                    params![token_hash(reset_token), actor]).map_err(|_| (Unavailable, false))?;
                let digits = code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit());
                let ok = row.is_some_and(|(hash, expires_at)| expires_at > now && digits
                    && bool::from(hash.as_bytes().ct_eq(code_hash(reset_token, &purpose, code).as_bytes())));
                owner_factor::count_attempt(tx, actor, ok, failures, now).map_err(|_| (Unavailable, false))?;
                ok
            }
        }
    };
    if !verified {
        audit(tx, actor, "mfa.verification_failed", None).map_err(|_| (Unavailable, false))?;
        return Err((FactorFailed, true));
    }
    // The second factor is verified; nothing of the reset has been written
    // yet. A refusal from here on (cooling window, unreadable admin log)
    // commits the savepoint so far: the factor stays consumed (recovery code
    // deleted, TOTP step recorded as used, email code deleted) and only the
    // reset is not performed. Rolling back would make a single-use code
    // reusable.
    if target_admin {
        qualifying(tx, actor, now).map_err(|refusal| (refusal, true))?;
    }
    if let AdminResetProof::Password { reauth_token: token, .. } | AdminResetProof::EmailCode { reset_token: token, .. } = proof {
        tx.execute("UPDATE mfa_challenges SET consumed=1 WHERE token_hash=?1", [token_hash(token)])
            .map_err(|_| (Unavailable, false))?;
    }
    apply(tx, actor, target).map_err(none)?;
    Ok(AdminResetOutcome { target_admin })
}

impl UserDb {
    /// `users.mfa_reset` (module docs) with a fresh attempt id; see
    /// [`Self::mfa_admin_reset_recorded`].
    pub fn mfa_admin_reset_checked(&self, actor: &str, target: &str, proof: AdminResetProof<'_>,
        cipher: &dyn MfaCipher, now: i64) -> Refused<AdminResetOutcome> {
        let attempt = reset_outbox::new_attempt_id().map_err(store)?;
        self.mfa_admin_reset_recorded(&attempt, proof.kind(), actor, target, proof, cipher, now).0
    }

    /// `users.mfa_reset` (module docs). One transaction decides, applies and
    /// writes the attempt's outcome row into the audit outbox
    /// (`reset_outbox`): the reset, its row, and for a failed proof the
    /// attempt counters, consumed codes and `mfa.verification_failed` commit
    /// together; a verified proof refused by the qualifying-Admin rule
    /// commits the proof's consumption with its row (never the reset); every
    /// other refusal commits only its outbox row. A full
    /// outbox refuses before any proof is checked. The flag is `true` when
    /// the outcome row was committed; `false` (nothing committed) when the
    /// store failed — the caller must then record the attempt itself.
    #[allow(clippy::too_many_arguments)]
    pub fn mfa_admin_reset_recorded(&self, attempt_id: &str, proof_label: &str, actor: &str, target: &str,
        proof: AdminResetProof<'_>, cipher: &dyn MfaCipher, now: i64) -> (Refused<AdminResetOutcome>, bool) {
        use AdminResetRefusal::*;
        let Ok(_authority) = self.google_authority_write() else { return (Err(Unavailable), false) };
        let mut conn = self.conn();
        let Ok(mut tx) = conn.transaction_with_behavior(TransactionBehavior::Immediate) else { return (Err(Unavailable), false) };
        if !matches!(reset_outbox::has_room(&tx), Ok(true)) { return (Err(Unavailable), false); }
        let decided = {
            let Ok(savepoint) = tx.savepoint() else { return (Err(Unavailable), false) };
            let decided = decide(&savepoint, actor, target, proof, cipher, now);
            match decided {
                // Keep the reset, the counters of a failed proof, or the
                // consumption of a verified proof whose reset was refused.
                Ok(_) | Err((_, true)) => if savepoint.commit().is_err() { return (Err(Unavailable), false) },
                // Every other refusal changes nothing (rolled back on drop).
                Err((_, false)) => drop(savepoint),
            }
            decided
        };
        let (result, row) = match decided {
            Ok(outcome) => (Ok(outcome), ResetAuditRow::new(attempt_id, actor, Some(target), proof_label,
                "allowed", "allowed", Some(outcome.target_admin))),
            Err((refusal, _)) => (Err(refusal), ResetAuditRow::new(attempt_id, actor, Some(target), proof_label,
                "refused", refusal.reason(), None)),
        };
        if reset_outbox::insert(&tx, &row).is_err() || tx.commit().is_err() {
            // Nothing committed, the reset included.
            return (Err(Unavailable), false);
        }
        (result, true)
    }

    /// Start the email-code confirmation (see
    /// [`Self::mfa_admin_reset_email_begin_recorded`]) without an outbox row.
    #[cfg(test)]
    pub(crate) fn mfa_admin_reset_email_begin(&self, actor: &str, target: &str, password: &str, session: &str,
        email_available: bool, now: i64) -> Refused<AdminResetEmail> {
        self.email_begin(None, actor, target, password, session, email_available, now)
    }

    /// Start the email-code confirmation for an actor without an
    /// authenticator app (module docs): verify `password` for this dashboard
    /// `session`, then mint a six-digit code for the actor's own current
    /// address, bound to this target, hashed with its capability,
    /// [`RESET_EMAIL_TTL`] seconds, single use, on the email-factor send
    /// budget (≤ 5 per hour, ≥ 60 s apart). `email_available` is the mail
    /// sender's availability, checked before any budget is spent.
    ///
    /// On success the transaction that mints the code also writes the
    /// attempt's **held** outbox row (`challenge_sent`); the caller releases
    /// it with the delivery outcome (`mfa_reset_outbox_release`). On a
    /// refusal nothing is recorded and the caller records the attempt.
    #[allow(clippy::too_many_arguments)]
    pub fn mfa_admin_reset_email_begin_recorded(&self, attempt_id: &str, proof_label: &str, actor: &str, target: &str,
        password: &str, session: &str, email_available: bool, now: i64) -> Refused<AdminResetEmail> {
        self.email_begin(Some((attempt_id, proof_label)), actor, target, password, session, email_available, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn email_begin(&self, record: Option<(&str, &str)>, actor: &str, target: &str, password: &str, session: &str,
        email_available: bool, now: i64) -> Refused<AdminResetEmail> {
        use AdminResetRefusal::*;
        if actor == target { return Err(SelfReset); }
        if password.is_empty() || password.len() > 4096 || session.is_empty() { return Err(InvalidRequest); }
        let check = |tx: &Connection| -> Refused<()> {
            if record.is_some() && !reset_outbox::has_room(tx).map_err(store)? { return Err(Unavailable); }
            preconditions(tx, actor, target, now)?;
            if has_totp(tx, actor)? { return Err(UseAuthenticator); }
            if !any_factor(tx, actor).map_err(store)? { return Err(NoSecondFactor); }
            Ok(())
        };
        {
            let mut conn = self.conn();
            let tx = conn.transaction().map_err(store)?;
            check(&*tx)?;
        }
        if !email_available { return Err(EmailUnavailable); }
        let token = self.mfa_factor_password_challenge(actor, password, session, now)
            .map_err(|error| if error.contains("嘗試次數過多") { RateLimited } else { FactorFailed })?;
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store)?;
        check(&*tx)?;
        tx.execute("DELETE FROM mfa_email_sends WHERE sent_at<=?1", [now - 3600]).map_err(store)?;
        let (count, last): (i64, Option<i64>) = tx.query_row("SELECT COUNT(*),MAX(sent_at) FROM mfa_email_sends WHERE user_id=?1",
            [actor], |r| Ok((r.get(0)?, r.get(1)?))).map_err(store)?;
        if count >= 5 || last.is_some_and(|at| at > now - 60) { return Err(RateLimited); }
        let account = crate::user_row::load_user(&tx, actor).map_err(store)?.ok_or(ActorNotAdmin)?;
        let code = email::numeric_code().map_err(store)?;
        let purpose = purpose(target);
        tx.execute("DELETE FROM mfa_restore_email_codes WHERE expires_at<=?1 OR (user_id=?2 AND purpose=?3)",
            params![now, actor, purpose]).map_err(store)?;
        tx.execute("INSERT INTO mfa_restore_email_codes(token_hash,user_id,purpose,email,code_hash,epoch,expires_at)
            VALUES(?1,?2,?3,?4,?5,?6,?7)", params![token_hash(&token), actor, purpose, account.email,
            code_hash(&token, &purpose, &code), account.credential_epoch, now + RESET_EMAIL_TTL]).map_err(store)?;
        tx.execute("INSERT INTO mfa_email_sends(user_id,sent_at) VALUES(?1,?2)", params![actor, now]).map_err(store)?;
        audit(&tx, actor, "mfa.reset_email_sent", None).map_err(store)?;
        if let Some((attempt_id, proof_label)) = record {
            let mut row = ResetAuditRow::new(attempt_id, actor, Some(target), proof_label, "challenge_sent", "challenge_sent", None);
            row.held = true;
            reset_outbox::insert(&tx, &row).map_err(store)?;
        }
        tx.commit().map_err(store)?;
        Ok(AdminResetEmail { reset_token: token, email: account.email, code })
    }

    /// The reset confirmation email could not be delivered: drop its code.
    pub fn mfa_admin_reset_email_discard(&self, actor: &str, reset_token: &str) -> Result<()> {
        self.conn().execute("DELETE FROM mfa_restore_email_codes WHERE token_hash=?1 AND user_id=?2",
            params![token_hash(reset_token), actor]).map(|_| ()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
#[path = "admin_reset_tests.rs"]
mod tests;
