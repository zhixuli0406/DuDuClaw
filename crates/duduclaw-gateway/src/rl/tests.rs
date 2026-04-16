#[cfg(test)]
mod rl_tests {
    use std::collections::HashMap;

    use chrono::Utc;

    use crate::rl::reward::*;
    use crate::rl::trajectory_export::*;
    use crate::rl::types::*;

    /// Helper to build a test trajectory with configurable parameters.
    fn make_test_trajectory(
        agent_tokens: u32,
        env_tokens: u32,
        tool_calls: usize,
        successful: usize,
        reward: f64,
    ) -> RLTrajectory {
        let mut turns = Vec::new();

        // Agent turn with tool calls
        let tools: Vec<ToolCallRecord> = (0..tool_calls)
            .map(|i| ToolCallRecord {
                name: format!("tool_{}", i),
                arguments: serde_json::json!({"arg": i}),
                result: serde_json::json!({"ok": true}),
                success: i < successful,
            })
            .collect();

        turns.push(RLTurn {
            role: TurnRole::AgentAction,
            content: "agent response".to_string(),
            tool_calls: if tool_calls > 0 {
                Some(tools)
            } else {
                None
            },
            token_count: agent_tokens,
            is_agent_generated: true,
        });

        // Environment turn
        turns.push(RLTurn {
            role: TurnRole::EnvironmentFeedback,
            content: "tool result".to_string(),
            tool_calls: None,
            token_count: env_tokens,
            is_agent_generated: false,
        });

        RLTrajectory {
            trajectory_id: "test_traj_001".to_string(),
            agent_id: "test_agent".to_string(),
            model_id: "test_model".to_string(),
            turns,
            total_tokens: (agent_tokens + env_tokens) as u64,
            outcome_reward: reward,
            metadata: HashMap::new(),
            created_at: Utc::now(),
        }
    }

    // --- Types tests ---

    #[test]
    fn test_agent_tokens_count() {
        let traj = make_test_trajectory(100, 50, 0, 0, 1.0);
        assert_eq!(traj.agent_tokens(), 100);
    }

    #[test]
    fn test_environment_tokens_count() {
        let traj = make_test_trajectory(100, 50, 0, 0, 1.0);
        assert_eq!(traj.environment_tokens(), 50);
    }

    #[test]
    fn test_total_tool_calls() {
        let traj = make_test_trajectory(100, 50, 5, 3, 1.0);
        assert_eq!(traj.total_tool_calls(), 5);
    }

    #[test]
    fn test_successful_tool_calls() {
        let traj = make_test_trajectory(100, 50, 5, 3, 1.0);
        assert_eq!(traj.successful_tool_calls(), 3);
    }

    #[test]
    fn test_no_tool_calls() {
        let traj = make_test_trajectory(100, 50, 0, 0, 1.0);
        assert_eq!(traj.total_tool_calls(), 0);
        assert_eq!(traj.successful_tool_calls(), 0);
    }

    // --- Export tests ---

    #[test]
    fn test_build_trajectory_role_mapping() {
        let messages = vec![
            ("user".to_string(), "hello".to_string()),
            ("assistant".to_string(), "hi there".to_string()),
            ("tool".to_string(), "result data".to_string()),
            ("system".to_string(), "system msg".to_string()),
        ];

        let traj =
            TrajectoryExporter::build_trajectory("sess_12345678", "agent1", "model1", &messages, 1.0);

        assert_eq!(traj.turns[0].role, TurnRole::UserMessage);
        assert_eq!(traj.turns[1].role, TurnRole::AgentAction);
        assert_eq!(traj.turns[2].role, TurnRole::EnvironmentFeedback);
        assert_eq!(traj.turns[3].role, TurnRole::EnvironmentFeedback);
    }

    #[test]
    fn test_build_trajectory_agent_flag() {
        let messages = vec![
            ("user".to_string(), "hello".to_string()),
            ("assistant".to_string(), "response".to_string()),
            ("tool".to_string(), "result".to_string()),
        ];

        let traj =
            TrajectoryExporter::build_trajectory("sess_12345678", "agent1", "model1", &messages, 0.5);

        assert!(!traj.turns[0].is_agent_generated); // user
        assert!(traj.turns[1].is_agent_generated); // assistant
        assert!(!traj.turns[2].is_agent_generated); // tool
    }

    #[test]
    fn test_token_estimation_ascii() {
        // 12 ASCII chars -> (12 + 3) / 4 = 3 tokens
        let tokens = estimate_tokens("hello world!");
        assert_eq!(tokens, 3);
    }

    #[test]
    fn test_token_estimation_cjk() {
        // 3 CJK chars -> 3 tokens each
        let tokens = estimate_tokens("\u{4F60}\u{597D}\u{554A}"); // ni hao a
        assert_eq!(tokens, 3);
    }

    #[test]
    fn test_token_estimation_mixed() {
        // "hello" (5 ASCII) + "你好" (2 CJK) = (5+3)/4 + 2 = 2 + 2 = 4
        let tokens = estimate_tokens("hello\u{4F60}\u{597D}");
        assert_eq!(tokens, 4);
    }

    #[test]
    fn test_token_estimation_empty() {
        let tokens = estimate_tokens("");
        // (0 + 3) / 4 = 0 (integer division)
        assert_eq!(tokens, 0);
    }

    #[test]
    fn test_write_trajectory_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let exporter = TrajectoryExporter::new(tmp.path().to_path_buf());

        let traj = make_test_trajectory(100, 50, 2, 1, 1.0);
        let path = exporter.write_trajectory(&traj).unwrap();

        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test_traj_001"));
        assert!(content.ends_with('\n'));
    }

    #[test]
    fn test_stats_counts_files() {
        let tmp = tempfile::tempdir().unwrap();
        let exporter = TrajectoryExporter::new(tmp.path().to_path_buf());

        // Write two trajectories
        let traj1 = make_test_trajectory(100, 50, 0, 0, 1.0);
        exporter.write_trajectory(&traj1).unwrap();

        let mut traj2 = make_test_trajectory(200, 100, 0, 0, 0.5);
        traj2.trajectory_id = "test_traj_002".to_string();
        exporter.write_trajectory(&traj2).unwrap();

        let stats = exporter.stats("test_agent");
        assert_eq!(stats.trajectory_count, 2);
    }

    // --- Reward tests ---

    #[test]
    fn test_outcome_reward() {
        let reward = OutcomeReward;
        let traj = make_test_trajectory(100, 50, 0, 0, 0.8);
        assert!((reward.compute(&traj) - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn test_tool_efficiency_reward() {
        let reward = ToolEfficiencyReward::new(5.0);

        // 5 calls with baseline 5 -> reward = 1.0
        let traj = make_test_trajectory(100, 50, 5, 5, 1.0);
        assert!((reward.compute(&traj) - 1.0).abs() < f64::EPSILON);

        // 10 calls with baseline 5 -> reward = 0.5
        let mut traj_many = make_test_trajectory(100, 50, 10, 10, 1.0);
        // Rebuild with 10 tool calls
        let tools: Vec<ToolCallRecord> = (0..10)
            .map(|i| ToolCallRecord {
                name: format!("tool_{}", i),
                arguments: serde_json::json!({}),
                result: serde_json::json!({}),
                success: true,
            })
            .collect();
        traj_many.turns[0].tool_calls = Some(tools);
        assert!((reward.compute(&traj_many) - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_tool_efficiency_no_calls() {
        let reward = ToolEfficiencyReward::new(5.0);
        let traj = make_test_trajectory(100, 50, 0, 0, 1.0);
        assert!((reward.compute(&traj) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_overlong_no_penalty_under_threshold() {
        let reward = SoftOverlongPunishment::new(1000, 0.5);
        let traj = make_test_trajectory(400, 400, 0, 0, 1.0);
        assert!((reward.compute(&traj) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_overlong_penalty_over_threshold() {
        let reward = SoftOverlongPunishment::new(1000, 0.5);
        // 1500 tokens, threshold 1000 -> penalty = -0.5 * 500 / 1000 = -0.25
        let mut traj = make_test_trajectory(1000, 500, 0, 0, 1.0);
        traj.total_tokens = 1500;
        assert!((reward.compute(&traj) - (-0.25)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_composite_reward_default() {
        let reward = CompositeReward::default_config();
        assert_eq!(reward.name(), "composite");

        let traj = make_test_trajectory(100, 50, 0, 0, 1.0);
        let score = reward.compute(&traj);
        // outcome=1.0*0.7 + efficiency=0.0*0.2 + overlong=0.0*0.1 = 0.7
        assert!((score - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn test_composite_reward_weights() {
        let reward = CompositeReward::new()
            .add(Box::new(OutcomeReward), 0.5)
            .add(Box::new(OutcomeReward), 0.5);

        let traj = make_test_trajectory(100, 50, 0, 0, 0.6);
        let score = reward.compute(&traj);
        // 0.6*0.5 + 0.6*0.5 = 0.6
        assert!((score - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trajectory_serialization_roundtrip() {
        let traj = make_test_trajectory(100, 50, 2, 1, 0.9);
        let json = serde_json::to_string(&traj).unwrap();
        let deserialized: RLTrajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.trajectory_id, traj.trajectory_id);
        assert_eq!(deserialized.turns.len(), traj.turns.len());
        assert!((deserialized.outcome_reward - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_turn_role_serde() {
        let role = TurnRole::AgentAction;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"agent_action\"");

        let parsed: TurnRole = serde_json::from_str("\"environment_feedback\"").unwrap();
        assert_eq!(parsed, TurnRole::EnvironmentFeedback);
    }
}
