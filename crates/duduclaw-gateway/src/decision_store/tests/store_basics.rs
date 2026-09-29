use super::*;

    #[test]
    fn ticket_score_v3_migration_scrubs_previously_revoked_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let source = "a".repeat(64);
        store
            .put(
                &scope,
                "outcome_model_candidate_score",
                "legacy-score",
                &serde_json::json!({
                    "ticket_source_sha256": source, "status": "exploratory", "sla_error": 42,
                }),
                None,
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO revoked_decision_sources
             (tenant_id,acl,source_version,revoked_at) VALUES (?1,?2,?3,1)",
            params![scope.tenant_id, scope.acl, source],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        drop(conn);
        store.open().unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let (payload, invalidated): (String, Option<i64>) = conn
            .query_row(
                "SELECT payload_json,invalidated_at FROM decision_inputs
             WHERE tenant_id=?1 AND acl=?2 AND kind='outcome_model_candidate_score'
               AND input_id='legacy-score'",
                params![scope.tenant_id, scope.acl],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(payload.is_empty() && invalidated.is_some());
        assert!(matches!(
            store
                .get::<serde_json::Value>(&scope, "outcome_model_candidate_score", "legacy-score",),
            Err(DecisionStoreError::Revoked)
        ));
    }

    #[test]
    fn dashboard_overview_is_scoped_digest_checked_and_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant-a".into(),
            acl: "private".into(),
        };
        let other = DecisionScope {
            tenant_id: "tenant-b".into(),
            acl: "private".into(),
        };
        let source = "a".repeat(64);
        store
            .put(
                &scope,
                "outcome_model_candidate_score",
                "score-1",
                &serde_json::json!({
                    "status": "exploratory", "candidate_id": "candidate-1",
                    "snapshot_id": "snapshot-1", "scenario_id": "scenario-1",
                    "outcome_id": "outcome-1", "replay_hash": "replay-1",
                    "ticket_source_sha256": source,
                    "raw_ticket_text": "must-never-leave-store"
                }),
                None,
            )
            .unwrap();
        store
            .put(
                &other,
                "model",
                "other-model",
                &serde_json::json!({
                    "status": "other", "raw_ticket_text": "other-tenant-secret"
                }),
                None,
            )
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO decision_ticket_source_blobs
             (tenant_id,acl,source_sha256,source_bytes,retention_until)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                scope.tenant_id,
                scope.acl,
                "b".repeat(64),
                b"raw-source".as_slice(),
                chrono::Utc::now().timestamp() + 3600
            ],
        )
        .unwrap();

        let overview = store.dashboard_overview(&scope, 20).unwrap();
        assert_eq!(overview.status, "exploratory");
        assert_eq!(overview.counts["outcome_model_candidate_score"], 1);
        assert_eq!(overview.ticket_sources.active, 1);
        assert_eq!(overview.artifacts.len(), 1);
        assert!(overview.artifacts[0].ticket_backed);
        assert_eq!(
            overview.artifacts[0].candidate_id.as_deref(),
            Some("candidate-1")
        );
        let encoded = serde_json::to_string(&overview).unwrap();
        assert!(!encoded.contains("must-never-leave-store"));
        assert!(!encoded.contains("raw-source"));
        assert!(!encoded.contains("other-tenant-secret"));
        assert!(matches!(
            store.dashboard_overview(&scope, 0),
            Err(DecisionStoreError::Invalid)
        ));
    }

    #[test]
    fn sla_day_boundary_rejects_replaced_opening_and_prior_resolution_ids() {
        let previous = ShadowSlaObservationExport {
            queue_id: "support".into(),
            target_day_utc: "2025-01-01T00:00:00Z".into(),
            observed_through_utc: "2025-01-02T00:00:00Z".into(),
            tickets: vec![
                TicketEvent {
                    queue_id: Some("support".into()),
                    ticket_id: "open".into(),
                    created_at_utc: "2024-12-31T00:00:00Z".into(),
                    resolved_at_utc: None,
                },
                TicketEvent {
                    queue_id: Some("support".into()),
                    ticket_id: "done".into(),
                    created_at_utc: "2025-01-01T00:00:00Z".into(),
                    resolved_at_utc: Some("2025-01-01T12:00:00Z".into()),
                },
            ],
        };
        let opening = ShadowSlaOpeningExport {
            inputs: KnownSlaDayInputs {
                target_day_utc: "2025-01-02T00:00:00Z".into(),
                queue_id: Some("support".into()),
                opening_cohorts: vec![],
                known: KnownDayInputs {
                    opening_backlog: 1,
                    planned_agents: 1,
                    planned_fixed_extra_capacity: 0,
                },
            },
            opening_tickets: vec![ShadowSlaOpeningTicket {
                ticket_id: "open".into(),
                created_at_utc: "2024-12-31T00:00:00Z".into(),
            }],
            prior_resolved_tickets: vec![previous.tickets[1].clone()],
        };
        assert!(matches!(
            shadow_sla_day_boundary_matches(&previous, &opening),
            Ok(true)
        ));
        let mut replaced_open = opening.clone();
        replaced_open.opening_tickets[0].ticket_id = "other".into();
        assert!(matches!(
            shadow_sla_day_boundary_matches(&previous, &replaced_open),
            Ok(false)
        ));
        let mut replaced_resolution = opening.clone();
        replaced_resolution.prior_resolved_tickets[0].ticket_id = "other".into();
        assert!(matches!(
            shadow_sla_day_boundary_matches(&previous, &replaced_resolution),
            Ok(false)
        ));
        let mut changed_age = opening;
        changed_age.opening_tickets[0].created_at_utc = "2025-01-01T00:00:00Z".into();
        assert!(matches!(
            shadow_sla_day_boundary_matches(&previous, &changed_age),
            Ok(false)
        ));
    }

    #[test]
    fn sla_fixed_prefix_interval_exposes_later_drift_without_recalibration() {
        let start = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let days: Vec<_> = (0..21)
            .map(|index| ShadowSlaDayAssessment {
                target_day_utc: start
                    .checked_add_days(chrono::Days::new(index))
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                aggregate_status: ShadowDayStatus::Scored,
                status: ShadowSlaDayStatus::Scored,
                aggregate_forecast_id: Some(format!("aggregate-{index}")),
                sla_forecast_id: Some(format!("sla-{index}")),
                score_revision_id: Some(format!("score-{index}")),
                score_revision_sha256: None,
                corrected: false,
                predicted_resolved_within_sla: Some(16),
                observed_resolved_within_sla: Some(if index < 14 { 16 } else { 25 }),
                abs_error: Some(if index < 14 { 0 } else { 9 }),
                no_change_abs_error: Some(25),
                seasonal_naive_abs_error: Some(25),
                seven_day_mean_abs_error: Some(25),
            })
            .collect();
        let diagnostic = diagnose_fixed_sla_intervals(&days, 14).unwrap();
        assert_eq!(diagnostic.calibration_radius, 0);
        assert_eq!(diagnostic.evaluated_points, 7);
        assert_eq!(diagnostic.recent_miss_count, 7);
        assert!(diagnostic.drift_signal);
        assert_eq!(diagnostic.observed_coverage_basis_points, 0);
        let mut changed = days.clone();
        changed[14].observed_resolved_within_sla = Some(100);
        let rescored = diagnose_fixed_sla_intervals(&changed, 14).unwrap();
        assert_eq!(
            diagnostic.points[0].lower_bound,
            rescored.points[0].lower_bound
        );
        assert_eq!(
            diagnostic.points[0].upper_bound,
            rescored.points[0].upper_bound
        );
        let mut gap = days.clone();
        gap[5].status = ShadowSlaDayStatus::Unscored;
        assert!(diagnose_fixed_sla_intervals(&gap, 14).is_err());
        assert!(diagnose_fixed_sla_intervals(&days[..20], 14).is_err());
    }

    #[test]
    fn shadow_source_preflight_rejects_zero_staff_overproduction() {
        let source = serde_json::json!({
            "queue_id": "support-queue",
            "window_start_utc": "2026-01-01T00:00:00Z",
            "observed_through_utc": "2026-01-02T00:00:00Z",
            "observed_days": [{
                "arrivals": 5, "backlog_start": 3, "resolved": 3,
                "backlog_end": 5, "agents": 0, "fixed_extra_capacity": 2
            }]
        });
        let export = parse_shadow_training_export(&source.to_string()).unwrap();
        assert!(matches!(
            shadow_export_window(&export),
            Err(DecisionStoreError::Observation(
                crate::decision_calibration::CalibrationError::InvalidObservation
            ))
        ));
    }

    #[test]
    fn outcome_window_starts_at_first_complete_utc_day_after_cutoff() {
        let midnight = chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00Z").unwrap();
        let intraday = chrono::DateTime::parse_from_rfc3339("2026-09-01T12:30:00Z").unwrap();
        assert_eq!(
            first_full_utc_day_at_or_after(midnight)
                .unwrap()
                .to_rfc3339(),
            "2026-09-01T00:00:00+00:00"
        );
        assert_eq!(
            first_full_utc_day_at_or_after(intraday)
                .unwrap()
                .to_rfc3339(),
            "2026-09-02T00:00:00+00:00"
        );
    }

    #[test]
    fn post_run_journal_refuses_observations_from_a_future_day() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let start = chrono::Utc::now()
            .date_naive()
            .checked_add_days(chrono::Days::new(1))
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let through = start + chrono::Duration::days(1);
        let snapshot = DecisionSnapshot {
            id: "future".into(),
            queue_id: Some("support-queue".into()),
            data_cutoff_utc: start.to_rfc3339(),
            source_version_hashes: vec!["source-v1".into()],
            seed: 1,
            arrivals_by_day: vec![1],
            initial_backlog: vec![],
        };
        let model = QueueModel {
            version: "model".into(),
            service_capacity_per_agent_day: 1,
            sla_days: 1,
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
            .put_daily_run(&scope, &snapshot.id, &model.version, &scenario.id)
            .unwrap();
        let export = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: start.to_rfc3339(),
            observed_through_utc: through.to_rfc3339(),
            observed_days: vec![ObservedSupportDay {
                arrivals: 1,
                backlog_start: 0,
                resolved: 1,
                backlog_end: 0,
                agents: 1,
                fixed_extra_capacity: 0,
            }],
        };
        assert!(matches!(
            store.record_observed_outcome(
                &scope,
                "future-observation",
                &snapshot.id,
                &model.version,
                &scenario.id,
                &run.replay_hash,
                "reviewer",
                &serde_json::to_vec(&export).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.get_observed_outcome(&scope, "future-observation"),
            Err(DecisionStoreError::NotFound)
        ));
    }

    #[test]
    fn shadow_policy_handoff_refuses_a_precommitted_future_target() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decision.db"));
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let start = (midnight + chrono::Duration::days(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let cutoff = (midnight + chrono::Duration::days(3))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end = (midnight + chrono::Duration::days(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let old = store
            .put_shadow_policy_at(
                &scope,
                "old",
                "queue",
                "support-queue",
                &start,
                &end,
                3_600,
                14,
                7,
                midnight.timestamp(),
            )
            .unwrap();
        store
            .reserve_shadow_target(&scope, "queue", &cutoff, "precommitted", &old.id)
            .unwrap();
        assert!(matches!(
            store.supersede_shadow_policy_at(
                &scope,
                &old.id,
                "new",
                &cutoff,
                &end,
                "reviewer",
                3_600,
                14,
                7,
                midnight.timestamp() + 10,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert_eq!(store.load_shadow_policy(&scope, &old.id).unwrap(), old);
        assert!(matches!(
            store.load_shadow_policy_supersession(&scope, &old.id),
            Err(DecisionStoreError::NotFound)
        ));
    }

    /// Regression: the ticket retention deadline had no scheduled executor, so
    /// expired source bytes stayed in the file until someone pressed a button.
    /// One sweeper iteration must scrub every scope it can reach and report
    /// the ones it cannot instead of counting them as "nothing to do".
    #[test]
    fn retention_sweep_scrubs_every_expired_scope_in_one_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decisions.db"));
        assert_eq!(
            store.sweep_expired_ticket_sources(),
            TicketRetentionSweep::default()
        );
        let now = chrono::Utc::now().timestamp();
        let conn = Connection::open(store.path()).unwrap();
        for (tenant, acl, digest, retention_until) in [
            ("tenant-a", "private", "a".repeat(64), now - 1),
            ("tenant-b", "private", "b".repeat(64), now - 86_400),
            ("tenant-b", "shared", "c".repeat(64), now + 86_400),
        ] {
            conn.execute(
                "INSERT INTO decision_ticket_source_blobs
                 (tenant_id,acl,source_sha256,source_bytes,retention_until)
                 VALUES (?1,?2,?3,?4,?5)",
                params![tenant, acl, digest, vec![1u8, 2, 3], retention_until],
            )
            .unwrap();
        }
        drop(conn);
        assert_eq!(store.ticket_source_scopes().unwrap().len(), 3);
        let report = store.sweep_expired_ticket_sources();
        assert_eq!(report.scopes, 3);
        assert_eq!(report.scrubbed, 2);
        assert!(report.failures.is_empty());
        let remaining: i64 = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM decision_ticket_source_blobs
                 WHERE invalidated_at IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1);
        let second = store.sweep_expired_ticket_sources();
        assert_eq!(second.scrubbed, 0);
        assert!(second.failures.is_empty());
    }

    /// Regression: `open()` skips the create-and-migrate write transaction on
    /// an already-current file, so the version stamp must be the only thing
    /// that unlocks the skip and a stale stamp must re-run the DDL batch.
    #[test]
    fn open_skips_schema_work_only_for_a_file_stamped_with_the_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = DecisionStore::new(dir.path().join("decision.db"));
        store.open().unwrap();
        let stamped: i64 = Connection::open(store.path())
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stamped, SCHEMA_VERSION);
        Connection::open(store.path())
            .unwrap()
            .execute_batch("DROP TABLE decision_shadow_targets; PRAGMA user_version=0;")
            .unwrap();
        store.open().unwrap();
        let rows: i64 = Connection::open(store.path())
            .unwrap()
            .query_row("SELECT count(*) FROM decision_shadow_targets", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0);
        let restamped: i64 = Connection::open(store.path())
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restamped, SCHEMA_VERSION);
    }

    /// Premise for the canonical-spelling regression below: chrono's RFC3339
    /// parser and SQLite's date functions do not accept the same spellings, so
    /// a chrono-validated day string is not automatically readable by SQL.
    #[test]
    fn chrono_accepts_utc_spellings_that_sqlite_strftime_cannot_read() {
        assert!(shadow_utc_midnight("2026-03-01t00:00:00Z").is_ok());
        assert!(shadow_utc_midnight("2026-03-01T00:00:00+00:00").is_ok());
        let conn = Connection::open_in_memory().unwrap();
        let day_seconds = |spelling: &str| -> Option<i64> {
            conn.query_row(
                "SELECT CAST(strftime('%s',?1) AS INTEGER)",
                params![spelling],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(day_seconds("2026-03-01t00:00:00Z"), None);
        assert!(day_seconds("2026-03-01T00:00:00Z").is_some());
        assert_eq!(
            shadow_utc_day_key("2026-03-01t00:00:00Z").unwrap(),
            "2026-03-01T00:00:00Z"
        );
        assert_eq!(
            shadow_utc_day_key("2026-03-01T00:00:00+00:00").unwrap(),
            "2026-03-01T00:00:00Z"
        );
    }

