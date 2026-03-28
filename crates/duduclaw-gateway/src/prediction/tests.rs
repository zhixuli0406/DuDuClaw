//! Unit tests for the prediction-error-driven evolution engine (Phase 1).

#[cfg(test)]
mod running_stats_tests {
    use crate::prediction::user_model::RunningStats;

    #[test]
    fn empty_stats() {
        let s = RunningStats::default();
        assert_eq!(s.sample_count(), 0);
        assert_eq!(s.mean(), 0.0);
        assert_eq!(s.variance(), 0.0);
        assert_eq!(s.std_dev(), 0.0);
    }

    #[test]
    fn single_value() {
        let mut s = RunningStats::default();
        s.push(42.0);
        assert_eq!(s.sample_count(), 1);
        assert!((s.mean() - 42.0).abs() < f64::EPSILON);
        assert_eq!(s.variance(), 0.0);
    }

    #[test]
    fn known_sequence() {
        let mut s = RunningStats::default();
        for v in [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0] {
            s.push(v);
        }
        assert_eq!(s.sample_count(), 8);
        assert!((s.mean() - 5.0).abs() < 1e-10);
        // Population variance of [2,4,4,4,5,5,7,9] = 4.0
        assert!((s.variance() - 4.0).abs() < 1e-10);
        assert!((s.std_dev() - 2.0).abs() < 1e-10);
    }

    #[test]
    fn large_n_stability() {
        let mut s = RunningStats::default();
        for i in 0..100_000 {
            s.push(i as f64);
        }
        assert_eq!(s.sample_count(), 100_000);
        // Mean of 0..99999 = 49999.5
        assert!((s.mean() - 49999.5).abs() < 1e-6);
    }
}

#[cfg(test)]
mod user_model_tests {
    use crate::prediction::metrics::ConversationMetrics;
    use crate::prediction::user_model::UserModel;
    use chrono::Utc;

    fn make_metrics(follow_ups: u32, corrections: u32, response_len: f64) -> ConversationMetrics {
        ConversationMetrics {
            session_id: "test-session".to_string(),
            user_id: "test-user".to_string(),
            agent_id: "test-agent".to_string(),
            message_count: 4,
            user_message_count: 2,
            assistant_message_count: 2,
            avg_assistant_response_length: response_len,
            total_tokens: 100,
            response_time_ms: 0,
            user_follow_ups: follow_ups,
            user_corrections: corrections,
            detected_language: "zh".to_string(),
            extracted_topics: vec!["test".to_string()],
            ended_naturally: true,
            feedback_signal: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn cold_start_confidence() {
        let model = UserModel::new("u1".into(), "a1".into());
        assert_eq!(model.confidence(), 0.0);
        assert_eq!(model.total_conversations, 0);
    }

    #[test]
    fn update_from_metrics_increases_conversations() {
        let mut model = UserModel::new("u1".into(), "a1".into());
        let m = make_metrics(0, 0, 200.0);
        model.update_from_metrics(&m);
        assert_eq!(model.total_conversations, 1);
        assert!((model.preferred_response_length.mean() - 200.0).abs() < 1e-6);
    }

    #[test]
    fn feedback_updates_satisfaction() {
        let mut model = UserModel::new("u1".into(), "a1".into());
        model.update_from_feedback("positive");
        assert!((model.avg_satisfaction.mean() - 1.0).abs() < 1e-6);

        model.update_from_feedback("negative");
        assert!((model.avg_satisfaction.mean() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn correction_increases_correction_rate() {
        let mut model = UserModel::new("u1".into(), "a1".into());
        let m = make_metrics(0, 2, 100.0);
        model.update_from_metrics(&m);
        // correction_ratio = 2/2 = 1.0
        assert!(model.correction_rate.mean() > 0.0);
    }

    #[test]
    fn confidence_grows_with_conversations() {
        let mut model = UserModel::new("u1".into(), "a1".into());
        for _ in 0..50 {
            model.update_from_metrics(&make_metrics(0, 0, 100.0));
        }
        assert!((model.confidence() - 1.0).abs() < 1e-6);
    }
}

#[cfg(test)]
mod metrics_tests {
    use crate::prediction::metrics::{extract_keywords, ConversationMetrics};
    use crate::session::SessionMessage;

    fn msg(role: &str, content: &str) -> SessionMessage {
        SessionMessage {
            role: role.to_string(),
            content: content.to_string(),
            tokens: 10,
            timestamp: "2026-03-27T10:00:00Z".to_string(),
        }
    }

    #[test]
    fn message_counts() {
        let messages = vec![
            msg("user", "hello"),
            msg("assistant", "hi there, how can I help?"),
            msg("user", "tell me about rust"),
            msg("assistant", "Rust is a systems programming language"),
        ];
        let m = ConversationMetrics::extract("s1", "a1", "u1", &messages, 0);
        assert_eq!(m.user_message_count, 2);
        assert_eq!(m.assistant_message_count, 2);
        assert_eq!(m.message_count, 4);
    }

    #[test]
    fn follow_up_detection() {
        let messages = vec![
            msg("user", "explain X"),
            msg("assistant", "X is blah blah blah blah blah blah"),
            msg("user", "what?"),  // short follow-up
        ];
        let m = ConversationMetrics::extract("s1", "a1", "u1", &messages, 0);
        assert_eq!(m.user_follow_ups, 1);
    }

    #[test]
    fn correction_detection() {
        let messages = vec![
            msg("user", "\u{4e0d}\u{662f}\u{ff0c}\u{6211}\u{8981}\u{7684}\u{4e0d}\u{662f}\u{9019}\u{500b}"), // 不是，我要的不是這個
            msg("assistant", "sorry"),
        ];
        let m = ConversationMetrics::extract("s1", "a1", "u1", &messages, 0);
        assert!(m.user_corrections >= 1);
    }

    #[test]
    fn keyword_extraction_ascii() {
        let keywords = extract_keywords("rust programming language systems safety", 3);
        assert!(!keywords.is_empty());
        assert!(keywords.len() <= 3);
    }

    #[test]
    fn keyword_extraction_cjk() {
        // 機器學習很有趣
        let keywords = extract_keywords("\u{6a5f}\u{5668}\u{5b78}\u{7fd2}\u{5f88}\u{6709}\u{8da3}", 3);
        assert!(!keywords.is_empty());
    }

    #[test]
    fn language_detection() {
        let messages = vec![
            msg("user", "\u{4f60}\u{597d}\u{ff0c}\u{8acb}\u{554f}\u{9019}\u{662f}\u{4ec0}\u{9ebc}"), // 你好，請問這是什麼
            msg("assistant", "response"),
        ];
        let m = ConversationMetrics::extract("s1", "a1", "u1", &messages, 0);
        assert_eq!(m.detected_language, "zh");
    }
}

#[cfg(test)]
mod router_tests {
    use crate::prediction::engine::{ErrorCategory, Prediction, PredictionError};
    use crate::prediction::metrics::ConversationMetrics;
    use crate::prediction::router::{route, EvolutionAction};
    use chrono::Utc;

    fn make_error(composite: f64, category: ErrorCategory) -> PredictionError {
        PredictionError {
            delta_satisfaction: 0.0,
            topic_surprise: 0.0,
            unexpected_correction: false,
            unexpected_follow_up: false,
            composite_error: composite,
            category,
            prediction: Prediction {
                expected_satisfaction: 0.7,
                expected_follow_up_rate: 0.3,
                expected_topic: None,
                confidence: 0.5,
                timestamp: Utc::now(),
            },
            actual: ConversationMetrics {
                session_id: "s".into(),
                user_id: "u".into(),
                agent_id: "a".into(),
                message_count: 2,
                user_message_count: 1,
                assistant_message_count: 1,
                avg_assistant_response_length: 100.0,
                total_tokens: 50,
                response_time_ms: 0,
                user_follow_ups: 0,
                user_corrections: 0,
                detected_language: "en".into(),
                extracted_topics: vec![],
                ended_naturally: true,
                feedback_signal: None,
                timestamp: Utc::now(),
            },
        }
    }

    #[test]
    fn negligible_routes_to_none() {
        let error = make_error(0.1, ErrorCategory::Negligible);
        assert!(matches!(route(&error, 0), EvolutionAction::None));
    }

    #[test]
    fn moderate_routes_to_store_episodic() {
        let error = make_error(0.35, ErrorCategory::Moderate);
        assert!(matches!(route(&error, 0), EvolutionAction::StoreEpisodic { .. }));
    }

    #[test]
    fn significant_routes_to_reflection() {
        let error = make_error(0.6, ErrorCategory::Significant);
        assert!(matches!(route(&error, 0), EvolutionAction::TriggerReflection { .. }));
    }

    #[test]
    fn significant_with_consecutive_escalates() {
        let error = make_error(0.6, ErrorCategory::Significant);
        assert!(matches!(route(&error, 3), EvolutionAction::TriggerEmergencyEvolution { .. }));
    }

    #[test]
    fn critical_routes_to_emergency() {
        let error = make_error(0.9, ErrorCategory::Critical);
        assert!(matches!(route(&error, 0), EvolutionAction::TriggerEmergencyEvolution { .. }));
    }
}

#[cfg(test)]
mod metacognition_tests {
    use crate::prediction::engine::{ErrorCategory, PredictionError, Prediction};
    use crate::prediction::metacognition::{AdaptiveThresholds, MetaCognition};
    use crate::prediction::metrics::ConversationMetrics;
    use chrono::Utc;

    #[test]
    fn default_thresholds() {
        let t = AdaptiveThresholds::default();
        assert_eq!(t.category_for(0.1), ErrorCategory::Negligible);
        assert_eq!(t.category_for(0.3), ErrorCategory::Moderate);
        assert_eq!(t.category_for(0.6), ErrorCategory::Significant);
        assert_eq!(t.category_for(0.9), ErrorCategory::Critical);
    }

    #[test]
    fn threshold_boundaries() {
        let t = AdaptiveThresholds::default();
        // At exact boundary values
        assert_eq!(t.category_for(0.0), ErrorCategory::Negligible);
        assert_eq!(t.category_for(0.2), ErrorCategory::Moderate); // >= 0.2
        assert_eq!(t.category_for(0.5), ErrorCategory::Significant); // >= 0.5
        assert_eq!(t.category_for(0.8), ErrorCategory::Critical); // >= 0.8
    }

    fn make_pred_error(category: ErrorCategory) -> PredictionError {
        PredictionError {
            delta_satisfaction: 0.0,
            topic_surprise: 0.0,
            unexpected_correction: false,
            unexpected_follow_up: false,
            composite_error: 0.5,
            category,
            prediction: Prediction {
                expected_satisfaction: 0.7,
                expected_follow_up_rate: 0.3,
                expected_topic: None,
                confidence: 0.5,
                timestamp: Utc::now(),
            },
            actual: ConversationMetrics {
                session_id: "s".into(), user_id: "u".into(), agent_id: "a".into(),
                message_count: 2, user_message_count: 1, assistant_message_count: 1,
                avg_assistant_response_length: 100.0, total_tokens: 50, response_time_ms: 0,
                user_follow_ups: 0, user_corrections: 0,
                detected_language: "en".into(), extracted_topics: vec![],
                ended_naturally: true, feedback_signal: None, timestamp: Utc::now(),
            },
        }
    }

    #[test]
    fn evaluation_after_interval() {
        let mut mc = MetaCognition::default();
        assert!(!mc.should_evaluate());

        for _ in 0..100 {
            mc.record_prediction(&make_pred_error(ErrorCategory::Significant));
        }
        assert!(mc.should_evaluate());
    }

    #[test]
    fn low_effectiveness_raises_thresholds() {
        let mut mc = MetaCognition::default();
        let initial_moderate = mc.thresholds.moderate_upper;

        // Record many Significant triggers with outcomes that show no improvement
        for _ in 0..10 {
            mc.record_prediction(&make_pred_error(ErrorCategory::Significant));
            mc.record_outcome(ErrorCategory::Significant, false); // not improved
        }
        // All outcomes negative → improvement_rate = 0.0, window_count = 10 >= 5

        mc.evaluate_and_adjust();
        assert!(mc.thresholds.moderate_upper > initial_moderate);
    }

    #[test]
    fn thresholds_clamped() {
        let mut mc = MetaCognition::default();
        mc.thresholds.negligible_upper = 0.01;
        mc.thresholds.moderate_upper = 0.02;
        mc.thresholds.significant_upper = 0.03;

        mc.evaluate_and_adjust();

        assert!(mc.thresholds.negligible_upper >= 0.1);
        assert!(mc.thresholds.moderate_upper >= 0.2);
        assert!(mc.thresholds.significant_upper >= 0.4);
    }

    #[test]
    fn ordering_maintained() {
        let mut mc = MetaCognition::default();
        mc.thresholds.negligible_upper = 0.5;
        mc.thresholds.moderate_upper = 0.5;
        mc.thresholds.significant_upper = 0.5;

        mc.evaluate_and_adjust();

        assert!(mc.thresholds.negligible_upper < mc.thresholds.moderate_upper);
        assert!(mc.thresholds.moderate_upper < mc.thresholds.significant_upper);
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let mc = MetaCognition::default();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        mc.persist(&path);
        let loaded = MetaCognition::load(&path).unwrap();

        assert!((loaded.thresholds.negligible_upper - mc.thresholds.negligible_upper).abs() < f64::EPSILON);
        assert!((loaded.thresholds.moderate_upper - mc.thresholds.moderate_upper).abs() < f64::EPSILON);
        assert_eq!(loaded.evaluation_interval, mc.evaluation_interval);
    }
}
