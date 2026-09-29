//! Activity-derived signal tests: the `(state, action)` visit graph (A2)
//! and the in-round tool-call streak advisory (H10).

mod goal_visit_graph {
    use super::super::*;

    // ── peek_streak / commit_dispatch: unchanged-streak bookkeeping ──

    #[tokio::test]
    async fn peek_streak_starts_at_one_for_unknown_task() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.peek_streak("g1", "hash-a").await, 1);
    }

    #[tokio::test]
    async fn repeated_state_hash_increments_streak_on_commit() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 1);
        assert_eq!(
            g.peek_streak("g1", "hash-a").await,
            2,
            "peek must not mutate"
        );
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 2);
        assert_eq!(g.commit_dispatch("g1", "hash-a").await, 3);
    }

    #[tokio::test]
    async fn changed_state_hash_resets_streak() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g1", "hash-a").await;
        assert_eq!(g.peek_streak("g1", "hash-a").await, 3);
        // A different hash resets the streak, even mid-sequence.
        assert_eq!(g.commit_dispatch("g1", "hash-b").await, 1);
        assert_eq!(g.peek_streak("g1", "hash-b").await, 2);
    }

    #[tokio::test]
    async fn peek_does_not_mutate_state() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        // Repeated peeks must be idempotent (read-only).
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(g.peek_streak("g1", "hash-a").await, 2);
        assert_eq!(
            g.commit_dispatch("g1", "hash-a").await,
            2,
            "peeks left the real streak at 1"
        );
    }

    #[tokio::test]
    async fn tasks_are_tracked_independently() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g1", "hash-a").await;
        assert_eq!(
            g.peek_streak("g2", "hash-a").await,
            1,
            "g2 has no history yet"
        );
    }

    // ── record_round / has_repeated_action: pair bookkeeping ────

    #[tokio::test]
    async fn repeated_action_flag_requires_two_visits() {
        let g = GoalVisitGraph::new();
        assert!(!g.has_repeated_action("g1", "hash-a").await);
        g.record_round("g1", "hash-a", "action-1").await;
        assert!(
            !g.has_repeated_action("g1", "hash-a").await,
            "one visit is not yet a repeat"
        );
        g.record_round("g1", "hash-a", "action-1").await;
        assert!(
            g.has_repeated_action("g1", "hash-a").await,
            "two visits of the same pair IS a repeat"
        );
    }

    #[tokio::test]
    async fn distinct_actions_from_same_state_do_not_flag_as_repeated() {
        let g = GoalVisitGraph::new();
        g.record_round("g1", "hash-a", "action-1").await;
        g.record_round("g1", "hash-a", "action-2").await;
        assert!(
            !g.has_repeated_action("g1", "hash-a").await,
            "two DIFFERENT actions from the same state is exploration, not a loop"
        );
    }

    #[tokio::test]
    async fn record_round_returns_growing_visit_count() {
        let g = GoalVisitGraph::new();
        assert_eq!(g.record_round("g1", "h", "a").await, 1);
        assert_eq!(g.record_round("g1", "h", "a").await, 2);
        assert_eq!(g.record_round("g1", "h", "a").await, 3);
    }

    // ── clear_task: terminal-state cleanup ───────────────────

    #[tokio::test]
    async fn clear_task_drops_all_tracking() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.record_round("g1", "hash-a", "action-1").await;
        g.record_round("g1", "hash-a", "action-1").await;
        assert_eq!(g.task_count().await, 1);

        g.clear_task("g1").await;
        assert_eq!(g.task_count().await, 0);
        // Post-clear, the task behaves as brand new.
        assert_eq!(g.peek_streak("g1", "hash-a").await, 1);
        assert!(!g.has_repeated_action("g1", "hash-a").await);
    }

    #[tokio::test]
    async fn clear_task_is_a_noop_for_unknown_task() {
        let g = GoalVisitGraph::new();
        g.clear_task("ghost").await; // must not panic
        assert_eq!(g.task_count().await, 0);
    }

    #[tokio::test]
    async fn clear_task_does_not_affect_other_tasks() {
        let g = GoalVisitGraph::new();
        g.commit_dispatch("g1", "hash-a").await;
        g.commit_dispatch("g2", "hash-a").await;
        g.clear_task("g1").await;
        assert_eq!(g.task_count().await, 1);
        assert_eq!(g.peek_streak("g2", "hash-a").await, 2, "g2 untouched");
    }

    // ── action_digest ─────────────────────────────────────────

    #[test]
    fn action_digest_is_deterministic_and_missing_file_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let a = action_digest(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "did the thing",
        );
        let b = action_digest(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "did the thing",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn action_digest_differs_on_different_result_text() {
        let dir = tempfile::tempdir().unwrap();
        let a = action_digest(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "result one",
        );
        let b = action_digest(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "result two",
        );
        assert_ne!(a, b);
    }

    #[test]
    fn action_digest_reflects_tool_categories_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "web_fetch", "success": true}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:20:00Z", "tool_name": "bash", "success": true}),
            // Out of window — must not affect the digest.
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T05:00:00Z", "tool_name": "later_tool", "success": true}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();

        let with_tools = action_digest(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "same text",
        );
        let empty_dir = tempfile::tempdir().unwrap();
        let without_tools = action_digest(
            empty_dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
            "same text",
        );
        assert_ne!(
            with_tools, without_tools,
            "tool activity must change the digest even when result text is identical"
        );
    }

    #[test]
    fn tool_categories_filters_by_agent_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "web_fetch", "success": true}),
            serde_json::json!({"agent_id": "bob", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "bash", "success": true}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-02T00:10:00Z", "tool_name": "outside_window", "success": true}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();
        let cats = tool_categories(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
        );
        assert_eq!(cats, vec!["web_fetch".to_string()]);
    }
}

mod goal_tool_streak {
    use super::super::*;

    fn rec(tool: &str, input: Option<&str>) -> ToolActivityRecord {
        ToolActivityRecord {
            tool_name: tool.to_string(),
            success: true,
            result_text: None,
            input_text: input.map(String::from),
        }
    }

    // ── longest_streak: pure computation ─────────────────────

    #[test]
    fn empty_input_yields_none() {
        assert!(longest_streak(&[]).is_none());
    }

    #[test]
    fn single_call_is_a_streak_of_one() {
        let hit = longest_streak(&[rec("bash", Some("ls"))]).unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 1);
    }

    #[test]
    fn same_tool_same_params_consecutive_streak() {
        let records = vec![
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "web_fetch");
        assert_eq!(hit.len, 4);
    }

    #[test]
    fn different_params_interrupt_the_streak() {
        let records = vec![
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            // Different params — breaks the run even though the tool name matches.
            rec("web_fetch", Some("{\"url\":\"https://y.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
        ];
        let hit = longest_streak(&records).unwrap();
        // Longest run anywhere is 2 (the first two), not 4.
        assert_eq!(hit.len, 2);
    }

    #[test]
    fn different_tool_interrupts_the_streak_even_with_same_params() {
        let records = vec![
            rec("bash", Some("ls")),
            rec("bash", Some("ls")),
            rec("bash", Some("ls")),
            rec("Read", Some("ls")), // same "params" text, different tool
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 3);
    }

    #[test]
    fn later_run_wins_the_tie() {
        let records = vec![
            rec("bash", Some("a")),
            rec("bash", Some("a")),
            rec("bash", Some("a")), // run of 3
            rec("Read", Some("b")),
            rec("web_fetch", Some("c")),
            rec("web_fetch", Some("c")),
            rec("web_fetch", Some("c")), // also a run of 3, later
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(
            hit.tool_name, "web_fetch",
            "on a tie, the most recent run is the more actionable signal"
        );
        assert_eq!(hit.len, 3);
    }

    #[test]
    fn masked_params_stable_compare_ignores_incidental_whitespace() {
        // Same masked input modulo whitespace/formatting must still count as
        // "the same call" — mirrors goal_state::short_hash's own contract
        // (NFKC-normalize + collapse whitespace), which this module reuses
        // rather than re-deriving.
        let records = vec![
            rec("bash", Some("ls  -la")),
            rec("bash", Some("ls -la")),
            rec("bash", Some("  ls -la  ")),
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(
            hit.len, 3,
            "whitespace-only differences must not break the streak"
        );
    }

    #[test]
    fn missing_input_text_still_forms_a_stable_streak() {
        // A tool the audit writer never captured input for (`None`)
        // normalizes to the empty-string signature — repeated calls with no
        // captured params still legitimately stream together.
        let records = vec![
            rec("poll_tool", None),
            rec("poll_tool", None),
            rec("poll_tool", None),
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "poll_tool");
        assert_eq!(hit.len, 3);
    }

    // ── advisory_text: threshold ladder ───────────────────────

    #[test]
    fn below_lowest_threshold_yields_no_advisory() {
        assert!(
            advisory_text(&StreakHit {
                tool_name: "bash".into(),
                len: 2
            })
            .is_none()
        );
    }

    #[test]
    fn tier_3_text_suggests_rereading_the_result() {
        let text = advisory_text(&StreakHit {
            tool_name: "bash".into(),
            len: 3,
        })
        .unwrap();
        assert!(text.contains("3"));
        assert!(text.contains("bash"));
        assert!(
            text.contains("重讀"),
            "tier 3 must nudge toward re-reading the last result"
        );
    }

    #[test]
    fn tier_4_still_reads_as_tier_3_not_the_next_rung() {
        // 4 has crossed 3 but not yet 5 — must render the tier-3 text, not
        // silently jump ahead or fall back to nothing.
        let text = advisory_text(&StreakHit {
            tool_name: "bash".into(),
            len: 4,
        })
        .unwrap();
        assert!(text.contains("重讀"));
    }

    #[test]
    fn tier_5_text_suggests_changing_approach() {
        let text = advisory_text(&StreakHit {
            tool_name: "bash".into(),
            len: 5,
        })
        .unwrap();
        assert!(text.contains("換一個方法") || text.contains("換個方法"));
    }

    #[test]
    fn tier_8_text_strongly_suggests_converging_or_asking_for_help() {
        let text = advisory_text(&StreakHit {
            tool_name: "bash".into(),
            len: 8,
        })
        .unwrap();
        assert!(text.contains("tasks_block"));
        assert!(text.contains("強烈建議"));
    }

    #[test]
    fn tier_beyond_8_still_renders_the_tier_8_text() {
        let text = advisory_text(&StreakHit {
            tool_name: "bash".into(),
            len: 20,
        })
        .unwrap();
        assert!(text.contains("tasks_block"));
    }

    // ── detect_tool_streak: filesystem-backed integration ─────

    #[test]
    fn detect_tool_streak_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            detect_tool_streak(
                dir.path(),
                "alice",
                "2026-01-01T00:00:00Z",
                "2026-01-01T01:00:00Z"
            )
            .is_none()
        );
    }

    #[test]
    fn detect_tool_streak_scopes_to_agent_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:11:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:12:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            // Different agent — must not count toward alice's streak.
            serde_json::json!({"agent_id": "bob", "timestamp": "2026-01-01T00:12:30Z", "tool_name": "bash", "success": true, "input": "ls"}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();
        let hit = detect_tool_streak(
            dir.path(),
            "alice",
            "2026-01-01T00:00:00Z",
            "2026-01-01T01:00:00Z",
        )
        .unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 3);
    }
}
