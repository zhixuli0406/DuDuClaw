//! R7d: a pending employee allocation through the real apply path
//! (`ConfirmationStore::apply_next` → `GatewayAuthority::stage` → the bounded
//! wait → `employee::allocate_with` → `check_response`), against a scripted
//! root that answers like spawnd (`Response::for_request`).
use super::*;
use super::employee::{allocate_with, AllocationRoot};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_migration::allocation::{self, Action, ErrorCode as Code, Phase, Receipt, Reply, Request, Response, SCHEMA_V3};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationStore};
use duduclaw_migration::ids::RequestId;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
enum Script {
    /// A Ready receipt matching the request.
    Ready,
    /// A refusal with this kind.
    Refuse(Code),
    /// A well-formed "pending" reply of an older schema.
    Older(u16),
    /// Transport failure / undecodable reply (an unknown kind included).
    Down,
}
#[derive(Default)]
struct Root { script: Mutex<VecDeque<Script>>, seen: Mutex<Vec<Request>>, cancel_on: Mutex<Option<(usize, Arc<AtomicBool>)>> }
impl Root {
    fn push(&self, steps: &[Script]) { self.script.lock().unwrap().extend(steps.iter().copied()); }
    fn calls(&self) -> usize { self.seen.lock().unwrap().len() }
}
impl AllocationRoot for Root {
    fn round_trip(&self, request: &Request) -> std::result::Result<Response, ()> {
        let calls = { let mut seen = self.seen.lock().unwrap(); seen.push(request.clone()); seen.len() };
        if let Some((at, flag)) = &*self.cancel_on.lock().unwrap() {
            if calls == *at { flag.store(true, Ordering::SeqCst); }
        }
        // Unscripted: the probe is still running.
        match self.script.lock().unwrap().pop_front().unwrap_or(Script::Refuse(Code::Pending)) {
            Script::Down => Err(()),
            Script::Refuse(code) => Ok(Response::for_request(request, Err(code))),
            Script::Older(schema) => {
                let mut older = request.clone(); older.schema = schema;
                Ok(Response::for_request(&older, Err(Code::Pending)))
            }
            Script::Ready => {
                let spec = allocation::specification(&request.item).unwrap();
                let receipt = Receipt { binding: request.item.binding, label: spec.label, employee: EmployeeId([7; 16]), uid: 70001,
                    bytes: spec.bytes, inodes: spec.inodes, disabled: false, principal_binding: spec.binding };
                Ok(Response::for_request(request, Ok(Reply { receipt, phase: Phase::Ready, idle: None })))
            }
        }
    }
}
/// The production Employee stage minus product validation and bookkeeping.
struct Backend { root: Arc<Root>, disables: AtomicUsize }
impl MigrationAuthorityBackend for Backend {
    fn supports(&self, category: ConfirmationCategory) -> bool { category == ConfirmationCategory::Employee }
    fn validate(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn stage(&self, item: &ConfirmationItem) -> Result<()> { allocate_with(self.root.as_ref(), item).map(|_| ()) }
    fn publish(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn read_back(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn disable(&self, _: &ConfirmationItem) -> Result<()> { self.disables.fetch_add(1, Ordering::SeqCst); Ok(()) }
}
struct Valid;
impl ProposalValidator for Valid {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}
struct Fx { _home: tempfile::TempDir, store: ConfirmationStore, authority: GatewayAuthority, root: Arc<Root>, backend: Arc<Backend>,
    item: ConfirmationItem, cancel: Arc<AtomicBool> }
fn ms(value: u64) -> Duration { Duration::from_millis(value) }
fn fx(first: u64, every: u64, total: u64) -> Fx {
    let home = tempfile::tempdir().unwrap();
    let root = Arc::new(Root::default());
    let backend = Arc::new(Backend { root: root.clone(), disables: AtomicUsize::new(0) });
    let cancel = Arc::new(AtomicBool::new(false));
    let authority = GatewayAuthority::new(home.path().into()).unwrap().with_backend(backend.clone())
        .with_pending_wait(bridge::PendingWait::for_test(ms(first), ms(every), ms(total), cancel.clone()));
    let mut store = ConfirmationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap();
    let field = |name: &str, value: &str| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } };
    let item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([5; 16]), revision: 1 }, ConfirmationCategory::Employee,
        vec![field("disk_bytes", "67108864"), field("employee_id", "sales")], &Valid).unwrap();
    let session = ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16]));
    store.approve(&[item.binding], &session, [7; 32], 101, &Valid).unwrap();
    Fx { _home: home, store, authority, root, backend, item, cancel }
}
impl Fx {
    fn apply(&mut self) -> std::result::Result<ConfirmationStatus, Error> {
        self.store.apply_next(&self.item.binding, &mut self.authority).map(|item| item.status)
    }
    fn status(&self) -> ConfirmationStatus {
        self.store.list().unwrap().into_iter().find(|item| item.binding == self.item.binding).unwrap().status
    }
    fn disables(&self) -> usize { self.backend.disables.load(Ordering::SeqCst) }
    /// Publish + read back: the apply completes.
    fn finish(&mut self) {
        for _ in 0..2 { self.apply().unwrap(); }
        assert_eq!(self.status(), ConfirmationStatus::Applied);
    }
    /// Every request is the same schema-3 Allocate of the same item (only
    /// the nonce differs).
    fn assert_identical_allocates(&self) {
        let seen = self.root.seen.lock().unwrap();
        let fields = serde_json::to_value(&self.item.fields).unwrap();
        for (index, request) in seen.iter().enumerate() {
            assert!(request.schema == SCHEMA_V3 && request.action == Action::Allocate && request.approver.is_none(), "{index}");
            assert!(request.item.binding == self.item.binding && serde_json::to_value(&request.item.fields).unwrap() == fields, "{index}");
            assert_eq!(request.item.status, ConfirmationStatus::Applying);
            assert!(seen.iter().take(index).all(|earlier| earlier.nonce != request.nonce), "fresh nonce per attempt");
        }
    }
}

#[test]
fn allocation_pending_then_ready_succeeds_without_compensation() {
    let mut fx = fx(5, 5, 5_000);
    fx.root.push(&[Script::Refuse(Code::Pending), Script::Refuse(Code::Pending), Script::Ready]);
    assert_eq!(fx.apply().unwrap(), ConfirmationStatus::Applying, "one stage step, resolved inside the wait");
    assert_eq!((fx.root.calls(), fx.disables()), (3, 0));
    fx.assert_identical_allocates();
    fx.finish();
    assert_eq!((fx.root.calls(), fx.disables()), (3, 0));
}

#[test]
fn allocation_pending_limit_then_ready_succeeds() {
    let mut fx = fx(5, 5, 5_000);
    fx.root.push(&[Script::Refuse(Code::PendingLimit), Script::Refuse(Code::Pending), Script::Ready]);
    fx.apply().unwrap();
    assert_eq!((fx.root.calls(), fx.disables()), (3, 0));
    fx.assert_identical_allocates();
    fx.finish();
}

/// Pending for the whole window: the distinct retry-safe outcome (Applying,
/// `Err(Pending)`, no Disable); staging the same employee again resumes it.
#[test]
fn allocation_pending_for_the_whole_window_is_retry_safe_and_resumes() {
    let mut fx = fx(5, 5, 100);
    let started = Instant::now();
    assert!(matches!(fx.apply(), Err(Error::Pending)));
    assert!(started.elapsed() < Duration::from_secs(5));
    let calls = fx.root.calls();
    // Attempts at 0, 5, 10, … while the next one still starts within 100 ms.
    assert!((2..=21).contains(&calls), "bounded schedule: {calls}");
    assert_eq!((fx.status(), fx.disables()), (ConfirmationStatus::Applying, 0));
    // Operator retry, still pending: same outcome, still nothing compensated.
    assert!(matches!(fx.apply(), Err(Error::Pending)));
    assert_eq!((fx.status(), fx.disables()), (ConfirmationStatus::Applying, 0));
    // Within spawnd's window the probe settles: the same Allocate resumes.
    let before = fx.root.calls();
    fx.root.push(&[Script::Ready]);
    fx.apply().unwrap();
    assert_eq!((fx.root.calls(), fx.disables()), (before + 1, 0));
    fx.assert_identical_allocates();
    fx.finish();
}

/// After spawnd retired the reservation it answers its tombstone refusal
/// (`IsolationUnavailable`): the existing failure, never Pending.
#[test]
fn allocation_after_spawnd_window_surfaces_existing_failure() {
    let mut fx = fx(5, 5, 30);
    assert!(matches!(fx.apply(), Err(Error::Pending)));
    fx.root.push(&[Script::Refuse(Code::IsolationUnavailable)]);
    assert!(matches!(fx.apply(), Err(Error::Apply)));
    assert_eq!((fx.status(), fx.disables()), (ConfirmationStatus::Failed, 1));
}

/// Hard failures keep the existing behaviour: one call, Failed, compensated.
/// An older-schema reply and a transport/decode failure (which is what an
/// unknown kind is) are refused the same way, never read as Pending.
#[test]
fn allocation_hard_failure_old_schema_and_garbled_reply_are_refused() {
    for step in [Script::Refuse(Code::IsolationUnavailable), Script::Refuse(Code::NotReady), Script::Refuse(Code::InvalidRequest),
        Script::Refuse(Code::Inconsistent), Script::Older(1), Script::Older(allocation::SCHEMA_V2), Script::Down] {
        let mut fx = fx(5, 5, 5_000);
        fx.root.push(&[step]);
        assert!(matches!(fx.apply(), Err(Error::Apply)), "{step:?}");
        assert_eq!((fx.root.calls(), fx.status(), fx.disables()), (1, ConfirmationStatus::Failed, 1), "{step:?}");
    }
}

/// Shutdown during the wait stops it promptly: Applying, no Disable; the
/// next apply (after restart) resumes the same allocation.
#[test]
fn allocation_shutdown_during_wait_stops_without_disable() {
    let mut fx = fx(20, 20, 30_000);
    *fx.root.cancel_on.lock().unwrap() = Some((2, fx.cancel.clone()));
    let started = Instant::now();
    assert!(matches!(fx.apply(), Err(Error::Pending)));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!((fx.root.calls(), fx.status(), fx.disables()), (2, ConfirmationStatus::Applying, 0));
    fx.root.push(&[Script::Ready]);
    fx.apply().unwrap();
    assert_eq!((fx.root.calls(), fx.disables()), (3, 0));
    fx.finish();
}

#[test]
fn allocation_pending_wait_production_schedule() {
    assert_eq!((bridge::PENDING_FIRST_RETRY, bridge::PENDING_RETRY_EVERY, bridge::PENDING_WAIT_TOTAL),
        (Duration::from_secs(3), Duration::from_secs(5), Duration::from_secs(120)));
}
