use super::*;

    /// Regression: a day spelling chrono accepts but SQLite cannot parse used
    /// to slip past every `strftime` day guard, so one UTC day could hold two
    /// independent frozen forecasts and a policy handoff could step over a
    /// pre-committed target it could not see.
    #[test]
    fn shadow_day_reservations_normalise_spelling_and_fail_closed_on_legacy_rows() {
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
        let end = (midnight + chrono::Duration::days(10))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let policy = store
            .put_shadow_policy_at(
                &scope,
                "policy-1",
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
        let day = (midnight + chrono::Duration::days(3))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let lowercase_separator = day.replace('T', "t");
        store
            .reserve_shadow_target(
                &scope,
                "queue",
                &lowercase_separator,
                "alias-first",
                &policy.id,
            )
            .unwrap();
        let stored_day: String = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT target_day_utc FROM decision_shadow_targets
                 WHERE forecast_id='alias-first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_day, day);
        assert!(matches!(
            store.reserve_shadow_target(&scope, "queue", &day, "canonical-second", &policy.id),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.reserve_shadow_target(
                &scope,
                "queue",
                &day.replace('Z', "+00:00"),
                "offset-alias-second",
                &policy.id,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        // A row written before normalisation is unreadable to SQLite's day
        // arithmetic. It must fail closed instead of dropping out of the guard.
        Connection::open(store.path())
            .unwrap()
            .execute(
                "UPDATE decision_shadow_targets SET target_day_utc=?1
                 WHERE forecast_id='alias-first'",
                params![lowercase_separator],
            )
            .unwrap();
        assert!(matches!(
            store.reserve_shadow_target(&scope, "queue", &day, "canonical-third", &policy.id),
            Err(DecisionStoreError::Corrupt)
        ));
        let cutoff = (midnight + chrono::Duration::days(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(matches!(
            store.supersede_shadow_policy_at(
                &scope,
                &policy.id,
                "policy-2",
                &cutoff,
                &end,
                "reviewer",
                3_600,
                14,
                7,
                midnight.timestamp() + 10,
            ),
            Err(DecisionStoreError::Corrupt)
        ));
        let forecast = StoredShadowForecast {
            id: "forecast-1".into(),
            policy_id: policy.id.clone(),
            policy_sha256: String::new(),
            training_artifact_id: String::new(),
            training_sha256: String::new(),
            source_lineage: "queue".into(),
            queue_id: Some("support-queue".into()),
            training_window_start_utc: start.clone(),
            target_day_utc: (midnight + chrono::Duration::days(4))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            committed_at: midnight.timestamp(),
            min_saturated_days: 7,
            known: KnownDayInputs {
                opening_backlog: 0,
                planned_agents: 1,
                planned_fixed_extra_capacity: 0,
            },
            calibration_engine_sha256: calibration_engine_sha256(),
            forecast: ProspectiveForecast {
                predicted_arrivals: 0,
                predicted_backlog_end: 0,
                no_change_backlog_end: 0,
                seasonal_naive_backlog_end: 0,
                mean_change_backlog_end: 0,
                fitted_capacity_per_agent: 1,
                training_days: 14,
            },
        };
        assert!(matches!(
            store.validate_shadow_sla_prior_boundary(&scope, &forecast, "{}", midnight.timestamp()),
            Err(DecisionStoreError::Corrupt)
        ));
    }

