use super::*;

#[cfg(test)]
mod todo_progress_tests {
    use super::*;

    #[test]
    fn parse_todo_write_input_valid() {
        let input = serde_json::json!({
            "todos": [
                { "content": "研究 API", "status": "completed", "activeForm": "研究中" },
                { "content": "實作轉換", "status": "in_progress", "activeForm": "實作中" },
                { "content": "寫測試", "status": "pending" }
            ]
        });
        let todos = parse_todo_write_input(&input).expect("should parse");
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].status, "completed");
        assert_eq!(todos[1].active_form.as_deref(), Some("實作中"));
        assert!(todos[2].active_form.is_none());
    }

    #[test]
    fn parse_todo_write_input_rejects_garbage() {
        assert!(parse_todo_write_input(&serde_json::json!({})).is_none());
        assert!(parse_todo_write_input(&serde_json::json!({"todos": []})).is_none());
        assert!(
            parse_todo_write_input(&serde_json::json!({"todos": [{"status": "pending"}]}))
                .is_none()
        );
        assert!(parse_todo_write_input(&serde_json::json!({"todos": "not-an-array"})).is_none());
    }

    #[test]
    fn render_todo_list_board() {
        let todos = vec![
            TodoItem {
                content: "完成的".into(),
                status: "completed".into(),
                active_form: None,
            },
            TodoItem {
                content: "進行的".into(),
                status: "in_progress".into(),
                active_form: Some("進行中".into()),
            },
            TodoItem {
                content: "待辦的".into(),
                status: "pending".into(),
                active_form: None,
            },
        ];
        let board = render_todo_list(&todos);
        assert!(board.contains("1/3 完成"));
        assert!(board.contains("✅ 完成的"));
        assert!(board.contains("🔄 進行中")); // in_progress uses activeForm
        assert!(board.contains("⬜ 待辦的"));
    }

    #[test]
    fn render_todo_list_caps_items() {
        let todos: Vec<TodoItem> = (0..20)
            .map(|i| TodoItem {
                content: format!("item{i}"),
                status: "pending".into(),
                active_form: None,
            })
            .collect();
        let board = render_todo_list(&todos);
        assert!(board.contains("及其他 8 項"));
    }

    #[test]
    fn todo_update_display_via_event() {
        let event = ProgressEvent::TodoUpdate {
            todos: vec![TodoItem {
                content: "x".into(),
                status: "pending".into(),
                active_form: None,
            }],
        };
        assert!(event.to_display().starts_with("📋"));
    }
}

#[cfg(test)]
mod step_tracker_tests {
    use super::*;
    use serde_json::json;

    /// Build an `assistant` stream-json event carrying one `tool_use` block.
    fn tool_use_event(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
        json!({
            "type": "assistant",
            "message": { "content": [ { "type": "tool_use", "id": id, "name": name, "input": input } ] }
        })
    }

    /// Build a `user` stream-json event carrying one `tool_result` block.
    fn tool_result_event(id: &str) -> serde_json::Value {
        json!({
            "type": "user",
            "message": { "content": [ { "type": "tool_result", "tool_use_id": id, "content": "ok" } ] }
        })
    }

    #[test]
    fn start_emits_step_with_summary_and_depth_zero() {
        let mut tr = StepTracker::new();
        let steps = tr.ingest(&tool_use_event(
            "t1",
            "Read",
            json!({ "file_path": "/etc/hosts" }),
        ));
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase, StepPhase::Start);
        assert_eq!(steps[0].tool, "Read");
        assert_eq!(steps[0].summary.as_deref(), Some("/etc/hosts"));
        assert_eq!(steps[0].depth, 0);
    }

    #[test]
    fn end_matches_open_call_by_id() {
        let mut tr = StepTracker::new();
        let _ = tr.ingest(&tool_use_event("t1", "Bash", json!({ "command": "ls" })));
        let ends = tr.ingest(&tool_result_event("t1"));
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].phase, StepPhase::End);
        assert_eq!(ends[0].tool, "Bash");
        assert!(ends[0].summary.is_none(), "end phase carries no summary");
        assert_eq!(ends[0].depth, 0);
    }

    #[test]
    fn nested_calls_increment_depth() {
        let mut tr = StepTracker::new();
        // Outer Task starts at depth 0…
        let outer = tr.ingest(&tool_use_event(
            "task1",
            "Task",
            json!({ "description": "sub" }),
        ));
        assert_eq!(outer[0].depth, 0);
        // …an inner Bash starts while Task is still open → depth 1.
        let inner = tr.ingest(&tool_use_event(
            "bash1",
            "Bash",
            json!({ "command": "make" }),
        ));
        assert_eq!(inner[0].depth, 1);
        // Inner resolves first, returning to depth 1.
        let inner_end = tr.ingest(&tool_result_event("bash1"));
        assert_eq!(inner_end[0].tool, "Bash");
        assert_eq!(inner_end[0].depth, 1);
        // Outer resolves, returning to depth 0.
        let outer_end = tr.ingest(&tool_result_event("task1"));
        assert_eq!(outer_end[0].tool, "Task");
        assert_eq!(outer_end[0].depth, 0);
    }

    #[test]
    fn non_tool_events_emit_nothing() {
        let mut tr = StepTracker::new();
        // Text-only assistant message.
        assert!(
            tr.ingest(&json!({
                "type": "assistant",
                "message": { "content": [ { "type": "text", "text": "hello" } ] }
            }))
            .is_empty()
        );
        // Thinking block.
        assert!(
            tr.ingest(&json!({
                "type": "assistant",
                "message": { "content": [ { "type": "thinking", "thinking": "…" } ] }
            }))
            .is_empty()
        );
        // Terminal result event.
        assert!(
            tr.ingest(&json!({ "type": "result", "subtype": "success", "result": "done" }))
                .is_empty()
        );
        // Unknown / system event.
        assert!(
            tr.ingest(&json!({ "type": "system", "subtype": "init" }))
                .is_empty()
        );
    }

    #[test]
    fn parallel_tool_uses_in_one_message_each_emit_a_start() {
        let mut tr = StepTracker::new();
        let event = json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "text", "text": "working" },
                { "type": "tool_use", "id": "a", "name": "Read", "input": { "file_path": "a.rs" } },
                { "type": "tool_use", "id": "b", "name": "Grep", "input": { "pattern": "foo" } }
            ] }
        });
        let steps = tr.ingest(&event);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].tool, "Read");
        assert_eq!(steps[0].depth, 0);
        assert_eq!(steps[1].tool, "Grep");
        assert_eq!(steps[1].depth, 1);
    }

    #[test]
    fn summary_is_cjk_safe_and_capped() {
        // 200 CJK chars — raw byte slicing at 120 would panic mid-char.
        let long = "指令".repeat(100);
        let steps =
            StepTracker::new().ingest(&tool_use_event("t1", "Bash", json!({ "command": long })));
        let summary = steps[0].summary.as_deref().expect("summary present");
        assert_eq!(summary.chars().count(), STEP_SUMMARY_CHAR_CAP);
    }

    #[test]
    fn summary_falls_back_to_key_list_then_none() {
        // No known field → comma-joined key list.
        let steps = StepTracker::new().ingest(&tool_use_event(
            "t1",
            "CustomTool",
            json!({ "alpha": 1, "beta": 2 }),
        ));
        let summary = steps[0].summary.as_deref().expect("fallback summary");
        assert!(summary.contains("alpha") && summary.contains("beta"));
        // Empty input object → no summary.
        let steps2 = StepTracker::new().ingest(&tool_use_event("t2", "NoArgs", json!({})));
        assert!(steps2[0].summary.is_none());
    }

    #[test]
    fn to_display_is_empty_for_step_variant() {
        let ev = ProgressEvent::Step(StepEvent {
            phase: StepPhase::Start,
            tool: "Read".into(),
            summary: Some("x".into()),
            depth: 0,
            ts_ms: 1,
        });
        assert!(
            ev.to_display().is_empty(),
            "channels must render Step as empty"
        );
    }

    // ── Custom-skill usage counting (L5 §14) ────────────────

    #[test]
    fn extract_skill_names_only_from_skill_tool_use() {
        // A `Skill` tool_use with the documented `skill` arg is picked up.
        let ev = tool_use_event(
            "t1",
            "Skill",
            json!({ "skill": "daily-report", "args": "x" }),
        );
        assert_eq!(
            extract_skill_tool_names(&ev),
            vec!["daily-report".to_string()]
        );

        // Non-Skill tools are ignored (Read here carries a `skill`-looking key).
        let read = tool_use_event("t2", "Read", json!({ "skill": "not-a-skill" }));
        assert!(extract_skill_tool_names(&read).is_empty());

        // Fallback arg keys (command / name) still resolve for Skill.
        let by_cmd = tool_use_event("t3", "Skill", json!({ "command": "翻譯校對" }));
        assert_eq!(
            extract_skill_tool_names(&by_cmd),
            vec!["翻譯校對".to_string()]
        );

        // tool_result / non-assistant events yield nothing.
        assert!(extract_skill_tool_names(&tool_result_event("t1")).is_empty());
    }

    #[test]
    fn matched_slug_is_token_equal_never_substring() {
        let approved: HashSet<String> = ["report", "daily-report", "翻譯校對"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Exact match hits.
        assert_eq!(matched_custom_slug("report", &approved), Some("report"));
        assert_eq!(matched_custom_slug("翻譯校對", &approved), Some("翻譯校對"));

        // Substring / superstring must NOT match (the anti-inflation invariant).
        assert_eq!(matched_custom_slug("report-daily", &approved), None);
        assert_eq!(matched_custom_slug("rep", &approved), None);
        assert_eq!(matched_custom_slug("daily-report-v2", &approved), None);
        // A CJK slug that is a substring of the invoked name must not match.
        assert_eq!(matched_custom_slug("翻譯校對稿", &approved), None);
        // Unknown / empty → None.
        assert_eq!(matched_custom_slug("", &approved), None);
        assert_eq!(matched_custom_slug("nope", &approved), None);
    }

    #[test]
    fn parallel_skill_tool_uses_all_extracted() {
        let ev = json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "tool_use", "id": "a", "name": "Skill", "input": { "skill": "s1" } },
                { "type": "tool_use", "id": "b", "name": "Bash", "input": { "command": "ls" } },
                { "type": "tool_use", "id": "c", "name": "Skill", "input": { "skill": "s2" } }
            ] }
        });
        assert_eq!(
            extract_skill_tool_names(&ev),
            vec!["s1".to_string(), "s2".to_string()]
        );
    }
}

