use super::*;

    #[test]
    fn outcome_calibration_is_new_versioned_candidate_and_source_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "forecast".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["forecast-source".into()],
            seed: 1,
            arrivals_by_day: vec![5; 7],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model-v1".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1; 7],
            fixed_extra_capacity_by_day: vec![0; 7],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let run = store
            .put_daily_run(&scope, "forecast", "model-v1", "base")
            .unwrap();
        let observed_days = (0..7)
            .map(|day| ObservedSupportDay {
                arrivals: 5,
                backlog_start: day * 2,
                resolved: 3,
                backlog_end: (day + 1) * 2,
                agents: 1,
                fixed_extra_capacity: 0,
            })
            .collect();
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            observed_through_utc: "2026-09-08T00:00:00Z".into(),
            observed_days,
        };
        let outcome = store
            .record_observed_outcome(
                &scope,
                "obs-v1",
                "forecast",
                "model-v1",
                "base",
                &run.replay_hash,
                "reviewer",
                &serde_json::to_vec(&export).unwrap(),
            )
            .unwrap();
        let fit = store
            .put_outcome_calibration(&scope, "fit-v2", "obs-v1", 3, 4)
            .unwrap();
        assert_eq!(fit.fit.service_per_agent_day, 3);
        assert_eq!(fit.fit.saturated_days, 4);
        let holdout = fit.holdout.as_ref().unwrap();
        assert_eq!(holdout.holdout_days, 3);
        assert_eq!(holdout.candidate_abs_error_sum, 0);
        assert_eq!(holdout.parent_abs_error_sum, 3);
        assert!(holdout.candidate_beats_parent);
        assert_eq!(
            store.load_outcome_calibration(&scope, "fit-v2").unwrap(),
            fit
        );
        let review_criteria = crate::decision_model_review::OutcomeModelReviewCriteria {
            min_saturated_days: 3,
            min_holdout_days: 3,
        };
        let historical_screen = store
            .screen_outcome_model_candidate(&scope, "fit-v2", &review_criteria)
            .unwrap();
        assert!(!historical_screen.eligible_for_human_review);
        assert!(
            historical_screen
                .failed_checks
                .contains(&"run_not_committed_before_observation_window".to_owned())
        );
        let fit_digest = store
            .get_with_digest::<StoredOutcomeCalibration>(&scope, "outcome_calibration", "fit-v2")
            .unwrap()
            .1;
        let mut prospectively_committed = outcome.clone();
        prospectively_committed.local_run_precedes_window = Some(true);
        let prospective_screen = crate::decision_model_review::screen_outcome_model_review(
            &fit,
            &fit_digest,
            &prospectively_committed,
            &review_criteria,
        )
        .unwrap();
        assert!(prospective_screen.eligible_for_human_review);
        assert!(prospective_screen.failed_checks.is_empty());
        assert_ne!(
            prospective_screen.replay_hash,
            historical_screen.replay_hash
        );
        let stricter_screen = crate::decision_model_review::screen_outcome_model_review(
            &fit,
            &fit_digest,
            &prospectively_committed,
            &crate::decision_model_review::OutcomeModelReviewCriteria {
                min_saturated_days: 5,
                min_holdout_days: 4,
            },
        )
        .unwrap();
        assert!(!stricter_screen.eligible_for_human_review);
        assert!(
            stricter_screen
                .failed_checks
                .contains(&"insufficient_saturated_training_days".to_owned())
        );
        assert!(
            stricter_screen
                .failed_checks
                .contains(&"insufficient_holdout_days".to_owned())
        );
        let calibration_conn = Connection::open(store.path()).unwrap();
        let write_calibration = |record: &StoredOutcomeCalibration| {
            let payload = serde_json::to_string(record).unwrap();
            let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
            calibration_conn
                .execute(
                    "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
                 WHERE tenant_id=?3 AND acl=?4 AND kind='outcome_calibration'
                 AND input_id='fit-v2'",
                    params![payload, digest, scope.tenant_id, scope.acl],
                )
                .unwrap();
        };
        let mut falsified = fit.clone();
        falsified.holdout.as_mut().unwrap().candidate_abs_error_sum += 1;
        write_calibration(&falsified);
        assert!(matches!(
            store.load_outcome_calibration(&scope, "fit-v2"),
            Err(DecisionStoreError::Corrupt)
        ));
        write_calibration(&fit);
        assert_eq!(
            store.load_outcome_calibration(&scope, "fit-v2").unwrap(),
            fit
        );
        assert_eq!(
            store
                .get::<QueueModel>(&scope, "model", "model-v1")
                .unwrap(),
            model
        );
        assert!(matches!(
            store.put_outcome_calibration(&scope, "fit-v2", "obs-v1", 4, 4),
            Err(DecisionStoreError::VersionConflict)
        ));
        let wrong = DecisionScope {
            tenant_id: "other".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.load_outcome_calibration(&wrong, "fit-v2"),
            Err(DecisionStoreError::NotFound)
        ));
        store
            .revoke_source_version(&scope, &outcome.observed_source_sha256)
            .unwrap();
        assert!(matches!(
            store.load_outcome_calibration(&scope, "fit-v2"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.screen_outcome_model_candidate(&scope, "fit-v2", &review_criteria,),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(store.load_daily_run(&scope, &run.replay_hash).is_ok());
    }

    #[tokio::test]
    async fn pilot_review_requires_exact_active_run_and_human_approval() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let broker = ApprovalBroker::new(std::sync::Arc::new(
            crate::approval::ApprovalStore::open_in_memory().unwrap(),
        ));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "forecast".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            arrivals_by_day: vec![3],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "base".into(),
            agents_by_day: vec![1],
            fixed_extra_capacity_by_day: vec![0],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let run = store
            .put_daily_run(&scope, "forecast", "model", "base")
            .unwrap();
        let link = store
            .request_pilot_review(
                &broker,
                &scope,
                &run.replay_hash,
                "support-agent",
                "Review synthetic pilot",
                3600,
            )
            .await
            .unwrap();
        let pending = store
            .pilot_review_status(
                &broker,
                &scope,
                &link.approval_id,
                &run.replay_hash,
                "forecast",
                "model",
                "base",
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ApprovalStatus::Pending);
        assert_eq!(pending.link, link);
        assert!(matches!(
            store
                .pilot_review_status(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &run.replay_hash,
                    "forecast",
                    "wrong-model",
                    "base",
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &link.approval_id, &run.replay_hash,)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let id = ApprovalId::from(link.approval_id.clone());
        broker.decide(&id, true, "human-reviewer").await.unwrap();
        assert_eq!(
            store
                .require_pilot_review(&broker, &scope, &link.approval_id, &run.replay_hash,)
                .await
                .unwrap(),
            link
        );
        let approved = store
            .pilot_review_status(
                &broker,
                &scope,
                &link.approval_id,
                &run.replay_hash,
                "forecast",
                "model",
                "base",
            )
            .await
            .unwrap();
        assert_eq!(approved.status, ApprovalStatus::Approved);
        assert_eq!(approved.decided_by.as_deref(), Some("human-reviewer"));
        // A DB writer can recalculate SQLite's payload digest after changing
        // a stored result. The human receipt must still bind the current
        // simulator's exact output, not merely a self-consistent row digest.
        let conn = Connection::open(store.path()).unwrap();
        let write_run = |stored: &StoredDailyRun| {
            let payload = serde_json::to_string(stored).unwrap();
            let digest = format!("{:x}", Sha256::digest(payload.as_bytes()));
            conn.execute(
                "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
                 WHERE tenant_id=?3 AND acl=?4 AND kind='daily_run' AND input_id=?5",
                params![payload, digest, scope.tenant_id, scope.acl, run.replay_hash],
            )
            .unwrap();
        };
        let mut forged = run.clone();
        forged.result.final_backlog += 1;
        write_run(&forged);
        assert_eq!(
            store.load_daily_run(&scope, &run.replay_hash).unwrap(),
            forged
        );
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &link.approval_id, &run.replay_hash)
                .await,
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store
                .pilot_review_status(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &run.replay_hash,
                    "forecast",
                    "model",
                    "base",
                )
                .await,
            Err(DecisionStoreError::VersionConflict)
        ));
        write_run(&run);
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &link.approval_id, "wrong-run",)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let other = DecisionScope {
            tenant_id: "other".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store
                .require_pilot_review(&broker, &other, &link.approval_id, &run.replay_hash,)
                .await,
            Err(DecisionStoreError::NotFound)
        ));
        let denied = store
            .request_pilot_review(
                &broker,
                &scope,
                &run.replay_hash,
                "support-agent",
                "Second review",
                3600,
            )
            .await
            .unwrap();
        broker
            .decide(
                &ApprovalId::from(denied.approval_id.clone()),
                false,
                "human-reviewer",
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &denied.approval_id, &run.replay_hash,)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert_eq!(
            store
                .pilot_review_status(
                    &broker,
                    &scope,
                    &denied.approval_id,
                    &run.replay_hash,
                    "forecast",
                    "model",
                    "base",
                )
                .await
                .unwrap()
                .status,
            ApprovalStatus::Denied
        );
        let short = store
            .request_pilot_review(
                &broker,
                &scope,
                &run.replay_hash,
                "support-agent",
                "Short review",
                1,
            )
            .await
            .unwrap();
        broker
            .decide(
                &ApprovalId::from(short.approval_id.clone()),
                true,
                "human-reviewer",
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &short.approval_id, &run.replay_hash,)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert_eq!(
            store
                .pilot_review_status(
                    &broker,
                    &scope,
                    &short.approval_id,
                    &run.replay_hash,
                    "forecast",
                    "model",
                    "base",
                )
                .await
                .unwrap()
                .status,
            ApprovalStatus::Expired
        );
        store.revoke_source_version(&scope, "source-v1").unwrap();
        assert!(matches!(
            store
                .require_pilot_review(&broker, &scope, &link.approval_id, &run.replay_hash,)
                .await,
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store
                .pilot_review_status(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &run.replay_hash,
                    "forecast",
                    "model",
                    "base",
                )
                .await,
            Err(DecisionStoreError::Revoked)
        ));
    }

