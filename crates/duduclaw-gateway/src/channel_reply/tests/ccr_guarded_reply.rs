use super::*;

#[cfg(test)]
mod ccr_guarded_reply_tests {
    use super::{
        CCR_DELIVERY_REFUSED_TEXT, CCR_LEASE_UNSUPPORTED_TEXT, CCR_TURN_DELIVERY, CcrTurnDelivery,
        GuardedReply, SessionManager, ccr_delivery_verdict, record_turn_assistant_message,
    };
    use duduclaw_llm::{CcrDeliveryGuards, CcrDeliveryLease};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Debug)]
    struct FakeLease {
        valid: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }

    impl CcrDeliveryLease for FakeLease {
        fn still_valid(&self) -> bool {
            self.valid.load(Ordering::SeqCst)
        }
    }

    impl Drop for FakeLease {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fake_guards() -> (Vec<CcrDeliveryGuards>, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let valid = Arc::new(AtomicBool::new(true));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut guards = CcrDeliveryGuards::default();
        guards.push(Arc::new(FakeLease {
            valid: valid.clone(),
            drops: drops.clone(),
        }));
        (vec![guards], valid, drops)
    }

    async fn guarded_reply() -> (GuardedReply, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let (guards, valid, drops) = fake_guards();
        (
            GuardedReply::new(
                "source answer".to_string(),
                Some(serde_json::json!({"kind":"card"})),
                guards,
                None,
            )
            .await,
            valid,
            drops,
        )
    }

    #[tokio::test]
    async fn lease_survives_awaited_send_and_revocation_changes_validity() {
        let (reply, valid, drops) = guarded_reply().await;
        async fn fake_send(reply: &GuardedReply) {
            assert!(reply.still_valid().await);
            tokio::task::yield_now().await;
            assert!(reply.still_valid().await);
        }
        fake_send(&reply).await;
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(reply.text, "source answer");
        valid.store(false, Ordering::SeqCst);
        assert!(!reply.still_valid().await);
        drop(reply);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn legacy_reply_refuses_to_drop_a_source_lease_with_answer() {
        let (reply, _valid, drops) = guarded_reply().await;
        let (text, artifact) = reply.into_legacy_parts();
        // The lease is intact here — the transport simply cannot hold one, so
        // the refusal must not claim the source changed (it did not).
        assert_eq!(text, CCR_LEASE_UNSUPPORTED_TEXT);
        assert_ne!(text, CCR_DELIVERY_REFUSED_TEXT);
        assert!(artifact.is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn legacy_reply_still_reports_a_genuinely_revoked_source_as_changed() {
        let (guards, valid, _drops) = fake_guards();
        valid.store(false, Ordering::SeqCst);
        let rebuilt = GuardedReply::new("source answer".to_string(), None, guards, None).await;
        assert!(rebuilt.refused);
        let (text, _) = rebuilt.into_legacy_parts();
        assert_eq!(text, CCR_DELIVERY_REFUSED_TEXT);
    }

    const SOURCE_DERIVED: &str = "客戶 X 的合約金額是 NT$1,234,567";

    type PendingTurn = (
        tempfile::TempDir,
        Arc<SessionManager>,
        Arc<CcrTurnDelivery>,
        tokio::sync::oneshot::Receiver<bool>,
    );

    async fn session_with_pending_assistant_turn() -> PendingTurn {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        mgr.get_or_create("telegram:-100123", "agent-a")
            .await
            .unwrap();
        mgr.append_message("telegram:-100123", "user", "查一下客戶 X 的合約", 8)
            .await
            .unwrap();
        let delivery = Arc::new(CcrTurnDelivery::default());
        // Exactly the order `build_reply_with_session_inner` uses: persist the
        // assistant turn and schedule distillation, both BEFORE the lease is
        // rechecked by `GuardedReply::new`.
        let verdict = CCR_TURN_DELIVERY
            .scope(delivery.clone(), async {
                let id = mgr
                    .append_delivery_message("telegram:-100123", "assistant", SOURCE_DERIVED, 12,
                        None, delivery.waits_for_turn_delivery())
                    .await
                    .unwrap();
                record_turn_assistant_message("telegram:-100123", id);
                ccr_delivery_verdict().expect("the gate is in scope")
            })
            .await;
        (dir, mgr, delivery, verdict)
    }

    #[tokio::test]
    async fn refused_delivery_erases_the_source_derived_turn_and_stops_distillation() {
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;

        // The source is revoked while the reply is being finalized.
        let (guards, valid, _drops) = fake_guards();
        valid.store(false, Ordering::SeqCst);
        let refused = GuardedReply::new(
            SOURCE_DERIVED.to_string(),
            None,
            guards,
            Some(super::TurnRollback {
                delivery: delivery.clone(),
                session_mgr: mgr.clone(),
            }),
        )
        .await;
        assert!(refused.refused);

        let history = mgr.get_messages("telegram:-100123").await.unwrap();
        assert!(
            history
                .iter()
                .all(|message| !message.content.contains("1,234,567")),
            "revoked source wording must not survive in session history"
        );
        assert!(
            !history
                .iter()
                .any(|message| message.role == "assistant" && message.content == SOURCE_DERIVED)
        );
        assert_eq!(
            verdict.await,
            Ok(false),
            "distillation must be told the turn was refused"
        );
    }

    /// Regression (W3-1 #4 / debt 5): the lease can survive `GuardedReply`
    /// construction and be lost between two outbound segments. The adapter
    /// only calls `still_valid()` (as all eleven already do) — that recheck
    /// must itself run the rollback: the persisted assistant row is revoked
    /// and distillation is told the turn was never delivered.
    #[tokio::test]
    async fn revocation_between_two_segments_rolls_the_turn_back_from_still_valid_alone() {
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
        let (guards, valid, _drops) = fake_guards();
        let reply = GuardedReply::new(
            SOURCE_DERIVED.to_string(),
            None,
            guards,
            Some(super::TurnRollback {
                delivery: delivery.clone(),
                session_mgr: mgr.clone(),
            }),
        )
        .await;
        assert!(!reply.refused);

        // Segment 1 goes out.
        assert!(reply.still_valid().await);
        assert!(
            mgr.get_messages("telegram:-100123")
                .await
                .unwrap()
                .iter()
                .any(|message| message.content == SOURCE_DERIVED),
            "the turn is still live after the first segment"
        );

        // The source is revoked; segment 2's pre-send recheck observes it.
        valid.store(false, Ordering::SeqCst);
        assert!(!reply.still_valid().await);

        let history = mgr.get_messages("telegram:-100123").await.unwrap();
        assert!(
            history
                .iter()
                .all(|message| !message.content.contains("1,234,567")),
            "the source-derived turn must not survive a mid-delivery revocation"
        );
        assert_eq!(
            verdict.await,
            Ok(false),
            "distillation must be told the turn was refused"
        );
        // Dropping the reply afterwards must not publish a second, contrary
        // verdict.
        drop(reply);
    }

    /// Regression (W3-1 #4): the sync `office_docs::DeliveryCheck` hook can
    /// only latch a lost lease — it has no `await` for `revoke_message`. The
    /// rollback must still happen, at the latest when the reply is dropped.
    #[tokio::test]
    async fn a_blocking_recheck_that_loses_the_lease_still_rolls_the_turn_back_on_drop() {
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
        let (guards, valid, _drops) = fake_guards();
        let reply = GuardedReply::new(
            SOURCE_DERIVED.to_string(),
            None,
            guards,
            Some(super::TurnRollback {
                delivery: delivery.clone(),
                session_mgr: mgr.clone(),
            }),
        )
        .await;
        valid.store(false, Ordering::SeqCst);
        assert!(!reply.still_valid_blocking());
        drop(reply);
        assert_eq!(verdict.await, Ok(false));
        for _ in 0..200 {
            let history = mgr.get_messages("telegram:-100123").await.unwrap();
            if history
                .iter()
                .all(|message| !message.content.contains("1,234,567"))
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("a latched refusal never reached session history");
    }

    #[tokio::test]
    async fn delivered_reply_keeps_its_turn_and_releases_distillation() {
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
        delivery.settle(&mgr, true).await;

        let history = mgr.get_messages("telegram:-100123").await.unwrap();
        assert!(
            history
                .iter()
                .any(|message| message.role == "assistant" && message.content == SOURCE_DERIVED),
            "a delivered reply must stay in history unchanged"
        );
        assert_eq!(verdict.await, Ok(true));
    }

    #[tokio::test]
    async fn webchat_cancel_during_raw_pipeline_rolls_back_saved_assistant() {
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task_mgr = mgr.clone();
        let task = tokio::spawn(async move {
            let _building = super::TurnBuildRollback::new(delivery, task_mgr);
            let _ = ready_tx.send(());
            // The raw reply has saved its row, but e.g. thinking finalization
            // or contract enforcement has not returned a GuardedReply yet.
            std::future::pending::<()>().await;
        });
        ready_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(verdict.await, Ok(false));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let history = mgr.get_messages("telegram:-100123").await.unwrap();
                if history.iter().all(|message| !message.content.contains("1,234,567")) {
                    assert!(history.iter().any(|message| message.role == "user"));
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.expect("raw-pipeline cancellation must revoke the assistant row");
    }

    #[tokio::test]
    async fn webchat_build_rollback_disarms_only_after_reply_handoff() {
        let (_dir, mgr, delivery, mut verdict) = session_with_pending_assistant_turn().await;
        let mut building = super::TurnBuildRollback::new(delivery.clone(), mgr.clone());
        let (guards, _valid, _drops) = fake_guards();
        let reply = GuardedReply::new(
            SOURCE_DERIVED.into(), None, guards,
            Some(super::TurnRollback { delivery, session_mgr: mgr.clone() }),
        ).await;
        building.disarm();
        drop(building);
        assert!(matches!(verdict.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        drop(reply);
        assert_eq!(verdict.await, Ok(true));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().any(|message| message.content == SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_cancel_during_guard_construction_never_publishes_delivered() {
        #[derive(Debug)]
        struct BlockingLease {
            entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            released: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
        }
        impl CcrDeliveryLease for BlockingLease {
            fn still_valid(&self) -> bool {
                if let Some(entered) = self.entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (lock, wake) = &*self.released;
                let released = lock.lock().unwrap();
                drop(wake.wait_while(released, |released| !*released).unwrap());
                true
            }
        }
        let (_dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
        let released = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let mut guards = CcrDeliveryGuards::default();
        guards.push(Arc::new(BlockingLease {
            entered: std::sync::Mutex::new(Some(entered_tx)), released: released.clone(),
        }));
        let task = tokio::spawn(GuardedReply::new(
            SOURCE_DERIVED.into(), None, vec![guards],
            Some(super::TurnRollback { delivery, session_mgr: mgr.clone() }),
        ));
        entered_rx.await.unwrap();
        task.abort();
        let result = task.await;
        // Unblock the validation worker before asserting, including on failure.
        *released.0.lock().unwrap() = true;
        released.1.notify_all();
        assert!(result.unwrap_err().is_cancelled());
        assert_eq!(verdict.await, Ok(false));
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while mgr.get_messages("telegram:-100123").await.unwrap()
                .iter().any(|message| message.content == SOURCE_DERIVED)
            {
                tokio::task::yield_now().await;
            }
        }).await.expect("constructor cancellation must revoke the assistant row");
    }

    #[tokio::test]
    async fn webchat_fifo_waits_for_cancelled_row_revocation() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            let building = super::TurnBuildRollback::new(delivery, mgr.clone());
            drop(building);
            (dir, mgr, verdict)
        }).await;
        assert_eq!(verdict.await, Ok(false));
        cleanup.unwrap();
        // No polling: a successor can read history immediately after the scope.
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().all(|message| message.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_unpublished_drop_never_notifies_success() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            let reply = GuardedReply::new(SOURCE_DERIVED.into(), None, Vec::new(),
                Some(super::TurnRollback { delivery, session_mgr: mgr.clone() })).await;
            assert!(reply.still_valid().await);
            drop(reply);
            (dir, mgr, verdict)
        }).await;
        cleanup.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.webchat_history("telegram:-100123").await.unwrap().iter()
            .all(|row| row.message.role != "assistant"));
    }

    #[tokio::test]
    async fn webchat_failed_revocation_stays_hidden_after_reopen_and_restore() {
        let ((dir, mgr, id, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            let id = delivery.assistant_message.lock().unwrap().as_ref().unwrap().1;
            // Preserve the row and inject the exact failure being reviewed.
            mgr.acquire().await.execute_batch(
                "CREATE TRIGGER reject_revoke BEFORE UPDATE OF content ON session_messages
                 BEGIN SELECT RAISE(ABORT, 'injected revocation failure'); END;"
            ).unwrap();
            assert!(mgr.webchat_history("telegram:-100123").await.unwrap().iter()
                .all(|row| row.message.content != SOURCE_DERIVED));
            let building = super::TurnBuildRollback::new(delivery, mgr.clone());
            drop(building);
            (dir, mgr, id, verdict)
        }).await;
        assert!(cleanup.is_err());
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_message_ids("telegram:-100123").await.unwrap().iter().all(|row| *row != id));
        drop(mgr);
        let reopened = SessionManager::new(&dir.path().join("sessions.db")).unwrap();
        reopened.restore_message("telegram:-100123", id).await.unwrap();
        for message in reopened.get_messages("telegram:-100123").await.unwrap() {
            assert_ne!(message.content, SOURCE_DERIVED);
        }
        assert!(reopened.webchat_history("telegram:-100123").await.unwrap().iter()
            .all(|row| row.id != id));
        let raw: String = reopened.acquire().await.query_row(
            "SELECT content FROM session_messages WHERE id=?1", [id], |r| r.get(0),
        ).unwrap();
        assert_eq!(raw, SOURCE_DERIVED, "failed scrub must be hidden even when bytes remain");
    }

    #[tokio::test]
    async fn webchat_cancel_during_refusal_waits_for_owned_revocation_job() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            let mut locks = Vec::new();
            for _ in 0..4 { locks.push(mgr.acquire().await); }
            {
                let refusing = delivery.settle(&mgr, false);
                tokio::pin!(refusing);
                tokio::select! {
                    () = &mut refusing => panic!("revocation must wait for the pool"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
                }
                // Dropping the refusal waiter must not lose its already-published row.
            }
            drop(locks);
            (dir, mgr, verdict)
        }).await;
        cleanup.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().all(|message| message.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_panic_waits_for_revocation_before_successor_reads() {
        use futures_util::FutureExt;
        let mut fixture = None;
        let result = std::panic::AssertUnwindSafe(super::with_delivery_cleanup(async {
            fixture = Some(session_with_pending_assistant_turn().await);
            let (_, mgr, delivery, _) = fixture.as_ref().unwrap();
            let _building = super::TurnBuildRollback::new(delivery.clone(), mgr.clone());
            panic!("injected panic while building reply");
        })).catch_unwind().await;
        assert!(result.is_err());
        let (_dir, mgr, _, verdict) = fixture.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().all(|message| message.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_panic_after_build_revokes_lease_free_reply() {
        use futures_util::FutureExt;
        let mut fixture = None;
        let result = std::panic::AssertUnwindSafe(super::with_delivery_cleanup(async {
            fixture = Some(session_with_pending_assistant_turn().await);
            let (dir, _, delivery, _) = fixture.as_ref().unwrap();
            std::fs::create_dir_all(dir.path().join("agents")).unwrap();
            let ctx = crate::agent_channels::e2e_tests::reply_ctx(dir.path()).await;
            let _reply = super::attach_turn_rollback(
                SOURCE_DERIVED.into(), None, Vec::new(), delivery.clone(), &ctx,
            ).await;
            panic!("injected panic after reply build");
        })).catch_unwind().await;
        assert!(result.is_err());
        let (_dir, mgr, _, verdict) = fixture.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().all(|message| message.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_cancel_between_build_and_delivery_revokes_persisted_reply() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            std::fs::create_dir_all(dir.path().join("agents")).unwrap();
            let ctx = crate::agent_channels::e2e_tests::reply_ctx(dir.path()).await;
            let reply = super::attach_turn_rollback(
                SOURCE_DERIVED.into(), None, Vec::new(), delivery, &ctx,
            ).await;
            // The production begin_delivery refusal takes this synchronous path.
            reply.refuse_delivery();
            drop(reply);
            (dir, mgr, verdict)
        }).await;
        cleanup.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap()
            .iter().all(|message| message.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_history_delivery_survives_transport_loss_but_not_source_revocation() {
        for revoke in [false, true] {
            let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
                let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
                let (guards, valid, _) = fake_guards();
                let reply = GuardedReply::new(SOURCE_DERIVED.into(), None, guards,
                    Some(super::TurnRollback { delivery, session_mgr: mgr.clone() })).await;
                reply.persist_webchat(&mgr, "telegram:-100123", SOURCE_DERIVED).await.unwrap();
                reply.persist_webchat(&mgr, "telegram:-100123", SOURCE_DERIVED).await.unwrap();
                // The live writer is gone; history is the delivery destination.
                let (tx, rx) = tokio::sync::mpsc::channel::<()>(1);
                drop(rx);
                assert!(tx.send(()).await.is_err());
                if revoke { valid.store(false, Ordering::SeqCst); }
                assert_eq!(reply.delivery_fence().valid().await, !revoke);
                assert_eq!(reply.still_valid().await, !revoke);
                if !revoke {
                    let users = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
                    reply.publish_webchat(&mgr, "telegram:-100123", &users, users.acl_generation().unwrap(), &crate::webchat::access::TargetPolicy::test_admin(),
                        &tokio_util::sync::CancellationToken::new()).await.unwrap();
                }
                drop(reply);
                (dir, mgr, verdict)
            }).await;
            cleanup.unwrap();
            assert_eq!(verdict.await, Ok(!revoke));
            let history = mgr.get_messages("telegram:-100123").await.unwrap();
            assert_eq!(history.iter().any(|m| m.content == SOURCE_DERIVED), !revoke);
            // Revocation tombstones the row, so get_messages must hide it.
            // Transport loss alone keeps exactly one row, even after a retry.
            assert_eq!(history.iter().filter(|m| m.role == "assistant").count(), if revoke { 0 } else { 1 });
        }
    }

    #[tokio::test]
    async fn webchat_logout_after_history_commit_revokes_delivery() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, delivery, verdict) = session_with_pending_assistant_turn().await;
            let reply = GuardedReply::new(SOURCE_DERIVED.into(), None, Vec::new(),
                Some(super::TurnRollback { delivery, session_mgr: mgr.clone() })).await;
            reply.persist_webchat(&mgr, "telegram:-100123", SOURCE_DERIVED).await.unwrap();
            let cancel = tokio_util::sync::CancellationToken::new();
            let fence = reply.delivery_fence().for_turn(&cancel);
            cancel.cancel();
            assert!(!fence.valid().await);
            drop(reply);
            (dir, mgr, verdict)
        }).await;
        cleanup.unwrap();
        assert_eq!(verdict.await, Ok(false));
        assert!(mgr.get_messages("telegram:-100123").await.unwrap().iter()
            .all(|m| m.content != SOURCE_DERIVED));
    }

    #[tokio::test]
    async fn webchat_history_delivery_records_missing_pipeline_row() {
        let ((_dir, mgr, verdict), cleanup) = super::with_delivery_cleanup(async {
            let (dir, mgr, _, _) = session_with_pending_assistant_turn().await;
            let delivery = Arc::new(CcrTurnDelivery::default());
            let verdict = delivery.subscribe();
            let reply = GuardedReply::new("early response".into(), None, Vec::new(),
                Some(super::TurnRollback { delivery, session_mgr: mgr.clone() })).await;
            reply.persist_webchat(&mgr, "telegram:-100123", "early response").await.unwrap();
            reply.persist_webchat(&mgr, "telegram:-100123", "early response").await.unwrap();
            assert!(mgr.get_messages("telegram:-100123").await.unwrap().iter()
                .all(|m| m.content != "early response"));
            let users = duduclaw_auth::UserDb::new(&dir.path().join("users.db")).unwrap();
            reply.publish_webchat(&mgr, "telegram:-100123", &users, users.acl_generation().unwrap(), &crate::webchat::access::TargetPolicy::test_admin(),
                &tokio_util::sync::CancellationToken::new()).await.unwrap();
            drop(reply);
            (dir, mgr, verdict)
        }).await;
        cleanup.unwrap();
        assert_eq!(verdict.await, Ok(true));
        assert_eq!(mgr.get_messages("telegram:-100123").await.unwrap().iter()
            .filter(|m| m.content == "early response").count(), 1);
    }

    #[tokio::test]
    async fn native_loop_collector_retains_guard_until_reply_takes_it() {
        let (reply, _valid, drops) = guarded_reply().await;
        assert!(
            !crate::ccr_runtime::capture_delivery_guards(reply.guards[0].clone()).await
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let collector = Arc::new(std::sync::Mutex::new(Vec::new()));
        crate::ccr_runtime::DELIVERY_GUARD_COLLECTOR
            .scope(collector.clone(), async {
                assert!(
                    crate::ccr_runtime::capture_delivery_guards(reply.guards[0].clone()).await
                );
            })
            .await;
        drop(reply);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let retained = std::mem::take(&mut *collector.lock().unwrap());
        assert_eq!(retained.len(), 1);
        drop(retained);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

