//! F6l (F6 design r5 §3.12, R14): an `EmployeeRetire` item is forward only.
use super::*;
use crate::allocation::{retire_fields, retire_specification, RetireSpec};
use duduclaw_isolation::ids::EmployeeId;

struct Valid;
impl ProposalValidator for Valid {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}
/// The gateway retire backend as the store sees it: `disable_and_read_back`
/// succeeds only while the root entry is still Ready.
#[derive(Default)]
struct Retire { root_started: bool, fail_stage: bool, disables: usize, stages: usize }
impl ProposalApplier for Retire {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<()> {
        self.stages += 1;
        self.root_started = true;
        if self.fail_stage { Err(ConfirmationError::Apply) } else { Ok(()) }
    }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> {
        self.disables += 1;
        if self.root_started { Err(ConfirmationError::Compensation) } else { Ok(()) }
    }
}
fn session() -> ApprovalSession { ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16])) }
fn spec() -> RetireSpec {
    RetireSpec { label: "sales".into(), target: ProposalBinding { operation_id: ProposalOperationId { proposal_id: RequestId([1; 16]), revision: 1 },
        proposal_digest: Sha256Digest([2; 32]) }, employee: EmployeeId([3; 16]), uid: 70_001, early: false,
        quarantine_digest_prefix: "00112233445566aa".into(), requested_by: "admin-a".into() }
}
fn staged(store: &mut ConfirmationStore, id: u8) -> ConfirmationItem {
    let operation = ProposalOperationId { proposal_id: RequestId([id; 16]), revision: 1 };
    let item = store.stage_proposal(operation, ConfirmationCategory::EmployeeRetire, retire_fields(&spec()), &Valid).unwrap();
    assert_eq!(retire_specification(&item).unwrap(), spec(), "staged fields parse back exactly");
    item
}
fn status(store: &ConfirmationStore, binding: &ProposalBinding) -> ConfirmationStatus {
    store.list().unwrap().into_iter().find(|i| i.binding == *binding).unwrap().status
}

#[test]
fn confirmation_retire_reject_forward_only_after_root_started() {
    let mut store = ConfirmationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();
    let item = staged(&mut store, 4);
    store.approve(&[item.binding], &session(), [7; 32], 101, &Valid).unwrap();
    assert_eq!(store.approver(&item.binding).unwrap(), Some(RequestId([8; 16])), "approval record names the approver");
    let mut root = Retire { fail_stage: true, ..Retire::default() };
    assert!(matches!(store.apply_next(&item.binding, &mut root), Err(ConfirmationError::Compensation)));
    assert_eq!(status(&store, &item.binding), ConfirmationStatus::Failed);
    // Reject refuses and nothing sticks in Rejecting.
    assert!(matches!(store.reject(&item.binding, &session(), [7; 32], 101, &mut root), Err(ConfirmationError::ForwardOnly)));
    assert_eq!(status(&store, &item.binding), ConfirmationStatus::Failed);
    // Retry resumes the same operation without depending on compensation.
    let disables = root.disables;
    store.retry(&item.binding, &session(), [7; 32], 101, &mut root, &Valid).unwrap();
    assert_eq!(root.disables, disables, "retry of a retire never calls the compensation read-back");
    assert_eq!(status(&store, &item.binding), ConfirmationStatus::Applying);
    root.fail_stage = false;
    for _ in 0..3 { store.apply_next(&item.binding, &mut root).unwrap(); }
    assert_eq!(status(&store, &item.binding), ConfirmationStatus::Applied);
    assert_eq!(root.stages, 2, "the second stage is the same retire operation, replayed");
}

#[test]
fn confirmation_retire_reject_allowed_while_root_ready() {
    let mut store = ConfirmationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();
    // Pending: rejected without touching the root.
    let pending = staged(&mut store, 5);
    let mut root = Retire::default();
    store.reject(&pending.binding, &session(), [7; 32], 101, &mut root).unwrap();
    assert_eq!((status(&store, &pending.binding), root.disables), (ConfirmationStatus::Rejected, 0));
    // Approved, root never started (read-back says Ready): Rejected.
    let approved = staged(&mut store, 6);
    store.approve(&[approved.binding], &session(), [7; 32], 101, &Valid).unwrap();
    store.reject(&approved.binding, &session(), [7; 32], 101, &mut root).unwrap();
    assert_eq!(status(&store, &approved.binding), ConfirmationStatus::Rejected);
    // A non-retire item keeps the old behaviour (stays Rejecting on a failed compensation).
    let operation = ProposalOperationId { proposal_id: RequestId([7; 16]), revision: 1 };
    let other = store.stage_proposal(operation, ConfirmationCategory::Department,
        vec![ProposalField { name: "name".into(), value: ProposalValue::Text { value: "sales".into() } }], &Valid).unwrap();
    store.approve(&[other.binding], &session(), [7; 32], 101, &Valid).unwrap();
    root.root_started = true;
    assert!(matches!(store.reject(&other.binding, &session(), [7; 32], 101, &mut root), Err(ConfirmationError::Compensation)));
    assert_eq!(status(&store, &other.binding), ConfirmationStatus::Rejecting);
}
