//! R8d D: the employee stage's Entitle (after Allocate, inside the effect
//! gate, verified) and the start-up reconcile, against a scripted root that
//! answers like spawnd (`Response::for_request`) and a planner over the
//! fixture signed runtimes (`isolation_entitlement::tests::host`).
use super::*;
use super::super::ShutdownGate;
use super::super::employee::allocate_entitled;
use crate::isolation_entitlement::{Desired, DesiredError};
use duduclaw_migration::allocation::{self, Action, Entitlement, ErrorCode as Code, Phase, Receipt, Reply, Request, Response};
use duduclaw_migration::confirmation::{ConfirmationError, ConfirmationStore, ProposalValidator};
use duduclaw_migration::contract::{ConfirmationCategory, ConfirmationStatus, ProposalField, ProposalOperationId, ProposalValue};
use duduclaw_migration::ids::RequestId;
use std::sync::Mutex;

#[derive(Clone)]
enum Script { Echo, Other(Entitlement), Refuse(Code), Down }
struct Root { seen: Mutex<Vec<Request>>, entitle: Mutex<Script> }
impl Root {
    fn new(entitle: Script) -> Self { Self { seen: Mutex::new(vec![]), entitle: Mutex::new(entitle) } }
    fn actions(&self) -> Vec<(u16, Action)> { self.seen.lock().unwrap().iter().map(|request| (request.schema, request.action)).collect() }
}
impl AllocationRoot for Root {
    fn round_trip(&self, request: &Request) -> Result<Response, ()> {
        self.seen.lock().unwrap().push(request.clone());
        let spec = allocation::specification(&request.item).unwrap();
        let receipt = Receipt { binding: request.item.binding, label: spec.label, employee: EmployeeId([7; 16]), uid: 70001,
            bytes: spec.bytes, inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
        let ready = |entitlement| Ok(Reply { receipt: receipt.clone(), phase: Phase::Ready, idle: None, entitlement });
        match (request.action, self.entitle.lock().unwrap().clone()) {
            (Action::Allocate, _) => Ok(Response::for_request(request, ready(None))),
            (Action::Entitle, Script::Echo) => Ok(Response::for_request(request, ready(request.entitlement.clone()))),
            (Action::Entitle, Script::Other(other)) => Ok(Response::for_request(request, ready(Some(other)))),
            (Action::Entitle, Script::Refuse(code)) => Ok(Response::for_request(request, Err(code))),
            _ => Err(()),
        }
    }
}
/// The real derivation over the fixture host (Claude, Codex, Qwen + helpers).
struct Fixture;
impl EntitlementPlanner for Fixture {
    fn desired(&self, settings: &serde_json::Value) -> Result<Desired, DesiredError> {
        crate::isolation_entitlement::desired(&crate::isolation_entitlement::tests::host(), settings)
    }
}
struct Valid;
impl ProposalValidator for Valid {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<(), ConfirmationError> { Ok(()) }
}
fn item(id: u8, label: &str, provider: &str, status: ConfirmationStatus) -> ConfirmationItem {
    let mut store = ConfirmationStore::from_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let field = |name: &str, value: &str| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } };
    let settings = serde_json::json!({ "runtime": { "provider": provider } }).to_string();
    let mut item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([id; 16]), revision: 1 }, ConfirmationCategory::Employee,
        vec![field("disk_bytes", "67108864"), field("employee_id", label), field("settings", &settings)], &Valid).unwrap();
    item.status = status;
    item
}
fn claude_set() -> Entitlement {
    Fixture.desired(&serde_json::json!({ "runtime": { "provider": "claude" } })).unwrap().entitlement
}

#[test]
fn entitle_follows_the_ready_allocation_and_must_match() {
    let root = Root::new(Script::Echo);
    let gate = ShutdownGate::new();
    let applying = item(1, "r8d-sales", "claude", ConfirmationStatus::Applying);
    let receipt = allocate_entitled(&root, &Fixture, &gate, &applying).unwrap();
    assert_eq!(receipt.employee, EmployeeId([7; 16]));
    assert_eq!(root.actions(), vec![(allocation::SCHEMA_V3, Action::Allocate), (allocation::SCHEMA_V4, Action::Entitle)]);
    assert_eq!(root.seen.lock().unwrap()[1].entitlement, Some(claude_set()));
    assert_eq!(gate.running(), 0, "the Entitle permit was released");
    // Root answers another set than the one asked for: a failure.
    let root = Root::new(Script::Other(Entitlement { profiles: claude_set().profiles[..1].to_vec(), max_turns: None }));
    assert!(matches!(allocate_entitled(&root, &Fixture, &gate, &applying), Err(Error::Apply)));
    // Hard refusals fail; a transient one is retry-safe (the allocation stays, nothing compensates).
    for (code, pending) in [(Code::NotSigned, false), (Code::Capacity, false), (Code::InvalidEntitlement, false),
        (Code::NotAllocated, false), (Code::IsolationUnavailable, true), (Code::NotReady, true)] {
        let root = Root::new(Script::Refuse(code));
        let result = allocate_entitled(&root, &Fixture, &gate, &applying);
        assert_eq!(matches!(result, Err(Error::Pending)), pending, "{code:?}");
        assert!(result.is_err(), "{code:?}");
    }
    let root = Root::new(Script::Down);
    assert!(matches!(allocate_entitled(&root, &Fixture, &gate, &applying), Err(Error::Pending)), "transport failure after Allocate");
}

#[test]
fn primary_runtime_without_signed_profile_fails_before_any_allocation() {
    let root = Root::new(Script::Echo);
    let gate = ShutdownGate::new();
    let grok = item(2, "r8d-grok", "grok", ConfirmationStatus::Applying);
    assert!(matches!(allocate_entitled(&root, &Fixture, &gate, &grok), Err(Error::Denied)));
    assert!(root.actions().is_empty(), "no Allocate was attempted");
    assert_eq!(crate::isolation_entitlement::RUNTIME_NOT_IN_ISOLATION, "runtime_not_available_in_isolation");
    // An unreadable projection is retry-safe, also before any allocation.
    struct Down;
    impl EntitlementPlanner for Down {
        fn desired(&self, _: &serde_json::Value) -> Result<Desired, DesiredError> { Err(DesiredError::Unavailable) }
    }
    assert!(matches!(allocate_entitled(&root, &Down, &gate, &grok), Err(Error::Pending)));
    assert!(root.actions().is_empty());
}

#[test]
fn closed_shutdown_gate_sends_no_entitle() {
    let root = Root::new(Script::Echo);
    let gate = ShutdownGate::new();
    gate.close();
    let applied = item(3, "r8d-closed", "claude", ConfirmationStatus::Applied);
    assert!(matches!(entitle_with(&root, &gate, &applied, &claude_set()), Err(Error::Pending)));
    assert!(root.actions().is_empty(), "nothing is sent once the gate is closed");
    // Inside a stage whose gate closed after Allocate: no Entitle, retry-safe.
    let applying = item(3, "r8d-closed", "claude", ConfirmationStatus::Applying);
    assert!(matches!(allocate_entitled(&root, &Fixture, &gate, &applying), Err(Error::Pending)));
    assert_eq!(root.actions(), vec![(allocation::SCHEMA_V3, Action::Allocate)]);
}

#[test]
fn startup_reconcile_entitles_each_live_confirmed_employee_once() {
    let root = Root::new(Script::Echo);
    let gate = ShutdownGate::new();
    let candidates = vec![
        Candidate { label: "r8d-a".into(), employee: EmployeeId([1; 16]), live: true,
            item: Ok(item(4, "r8d-a", "claude", ConfirmationStatus::Applied)) },
        Candidate { label: "r8d-b".into(), employee: EmployeeId([2; 16]), live: true,
            item: Ok(item(5, "r8d-b", "codex", ConfirmationStatus::Applied)) },
        // Retired / tombstoned (receipt disabled): never entitled.
        Candidate { label: "r8d-retired".into(), employee: EmployeeId([3; 16]), live: false,
            item: Ok(item(6, "r8d-retired", "claude", ConfirmationStatus::Applied)) },
    ];
    assert!(!reconcile_pass(&root, &Fixture, &gate, &candidates), "nothing left to retry");
    let seen = root.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|request| request.schema == allocation::SCHEMA_V4 && request.action == Action::Entitle));
    let labels: Vec<String> = seen.iter().map(|request| allocation::specification(&request.item).unwrap().label).collect();
    assert_eq!(labels, ["r8d-a", "r8d-b"]);
    // A transient answer asks for another pass; a closed gate ends it with nothing sent.
    let root = Root::new(Script::Refuse(Code::IsolationUnavailable));
    assert!(reconcile_pass(&root, &Fixture, &gate, &candidates));
    gate.close();
    let root = Root::new(Script::Echo);
    assert!(!reconcile_pass(&root, &Fixture, &gate, &candidates));
    assert!(root.actions().is_empty());
}
