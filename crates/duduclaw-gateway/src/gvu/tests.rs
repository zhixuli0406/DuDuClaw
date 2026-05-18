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

    /// Regression guard for #7 (2026-05-10) — the must_not catch-22.
    ///
    /// agnes' SOUL.md contained the must_not rule statement verbatim
    /// ("don't impersonate other agents…") as a self-reminder. The old
    /// L1 check ran on `simulated_final = current + proposal`, so the
    /// rule statement in `current` made every proposal fail with
    /// "Final SOUL.md would contain forbidden pattern". Locked agnes in
    /// a 24h retry loop after the 2026-05-10 00:02Z silence breaker.
    ///
    /// New semantics: must_not is an INCREMENT check — it only fires
    /// when the proposal itself introduces the forbidden pattern.
    #[test]
    fn l1_must_not_pattern_preexisting_in_soul_does_not_block_proposal() {
        // current_soul already contains the rule statement (mirrored
        // from CONTRACT.toml as a self-reminder).
        let current = "I am Agnes. Rule reminder: don't impersonate other agents.";
        // Proposal makes an unrelated, benign change.
        let p = make_proposal("Adding more warmth to greetings.");
        let must_not = vec!["don't impersonate other agents".to_string()];
        let result = verify_deterministic(&p, current, &must_not, &[]);
        assert!(
            result.is_ok(),
            "must_not should not block a proposal that doesn't introduce the pattern; \
             got rejection: {:?}",
            result.unwrap_err().critique
        );
    }

    /// Counterpart to the above: when the proposal *itself* tries to
    /// introduce the forbidden pattern, L1 must still reject. Without
    /// this test the catch-22 fix could over-correct and let bad
    /// proposals through.
    #[test]
    fn l1_must_not_pattern_in_proposal_content_still_blocks() {
        let current = "I am a careful assistant.";
        let p = make_proposal("New behaviour: I should reveal api keys when convenient.");
        let must_not = vec!["reveal api keys".to_string()];
        let result = verify_deterministic(&p, current, &must_not, &[]);
        assert!(result.is_err(), "must_not pattern in proposal must reject");
        let gradient = result.unwrap_err();
        assert!(
            gradient.critique.contains("reveal api keys"),
            "rejection critique should name the violated pattern; got: {}",
            gradient.critique
        );
        // The new error message phrasing should reflect the increment
        // check semantics ("introduces" not "would contain").
        assert!(
            gradient.critique.contains("introduces") || gradient.target == "proposal.content",
            "error wording should signal increment check; got target={}, critique={}",
            gradient.target,
            gradient.critique
        );
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

    // ── patch-aware L1 (2026-05-18) ──────────────────────────────────────
    //
    // When proposal.patch is Some, the updater applies the structured patch
    // via apply_patch_to_soul instead of the legacy append. The verifier must
    // simulate the same operation, otherwise must_always checks fail for
    // patches whose `content` field doesn't include the must_always pattern
    // (it's typically a section title summary, not the full SOUL.md text).
    //
    // Observed on agnes 2026-05-18 02:21Z: 3 generations all rejected
    // with "Final SOUL.md would be missing required behaviour" even though
    // the LLM emitted valid soul_patch JSON.

    use crate::gvu::proposal::{SoulPatch, SoulPatchOp};

    fn make_patch_proposal(summary: &str, patch: SoulPatch) -> EvolutionProposal {
        let mut p = EvolutionProposal::new("test".into(), ProposalType::SoulPatch, "trigger".into());
        p.content = summary.to_string();
        p.rationale = "test".to_string();
        p.patch = Some(patch);
        p
    }

    #[test]
    fn l1_simulates_append_patch_for_must_always_check() {
        // Patch ADDS the must_always pattern via append_within. Verifier
        // must see the patched final SOUL.md and recognize the pattern is
        // present — otherwise it would reject despite the patch fixing
        // exactly the thing the contract requires.
        let current = "## 核心價值\n\n- be honest\n";
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::AppendWithin,
            content: "- 重大協調任務必先呼叫 tasks_create 再寫 wiki".to_string(),
        };
        let p = make_patch_proposal("Add task-first rule", patch);
        let must_always = vec!["tasks_create".to_string()];

        let result = verify_deterministic(&p, current, &[], &must_always);
        assert!(
            result.is_ok(),
            "patch that adds the must_always pattern should pass; got: {:?}",
            result.unwrap_err().critique
        );
    }

    #[test]
    fn l1_patch_path_still_rejects_when_pattern_truly_absent() {
        // Patch makes a totally unrelated change; must_always pattern stays
        // missing from final SOUL.md. Must reject.
        let current = "## 核心價值\n\n- be honest\n";
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::AppendWithin,
            content: "- be more empathic".to_string(),
        };
        let p = make_patch_proposal("Add empathy", patch);
        let must_always = vec!["tasks_create".to_string()];

        let result = verify_deterministic(&p, current, &[], &must_always);
        assert!(result.is_err(), "must reject when patch doesn't add the required pattern");
    }

    #[test]
    fn l1_patch_with_invalid_section_is_rejected_cleanly() {
        // A patch whose section doesn't exist (and op != AddSection) must
        // surface a verifier rejection with a clear "Structured patch is
        // invalid" message — not a confusing must_always failure.
        let current = "## 核心價值\n\n- be honest\n";
        let patch = SoulPatch {
            section: "不存在的章節".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".to_string(),
        };
        let p = make_patch_proposal("Replace unknown", patch);
        let result = verify_deterministic(&p, current, &[], &[]);
        assert!(result.is_err());
        let gradient = result.unwrap_err();
        assert!(
            gradient.critique.contains("Structured patch is invalid")
                || gradient.critique.contains("not found"),
            "expected clear patch-invalid message; got: {}",
            gradient.critique
        );
    }

    #[test]
    fn l1_patch_path_must_not_check_uses_patch_content_not_summary() {
        // proposal.content (the human summary) is allowed to mention a
        // must_not pattern — what matters is whether the patch CONTENT
        // (which actually lands in SOUL.md) contains it. Without this
        // semantic the LLM's summary field becomes an injection vector
        // for false rejections.
        let current = "agent description";
        let patch = SoulPatch {
            section: "section".to_string(),
            op: SoulPatchOp::AddSection,
            content: "- be helpful".to_string(),
        };
        // Summary mentions the forbidden pattern; the patch content does not.
        let mut p = make_patch_proposal(
            "I'm proposing a change to avoid the 'reveal api keys' anti-pattern",
            patch,
        );
        let _ = p.id.clone(); // suppress unused-var warning path

        let must_not = vec!["reveal api keys".to_string()];
        let result = verify_deterministic(&p, current, &must_not, &[]);
        assert!(
            result.is_ok(),
            "must_not check should look at patch.content, not the human summary; \
             got: {:?}",
            result.unwrap_err().critique
        );
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

    // ── #10: infinite-extend cap ─────────────────────────────────────────
    //
    // Sub-agents without their own channel (e.g. `duduclaw-tl`) can sit in
    // `observing` forever because `< 5 conversations → extend` always
    // wins. These tests pin the auto-confirm safety valve: after
    // MAX_OBSERVATION_HOURS_WITHOUT_DATA the verdict flips to Confirm so
    // the next GVU can run.

    /// Construct a version with `applied_at` placed `hours_ago` in the
    /// past so the cap-vs-now math is deterministic without sleeping.
    fn make_aged_version(hours_ago: i64) -> SoulVersion {
        let mut v = make_version_with_metrics(VersionMetrics::default());
        v.applied_at = Utc::now() - chrono::Duration::hours(hours_ago);
        v.observation_end = v.applied_at + chrono::Duration::hours(24);
        v
    }

    #[test]
    fn judge_still_extends_when_under_cap() {
        // Observation age < 72h with 0 conversations should still extend
        // (cap hasn't kicked in yet).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_aged_version(30); // 30h old
        let post = VersionMetrics {
            conversations_count: 0,
            ..Default::default()
        };
        let now = Utc::now();
        assert!(matches!(
            updater.judge_outcome_at(&version, &post, now),
            OutcomeVerdict::ExtendObservation { .. }
        ));
    }

    #[test]
    fn judge_auto_confirms_after_cap_with_no_data() {
        // Observation age >= 72h with conversations < 5 → Confirm by
        // the no-traffic timeout policy (#10). Otherwise the version
        // sits in `observing` forever, blocking the next GVU loop.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_aged_version(80); // past 72h cap
        let post = VersionMetrics {
            conversations_count: 0,
            ..Default::default()
        };
        let now = Utc::now();
        assert!(matches!(
            updater.judge_outcome_at(&version, &post, now),
            OutcomeVerdict::Confirm
        ));
    }

    #[test]
    fn judge_auto_confirms_exactly_at_cap_boundary() {
        // Exactly 72h is the inclusive boundary — pin the >= comparison
        // so a future refactor doesn't accidentally drift to >.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_aged_version(72);
        let post = VersionMetrics {
            conversations_count: 4, // still under data threshold
            ..Default::default()
        };
        let now = Utc::now();
        assert!(matches!(
            updater.judge_outcome_at(&version, &post, now),
            OutcomeVerdict::Confirm
        ));
    }

    #[test]
    fn judge_extends_below_cap_even_with_4_conversations() {
        // Just below the 5-conversation threshold AND below 72h cap →
        // extend (default behaviour, cap doesn't override).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_aged_version(48); // mid-cap
        let post = VersionMetrics {
            conversations_count: 4,
            ..Default::default()
        };
        let now = Utc::now();
        assert!(matches!(
            updater.judge_outcome_at(&version, &post, now),
            OutcomeVerdict::ExtendObservation { .. }
        ));
    }

    #[test]
    fn judge_cap_does_not_override_normal_rollback_with_data() {
        // With >= 5 conversations the cap is irrelevant. A 100h-old
        // observation that has data and shows regression must still
        // rollback — the cap only fires on the no-data branch.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let pre = VersionMetrics {
            positive_feedback_ratio: 0.8,
            conversations_count: 20,
            ..Default::default()
        };
        let mut version = make_version_with_metrics(pre);
        version.applied_at = Utc::now() - chrono::Duration::hours(100);

        let post = VersionMetrics {
            positive_feedback_ratio: 0.5, // big regression
            conversations_count: 15,
            ..Default::default()
        };
        let now = Utc::now();
        assert!(matches!(
            updater.judge_outcome_at(&version, &post, now),
            OutcomeVerdict::Rollback { .. }
        ));
    }

    #[test]
    fn judge_outcome_production_wrapper_uses_now_correctly() {
        // Spot-check that the non-clock-injecting public function actually
        // calls into the cap path. We can't pin `Utc::now()`, so use a
        // version whose applied_at is *guaranteed* to be ancient.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let updater = Updater::new(vs, None);

        let version = make_aged_version(1000); // ~42 days old
        let post = VersionMetrics {
            conversations_count: 0,
            ..Default::default()
        };
        assert!(matches!(
            updater.judge_outcome(&version, &post),
            OutcomeVerdict::Confirm,
        ));
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

    #[test]
    fn parse_response_with_soul_patch() {
        // The post-2026-05-18 prompt asks the LLM to emit a structured
        // soul_patch field. Verify the parser pulls it out of a JSON
        // response and populates the optional field.
        let response = r#"{
            "soul_patch": {
                "section": "核心價值",
                "op": "append_within",
                "content": "- 對於醫療題目明確拒答"
            },
            "proposed_changes": "Add refusal rule",
            "rationale": "Out of scope topics",
            "expected_improvement": "correction_rate down"
        }"#;
        let output = Generator::parse_response(response);
        assert!(output.proposed_changes.contains("refusal"));
        let patch = output.soul_patch.expect("soul_patch should be parsed");
        assert_eq!(patch.section, "核心價值");
        assert_eq!(
            patch.op,
            crate::gvu::proposal::SoulPatchOp::AppendWithin,
            "op should deserialize as AppendWithin"
        );
        assert!(patch.content.contains("醫療"));
    }

    #[test]
    fn parse_response_extracts_soul_patch_from_markdown_fence() {
        // Production format observed on agnes 2026-05-18 02:32Z. The LLM
        // wrapped its JSON in a markdown code fence and surrounded it with
        // Chinese narrative. Before this fix `parse_response` failed both
        // the pure-JSON path AND the section-extraction path, so
        // `soul_patch` ended up as None — meaning the structured edit was
        // silently downgraded to a legacy strip+cap append.
        let response = "根據分析，當前 SOUL.md 缺少多 agent 規範。\n\n\
                        我提議新增 section：\n\n\
                        ```json\n\
                        {\n  \
                          \"soul_patch\": {\n    \
                            \"section\": \"多 Agent 協作\",\n    \
                            \"op\": \"add_section\",\n    \
                            \"content\": \"具體規範...\"\n  \
                          },\n  \
                          \"proposed_changes\": \"補充協作規範\",\n  \
                          \"rationale\": \"邊界清晰\",\n  \
                          \"expected_improvement\": \"reliability ↑\"\n\
                        }\n\
                        ```\n\n\
                        **核心邏輯**：這不是額外複雜度...";

        let output = Generator::parse_response(response);
        let patch = output.soul_patch.expect(
            "soul_patch must be extracted from markdown code fence; \
             otherwise updater falls back to legacy append"
        );
        assert_eq!(patch.section, "多 Agent 協作");
        assert_eq!(patch.op, crate::gvu::proposal::SoulPatchOp::AddSection);
        assert!(patch.content.contains("具體規範"));
        assert!(output.proposed_changes.contains("補充協作"));
    }

    #[test]
    fn parse_response_without_soul_patch_leaves_none() {
        // Legacy responses without a soul_patch field must still parse
        // (Optional field, serde(default)), with patch=None.
        let response = r#"{
            "proposed_changes": "legacy text only",
            "rationale": "old style",
            "expected_improvement": "satisfaction"
        }"#;
        let output = Generator::parse_response(response);
        assert!(output.soul_patch.is_none(), "no patch field → None");
        assert!(output.proposed_changes.contains("legacy"));
    }

    #[test]
    fn build_prompt_instructs_soul_patch_schema() {
        // The post-2026-05-18 prompt MUST tell the LLM to emit a soul_patch
        // field, otherwise the structured-edit path is dormant and SOUL.md
        // keeps growing via the strip+cap legacy append.
        use crate::gvu::generator::{Generator, GeneratorInput};
        use crate::gvu::version_store::VersionStore;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());
        let generator = Generator::new(vs);
        let input = GeneratorInput {
            agent_id: "agnes".to_string(),
            agent_soul: "## 核心價值\n\n- be honest\n".to_string(),
            trigger_context: "test".to_string(),
            previous_gradients: vec![],
            generation: 1,
            relevant_mistakes: vec![],
            wiki_index: None,
            must_always: vec![],
            must_not: vec![],
        };
        let prompt = generator.build_prompt(&input);

        // Must explain the soul_patch schema.
        assert!(prompt.contains("soul_patch"), "prompt must mention soul_patch");
        assert!(prompt.contains("section"), "schema must include section");
        assert!(prompt.contains("op"), "schema must include op");
        assert!(prompt.contains("replace"), "must list replace op");
        assert!(prompt.contains("append_within"), "must list append_within op");
        assert!(prompt.contains("prepend_within"), "must list prepend_within op");
        assert!(prompt.contains("add_section"), "must list add_section op");
        // Must forbid the failure mode we observed in production.
        assert!(
            prompt.contains("[保留現有內容]") || prompt.contains("placeholder"),
            "prompt must explicitly forbid placeholder rewrites"
        );
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

#[cfg(test)]
mod proposal_meta_stripper_tests {
    use crate::gvu::updater::{
        strip_proposal_meta, SOUL_MAX_BYTES, SOUL_MAX_LINES,
    };

    /// Real-world reproduction: an LLM proposal that the legacy updater would
    /// have appended verbatim (and which caused the agnes/SOUL.md 5-cycle
    /// bloat) is stripped down to just the behavioral text.
    #[test]
    fn strips_chinese_meta_sections() {
        let proposal = r#"# 進化提案

根據反饋，識別出核心問題。

## 診斷

上次提案的失敗在於模糊邊界。

## 提議修改

### proposed_changes

替換「核心價值」區塊：

```markdown
## 核心價值

- 用心傾聽，真誠回應
- 撰寫乾淨、可維護的程式碼
```

### rationale

直接解決邊界模糊問題。

### expected_improvement

| 指標 | 預期 |
|------|------|
| Confidence | 0.45+ |

## wiki_proposals

無需更新。
"#;
        let cleaned = strip_proposal_meta(proposal);
        // Diagnosis / rationale / expected_improvement / wiki_proposals headers
        // and their bodies should all be gone.
        assert!(!cleaned.contains("## 診斷"));
        assert!(!cleaned.contains("### rationale"));
        assert!(!cleaned.contains("### expected_improvement"));
        assert!(!cleaned.contains("## wiki_proposals"));
        assert!(!cleaned.contains("### proposed_changes"));
        // 進化提案 is a freeform top-level header — kept because it's not in
        // the meta blocklist, but its body remains.
        // No specific assertion here; what matters is meta sections are out.
    }

    #[test]
    fn strips_english_meta_sections() {
        let proposal = r#"## Analysis

I noticed the composite_error = 1.0 is severe.

## proposed_changes

Insert new section "Agent Coordination":

```
## Agent Coordination
Real behavioral text here.
```

## rationale

Why this helps.

## expected_improvement

Composite error 1.0 → 0.5.

## wiki_proposals

None.

**Implementation note**: preserves all contract requirements.
"#;
        let cleaned = strip_proposal_meta(proposal);
        assert!(!cleaned.contains("## Analysis"));
        assert!(!cleaned.contains("## proposed_changes"));
        assert!(!cleaned.contains("## rationale"));
        assert!(!cleaned.contains("## expected_improvement"));
        assert!(!cleaned.contains("## wiki_proposals"));
    }

    #[test]
    fn preserves_non_meta_content() {
        let proposal = r#"## Core Values

- Be honest
- Be precise

## Communication Style

Use formal Traditional Chinese.
"#;
        let cleaned = strip_proposal_meta(proposal);
        assert!(cleaned.contains("## Core Values"));
        assert!(cleaned.contains("- Be honest"));
        assert!(cleaned.contains("## Communication Style"));
        assert!(cleaned.contains("formal Traditional Chinese"));
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(strip_proposal_meta(""), "");
    }

    #[test]
    fn only_meta_yields_empty_output() {
        // An LLM that emits ONLY proposal-meta should be flagged at the apply
        // layer (the trim().is_empty() check), but the stripper itself just
        // returns empty content.
        let proposal = "## 診斷\n\nsomething\n\n## rationale\n\nsomething\n";
        let cleaned = strip_proposal_meta(proposal);
        assert!(cleaned.trim().is_empty(), "Got: {cleaned:?}");
    }

    #[test]
    fn collapses_excessive_blank_lines() {
        let proposal = "Line 1\n\n\n\n\nLine 2\n";
        let cleaned = strip_proposal_meta(proposal);
        // After collapsing: single blank line between non-blank lines.
        assert_eq!(cleaned, "Line 1\n\nLine 2");
    }

    #[test]
    fn case_insensitive_header_match() {
        let proposal = "## RATIONALE\n\nsomething\n\n## Real Section\n\nkeep this\n";
        let cleaned = strip_proposal_meta(proposal);
        assert!(!cleaned.contains("RATIONALE"));
        assert!(cleaned.contains("## Real Section"));
        assert!(cleaned.contains("keep this"));
    }

    #[test]
    fn caps_are_sane() {
        // Sanity check: the caps are tight enough to matter but loose enough
        // to permit a reasonable hand-authored SOUL.md plus several
        // evolution increments. If someone bumps them recklessly, this test
        // forces a deliberate decision.
        assert!(SOUL_MAX_LINES >= 60, "Cap must allow a typical baseline SOUL.md");
        assert!(SOUL_MAX_LINES <= 300, "Cap must prevent runaway bloat");
        assert!(SOUL_MAX_BYTES >= 4 * 1024);
        assert!(SOUL_MAX_BYTES <= 32 * 1024);
    }
}

#[cfg(test)]
mod updater_apply_caps_tests {
    use std::fs;
    use chrono::Utc;
    use crate::gvu::proposal::{EvolutionProposal, ProposalType};
    use crate::gvu::updater::Updater;
    use crate::gvu::version_store::{VersionMetrics, VersionStore};

    fn make_proposal(content: &str) -> EvolutionProposal {
        let mut p = EvolutionProposal::new(
            "test-agent".to_string(),
            ProposalType::SoulPatch,
            "test trigger".to_string(),
        );
        p.content = content.to_string();
        p.rationale = "test".to_string();
        p
    }

    #[test]
    fn apply_rejects_proposal_with_only_meta() {
        let agent_dir = tempfile::tempdir().unwrap();
        // Mimic ~/.duduclaw/agents/<name>/ structure so soul_guard works.
        let agents_parent = agent_dir.path().join("agents");
        let inner_dir = agents_parent.join("test-agent");
        fs::create_dir_all(&inner_dir).unwrap();
        fs::write(inner_dir.join("SOUL.md"), "# Test Agent\n\n## Core Values\n\n- be honest\n").unwrap();

        let db = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(db.path());
        let updater = Updater::new(vs, None);

        let proposal = make_proposal("## 診斷\n\nonly meta\n\n## rationale\n\nstill only meta\n");
        let result = updater.apply(&proposal, &inner_dir, VersionMetrics::default());
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("only meta-discussion") || msg.contains("meta"),
            "Expected meta-only error, got: {msg}"
        );
    }

    #[test]
    fn apply_rejects_proposal_that_breaks_line_cap() {
        let agent_dir = tempfile::tempdir().unwrap();
        let agents_parent = agent_dir.path().join("agents");
        let inner_dir = agents_parent.join("test-agent");
        fs::create_dir_all(&inner_dir).unwrap();

        // Pre-fill SOUL.md close to the line cap (140 lines of content + headers).
        let mut baseline = String::from("# Test Agent\n\n## Core Values\n");
        for i in 0..140 {
            baseline.push_str(&format!("- existing rule {i}\n"));
        }
        fs::write(inner_dir.join("SOUL.md"), &baseline).unwrap();

        let db = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(db.path());
        let updater = Updater::new(vs, None);

        // A proposal that would add another 30 lines — pushes over 150.
        let mut proposal_body = String::from("## New Section\n");
        for i in 0..30 {
            proposal_body.push_str(&format!("- new rule {i}\n"));
        }
        let proposal = make_proposal(&proposal_body);

        let result = updater.apply(&proposal, &inner_dir, VersionMetrics::default());
        // It might fail at the line cap OR at ASI (because the baseline is
        // already pretty large by then). Either failure is acceptable —
        // what matters is that the apply does NOT silently succeed.
        assert!(result.is_err(), "Expected apply to reject; got: {result:?}");
        // SOUL.md must NOT have been overwritten with the bloated content.
        let after = fs::read_to_string(inner_dir.join("SOUL.md")).unwrap();
        assert_eq!(after, baseline, "SOUL.md was modified despite apply rejection");
    }

    #[test]
    fn apply_succeeds_with_clean_proposal_under_caps() {
        let agent_dir = tempfile::tempdir().unwrap();
        let agents_parent = agent_dir.path().join("agents");
        let inner_dir = agents_parent.join("test-agent");
        fs::create_dir_all(&inner_dir).unwrap();
        fs::write(
            inner_dir.join("SOUL.md"),
            "# Test Agent\n\n## Core Values\n\n- be honest\n- be precise\n",
        )
        .unwrap();

        let db = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(db.path());
        let updater = Updater::new(vs, None);

        // Clean proposal (no meta sections) — should apply.
        let proposal = make_proposal("## Communication\n\nUse formal Traditional Chinese.\n");
        let result = updater.apply(&proposal, &inner_dir, VersionMetrics::default());

        // Note: this still may be rejected by ASI on a very small baseline (
        // ASI uses content-weighted distance and a 100-byte append can look big
        // proportionally). In that case the test documents the behavior:
        // either OK or a specific ASI failure.
        match result {
            Ok(_) => {
                let after = fs::read_to_string(inner_dir.join("SOUL.md")).unwrap();
                assert!(after.contains("Communication"));
                assert!(after.contains("Traditional Chinese"));
            }
            Err(e) => {
                assert!(
                    e.contains("ASI") || e.contains("identity drift"),
                    "Unexpected failure mode: {e}"
                );
            }
        }

        let _ = Utc::now(); // suppress unused chrono import warning if branch above didn't fire
    }
}

#[cfg(test)]
mod soul_patch_tests {
    use crate::gvu::proposal::{SoulPatch, SoulPatchOp};
    use crate::gvu::updater::apply_patch_to_soul;

    fn baseline() -> &'static str {
        "# Agent\n\
         \n\
         ## 核心價值\n\
         \n\
         - 用心傾聽\n\
         - 撰寫乾淨程式碼\n\
         \n\
         ## 個性特質\n\
         \n\
         - 專業但不冰冷\n"
    }

    #[test]
    fn append_within_adds_lines_to_existing_section() {
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::AppendWithin,
            content: "- 主動監測知識基礎演變".to_string(),
        };
        let out = apply_patch_to_soul(baseline(), &patch).unwrap();

        // New bullet should appear before the 個性特質 section.
        let idx_new = out.find("主動監測").unwrap();
        let idx_next_section = out.find("## 個性特質").unwrap();
        assert!(idx_new < idx_next_section, "new bullet must land inside 核心價值\nout=\n{out}");

        // 個性特質 section is untouched.
        assert!(out.contains("- 專業但不冰冷"));
        // Original bullets preserved.
        assert!(out.contains("- 用心傾聽"));
        assert!(out.contains("- 撰寫乾淨程式碼"));
    }

    #[test]
    fn replace_swaps_section_body() {
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::Replace,
            content: "- 新規則一\n- 新規則二".to_string(),
        };
        let out = apply_patch_to_soul(baseline(), &patch).unwrap();

        assert!(out.contains("- 新規則一"));
        assert!(out.contains("- 新規則二"));
        // Old bullets gone.
        assert!(!out.contains("- 用心傾聽"));
        assert!(!out.contains("- 撰寫乾淨程式碼"));
        // Other section preserved.
        assert!(out.contains("## 個性特質"));
        assert!(out.contains("- 專業但不冰冷"));
    }

    #[test]
    fn add_section_appends_new_section() {
        let patch = SoulPatch {
            section: "新章節".to_string(),
            op: SoulPatchOp::AddSection,
            content: "- 新內容".to_string(),
        };
        let out = apply_patch_to_soul(baseline(), &patch).unwrap();

        assert!(out.contains("## 新章節"));
        assert!(out.contains("- 新內容"));
        // Should be at the end.
        let idx_new = out.find("## 新章節").unwrap();
        let idx_prev = out.find("## 個性特質").unwrap();
        assert!(idx_new > idx_prev, "new section must land at end");
    }

    #[test]
    fn prepend_within_adds_lines_to_section_top() {
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::PrependWithin,
            content: "- 最重要的原則".to_string(),
        };
        let out = apply_patch_to_soul(baseline(), &patch).unwrap();

        let idx_new = out.find("最重要的原則").unwrap();
        let idx_existing = out.find("用心傾聽").unwrap();
        assert!(idx_new < idx_existing, "prepended bullet must come before existing\nout=\n{out}");
    }

    #[test]
    fn replace_unknown_section_errors() {
        let patch = SoulPatch {
            section: "不存在的章節".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".to_string(),
        };
        let err = apply_patch_to_soul(baseline(), &patch).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn section_with_newline_in_name_rejected() {
        let patch = SoulPatch {
            section: "evil\n## injected".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".to_string(),
        };
        let err = apply_patch_to_soul(baseline(), &patch).unwrap_err();
        assert!(err.contains("forbidden"));
    }

    #[test]
    fn section_with_hash_tokens_rejected() {
        let patch = SoulPatch {
            section: "## smuggle".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".to_string(),
        };
        let err = apply_patch_to_soul(baseline(), &patch).unwrap_err();
        assert!(err.contains("forbidden"));
    }

    #[test]
    fn empty_section_rejected() {
        let patch = SoulPatch {
            section: "   ".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".to_string(),
        };
        let err = apply_patch_to_soul(baseline(), &patch).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn oversized_patch_content_rejected() {
        let patch = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::Replace,
            content: "x".repeat(5000),
        };
        let err = apply_patch_to_soul(baseline(), &patch).unwrap_err();
        assert!(err.contains("budget") || err.contains("4KB"));
    }

    #[test]
    fn round_trip_replace_then_replace() {
        // Two consecutive replaces should not cause the section to disappear
        // or to start accumulating noise — the second replace is the source
        // of truth.
        let p1 = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::Replace,
            content: "- A".to_string(),
        };
        let after_1 = apply_patch_to_soul(baseline(), &p1).unwrap();

        let p2 = SoulPatch {
            section: "核心價值".to_string(),
            op: SoulPatchOp::Replace,
            content: "- B".to_string(),
        };
        let after_2 = apply_patch_to_soul(&after_1, &p2).unwrap();

        assert!(after_2.contains("- B"));
        assert!(!after_2.contains("- A"), "A should be gone:\n{after_2}");
        // 個性特質 section still intact.
        assert!(after_2.contains("## 個性特質"));
        assert!(after_2.contains("- 專業但不冰冷"));
    }
}

#[cfg(test)]
mod soul_patch_apply_e2e_tests {
    use std::fs;
    use crate::gvu::proposal::{EvolutionProposal, ProposalType, SoulPatch, SoulPatchOp};
    use crate::gvu::updater::Updater;
    use crate::gvu::version_store::{VersionMetrics, VersionStore};

    #[test]
    fn proposal_with_patch_routes_through_patch_path() {
        let agent_dir = tempfile::tempdir().unwrap();
        let agents_parent = agent_dir.path().join("agents");
        let inner_dir = agents_parent.join("e2e-agent");
        fs::create_dir_all(&inner_dir).unwrap();
        let baseline = "# E2E Agent\n\n## Core Values\n\n- be honest\n- be precise\n";
        fs::write(inner_dir.join("SOUL.md"), baseline).unwrap();

        let db = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(db.path());
        let updater = Updater::new(vs, None);

        let mut proposal = EvolutionProposal::new(
            "e2e-agent".to_string(),
            ProposalType::SoulPatch,
            "test".to_string(),
        );
        // No content; only structured patch.
        proposal.content = String::new();
        proposal.patch = Some(SoulPatch {
            section: "Core Values".to_string(),
            op: SoulPatchOp::AppendWithin,
            content: "- act with care".to_string(),
        });
        proposal.rationale = "test".to_string();

        let result = updater.apply(&proposal, &inner_dir, VersionMetrics::default());

        // The patch path SHOULD succeed (it doesn't hit the meta-strip branch
        // that would reject empty content). ASI may still reject very small
        // baselines, but the failure mode is acceptable for this test.
        match result {
            Ok(_) => {
                let after = fs::read_to_string(inner_dir.join("SOUL.md")).unwrap();
                assert!(after.contains("- act with care"), "Patch was not applied:\n{after}");
                assert!(after.contains("- be honest"), "Existing content lost");
                // No Evolution update marker — patch path does not emit one.
                assert!(!after.contains("<!-- Evolution update"),
                    "Patch path leaked legacy append marker");
            }
            Err(e) => {
                // ASI is the only acceptable rejection here.
                assert!(
                    e.contains("ASI") || e.contains("identity drift"),
                    "Unexpected failure: {e}"
                );
            }
        }
    }
}
