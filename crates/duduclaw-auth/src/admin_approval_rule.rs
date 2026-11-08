//! Second-Admin approval rule (native Linux isolation v1.81, F6 design §3.4;
//! review rows R1, R2, S1, S2, S12, S16). One rule, used by
//! `isolation.employee_restore_decide` (every request kind, incl.
//! `early_purge`), the adoption proposal card (§3.10) and the retire card of
//! an early-purged row (§3.12).
//!
//! **Call it inside the transaction that applies the decision.** [`decide`]
//! takes the users.db connection of the caller's open transaction (a
//! `&rusqlite::Transaction` derefs to it), never opens one itself, and refuses
//! with [`ApprovalRefusal::NotInTransaction`] on an autocommit connection, so
//! the Admin count and the log are read in the same snapshot as the apply.
//!
//! **Rule.**
//! - A *qualifying* Admin is eligible now (role admin, active, no pending
//!   forced password change, a second factor; [`crate::admin_eligibility`]),
//!   has been eligible continuously for the whole cooling window
//!   ([`COOLING_SECS`], 7 days) according to `admin_eligibility_log`, and was
//!   not created or promoted inside the window by the requester or the owner
//!   (an unattributed creation/promotion counts as by them).
//! - The approver is a *party* when it is the requester or (Personal kinds)
//!   the receipt owner. A party may only approve when no other qualifying
//!   Admin exists ([`ApprovalRefusal::SecondAdminRequired`] otherwise); a
//!   non-party approver must qualify ([`ApprovalRefusal::ApproverNotQualified`]).
//! - Sole-Admin self-approval needs the approver eligible now, a fresh step-up,
//!   a log covering at least the cooling window (else
//!   [`ApprovalRefusal::AdminLogUnavailable`]), and no other account that was
//!   eligible at any moment of the window and is not eligible now, or that
//!   lost eligibility inside the window and regained it (demoted, disabled,
//!   forced password change, MFA/passkey reset, deleted;
//!   [`ApprovalRefusal::CoolingWindow`]). Lockout is never a loss.
//! - Every approval needs a fresh step-up ([`ApprovalRefusal::StepUpRequired`]);
//!   the caller verifies it and passes the result.
//!
//! **Fail closed.** Any read error, a missing creation marker, a schema
//! version other than this build's, a missing log trigger, or a log whose
//! latest row per account disagrees with the live accounts refuses with
//! [`ApprovalRefusal::AdminLogUnavailable`] — for every path, because
//! qualification itself is read from the log. Consequence (design §7): no
//! approval that needs the log can pass until the log covers 7 days after the
//! upgrade that created it.
//!
//! **Time.** `now` and the log's `at` are Unix seconds. Window membership is
//! decided by `seq` (append order): the window starts at the first row whose
//! `at` is after `now - 7 d`, so a clock that stepped back can only move rows
//! into the window, never out of it.
//!
//! **Notices (S16, S2).** [`ApprovalDecision::Allowed`] carries every notice
//! address the log shows for an account eligible at any time in the last
//! [`NOTICE_LOOKBACK_SECS`] (30 days) — the log, not the live account list, so
//! deleting an Admin does not silence them. The caller adds the Personal
//! tombstone-time address and delivers after commit. [`promotion_notices_since`]
//! gives the S2 "every Admin creation or promotion notifies every log-eligible
//! Admin" recipients for a writer that read [`crate::admin_eligibility::high_water`]
//! before its change.
use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, Connection};

use crate::admin_eligibility::{self, EligibilityEntry};

/// Cooling window and continuous-eligibility period (R2, S1, S2): 7 days.
pub const COOLING_SECS: i64 = 7 * 86_400;
/// How far back an account's eligibility earns it a notice (S16): 30 days.
pub const NOTICE_LOOKBACK_SECS: i64 = 30 * 86_400;
/// Audit action of a sole-Admin self-approval (§3.4), for every kind.
pub const SELF_APPROVED_AUDIT_ACTION: &str = "isolation.employee_restore_self_approved";
/// Rows read per query; a longer log is an error, never a truncated history.
const READ_LIMIT: usize = 200_000;
/// Longest accepted audit subject (request or proposal id).
const MAX_SUBJECT_CHARS: usize = 128;

/// What is being approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    PersonalRestore,
    SharedRestore,
    PersonalAdopt,
    SharedAdopt,
    EarlyPurge,
    /// Retire card of a row that was purged early (§3.12, S12).
    EarlyPurgedRetire,
}

impl ApprovalKind {
    /// Stable wire name (the `employee_restores.kind` values plus the retire card).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PersonalRestore => "personal_restore",
            Self::SharedRestore => "shared_restore",
            Self::PersonalAdopt => "personal_adopt",
            Self::SharedAdopt => "shared_adopt",
            Self::EarlyPurge => "early_purge",
            Self::EarlyPurgedRetire => "early_purged_retire",
        }
    }

    /// Personal kinds are owner-filed: the owner is required and is the requester.
    pub fn is_personal(self) -> bool {
        matches!(self, Self::PersonalRestore | Self::PersonalAdopt)
    }
}

/// One approval attempt. All ids are users.db `users.id` values.
#[derive(Debug, Clone, Copy)]
pub struct ApprovalRequest<'a> {
    pub kind: ApprovalKind,
    /// Who filed the request (Personal: the owner; retire card: the proposer).
    pub requester_user_id: &'a str,
    /// The receipt owner of a Personal row (required for Personal kinds and
    /// equal to the requester there; optional otherwise, e.g. an early purge
    /// of a Personal row). The owner never approves while another qualifying
    /// Admin exists.
    pub owner_user_id: Option<&'a str>,
    /// The Admin approving in this call.
    pub approver_user_id: &'a str,
    /// The approver presented a fresh step-up in this call (verified by the caller).
    pub fresh_step_up: bool,
    /// Unix seconds.
    pub now: i64,
}

/// Why an approver does not qualify (S2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotQualified {
    /// Not an eligible Admin right now.
    NotEligibleNow,
    /// Created or promoted inside the window by the requester/owner, or unattributed.
    PromotedByParty,
    /// Not eligible for the whole window (recent Admin, recent MFA, log too young).
    EligibleUnderCooling,
}

/// A refused approval. [`Self::code`] is the stable error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRefusal {
    /// Malformed request (empty id, Personal kind without its owner, `now <= 0`).
    InvalidRequest,
    /// No fresh step-up in this call.
    StepUpRequired,
    /// Called on an autocommit connection instead of inside the apply transaction.
    NotInTransaction,
    /// The approver is the requester or owner and another qualifying Admin exists (R1).
    SecondAdminRequired,
    /// The approver is not a qualifying Admin (S2).
    ApproverNotQualified(NotQualified),
    /// Sole-Admin self-approval inside the cooling window (R2, S1).
    CoolingWindow,
    /// The eligibility log is missing, unreadable, inconsistent or covers less
    /// than the cooling window (S1).
    AdminLogUnavailable,
}

impl ApprovalRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "INVALID_REQUEST",
            Self::StepUpRequired => "STEP_UP_REQUIRED",
            Self::NotInTransaction => "APPROVAL_NOT_IN_TRANSACTION",
            Self::AdminLogUnavailable => "ADMIN_LOG_UNAVAILABLE",
            Self::SecondAdminRequired => "SECOND_ADMIN_REQUIRED",
            Self::ApproverNotQualified(_) => "APPROVER_NOT_QUALIFIED",
            Self::CoolingWindow => "SELF_APPROVAL_COOLING",
        }
    }
}

/// Result of [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Approve. `self_approved` is the sole-Admin path: persist it on the
    /// request, audit [`SELF_APPROVED_AUDIT_ACTION`] (see
    /// [`record_self_approval`]) and notify `notice_addresses` after commit.
    Allowed { self_approved: bool, notice_addresses: Vec<String> },
    Refused(ApprovalRefusal),
}

impl ApprovalDecision {
    pub fn is_allowed(&self) -> bool { matches!(self, Self::Allowed { .. }) }
    pub fn refusal(&self) -> Option<ApprovalRefusal> {
        match self { Self::Refused(refusal) => Some(*refusal), Self::Allowed { .. } => None }
    }
    pub fn self_approved(&self) -> bool { matches!(self, Self::Allowed { self_approved: true, .. }) }
}

/// The rule behind a trait, so the gateway restore engine (F6c), the purge
/// decide path (F6i), the adoption card (F6g) and the retire card (F6l) can
/// take it as a dependency and tests can substitute it.
pub trait AdminApprovalRule: Send + Sync {
    /// See [`decide`]. `users_tx` is the users.db connection of the open
    /// transaction the decision is applied in.
    fn decide(&self, users_tx: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision;
}

/// The production rule: [`decide`] over `admin_eligibility_log`.
#[derive(Debug, Clone, Copy, Default)]
pub struct EligibilityLogRule;

impl AdminApprovalRule for EligibilityLogRule {
    fn decide(&self, users_tx: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        decide(users_tx, request)
    }
}

/// Decide one approval (module docs). Never errors: every failure is a refusal.
pub fn decide(conn: &Connection, request: &ApprovalRequest<'_>) -> ApprovalDecision {
    if let Some(refusal) = precheck(conn, request) {
        return ApprovalDecision::Refused(refusal);
    }
    match Snapshot::read(conn, request.now) {
        Ok(snapshot) => snapshot.decide(request),
        Err(_) => ApprovalDecision::Refused(ApprovalRefusal::AdminLogUnavailable),
    }
}

fn precheck(conn: &Connection, request: &ApprovalRequest<'_>) -> Option<ApprovalRefusal> {
    let owner_blank = request.owner_user_id.is_some_and(str::is_empty);
    let personal_mismatch = request.kind.is_personal()
        && request.owner_user_id != Some(request.requester_user_id);
    if request.requester_user_id.is_empty() || request.approver_user_id.is_empty()
        || owner_blank || personal_mismatch || request.now <= 0
    {
        return Some(ApprovalRefusal::InvalidRequest);
    }
    if !request.fresh_step_up {
        return Some(ApprovalRefusal::StepUpRequired);
    }
    if conn.is_autocommit() {
        return Some(ApprovalRefusal::NotInTransaction);
    }
    None
}

/// The log as read once inside the caller's transaction.
struct Snapshot {
    /// The log has existed for at least the cooling window.
    covered: bool,
    /// Eligible now (live accounts, equal to the log's latest rows).
    eligible_now: BTreeSet<String>,
    /// Each account's last row before the cooling window.
    at_window_start: BTreeMap<String, EligibilityEntry>,
    /// Rows inside the cooling window, append order.
    window: Vec<EligibilityEntry>,
    notice_addresses: Vec<String>,
}

impl Snapshot {
    fn read(conn: &Connection, now: i64) -> Result<Self, String> {
        if !admin_eligibility::log_intact(conn)? {
            return Err("admin eligibility log schema or triggers missing".into());
        }
        let since = admin_eligibility::log_since(conn)?.ok_or("admin eligibility log creation time missing")?;
        let window_start = now.checked_sub(COOLING_SECS).ok_or("time out of range")?;
        let lookback_start = now.checked_sub(NOTICE_LOOKBACK_SECS).ok_or("time out of range")?;
        let eligible_now = admin_eligibility::eligible_user_ids(conn)?;
        let logged_now: BTreeSet<String> = rows_before(conn, i64::MAX)?
            .into_values().filter(|e| e.eligible).map(|e| e.user_id).collect();
        if logged_now != eligible_now {
            return Err("admin eligibility log disagrees with the accounts".into());
        }
        let (at_window_start, window) = split_at(conn, window_start)?;
        let (at_lookback_start, lookback) = split_at(conn, lookback_start)?;
        Ok(Self {
            covered: since <= window_start,
            eligible_now,
            at_window_start,
            window,
            notice_addresses: notice_addresses_from(&at_lookback_start, &lookback),
        })
    }

    fn decide(self, request: &ApprovalRequest<'_>) -> ApprovalDecision {
        let parties: Vec<&str> = std::iter::once(request.requester_user_id)
            .chain(request.owner_user_id)
            .collect();
        let approver = request.approver_user_id;
        if !parties.contains(&approver) {
            return match self.qualification(approver, &parties) {
                Ok(()) => ApprovalDecision::Allowed { self_approved: false, notice_addresses: self.notice_addresses },
                Err(why) => ApprovalDecision::Refused(ApprovalRefusal::ApproverNotQualified(why)),
            };
        }
        let another_qualifies = self.eligible_now.iter()
            .filter(|user| !parties.contains(&user.as_str()))
            .any(|user| self.qualification(user, &parties).is_ok());
        let refusal = if another_qualifies {
            Some(ApprovalRefusal::SecondAdminRequired)
        } else if !self.eligible_now.contains(approver) {
            Some(ApprovalRefusal::ApproverNotQualified(NotQualified::NotEligibleNow))
        } else if !self.covered {
            Some(ApprovalRefusal::AdminLogUnavailable)
        } else if self.lost_in_window(approver) {
            Some(ApprovalRefusal::CoolingWindow)
        } else {
            None
        };
        match refusal {
            Some(refusal) => ApprovalDecision::Refused(refusal),
            None => ApprovalDecision::Allowed { self_approved: true, notice_addresses: self.notice_addresses },
        }
    }

    fn window_rows<'s>(&'s self, user: &'s str) -> impl Iterator<Item = &'s EligibilityEntry> + Clone + 's {
        self.window.iter().filter(move |entry| entry.user_id == user)
    }

    /// S2: eligible now, continuously for the whole window, not made Admin
    /// inside it by a party (an unattributed creation/promotion counts).
    fn qualification(&self, user: &str, parties: &[&str]) -> Result<(), NotQualified> {
        if !self.eligible_now.contains(user) {
            return Err(NotQualified::NotEligibleNow);
        }
        let mut rows = self.window_rows(user);
        let by_party = rows.clone().any(|entry| is_promotion(entry)
            && entry.actor_user_id.as_deref().is_none_or(|actor| parties.contains(&actor)));
        if by_party {
            return Err(NotQualified::PromotedByParty);
        }
        let eligible_at_start = self.at_window_start.get(user).is_some_and(|entry| entry.eligible);
        if !(self.covered && eligible_at_start && rows.all(|entry| entry.eligible)) {
            return Err(NotQualified::EligibleUnderCooling);
        }
        Ok(())
    }

    /// R2/S1 cooling: an account other than `except` that was eligible at any
    /// moment of the window and is not eligible now, or that lost eligibility
    /// inside the window at all (an eligible → ineligible row), even if it
    /// is eligible again: §5 counts every loss in the 7 days, so a forced
    /// password change the victim already cleared still blocks.
    fn lost_in_window(&self, except: &str) -> bool {
        let accounts: BTreeSet<&str> = self.at_window_start.keys().map(String::as_str)
            .chain(self.window.iter().map(|entry| entry.user_id.as_str()))
            .filter(|user| *user != except)
            .collect();
        accounts.into_iter().any(|user| {
            let at_start = self.at_window_start.get(user).is_some_and(|entry| entry.eligible);
            let was_eligible = at_start || self.window_rows(user).any(|entry| entry.eligible);
            let lost_on_the_way = self.window_rows(user)
                .try_fold(at_start, |previous, entry| {
                    if previous && !entry.eligible { None } else { Some(entry.eligible) }
                })
                .is_none();
            lost_on_the_way || (was_eligible && !self.eligible_now.contains(user))
        })
    }
}

fn is_promotion(entry: &EligibilityEntry) -> bool {
    matches!(entry.reason.as_str(), "admin_created" | "role")
}

const ENTRY_COLUMNS: &str = "seq,user_id,eligible,reason,actor_user_id,notice_address,at";

fn entry_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EligibilityEntry> {
    Ok(EligibilityEntry {
        seq: r.get(0)?,
        user_id: r.get(1)?,
        eligible: r.get::<_, i64>(2)? == 1,
        reason: r.get(3)?,
        actor_user_id: r.get(4)?,
        notice_address: r.get(5)?,
        at: r.get(6)?,
    })
}

fn bound() -> Result<i64, String> {
    i64::try_from(READ_LIMIT.saturating_add(1)).map_err(|e| e.to_string())
}

fn checked_len<T>(rows: Vec<T>) -> Result<Vec<T>, String> {
    if rows.len() > READ_LIMIT { Err("admin eligibility log longer than the read limit".into()) } else { Ok(rows) }
}

/// Each account's last row with `seq < before`.
fn rows_before(conn: &Connection, before: i64) -> Result<BTreeMap<String, EligibilityEntry>, String> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM admin_eligibility_log WHERE seq IN
         (SELECT MAX(seq) FROM admin_eligibility_log WHERE seq<?1 GROUP BY user_id) ORDER BY seq LIMIT ?2"
    )).map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![before, bound()?], entry_from_row).map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok(checked_len(rows)?.into_iter().map(|entry| (entry.user_id.clone(), entry)).collect())
}

/// Split the log at the first row (by `seq`) whose `at` is after `start`:
/// each account's state before it, and every row from it on.
fn split_at(conn: &Connection, start: i64) -> Result<(BTreeMap<String, EligibilityEntry>, Vec<EligibilityEntry>), String> {
    let first: Option<i64> = conn
        .query_row("SELECT MIN(seq) FROM admin_eligibility_log WHERE at>?1", [start], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let boundary = first.unwrap_or(i64::MAX);
    let before = rows_before(conn, boundary)?;
    let mut statement = conn.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM admin_eligibility_log WHERE seq>=?1 ORDER BY seq LIMIT ?2"
    )).map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![boundary, bound()?], entry_from_row).map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    Ok((before, checked_len(rows)?))
}

/// Every address the log shows for an account eligible at any moment from
/// the split point on, sorted and de-duplicated.
fn notice_addresses_from(before: &BTreeMap<String, EligibilityEntry>, rows: &[EligibilityEntry]) -> Vec<String> {
    let eligible: BTreeSet<&str> = before.values().filter(|e| e.eligible).map(|e| e.user_id.as_str())
        .chain(rows.iter().filter(|e| e.eligible).map(|e| e.user_id.as_str()))
        .collect();
    before.values().chain(rows.iter())
        .filter(|entry| eligible.contains(entry.user_id.as_str()))
        .filter_map(|entry| entry.notice_address.as_deref())
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// S16 recipients at `now`: every address the log shows for an account
/// eligible at any time in the last 30 days, including deleted accounts.
/// Fails closed on an unreadable log (callers must not fall back to the live
/// account list).
pub fn notice_addresses(conn: &Connection, now: i64) -> Result<Vec<String>, String> {
    let start = now.checked_sub(NOTICE_LOOKBACK_SECS).ok_or("time out of range")?;
    let (before, rows) = split_at(conn, start)?;
    Ok(notice_addresses_from(&before, &rows))
}

/// One Admin creation or promotion to announce (S2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionNotice {
    pub user_id: String,
    /// `admin_created` or `role`.
    pub reason: String,
    /// `None` when unattributed (CLI, migration, self-service).
    pub actor_user_id: Option<String>,
    pub at: i64,
    /// [`notice_addresses`] at `now`.
    pub recipients: Vec<String>,
}

/// S2: Admin creations and promotions logged after `after_seq` (read with
/// [`crate::admin_eligibility::high_water`] before the change), each with
/// every log-eligible Admin as recipient. A `role` row counts only while the
/// account is an Admin, so call it in or right after the writer's
/// transaction; demotions are not announced here.
pub fn promotion_notices_since(conn: &Connection, after_seq: i64, now: i64) -> Result<Vec<PromotionNotice>, String> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ENTRY_COLUMNS} FROM admin_eligibility_log l WHERE l.seq>?1
         AND (l.reason='admin_created' OR (l.reason='role'
              AND EXISTS(SELECT 1 FROM users u WHERE u.id=l.user_id AND u.role='admin')))
         ORDER BY l.seq LIMIT ?2"
    )).map_err(|e| e.to_string())?;
    let rows = statement.query_map(params![after_seq, bound()?], entry_from_row).map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let rows = checked_len(rows)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let recipients = notice_addresses(conn, now)?;
    Ok(rows.into_iter().map(|entry| PromotionNotice {
        user_id: entry.user_id,
        reason: entry.reason,
        actor_user_id: entry.actor_user_id,
        at: entry.at,
        recipients: recipients.clone(),
    }).collect())
}

/// Audit fields of a decision (§3.8 `audit_and_emit` carries the same names).
pub fn audit_fields(request: &ApprovalRequest<'_>, self_approved: bool) -> serde_json::Value {
    serde_json::json!({
        "kind": request.kind.as_str(),
        "requester": request.requester_user_id,
        "owner": request.owner_user_id,
        "approver": request.approver_user_id,
        "self_approved": self_approved,
    })
}

/// Write the users.db audit row of a sole-Admin self-approval
/// ([`SELF_APPROVED_AUDIT_ACTION`], target = `subject`, the request or
/// proposal id) on the caller's transaction, so it commits or rolls back with
/// the decision. Refuses outside a transaction and an empty or over-long subject.
pub fn record_self_approval(conn: &Connection, request: &ApprovalRequest<'_>, subject: &str) -> Result<(), String> {
    if conn.is_autocommit() {
        return Err("self-approval audit must be written inside the decision transaction".into());
    }
    if subject.is_empty() || subject.chars().count() > MAX_SUBJECT_CHARS || subject.chars().any(char::is_control) {
        return Err("invalid self-approval audit subject".into());
    }
    conn.execute(
        "INSERT INTO auth_audit_log(user_id,action,target,detail,timestamp) VALUES(?1,?2,?3,?4,?5)",
        params![request.approver_user_id, SELF_APPROVED_AUDIT_ACTION, subject,
            audit_fields(request, true).to_string(), chrono::Utc::now().to_rfc3339()],
    ).map(|_| ()).map_err(|e| e.to_string())
}

impl crate::UserDb {
    /// [`crate::admin_eligibility::high_water`] on a pooled connection: read
    /// it before an account write, then pass it to
    /// [`Self::admin_promotion_notices_since`] after the write commits.
    pub fn admin_eligibility_high_water(&self) -> Result<i64, String> {
        admin_eligibility::high_water(&self.conn())
    }

    /// [`promotion_notices_since`] on a pooled connection.
    pub fn admin_promotion_notices_since(&self, after_seq: i64, now: i64) -> Result<Vec<PromotionNotice>, String> {
        promotion_notices_since(&self.conn(), after_seq, now)
    }

    /// [`notice_addresses`] on a pooled connection.
    pub fn admin_notice_addresses(&self, now: i64) -> Result<Vec<String>, String> {
        notice_addresses(&self.conn(), now)
    }
}

#[cfg(test)]
#[path = "admin_approval_rule_tests.rs"]
mod tests;
