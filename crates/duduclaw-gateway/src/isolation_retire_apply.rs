//! Approval and apply side of an `EmployeeRetire` card (F6 design r5 §3.12
//! "Gateway backend"). The confirmation store calls these through
//! `handlers::migration_authority::retire::EmployeeRetireAuthority`:
//! - approval (`validate`, item `Pending`): R11 again, and for an early-purged
//!   row the second-Admin rule with requester = proposer, approver = the live
//!   Admin approving the card (S12);
//! - `stage` / `publish` / `read_back` (item `Applying`): R11 and the rule
//!   again with the approver from the item's approval record, then
//!   `Action::Retire` (idempotent). At `stage` (F6fa) an early-purged row's
//!   rule is decided once more inside an apply.db IMMEDIATE transaction
//!   (R11 re-run there; the rule's users.db write transaction inside it, M6)
//!   for every approval, not only a self-approval, before any root effect.
//!   A Retired receipt matching `root_item` sets `root_state = retired`;
//! - `disable_and_read_back`: a ReadBack of the old allocation. Ready ⇒ no
//!   root effect yet ⇒ `Ok` (and, when the card is being rejected,
//!   `root_state` back to `allocated`); anything else ⇒ `Err` (forward only,
//!   R14): the item stays `Failed` and `retry` resumes the same operation.
use super::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Which apply step called (`stage` audits the approval once per attempt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step { Stage, Publish, ReadBack }

/// The result of the shared approval/apply checks.
pub(crate) struct Validated {
    pub(crate) spec: RetireSpec,
    pub(crate) approver: String,
    /// `Some` only for an early-purged row (S12).
    pub(crate) approval: Option<Approval>,
    pub(crate) checked: Checked,
}

pub(crate) struct RetireBackend {
    home: PathBuf,
    root: Arc<dyn RetireRoot>,
    rule: Arc<dyn ApprovalRule>,
    notifier: Arc<dyn RetireNotifier>,
    /// The live dashboard Admin of this call (`LiveAuthority.context.user_id`):
    /// the approver while the card is still Pending.
    actor: Option<String>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl RetireBackend {
    pub(crate) fn new(home: PathBuf, root: Arc<dyn RetireRoot>, rule: Arc<dyn ApprovalRule>,
        notifier: Arc<dyn RetireNotifier>, actor: Option<String>) -> Self {
        Self { home, root, rule, notifier, actor, clock: Arc::new(now_unix) }
    }
    /// Unix-seconds clock (tests drive the 24 h host-busy window).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_clock(mut self, clock: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self { self.clock = clock; self }

    /// Pending: the live approver; later: the actor of the approval record
    /// (the confirmation store's `approved` audit row, a user UUID).
    fn approver(&self, item: &ConfirmationItem) -> Result<String, RetireError> {
        if item.status == ConfirmationStatus::Pending { return self.actor.clone().ok_or(RetireError::NoApprover); }
        let store = ConfirmationStore::from_read_only_connection(
            crate::fs_safe::sqlite::open_existing(&self.home, CONFIRMATIONS_DB, Access::ReadOnly).map_err(|_| RetireError::Unavailable)?)
            .map_err(|_| RetireError::Unavailable)?;
        let actor = store.approver(&item.binding).map_err(|_| RetireError::Unavailable)?.ok_or(RetireError::NoApprover)?;
        Ok(uuid::Uuid::from_bytes(actor.0).to_string())
    }

    /// R11 (+ S12 rule) on one apply.db read snapshot; the rule reads
    /// users.db while that snapshot is held (apply.db before users.db, M6).
    pub(crate) fn validate(&self, item: &ConfirmationItem) -> Result<Validated, RetireError> {
        if item.category != ConfirmationCategory::EmployeeRetire { return Err(RetireError::Invalid); }
        let spec = allocation::retire_specification(item).map_err(|_| RetireError::Invalid)?;
        let key = proposal_key(&item.binding);
        let db = readable_apply(&self.home)?;
        let tx = db.unchecked_transaction().map_err(|_| RetireError::Unavailable)?;
        let checked = check(&tx, spec.employee, Expect::Proposal(&key))?;
        // Every digest-covered field still equals the live state (early flag,
        // digest prefix, target, UID, label).
        if spec_for(&checked, spec.employee, &spec.requested_by)? != spec { return Err(RetireError::Stale); }
        let approver = self.approver(item)?;
        allocation::validate_actor_id(&approver).map_err(|_| RetireError::NoApprover)?;
        let approval = if spec.early && !checked.retired {
            Some(self.rule.decide(&self.home, &spec.requested_by, &approver, checked.owner.as_deref()).map_err(RetireError::Approval)?)
        } else { None };
        tx.finish().map_err(|_| RetireError::Unavailable)?;
        Ok(Validated { spec, approver, approval, checked })
    }

    /// The stage step's binding decision for an early-purged row (S12,
    /// F6fa): one apply.db IMMEDIATE transaction re-runs R11, and inside it
    /// the rule decides again in its own users.db write transaction (the
    /// sole-Admin row written there), for a self-approval and for another
    /// Admin's alike. The answer must equal the validation's; the root retire
    /// runs only after this commits.
    fn confirm_in_apply_tx(&self, v: &Validated, key: &str, validated: Approval) -> Result<Approval, RetireError> {
        let mut db = writable_apply(&self.home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
        check(&tx, v.spec.employee, Expect::Proposal(key))?;
        let confirmed = self.rule.confirm_at_apply(&self.home, &v.spec.requested_by, &v.approver, v.checked.owner.as_deref(), key)
            .map_err(RetireError::Approval)?;
        if confirmed != validated { return Err(RetireError::Stale); }
        tx.commit().map_err(|_| RetireError::Unavailable)?;
        Ok(confirmed)
    }

    /// One apply step: `Action::Retire` with the recorded approver.
    pub(crate) fn apply(&self, item: &ConfirmationItem, step: Step) -> Result<(), RetireError> {
        let v = self.validate(item)?;
        let hex = hex::encode(v.spec.employee.0);
        let key = proposal_key(&item.binding);
        let staging = step == Step::Stage && !v.checked.retired;
        // S12: decided again inside the apply transaction before any root
        // effect (the self-approval row idempotent per proposal); a refusal
        // refuses the retire.
        let approval = match (staging, v.approval) {
            (true, Some(validated)) => Some(self.confirm_in_apply_tx(&v, &key, validated)?),
            (_, approval) => approval,
        };
        let self_approved = approval == Some(Approval::SelfApproved);
        let details = json!({"employee": hex, "proposal_id": key, "requested_by": v.spec.requested_by, "approved_by": v.approver,
            "early": v.spec.early, "self_approved": self_approved, "digest_prefix": v.spec.quarantine_digest_prefix, "uid": v.spec.uid});
        if staging {
            audit(&self.home, "isolation.employee_retire_approved", &v.spec.label, false, details.clone());
            if self_approved {
                audit(&self.home, "isolation.employee_retire_self_approved", &v.spec.label, true, details.clone());
            }
            self.notifier.notify(&self.home, "isolation.employee_retire_approved", v.checked.owner.as_deref(), &details);
        }
        let request = Request { schema: allocation::SCHEMA_V2, nonce: nonce(), action: Action::Retire, item: item.clone(),
            approver: Some(v.approver.clone()) };
        let inconsistent = |reason: &str| -> Result<(), RetireError> {
            audit(&self.home, "isolation.employee_root_inconsistent", &v.spec.label, true,
                json!({"employee": hex, "proposal_id": key, "stage": "retire_apply", "reason": reason}));
            self.notifier.notify(&self.home, "isolation.employee_root_inconsistent", None, &json!({"employee": hex, "label": v.spec.label}));
            Err(RetireError::RootInconsistent)
        };
        match self.root.exchange(&request) {
            Err(()) | Ok(Answer::Refused(Root::IsolationUnavailable)) => Err(RetireError::RootUnavailable),
            Ok(Answer::Refused(Root::NotReady)) => {
                self.host_busy(&v)?;
                Err(RetireError::HostBusy)
            }
            Ok(Answer::Refused(Root::NotFound)) => inconsistent("not_found"),
            Ok(Answer::Refused(Root::Inconsistent)) => inconsistent("inconsistent"),
            Ok(Answer::Refused(Root::InvalidRequest)) => inconsistent("invalid_request"),
            Ok(Answer::Reply(reply)) => {
                let (r, root) = (&reply.receipt, &v.checked.root.receipt);
                if reply.phase != Phase::Retired || !r.disabled || r.binding != v.checked.root.item.binding
                    || r.employee != root.employee || r.uid != root.uid || r.label != root.label {
                    return inconsistent("receipt_mismatch");
                }
                if record_retired(&self.home, v.spec.employee, Some(key.as_str()), r, (self.clock)())? {
                    audit(&self.home, "isolation.employee_retired", &v.spec.label, false, details.clone());
                    self.notifier.notify(&self.home, "isolation.employee_retired", v.checked.owner.as_deref(), &details);
                }
                Ok(())
            }
        }
    }

    /// S15: the first busy answer is recorded; after 24 h of waiting the
    /// operator is alerted once (the card keeps 等待主機空閒 and retries).
    fn host_busy(&self, v: &Validated) -> Result<(), RetireError> {
        let now = (self.clock)();
        let hex = hex::encode(v.spec.employee.0);
        let mut db = writable_apply(&self.home)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
        tx.execute("INSERT INTO employee_retire_waits(employee_hex,busy_since,alerted) VALUES(?1,?2,0)
            ON CONFLICT(employee_hex) DO NOTHING", params![hex, now]).map_err(|_| RetireError::Unavailable)?;
        let (since, alerted): (i64, i64) = tx.query_row("SELECT busy_since,alerted FROM employee_retire_waits WHERE employee_hex=?1",
            [&hex], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|_| RetireError::Unavailable)?;
        let waited = now.checked_sub(since).unwrap_or(0).max(0);
        let alert = alerted == 0 && waited >= HOST_BUSY_ALERT_SECS;
        if alert {
            tx.execute("UPDATE employee_retire_waits SET alerted=1 WHERE employee_hex=?1", [&hex]).map_err(|_| RetireError::Unavailable)?;
        }
        tx.commit().map_err(|_| RetireError::Unavailable)?;
        if alert {
            let details = json!({"employee": hex, "waited_secs": waited, "label": v.spec.label});
            audit(&self.home, "isolation.employee_retire_host_busy", &v.spec.label, true, details.clone());
            self.notifier.notify(&self.home, "isolation.employee_retire_host_busy", None, &details);
        }
        Ok(())
    }

    /// `disable_and_read_back` (R14): `Ok` only while the root still reports
    /// the old allocation Ready (no root effect yet).
    pub(crate) fn compensate(&self, item: &ConfirmationItem) -> Result<(), RetireError> {
        let spec = allocation::retire_specification(item).map_err(|_| RetireError::Invalid)?;
        let row = root_row(&readable_apply(&self.home)?, spec.employee)?.ok_or(RetireError::NotFound)?;
        if row.item.binding != spec.target { return Err(RetireError::Stale); }
        let (class, _) = classify_readback(&row.receipt, self.root.exchange(&readback_request(&row.item)));
        if class != RootClass::Ok { return Err(RetireError::ForwardOnly); }
        if item.status == ConfirmationStatus::Rejecting {
            let hex = hex::encode(spec.employee.0);
            let key = proposal_key(&item.binding);
            let mut db = writable_apply(&self.home)?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| RetireError::Unavailable)?;
            let changed = tx.execute("UPDATE root_allocations SET root_state='allocated',retire_proposal_id=NULL
                WHERE employee_hex=?1 AND root_state='retiring' AND retire_proposal_id=?2", params![hex, key])
                .map_err(|_| RetireError::Unavailable)?;
            tx.execute("DELETE FROM employee_retire_waits WHERE employee_hex=?1", [&hex]).map_err(|_| RetireError::Unavailable)?;
            tx.commit().map_err(|_| RetireError::Unavailable)?;
            if changed == 1 {
                let details = json!({"employee": hex, "proposal_id": key});
                audit(&self.home, "isolation.employee_retire_rejected", &spec.label, false, details.clone());
                self.notifier.notify(&self.home, "isolation.employee_retire_rejected", None, &details);
            }
        }
        Ok(())
    }
}
