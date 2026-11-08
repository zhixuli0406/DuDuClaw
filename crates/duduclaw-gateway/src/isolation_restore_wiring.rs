//! Production wiring of the F6 restore / retire seams (native Linux isolation
//! v1.81, F6 design r5 §3.2–§3.12, task F6d).
//!
//! F6c, F6h and F6l left fail-closed seams; this module supplies the
//! production collaborators, each still failing closed:
//! - **Root idle** (M8) and **run stop** (S13): the spawnd `EmployeeIdle` and
//!   `StopEmployeeRuns` requests (`duduclaw_execution::spawnd::employee_control`).
//!   A transport error is unavailable, never "gone" and never "idle"; an
//!   unconfirmed stop stays the `runs:stop_unconfirmed` fence.
//! - **Store marks** (§3.3): [`RegistryMarks`] counts every `Gate`/`Pause`
//!   store of F6h's `LABEL_KEYED_STORES` exactly as `tombstone_effects`
//!   recorded them; an unreadable store is `Err` (`STORES_UNAVAILABLE`).
//!   [`RegistryCounts`] gives F6l's label-free decision the positive count.
//! - **Approval rule** (R1/R2/S1/S2/S16): [`LogRule`] (F6c restore) and
//!   [`RetireLogRule`] (F6l early-purged retire card) both run F6c2's
//!   [`EligibilityLogRule`] on the caller's users.db transaction.
//! - **Notices** (L1, S16, §3.12): [`queue_notice`] writes the outbox
//!   `employee_restore_notices` in apply.db with the ≤ 5 per owner / tenant
//!   per day cap; [`OutboxNotifier`] is F6l's production `RetireNotifier`.
//!   Delivery of the outbox (mail) is not in this build (F6e/F6f).
use std::path::Path;
use std::sync::Mutex;

use duduclaw_auth::admin_approval_rule::{self as rule, AdminApprovalRule, ApprovalDecision, ApprovalKind, ApprovalRequest,
    EligibilityLogRule};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_security::audit::Severity;
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::{json, Value};

use crate::fs_safe::sqlite::Access;
use crate::isolation_label_stores::{TombstoneAction, LABEL_KEYED_STORES};
use crate::isolation_lifecycle::{self as lifecycle, USERS_DB};
use crate::isolation_restore::{self as restore, Approval, ApprovalRule, Refusal, RequestKind, RootError};

/// L1: notices per owner (Personal) or per tenant (Shared) per day.
pub(crate) const MAX_NOTICES_PER_DAY: i64 = 5;
const DAY_SECS: i64 = 86_400;
const NOTICE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS employee_restore_notices (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        scope TEXT NOT NULL,
        event TEXT NOT NULL,
        recipients TEXT NOT NULL,
        details TEXT NOT NULL,
        capped INTEGER NOT NULL CHECK (capped IN (0, 1)),
        at INTEGER NOT NULL,
        delivered_at INTEGER);
    CREATE INDEX IF NOT EXISTS idx_employee_restore_notices_scope ON employee_restore_notices(scope, at);";

// ── spawnd (S13, M8) ────────────────────────────────────────────────────

/// `StopEmployeeRuns`: `Ok(run ids)` only for a **confirmed** stop.
pub(crate) fn spawnd_stop_runs(employee: EmployeeId) -> Result<Vec<String>, ()> {
    #[cfg(target_os = "linux")]
    {
        match duduclaw_execution::spawnd::employee_control::stop_employee_runs(employee) {
            Ok(stopped) if stopped.confirmed => Ok(stopped.runs.iter().map(hex::encode).collect()),
            Ok(_) | Err(_) => Err(()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = employee; Err(()) }
}

/// `EmployeeIdle`: root `NotFound` is reported as such (the precheck reads
/// it as inconsistent); every other failure is unavailable.
pub(crate) fn spawnd_employee_idle(employee: EmployeeId) -> Result<bool, RootError> {
    #[cfg(target_os = "linux")]
    {
        use duduclaw_execution::spawnd::employee_control::{employee_idle, EmployeeControlError};
        match employee_idle(employee) {
            Ok(report) => Ok(report.idle()),
            Err(EmployeeControlError::NotFound) => Err(RootError::NotFound),
            Err(EmployeeControlError::Unavailable | EmployeeControlError::Refused) => Err(RootError::IsolationUnavailable),
        }
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = employee; Err(RootError::IsolationUnavailable) }
}

// ── Label-keyed stores (§3.3, S10) ──────────────────────────────────────

/// The marks `tombstone_effects` records: one count per `Gate`/`Pause` store.
pub(crate) fn current_marks(home: &Path, label: &str) -> Result<Value, ()> {
    let mut marks = serde_json::Map::new();
    for store in LABEL_KEYED_STORES.iter().filter(|s| matches!(s.action, TombstoneAction::Gate | TombstoneAction::Pause)) {
        marks.insert(store.name.into(), json!(store.count_under(home, label).map_err(|_| ())?));
    }
    Ok(Value::Object(marks))
}

/// F6c `LabelStores` over the F6h registry.
pub(crate) struct RegistryMarks;
impl restore::LabelStores for RegistryMarks {
    fn marks(&self, home: &Path, label: &str) -> Result<Value, ()> { current_marks(home, label) }
}

/// F6l `LabelStores`: the sum over every registered store (S10 positive zero).
pub(crate) struct RegistryCounts;
impl crate::isolation_retire::LabelStores for RegistryCounts {
    fn count_under(&self, home: &Path, label: &str) -> Result<u64, ()> {
        LABEL_KEYED_STORES.iter().try_fold(0u64, |sum, store| {
            store.count_under(home, label).map(|n| sum.saturating_add(n)).map_err(|_| ())
        })
    }
}

// ── Approval rule (F6c2) ────────────────────────────────────────────────

pub(crate) fn approval_kind(kind: RequestKind) -> ApprovalKind {
    match kind {
        RequestKind::PersonalRestore => ApprovalKind::PersonalRestore,
        RequestKind::SharedRestore => ApprovalKind::SharedRestore,
        RequestKind::PersonalAdopt => ApprovalKind::PersonalAdopt,
        RequestKind::SharedAdopt => ApprovalKind::SharedAdopt,
        RequestKind::EarlyPurge => ApprovalKind::EarlyPurge,
    }
}

/// F6c's `ApprovalRule` over [`EligibilityLogRule`]. `fresh_step_up` is the
/// decide RPC's verified step-up of this call; the request/cancel paths pass
/// `false`, so the rule can never approve there. Notice addresses of every
/// allowed decision are kept for delivery after commit.
pub(crate) struct LogRule {
    pub(crate) fresh_step_up: bool,
    pub(crate) now: i64,
    notices: Mutex<Vec<String>>,
}

impl LogRule {
    pub(crate) fn new(fresh_step_up: bool, now: i64) -> Self { Self { fresh_step_up, now, notices: Mutex::new(Vec::new()) } }
    /// Addresses of the last allowed decision (S16).
    pub(crate) fn notice_addresses(&self) -> Vec<String> {
        self.notices.lock().map(|n| n.clone()).unwrap_or_default()
    }
}

impl LogRule {
    fn request<'a>(&self, requester: &'a str, approver: &'a str, kind: RequestKind) -> ApprovalRequest<'a> {
        let kind = approval_kind(kind);
        // Personal kinds are owner-filed: the owner is the requester.
        let owner = kind.is_personal().then_some(requester);
        ApprovalRequest { kind, requester_user_id: requester, owner_user_id: owner,
            approver_user_id: approver, fresh_step_up: self.fresh_step_up, now: self.now }
    }
}

/// Refusal code when the users.db self-approval audit row cannot be written.
pub(crate) const SELF_APPROVAL_AUDIT_UNAVAILABLE: &str = "SELF_APPROVAL_AUDIT_UNAVAILABLE";

impl ApprovalRule for LogRule {
    fn decide(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind) -> Result<Approval, &'static str> {
        match EligibilityLogRule.decide(users, &self.request(requester, approver, kind)) {
            ApprovalDecision::Allowed { self_approved, notice_addresses } => {
                if let Ok(mut kept) = self.notices.lock() { *kept = notice_addresses; }
                Ok(Approval { self_approved })
            }
            ApprovalDecision::Refused(refusal) => Err(refusal.code()),
        }
    }
    fn record_self_approval(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind, subject: &str)
        -> Result<(), &'static str> {
        // F6c2 refuses an autocommit connection: only the decision's tx writes it.
        rule::record_self_approval(users, &self.request(requester, approver, kind), subject)
            .map_err(|_| SELF_APPROVAL_AUDIT_UNAVAILABLE)
    }
}

/// F6l's retire-card rule for an early-purged row (S12), requester = proposer.
/// `step_up_verified_at`: the `verified_at` of the migration step-up proof the
/// confirmation RPC read for this call (`None`: none, e.g. a card listing).
/// F6fa: the rule's `fresh_step_up` is computed from it at each decision
/// (verified within [`duduclaw_auth::mfa::TTL`] seconds, not in the future),
/// never assumed.
pub(crate) struct RetireLogRule {
    pub(crate) now: fn() -> i64,
    pub(crate) step_up_verified_at: Option<u64>,
}

/// A migration step-up proof `verified_at` is fresh at `now`: the same
/// five-minute window `ApprovalSession::authorize` and
/// `UserDb::migration_confirmation_session` apply.
pub(crate) fn step_up_fresh(verified_at: Option<u64>, now: i64) -> bool {
    let Ok(now) = u64::try_from(now) else { return false };
    let ttl = u64::try_from(duduclaw_auth::mfa::TTL).unwrap_or(0);
    verified_at.is_some_and(|at| at <= now && now - at <= ttl)
}

fn retire_approval(decision: ApprovalDecision)
    -> Result<crate::isolation_retire::Approval, crate::isolation_retire::ApprovalRefusal> {
    use crate::isolation_retire::{Approval as A, ApprovalRefusal as R};
    match decision {
        ApprovalDecision::Allowed { self_approved: true, .. } => Ok(A::SelfApproved),
        ApprovalDecision::Allowed { .. } => Ok(A::OtherAdmin),
        ApprovalDecision::Refused(rule::ApprovalRefusal::SecondAdminRequired) => Err(R::NeedsOtherAdmin),
        ApprovalDecision::Refused(rule::ApprovalRefusal::ApproverNotQualified(_)) => Err(R::NotQualified),
        ApprovalDecision::Refused(rule::ApprovalRefusal::CoolingWindow) => Err(R::Cooling),
        ApprovalDecision::Refused(rule::ApprovalRefusal::StepUpRequired) => Err(R::StepUpRequired),
        ApprovalDecision::Refused(_) => Err(R::LogUnavailable),
    }
}

impl RetireLogRule {
    fn request<'a>(&self, requester: &'a str, approver: &'a str, owner: Option<&'a str>) -> ApprovalRequest<'a> {
        let now = (self.now)();
        ApprovalRequest { kind: ApprovalKind::EarlyPurgedRetire, requester_user_id: requester, owner_user_id: owner,
            approver_user_id: approver, fresh_step_up: step_up_fresh(self.step_up_verified_at, now), now }
    }
}

impl crate::isolation_retire::ApprovalRule for RetireLogRule {
    fn decide(&self, home: &Path, requester: &str, approver: &str, owner: Option<&str>)
        -> Result<crate::isolation_retire::Approval, crate::isolation_retire::ApprovalRefusal> {
        use crate::isolation_retire::ApprovalRefusal as R;
        let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(|_| R::LogUnavailable)?;
        let tx = users.transaction().map_err(|_| R::LogUnavailable)?;
        let decision = EligibilityLogRule.decide(&tx, &self.request(requester, approver, owner));
        tx.finish().map_err(|_| R::LogUnavailable)?;
        retire_approval(decision)
    }

    /// S12 + F6fa: the binding decision of the stage step, for every
    /// approval (self or another Admin). The retire card has no users.db
    /// write of its own, so this opens one IMMEDIATE users.db transaction
    /// (the caller holds its apply.db IMMEDIATE transaction, M6), decides in
    /// it and, for a self-approval, writes F6c2's row there — once per
    /// proposal (`subject`): a retried stage finds the row and writes nothing.
    fn confirm_at_apply(&self, home: &Path, requester: &str, approver: &str, owner: Option<&str>, subject: &str)
        -> Result<crate::isolation_retire::Approval, crate::isolation_retire::ApprovalRefusal> {
        use crate::isolation_retire::{Approval as A, ApprovalRefusal as R};
        let mut users = lifecycle::open_store(home, USERS_DB, Access::ReadWrite).map_err(|_| R::LogUnavailable)?;
        let tx = users.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| R::LogUnavailable)?;
        let request = self.request(requester, approver, owner);
        let approval = retire_approval(EligibilityLogRule.decide(&tx, &request))?;
        if approval == A::SelfApproved {
            let written: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM auth_audit_log WHERE action=?1 AND target=?2)",
                params![rule::SELF_APPROVED_AUDIT_ACTION, subject], |r| r.get(0)).map_err(|_| R::LogUnavailable)?;
            if !written { rule::record_self_approval(&tx, &request, subject).map_err(|_| R::LogUnavailable)?; }
        }
        tx.commit().map_err(|_| R::LogUnavailable)?;
        Ok(approval)
    }
}

pub(crate) fn unix_now() -> i64 { chrono::Utc::now().timestamp() }

// ── Notices (L1, S16) ───────────────────────────────────────────────────

/// Whom a notice is counted against (the L1 cap scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NoticeScope { Owner(String), Tenant(String) }
impl NoticeScope {
    fn key(&self) -> String {
        match self { Self::Owner(id) => format!("owner:{id}"), Self::Tenant(id) => format!("tenant:{id}") }
    }
}

/// Queue one notice. `capped` notices (request, cancel, deny, expiry: the
/// ones a requester can trigger at will) count against
/// [`MAX_NOTICES_PER_DAY`] per scope; over the cap they are dropped and the
/// drop is audited. Security notices (approval, self-approval, restore,
/// retire, root alerts) are never dropped. `Ok(false)` = suppressed.
pub(crate) fn queue_notice(home: &Path, scope: &NoticeScope, event: &str, recipients: &[String], details: &Value,
    capped: bool, now: i64) -> Result<bool, Refusal> {
    let mut db = restore::open_apply(home)?;
    db.execute_batch(NOTICE_SCHEMA).map_err(|_| Refusal::Unavailable)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| Refusal::Unavailable)?;
    let key = scope.key();
    if capped {
        let sent: i64 = tx.query_row("SELECT count(*) FROM employee_restore_notices WHERE scope=?1 AND capped=1 AND at>?2",
            params![key, now - DAY_SECS], |r| r.get(0)).map_err(|_| Refusal::Unavailable)?;
        if sent >= MAX_NOTICES_PER_DAY {
            tx.finish().map_err(|_| Refusal::Unavailable)?;
            restore::audit(home, "isolation.employee_restore_notice_suppressed", details["label"].as_str().unwrap_or(""),
                Severity::Info, json!({"scope": key, "event": event, "cap": MAX_NOTICES_PER_DAY}));
            return Ok(false);
        }
    }
    let mut recipients: Vec<String> = recipients.iter().filter(|r| !r.is_empty()).cloned().collect();
    recipients.sort();
    recipients.dedup();
    tx.execute("INSERT INTO employee_restore_notices(scope,event,recipients,details,capped,at) VALUES(?1,?2,?3,?4,?5,?6)",
        params![key, event, json!(recipients).to_string(), details.to_string(), capped, now]).map_err(|_| Refusal::Unavailable)?;
    tx.commit().map_err(|_| Refusal::Unavailable)?;
    Ok(true)
}

/// Queue one notice; a failure is never silent (F6fa). An uncapped notice
/// (a security event: approval, self-approval, restore, adoption, retire,
/// root alert) that cannot be queued is audited Critical
/// `isolation.employee_restore_notice_unqueued` (the Admins learn of the
/// event from the audit even with no outbox); a capped one is logged.
/// Returns whether the notice was queued.
pub(crate) fn queue_or_alert(home: &Path, scope: &NoticeScope, event: &str, recipients: &[String], details: &Value,
    capped: bool, now: i64) -> bool {
    match queue_notice(home, scope, event, recipients, details, capped, now) {
        Ok(queued) => queued,
        Err(refusal) => {
            let label = details["label"].as_str().unwrap_or("");
            if capped {
                tracing::warn!(event, label, code = refusal.code(), "restore notice could not be queued");
            } else {
                restore::audit(home, "isolation.employee_restore_notice_unqueued", label, Severity::Critical,
                    json!({"event": event, "scope": scope.key(), "recipients": recipients.len(), "code": refusal.code()}));
            }
            false
        }
    }
}

/// S16: every address the eligibility log shows for an account eligible in
/// the last 30 days (deleted accounts included). Unreadable ⇒ empty and,
/// since the Admins then get no notice of `event` at all, audited Critical
/// `isolation.employee_restore_notice_recipients_unavailable` (F6fa).
pub(crate) fn admin_notice_addresses(home: &Path, label: &str, event: &str, now: i64) -> Vec<String> {
    let read = || -> Result<Vec<String>, ()> {
        let users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).map_err(|_| ())?;
        rule::notice_addresses(&users, now).map_err(|_| ())
    };
    read().unwrap_or_else(|()| {
        restore::audit(home, "isolation.employee_restore_notice_recipients_unavailable", label, Severity::Critical,
            json!({"event": event, "reason": "admin_eligibility_log_unreadable"}));
        Vec::new()
    })
}

/// The current address of one account (the retire notice of a Personal owner).
fn account_address(home: &Path, user: &str) -> Option<String> {
    let users = lifecycle::open_store(home, USERS_DB, Access::ReadOnly).ok()?;
    users.query_row("SELECT email FROM users WHERE id=?1", [user], |r| r.get(0)).ok()
}

/// F6l's production `RetireNotifier`: the outbox, never capped.
pub(crate) struct OutboxNotifier;
impl crate::isolation_retire::RetireNotifier for OutboxNotifier {
    fn notify(&self, home: &Path, event: &str, owner: Option<&str>, details: &Value) {
        let now = unix_now();
        let label = details["label"].as_str().unwrap_or("");
        let mut recipients = admin_notice_addresses(home, label, event, now);
        if let Some(address) = owner.and_then(|owner| account_address(home, owner)) { recipients.push(address); }
        let scope = match owner {
            Some(owner) => NoticeScope::Owner(owner.to_owned()),
            None => NoticeScope::Tenant(String::new()),
        };
        queue_or_alert(home, &scope, event, &recipients, details, false, now);
    }
}

#[cfg(test)]
#[path = "isolation_restore_wiring_tests.rs"]
mod tests;
