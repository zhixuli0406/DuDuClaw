use super::*;

    #[test]
    fn complete_shadow_window_reports_fixed_prefix_drift_after_later_demand_shift() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decision.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            - chrono::Duration::days(30);
        let training_start = start - chrono::Duration::days(14);
        let policy_end = start + chrono::Duration::days(21);
        let policy = store
            .put_shadow_policy_at(
                &scope,
                "policy-drift",
                "queue",
                "support-queue",
                &start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                &policy_end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                3_600,
                14,
                7,
                start.timestamp() - 86_400,
            )
            .unwrap();
        let causal_conn = Connection::open(causal.path()).unwrap();
        let mut backlog = 10_u64;
        let mut history = Vec::new();
        for _ in 0..14 {
            history.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + 4,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog += 4;
        }
        for index in 0..21 {
            let target = start + chrono::Duration::days(index);
            let end = target + chrono::Duration::days(1);
            let training = ObservedOutcomeExport {
                sla_days: None,
                resolved_within_sla_by_day: None,
                queue_id: Some("support-queue".into()),
                window_start_utc: training_start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_through_utc: target.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_days: history.clone(),
            };
            let training_artifact = causal
                .add_artifact(
                    &evidence_scope,
                    "shadow_training_export",
                    &format!("training-{index}"),
                    "v1",
                    "queue",
                    &serde_json::to_string(&training).unwrap(),
                    target.timestamp(),
                    i64::MAX,
                )
                .unwrap();
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![target.timestamp() + 60, training_artifact.id],
                )
                .unwrap();
            let forecast = store
                .put_shadow_forecast_at(
                    &scope,
                    &format!("forecast-{index}"),
                    &training_artifact.id,
                    &target.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    KnownDayInputs {
                        opening_backlog: backlog,
                        planned_agents: 2,
                        planned_fixed_extra_capacity: 0,
                    },
                    &policy.id,
                    target.timestamp() + 120,
                )
                .unwrap();
            let arrivals = if index < 14 { 20 } else { 30 };
            let observed = ObservedSupportDay {
                arrivals,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + arrivals as u64 - 16,
                agents: 2,
                fixed_extra_capacity: 0,
            };
            let export = ObservedOutcomeExport {
                sla_days: None,
                resolved_within_sla_by_day: None,
                queue_id: Some("support-queue".into()),
                window_start_utc: target.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_through_utc: end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                observed_days: vec![observed.clone()],
            };
            let observation_artifact = causal
                .add_artifact(
                    &evidence_scope,
                    "shadow_observation_export",
                    &format!("observed-{index}"),
                    "v1",
                    "queue",
                    &serde_json::to_string(&export).unwrap(),
                    end.timestamp(),
                    i64::MAX,
                )
                .unwrap();
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![end.timestamp() + 1, observation_artifact.id],
                )
                .unwrap();
            store
                .put_shadow_score_at(
                    &scope,
                    &format!("score-{index}"),
                    &forecast.id,
                    &observation_artifact.id,
                    end.timestamp() + 2,
                )
                .unwrap();
            backlog = observed.backlog_end;
            history.push(observed);
        }
        let report = store
            .assess_shadow_policy_at(&scope, &policy.id, policy_end.timestamp() + 10)
            .unwrap();
        assert_eq!(report.queue_id.as_deref(), Some("support-queue"));
        assert_eq!(report.due_days, 21);
        assert_eq!(report.scored_days, 21);
        assert!(report.complete);
        assert!(report.backlog_abs_error_below_each_baseline.is_some());
        let interval = report.fixed_prefix_interval.as_ref().unwrap();
        assert_eq!(interval.calibration_points, 14);
        assert_eq!(interval.evaluated_points, 7);
        assert_eq!(interval.points[0].calibration_radius, 0);
        assert!(interval.drift_signal);
        let recent = report.recent_7_day_error_sums.as_ref().unwrap();
        assert_eq!(recent.arrivals, 40);
        assert_eq!(recent.backlog, 40);
        assert_eq!(
            report.recent_7_day_backlog_abs_error_below_each_baseline,
            Some(true)
        );
        let screen = crate::decision_shadow_screen::screen_shadow_assessment(
            report,
            &crate::decision_shadow_screen::ShadowReviewCriteria {
                min_complete_days: 21,
                min_fixed_coverage_bps: 0,
            },
        )
        .unwrap();
        assert!(!screen.eligible_for_human_review);
        assert!(
            screen
                .failed_checks
                .iter()
                .any(|check| check == "recent_drift_signal")
        );
        let stored_screen = store
            .put_shadow_review_screen(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 21,
                    min_fixed_coverage_bps: 0,
                },
            )
            .unwrap();
        assert!(!stored_screen.report.eligible_for_human_review);
        assert_eq!(stored_screen.source_version_hashes.len(), 42);
        assert_eq!(
            store
                .load_shadow_review_screen(&scope, &stored_screen.replay_hash)
                .unwrap(),
            stored_screen
        );
        // Regression: the screen hash binds the verdict to the stored
        // assessment, so a record written straight into the SQLite file can be
        // internally consistent while carrying a verdict this engine would
        // never produce. Loading must recompute the verdict, not trust it.
        let mut forged = stored_screen.clone();
        forged.report.failed_checks.clear();
        forged.report.eligible_for_human_review = true;
        forged.report.replay_hash =
            crate::decision_shadow_screen::screen_hash(&forged.report).unwrap();
        forged.replay_hash = forged.report.replay_hash.clone();
        assert_ne!(forged.replay_hash, stored_screen.replay_hash);
        store
            .put(
                &scope,
                "shadow_review_screen",
                &forged.replay_hash,
                &forged,
                None,
            )
            .unwrap();
        assert!(matches!(
            store.load_shadow_review_screen(&scope, &forged.replay_hash),
            Err(DecisionStoreError::Corrupt)
        ));
        let mut corrected_day = history.last().unwrap().clone();
        corrected_day.arrivals += 1;
        corrected_day.backlog_end += 1;
        let corrected_export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: (policy_end - chrono::Duration::days(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_through_utc: policy_end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_days: vec![corrected_day],
        };
        let corrected_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "observed-20-corrected",
                "v1",
                "queue",
                &serde_json::to_string(&corrected_export).unwrap(),
                policy_end.timestamp(),
                i64::MAX,
            )
            .unwrap();
        causal_conn
            .execute(
                "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                params![policy_end.timestamp() + 10, corrected_artifact.id],
            )
            .unwrap();
        let correction = store
            .put_shadow_score_correction_at(
                &scope,
                "corrected-score-20",
                "forecast-20",
                "score-20",
                &corrected_artifact.id,
                "reviewer",
                "verify that a later revision leaves the saved assessment unchanged",
                policy_end.timestamp() + 20,
            )
            .unwrap();
        assert_eq!(correction.previous_revision_id, "score-20");
        let current = store.assess_shadow_policy(&scope, &policy.id).unwrap();
        assert_eq!(current.corrected_days, 1);
        assert_ne!(
            current.error_sums,
            stored_screen.report.assessment.error_sums
        );
        assert_eq!(
            current.recent_7_day_error_sums.as_ref().unwrap().backlog,
            41
        );
        assert_eq!(
            store
                .load_shadow_review_screen(&scope, &stored_screen.replay_hash)
                .unwrap(),
            stored_screen
        );
        store
            .revoke_source_version(&scope, &stored_screen.source_version_hashes[0])
            .unwrap();
        assert!(matches!(
            store.load_shadow_review_screen(&scope, &stored_screen.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
    }

