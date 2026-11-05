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

#[test]
fn obsolete_effect_token_cannot_commit_a_receipt() {
    let mut store = store(); let item = stage(&mut store, 1, ConfirmationCategory::Employee);
    let first = effects::intent(&store.conn, &item, 0).unwrap();
    let second = effects::intent(&store.conn, &item, 0).unwrap();
    assert!(effects::consume(&store.conn, &item, 0, &first).is_err());
    effects::consume(&store.conn, &item, 0, &second).unwrap();
}
