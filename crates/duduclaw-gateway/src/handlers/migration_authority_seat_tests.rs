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
