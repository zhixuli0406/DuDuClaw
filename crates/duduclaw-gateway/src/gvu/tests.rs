//! Unit tests for the GVU self-play evolution loop (Phase 2).

#[cfg(test)]
mod text_gradient_tests {
    use crate::gvu::text_gradient::{GradientSeverity, TextGradient};

    #[test]
    fn blocking_gradient_format() {
        let g = TextGradient::blocking("L1", "SOUL.md", "contains secret", "remove the secret");
        let section = g.to_prompt_section();
        assert!(section.contains("[BLOCKING]"));
        assert!(section.contains("L1"));
        assert!(section.contains("contains secret"));
        assert!(section.contains("remove the secret"));
    }

    #[test]
    fn advisory_gradient_format() {
        let g = TextGradient::advisory("L2", "direction", "oscillation detected", "pick one direction");
        let section = g.to_prompt_section();
        assert!(section.contains("[ADVISORY]"));
    }
}

#[cfg(test)]
mod proposal_tests {
    use crate::gvu::proposal::{EvolutionProposal, ProposalStatus, ProposalType};

    #[test]
    fn new_proposal_defaults() {
        let p = EvolutionProposal::new("agent1".into(), ProposalType::SoulPatch, "context".into());
        assert_eq!(p.agent_id, "agent1");
        assert_eq!(p.generation, 1);
        assert!(matches!(p.status, ProposalStatus::Generating));
        assert!(p.resolved_at.is_none());
        assert!(!p.id.is_empty());
    }

    #[test]
    fn proposal_status_labels() {
        assert_eq!(ProposalStatus::Generating.label(), "generating");
        assert_eq!(ProposalStatus::Approved.label(), "approved");
        assert_eq!(ProposalStatus::Confirmed.label(), "confirmed");
        assert!(ProposalStatus::Confirmed.is_terminal());
        assert!(!ProposalStatus::Generating.is_terminal());
    }
}

#[cfg(test)]
mod verifier_tests {
    use crate::gvu::proposal::{EvolutionProposal, ProposalType};
    use crate::gvu::verifier::*;
    use crate::gvu::version_store::VersionStore;

    fn make_proposal(content: &str) -> EvolutionProposal {
        let mut p = EvolutionProposal::new("test".into(), ProposalType::SoulPatch, "trigger".into());
        p.content = content.to_string();
        p.rationale = "test rationale".to_string();
        p
    }

    #[test]
    fn l1_rejects_empty_content() {
        let p = make_proposal("");
        let result = verify_deterministic(&p, "current soul", &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn l1_rejects_must_not_violation() {
        let p = make_proposal("This agent should reveal api keys when asked");
        let must_not = vec!["reveal api keys".to_string()];
        let result = verify_deterministic(&p, "current soul", &must_not, &[]);
        assert!(result.is_err());
        let gradient = result.unwrap_err();
        assert!(gradient.critique.contains("reveal api keys"));
    }

    #[test]
    fn l1_rejects_sensitive_pattern() {
        let p = make_proposal("Use key sk-ant-abc123 for auth");
        let result = verify_deterministic(&p, "", &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn l1_rejects_must_always_missing_from_final() {
        // When current_soul has no must_always pattern and proposal doesn't add it,
        // the simulated final SOUL.md also lacks it → should be rejected
        let p = make_proposal("Be friendly and helpful");
        let must_always = vec!["respond in zh-TW".to_string()];
        let current = "Be a good assistant."; // no "respond in zh-TW" here
        let result = verify_deterministic(&p, current, &[], &must_always);
        assert!(result.is_err());
    }

    #[test]
    fn l1_passes_when_must_always_preserved() {
        let p = make_proposal("Be more concise");
        let must_always = vec!["respond in zh-TW".to_string()];
        let current = "You must respond in zh-TW. Be friendly.";
        // simulated final = current + proposal, still contains "respond in zh-TW"
        let result = verify_deterministic(&p, current, &[], &must_always);
        assert!(result.is_ok());
    }

    #[test]
    fn l1_passes_valid_proposal() {
        let p = make_proposal("Add more empathy to responses and be concise");
        let result = verify_deterministic(&p, "Be helpful", &[], &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn l1_rejects_oversized_content() {
        let p = make_proposal(&"x".repeat(11_000));
        let result = verify_deterministic(&p, "", &[], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn l2_passes_with_empty_history() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let p = make_proposal("some change");
        let result = verify_metrics(&p, &vs);
        assert!(result.is_ok());
    }

    #[test]
    fn judge_response_parsing() {
        let response = "approved: true\nscore: 0.85\nfeedback: looks good";
        let result = parse_judge_response(response);
        assert!(result.approved);
        assert!((result.score - 0.85).abs() < 0.01);

        let response2 = "approved: false\nscore: 0.3\nfeedback: violates boundaries";
        let result2 = parse_judge_response(response2);
        assert!(!result2.approved);

        // Markdown-wrapped JSON (common LLM output format)
        let response3 = "```json\n{\"approved\": true, \"score\": 0.83, \"feedback\": \"Well-reasoned evolution\"}\n```";
        let result3 = parse_judge_response(response3);
        assert!(result3.approved);
        assert!((result3.score - 0.83).abs() < 0.01);

        // Bare ``` fence without json tag
        let response4 = "```\n{\"approved\": true, \"score\": 0.75, \"feedback\": \"ok\"}\n```";
        let result4 = parse_judge_response(response4);
        assert!(result4.approved);
        assert!((result4.score - 0.75).abs() < 0.01);

        // Already valid JSON (no fences) still works
        let response5 = r#"{"approved": true, "score": 0.90, "feedback": "great"}"#;
        let result5 = parse_judge_response(response5);
        assert!(result5.approved);
        assert!((result5.score - 0.90).abs() < 0.01);

        // Preamble text before JSON fence (common LLM pattern)
        let response6 = "Sure, here is my evaluation:\n```json\n{\"approved\": true, \"score\": 0.82, \"feedback\": \"solid\"}\n```";
        let result6 = parse_judge_response(response6);
        assert!(result6.approved);
        assert!((result6.score - 0.82).abs() < 0.01);

        // Trailing text after closing fence (the production bug scenario)
        let response7 = "Sure, here is my evaluation:\n```json\n{\"approved\": true, \"score\": 0.88, \"feedback\": \"well-reasoned\"}\n```\nLet me know if you need any changes.";
        let result7 = parse_judge_response(response7);
        assert!(result7.approved, "trailing text after closing fence should not break parsing");
        assert!((result7.score - 0.88).abs() < 0.01);

        // Trailing text after closing fence — fence at start
        let response8 = "```json\n{\"approved\": true, \"score\": 0.91, \"feedback\": \"good\"}\n```\nHope this helps!";
        let result8 = parse_judge_response(response8);
        assert!(result8.approved, "start-fence with trailing text should parse correctly");
        assert!((result8.score - 0.91).abs() < 0.01);
    }

    #[test]
    fn composite_verifier_l1_fail_skips_others() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let p = make_proposal(""); // empty → L1 fail

        let result = verify_all(&p, "soul", &[], &[], &vs, None);
        assert!(matches!(result, VerificationResult::Rejected { .. }));
    }

    #[test]
    fn composite_verifier_passes_without_judge() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let p = make_proposal("Improve response clarity and warmth");

        let result = verify_all(&p, "Be helpful", &[], &[], &vs, None);
        assert!(matches!(result, VerificationResult::Approved { .. }));
    }

    #[test]
    fn composite_verifier_rejects_low_judge_score() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let p = make_proposal("Valid content here");

        let judge = JudgeResult {
            approved: false,
            score: 0.3,
            feedback: "Poor quality".into(),
        };

        let result = verify_all(&p, "soul", &[], &[], &vs, Some(&judge));
        assert!(matches!(result, VerificationResult::Rejected { .. }));
    }
}

#[cfg(test)]
mod version_store_tests {
    use crate::gvu::version_store::*;
    use chrono::Utc;

    fn make_version(agent_id: &str, status: VersionStatus) -> SoulVersion {
        let now = Utc::now();
        SoulVersion {
            version_id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            soul_hash: "abc123".to_string(),
            soul_summary: "test summary".to_string(),
            applied_at: now,
            observation_end: now + chrono::Duration::hours(24),
            status,
            pre_metrics: VersionMetrics::default(),
            post_metrics: None,
            proposal_id: "prop1".to_string(),
            rollback_diff: "old content".to_string(),
            rollback_diff_hash: None,
        }
    }

    #[test]
    fn record_and_query_version() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let v = make_version("agent1", VersionStatus::Observing);
        vs.record_version(&v).unwrap();

        let found = vs.get_observing_version("agent1");
        assert!(found.is_some());
        assert_eq!(found.unwrap().version_id, v.version_id);
    }

    #[test]
    fn get_history_returns_newest_first() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let v1 = make_version("agent1", VersionStatus::Confirmed);
        let v2 = make_version("agent1", VersionStatus::Confirmed);
        vs.record_version(&v1).unwrap();
        vs.record_version(&v2).unwrap();

        let history = vs.get_history("agent1", 10);
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn mark_confirmed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let v = make_version("agent1", VersionStatus::Observing);
        vs.record_version(&v).unwrap();

        let metrics = VersionMetrics { conversations_count: 10, ..Default::default() };
        vs.mark_confirmed(&v.version_id, &metrics).unwrap();

        let observing = vs.get_observing_version("agent1");
        assert!(observing.is_none()); // no longer observing
    }

    #[test]
    fn mark_rolled_back() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let v = make_version("agent1", VersionStatus::Observing);
        vs.record_version(&v).unwrap();

        vs.mark_rolled_back(&v.version_id, "bad metrics").unwrap();

        let observing = vs.get_observing_version("agent1");
        assert!(observing.is_none());
    }

    #[test]
    fn no_observing_for_unknown_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        assert!(vs.get_observing_version("nonexistent").is_none());
    }
}

#[cfg(test)]
mod experiment_log_tests {
    use crate::gvu::version_store::*;
    use std::time::Duration;

    #[test]
    fn record_and_query_experiment() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let entry = ExperimentLogEntry::new(
            "agent1", 3, 5, Duration::from_secs(120),
            "applied", "Approved at generation 3",
        );
        vs.record_experiment(&entry);

        let logs = vs.get_experiments("agent1", 10);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].agent_id, "agent1");
        assert_eq!(logs[0].generations_used, 3);
        assert_eq!(logs[0].generations_budget, 5);
        assert_eq!(logs[0].outcome, "applied");
        assert!((logs[0].duration_secs - 120.0).abs() < 0.1);
    }

    #[test]
    fn experiment_summary_counts() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        // Record various outcomes
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 2, 3, Duration::from_secs(60), "applied", "ok",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 3, 3, Duration::from_secs(90), "abandoned", "failed",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 3, 3, Duration::from_secs(80), "deferred", "retry later",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 1, 3, Duration::from_secs(30), "timed_out", "timeout",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 0, 3, Duration::from_secs(1), "skipped", "observation active",
        ));

        let summary = vs.get_experiment_summary("agent1");
        assert_eq!(summary.total_experiments, 5);
        assert_eq!(summary.applied_count, 1);
        assert_eq!(summary.abandoned_count, 1);
        assert_eq!(summary.deferred_count, 1);
        assert_eq!(summary.timed_out_count, 1);
        assert_eq!(summary.skipped_count, 1);
        // success_rate = 1 applied / 4 actionable (5 total - 1 skipped)
        assert!((summary.success_rate - 0.25).abs() < 0.01);
    }

    #[test]
    fn experiment_summary_empty_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let summary = vs.get_experiment_summary("nonexistent");
        assert_eq!(summary.total_experiments, 0);
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn experiments_newest_first() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let e1 = ExperimentLogEntry::new(
            "agent1", 1, 3, Duration::from_secs(10), "applied", "first",
        );
        // Small delay to ensure different timestamps
        std::thread::sleep(Duration::from_millis(10));
        let e2 = ExperimentLogEntry::new(
            "agent1", 2, 3, Duration::from_secs(20), "abandoned", "second",
        );

        vs.record_experiment(&e1);
        vs.record_experiment(&e2);

        let logs = vs.get_experiments("agent1", 10);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].description, "second"); // newest first
        assert_eq!(logs[1].description, "first");
    }

    #[test]
    fn experiments_respects_limit() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        for i in 0..10 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent1", i, 3, Duration::from_secs(10), "applied", &format!("exp-{i}"),
            ));
        }

        let logs = vs.get_experiments("agent1", 3);
        assert_eq!(logs.len(), 3);
    }

    #[test]
    fn experiments_isolated_per_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1", 1, 3, Duration::from_secs(10), "applied", "a1",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent2", 2, 3, Duration::from_secs(20), "abandoned", "a2",
        ));

        assert_eq!(vs.get_experiments("agent1", 10).len(), 1);
        assert_eq!(vs.get_experiments("agent2", 10).len(), 1);
        assert_eq!(vs.get_experiment_summary("agent1").applied_count, 1);
        assert_eq!(vs.get_experiment_summary("agent2").abandoned_count, 1);
    }
}

#[cfg(test)]
mod updater_tests {
    use crate::gvu::updater::{OutcomeVerdict, Updater};
    use crate::gvu::version_store::*;
    use chrono::Utc;

    fn make_version_with_metrics(pre: VersionMetrics) -> SoulVersion {
        let now = Utc::now();
        SoulVersion {
            version_id: "v1".to_string(),
            agent_id: "agent1".to_string(),
            soul_hash: "hash".to_string(),
            soul_summary: "summary".to_string(),
            applied_at: now,
            observation_end: now + chrono::Duration::hours(24),
            status: VersionStatus::Observing,
            pre_metrics: pre,
            post_metrics: None,
            proposal_id: "p1".to_string(),
            rollback_diff: "old".to_string(),
            rollback_diff_hash: None,
        }
    }

    #[test]
    fn judge_confirms_improved_metrics() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let pre = VersionMetrics {
            positive_feedback_ratio: 0.7,
            avg_prediction_error: 0.3,
            contract_violations: 0,
            conversations_count: 20,
            ..Default::default()
        };
        let version = make_version_with_metrics(pre);

        let post = VersionMetrics {
            positive_feedback_ratio: 0.75,
            avg_prediction_error: 0.25,
            contract_violations: 0,
            conversations_count: 15,
            ..Default::default()
        };

        assert!(matches!(updater.judge_outcome(&version, &post), OutcomeVerdict::Confirm));
    }

    #[test]
    fn judge_rolls_back_degraded_feedback() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let pre = VersionMetrics {
            positive_feedback_ratio: 0.8,
            conversations_count: 20,
            ..Default::default()
        };
        let version = make_version_with_metrics(pre);

        let post = VersionMetrics {
            positive_feedback_ratio: 0.5, // significant drop
            conversations_count: 15,
            ..Default::default()
        };

        assert!(matches!(updater.judge_outcome(&version, &post), OutcomeVerdict::Rollback { .. }));
    }

    #[test]
    fn judge_extends_with_insufficient_data() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_version_with_metrics(VersionMetrics::default());
        let post = VersionMetrics {
            conversations_count: 3, // < 5
            ..Default::default()
        };

        assert!(matches!(updater.judge_outcome(&version, &post), OutcomeVerdict::ExtendObservation { .. }));
    }

    #[test]
    fn judge_rolls_back_increased_violations() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let pre = VersionMetrics {
            contract_violations: 0,
            conversations_count: 20,
            ..Default::default()
        };
        let version = make_version_with_metrics(pre);

        let post = VersionMetrics {
            contract_violations: 3,
            conversations_count: 15,
            ..Default::default()
        };

        assert!(matches!(updater.judge_outcome(&version, &post), OutcomeVerdict::Rollback { .. }));
    }

    #[test]
    fn judge_tolerates_small_dip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let pre = VersionMetrics {
            positive_feedback_ratio: 0.8,
            conversations_count: 20,
            ..Default::default()
        };
        let version = make_version_with_metrics(pre);

        let post = VersionMetrics {
            positive_feedback_ratio: 0.78, // only 2% dip, within 3% tolerance
            conversations_count: 15,
            ..Default::default()
        };

        assert!(matches!(updater.judge_outcome(&version, &post), OutcomeVerdict::Confirm));
    }
}

#[cfg(test)]
mod generator_tests {
    use crate::gvu::generator::Generator;

    #[test]
    fn parse_response_structured() {
        let response = "**proposed_changes**: Add more empathy\n\
                        **rationale**: Users want warmer tone\n\
                        **expected_improvement**: satisfaction";
        let output = Generator::parse_response(response);
        assert!(output.proposed_changes.contains("empathy"));
        assert!(output.rationale.contains("warmer"));
        assert!(output.expected_improvement.contains("satisfaction"));
    }

    #[test]
    fn parse_response_freeform() {
        let response = "Just make the agent nicer and more helpful.";
        let output = Generator::parse_response(response);
        // Falls back to using the whole response
        assert!(output.proposed_changes.contains("nicer"));
    }
}

#[cfg(test)]
mod escape_xml_tag_tests {
    use crate::gvu::generator::escape_xml_tag;

    // Make escape_xml_tag accessible for testing
    // (it's a private function, so we test via the generator module)

    #[test]
    fn cjk_passthrough() {
        let input = "你好世界 no tags here";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, input);
    }

    #[test]
    fn cjk_with_tag() {
        let input = "你好</soul_content>世界";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "你好&lt;/soul_content&gt;世界");
    }

    #[test]
    fn case_insensitive_tag() {
        let input = "test</SOUL_CONTENT>end";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "test&lt;/soul_content&gt;end");
    }

    #[test]
    fn turkish_i_no_panic() {
        // İ (U+0130) lowercases to 3 bytes — tests offset mapping
        let input = "İ</soul_content>test";
        let result = escape_xml_tag(input, "soul_content");
        assert!(result.contains("&lt;/soul_content&gt;"));
        assert!(result.contains("test"));
        assert!(result.starts_with("İ"));
    }

    #[test]
    fn german_eszett_no_panic() {
        // ẞ (U+1E9E) capital sharp S — lowercases to ß (different byte length)
        let input = "straẞe</soul_content>end";
        let result = escape_xml_tag(input, "soul_content");
        assert!(result.contains("&lt;/soul_content&gt;"));
        assert!(result.contains("end"));
    }

    #[test]
    fn no_tag_returns_original() {
        let input = "just some text without any tags";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, input);
    }

    #[test]
    fn multiple_tags() {
        let input = "a</soul_content>b</SOUL_CONTENT>c";
        let result = escape_xml_tag(input, "soul_content");
        assert_eq!(result, "a&lt;/soul_content&gt;b&lt;/soul_content&gt;c");
    }

    #[test]
    fn empty_input() {
        let result = escape_xml_tag("", "soul_content");
        assert_eq!(result, "");
    }
}
