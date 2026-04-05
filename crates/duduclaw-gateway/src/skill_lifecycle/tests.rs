//! Tests for the skill lifecycle pipeline.

#[cfg(test)]
mod compression_tests {
    use crate::skill_lifecycle::compression::*;

    #[test]
    fn compress_with_description() {
        let c = CompressedSkill::compress("rust_expert", "Use idiomatic Rust patterns.", Some("Rust programming expertise"));
        assert_eq!(c.name, "rust_expert");
        assert_eq!(c.tag, "rust_expert");
        assert_eq!(c.summary, "Rust programming expertise");
        assert!(c.tokens_layer0 > 0);
        assert!(c.tokens_layer1 > 0);
        assert!(c.tokens_layer2 > 0);
        assert!(c.tokens_layer0 < c.tokens_layer1);
        assert!(c.tokens_layer1 <= c.tokens_layer2);
    }

    #[test]
    fn compress_without_description() {
        let content = "First line of skill.\nSecond line of skill.\nThird line.";
        let c = CompressedSkill::compress("my_skill", content, None);
        assert!(c.summary.contains("First line"));
        assert!(c.summary.contains("Second line"));
    }

    #[test]
    fn cache_refresh_and_query() {
        let mut cache = CompressedSkillCache::new();
        assert!(cache.is_empty());

        cache.refresh(&[
            ("skill1".into(), "content1".into(), Some("desc1".into())),
            ("skill2".into(), "content2".into(), None),
        ]);

        assert_eq!(cache.len(), 2);
        assert!(cache.get("skill1").is_some());
        assert!(cache.get("nonexistent").is_none());
        assert_eq!(cache.all().len(), 2);
    }
}

#[cfg(test)]
mod relevance_tests {
    use crate::skill_lifecycle::compression::CompressedSkill;
    use crate::skill_lifecycle::relevance::*;

    fn make_skill(name: &str, content: &str) -> CompressedSkill {
        CompressedSkill::compress(name, content, None)
    }

    #[test]
    fn rank_returns_relevant_first() {
        let skills = vec![
            make_skill("rust_expert", "Rust programming tokio async await cargo"),
            make_skill("cooking", "recipe ingredients oven temperature"),
        ];
        let ranked = rank_skills("How do I use tokio in Rust?", &skills);
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].0, 0); // rust_expert first
    }

    #[test]
    fn rank_empty_message() {
        let skills = vec![make_skill("s1", "content")];
        assert!(rank_skills("", &skills).is_empty());
    }

    #[test]
    fn rank_no_skills() {
        assert!(rank_skills("hello", &[]).is_empty());
    }

    #[test]
    fn rank_cjk_message() {
        let skills = vec![
            make_skill("zh_tone", "\u{7e41}\u{9ad4}\u{4e2d}\u{6587}\u{56de}\u{8986}\u{98a8}\u{683c}"),
        ];
        // 繁體中文回覆
        let ranked = rank_skills("\u{8acb}\u{7528}\u{7e41}\u{9ad4}\u{4e2d}\u{6587}\u{56de}\u{7b54}", &skills);
        assert!(!ranked.is_empty());
    }

    #[test]
    fn select_layers_active_skills_get_layer2() {
        let skills = vec![
            make_skill("active_one", "some content here"),
            make_skill("passive_one", "other content here"),
        ];
        let mut active = std::collections::HashSet::new();
        active.insert("active_one".to_string());

        let ranked = vec![(0, 0.3), (1, 0.2)];
        let config = RelevanceConfig::default();
        let selection = select_layers(&ranked, &active, &skills, &config);

        assert!(selection.layer2.contains(&0)); // active skill in Layer 2
    }
}

#[cfg(test)]
mod diagnostician_tests {
    use crate::prediction::engine::{ErrorCategory, Prediction, PredictionError};
    use crate::prediction::metrics::ConversationMetrics;
    use crate::skill_lifecycle::compression::CompressedSkill;
    use crate::skill_lifecycle::diagnostician::*;
    use chrono::Utc;

    fn make_error(composite: f64, corrections: u32, follow_ups: u32, topics: Vec<String>) -> PredictionError {
        PredictionError {
            delta_satisfaction: 0.2,
            topic_surprise: if topics.is_empty() { 0.0 } else { 0.6 },
            unexpected_correction: corrections > 0,
            unexpected_follow_up: follow_ups > 2,
            task_completion_failure: false,
            composite_error: composite,
            category: ErrorCategory::Moderate,
            prediction: Prediction {
                expected_satisfaction: 0.7,
                expected_follow_up_rate: 0.2,
                expected_topic: None,
                confidence: 0.5,
                timestamp: Utc::now(),
            },
            actual: ConversationMetrics {
                session_id: "s".into(), user_id: "u".into(), agent_id: "a".into(),
                message_count: 4, user_message_count: 2, assistant_message_count: 2,
                avg_assistant_response_length: 200.0, total_tokens: 100, response_time_ms: 0,
                user_follow_ups: follow_ups, user_corrections: corrections,
                feedback_details: Default::default(),
                detected_language: "en".into(), extracted_topics: topics,
                ended_naturally: true, feedback_signal: None, timestamp: Utc::now(),
                user_text: String::new(),
            },
        }
    }

    #[test]
    fn negligible_error_returns_none() {
        let error = make_error(0.1, 0, 0, vec![]);
        assert!(diagnose(&error, &[]).is_none());
    }

    #[test]
    fn correction_with_matching_skill_suggests_it() {
        let skills = vec![CompressedSkill::compress("precision", "accurate precise exact correct", None)];
        let error = make_error(0.4, 1, 0, vec!["precise".into()]);
        let diag = diagnose(&error, &skills).unwrap();
        assert!(!diag.suggested_skills.is_empty());
    }

    #[test]
    fn no_matching_skill_produces_gap() {
        let error = make_error(0.5, 0, 3, vec!["quantum_physics".into()]);
        let diag = diagnose(&error, &[]).unwrap();
        assert!(diag.skill_gap.is_some());
        assert!(diag.skill_gap.unwrap().suggested_name.contains("quantum"));
    }
}

#[cfg(test)]
mod activation_tests {
    use crate::skill_lifecycle::activation::*;

    #[test]
    fn activate_and_get_active() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.activate("agent1", "skill_a", 0.5);
        let active = ctrl.get_active("agent1");
        assert!(active.contains("skill_a"));
    }

    #[test]
    fn deactivate_removes_skill() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.activate("agent1", "skill_a", 0.5);
        ctrl.deactivate("agent1", "skill_a");
        assert!(ctrl.get_active("agent1").is_empty());
    }

    #[test]
    fn record_conversation_updates_stats() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.activate("agent1", "skill_a", 0.5);
        for _ in 0..10 {
            ctrl.record_conversation("agent1", 0.5); // error stays same as trigger
        }
        // Skill not helping → should be deactivated
        let deactivated = ctrl.evaluate_all("agent1");
        assert!(deactivated.contains(&"skill_a".to_string()));
    }

    #[test]
    fn effective_skill_stays_active() {
        let mut ctrl = SkillActivationController::new(5);
        ctrl.activate("agent1", "skill_a", 0.5);
        for _ in 0..10 {
            ctrl.record_conversation("agent1", 0.2); // error much lower than trigger
        }
        let deactivated = ctrl.evaluate_all("agent1");
        assert!(deactivated.is_empty());
    }

    #[test]
    fn max_active_evicts_worst() {
        let mut ctrl = SkillActivationController::new(2);
        ctrl.activate("agent1", "skill_a", 0.3);
        ctrl.activate("agent1", "skill_b", 0.3);
        // Fill up stats for skill_a (worse performer)
        for _ in 0..5 {
            ctrl.record_conversation("agent1", 0.4);
        }
        // Adding a third should evict skill_a (highest post error)
        ctrl.activate("agent1", "skill_c", 0.3);
        let active = ctrl.get_active("agent1");
        assert!(active.contains("skill_c"));
        assert_eq!(active.len(), 2);
    }
}

#[cfg(test)]
mod lift_tests {
    use crate::skill_lifecycle::lift::*;

    #[test]
    fn lift_positive_when_skill_helps() {
        let mut tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        for _ in 0..15 {
            tracker.record_with(0.1);    // low error with skill
            tracker.record_without(0.4); // high error without
        }
        assert!(tracker.lift() > 0.0);
        assert!(tracker.is_mature() || tracker.load_count >= 15);
    }

    #[test]
    fn lift_zero_when_insufficient_data() {
        let tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        assert_eq!(tracker.lift(), 0.0);
    }

    #[test]
    fn is_stable_requires_low_variance() {
        let mut tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        for _ in 0..25 {
            tracker.record_with(0.15); // very consistent
        }
        assert!(tracker.is_stable());
    }
}

#[cfg(test)]
mod distillation_tests {
    use crate::skill_lifecycle::compression::CompressedSkill;
    use crate::skill_lifecycle::distillation::*;
    use crate::skill_lifecycle::lift::SkillLiftTracker;

    #[test]
    fn high_readiness_when_mature_and_effective() {
        let mut tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        tracker.load_count = 60;
        for _ in 0..15 {
            tracker.record_with(0.1);
            tracker.record_without(0.4);
        }
        let candidate = DistillationCandidate::from_tracker(&tracker);
        assert!(candidate.readiness > DISTILLATION_THRESHOLD);
        assert!(candidate.is_ready());
    }

    #[test]
    fn low_readiness_when_immature() {
        let tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        let candidate = DistillationCandidate::from_tracker(&tracker);
        assert!(!candidate.is_ready());
    }

    #[test]
    fn scan_returns_only_ready_candidates() {
        let mut t1 = SkillLiftTracker::new("mature".into(), "a1".into());
        t1.load_count = 60;
        for _ in 0..15 {
            t1.record_with(0.1);
            t1.record_without(0.4);
        }

        let t2 = SkillLiftTracker::new("immature".into(), "a1".into());

        let candidates = scan_for_distillation("a1", &[&t1, &t2]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].skill_name, "mature");
    }

    #[test]
    fn build_distillation_input_has_xml_tags() {
        let skill = CompressedSkill::compress("s1", "skill content", None);
        let mut tracker = SkillLiftTracker::new("s1".into(), "a1".into());
        tracker.load_count = 60;
        for _ in 0..15 { tracker.record_with(0.1); tracker.record_without(0.4); }
        let candidate = DistillationCandidate::from_tracker(&tracker);

        let input = build_distillation_input(&skill, &candidate, "current soul");
        assert!(input.trigger_context.contains("<skill_to_distill>"));
        assert!(input.trigger_context.contains("</skill_to_distill>"));
        assert!(input.trigger_context.contains("2-5"));
    }
}
