use super::*;

    #[test]
    fn observed_outcome_is_immutable_scoped_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "forecast-1".into(),
            queue_id: Some("support-queue".into()),
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["forecast-source".into()],
            seed: 1,
            arrivals_by_day: vec![3, 4],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "v1".into(),
            service_capacity_per_agent_day: 2,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        let scenario = StaffingScenario {
            id: "baseline".into(),
            agents_by_day: vec![1, 1],
            fixed_extra_capacity_by_day: vec![0, 0],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        store.put_model(&scope, &model).unwrap();
        store.put_scenario(&scope, &scenario).unwrap();
        let manifest = store
            .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
            .unwrap();
        assert_eq!(
            store.load_daily_run(&scope, &manifest.replay_hash).unwrap(),
            manifest
        );
        assert_eq!(
            store
                .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
                .unwrap(),
            manifest
        );
        let replay = manifest.result;
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: "2026-09-01T00:00:00Z".into(),
            observed_through_utc: "2026-09-03T00:00:00Z".into(),
            observed_days: vec![
                ObservedSupportDay {
                    arrivals: 3,
                    backlog_start: 0,
                    resolved: 2,
                    backlog_end: 1,
                    agents: 1,
                    fixed_extra_capacity: 0,
                },
                ObservedSupportDay {
                    arrivals: 5,
                    backlog_start: 1,
                    resolved: 2,
                    backlog_end: 4,
                    agents: 1,
                    fixed_extra_capacity: 0,
                },
            ],
        };
        let bytes = serde_json::to_vec(&export).unwrap();
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "bad-hash",
                &snapshot.id,
                &model.version,
                &scenario.id,
                "wrong",
                "reviewer-1",
                &bytes,
            ),
            Err(DecisionStoreError::NotFound)
        ));
        let mut premature = export.clone();
        premature.observed_through_utc = snapshot.data_cutoff_utc.clone();
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "premature",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&premature).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut overstated = export.clone();
        overstated.observed_through_utc = "2026-09-04T00:00:00Z".into();
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "overstated-window",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&overstated).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut selected_later_window = export.clone();
        selected_later_window.window_start_utc = "2026-09-02T00:00:00Z".into();
        selected_later_window.observed_through_utc = "2026-09-04T00:00:00Z".into();
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "selected-later-window",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&selected_later_window).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut wrong_queue = export.clone();
        wrong_queue.queue_id = Some("another-queue".into());
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "wrong-queue",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&wrong_queue).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        wrong_queue.queue_id = None;
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "missing-queue",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&wrong_queue).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut hindsight = export.clone();
        hindsight.window_start_utc = "2026-08-31T00:00:00Z".into();
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "hindsight",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&hindsight).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut wrong_staffing = export.clone();
        wrong_staffing.observed_days[1].agents = 2;
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "wrong-staffing",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&wrong_staffing).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let saved = store
            .record_observed_outcome(
                &scope,
                "observation-1",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &bytes,
            )
            .unwrap();
        assert_eq!(saved.assessment.arrivals_abs_error_sum, 1);
        assert_eq!(saved.queue_id.as_deref(), Some("support-queue"));
        assert!(saved.local_run_created_at_unix.is_some());
        assert_eq!(saved.local_run_precedes_window, Some(false));
        assert_eq!(saved.assessment.backlog_abs_error_sum, 1);
        assert_eq!(saved.assessment.observed_final_backlog, 4);
        assert_eq!(saved.observed_days, export.observed_days);
        assert!(matches!(
            store.put_outcome_calibration(&scope, "underidentified", "observation-1", 3, 3),
            Err(DecisionStoreError::Observation(
                crate::decision_calibration::CalibrationError::InsufficientHistory
            ))
        ));
        assert_eq!(
            store.get_observed_outcome(&scope, "observation-1").unwrap(),
            saved
        );
        let conn = Connection::open(store.path()).unwrap();
        let mut altered_record = saved.clone();
        altered_record.queue_id = Some("another-queue".into());
        let altered_payload = serde_json::to_string(&altered_record).unwrap();
        let altered_digest = format!("{:x}", Sha256::digest(altered_payload.as_bytes()));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='observed_outcome' AND input_id=?5",
            params![
                altered_payload,
                altered_digest,
                scope.tenant_id,
                scope.acl,
                saved.id
            ],
        )
        .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, "observation-1"),
            Err(DecisionStoreError::Corrupt)
        ));
        let original_payload = serde_json::to_string(&saved).unwrap();
        let original_digest = format!("{:x}", Sha256::digest(original_payload.as_bytes()));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='observed_outcome' AND input_id=?5",
            params![
                original_payload,
                original_digest,
                scope.tenant_id,
                scope.acl,
                saved.id
            ],
        )
        .unwrap();
        altered_record = saved.clone();
        altered_record.assessment.backlog_abs_error_sum += 1;
        let altered_payload = serde_json::to_string(&altered_record).unwrap();
        let altered_digest = format!("{:x}", Sha256::digest(altered_payload.as_bytes()));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='observed_outcome' AND input_id=?5",
            params![
                altered_payload,
                altered_digest,
                scope.tenant_id,
                scope.acl,
                saved.id
            ],
        )
        .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, "observation-1"),
            Err(DecisionStoreError::Corrupt)
        ));
        let original_payload = serde_json::to_string(&saved).unwrap();
        let original_digest = format!("{:x}", Sha256::digest(original_payload.as_bytes()));
        conn.execute(
            "UPDATE decision_inputs SET payload_json=?1,payload_sha256=?2
             WHERE tenant_id=?3 AND acl=?4 AND kind='observed_outcome' AND input_id=?5",
            params![
                original_payload,
                original_digest,
                scope.tenant_id,
                scope.acl,
                saved.id
            ],
        )
        .unwrap();
        let run_created_at = saved.local_run_created_at_unix.unwrap();
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='daily_run' AND input_id=?4",
            params![
                run_created_at + 1,
                scope.tenant_id,
                scope.acl,
                replay.replay_hash
            ],
        )
        .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, "observation-1"),
            Err(DecisionStoreError::Corrupt)
        ));
        conn.execute(
            "UPDATE decision_inputs SET created_at=?1
             WHERE tenant_id=?2 AND acl=?3 AND kind='daily_run' AND input_id=?4",
            params![
                run_created_at,
                scope.tenant_id,
                scope.acl,
                replay.replay_hash
            ],
        )
        .unwrap();
        assert_eq!(
            store
                .record_observed_outcome(
                    &scope,
                    "observation-1",
                    &snapshot.id,
                    &model.version,
                    &scenario.id,
                    &replay.replay_hash,
                    "reviewer-1",
                    &bytes,
                )
                .unwrap(),
            saved
        );
        let wrong = DecisionScope {
            tenant_id: "other".into(),
            acl: scope.acl.clone(),
        };
        assert!(matches!(
            store.get_observed_outcome(&wrong, "observation-1"),
            Err(DecisionStoreError::NotFound)
        ));
        let mut changed = export.clone();
        changed.observed_days[1].arrivals = 6;
        changed.observed_days[1].backlog_end = 5;
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "observation-1",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&changed).unwrap(),
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        store
            .revoke_source_version(&scope, &saved.observed_source_sha256)
            .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, "observation-1"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .replay(&scope, &snapshot.id, &model.version, &scenario.id)
                .is_ok()
        );
        assert!(store.load_daily_run(&scope, &replay.replay_hash).is_ok());
        store
            .record_observed_outcome(
                &scope,
                "observation-2",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &replay.replay_hash,
                "reviewer-1",
                &serde_json::to_vec(&changed).unwrap(),
            )
            .unwrap();
        store
            .revoke_source_version(&scope, "forecast-source")
            .unwrap();
        assert!(matches!(
            store.get_observed_outcome(&scope, "observation-2"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_daily_run(&scope, &replay.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
    }

    #[test]
    fn legacy_source_refs_migrate_without_cross_kind_id_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let snapshot = DecisionSnapshot {
            id: "shared-id".into(),
            queue_id: None,
            data_cutoff_utc: "2026-09-01T00:00:00Z".into(),
            source_version_hashes: vec!["snapshot-source".into()],
            seed: 1,
            arrivals_by_day: vec![1],
            initial_backlog: vec![],
        };
        store.put_snapshot(&scope, &snapshot).unwrap();
        let fit = StoredEmpiricalParameterFit {
            id: "shared-id".into(),
            snapshot_id: "other-snapshot".into(),
            snapshot_sha256: "digest".into(),
            source_version_hashes: vec!["fit-source".into()],
            engine_sha256: "engine".into(),
            fit_engine_sha256: "fit-engine".into(),
            fit: EmpiricalParameterFit {
                method: "training_prefix_empirical_resampling_v4".into(),
                training_days: 7,
                min_saturated_days: 3,
                arrival_samples: vec![1; 7],
                capacity_samples: vec![],
                saturated_day_pairs: vec![],
                unsaturated_day_lower_bounds: vec![],
                partial_saturated_day_lower_bounds: vec![],
                capacity_identification:
                    crate::decision_empirical::CapacityIdentification::Unidentified,
            },
        };
        store
            .put(
                &scope,
                "parameter_fit",
                &fit.id,
                &fit,
                Some(&fit.source_version_hashes),
            )
            .unwrap();
        store
            .put(
                &scope,
                "empirical_run",
                "shared-id",
                &serde_json::json!({ "id": "shared-id" }),
                Some(&["run-source".into()]),
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "DELETE FROM decision_derived_source_refs WHERE kind='parameter_fit'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO decision_source_refs
             (tenant_id,acl,snapshot_id,source_version) VALUES (?1,?2,?3,?4)",
            params![scope.tenant_id, scope.acl, fit.id, "fit-source"],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        drop(conn);

        store.open().unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let raw_fit_refs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM decision_source_refs WHERE source_version='fit-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_fit_refs, 0);
        let derived_fit_refs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM decision_derived_source_refs
                 WHERE kind='parameter_fit' AND source_version='fit-source'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(derived_fit_refs, 1);
        drop(conn);

        assert_eq!(
            store
                .revoke_source_version(&scope, "snapshot-source")
                .unwrap(),
            1
        );
        assert!(matches!(
            store.get::<DecisionSnapshot>(&scope, "snapshot", "shared-id"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .get::<StoredEmpiricalParameterFit>(&scope, "parameter_fit", "shared-id")
                .is_ok()
        );
        assert!(
            store
                .get::<serde_json::Value>(&scope, "empirical_run", "shared-id")
                .is_ok()
        );
        assert_eq!(
            store.revoke_source_version(&scope, "fit-source").unwrap(),
            0
        );
        assert!(matches!(
            store.get::<StoredEmpiricalParameterFit>(&scope, "parameter_fit", "shared-id"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .get::<serde_json::Value>(&scope, "empirical_run", "shared-id")
                .is_ok()
        );
        assert_eq!(
            store.revoke_source_version(&scope, "run-source").unwrap(),
            0
        );
        assert!(matches!(
            store.get::<serde_json::Value>(&scope, "empirical_run", "shared-id"),
            Err(DecisionStoreError::Revoked)
        ));
    }

