use super::*;
use std::collections::BTreeSet;

struct Valid;
impl ProposalValidator for Valid {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}
struct Invalid;
impl ProposalValidator for Invalid {
    fn validate_fresh(&self, _: &ConfirmationItem) -> Result<()> { Err(ConfirmationError::Denied) }
}
fn session() -> ApprovalSession {
    ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16]))
}
fn store() -> ConfirmationStore { ConfirmationStore::from_connection(Connection::open_in_memory().unwrap()).unwrap() }
fn operation(id: u16) -> ProposalOperationId {
    let mut bytes = [0; 16]; bytes[0..2].copy_from_slice(&id.to_be_bytes());
    ProposalOperationId { proposal_id: RequestId(bytes), revision: 1 }
}
fn text(name: &str, value: &str) -> ProposalField {
    ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }
}
fn stage(store: &mut ConfirmationStore, id: u16, category: ConfirmationCategory) -> ConfirmationItem {
    let fields = match category {
        ConfirmationCategory::Employee => vec![text("employee_id", "employee"), text("runtime", "codex")],
        ConfirmationCategory::McpServer => vec![text("name", "legacy"), text("command", "dangerous-command")],
        ConfirmationCategory::User => vec![text("name", "planted-admin"), text("role", "admin")],
        ConfirmationCategory::Hook => vec![text("name", "hook"), text("command", "dangerous-command")],
        ConfirmationCategory::LegacyApproval => vec![text("reference", "old-approval")],
        _ => vec![text("name", "department")],
    };
    store.stage_proposal(operation(id), category, fields, &Valid).unwrap()
}
fn approve(store: &mut ConfirmationStore, item: &ConfirmationItem) {
    store.approve(&[item.binding], &session(), [7; 32], 101, &Valid).unwrap();
}

#[derive(Default)]
struct Adapter {
    effects: BTreeSet<(String, u8)>,
    allocated: usize,
    live: bool,
    fail: Option<u8>,
    crash: Option<u8>,
}
impl Adapter {
    fn effect(&mut self, item: &ConfirmationItem, step: u8) -> Result<()> {
        let key = serde_json::to_string(&item.binding).unwrap();
        if self.effects.insert((key, step)) && step == 0 { self.allocated += 1; }
        if step == 1 { self.live = true; }
        if step == 3 { self.live = false; }
        if self.crash == Some(step) { self.crash = None; panic!("injected crash after external effect"); }
        if self.fail == Some(step) { return Err(ConfirmationError::Apply); }
        Ok(())
    }
}
impl ProposalApplier for Adapter {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<()> { self.effect(item, 0) }
    fn publish(&mut self, item: &ConfirmationItem) -> Result<()> { self.effect(item, 1) }
    fn read_back(&mut self, item: &ConfirmationItem) -> Result<()> { self.effect(item, 2) }
    fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<()> { self.effect(item, 3) }
}

/// R7d: a stage answering `Pending` (root allocation still verifying) keeps
/// the item `Applying` with no receipt, no Failed and no compensation; the
/// next `apply_next` re-runs the same stage and the apply resumes. One
/// intent row per proposal throughout; `Pending` from a later step fails.
#[test]
fn pending_stage_stays_applying_without_compensation_and_resumes() {
    #[derive(Default)]
    struct Root { pending: usize, stages: usize, disables: usize, publish_pending: bool }
    impl ProposalApplier for Root {
        fn stage(&mut self, _: &ConfirmationItem) -> Result<()> {
            self.stages += 1;
            // The last pending answer is `PendingLimit`: the same outcome.
            if self.pending > 0 { self.pending -= 1;
                return Err(if self.pending == 0 { ConfirmationError::PendingLimit } else { ConfirmationError::Pending }); }
            Ok(())
        }
        fn publish(&mut self, _: &ConfirmationItem) -> Result<()> {
            if self.publish_pending { Err(ConfirmationError::Pending) } else { Ok(()) }
        }
        fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
        fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { self.disables += 1; Ok(()) }
    }
    let status = |store: &ConfirmationStore, binding: &ProposalBinding|
        store.list().unwrap().into_iter().find(|i| i.binding == *binding).unwrap().status;
    let count = |store: &ConfirmationStore, sql: &str| store.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    let mut store = store();
    let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    approve(&mut store, &item);
    let mut root = Root { pending: 2, ..Root::default() };
    for attempt in 1..=2 {
        assert!(matches!(store.apply_next(&item.binding, &mut root), Err(ConfirmationError::Pending)));
        assert_eq!(status(&store, &item.binding), ConfirmationStatus::Applying);
        assert_eq!((root.stages, root.disables), (attempt, 0), "no compensation while pending");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM migration_apply"), 0, "no receipt for a pending stage");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM migration_effect_intents"), 0, "the intent is consumed");
    }
    assert_eq!(count(&store, "SELECT COUNT(*) FROM migration_confirmation_audit WHERE event='apply_pending'"), 2);
    for _ in 0..3 { store.apply_next(&item.binding, &mut root).unwrap(); }
    assert_eq!(status(&store, &item.binding), ConfirmationStatus::Applied);
    assert_eq!((root.stages, root.disables), (3, 0), "the same stage resumed once more, nothing compensated");
    assert_eq!(count(&store, "SELECT COUNT(*) FROM migration_apply"), 1);
    // Only the stage may be pending: a later step reporting it fails and compensates.
    let other = stage(&mut store, 2, ConfirmationCategory::Employee);
    approve(&mut store, &other);
    let mut late = Root { publish_pending: true, ..Root::default() };
    store.apply_next(&other.binding, &mut late).unwrap();
    assert!(matches!(store.apply_next(&other.binding, &mut late), Err(ConfirmationError::Apply)));
    assert_eq!((status(&store, &other.binding), late.disables), (ConfirmationStatus::Failed, 1));
}

#[test]
fn planted_admin_mcp_and_employees_remain_inert_until_approved() {
    let mut store = store(); let mut adapter = Adapter::default();
    for (id, category) in [ConfirmationCategory::User, ConfirmationCategory::McpServer,
        ConfirmationCategory::Employee].into_iter().enumerate() {
        let item = stage(&mut store, id as u16, category);
        assert!(store.apply_next(&item.binding, &mut adapter).is_err());
        assert!(item.high_risk);
    }
    assert_eq!(adapter.allocated, 0); assert!(!adapter.live); assert!(store.incomplete().unwrap());
}

#[test]
fn confirmation_requires_admin_mfa_fresh_session_csrf() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    for auth in [
        ApprovalSession::from_verified_session(false, true, Some(100), [7; 32], RequestId([1; 16])),
        ApprovalSession::from_verified_session(true, false, Some(100), [7; 32], RequestId([1; 16])),
        ApprovalSession::from_verified_session(true, true, None, [7; 32], RequestId([1; 16])),
        ApprovalSession::from_verified_session(true, true, Some(102), [7; 32], RequestId([1; 16])),
        ApprovalSession::from_verified_session(true, true, Some(100), [8; 32], RequestId([1; 16])),
    ] { assert!(store.approve(&[item.binding], &auth, [7; 32], 101, &Valid).is_err()); }
    assert!(store.approve(&[item.binding], &session(), [7; 32], 401, &Valid).is_err());
    assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Pending);
    approve(&mut store, &item);
}

#[test]
fn atomic_bulk_rejects_stale_duplicate_and_high_risk() {
    let mut store = store();
    let a = stage(&mut store, 1, ConfirmationCategory::Department);
    let b = stage(&mut store, 2, ConfirmationCategory::Hook);
    assert!(store.approve(&[a.binding, b.binding], &session(), [7; 32], 101, &Valid).is_err());
    assert!(store.list().unwrap().iter().all(|i| i.status == ConfirmationStatus::Pending));
    assert!(store.approve(&[a.binding, a.binding], &session(), [7; 32], 101, &Valid).is_err());
    let mut wrong = b.binding; wrong.proposal_digest = Sha256Digest([99; 32]);
    assert!(store.approve(&[a.binding, wrong], &session(), [7; 32], 101, &Valid).is_err());
    assert!(store.list().unwrap().iter().all(|i| i.status == ConfirmationStatus::Pending));
    let c = stage(&mut store, 3, ConfirmationCategory::Department);
    store.approve(&[a.binding, c.binding], &session(), [7; 32], 101, &Valid).unwrap();
}

#[test]
fn bulk_rejection_is_atomic_and_never_publishes_high_risk_authority() {
    let mut store = store();
    let a = stage(&mut store, 1, ConfirmationCategory::Department);
    let b = stage(&mut store, 2, ConfirmationCategory::McpServer);
    let mut stale = b.binding; stale.proposal_digest = Sha256Digest([99; 32]);
    assert!(store.reject_pending(&[a.binding, stale], &session(), [7; 32], 101).is_err());
    assert!(store.list().unwrap().iter().all(|item| item.status == ConfirmationStatus::Pending));
    assert!(store.reject_pending(&[a.binding, a.binding], &session(), [7; 32], 101).is_err());
    assert!(store.list().unwrap().iter().all(|item| item.status == ConfirmationStatus::Pending));
    store.reject_pending(&[a.binding, b.binding], &session(), [7; 32], 101).unwrap();
    assert!(store.list().unwrap().iter().all(|item| item.status == ConfirmationStatus::Rejected));
    assert!(!store.incomplete().unwrap());
}

/// SR2 finding 3: rejected/expired Employee proposals stop holding a quota
/// slot; open ones (pending, approved, applying, rejecting) still do.
#[test]
fn employee_quota_counts_open_only() {
    let mut store = store();
    let limit = DEFAULT_EMPLOYEE_PROPOSAL_LIMIT as u16;
    let staged: Vec<_> = (0..limit).map(|id| stage(&mut store, id, ConfirmationCategory::Employee)).collect();
    let fields = || vec![text("employee_id", "employee"), text("runtime", "codex")];
    assert!(matches!(store.stage_proposal(operation(limit), ConfirmationCategory::Employee, fields(), &Valid),
        Err(ConfirmationError::Limit)));
    // An approved (still open) proposal keeps its slot.
    approve(&mut store, &staged[0]);
    assert!(matches!(store.stage_proposal(operation(limit), ConfirmationCategory::Employee, fields(), &Valid),
        Err(ConfirmationError::Limit)));
    // A rejected one frees it, exactly once.
    store.reject_pending(&[staged[1].binding], &session(), [7; 32], 101).unwrap();
    store.stage_proposal(operation(limit), ConfirmationCategory::Employee, fields(), &Valid).unwrap();
    assert!(matches!(store.stage_proposal(operation(limit + 1), ConfirmationCategory::Employee, fields(), &Valid),
        Err(ConfirmationError::Limit)));
    // A revision of an open proposal is never refused by the quota.
    let next = ProposalOperationId { proposal_id: staged[2].binding.operation_id.proposal_id, revision: 2 };
    store.stage_proposal(next, ConfirmationCategory::Employee, vec![text("employee_id", "employee"),
        text("runtime", "claude")], &Valid).unwrap();
    // Other categories are not counted against it.
    stage(&mut store, limit + 2, ConfirmationCategory::Department);
}

/// SR2 finding 3: only never-applied, closed Employee proposals are purged,
/// and only with the Admin step-up session.
#[test]
fn purge_closed_employee_removes_only_untouched_rejected_proposals() {
    let mut store = store();
    let rejected = stage(&mut store, 1, ConfirmationCategory::Employee);
    let pending = stage(&mut store, 2, ConfirmationCategory::Employee);
    let department = stage(&mut store, 3, ConfirmationCategory::Department);
    let applied = stage(&mut store, 4, ConfirmationCategory::Employee);
    store.reject_pending(&[rejected.binding, department.binding], &session(), [7; 32], 101).unwrap();
    approve(&mut store, &applied);
    let mut adapter = Adapter::default();
    for _ in 0..3 { store.apply_next(&applied.binding, &mut adapter).unwrap(); }
    let ids: Vec<_> = [&rejected, &pending, &department, &applied].iter()
        .map(|item| item.binding.operation_id.proposal_id).collect();
    // No step-up, wrong CSRF: refused, nothing removed.
    let stale = ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([1; 16]));
    assert!(store.purge_closed_employee(&ids, &stale, [7; 32], 401).is_err());
    assert!(store.purge_closed_employee(&ids, &session(), [8; 32], 101).is_err());
    assert_eq!(store.list().unwrap().len(), 4);
    assert_eq!(store.purge_closed_employee(&ids, &session(), [7; 32], 101).unwrap(), 1);
    let left: Vec<_> = store.list().unwrap().into_iter().map(|item| item.binding.operation_id.proposal_id).collect();
    assert_eq!(left, vec![ids[1], ids[2], ids[3]]);
    // Idempotent.
    assert_eq!(store.purge_closed_employee(&ids, &session(), [7; 32], 101).unwrap(), 0);
}

#[test]
fn existing_store_open_never_creates_a_missing_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.db");
    assert!(ConfirmationStore::open_existing(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn fresh_product_validator_is_required_again_at_approval() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
    assert!(store.approve(&[item.binding], &session(), [7; 32], 101, &Invalid).is_err());
    assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Pending);
}

#[test]
fn full_digest_normalizes_field_order_and_covers_tail() {
    let fields = vec![text("name", "dept"), text("display_name", &"中".repeat(20000))];
    let mut a = store(); let mut b = store();
    let first = a.stage_proposal(operation(1), ConfirmationCategory::Department, fields.clone(), &Valid).unwrap();
    let mut reversed = fields.clone(); reversed.reverse();
    let second = b.stage_proposal(operation(1), ConfirmationCategory::Department, reversed, &Valid).unwrap();
    assert_eq!(first.binding, second.binding);
    let mut changed = fields; changed[1] = text("display_name", &("中".repeat(19999) + "文"));
    let mut c = store();
    let third = c.stage_proposal(operation(1), ConfirmationCategory::Department, changed, &Valid).unwrap();
    assert_ne!(first.binding.proposal_digest, third.binding.proposal_digest);
}

#[test]
fn malformed_fields_and_plaintext_secrets_are_refused() {
    let mut store = store();
    for fields in [vec![text("unknown", "x")], vec![text("credential", "secret")],
        vec![text("name", "a"), text("name", "b")], vec![text("name", &"x".repeat(65537))]] {
        assert!(store.stage_proposal(operation(1), ConfirmationCategory::Connector, fields, &Valid).is_err());
    }
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn approved_payload_is_immutable_and_digest_mismatch_cannot_apply() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
    approve(&mut store, &item);
    assert!(store.stage_proposal(operation(1), ConfirmationCategory::Department,
        vec![text("name", "changed")], &Valid).is_err());
    let mut revision = operation(1); revision.revision = 2;
    assert!(store.stage_proposal(revision, ConfirmationCategory::Department,
        vec![text("name", "changed")], &Valid).is_err());
    let mut wrong = item.binding; wrong.proposal_digest = Sha256Digest([99; 32]);
    assert!(store.apply_next(&wrong, &mut Adapter::default()).is_err());
}

#[test]
fn pending_edits_create_revisions_and_expire_old_confirmations_atomically() {
    let mut store = store(); let old = stage(&mut store, 1, ConfirmationCategory::Department);
    let mut revision = operation(1); revision.revision = 2;
    let new = store.stage_proposal(revision, ConfirmationCategory::Department,
        vec![text("name", "updated")], &Valid).unwrap();
    assert!(store.approve(&[old.binding], &session(), [7; 32], 101, &Valid).is_err());
    approve(&mut store, &new);
    let all = store.list().unwrap();
    assert_eq!(all[0].status, ConfirmationStatus::ExpiredRejected);
    assert_eq!(all[1].status, ConfirmationStatus::Approved);
}

#[test]
fn crash_during_failure_compensation_cannot_resume_publishing() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    approve(&mut store, &item);
    let mut adapter = Adapter { fail: Some(1), crash: Some(3), ..Default::default() };
    store.apply_next(&item.binding, &mut adapter).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.apply_next(&item.binding, &mut adapter);
    })).is_err());
    assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Failed);
    assert!(store.apply_next(&item.binding, &mut adapter).is_err());
    assert!(!adapter.live);
}

#[test]
fn applying_requires_staged_publish_and_readback_and_is_idempotent() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    approve(&mut store, &item); let mut adapter = Adapter::default();
    assert_eq!(store.apply_next(&item.binding, &mut adapter).unwrap().status, ConfirmationStatus::Applying);
    assert!(!adapter.live);
    assert_eq!(store.apply_next(&item.binding, &mut adapter).unwrap().status, ConfirmationStatus::Applying);
    assert!(adapter.live); assert!(store.incomplete().unwrap());
    assert_eq!(store.apply_next(&item.binding, &mut adapter).unwrap().status, ConfirmationStatus::Applied);
    store.apply_next(&item.binding, &mut adapter).unwrap();
    assert_eq!(adapter.allocated, 1); assert!(!store.incomplete().unwrap());
}

#[test]
fn apply_failure_disables_effects_and_retry_reuses_binding() {
    for failed_step in 0..3 {
        let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
        approve(&mut store, &item); let mut adapter = Adapter { fail: Some(failed_step), ..Default::default() };
        for _ in 0..failed_step { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        assert!(store.apply_next(&item.binding, &mut adapter).is_err());
        assert!(!adapter.live); assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Failed);
        adapter.fail = None;
        store.retry(&item.binding, &session(), [7; 32], 101, &mut adapter, &Valid).unwrap();
        for _ in 0..3 { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        assert_eq!(adapter.allocated, 1); assert!(!store.incomplete().unwrap());
    }
}

#[test]
fn retry_reconciles_partial_effect_before_absence_validation() {
    use std::{rc::Rc,cell::Cell};
    struct Partial {live:Rc<Cell<bool>>,fail_readback:bool,fail_disable:bool}
    impl ProposalApplier for Partial {
        fn stage(&mut self,_:&ConfirmationItem)->Result<()> {Ok(())}
        fn publish(&mut self,_:&ConfirmationItem)->Result<()> {self.live.set(true);Ok(())}
        fn read_back(&mut self,_:&ConfirmationItem)->Result<()> {if self.fail_readback {Err(ConfirmationError::Apply)} else {Ok(())}}
        fn disable_and_read_back(&mut self,_:&ConfirmationItem)->Result<()> {
            if self.fail_disable {self.fail_disable=false;return Err(ConfirmationError::Compensation);}
            self.live.set(false);Ok(())
        }
    }
    struct Absent(Rc<Cell<bool>>);
    impl ProposalValidator for Absent {
        fn validate_fresh(&self,_:&ConfirmationItem)->Result<()> {if self.0.get() {Err(ConfirmationError::Stale)} else {Ok(())}}
    }
    for refuse_after_cleanup in [false,true] {
        let mut store=store();let item=stage(&mut store,1,ConfirmationCategory::Employee);approve(&mut store,&item);
        let live=Rc::new(Cell::new(false));let mut adapter=Partial{live:live.clone(),fail_readback:true,fail_disable:true};
        store.apply_next(&item.binding,&mut adapter).unwrap();store.apply_next(&item.binding,&mut adapter).unwrap();
        assert!(matches!(store.apply_next(&item.binding,&mut adapter),Err(ConfirmationError::Compensation)));
        assert!(live.get());assert_eq!(store.list().unwrap()[0].status,ConfirmationStatus::Failed);
        let absent=Absent(live.clone());let validator:&dyn ProposalValidator=if refuse_after_cleanup {&Invalid}else{&absent};
        let result=store.retry(&item.binding,&session(),[7;32],101,&mut adapter,validator);
        assert!(!live.get());
        if refuse_after_cleanup {
            assert!(matches!(result,Err(ConfirmationError::Denied)));
            assert_eq!(store.list().unwrap()[0].status,ConfirmationStatus::Failed);
        } else {
            result.unwrap();adapter.fail_readback=false;
            for _ in 0..3 {store.apply_next(&item.binding,&mut adapter).unwrap();}
            assert!(live.get());assert_eq!(store.list().unwrap()[0].status,ConfirmationStatus::Applied);
        }
    }
}

#[test]
fn crash_after_every_external_effect_reopens_without_duplicate_allocation() {
    for crash_step in 0..3 {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("confirmations.db");
        let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
        let item = stage(&mut store, 1, ConfirmationCategory::Employee); approve(&mut store, &item);
        let mut adapter = Adapter { crash: Some(crash_step), ..Default::default() };
        for _ in 0..crash_step { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.apply_next(&item.binding, &mut adapter).unwrap();
        })).is_err());
        drop(store);
        let mut reopened = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
        for _ in crash_step..3 { reopened.apply_next(&item.binding, &mut adapter).unwrap(); }
        assert_eq!(adapter.allocated, 1); assert!(adapter.live); assert!(!reopened.incomplete().unwrap());
    }
}

#[test]
fn rejection_compensates_partial_publish_and_resumes_after_crash() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    approve(&mut store, &item); let mut adapter = Adapter::default();
    store.apply_next(&item.binding, &mut adapter).unwrap();
    store.apply_next(&item.binding, &mut adapter).unwrap();
    adapter.crash = Some(3);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.reject(&item.binding, &session(), [7; 32], 101, &mut adapter).unwrap();
    })).is_err());
    assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Rejecting);
    assert!(store.apply_next(&item.binding, &mut adapter).is_err());
    store.reject(&item.binding, &session(), [7; 32], 101, &mut adapter).unwrap();
    assert!(!adapter.live); assert!(!store.incomplete().unwrap());
}

#[test]
fn employee_proposals_are_bounded_without_allocating_anything() {
    let mut store = store();
    for id in 0..200 { stage(&mut store, id, ConfirmationCategory::Employee); }
    assert!(store.stage_proposal(operation(201), ConfirmationCategory::Employee,
        vec![text("employee_id", "employee")], &Valid).is_err());
    assert_eq!(store.list().unwrap().len(), 200);
}

#[test]
fn legacy_approval_is_terminal_and_never_reactivated() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::LegacyApproval);
    assert_eq!(item.status, ConfirmationStatus::ExpiredRejected);
    assert!(store.approve(&[item.binding], &session(), [7; 32], 101, &Valid).is_err());
    assert!(store.apply_next(&item.binding, &mut Adapter::default()).is_err());
}

struct Rotated;
impl RotationVerifier for Rotated {
    fn verify_rotated(&self, _: &ProposalBinding, _: &str, _: Sha256Digest) -> Result<()> { Ok(()) }
}
#[test]
fn kept_secret_stays_incomplete_until_verified_rotation_without_secret_leaks() {
    let secret = b"never-store-this-secret";
    let value = secret_fingerprint(&[1; 32], "credential", secret, true).unwrap();
    let mut store = store();
    let item = store.stage_proposal(operation(1), ConfirmationCategory::Connector,
        vec![text("name", "provider"), ProposalField { name: "credential".into(), value }], &Valid).unwrap();
    approve(&mut store, &item); let mut adapter = Adapter::default();
    for _ in 0..3 { store.apply_next(&item.binding, &mut adapter).unwrap(); }
    assert!(store.incomplete().unwrap());
    let exported = serde_json::to_string(&store.list().unwrap()).unwrap();
    assert!(!exported.contains(std::str::from_utf8(secret).unwrap()));
    let events: String = store.conn.query_row("SELECT group_concat(event) FROM migration_confirmation_audit", [], |r| r.get(0)).unwrap();
    assert!(!events.contains(std::str::from_utf8(secret).unwrap()));
    store.record_rotation(&item.binding, "credential", Sha256Digest([9; 32]), &Rotated).unwrap();
    assert!(!store.incomplete().unwrap());
}

#[test]
fn read_only_open_never_creates_a_missing_store() {
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("missing.db");
    assert!(ConfirmationStore::open_read_only(&path).is_err());
    assert!(!path.exists());
}

#[test]
fn external_effects_commit_intents_and_release_the_sqlite_writer() {
    struct Probe { path: std::path::PathBuf, calls: usize }
    impl Probe {
        fn check(&mut self, item: &ConfirmationItem) -> Result<()> {
            let mut other = Connection::open(&self.path)?;
            other.busy_timeout(std::time::Duration::from_millis(20))?;
            let tx = other.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let count: u64 = tx.query_row("SELECT COUNT(*) FROM migration_effect_intents WHERE id=?1 AND revision=?2 AND digest=?3",
                params![item.binding.operation_id.proposal_id.0.as_slice(), item.binding.operation_id.revision,
                    item.binding.proposal_digest.0.as_slice()], |r|r.get(0))?;
            assert_eq!(count, 1, "effect must have a committed intent");
            tx.commit()?;
            self.calls += 1;
            Ok(())
        }
    }
    impl ProposalApplier for Probe {
        fn stage(&mut self, item: &ConfirmationItem) -> Result<()> { self.check(item) }
        fn publish(&mut self, item: &ConfirmationItem) -> Result<()> { self.check(item) }
        fn read_back(&mut self, item: &ConfirmationItem) -> Result<()> { self.check(item) }
        fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<()> { self.check(item) }
    }
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("confirmations.db");
    let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
    let item = stage(&mut store, 1, ConfirmationCategory::Employee); approve(&mut store, &item);
    let mut adapter = Probe {path, calls:0};
    for _ in 0..3 { store.apply_next(&item.binding, &mut adapter).unwrap(); }
    let partial = stage(&mut store, 2, ConfirmationCategory::Employee); approve(&mut store, &partial);
    for _ in 0..2 { store.apply_next(&partial.binding, &mut adapter).unwrap(); }
    store.reject(&partial.binding, &session(), [7;32], 101, &mut adapter).unwrap();
    assert_eq!(adapter.calls, 6);
    assert_eq!(store.conn.query_row("SELECT COUNT(*) FROM migration_effect_intents", [], |r|r.get::<_,u64>(0)).unwrap(), 0);
}

#[test]
fn approval_validation_releases_writer_and_rechecks_concurrent_rejection() {
    struct RejectDuringValidation { path: std::path::PathBuf }
    impl ProposalValidator for RejectDuringValidation {
        fn validate_fresh(&self, item: &ConfirmationItem) -> Result<()> {
            let mut store = ConfirmationStore::open_existing(&self.path)?;
            store.reject_pending(&[item.binding], &session(), [7;32], 101)
        }
    }
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("confirmations.db");
    let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
    let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    assert!(store.approve(&[item.binding], &session(), [7;32], 101, &RejectDuringValidation {path}).is_err());
    assert_eq!(store.list().unwrap()[0].status, ConfirmationStatus::Rejected);
}

#[test]
fn active_effect_blocks_same_proposal_compensation_but_not_other_decisions() {
    struct Concurrent { path: std::path::PathBuf, other: ProposalBinding }
    impl ProposalApplier for Concurrent {
        fn stage(&mut self, item: &ConfirmationItem) -> Result<()> {
            let mut store = ConfirmationStore::open_existing(&self.path)?;
            assert!(matches!(store.reject(&item.binding, &session(), [7;32], 101,
                &mut Adapter::default()), Err(ConfirmationError::Stale)));
            store.approve(&[self.other], &session(), [7;32], 101, &Valid)?;
            Ok(())
        }
        fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
        fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
        fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    }
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("confirmations.db");
    let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
    let item = stage(&mut store, 1, ConfirmationCategory::Employee); approve(&mut store, &item);
    let other = stage(&mut store, 2, ConfirmationCategory::Department);
    store.apply_next(&item.binding, &mut Concurrent {path,other:other.binding}).unwrap();
    assert_eq!(required(&store.conn, &other.binding).unwrap().status, ConfirmationStatus::Approved);
}

/// Records every adapter call in order (0 stage, 1 publish, 2 read back,
/// 3 disable_and_read_back); `fail` makes that call answer `Apply`.
#[derive(Default)]
struct Calls { calls: Vec<u8>, fail: Option<u8> }
impl Calls {
    fn call(&mut self, step: u8) -> Result<()> {
        self.calls.push(step);
        if self.fail == Some(step) { Err(ConfirmationError::Apply) } else { Ok(()) }
    }
}
impl ProposalApplier for Calls {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<()> { self.call(0) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { self.call(1) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { self.call(2) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { self.call(3) }
}
/// A `stop` answering `true` from its `from`-th call on (1-based); `calls` counts them.
fn stop_from(calls: &std::cell::Cell<usize>, from: usize) -> impl Fn() -> bool + '_ {
    move || { calls.set(calls.get() + 1); calls.get() >= from }
}
fn state(store: &ConfirmationStore, binding: &ProposalBinding) -> (ConfirmationStatus, Option<i64>) {
    (required(&store.conn, binding).unwrap().status, step(&store.conn, binding).unwrap())
}
fn intents(store: &ConfirmationStore) -> i64 {
    store.conn.query_row("SELECT COUNT(*) FROM migration_effect_intents", [], |r| r.get(0)).unwrap()
}
fn events(store: &ConfirmationStore, event: &str) -> i64 {
    store.conn.query_row("SELECT COUNT(*) FROM migration_confirmation_audit WHERE event=?1", [event], |r| r.get(0)).unwrap()
}

/// R7d round 3: `stop` turning true at the gate right before each apply
/// step (stage, publish, read back): the step is not called, the item is
/// exactly as before (Approved stays Approved, never Failed), the intent is
/// consumed, and the same apply without stop completes.
#[test]
fn apply_effect_gate_skips_each_step_and_the_same_apply_completes() {
    for effect in 0..3u8 {
        let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
        approve(&mut store, &item);
        let mut adapter = Calls::default();
        for _ in 0..effect { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        let before = state(&store, &item.binding);
        let calls = std::cell::Cell::new(0);
        // Call 1 precedes every write; call 2 is the gate right before the step.
        let result = store.apply_next_until(&item.binding, &mut adapter, &stop_from(&calls, 2));
        assert!(matches!(result, Err(ConfirmationError::Pending)), "step {effect}");
        assert_eq!(calls.get(), 2);
        assert_eq!(adapter.calls.len(), effect as usize, "step {effect} not called");
        assert_eq!(state(&store, &item.binding), before);
        assert!(matches!(before.0, ConfirmationStatus::Approved | ConfirmationStatus::Applying));
        assert_eq!((intents(&store), events(&store, "apply_interrupted")), (0, 1));
        for _ in effect..3 { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        assert_eq!(state(&store, &item.binding).0, ConfirmationStatus::Applied);
        assert_eq!(adapter.calls, vec![0, 1, 2]);
    }
}

/// R7d round 3: a step fails and `stop` turns true at the gate right before
/// its compensation: no Disable, the item goes back to how the call found
/// it (not Failed), no receipt for the failed step; the same apply without
/// stop re-runs the step and compensates.
#[test]
fn apply_effect_gate_skips_compensation_and_never_marks_failed() {
    for failing in 0..3u8 {
        let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
        approve(&mut store, &item);
        let mut adapter = Calls { fail: Some(failing), ..Calls::default() };
        for _ in 0..failing { store.apply_next(&item.binding, &mut adapter).unwrap(); }
        let before = state(&store, &item.binding);
        let calls = std::cell::Cell::new(0);
        // 1: before any write; 2: gate before the step; 3: after the failed
        // step; 4: gate right before the Disable.
        let result = store.apply_next_until(&item.binding, &mut adapter, &stop_from(&calls, 4));
        assert!(matches!(result, Err(ConfirmationError::Pending)), "step {failing}");
        assert_eq!(calls.get(), 4);
        assert_eq!(adapter.calls, (0..=failing).collect::<Vec<u8>>(), "the step ran, no Disable");
        assert_eq!(state(&store, &item.binding), before);
        assert_ne!(before.0, ConfirmationStatus::Failed);
        assert_eq!((intents(&store), events(&store, "apply_interrupted")), (0, 1));
        assert!(matches!(store.apply_next(&item.binding, &mut adapter), Err(ConfirmationError::Apply)));
        assert_eq!(adapter.calls.last(), Some(&3));
        assert_eq!(state(&store, &item.binding), (ConfirmationStatus::Failed, Some(3)));
    }
}

/// R7d round 3: a Failed-item retry with `stop` true before any write or at
/// the gate right before its Disable: no Disable, the item stays Failed,
/// no intent left; the same retry without stop compensates and resumes.
#[test]
fn retry_effect_gate_sends_no_disable_keeps_failed_and_the_same_retry_completes() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
    approve(&mut store, &item);
    let mut adapter = Calls { fail: Some(0), ..Calls::default() };
    assert!(matches!(store.apply_next(&item.binding, &mut adapter), Err(ConfirmationError::Apply)));
    assert_eq!(adapter.calls, vec![0, 3]);
    adapter.fail = None;
    let failed = state(&store, &item.binding);
    assert_eq!(failed, (ConfirmationStatus::Failed, Some(3)));
    for from in [1, 2] {
        let calls = std::cell::Cell::new(0);
        let result = store.retry_until(&item.binding, &session(), [7; 32], 101, &mut adapter, &Valid,
            &stop_from(&calls, from));
        assert!(matches!(result, Err(ConfirmationError::Pending)), "stop from {from}");
        assert_eq!(calls.get(), from);
        assert_eq!(adapter.calls, vec![0, 3], "no Disable (stop from {from})");
        assert_eq!(state(&store, &item.binding), failed);
        assert_eq!(intents(&store), 0);
    }
    assert_eq!(events(&store, "retry_interrupted"), 1);
    store.retry_until(&item.binding, &session(), [7; 32], 101, &mut adapter, &Valid, &|| false).unwrap();
    assert_eq!(adapter.calls, vec![0, 3, 3]);
    assert_eq!(state(&store, &item.binding).0, ConfirmationStatus::Applying);
    for _ in 0..3 { store.apply_next(&item.binding, &mut adapter).unwrap(); }
    assert_eq!(state(&store, &item.binding).0, ConfirmationStatus::Applied);
}

/// R7d round 3: reject of an Applying (staged) and of a Failed item with
/// `stop` true before any write or at the gate right before the Disable: no
/// Disable, the item keeps its state (not Rejecting / Rejected, an Applying
/// one not Failed), no intent left; the same reject without stop completes.
/// A Pending item needs no effect and is rejected even while stopping.
#[test]
fn reject_effect_gate_sends_no_disable_keeps_state_and_the_same_reject_completes() {
    for failed in [false, true] {
        let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
        approve(&mut store, &item);
        let mut adapter = Calls { fail: failed.then_some(0), ..Calls::default() };
        let staged = store.apply_next(&item.binding, &mut adapter);
        assert_eq!(staged.is_err(), failed);
        adapter.fail = None;
        let before = state(&store, &item.binding);
        assert_eq!(before.0, if failed { ConfirmationStatus::Failed } else { ConfirmationStatus::Applying });
        let made = adapter.calls.clone();
        for from in [1, 2] {
            let calls = std::cell::Cell::new(0);
            let result = store.reject_until(&item.binding, &session(), [7; 32], 101, &mut adapter, &stop_from(&calls, from));
            assert!(matches!(result, Err(ConfirmationError::Pending)), "stop from {from}");
            assert_eq!(calls.get(), from);
            assert_eq!(adapter.calls, made, "no Disable (stop from {from})");
            assert_eq!(state(&store, &item.binding), before);
            assert_eq!(intents(&store), 0);
        }
        assert_eq!(events(&store, "reject_interrupted"), 1);
        store.reject_until(&item.binding, &session(), [7; 32], 101, &mut adapter, &|| false).unwrap();
        assert_eq!(adapter.calls.last(), Some(&3));
        assert_eq!(state(&store, &item.binding).0, ConfirmationStatus::Rejected);
    }
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Department);
    let mut adapter = Calls::default();
    store.reject_until(&item.binding, &session(), [7; 32], 101, &mut adapter, &|| true).unwrap();
    assert!(adapter.calls.is_empty());
    assert_eq!(state(&store, &item.binding).0, ConfirmationStatus::Rejected);
}

/// R7d round 3, the review scenario: a Failed-item retry waits behind
/// another SQLite writer (its transaction set-up, after the proposal lock)
/// while shutdown begins; when it proceeds it sends no Disable, the item
/// stays Failed and no intent is left.
#[test]
fn retry_waiting_for_the_writer_while_shutdown_begins_sends_no_disable() {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("confirmations.db");
    let conn = Connection::open(&path).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    let mut store = ConfirmationStore::from_connection(conn).unwrap();
    let item = stage(&mut store, 1, ConfirmationCategory::Department); approve(&mut store, &item);
    let mut adapter = Calls { fail: Some(0), ..Calls::default() };
    assert!(store.apply_next(&item.binding, &mut adapter).is_err());
    assert_eq!(adapter.calls, vec![0, 3]);
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let stopping = Arc::new(AtomicBool::new(false));
    let flag = stopping.clone();
    let binding = item.binding;
    let worker = std::thread::spawn(move || {
        let mut adapter = Calls::default();
        let result = store.retry_until(&binding, &session(), [7; 32], 101, &mut adapter, &Valid,
            &|| flag.load(Ordering::SeqCst));
        (store, adapter.calls, result)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!worker.is_finished(), "the retry waits behind the writer");
    stopping.store(true, Ordering::SeqCst);
    blocker.execute_batch("COMMIT").unwrap();
    let (store, calls, result) = worker.join().unwrap();
    assert!(matches!(result, Err(ConfirmationError::Pending)));
    assert!(calls.is_empty(), "no Disable once shutdown began");
    assert_eq!(state(&store, &binding), (ConfirmationStatus::Failed, Some(3)));
    assert_eq!(intents(&store), 0);
}

#[test]
fn obsolete_effect_token_cannot_commit_a_receipt() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    let first = effects::intent(&store.conn, &item, 0).unwrap();
    let second = effects::intent(&store.conn, &item, 0).unwrap();
    assert!(effects::consume(&store.conn, &item, 0, &first).is_err());
    effects::consume(&store.conn, &item, 0, &second).unwrap();
}
