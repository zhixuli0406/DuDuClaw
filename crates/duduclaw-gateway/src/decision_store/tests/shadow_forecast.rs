use super::*;

    #[tokio::test]
    async fn shadow_forecast_is_frozen_before_separate_observation_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decision.db"), causal.clone());
        let scope = DecisionScope {
            tenant_id: "tenant".into(),
            acl: "private".into(),
        };
        let evidence_scope = EvidenceScope {
            tenant_id: scope.tenant_id.clone(),
            acl: scope.acl.clone(),
        };
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let target = midnight.timestamp();
        let target_day = midnight.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let policy_until = (midnight + chrono::Duration::days(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let policy = store
            .put_shadow_policy_at(
                &scope,
                "policy-1",
                "queue",
                "support-queue",
                &target_day,
                &policy_until,
                3_600,
                14,
                7,
                target - 86_400,
            )
            .unwrap();
        assert_eq!(
            store
                .put_shadow_policy_at(
                    &scope,
                    "policy-1",
                    "queue",
                    "support-queue",
                    &target_day,
                    &policy_until,
                    3_600,
                    14,
                    7,
                    target + 100,
                )
                .unwrap(),
            policy
        );
        assert!(matches!(
            store.put_shadow_policy_at(
                &scope,
                "policy-1",
                "queue",
                "different-queue",
                &target_day,
                &policy_until,
                3_600,
                14,
                7,
                target + 100,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_policy_at(
                &scope,
                "policy-1",
                "queue",
                "support-queue",
                &target_day,
                &policy_until,
                1_800,
                14,
                7,
                target + 100,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_policy_at(
                &scope,
                "overlap",
                "queue",
                "support-queue",
                &target_day,
                &policy_until,
                3_600,
                14,
                7,
                target - 86_400,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_policy_at(
                &scope,
                "late-policy",
                "another-queue",
                "support-queue",
                &target_day,
                &policy_until,
                3_600,
                14,
                7,
                target + 1,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let training_start = (midnight - chrono::Duration::days(14))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut backlog = 10_u64;
        let mut training_days = Vec::new();
        for _ in 0..14 {
            training_days.push(ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + 4,
                agents: 2,
                fixed_extra_capacity: 0,
            });
            backlog += 4;
        }
        let training = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: training_start,
            observed_through_utc: target_day.clone(),
            observed_days: training_days,
        };
        let mut smuggled_training = serde_json::to_value(&training).unwrap();
        smuggled_training["observed_days"][13]["target_day_arrivals"] = serde_json::json!(999);
        assert!(
            validate_shadow_training_source(
                &serde_json::to_string(&smuggled_training).unwrap(),
                &target_day,
                &policy,
                &KnownDayInputs {
                    opening_backlog: backlog,
                    planned_agents: 2,
                    planned_fixed_extra_capacity: 0
                },
            )
            .is_err()
        );
        let smuggled_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_training_export",
                "smuggled",
                "v1",
                "queue",
                &serde_json::to_string(&smuggled_training).unwrap(),
                target,
                i64::MAX,
            )
            .unwrap();
        let training_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_training_export",
                "training",
                "v1",
                "queue",
                &serde_json::to_string(&training).unwrap(),
                target,
                i64::MAX,
            )
            .unwrap();
        let mut wrong_queue_training = training.clone();
        wrong_queue_training.queue_id = Some("different-queue".into());
        let wrong_queue_training_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_training_export",
                "wrong-queue-training",
                "v1",
                "queue",
                &serde_json::to_string(&wrong_queue_training).unwrap(),
                target,
                i64::MAX,
            )
            .unwrap();
        let conn = Connection::open(causal.path()).unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![target + 60, training_artifact.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![target + 60, wrong_queue_training_artifact.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![target + 60, smuggled_artifact.id],
        )
        .unwrap();
        let known = KnownDayInputs {
            opening_backlog: backlog,
            planned_agents: 2,
            planned_fixed_extra_capacity: 0,
        };
        assert!(matches!(
            store.put_shadow_forecast_at(
                &scope,
                "wrong-queue-forecast",
                &wrong_queue_training_artifact.id,
                &target_day,
                known.clone(),
                &policy.id,
                target + 120,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.put_shadow_forecast_at(
                &scope,
                "smuggled-forecast",
                &smuggled_artifact.id,
                &target_day,
                known.clone(),
                &policy.id,
                target + 120,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.put_shadow_forecast_at(
                &scope,
                "too-late",
                &training_artifact.id,
                &target_day,
                known.clone(),
                &policy.id,
                target + 3_601,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        store
            .put(
                &scope,
                "shadow_forecast",
                "conflicting-forecast",
                &serde_json::json!({"sentinel": true}),
                None,
            )
            .unwrap();
        assert!(matches!(
            store.put_shadow_forecast_at(
                &scope,
                "conflicting-forecast",
                &training_artifact.id,
                &target_day,
                known.clone(),
                &policy.id,
                target + 120,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let reserved_after_failed_forecast: i64 = Connection::open(store.path()).unwrap()
            .query_row("SELECT count(*) FROM decision_shadow_targets WHERE forecast_id='conflicting-forecast'",
                [], |row| row.get(0)).unwrap();
        assert_eq!(reserved_after_failed_forecast, 0);
        let frozen = store
            .put_shadow_forecast_at(
                &scope,
                "forecast-1",
                &training_artifact.id,
                &target_day,
                known,
                &policy.id,
                target + 120,
            )
            .unwrap();
        assert_eq!(frozen.queue_id.as_deref(), Some("support-queue"));
        assert_eq!(frozen.forecast.training_days, 14);
        assert_eq!(frozen.forecast.predicted_arrivals, 20);
        assert_eq!(
            store.load_shadow_forecast(&scope, "forecast-1").unwrap(),
            frozen
        );
        let sla_model = QueueModel {
            version: "shadow-sla-v1".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 2,
            staff_cost_cents_per_agent_day: 100,
        };
        store.put_model(&scope, &sla_model).unwrap();
        let opening = KnownSlaDayInputs {
            target_day_utc: target_day.clone(),
            queue_id: Some("support-queue".into()),
            opening_cohorts: vec![crate::decision_sim::InitialCohort {
                age_days: 1,
                count: backlog as u32,
            }],
            known: frozen.known.clone(),
        };
        let opening_export = ShadowSlaOpeningExport {
            inputs: opening.clone(),
            opening_tickets: (0..backlog)
                .map(|n| ShadowSlaOpeningTicket {
                    ticket_id: format!("opening-{n}"),
                    created_at_utc: (midnight - chrono::Duration::days(1))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                })
                .collect(),
            prior_resolved_tickets: (0..7)
                .flat_map(|day| {
                    (0..16).map(move |n| {
                        let prior_day = midnight - chrono::Duration::days(7 - day);
                        TicketEvent {
                            queue_id: Some("support-queue".into()),
                            ticket_id: format!("prior-{day}-{n}"),
                            created_at_utc: (prior_day - chrono::Duration::days(2)
                                + chrono::Duration::seconds(100))
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            resolved_at_utc: Some(
                                (prior_day + chrono::Duration::seconds(200))
                                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                            ),
                        }
                    })
                })
                .collect(),
        };
        let opening_text = serde_json::to_string(&opening_export).unwrap();
        let mut smuggled_opening = serde_json::to_value(&opening_export).unwrap();
        smuggled_opening["inputs"]["known"]["target_day_resolved"] = serde_json::json!(999);
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&smuggled_opening).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let mut wrong_opening = opening_export.clone();
        wrong_opening.inputs.queue_id = Some("different-queue".into());
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&wrong_opening).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let mut wrong_identity = opening_export.clone();
        wrong_identity.opening_tickets[1].ticket_id =
            wrong_identity.opening_tickets[0].ticket_id.clone();
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&wrong_identity).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let mut wrong_age = opening_export.clone();
        wrong_age.opening_tickets[0].created_at_utc = target_day.clone();
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&wrong_age).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let mut missing_prior = opening_export.clone();
        missing_prior.prior_resolved_tickets.pop();
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&missing_prior).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let mut future_prior = opening_export.clone();
        future_prior.prior_resolved_tickets[0].resolved_at_utc = Some(target_day.clone());
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&future_prior).unwrap(),
                &frozen,
                &training.observed_days,
                &sla_model,
            )
            .is_err()
        );
        let opening_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_opening_export",
                "opening",
                "v1",
                "queue",
                &opening_text,
                target,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![target + 130, opening_artifact.id],
        )
        .unwrap();
        assert!(matches!(
            store.put_shadow_sla_forecast_at(
                &scope,
                "before-opening-ingest",
                &frozen.id,
                &sla_model.version,
                &opening_artifact.id,
                target + 129,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        assert!(matches!(
            store.put_shadow_sla_forecast_at(
                &scope,
                "late-sla",
                &frozen.id,
                &sla_model.version,
                &opening_artifact.id,
                target + 3_601,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        store
            .put(
                &scope,
                "shadow_sla_forecast",
                "conflicting-sla",
                &serde_json::json!({"sentinel": true}),
                None,
            )
            .unwrap();
        assert!(matches!(
            store.put_shadow_sla_forecast_at(
                &scope,
                "conflicting-sla",
                &frozen.id,
                &sla_model.version,
                &opening_artifact.id,
                target + 150,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let reserved_failed_sla: i64 = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM decision_shadow_sla_forecasts WHERE sla_id='conflicting-sla'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reserved_failed_sla, 0);
        let sla_frozen = store
            .put_shadow_sla_forecast_at(
                &scope,
                "sla-1",
                &frozen.id,
                &sla_model.version,
                &opening_artifact.id,
                target + 150,
            )
            .unwrap();
        assert_eq!(sla_frozen.prediction.backlog_forecast, frozen.forecast);
        assert_eq!(sla_frozen.prediction.predicted_resolved_within_sla, 16);
        assert_eq!(sla_frozen.baselines.no_change, 0);
        assert_eq!(sla_frozen.baselines.seasonal_naive, 0);
        assert_eq!(sla_frozen.baselines.seven_day_mean, 0);
        assert_eq!(
            store
                .load_shadow_sla_forecast(&scope, &sla_frozen.id)
                .unwrap(),
            sla_frozen
        );
        assert!(matches!(
            store.put_shadow_sla_forecast_at(
                &scope,
                "alternate-sla",
                &frozen.id,
                &sla_model.version,
                &opening_artifact.id,
                target + 151,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let cutoff = (midnight + chrono::Duration::days(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let handoff = store
            .supersede_shadow_policy_at(
                &scope,
                &policy.id,
                "policy-2",
                &cutoff,
                &policy_until,
                "reviewer-1",
                1_800,
                14,
                7,
                target + 121,
            )
            .unwrap();
        assert_eq!(handoff.new_policy_id, "policy-2");
        assert_eq!(
            store
                .load_shadow_policy(&scope, "policy-2")
                .unwrap()
                .queue_id,
            policy.queue_id
        );
        assert_eq!(
            store
                .supersede_shadow_policy_at(
                    &scope,
                    &policy.id,
                    "policy-2",
                    &cutoff,
                    &policy_until,
                    "reviewer-1",
                    1_800,
                    14,
                    7,
                    target + 122,
                )
                .unwrap(),
            handoff
        );
        assert_eq!(
            store
                .supersede_shadow_policy_at(
                    &scope,
                    &policy.id,
                    "policy-2",
                    &cutoff,
                    &policy_until,
                    "reviewer-1",
                    1_800,
                    14,
                    7,
                    target + 3 * 86_400,
                )
                .unwrap(),
            handoff
        );
        assert!(matches!(
            store.reserve_shadow_target(&scope, "queue", &cutoff, "old-after-cutoff", &policy.id,),
            Err(DecisionStoreError::VersionConflict)
        ));
        store
            .reserve_shadow_target(&scope, "queue", &cutoff, "new-at-cutoff", "policy-2")
            .unwrap();
        let alternate_offset_spelling = cutoff.replace('Z', "+00:00");
        assert!(matches!(
            store.reserve_shadow_target(
                &scope,
                "queue",
                &alternate_offset_spelling,
                "duplicate-utc-day",
                "policy-2",
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert_eq!(
            store.load_shadow_forecast(&scope, "forecast-1").unwrap(),
            frozen
        );
        assert!(matches!(
            store.put_shadow_forecast_at(
                &scope,
                "alternate-forecast",
                &training_artifact.id,
                &target_day,
                frozen.known.clone(),
                &policy.id,
                target + 121,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let end = target + 86_400;
        let observation = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: target_day,
            observed_through_utc: (midnight + chrono::Duration::days(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_days: vec![ObservedSupportDay {
                arrivals: 22,
                backlog_start: backlog,
                resolved: 16,
                backlog_end: backlog + 6,
                agents: 2,
                fixed_extra_capacity: 0,
            }],
        };
        assert_eq!(
            validate_shadow_observation_source(
                &serde_json::to_string(&observation).unwrap(),
                &frozen,
            )
            .unwrap(),
            observation.observed_days[0]
        );
        let mut smuggled_observation = serde_json::to_value(&observation).unwrap();
        smuggled_observation["observed_days"][0]["future_arrivals"] = serde_json::json!(999);
        assert!(
            validate_shadow_observation_source(
                &serde_json::to_string(&smuggled_observation).unwrap(),
                &frozen,
            )
            .is_err()
        );
        let observation_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "observation",
                "v1",
                "queue",
                &serde_json::to_string(&observation).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        assert!(
            store
                .put_shadow_score(&scope, "score-1", &frozen.id, &observation_artifact.id)
                .is_err()
        );
        // Advance only the test fixture's ingestion clock; production uses the
        // causal store's actual ingestion time and the current wall clock.
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 1, observation_artifact.id],
        )
        .unwrap();
        let other_queue = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "other-observation",
                "v1",
                "another-queue",
                &serde_json::to_string(&observation).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 1, other_queue.id],
        )
        .unwrap();
        assert!(matches!(
            store.put_shadow_score_at(
                &scope,
                "wrong-queue-score",
                &frozen.id,
                &other_queue.id,
                end + 2,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let mut mislabeled_observation = observation.clone();
        mislabeled_observation.queue_id = Some("different-queue".into());
        let mislabeled_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "mislabeled-observation",
                "v1",
                "queue",
                &serde_json::to_string(&mislabeled_observation).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 1, mislabeled_artifact.id],
        )
        .unwrap();
        assert!(matches!(
            store.put_shadow_score_at(
                &scope,
                "mislabeled-score",
                &frozen.id,
                &mislabeled_artifact.id,
                end + 2,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        store
            .put(
                &scope,
                "shadow_score",
                "conflicting-score",
                &serde_json::json!({"sentinel": true}),
                None,
            )
            .unwrap();
        assert!(matches!(
            store.put_shadow_score_at(
                &scope,
                "conflicting-score",
                &frozen.id,
                &observation_artifact.id,
                end + 2,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let reserved_after_failed_score: i64 = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM decision_shadow_scores WHERE score_id='conflicting-score'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reserved_after_failed_score, 0);
        let scored = store
            .put_shadow_score_at(
                &scope,
                "score-1",
                &frozen.id,
                &observation_artifact.id,
                end + 2,
            )
            .unwrap();
        assert_eq!(scored.arrivals_abs_error, 2);
        assert_eq!(scored.backlog_abs_error, 2);
        assert_eq!(store.load_shadow_score(&scope, "score-1").unwrap(), scored);
        let mut tickets: Vec<TicketEvent> = opening_export
            .opening_tickets
            .iter()
            .enumerate()
            .map(|(n, ticket)| TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: ticket.ticket_id.clone(),
                created_at_utc: ticket.created_at_utc.clone(),
                resolved_at_utc: (n < 16).then(|| {
                    (midnight + chrono::Duration::seconds(300))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
            })
            .collect();
        tickets.extend((0..22).map(|n| {
            TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: format!("arrival-{n}"),
                created_at_utc: (midnight + chrono::Duration::seconds(100))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                resolved_at_utc: None,
            }
        }));
        let ticket_observation = ShadowSlaObservationExport {
            queue_id: "support-queue".into(),
            target_day_utc: sla_frozen.target_day_utc.clone(),
            observed_through_utc: observation.observed_through_utc.clone(),
            tickets,
        };
        let ticket_text = serde_json::to_string(&ticket_observation).unwrap();
        assert_eq!(
            store
                .preview_shadow_sla_score(&scope, &sla_frozen.id, &scored.id, &ticket_text)
                .unwrap(),
            16
        );
        let mut missing_ticket = ticket_observation.clone();
        missing_ticket.tickets.remove(0);
        assert!(
            store
                .preview_shadow_sla_score(
                    &scope,
                    &sla_frozen.id,
                    &scored.id,
                    &serde_json::to_string(&missing_ticket).unwrap()
                )
                .is_err()
        );
        let mut swapped_ticket = ticket_observation.clone();
        swapped_ticket.tickets[0].ticket_id = "unknown-opening".into();
        assert!(
            store
                .preview_shadow_sla_score(
                    &scope,
                    &sla_frozen.id,
                    &scored.id,
                    &serde_json::to_string(&swapped_ticket).unwrap()
                )
                .is_err()
        );
        let mut future_resolution = ticket_observation.clone();
        future_resolution.tickets[0].resolved_at_utc =
            Some(observation.observed_through_utc.clone());
        assert!(
            store
                .preview_shadow_sla_score(
                    &scope,
                    &sla_frozen.id,
                    &scored.id,
                    &serde_json::to_string(&future_resolution).unwrap()
                )
                .is_err()
        );
        let ticket_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_observation_export",
                "ticket-observation",
                "v1",
                "queue",
                &ticket_text,
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 2, ticket_artifact.id],
        )
        .unwrap();
        let sla_scored = store
            .put_shadow_sla_score_at(
                &scope,
                "sla-score-1",
                &sla_frozen.id,
                &scored.id,
                &ticket_artifact.id,
                end + 3,
            )
            .unwrap();
        assert_eq!(sla_scored.observed_resolved_within_sla, 16);
        assert_eq!(sla_scored.abs_error, 0);
        assert_eq!(sla_scored.no_change_abs_error, 16);
        assert_eq!(sla_scored.seasonal_naive_abs_error, 16);
        assert_eq!(sla_scored.seven_day_mean_abs_error, 16);
        assert_eq!(
            store.load_shadow_sla_score(&scope, &sla_scored.id).unwrap(),
            sla_scored
        );
        let initial_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 3)
            .unwrap();
        assert!(initial_sla_assessment.complete);
        assert_eq!(initial_sla_assessment.due_days, 1);
        assert_eq!(initial_sla_assessment.total_abs_error, Some(0));
        assert_eq!(initial_sla_assessment.no_change_total_abs_error, Some(16));
        assert_eq!(
            initial_sla_assessment.model_abs_error_below_each_baseline,
            Some(true)
        );
        assert!(initial_sla_assessment.fixed_prefix_interval.is_none());
        assert!(initial_sla_assessment.recent_7_day_error_sums.is_none());
        assert_eq!(
            initial_sla_assessment.days[0].status,
            ShadowSlaDayStatus::Scored
        );
        let saved_sla_screen = store
            .put_sla_shadow_review_screen_at(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 21,
                    min_fixed_coverage_bps: 8_000,
                },
                end + 3,
            )
            .unwrap();
        assert!(!saved_sla_screen.report.eligible_for_human_review);
        assert!(
            saved_sla_screen
                .report
                .failed_checks
                .contains(&"insufficient_complete_days".to_owned())
        );
        assert_eq!(
            store
                .load_sla_shadow_review_screen(&scope, &saved_sla_screen.replay_hash)
                .unwrap(),
            saved_sla_screen
        );
        let broker = ApprovalBroker::new(std::sync::Arc::new(
            crate::approval::ApprovalStore::open_in_memory().unwrap(),
        ));
        assert!(matches!(
            store
                .request_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &saved_sla_screen.replay_hash,
                    "support-agent",
                    "Inspect an ineligible SLA screen",
                    3_600,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        // A second scored day first preserves every ticket identity. A later
        // correction to day one must invalidate this boundary assessment.
        let next_target_day = observation.observed_through_utc.clone();
        let next_end = end + 86_400;
        let mut next_training_days = training.observed_days.clone();
        next_training_days.push(observation.observed_days[0].clone());
        let next_training = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: training.window_start_utc.clone(),
            observed_through_utc: next_target_day.clone(),
            observed_days: next_training_days,
        };
        let next_training_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_training_export",
                "training-next",
                "v1",
                "queue",
                &serde_json::to_string(&next_training).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 60, next_training_artifact.id],
        )
        .unwrap();
        let next_known = KnownDayInputs {
            opening_backlog: observation.observed_days[0].backlog_end,
            planned_agents: 2,
            planned_fixed_extra_capacity: 0,
        };
        let next_forecast = store
            .put_shadow_forecast_at(
                &scope,
                "forecast-next",
                &next_training_artifact.id,
                &next_target_day,
                next_known.clone(),
                &policy.id,
                end + 120,
            )
            .unwrap();
        let next_opening_tickets: Vec<ShadowSlaOpeningTicket> = ticket_observation
            .tickets
            .iter()
            .filter(|ticket| ticket.resolved_at_utc.is_none())
            .map(|ticket| ShadowSlaOpeningTicket {
                ticket_id: ticket.ticket_id.clone(),
                created_at_utc: ticket.created_at_utc.clone(),
            })
            .collect();
        assert_eq!(
            next_opening_tickets.len() as u64,
            next_known.opening_backlog
        );
        let mut next_prior_resolved: Vec<TicketEvent> = opening_export
            .prior_resolved_tickets
            .iter()
            .filter(|ticket| {
                chrono::DateTime::parse_from_rfc3339(ticket.resolved_at_utc.as_deref().unwrap())
                    .unwrap()
                    .timestamp()
                    >= target - 6 * 86_400
            })
            .cloned()
            .collect();
        next_prior_resolved.extend(
            ticket_observation
                .tickets
                .iter()
                .filter(|ticket| ticket.resolved_at_utc.is_some())
                .cloned(),
        );
        let next_opening = ShadowSlaOpeningExport {
            inputs: KnownSlaDayInputs {
                target_day_utc: next_target_day.clone(),
                queue_id: Some("support-queue".into()),
                opening_cohorts: vec![
                    crate::decision_sim::InitialCohort {
                        age_days: 1,
                        count: observation.observed_days[0].arrivals,
                    },
                    crate::decision_sim::InitialCohort {
                        age_days: 2,
                        count: (backlog - 16) as u32,
                    },
                ],
                known: next_known,
            },
            opening_tickets: next_opening_tickets,
            prior_resolved_tickets: next_prior_resolved,
        };
        assert!(
            validate_shadow_sla_opening_source(
                &serde_json::to_string(&next_opening).unwrap(),
                &next_forecast,
                &next_training.observed_days,
                &sla_model,
            )
            .is_ok()
        );
        assert!(matches!(
            shadow_sla_day_boundary_matches(&ticket_observation, &next_opening),
            Ok(true)
        ));
        let mut replaced_opening = next_opening.clone();
        replaced_opening.opening_tickets[0].ticket_id = "replaced-opening-id".into();
        assert!(matches!(
            store.preview_shadow_sla_forecast(
                &scope,
                &next_forecast.id,
                &sla_model.version,
                &serde_json::to_string(&replaced_opening).unwrap(),
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let replaced_opening_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_opening_export",
                "opening-next-replaced",
                "v1",
                "queue",
                &serde_json::to_string(&replaced_opening).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 130, replaced_opening_artifact.id],
        )
        .unwrap();
        assert!(matches!(
            store.put_shadow_sla_forecast_at(
                &scope,
                "sla-next-replaced",
                &next_forecast.id,
                &sla_model.version,
                &replaced_opening_artifact.id,
                end + 150,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let reserved_replaced: i64 = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM decision_shadow_sla_forecasts
                WHERE sla_id='sla-next-replaced'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reserved_replaced, 0);
        let next_opening_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_opening_export",
                "opening-next",
                "v1",
                "queue",
                &serde_json::to_string(&next_opening).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 130, next_opening_artifact.id],
        )
        .unwrap();
        let next_sla = store
            .put_shadow_sla_forecast_at(
                &scope,
                "sla-next",
                &next_forecast.id,
                &sla_model.version,
                &next_opening_artifact.id,
                end + 150,
            )
            .unwrap();
        let next_aggregate_observation = ObservedOutcomeExport {
            sla_days: None,
            resolved_within_sla_by_day: None,
            queue_id: Some("support-queue".into()),
            window_start_utc: next_target_day.clone(),
            observed_through_utc: (midnight + chrono::Duration::days(2))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            observed_days: vec![ObservedSupportDay {
                arrivals: 22,
                backlog_start: observation.observed_days[0].backlog_end,
                resolved: 16,
                backlog_end: observation.observed_days[0].backlog_end + 6,
                agents: 2,
                fixed_extra_capacity: 0,
            }],
        };
        let next_observation_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "observation-next",
                "v1",
                "queue",
                &serde_json::to_string(&next_aggregate_observation).unwrap(),
                next_end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![next_end + 1, next_observation_artifact.id],
        )
        .unwrap();
        let next_score = store
            .put_shadow_score_at(
                &scope,
                "score-next",
                &next_forecast.id,
                &next_observation_artifact.id,
                next_end + 2,
            )
            .unwrap();
        let mut next_tickets: Vec<TicketEvent> = next_opening
            .opening_tickets
            .iter()
            .enumerate()
            .map(|(index, ticket)| TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: ticket.ticket_id.clone(),
                created_at_utc: ticket.created_at_utc.clone(),
                resolved_at_utc: (index < 16).then(|| {
                    (midnight + chrono::Duration::days(1) + chrono::Duration::seconds(300))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
            })
            .collect();
        next_tickets.extend((0..22).map(|index| {
            TicketEvent {
                queue_id: Some("support-queue".into()),
                ticket_id: format!("next-arrival-{index}"),
                created_at_utc: (midnight
                    + chrono::Duration::days(1)
                    + chrono::Duration::seconds(100))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                resolved_at_utc: None,
            }
        }));
        let next_ticket_observation = ShadowSlaObservationExport {
            queue_id: "support-queue".into(),
            target_day_utc: next_target_day,
            observed_through_utc: next_aggregate_observation.observed_through_utc,
            tickets: next_tickets,
        };
        let next_ticket_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_observation_export",
                "tickets-next",
                "v1",
                "queue",
                &serde_json::to_string(&next_ticket_observation).unwrap(),
                next_end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![next_end + 2, next_ticket_artifact.id],
        )
        .unwrap();
        store
            .put_shadow_sla_score_at(
                &scope,
                "sla-score-next",
                &next_sla.id,
                &next_score.id,
                &next_ticket_artifact.id,
                next_end + 3,
            )
            .unwrap();
        let boundary_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, next_end + 3)
            .unwrap();
        assert_eq!(boundary_assessment.due_days, 2);
        assert_eq!(
            boundary_assessment.days[0].status,
            ShadowSlaDayStatus::Scored
        );
        assert_eq!(
            boundary_assessment.days[1].status,
            ShadowSlaDayStatus::Scored
        );
        assert!(boundary_assessment.complete);
        assert!(boundary_assessment.total_abs_error.is_some());
        assert!(matches!(
            store.put_shadow_sla_score_at(
                &scope,
                "alternate-sla-score",
                &sla_frozen.id,
                &scored.id,
                &ticket_artifact.id,
                end + 4,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_score_at(
                &scope,
                "alternate-score",
                &frozen.id,
                &observation_artifact.id,
                end + 2,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert_eq!(
            store.load_current_shadow_score(&scope, &frozen.id).unwrap(),
            scored
        );
        let mut corrected_observation = observation.clone();
        corrected_observation.observed_days[0].arrivals = 23;
        corrected_observation.observed_days[0].backlog_end = backlog + 7;
        let corrected_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "corrected-observation",
                "v1",
                "queue",
                &serde_json::to_string(&corrected_observation).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 3, corrected_artifact.id],
        )
        .unwrap();
        assert!(matches!(
            store.put_shadow_score_correction_at(
                &scope,
                "mislabeled-correction",
                &frozen.id,
                &scored.id,
                &mislabeled_artifact.id,
                "reviewer-1",
                "wrong queue",
                end + 4,
            ),
            Err(DecisionStoreError::Invalid)
        ));
        let decision_conn = Connection::open(store.path()).unwrap();
        decision_conn.execute(
            "INSERT INTO decision_shadow_score_revision_audit
             (tenant_id,acl,kind,revision_id,forecast_id,recorded_at,payload_sha256)
             VALUES (?1,?2,'shadow_score_correction','conflicting-correction',?3,?4,'wrong-digest')",
            params![scope.tenant_id, scope.acl, frozen.id, end + 4],
        ).unwrap();
        assert!(matches!(
            store.put_shadow_score_correction_at(
                &scope,
                "conflicting-correction",
                &frozen.id,
                &scored.id,
                &corrected_artifact.id,
                "reviewer-1",
                "verified export repair",
                end + 4,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        let linked_after_failed_correction: i64 = decision_conn.query_row(
            "SELECT count(*) FROM decision_shadow_score_correction_links WHERE correction_id='conflicting-correction'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(linked_after_failed_correction, 0);
        let payload_after_failed_correction: i64 = decision_conn.query_row(
            "SELECT count(*) FROM decision_inputs WHERE kind='shadow_score_correction' AND input_id='conflicting-correction'",
            [], |row| row.get(0)).unwrap();
        assert_eq!(payload_after_failed_correction, 0);
        let head_after_failed_correction: i64 = decision_conn
            .query_row(
                "SELECT count(*) FROM decision_shadow_score_heads WHERE forecast_id=?1",
                [&frozen.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(head_after_failed_correction, 0);
        let correction = store
            .put_shadow_score_correction_at(
                &scope,
                "correction-1",
                &frozen.id,
                &scored.id,
                &corrected_artifact.id,
                "reviewer-1",
                "verified export repair",
                end + 4,
            )
            .unwrap();
        assert_eq!(
            store
                .put_shadow_score_correction_at(
                    &scope,
                    "correction-1",
                    &frozen.id,
                    &scored.id,
                    &corrected_artifact.id,
                    "reviewer-1",
                    "verified export repair",
                    end + 5,
                )
                .unwrap(),
            correction
        );
        assert!(matches!(
            store.put_shadow_score_correction_at(
                &scope,
                "correction-1",
                &frozen.id,
                &scored.id,
                &corrected_artifact.id,
                "reviewer-1",
                "changed reason",
                end + 5,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        // Simulate an interruption after the immutable payload commit but
        // before the head/link transaction, then retry the same correction.
        let decision_conn = Connection::open(store.path()).unwrap();
        decision_conn
            .execute(
                "DELETE FROM decision_shadow_score_heads WHERE correction_id='correction-1'",
                [],
            )
            .unwrap();
        decision_conn.execute("DELETE FROM decision_shadow_score_correction_links WHERE correction_id='correction-1'", []).unwrap();
        assert_eq!(
            store
                .put_shadow_score_correction_at(
                    &scope,
                    "correction-1",
                    &frozen.id,
                    &scored.id,
                    &corrected_artifact.id,
                    "reviewer-1",
                    "verified export repair",
                    end + 5,
                )
                .unwrap(),
            correction
        );
        assert_eq!(correction.corrected_score.arrivals_abs_error, 3);
        assert_eq!(
            store.load_current_shadow_score(&scope, &frozen.id).unwrap(),
            correction.corrected_score
        );
        assert_eq!(store.load_shadow_score(&scope, &scored.id).unwrap(), scored);
        assert_eq!(
            store
                .load_sla_shadow_review_screen(&scope, &saved_sla_screen.replay_hash)
                .unwrap(),
            saved_sla_screen
        );
        assert!(matches!(
            store.load_current_shadow_sla_score(&scope, &sla_frozen.id),
            Err(DecisionStoreError::VersionConflict)
        ));
        let stale_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 4)
            .unwrap();
        assert!(!stale_sla_assessment.complete);
        assert_eq!(stale_sla_assessment.total_abs_error, None);
        assert_eq!(
            stale_sla_assessment.model_abs_error_below_each_baseline,
            None
        );
        assert_eq!(
            stale_sla_assessment.days[0].status,
            ShadowSlaDayStatus::ScoreStale
        );
        assert!(
            store
                .preview_shadow_sla_score_correction(
                    &scope,
                    &sla_frozen.id,
                    &sla_scored.id,
                    &correction.id,
                    &ticket_text,
                )
                .is_err()
        );
        let mut repaired_tickets = ticket_observation.clone();
        repaired_tickets.tickets.push(TicketEvent {
            queue_id: Some("support-queue".into()),
            ticket_id: "arrival-22".into(),
            created_at_utc: (midnight + chrono::Duration::seconds(100))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            resolved_at_utc: None,
        });
        let repaired_ticket_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_observation_export",
                "repaired-tickets",
                "v1",
                "queue",
                &serde_json::to_string(&repaired_tickets).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 4, repaired_ticket_artifact.id],
        )
        .unwrap();
        let sla_correction = store
            .put_shadow_sla_score_correction_at(
                &scope,
                "sla-correction-1",
                &sla_frozen.id,
                &sla_scored.id,
                &correction.id,
                &repaired_ticket_artifact.id,
                "reviewer-1",
                "verified ticket repair",
                end + 5,
            )
            .unwrap();
        assert_eq!(
            store
                .put_shadow_sla_score_correction_at(
                    &scope,
                    "sla-correction-1",
                    &sla_frozen.id,
                    &sla_scored.id,
                    &correction.id,
                    &repaired_ticket_artifact.id,
                    "reviewer-1",
                    "verified ticket repair",
                    end + 6,
                )
                .unwrap(),
            sla_correction
        );
        assert_eq!(
            store
                .load_current_shadow_sla_score(&scope, &sla_frozen.id)
                .unwrap(),
            sla_correction.corrected_score
        );
        let repaired_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 5)
            .unwrap();
        assert!(repaired_sla_assessment.complete);
        assert_eq!(repaired_sla_assessment.corrected_days, 1);
        let changed_boundary = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, next_end + 3)
            .unwrap();
        assert_eq!(
            changed_boundary.days[1].status,
            ShadowSlaDayStatus::CrossDayIdentityMismatch
        );
        assert!(!changed_boundary.complete);
        assert!(changed_boundary.total_abs_error.is_none());
        assert!(matches!(
            store.put_shadow_sla_score_correction_at(
                &scope,
                "stale-sla-correction",
                &sla_frozen.id,
                &sla_scored.id,
                &correction.id,
                &repaired_ticket_artifact.id,
                "reviewer-1",
                "stale predecessor",
                end + 6,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_sla_score_correction_at(
                &scope,
                &sla_scored.id,
                &sla_frozen.id,
                &sla_correction.id,
                &correction.id,
                &repaired_ticket_artifact.id,
                "reviewer-1",
                "reused initial ID",
                end + 6,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        assert!(matches!(
            store.put_shadow_score_correction_at(
                &scope,
                "stale-correction",
                &frozen.id,
                &scored.id,
                &corrected_artifact.id,
                "reviewer-1",
                "stale base",
                end + 5,
            ),
            Err(DecisionStoreError::VersionConflict)
        ));
        causal
            .invalidate_artifact(&evidence_scope, &observation_artifact.id)
            .unwrap();
        assert!(matches!(
            store.load_shadow_score(&scope, "score-1"),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store.load_sla_shadow_review_screen(&scope, &saved_sla_screen.replay_hash),
            Err(DecisionStoreError::Revoked)
        ));
        assert_eq!(
            store.load_current_shadow_score(&scope, &frozen.id).unwrap(),
            correction.corrected_score
        );
        assert_eq!(
            store
                .load_current_shadow_sla_score(&scope, &sla_frozen.id)
                .unwrap(),
            sla_correction.corrected_score
        );
        causal
            .invalidate_artifact(&evidence_scope, &corrected_artifact.id)
            .unwrap();
        assert!(matches!(
            store.load_current_shadow_score(&scope, &frozen.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .load_current_shadow_sla_score(&scope, &sla_frozen.id)
                .is_err()
        );
        let mut corrected_again = observation.clone();
        corrected_again.observed_days[0].arrivals = 24;
        corrected_again.observed_days[0].backlog_end = backlog + 8;
        let second_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_observation_export",
                "corrected-observation-2",
                "v1",
                "queue",
                &serde_json::to_string(&corrected_again).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 5, second_artifact.id],
        )
        .unwrap();
        let second = store
            .put_shadow_score_correction_at(
                &scope,
                "correction-2",
                &frozen.id,
                &correction.id,
                &second_artifact.id,
                "reviewer-2",
                "second verified repair",
                end + 6,
            )
            .unwrap();
        assert_eq!(
            store.load_current_shadow_score(&scope, &frozen.id).unwrap(),
            second.corrected_score
        );
        assert!(matches!(
            store.load_current_shadow_sla_score(&scope, &sla_frozen.id),
            Err(DecisionStoreError::Revoked)
        ));
        let mut second_repaired_tickets = repaired_tickets.clone();
        second_repaired_tickets.tickets.push(TicketEvent {
            queue_id: Some("support-queue".into()),
            ticket_id: "arrival-23".into(),
            created_at_utc: (midnight + chrono::Duration::seconds(100))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            resolved_at_utc: None,
        });
        let second_ticket_artifact = causal
            .add_artifact(
                &evidence_scope,
                "shadow_sla_observation_export",
                "repaired-tickets-2",
                "v1",
                "queue",
                &serde_json::to_string(&second_repaired_tickets).unwrap(),
                end,
                i64::MAX,
            )
            .unwrap();
        conn.execute(
            "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
            params![end + 6, second_ticket_artifact.id],
        )
        .unwrap();
        let second_sla_correction = store
            .put_shadow_sla_score_correction_at(
                &scope,
                "sla-correction-2",
                &sla_frozen.id,
                &sla_correction.id,
                &second.id,
                &second_ticket_artifact.id,
                "reviewer-2",
                "second verified ticket repair",
                end + 7,
            )
            .unwrap();
        assert_eq!(
            store
                .load_current_shadow_sla_score(&scope, &sla_frozen.id)
                .unwrap(),
            second_sla_correction.corrected_score
        );
        let second_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 7)
            .unwrap();
        assert!(second_sla_assessment.complete);
        assert_eq!(second_sla_assessment.total_abs_error, Some(0));
        assert_eq!(
            second_sla_assessment.model_abs_error_below_each_baseline,
            Some(true)
        );
        let complete_assessment = store
            .assess_shadow_policy_at(&scope, &policy.id, end + 7)
            .unwrap();
        assert_eq!(
            complete_assessment.queue_id.as_deref(),
            Some("support-queue")
        );
        assert_eq!(complete_assessment.due_days, 1);
        assert_eq!(complete_assessment.scored_days, 1);
        assert_eq!(complete_assessment.corrected_days, 1);
        assert!(complete_assessment.complete);
        assert_eq!(complete_assessment.error_sums.as_ref().unwrap().arrivals, 4);
        assert!(
            complete_assessment
                .backlog_abs_error_below_each_baseline
                .is_some()
        );
        assert!(complete_assessment.fixed_prefix_interval.is_none());
        let two_day_aggregate_assessment = store
            .assess_shadow_policy_at(&scope, &policy.id, end + 86_400 + 7)
            .unwrap();
        assert_eq!(two_day_aggregate_assessment.due_days, 2);
        assert_eq!(two_day_aggregate_assessment.scored_days, 2);
        assert!(two_day_aggregate_assessment.complete);
        assert!(two_day_aggregate_assessment.error_sums.is_some());
        assert_eq!(
            two_day_aggregate_assessment.days[1].status,
            ShadowDayStatus::Scored
        );
        let two_day_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 86_400 + 7)
            .unwrap();
        assert_eq!(two_day_sla_assessment.due_days, 2);
        assert_eq!(two_day_sla_assessment.scored_days, 1);
        assert!(!two_day_sla_assessment.complete);
        assert_eq!(two_day_sla_assessment.total_abs_error, None);
        assert_eq!(
            two_day_sla_assessment.model_abs_error_below_each_baseline,
            None
        );
        assert_eq!(
            two_day_sla_assessment.days[1].status,
            ShadowSlaDayStatus::CrossDayIdentityMismatch
        );
        assert_eq!(
            store.load_shadow_forecast(&scope, "forecast-1").unwrap(),
            frozen
        );
        causal
            .invalidate_artifact(&evidence_scope, &ticket_artifact.id)
            .unwrap();
        assert!(matches!(
            store.load_shadow_sla_score(&scope, &sla_scored.id),
            Err(DecisionStoreError::Revoked)
        ));
        assert!(
            store
                .assess_shadow_sla_policy_at(&scope, &policy.id, end + 7)
                .unwrap()
                .complete
        );
        causal
            .invalidate_artifact(&evidence_scope, &second_ticket_artifact.id)
            .unwrap();
        let revoked_sla_assessment = store
            .assess_shadow_sla_policy_at(&scope, &policy.id, end + 7)
            .unwrap();
        assert_eq!(
            revoked_sla_assessment.days[0].status,
            ShadowSlaDayStatus::ScoreRevoked
        );
        assert_eq!(revoked_sla_assessment.total_abs_error, None);
        causal
            .invalidate_artifact(&evidence_scope, &opening_artifact.id)
            .unwrap();
        assert!(
            store
                .load_shadow_sla_forecast(&scope, &sla_frozen.id)
                .is_err()
        );
        assert!(store.load_shadow_sla_score(&scope, &sla_scored.id).is_err());
        causal
            .invalidate_artifact(&evidence_scope, &training_artifact.id)
            .unwrap();
        assert!(matches!(
            store.load_shadow_forecast(&scope, "forecast-1"),
            Err(DecisionStoreError::Revoked)
        ));
        let revoked_assessment = store
            .assess_shadow_policy_at(&scope, &policy.id, end + 7)
            .unwrap();
        assert_eq!(
            revoked_assessment.days[0].status,
            ShadowDayStatus::ForecastRevoked
        );
        assert!(revoked_assessment.error_sums.is_none());
        assert!(
            revoked_assessment
                .backlog_abs_error_below_each_baseline
                .is_none()
        );
        assert!(revoked_assessment.fixed_prefix_interval.is_none());
        assert!(matches!(
            store.load_shadow_score(&scope, "score-1"),
            Err(DecisionStoreError::Revoked)
        ));
    }

