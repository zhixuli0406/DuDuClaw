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
                    .append_message_with_id("telegram:-100123", "assistant", SOURCE_DERIVED, 12)
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

