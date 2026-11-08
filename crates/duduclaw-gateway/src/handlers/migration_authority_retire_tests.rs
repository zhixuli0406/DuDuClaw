//! F6l gateway retire (F6 design r5 §3.12, §6 F6l row): real apply.db and
//! confirmation store, the real `GatewayAuthority` card path, a fake spawnd
//! (schema 2), a fake F6c2 approval rule and a notice recorder.
use super::*;
use super::super::GatewayAuthority;
use crate::isolation_retire::*;
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Action, Answer, ErrorCode as Root, Phase, Receipt, Reply, Request};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationStore, ConfirmationError, ProposalApplier, ProposalValidator};
use duduclaw_migration::ids::RequestId;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

const LABEL: &str = "sales";
const EMPLOYEE: EmployeeId = EmployeeId([0xe1; 16]);
const OWNER: &str = "owner-1";
const PROPOSER: &str = "11111111-1111-1111-1111-111111111111";
const APPROVER: &str = "22222222-2222-2222-2222-222222222222";
const CONFIRMATIONS: &str = "migration-confirmations.db";
const APPLY: &str = "migration-authority/apply.db";
const DIGEST: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

struct Accept;
impl ProposalValidator for Accept { fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) } }
impl ProposalApplier for Accept {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}

/// spawnd as the retire sees it. `phase: None` = no trace anywhere.
#[derive(Default)]
struct Spawnd { phase: Option<Phase>, busy: bool, unavailable: bool, inconsistent: bool, lose_reply_after_start: bool,
    retires: usize, readbacks: usize, approvers: Vec<String> }
struct FakeRoot { state: Mutex<Spawnd>, receipt: Receipt }
impl FakeRoot {
    fn reply(&self, phase: Phase) -> Answer {
        let mut receipt = self.receipt.clone();
        receipt.disabled = matches!(phase, Phase::Disabled | Phase::Retired);
        Answer::Reply(Reply { receipt, phase, idle: (phase == Phase::Ready).then_some(true) })
    }
}
impl RetireRoot for FakeRoot {
    fn exchange(&self, request: &Request) -> std::result::Result<Answer, ()> {
        let mut s = self.state.lock().unwrap();
        if s.unavailable { return Err(()); }
        if s.inconsistent { return Ok(Answer::Refused(Root::Inconsistent)); }
        match request.action {
            Action::ReadBack => {
                s.readbacks += 1;
                Ok(match s.phase { None => Answer::Refused(Root::NotFound), Some(phase) => self.reply(phase) })
            }
            Action::Retire => {
                let spec = request.validate_retire().map_err(|_| ())?;
                assert_eq!(spec.target, self.receipt.binding, "the retire names the stored allocation");
                s.approvers.push(request.approver.clone().unwrap());
                match s.phase {
                    Some(Phase::Retired) => Ok(self.reply(Phase::Retired)),
                    Some(Phase::Ready) if s.busy => Ok(Answer::Refused(Root::NotReady)),
                    Some(Phase::Ready | Phase::Retiring) => {
                        if s.lose_reply_after_start { s.phase = Some(Phase::Retiring); return Err(()); }
                        s.retires += 1; s.phase = Some(Phase::Retired); Ok(self.reply(Phase::Retired))
                    }
                    None => Ok(Answer::Refused(Root::NotFound)),
                    Some(_) => Ok(Answer::Refused(Root::InvalidRequest)),
                }
            }
            _ => Err(()),
        }
    }
}
/// F6c2's rule as a fake: a configured answer, every call recorded.
struct Rule { answer: Mutex<std::result::Result<Approval, ApprovalRefusal>>, calls: Mutex<Vec<(String, String, Option<String>)>>,
    /// Subjects of every users.db self-approval row requested (S12).
    recorded: Mutex<Vec<String>> }
impl ApprovalRule for Rule {
    fn decide(&self, _: &Path, requester: &str, approver: &str, owner: Option<&str>) -> std::result::Result<Approval, ApprovalRefusal> {
        self.calls.lock().unwrap().push((requester.into(), approver.into(), owner.map(str::to_owned)));
        *self.answer.lock().unwrap()
    }
    fn record_self_approval(&self, _: &Path, _: &str, _: &str, _: Option<&str>, subject: &str) -> std::result::Result<(), ApprovalRefusal> {
        self.recorded.lock().unwrap().push(subject.into());
        Ok(())
    }
}
#[derive(Default)]
struct Notices(Mutex<Vec<(String, Option<String>)>>);
impl RetireNotifier for Notices {
    fn notify(&self, _: &Path, event: &str, owner: Option<&str>, _: &serde_json::Value) {
        self.0.lock().unwrap().push((event.into(), owner.map(str::to_owned)));
    }
}
struct Stores(std::result::Result<u64, ()>);
impl LabelStores for Stores { fn count_under(&self, _: &Path, _: &str) -> std::result::Result<u64, ()> { self.0 } }

struct Fx { dir: tempfile::TempDir, root: Arc<FakeRoot>, rule: Arc<Rule>, notices: Arc<Notices>, now: Arc<AtomicI64> }
impl Fx {
    fn home(&self) -> &Path { self.dir.path() }
    fn env(&self) -> ProposeEnv<'_> {
        ProposeEnv { actor: PROPOSER, root: &*self.root, notifier: &*self.notices, now: self.now.load(Ordering::SeqCst) }
    }
    fn propose(&self) -> std::result::Result<ProposeOutcome, RetireError> { employee_retire_propose(self.home(), EMPLOYEE, &self.env()) }
    fn staged(&self) -> ProposalBinding {
        let ProposeOutcome::Staged { proposal, .. } = self.propose().unwrap() else { panic!("expected a staged proposal") };
        self.items().into_iter().find(|i| i.binding.operation_id == proposal).unwrap().binding
    }
    fn authority(&self, actor: Option<&str>) -> GatewayAuthority {
        let now = self.now.clone();
        let backend = RetireBackend::new(self.home().into(), self.root.clone(), self.rule.clone(), self.notices.clone(),
            actor.map(str::to_owned)).with_clock(Arc::new(move || now.load(Ordering::SeqCst)));
        GatewayAuthority::new(self.home().into()).unwrap().with_backend(Arc::new(EmployeeRetireAuthority::from_backend(backend)))
    }
    fn store(&self) -> ConfirmationStore { ConfirmationStore::open_existing(&self.home().join(CONFIRMATIONS)).unwrap() }
    fn items(&self) -> Vec<ConfirmationItem> { self.store().list().unwrap() }
    fn status(&self, binding: &ProposalBinding) -> ConfirmationStatus {
        self.items().into_iter().find(|i| i.binding == *binding).unwrap().status
    }
    fn retire_items(&self) -> usize { self.items().iter().filter(|i| i.category == ConfirmationCategory::EmployeeRetire).count() }
    fn approve(&self, binding: &ProposalBinding, approver: &str) -> Result<()> {
        self.store().approve(&[*binding], &session(approver), [2; 32], 101, &self.authority(Some(approver)))
    }
    fn apply_all(&self, binding: &ProposalBinding) -> Result<()> {
        let mut authority = self.authority(None);
        let mut store = self.store();
        for _ in 0..3 { store.apply_next(binding, &mut authority)?; }
        Ok(())
    }
    fn retry(&self, binding: &ProposalBinding) -> Result<()> {
        let mut authority = self.authority(None);
        let validator = self.authority(None);
        self.store().retry(binding, &session(APPROVER), [2; 32], 101, &mut authority, &validator)
    }
    fn reject(&self, binding: &ProposalBinding) -> Result<()> {
        self.store().reject(binding, &session(APPROVER), [2; 32], 101, &mut self.authority(None))
    }
    fn root_state(&self) -> (String, Option<String>) {
        Connection::open(self.home().join(APPLY)).unwrap().query_row(
            "SELECT root_state,retire_proposal_id FROM root_allocations WHERE employee_hex=?1", [hex::encode(EMPLOYEE.0)],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    }
    fn sql(&self, statement: &str, values: &[&dyn rusqlite::ToSql]) {
        Connection::open(self.home().join(APPLY)).unwrap().execute(statement, values).unwrap();
    }
    fn quarantine(&self, employee: EmployeeId, incarnation: u32, seal: &str, early: bool) {
        self.sql("INSERT INTO employee_quarantine(employee_hex,incarnation,label,kind,owner_user_id,tenant_id,quarantined_at,digest,seal,
            principal_snapshot,early_purge) VALUES(?1,?2,?3,'personal',?4,'tenant-a',1,?5,?6,'{}',?7)
            ON CONFLICT(employee_hex,incarnation) DO UPDATE SET seal=excluded.seal,early_purge=excluded.early_purge",
            &[&hex::encode(employee.0), &incarnation, &LABEL, &OWNER, &DIGEST, &seal, &early.then_some("early-request-1")]);
    }
    fn confirmed(&self, disabled: bool) {
        let mut receipt = self.root.receipt.clone(); receipt.disabled = disabled;
        self.sql("UPDATE confirmed_employees SET receipt=?1 WHERE label=?2", &[&serde_json::to_string(&receipt).unwrap(), &LABEL]);
    }
    fn audit_log(&self) -> String { std::fs::read_to_string(self.home().join("security_audit.jsonl")).unwrap_or_default() }
    fn noticed(&self, event: &str) -> Vec<Option<String>> {
        self.notices.0.lock().unwrap().iter().filter(|(e, _)| e == event).map(|(_, owner)| owner.clone()).collect()
    }
}
fn session(user: &str) -> ApprovalSession {
    ApprovalSession::from_verified_session(true, true, Some(100), [2; 32], RequestId(*uuid::Uuid::parse_str(user).unwrap().as_bytes()))
}
fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap(); }
    #[cfg(not(unix))]
    let _ = (path, mode);
}
/// A purged Personal employee whose root allocation is still `allocated`.
fn fixture(early: bool) -> Fx {
    let dir = crate::fs_safe::private_test_home().unwrap();
    let home = dir.path();
    #[cfg(unix)]
    { use std::os::unix::fs::DirBuilderExt; std::fs::DirBuilder::new().mode(0o700).create(home.join("migration-authority")).unwrap(); }
    #[cfg(not(unix))]
    std::fs::create_dir(home.join("migration-authority")).unwrap();
    std::fs::File::create(home.join(APPLY)).unwrap();
    let mut store = ConfirmationStore::from_connection(Connection::open(home.join(CONFIRMATIONS)).unwrap()).unwrap();
    for path in [APPLY, CONFIRMATIONS] { set_mode(&home.join(path), 0o600); }
    crate::isolation_quarantine::ensure_employee_schema(&Connection::open(home.join(APPLY)).unwrap()).unwrap();
    let fields = [("employee_id", LABEL), ("disk_bytes", "67108864"), ("binding_kind", "personal"), ("owner_id", OWNER)].iter()
        .map(|(n, v)| ProposalField { name: (*n).into(), value: ProposalValue::Text { value: (*v).into() } }).collect();
    let staged = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
        ConfirmationCategory::Employee, fields, &Accept).unwrap();
    store.approve(&[staged.binding], &session(APPROVER), [2; 32], 101, &Accept).unwrap();
    for _ in 0..3 { store.apply_next(&staged.binding, &mut Accept).unwrap(); }
    let item = store.list().unwrap().into_iter().find(|i| i.binding == staged.binding).unwrap();
    let spec = allocation::specification(&item).unwrap();
    let receipt = Receipt { binding: item.binding, label: LABEL.into(), employee: EMPLOYEE, uid: 70_001, bytes: spec.bytes,
        inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
    let fx = Fx { dir, root: Arc::new(FakeRoot { state: Mutex::new(Spawnd { phase: Some(Phase::Ready), ..Spawnd::default() }), receipt: receipt.clone() }),
        rule: Arc::new(Rule { answer: Mutex::new(Ok(Approval::OtherAdmin)), calls: Mutex::new(Vec::new()), recorded: Mutex::new(Vec::new()) }),
        notices: Arc::new(Notices::default()), now: Arc::new(AtomicI64::new(1_000_000)) };
    let mut dead = receipt.clone(); dead.disabled = true;
    fx.sql("INSERT INTO confirmed_employees(label,operation,digest,receipt,tenant_id,incarnation) VALUES(?1,'op-1',X'01',?2,'tenant-a',0)",
        &[&LABEL, &serde_json::to_string(&dead).unwrap()]);
    fx.sql("INSERT INTO root_allocations(employee_hex,root_item,root_state) VALUES(?1,?2,'allocated')",
        &[&hex::encode(EMPLOYEE.0), &serde_json::json!({"item": item, "receipt": receipt}).to_string()]);
    fx.quarantine(EMPLOYEE, 0, "purged", early);
    fx
}
fn spawnd(fx: &Fx) -> std::sync::MutexGuard<'_, Spawnd> { fx.root.state.lock().unwrap() }

#[test]
fn retire_only_when_every_row_closed() {
    let fx = fixture(false);
    for seal in ["sealed", "pending", "oversize", "needs_reseal", "restoring", "adopting", "purging", "restored", "adopted"] {
        fx.quarantine(EMPLOYEE, 0, seal, false);
        assert_eq!(fx.propose().unwrap_err(), RetireError::RowsOpen, "newest row {seal}");
    }
    // An older row still open (S7) refuses even with the newest purged.
    fx.quarantine(EMPLOYEE, 0, "sealed", false);
    fx.quarantine(EMPLOYEE, 1, "purged", false);
    assert_eq!(fx.propose().unwrap_err(), RetireError::RowsOpen);
    assert_eq!(fx.retire_items(), 0, "a refusal stages nothing");
    assert_eq!(fx.root_state().0, "allocated");
    // restore → tombstone → purge: the restored inc0 row is closed.
    fx.quarantine(EMPLOYEE, 0, "restored", false);
    let binding = fx.staged();
    assert_eq!(fx.root_state(), ("retiring".into(), Some(hex::encode(binding.operation_id.proposal_id.0))));
    let item = fx.items().into_iter().find(|i| i.binding == binding).unwrap();
    let spec = allocation::retire_specification(&item).unwrap();
    assert_eq!((spec.label.as_str(), spec.employee, spec.uid, spec.early, spec.requested_by.as_str()), (LABEL, EMPLOYEE, 70_001, false, PROPOSER));
    assert_eq!(spec.target, fx.root.receipt.binding);
    assert_eq!(spec.quarantine_digest_prefix, "abcdef0123456789");
    // While retiring, a second propose refuses (RETIRE_IN_PROGRESS).
    assert_eq!(fx.propose().unwrap_err(), RetireError::InProgress);
    // Approval re-runs the checks: a row reopened meanwhile refuses the card.
    fx.quarantine(EMPLOYEE, 1, "restoring", false);
    assert!(fx.approve(&binding, APPROVER).is_err());
    assert_eq!(fx.status(&binding), ConfirmationStatus::Pending);
}

#[test]
fn retire_refused_with_open_request() {
    let fx = fixture(false);
    fx.sql("CREATE TABLE employee_restores(request_id TEXT PRIMARY KEY, employee_hex TEXT, label TEXT, state TEXT)", &[]);
    fx.sql("INSERT INTO employee_restores VALUES('r1',?1,?2,'requested')", &[&hex::encode(EMPLOYEE.0), &LABEL]);
    assert_eq!(fx.propose().unwrap_err(), RetireError::OpenRequest);
    assert_eq!(RetireError::OpenRequest.message(), "有還原申請進行中");
    fx.sql("UPDATE employee_restores SET state='denied'", &[]);
    // An open request of another employee id on the same label also blocks.
    fx.sql("INSERT INTO employee_restores VALUES('r2',?1,?2,'applying')", &[&"e2".repeat(16), &LABEL]);
    assert_eq!(fx.propose().unwrap_err(), RetireError::OpenRequest);
    fx.sql("UPDATE employee_restores SET state='done'", &[]);
    // Another row on the label being restored or adopted: RESTORE_IN_PROGRESS.
    fx.quarantine(EmployeeId([0xe2; 16]), 0, "adopting", false);
    assert_eq!(fx.propose().unwrap_err(), RetireError::RestoreInProgress);
    fx.quarantine(EmployeeId([0xe2; 16]), 0, "purged", false);
    let binding = fx.staged();
    // A request filed after staging refuses the approval and the apply.
    fx.sql("INSERT INTO employee_restores VALUES('r3',?1,?2,'requested')", &[&hex::encode(EMPLOYEE.0), &LABEL]);
    assert!(fx.approve(&binding, APPROVER).is_err());
    fx.sql("UPDATE employee_restores SET state='cancelled' WHERE request_id='r3'", &[]);
    fx.approve(&binding, APPROVER).unwrap();
    fx.sql("UPDATE employee_restores SET state='requested' WHERE request_id='r3'", &[]);
    assert!(fx.apply_all(&binding).is_err());
    assert_eq!(spawnd(&fx).retires, 0, "the root is never asked while a request is open");
}

#[test]
fn retire_refused_for_live_old_id() {
    let fx = fixture(false);
    fx.confirmed(false);
    assert_eq!(fx.propose().unwrap_err(), RetireError::LiveOldId);
    fx.confirmed(true);
    let binding = fx.staged();
    // Restored again after staging: the approval and the apply refuse.
    fx.confirmed(false);
    assert!(fx.approve(&binding, APPROVER).is_err());
    fx.confirmed(true);
    fx.approve(&binding, APPROVER).unwrap();
    fx.confirmed(false);
    assert!(fx.apply_all(&binding).is_err());
    assert_eq!(spawnd(&fx).retires, 0);
}

#[test]
fn retire_early_purged_row_needs_second_admin() {
    let fx = fixture(true);
    let ProposeOutcome::Staged { early: true, .. } = fx.propose().unwrap() else { panic!("early-purged row") };
    let binding = fx.items().into_iter().find(|i| i.category == ConfirmationCategory::EmployeeRetire).unwrap().binding;
    // The proposer approving while another qualifying Admin exists: refused.
    *fx.rule.answer.lock().unwrap() = Err(ApprovalRefusal::NeedsOtherAdmin);
    assert!(fx.approve(&binding, PROPOSER).is_err());
    assert_eq!(fx.status(&binding), ConfirmationStatus::Pending);
    assert_eq!(fx.rule.calls.lock().unwrap().last().cloned(), Some((PROPOSER.into(), PROPOSER.into(), Some(OWNER.into()))));
    // Another qualifying Admin approves; apply re-runs the rule with the recorded approver.
    *fx.rule.answer.lock().unwrap() = Ok(Approval::OtherAdmin);
    fx.approve(&binding, APPROVER).unwrap();
    fx.rule.calls.lock().unwrap().clear();
    fx.apply_all(&binding).unwrap();
    assert!(fx.rule.calls.lock().unwrap().iter().all(|(r, a, o)| r == PROPOSER && a == APPROVER && o.as_deref() == Some(OWNER)));
    assert!(!fx.rule.calls.lock().unwrap().is_empty());
    assert_eq!(spawnd(&fx).approvers.first().map(String::as_str), Some(APPROVER), "the approver reaches the root audit");
    assert_eq!(fx.root_state().0, "retired");
    // The rule turning (e.g. cooling) between approval and apply refuses the apply.
    let fx = fixture(true);
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    *fx.rule.answer.lock().unwrap() = Err(ApprovalRefusal::Cooling);
    assert!(fx.apply_all(&binding).is_err());
    assert_eq!(spawnd(&fx).retires, 0);
    // Sole Admin: self-approval allowed by the rule, audited `self_approved`.
    let fx = fixture(true);
    let binding = fx.staged();
    *fx.rule.answer.lock().unwrap() = Ok(Approval::SelfApproved);
    fx.approve(&binding, PROPOSER).unwrap();
    fx.apply_all(&binding).unwrap();
    assert!(fx.audit_log().contains("isolation.employee_retire_self_approved"));
    assert!(fx.audit_log().contains(r#""self_approved":true"#));
    // F6c2's users.db row: requested once, at stage, for this proposal.
    assert_eq!(*fx.rule.recorded.lock().unwrap(), vec![hex::encode(binding.operation_id.proposal_id.0)]);
    // A row purged after 90 days: Admin + step-up only, the rule is never consulted.
    let fx = fixture(false);
    let binding = fx.staged();
    fx.approve(&binding, PROPOSER).unwrap();
    fx.apply_all(&binding).unwrap();
    assert!(fx.rule.calls.lock().unwrap().is_empty());
    assert!(fx.rule.recorded.lock().unwrap().is_empty(), "no self-approval row without the rule");
}

#[test]
fn retire_reject_forward_only_after_retiring() {
    let fx = fixture(false);
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    // The root started (Retiring) and the reply was lost.
    spawnd(&fx).lose_reply_after_start = true;
    assert!(fx.apply_all(&binding).is_err());
    assert_eq!(fx.status(&binding), ConfirmationStatus::Failed);
    assert!(matches!(fx.reject(&binding), Err(ConfirmationError::ForwardOnly)));
    assert_eq!(fx.status(&binding), ConfirmationStatus::Failed, "nothing sticks in Rejecting");
    assert_eq!(fx.root_state().0, "retiring", "the label stays held");
    assert_eq!(RetireError::ForwardOnly.code(), "RETIRE_FORWARD_ONLY");
    // Retry resumes the same operation.
    spawnd(&fx).lose_reply_after_start = false;
    fx.retry(&binding).unwrap();
    fx.apply_all(&binding).unwrap();
    assert_eq!(fx.status(&binding), ConfirmationStatus::Applied);
    assert_eq!(fx.root_state().0, "retired");
    assert_eq!(spawnd(&fx).retires, 1);
}

#[test]
fn retire_compensation_ok_while_ready() {
    let fx = fixture(false);
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    spawnd(&fx).busy = true;
    assert!(fx.apply_all(&binding).is_err());
    // A failed step with the root still Ready compensates Ok and keeps the proposal resumable.
    assert_eq!(fx.status(&binding), ConfirmationStatus::Failed);
    assert_eq!(fx.root_state().0, "retiring");
    // Reject: the read-back says Ready, so Rejected and the allocation is back to `allocated`.
    fx.reject(&binding).unwrap();
    assert_eq!(fx.status(&binding), ConfirmationStatus::Rejected);
    assert_eq!(fx.root_state(), ("allocated".into(), None));
    assert!(fx.audit_log().contains("isolation.employee_retire_rejected"));
    // A new propose is possible again.
    spawnd(&fx).busy = false;
    let again = fx.staged();
    assert_ne!(again, binding);
}

#[test]
fn retire_retired_row_reads_tombstoned_without_root() {
    struct NeverAsked;
    impl crate::isolation_lifecycle::RootAllocation for NeverAsked {
        fn read_back(&self, _: &ConfirmationItem) -> std::result::Result<Receipt, crate::isolation_lifecycle::LifecycleError> {
            panic!("a retired row must not ask the root")
        }
    }
    let fx = fixture(false);
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    fx.apply_all(&binding).unwrap();
    assert_eq!(crate::isolation_lifecycle::read_lifecycle_with(fx.home(), LABEL, &NeverAsked),
        Err(crate::isolation_lifecycle::LifecycleError::Tombstoned));
    let readbacks = spawnd(&fx).readbacks;
    assert_eq!(root_precheck(fx.home(), EMPLOYEE, &*fx.root).unwrap(), RootCheck { class: RootClass::Retired, idle: None });
    assert_eq!(spawnd(&fx).readbacks, readbacks, "ROOT_RETIRED without asking the root");
    assert_eq!(RootClass::Retired.code(), "ROOT_RETIRED");
    // The label's row now holds the Retired receipt of the old id.
    let stored: Receipt = serde_json::from_str(&Connection::open(fx.home().join(APPLY)).unwrap()
        .query_row("SELECT receipt FROM confirmed_employees WHERE label=?1", [LABEL], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    assert!(stored.disabled && stored.employee == EMPLOYEE);
    // Publishing again after Retired is an idempotent replay.
    let item = fx.items().into_iter().find(|i| i.binding == binding).unwrap();
    let mut applying = item.clone(); applying.status = ConfirmationStatus::Applying;
    fx.authority(None).publish(&applying).unwrap();
    assert_eq!(spawnd(&fx).retires, 1);
}

#[test]
fn retire_not_found_records_root_absent() {
    // A transport error is never an absence.
    let fx = fixture(false);
    spawnd(&fx).unavailable = true;
    assert_eq!(fx.propose().unwrap_err(), RetireError::RootUnavailable);
    assert_eq!(fx.root_state().0, "allocated");
    // Only an authoritative NotFound records root_absent, without a proposal.
    spawnd(&fx).unavailable = false;
    spawnd(&fx).phase = None;
    assert_eq!(fx.propose().unwrap(), ProposeOutcome::RootAbsent);
    assert_eq!(fx.root_state(), ("root_absent".into(), None));
    assert_eq!(fx.retire_items(), 0);
    assert!(fx.audit_log().contains("isolation.employee_root_absent"));
    assert_eq!(fx.noticed("isolation.employee_root_absent").len(), 1, "operator alerted");
    assert_eq!(fx.propose().unwrap_err(), RetireError::RootAbsent);
    let readbacks = spawnd(&fx).readbacks;
    assert_eq!(root_precheck(fx.home(), EMPLOYEE, &*fx.root).unwrap().class, RootClass::Gone);
    assert_eq!(spawnd(&fx).readbacks, readbacks);
}

#[test]
fn retire_inconsistent_keeps_label_blocked() {
    let fx = fixture(false);
    spawnd(&fx).inconsistent = true;
    assert_eq!(fx.propose().unwrap_err(), RetireError::RootInconsistent);
    assert_eq!(fx.root_state().0, "allocated", "Inconsistent stays allocated");
    assert!(fx.audit_log().contains("isolation.employee_root_inconsistent"));
    assert!(!label_free(fx.home(), LABEL, &Stores(Ok(0))).unwrap(), "the label stays blocked");
    assert_eq!(root_precheck(fx.home(), EMPLOYEE, &*fx.root).unwrap().class, RootClass::Inconsistent);
    // Any ReadBack mismatch is Inconsistent (S4), never Gone; unavailable is never Gone (S5d).
    let expected = fx.root.receipt.clone();
    for change in 0..4 {
        let mut other = expected.clone();
        match change { 0 => other.uid += 1, 1 => other.label = "other".into(), 2 => other.bytes += 1, _ => other.principal_binding = None }
        let answer = Ok(Answer::Reply(Reply { receipt: other, phase: Phase::Ready, idle: Some(true) }));
        assert_eq!(classify_readback(&expected, answer).0, RootClass::Inconsistent, "change {change}");
    }
    for refused in [Root::NotReady, Root::IsolationUnavailable] {
        assert_eq!(classify_readback(&expected, Ok(Answer::Refused(refused))).0, RootClass::Unavailable);
    }
    assert_eq!(classify_readback(&expected, Err(())).0, RootClass::Unavailable);
    assert_eq!(classify_readback(&expected, Ok(Answer::Refused(Root::NotFound))).0, RootClass::Gone);
    // An inconsistent root during apply alerts and keeps the label too.
    spawnd(&fx).inconsistent = false;
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    spawnd(&fx).inconsistent = true;
    assert!(fx.apply_all(&binding).is_err());
    assert_eq!(fx.root_state().0, "retiring");
    assert!(!label_free(fx.home(), LABEL, &Stores(Ok(0))).unwrap());
}

#[test]
fn retire_host_busy_24h_alerts_operator() {
    let fx = fixture(false);
    let binding = fx.staged();
    fx.approve(&binding, APPROVER).unwrap();
    spawnd(&fx).busy = true;
    let alerts = |fx: &Fx| fx.audit_log().matches("isolation.employee_retire_host_busy").count();
    let start = fx.now.load(Ordering::SeqCst);
    for (offset, expected) in [(0, 0), (23 * 3600, 0), (HOST_BUSY_ALERT_SECS, 1), (HOST_BUSY_ALERT_SECS + 3600, 1)] {
        fx.now.store(start + offset, Ordering::SeqCst);
        if fx.status(&binding) == ConfirmationStatus::Failed { fx.retry(&binding).unwrap(); }
        assert!(fx.apply_all(&binding).is_err(), "busy host at +{offset}s");
        assert_eq!(alerts(&fx), expected, "alerts at +{offset}s");
        assert_eq!(fx.root_state().0, "retiring", "keeps retrying, label held");
    }
    assert_eq!(fx.noticed("isolation.employee_retire_host_busy").len(), 1, "the operator is alerted once");
    assert_eq!(RetireError::HostBusy.message(), "主機正在執行其他工作，稍後會自動重試");
    spawnd(&fx).busy = false;
    fx.retry(&binding).unwrap();
    fx.apply_all(&binding).unwrap();
    let waits: i64 = Connection::open(fx.home().join(APPLY)).unwrap()
        .query_row("SELECT COUNT(*) FROM employee_retire_waits", [], |r| r.get(0)).unwrap();
    assert_eq!(waits, 0, "the wait record goes with the retire");
}

#[test]
fn retire_label_free_only_after_all_r7_conditions() {
    let fx = fixture(false);
    let zero = Stores(Ok(0));
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "root still allocated");
    let binding = fx.staged();
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "retiring, open retire proposal");
    fx.approve(&binding, APPROVER).unwrap();
    fx.apply_all(&binding).unwrap();
    assert!(label_free(fx.home(), LABEL, &zero).unwrap());
    assert!(!label_free(fx.home(), LABEL, &Stores(Ok(3))).unwrap(), "rows still under the label (S10)");
    assert_eq!(label_free(fx.home(), LABEL, &Stores(Err(()))).unwrap_err(), RetireError::Unavailable, "unreadable is not zero");
    fx.quarantine(EmployeeId([0xe3; 16]), 0, "sealed", false);
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "another row on the label is not closed");
    fx.quarantine(EmployeeId([0xe3; 16]), 0, "purged", false);
    fx.sql("INSERT INTO root_allocations(employee_hex,root_item,root_state) SELECT ?1,root_item,'allocated' FROM root_allocations WHERE employee_hex=?2",
        &[&"e3".repeat(16), &hex::encode(EMPLOYEE.0)]);
    // A stored root_item names its own employee; a copied one is corrupt and refuses.
    assert_eq!(label_free(fx.home(), LABEL, &zero).unwrap_err(), RetireError::Unavailable);
    fx.sql("DELETE FROM root_allocations WHERE employee_hex=?1", &[&"e3".repeat(16)]);
    assert!(label_free(fx.home(), LABEL, &zero).unwrap());
    fx.confirmed(false);
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "a live row");
    fx.confirmed(true);
    fx.sql("CREATE TABLE employee_restores(request_id TEXT PRIMARY KEY, employee_hex TEXT, label TEXT, state TEXT)", &[]);
    fx.sql("INSERT INTO employee_restores VALUES('r1',?1,?2,'requested')", &[&"e3".repeat(16), &LABEL]);
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "an open request");
    fx.sql("DELETE FROM employee_restores", &[]);
    assert!(label_free(fx.home(), LABEL, &zero).unwrap());
    let fields = [("employee_id", LABEL), ("disk_bytes", "67108864"), ("binding_kind", "shared"), ("tenant_id", "tenant-a")].iter()
        .map(|(n, v)| ProposalField { name: (*n).into(), value: ProposalValue::Text { value: (*v).into() } }).collect();
    fx.store().stage_proposal(ProposalOperationId { proposal_id: RequestId([5; 16]), revision: 1 }, ConfirmationCategory::Employee, fields, &Accept).unwrap();
    assert!(!label_free(fx.home(), LABEL, &zero).unwrap(), "an open proposal");
}

#[test]
fn retire_audited_and_notified() {
    let fx = fixture(false);
    // The purge hook stages only when the box is ticked.
    assert_eq!(purge_hook(fx.home(), EMPLOYEE, false, &fx.env()).unwrap(), None);
    assert_eq!(fx.retire_items(), 0);
    let Some(ProposeOutcome::Staged { proposal, early: false }) = purge_hook(fx.home(), EMPLOYEE, true, &fx.env()).unwrap() else { panic!() };
    let binding = fx.items().into_iter().find(|i| i.binding.operation_id == proposal).unwrap().binding;
    let log = fx.audit_log();
    assert!(log.contains("isolation.employee_retire_proposed") && log.contains(PROPOSER) && log.contains("abcdef0123456789"));
    assert_eq!(fx.noticed("isolation.employee_retire_proposed"), vec![Some(OWNER.to_owned())], "owner and Admins are notified");
    fx.approve(&binding, APPROVER).unwrap();
    fx.apply_all(&binding).unwrap();
    let log = fx.audit_log();
    for event in ["isolation.employee_retire_approved", "isolation.employee_retired"] {
        assert!(log.contains(event), "{event}");
        assert_eq!(fx.noticed(event), vec![Some(OWNER.to_owned())], "{event}");
    }
    assert!(log.contains(&format!(r#""approved_by":"{APPROVER}""#)) && log.contains(&format!(r#""requested_by":"{PROPOSER}""#)));
    assert!(log.contains(r#""self_approved":false"#));
    assert!(log.contains(&hex::encode(EMPLOYEE.0)));
    assert!(!log.contains("isolation.employee_retire_self_approved"));
}
