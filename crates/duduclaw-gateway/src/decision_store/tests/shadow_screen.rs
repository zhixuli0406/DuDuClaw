use super::*;

    #[tokio::test]
    async fn shadow_screen_review_requires_passing_exact_source_bound_screen() {
        let dir = tempfile::tempdir().unwrap();
        let causal = CausalStore::new(dir.path().join("causal.db"));
        let store =
            DecisionStore::with_causal_store(dir.path().join("decision.db"), causal.clone());
        let broker = ApprovalBroker::new(std::sync::Arc::new(
            crate::approval::ApprovalStore::open_in_memory().unwrap(),
        ));
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
        let policy_end = start + chrono::Duration::days(21);
        let training_start = start - chrono::Duration::days(14);
        let policy = store
            .put_shadow_policy_at(
                &scope,
                "policy-review",
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
        // Keep all synthetic tickets inside the SLA to isolate the review
        // receipt path from age-threshold calibration.
        let sla_model = QueueModel {
            version: "review-sla-model".into(),
            service_capacity_per_agent_day: 8,
            sla_days: 1_000,
            staff_cost_cents_per_agent_day: 100,
        };
        store.put_model(&scope, &sla_model).unwrap();
        let causal_conn = Connection::open(causal.path()).unwrap();
        let mut backlog = 100_u64;
        let mut history = Vec::new();
        let mut last_observation_artifact_id = None;
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
        let mut open_tickets: Vec<ShadowSlaOpeningTicket> = (0..backlog)
            .map(|index| ShadowSlaOpeningTicket {
                ticket_id: format!("review-opening-{index}"),
                created_at_utc: (start - chrono::Duration::days(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            })
            .collect();
        let mut prior_resolved: Vec<TicketEvent> = (0..7)
            .flat_map(|day| {
                (0..16).map(move |ticket| {
                    let prior_day = start - chrono::Duration::days(7 - day);
                    TicketEvent {
                        queue_id: Some("support-queue".into()),
                        ticket_id: format!("review-prior-{day}-{ticket}"),
                        created_at_utc: (prior_day - chrono::Duration::days(1))
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        resolved_at_utc: Some(
                            (prior_day + chrono::Duration::seconds(300))
                                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                        ),
                    }
                })
            })
            .collect();
        for index in 0..21 {
            let target = start + chrono::Duration::days(index);
            let end = target + chrono::Duration::days(1);
            let agents = if index % 2 == 0 { 2 } else { 3 };
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
                    &format!("training-review-{index}"),
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
                    &format!("forecast-review-{index}"),
                    &training_artifact.id,
                    &target.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    KnownDayInputs {
                        opening_backlog: backlog,
                        planned_agents: agents,
                        planned_fixed_extra_capacity: 0,
                    },
                    &policy.id,
                    target.timestamp() + 120,
                )
                .unwrap();
            assert_eq!(open_tickets.len() as u64, backlog);
            let mut ages = std::collections::BTreeMap::<u32, u32>::new();
            for ticket in &open_tickets {
                let created = chrono::DateTime::parse_from_rfc3339(&ticket.created_at_utc)
                    .unwrap()
                    .timestamp();
                let age = u32::try_from((target.timestamp() - created + 86_399) / 86_400).unwrap();
                *ages.entry(age).or_default() += 1;
            }
            let opening = ShadowSlaOpeningExport {
                inputs: KnownSlaDayInputs {
                    target_day_utc: forecast.target_day_utc.clone(),
                    queue_id: Some("support-queue".into()),
                    opening_cohorts: ages
                        .into_iter()
                        .map(|(age_days, count)| crate::decision_sim::InitialCohort {
                            age_days,
                            count,
                        })
                        .collect(),
                    known: forecast.known.clone(),
                },
                opening_tickets: open_tickets.clone(),
                prior_resolved_tickets: prior_resolved
                    .iter()
                    .filter(|ticket| {
                        chrono::DateTime::parse_from_rfc3339(
                            ticket.resolved_at_utc.as_deref().unwrap(),
                        )
                        .unwrap()
                        .timestamp()
                            >= target.timestamp() - 7 * 86_400
                    })
                    .cloned()
                    .collect(),
            };
            let opening_artifact = causal
                .add_artifact(
                    &evidence_scope,
                    "shadow_sla_opening_export",
                    &format!("opening-review-{index}"),
                    "v1",
                    "queue",
                    &serde_json::to_string(&opening).unwrap(),
                    target.timestamp(),
                    i64::MAX,
                )
                .unwrap();
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![target.timestamp() + 130, opening_artifact.id],
                )
                .unwrap();
            let sla_forecast = store
                .put_shadow_sla_forecast_at(
                    &scope,
                    &format!("sla-review-{index}"),
                    &forecast.id,
                    &sla_model.version,
                    &opening_artifact.id,
                    target.timestamp() + 150,
                )
                .unwrap();
            let resolved = agents * 8;
            assert_eq!(
                sla_forecast.prediction.predicted_resolved_within_sla,
                resolved
            );
            let observed = ObservedSupportDay {
                arrivals: 20,
                backlog_start: backlog,
                resolved,
                backlog_end: backlog + 20 - resolved as u64,
                agents,
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
                    &format!("observed-review-{index}"),
                    "v1",
                    "queue",
                    &serde_json::to_string(&export).unwrap(),
                    end.timestamp(),
                    i64::MAX,
                )
                .unwrap();
            if index == 20 {
                last_observation_artifact_id = Some(observation_artifact.id.clone());
            }
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![end.timestamp() + 1, observation_artifact.id],
                )
                .unwrap();
            let score = store
                .put_shadow_score_at(
                    &scope,
                    &format!("score-review-{index}"),
                    &forecast.id,
                    &observation_artifact.id,
                    end.timestamp() + 2,
                )
                .unwrap();
            let mut day_tickets: Vec<TicketEvent> = open_tickets
                .iter()
                .enumerate()
                .map(|(ticket_index, ticket)| TicketEvent {
                    queue_id: Some("support-queue".into()),
                    ticket_id: ticket.ticket_id.clone(),
                    created_at_utc: ticket.created_at_utc.clone(),
                    resolved_at_utc: (ticket_index < resolved as usize).then(|| {
                        (target + chrono::Duration::seconds(300))
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                    }),
                })
                .collect();
            day_tickets.extend((0..20).map(|arrival| {
                TicketEvent {
                    queue_id: Some("support-queue".into()),
                    ticket_id: format!("review-arrival-{index}-{arrival}"),
                    created_at_utc: (target + chrono::Duration::seconds(100))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    resolved_at_utc: None,
                }
            }));
            let day_ticket_export = ShadowSlaObservationExport {
                queue_id: "support-queue".into(),
                target_day_utc: forecast.target_day_utc.clone(),
                observed_through_utc: export.observed_through_utc.clone(),
                tickets: day_tickets.clone(),
            };
            let ticket_artifact = causal
                .add_artifact(
                    &evidence_scope,
                    "shadow_sla_observation_export",
                    &format!("tickets-review-{index}"),
                    "v1",
                    "queue",
                    &serde_json::to_string(&day_ticket_export).unwrap(),
                    end.timestamp(),
                    i64::MAX,
                )
                .unwrap();
            causal_conn
                .execute(
                    "UPDATE causal_artifacts SET ingested_at=?1 WHERE id=?2",
                    params![end.timestamp() + 2, ticket_artifact.id],
                )
                .unwrap();
            let sla_score = store
                .put_shadow_sla_score_at(
                    &scope,
                    &format!("sla-score-review-{index}"),
                    &sla_forecast.id,
                    &score.id,
                    &ticket_artifact.id,
                    end.timestamp() + 3,
                )
                .unwrap();
            assert_eq!(sla_score.observed_resolved_within_sla, u64::from(resolved));
            assert_eq!(sla_score.abs_error, 0);
            prior_resolved.extend(
                day_tickets
                    .iter()
                    .filter(|ticket| ticket.resolved_at_utc.is_some())
                    .cloned(),
            );
            open_tickets = day_tickets
                .into_iter()
                .filter(|ticket| ticket.resolved_at_utc.is_none())
                .map(|ticket| ShadowSlaOpeningTicket {
                    ticket_id: ticket.ticket_id,
                    created_at_utc: ticket.created_at_utc,
                })
                .collect();
            backlog = observed.backlog_end;
            history.push(observed);
        }
        let sla_screen = store
            .put_sla_shadow_review_screen(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 21,
                    min_fixed_coverage_bps: 8_000,
                },
            )
            .unwrap();
        assert!(
            sla_screen.report.eligible_for_human_review,
            "{:?}",
            sla_screen.report.failed_checks
        );
        assert_eq!(sla_screen.report.assessment.due_days, 21);
        assert_eq!(
            sla_screen
                .report
                .assessment
                .recent_7_day_error_sums
                .as_ref()
                .unwrap()
                .model,
            0
        );
        let failed_sla_screen = store
            .put_sla_shadow_review_screen(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 22,
                    min_fixed_coverage_bps: 8_000,
                },
            )
            .unwrap();
        assert!(!failed_sla_screen.report.eligible_for_human_review);
        assert!(matches!(
            store
                .request_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &failed_sla_screen.replay_hash,
                    "support-agent",
                    "Do not review an ineligible SLA screen",
                    3_600,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let sla_link = store
            .request_sla_shadow_screen_review(
                &broker,
                &scope,
                &sla_screen.replay_hash,
                "support-agent",
                "Inspect synthetic 21-day SLA candidate",
                3_600,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .require_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &sla_link.approval_id,
                    &sla_screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        broker
            .decide(
                &ApprovalId::from(sla_link.approval_id.clone()),
                true,
                "human-sla-reviewer",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .require_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &sla_link.approval_id,
                    &sla_screen.replay_hash,
                )
                .await
                .unwrap(),
            sla_link
        );
        assert!(matches!(
            store
                .require_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &sla_link.approval_id,
                    "wrong-sla-screen",
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let screen = store
            .put_shadow_review_screen(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 21,
                    min_fixed_coverage_bps: 8_000,
                },
            )
            .unwrap();
        assert!(screen.report.eligible_for_human_review);
        assert_eq!(
            screen.report.assessment.queue_id.as_deref(),
            Some("support-queue")
        );
        assert_eq!(
            screen
                .report
                .assessment
                .recent_7_day_error_sums
                .as_ref()
                .unwrap()
                .backlog,
            0
        );
        let failed_screen = store
            .put_shadow_review_screen(
                &scope,
                &policy.id,
                &crate::decision_shadow_screen::ShadowReviewCriteria {
                    min_complete_days: 22,
                    min_fixed_coverage_bps: 8_000,
                },
            )
            .unwrap();
        assert!(!failed_screen.report.eligible_for_human_review);
        assert!(matches!(
            store
                .request_shadow_screen_review(
                    &broker,
                    &scope,
                    &failed_screen.replay_hash,
                    "support-agent",
                    "Do not review failed screen as a candidate",
                    3_600,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        let link = store
            .request_shadow_screen_review(
                &broker,
                &scope,
                &screen.replay_hash,
                "support-agent",
                "Inspect synthetic shadow candidate",
                3_600,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .require_shadow_screen_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        broker
            .decide(
                &ApprovalId::from(link.approval_id.clone()),
                true,
                "human-reviewer",
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .require_shadow_screen_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await
                .unwrap(),
            link
        );
        assert!(matches!(
            store
                .require_shadow_screen_review(&broker, &scope, &link.approval_id, "wrong-screen",)
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        store
            .put_shadow_score_correction_at(
                &scope,
                "review-correction-20",
                "forecast-review-20",
                "score-review-20",
                last_observation_artifact_id.as_deref().unwrap(),
                "reviewer",
                "new revision after the inspection receipt",
                policy_end.timestamp() + 20,
            )
            .unwrap();
        assert!(matches!(
            store
                .require_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &sla_link.approval_id,
                    &sla_screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert!(matches!(
            store
                .require_shadow_screen_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        assert!(matches!(
            store
                .request_shadow_screen_review(
                    &broker,
                    &scope,
                    &screen.replay_hash,
                    "support-agent",
                    "Do not reuse a stale screen",
                    3_600,
                )
                .await,
            Err(DecisionStoreError::ReviewDenied)
        ));
        store
            .revoke_source_version(&scope, &screen.source_version_hashes[0])
            .unwrap();
        assert!(matches!(
            store
                .require_shadow_screen_review(
                    &broker,
                    &scope,
                    &link.approval_id,
                    &screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::Revoked)
        ));
        assert!(matches!(
            store
                .require_sla_shadow_screen_review(
                    &broker,
                    &scope,
                    &sla_link.approval_id,
                    &sla_screen.replay_hash,
                )
                .await,
            Err(DecisionStoreError::Revoked)
        ));
    }

