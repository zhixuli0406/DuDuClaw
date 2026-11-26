//! Admission must precede the durable proposal lock and every account effect.
use super::*;
use duduclaw_core::agent_seats::{SeatAdmission, SeatDecision, seat_holders};
use duduclaw_migration::confirmation::{ApprovalSession, ConfirmationStore};
use duduclaw_migration::ids::RequestId;
use std::sync::{Arc, mpsc};
use std::time::Duration;

struct FailReadback<'a>(&'a mut GatewayAuthority);
impl ProposalApplier for FailReadback<'_> {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<()> { self.0.stage(item) }
    fn publish(&mut self, item: &ConfirmationItem) -> Result<()> { self.0.publish(item) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Err(Error::Apply) }
    fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<()> { self.0.disable_and_read_back(item) }
}

#[test]
fn migration_account_effects_take_admission_before_authority_during_concurrent_create() {
    for action in ["apply", "retry", "reject"] {
        for cap in [None, Some(1)] {
            let home = tempfile::tempdir().unwrap();
            let db = Arc::new(duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap());
            let path = home.path().join("migration-confirmations.db");
            let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
            let mut authority = GatewayAuthority::new(home.path().into()).unwrap().with_user_db(db.clone()).with_agent_cap(cap);
            let validator = GatewayAuthority::new(home.path().into()).unwrap().with_user_db(db.clone()).with_agent_cap(cap);
            let approval = ApprovalSession::from_verified_session(true, true, Some(100), [2;32], RequestId([3;16]));
            let item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([7;16]), revision: 1 },
                ConfirmationCategory::User,
                [("name", "concurrent@example.test"), ("display_name", "Concurrent"), ("role", "employee")]
                    .into_iter().map(|(name, value)| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }).collect(),
                &validator).unwrap();
            let binding = item.binding;
            store.approve(&[binding], &approval, [2;32], 100, &validator).unwrap();
            // Start at publish for apply, or at compensation for retry/reject.
            authority.with_confirmation_admission(&mut store, &binding, |s, a| s.apply_next(&binding, a)).unwrap();
            if action != "apply" {
                authority.with_confirmation_admission(&mut store, &binding, |s, a| s.apply_next(&binding, a)).unwrap();
            }
            if action == "retry" {
                let result = authority.with_confirmation_admission(&mut store, &binding, |s, a| {
                    s.apply_next(&binding, &mut FailReadback(a))
                });
                assert!(matches!(result, Err(Error::Apply)));
            }
            let held = SeatAdmission::acquire(home.path()).unwrap();
            let (ready, started) = mpsc::channel();
            let (done, finished) = mpsc::channel();
            let apply_home = home.path().to_path_buf();
            std::thread::scope(|scope| {
                let worker = scope.spawn(move || {
                    ready.send(()).unwrap();
                    let result = authority.with_confirmation_admission(&mut store, &binding, |s, a| {
                        assert!(SeatAdmission::try_acquire(&apply_home).unwrap().is_none());
                        match action {
                            "retry" => s.retry(&binding, &approval, [2;32], 100, a, &validator),
                            "reject" => s.reject(&binding, &approval, [2;32], 100, a),
                            _ => s.apply_next(&binding, a).map(|_| ()),
                        }
                    });
                    done.send(result).unwrap();
                    store.list().unwrap()[0].status
                });
                started.recv_timeout(Duration::from_secs(2)).unwrap();
                let waiting = matches!(finished.recv_timeout(Duration::from_millis(200)), Err(mpsc::RecvTimeoutError::Timeout));
                // The exact proposal lock must remain available while migration
                // waits for admission. An inverted order fails this assertion.
                let lock_path = path.with_extension(format!("proposal-{}.lock", hex::encode(binding.operation_id.proposal_id.0)));
                let proposal_lock = std::fs::OpenOptions::new().read(true).write(true).open(lock_path).unwrap();
                let order_ok = proposal_lock.try_lock().is_ok();
                // Publish a real seat-affecting create under its admission and
                // an authority writer. Migration must not hold either first.
                let mut authority_db = Connection::open(home.path().join("migration-authority/apply.db")).unwrap();
                authority_db.busy_timeout(Duration::from_secs(2)).unwrap();
                let create = (|| -> std::result::Result<(), String> {
                    let tx = authority_db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
                    if !matches!(held.decide(1, 1, None), SeatDecision::Admitted) { return Err("create refused".into()); }
                    let dir = home.path().join("agents/concurrent");
                    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                    std::fs::write(dir.join("agent.toml"), "[agent]\nname='concurrent'\nstatus='active'\n").map_err(|e| e.to_string())?;
                    held.recount(1)?;
                    tx.commit().map_err(|e| e.to_string())
                })();
                drop(proposal_lock);
                drop(held);
                assert!(waiting, "{action} bypassed admission");
                assert!(order_ok, "{action} acquired authority before admission");
                create.unwrap();
                finished.recv_timeout(Duration::from_secs(5)).expect("migration and create deadlocked").unwrap();
                let status = worker.join().unwrap();
                assert_eq!(status, if action == "reject" { ConfirmationStatus::Rejected } else { ConfirmationStatus::Applying });
            });
            assert_eq!(seat_holders(home.path()).unwrap().len(), 1);
            let user = db.get_user_by_email("concurrent@example.test").unwrap().unwrap();
            assert_eq!(user.status, if action == "apply" { duduclaw_auth::UserStatus::Active } else { duduclaw_auth::UserStatus::Suspended });
        }
    }
}

// These employee fixtures use the real receipt/lifecycle stores, whose
// hardened open_existing is Unix-only. The production allocator is Linux-only;
// the account admission test above remains cross-platform.
#[cfg(unix)]
struct EmployeeSeatBackend {
    home: PathBuf,
    waiting: mpsc::Sender<()>,
    proceed: std::sync::Mutex<mpsc::Receiver<()>>,
    fail: bool,
    fail_compensation: bool,
}
#[cfg(unix)]
impl MigrationAuthorityBackend for EmployeeSeatBackend {
    fn supports(&self, category: ConfirmationCategory) -> bool { category == ConfirmationCategory::Employee }
    fn validate(&self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn stage(&self, _: &ConfirmationItem) -> Result<()> { panic!("unorchestrated employee stage") }
    fn publish(&self, _: &ConfirmationItem) -> Result<()> { panic!("unorchestrated employee publish") }
    fn read_back(&self, _: &ConfirmationItem) -> Result<()> { panic!("unorchestrated employee readback") }
    fn disable(&self, _: &ConfirmationItem) -> Result<()> { panic!("unorchestrated employee compensation") }
    fn employee_effect(&self, item: &ConfirmationItem, effect: ProposalEffect, commit: &employee::EmployeeCommit<'_>) -> Result<()> {
        if self.fail && effect != ProposalEffect::Disable { return Err(Error::Apply); }
        // Model a remote wait: neither admission nor proposal may be held.
        assert!(SeatAdmission::try_acquire(&self.home).unwrap().is_some());
        self.waiting.send(()).unwrap();
        self.proceed.lock().unwrap().recv_timeout(Duration::from_secs(5)).unwrap();
        let spec = duduclaw_migration::allocation::specification(item).unwrap();
        let receipt = duduclaw_migration::allocation::Receipt {
            binding: item.binding, label: spec.label, employee: duduclaw_isolation::ids::EmployeeId([8;16]),
            uid: 70001, bytes: 67108864, inodes: 4096, disabled: effect == ProposalEffect::Disable,
            principal_binding: spec.binding,
        };
        commit.run(|admission| {
            assert!(SeatAdmission::try_acquire(&self.home).unwrap().is_none());
            let proposal = std::fs::OpenOptions::new().read(true).write(true).open(
                self.home.join("migration-confirmations.db").with_extension(format!("proposal-{}.lock", hex::encode(item.binding.operation_id.proposal_id.0)))).unwrap();
            assert!(proposal.try_lock().is_err(), "employee commit has no proposal lock");
            employee::save_confirmed_receipt_gated(&self.home, item, &receipt, None, admission)?;
            if self.fail_compensation { Err(Error::Compensation) } else { Ok(()) }
        })
    }
}

#[cfg(unix)]
struct EmployeeSeedSteps;
#[cfg(unix)]
impl ProposalApplier for EmployeeSeedSteps {
    fn stage(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn publish(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
    fn disable_and_read_back(&mut self, _: &ConfirmationItem) -> Result<()> { Ok(()) }
}

#[cfg(unix)]
fn employee_contention(action: &str) {
    let applying = matches!(action, "apply" | "publish" | "readback");
    let home = crate::fs_safe::private_test_home().unwrap();
    let users = Arc::new(duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap());
    let owner = users.create_user("owner@example.test", "Owner", "password1", duduclaw_auth::UserRole::Employee).unwrap().id;
    Connection::open(home.path().join("users.db")).unwrap().execute(
        "UPDATE users SET status='active',must_change_password=0,last_login='2026-11-25T00:00:00Z' WHERE id=?1", [&owner]).unwrap();
    let path = home.path().join("migration-confirmations.db");
    let mut store = ConfirmationStore::from_connection(Connection::open(&path).unwrap()).unwrap();
    let (waiting, remote) = mpsc::channel();
    let (proceed, resume) = mpsc::channel();
    let backend = Arc::new(EmployeeSeatBackend { home: home.path().into(), waiting, proceed: std::sync::Mutex::new(resume), fail: matches!(action, "automatic_compensation" | "failed_compensation"), fail_compensation: action == "failed_compensation" });
    // Exercise the production composite forwarding as well as the gateway.
    let backend = Arc::new(retire::CompositeBackend::new(vec![backend]));
    let mut authority = GatewayAuthority::new(home.path().into()).unwrap().with_user_db(users.clone()).with_backend(backend.clone());
    let validator = GatewayAuthority::new(home.path().into()).unwrap().with_user_db(users).with_backend(backend);
    let approval = ApprovalSession::from_verified_session(true, true, Some(100), [2;32], RequestId([3;16]));
    let item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([9;16]), revision: 1 }, ConfirmationCategory::Employee,
        [("employee_id", "sales"), ("disk_bytes", "67108864"), ("binding_kind", "personal"), ("owner_id", owner.as_str())]
            .into_iter().map(|(name, value)| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } }).collect(), &validator).unwrap();
    let binding = item.binding;
    store.approve(&[binding], &approval, [2;32], 100, &validator).unwrap();
    for _ in 0..match action { "publish" => 1, "readback" => 2, _ => 0 } {
        store.apply_next(&binding, &mut EmployeeSeedSteps).unwrap();
    }
    if action != "apply" {
        let spec = duduclaw_migration::allocation::specification(&item).unwrap();
        let receipt = duduclaw_migration::allocation::Receipt {
            binding, label: spec.label, employee: duduclaw_isolation::ids::EmployeeId([8;16]),
            uid: 70001, bytes: 67108864, inodes: 4096, disabled: false, principal_binding: spec.binding,
        };
        employee::save_confirmed_receipt(home.path(), &item, &receipt).unwrap();
    }
    let (done, finished) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let result = authority.with_confirmation_admission(&mut store, &binding, |s, a| {
                if action == "reject" { s.reject(&binding, &approval, [2;32], 100, a) }
                else { s.apply_next(&binding, a).map(|_| ()) }
            });
            done.send(result).unwrap();
            store.list().unwrap()[0].status
        });
        remote.recv_timeout(Duration::from_secs(5)).unwrap();
        let lock_path = path.with_extension(format!("proposal-{}.lock", hex::encode(binding.operation_id.proposal_id.0)));
        let proposal = std::fs::OpenOptions::new().read(true).write(true).open(lock_path).unwrap();
        assert!(proposal.try_lock().is_ok(), "remote wait holds proposal");
        drop(proposal);
        let held = SeatAdmission::acquire(home.path()).unwrap();
        // A second operation must not race the remote effect, even while the
        // proposal commit lock is free.
        let mut other = ConfirmationStore::open_existing(&path).unwrap();
        let mut other_authority = GatewayAuthority::new(home.path().into()).unwrap();
        assert!(matches!(other.apply_next(&binding, &mut other_authority), Err(Error::Stale)));
        proceed.send(()).unwrap();
        let waiting = matches!(finished.recv_timeout(Duration::from_millis(200)), Err(mpsc::RecvTimeoutError::Timeout));
        let proposal = std::fs::OpenOptions::new().read(true).write(true)
            .open(path.with_extension(format!("proposal-{}.lock", hex::encode(binding.operation_id.proposal_id.0)))).unwrap();
        let order_ok = proposal.try_lock().is_ok();
        let mut apply = Connection::open(home.path().join("migration-authority/apply.db")).unwrap();
        apply.busy_timeout(Duration::from_secs(1)).unwrap();
        let writer_free = apply.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).and_then(|tx| tx.commit()).is_ok();
        drop(proposal);
        drop(held);
        assert!(waiting && order_ok && writer_free, "{action}: employee violated admission order");
        let result = finished.recv_timeout(Duration::from_secs(5)).expect("employee commit deadlocked");
        match action {
            "failed_compensation" => assert!(matches!(result, Err(Error::Compensation))),
            "automatic_compensation" => assert!(matches!(result, Err(Error::Apply))),
            _ => result.unwrap(),
        }
        assert_eq!(worker.join().unwrap(), match action {
            "reject" => ConfirmationStatus::Rejected, "automatic_compensation" | "failed_compensation" => ConfirmationStatus::Failed,
            "readback" => ConfirmationStatus::Applied, _ => ConfirmationStatus::Applying,
        });
    });
    let db = Connection::open(home.path().join("migration-authority/apply.db")).unwrap();
    let encoded: String = db.query_row("SELECT receipt FROM confirmed_employees WHERE label='sales'", [], |r| r.get(0)).unwrap();
    let receipt: duduclaw_migration::allocation::Receipt = serde_json::from_str(&encoded).unwrap();
    assert_eq!(receipt.disabled, !applying);
    let users = Connection::open(home.path().join("users.db")).unwrap();
    let mirrors: i64 = users.query_row("SELECT count(*) FROM user_agent_bindings WHERE agent_name='sales'", [], |r| r.get(0)).unwrap();
    assert_eq!(mirrors, if applying { 1 } else { 0 });
}

#[test]
#[cfg(unix)]
fn migration_employee_apply_contention() {
    for action in ["apply", "publish", "readback"] { employee_contention(action); }
}
#[test]
#[cfg(unix)]
fn migration_employee_reject_contention() { employee_contention("reject"); }
#[test]
#[cfg(unix)]
fn migration_employee_failed_compensation_contention() { employee_contention("failed_compensation"); }

#[test]
#[cfg(unix)]
fn migration_employee_automatic_compensation_contention() { employee_contention("automatic_compensation"); }
