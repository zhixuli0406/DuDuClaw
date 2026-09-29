use super::*;

    // ── WP4 GroundEval: `<tool_activity>` judge evidence ────────

    #[test]
    fn filter_tool_activity_scopes_to_agent_and_window() {
        let jsonl = concat!(
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true}\n",
            "{\"timestamp\":\"2026-07-11T10:03:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":false}\n",
            // other agent — excluded
            "{\"timestamp\":\"2026-07-11T10:02:30Z\",\"agent_id\":\"other\",\"tool_name\":\"Bash\",\"success\":true}\n",
            // before the window — excluded
            "{\"timestamp\":\"2026-07-11T09:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Bash\",\"success\":true}\n",
            // after the window — excluded
            "{\"timestamp\":\"2026-07-11T12:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Bash\",\"success\":true}\n",
            // malformed — skipped, no panic
            "not json\n",
            "{\"agent_id\":\"w\"}\n", // missing timestamp/tool_name
        );
        let records =
            filter_tool_activity(jsonl, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].tool_name, "memory_search");
        assert!(records[0].success);
        assert!(!records[1].success);
    }

    #[test]
    fn filter_tool_activity_window_boundaries_are_inclusive() {
        let jsonl = concat!(
            "{\"timestamp\":\"2026-07-11T10:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Read\",\"success\":true}\n",
            "{\"timestamp\":\"2026-07-11T10:05:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Read\",\"success\":true}\n",
        );
        let records =
            filter_tool_activity(jsonl, "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z");
        assert_eq!(records.len(), 2, "both boundary timestamps are in-window");
    }

    #[test]
    fn filter_tool_activity_bad_bounds_yields_empty_not_panic() {
        let jsonl = "{\"timestamp\":\"2026-07-11T10:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Read\",\"success\":true}\n";
        assert!(filter_tool_activity(jsonl, "w", "not-a-date", "also-not-a-date").is_empty());
    }

    #[test]
    fn format_tool_activity_none_when_empty() {
        assert!(format_tool_activity(&[], &[]).is_none());
    }

    /// BUG-2 fix (WP-A10 §6 復驗): native-only evidence (no MCP records at
    /// all) still produces a block — this is the exact case that used to
    /// leave the judge staring at "zero tool call evidence" despite the
    /// agent having actually run Read/Write/Bash.
    #[test]
    fn format_tool_activity_native_only_produces_block() {
        let evidence = vec![native("Read", true), native("Write", true)];
        let block = format_tool_activity(&[], &evidence).unwrap();
        assert!(block.contains("Read (native): 1 ok, 0 err"));
        assert!(block.contains("Write (native): 1 ok, 0 err"));
    }

    /// A same-named MCP tool and native tool must not silently merge counts
    /// — the `(native)` suffix keeps them as distinct lines.
    #[test]
    fn format_tool_activity_merges_mcp_and_native_without_collapsing_names() {
        let records = vec![ToolActivityRecord {
            tool_name: "Bash".into(),
            success: true,
            result_text: None,
            input_text: None,
        }];
        let evidence = vec![native("Bash", false)];
        let block = format_tool_activity(&records, &evidence).unwrap();
        assert!(block.contains("Bash: 1 ok, 0 err"));
        assert!(block.contains("Bash (native): 0 ok, 1 err"));
    }

    #[test]
    fn format_tool_activity_aggregates_ok_err_per_tool() {
        let records = vec![
            ToolActivityRecord {
                tool_name: "memory_search".into(),
                success: true,
                result_text: None,
                input_text: None,
            },
            ToolActivityRecord {
                tool_name: "memory_search".into(),
                success: false,
                result_text: None,
                input_text: None,
            },
            ToolActivityRecord {
                tool_name: "Bash".into(),
                success: true,
                result_text: None,
                input_text: None,
            },
        ];
        let block = format_tool_activity(&records, &[]).unwrap();
        assert!(block.starts_with("<tool_activity>\n"));
        assert!(block.ends_with("\n</tool_activity>"));
        assert!(block.contains("memory_search: 1 ok, 1 err"));
        assert!(block.contains("Bash: 1 ok, 0 err"));
    }

    #[test]
    fn format_tool_activity_caps_at_line_limit() {
        let records: Vec<ToolActivityRecord> = (0..25)
            .map(|i| ToolActivityRecord {
                tool_name: format!("tool_{i:02}"),
                success: true,
                result_text: None,
                input_text: None,
            })
            .collect();
        let block = format_tool_activity(&records, &[]).unwrap();
        let line_count = block.lines().count();
        // 20 tool lines + the "N more omitted" line + 2 XML fence lines.
        assert_eq!(line_count, 20 + 1 + 2);
        assert!(block.contains("5 more tool(s) omitted"));
    }


    #[test]
    fn one_agent_set_is_byte_identical_to_the_single_agent_read() {
        let dir = team_audit_fixture();
        let single = read_tool_activity_records(dir.path(), "agnes", T_START, T_END);
        let as_set = read_tool_activity_records_for_agents(dir.path(), &["agnes"], T_START, T_END);
        assert_eq!(single, as_set);
        assert_eq!(
            format_tool_activity(&single, &[]),
            format_tool_activity(&as_set, &[])
        );
    }

    #[test]
    fn the_agent_set_unions_member_evidence_and_excludes_strangers() {
        let dir = team_audit_fixture();
        // Employee alone: the pre-E3 view — one bookkeeping call, nothing
        // that could ground "I wrote the file".
        let employee_only =
            read_tool_activity_records_for_agents(dir.path(), &["agnes"], T_START, T_END);
        assert_eq!(employee_only.len(), 1);

        let team = read_tool_activity_records_for_agents(
            dir.path(),
            &["agnes", "eph-r1-plan", "eph-r1-exec"],
            T_START,
            T_END,
        );
        assert_eq!(
            team.len(),
            4,
            "employee + 2 executor calls + 1 planner call"
        );
        assert!(
            !team.iter().any(|r| r.tool_name == "Bash"),
            "stranger leaked in"
        );

        let block = format_tool_activity(&team, &[]).unwrap();
        assert!(block.contains("Write: 1 ok, 1 err"), "{block}");
        assert!(block.contains("team_handoff: 1 ok, 0 err"), "{block}");
    }

    #[test]
    fn duplicate_and_blank_agent_ids_never_double_count() {
        let dir = team_audit_fixture();
        let once =
            read_tool_activity_records_for_agents(dir.path(), &["eph-r1-exec"], T_START, T_END);
        let twice = read_tool_activity_records_for_agents(
            dir.path(),
            &["eph-r1-exec", " eph-r1-exec ", "", "   "],
            T_START,
            T_END,
        );
        assert_eq!(once.len(), twice.len());
    }

    #[test]
    fn has_tool_activity_separates_mcp_only_from_none() {
        let dir = team_audit_fixture();
        // A member with rows in the window ⇒ McpOnly is an honest grade.
        assert!(has_tool_activity(dir.path(), "eph-r1-exec", T_START, T_END));
        // A member with none ⇒ None; claiming McpOnly would invent evidence.
        assert!(!has_tool_activity(
            dir.path(),
            "eph-never-ran",
            T_START,
            T_END
        ));
        // Out-of-window rows do not count.
        assert!(!has_tool_activity(
            dir.path(),
            "eph-r1-exec",
            "2026-09-25T00:00:00Z",
            "2026-09-25T01:00:00Z"
        ));
        // Missing audit file answers false rather than failing.
        let empty = tempfile::tempdir().unwrap();
        assert!(!has_tool_activity(empty.path(), "agnes", T_START, T_END));
    }

    #[test]
    fn tool_activity_block_for_agents_needs_a_window_start() {
        // `until` is wall-clock `now`, so this fixture stamps its rows at now
        // rather than at a fixed date the run could precede.
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            format!(
                "{{\"timestamp\":\"{now}\",\"agent_id\":\"eph-r1-exec\",\"tool_name\":\"Write\",\"success\":true}}\n"
            ),
        )
        .unwrap();
        let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        // Review finding 1 regression: no window start must NOT collapse onto
        // the same `None` that means "the window held no tool calls". The
        // block is present and says the window could not be established.
        let no_window = tool_activity_block_for_agents(dir.path(), &["eph-r1-exec"], None)
            .expect("an unknown window must render an explicit marker, not vanish");
        assert!(no_window.contains(TOOL_ACTIVITY_NO_WINDOW), "{no_window}");
        assert!(
            !no_window.contains("Write: 1 ok"),
            "an unknown window must not smuggle evidence in: {no_window}"
        );
        // The employee alone sees nothing — this is exactly the E3 defect.
        assert!(tool_activity_block_for_agents(dir.path(), &["agnes"], Some(&since)).is_none());
        // Employee ∪ member sees the member's work.
        let block =
            tool_activity_block_for_agents(dir.path(), &["agnes", "eph-r1-exec"], Some(&since))
                .expect("member evidence must reach the block");
        assert!(block.contains("Write: 1 ok, 0 err"), "{block}");
    }

    // ── Team-as-Agent live round 8: native evidence + artifact receipts ──

    /// The round-8 defect end to end: a codex member's work lives in
    /// `tool_calls.jsonl` rows the composer persisted under the MEMBER id, and
    /// the union read must surface them — otherwise the settle says "no tool
    /// activity exists to evidence that any of the files were created" about
    /// work that demonstrably happened.
    #[test]
    fn persisted_native_member_rows_join_the_evidence_union() {
        let dir = tempfile::tempdir().unwrap();
        // Written by the REAL producer, so this test fails if the row shape
        // and the reader ever drift apart.
        crate::team_composer::persist_member_native_events(
            dir.path(),
            "eph-r1-exec",
            "codex",
            Some("gpt-5.6-sol"),
            &[crate::runtime::NativeToolEvent {
                tool_name: "shell".to_string(),
                success: true,
                result_text: Some("created notes/a.md".to_string()),
                input_text: Some("mkdir notes".to_string()),
            }],
        );
        let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        // Employee alone: the round-8 view — nothing.
        assert!(tool_activity_block_for_agents(dir.path(), &["agnes"], Some(&since)).is_none());

        // Employee ∪ member: the native work is now first-class evidence.
        let block =
            tool_activity_block_for_agents(dir.path(), &["agnes", "eph-r1-exec"], Some(&since))
                .expect("persisted native rows must reach the digest");
        assert!(block.contains("shell: 1 ok, 0 err"), "{block}");

        // And it can actually GROUND a claim — the whole point. A persisted
        // native row carries `result_text`, so `check_grounded` has something
        // to compare against instead of perpetually `ResultTextMissing`.
        let now = chrono::Utc::now().to_rfc3339();
        let records = read_tool_activity_records_for_agents(
            dir.path(),
            &["agnes", "eph-r1-exec"],
            &since,
            &now,
        );
        assert_eq!(
            grounding_precheck(
                "I created notes/a.md in the workspace.",
                &records,
                &[],
                GroundingPrecheckConfig {
                    enabled: true,
                    min_overlap_chars: 10,
                },
            ),
            GroundingPrecheck::Grounded {
                tool_name: "shell".to_string()
            }
        );
    }

    /// A self-echo tool must never self-ground. `check_grounded` applies the
    /// deny-list at WRITE time (Fix-2 C1a), not at read time — so the persist
    /// path has to do the suppressing, and this proves it does. A codex member
    /// reports its MCP calls as native events, so `team_handoff` really does
    /// arrive here.
    #[test]
    fn a_persisted_native_self_echo_tool_still_cannot_self_ground() {
        let dir = tempfile::tempdir().unwrap();
        crate::team_composer::persist_member_native_events(
            dir.path(),
            "eph-r1-exec",
            "codex",
            None,
            &[crate::runtime::NativeToolEvent {
                tool_name: "mcp__duduclaw__team_handoff".to_string(),
                success: true,
                result_text: Some("packet accepted: notes/a.md created".to_string()),
                input_text: None,
            }],
        );
        let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let now = chrono::Utc::now().to_rfc3339();
        let records =
            read_tool_activity_records_for_agents(dir.path(), &["eph-r1-exec"], &since, &now);
        assert_eq!(records.len(), 1);
        assert!(
            records[0].result_text.is_none(),
            "a self-echo tool's output must not be persisted as grounding evidence"
        );
        assert!(matches!(
            grounding_precheck(
                "notes/a.md created",
                &records,
                &[],
                GroundingPrecheckConfig {
                    enabled: true,
                    min_overlap_chars: 10,
                },
            ),
            GroundingPrecheck::Degraded { .. }
        ));
    }

    #[test]
    fn artifact_receipt_rows_render_into_their_own_block() {
        let records = vec![
            ok_record("Write", "ok"),
            ok_record(
                crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME,
                "notes/a.md 5B sha256=2cf2 exists",
            ),
            ToolActivityRecord {
                tool_name: crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME.to_string(),
                success: false,
                result_text: Some("notes/b.md missing".to_string()),
                input_text: None,
            },
            // A re-verified artifact must not print twice.
            ok_record(
                crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME,
                "notes/a.md 5B sha256=2cf2 exists",
            ),
            // A receipt row with no text carries no observation.
            ToolActivityRecord {
                tool_name: crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME.to_string(),
                success: true,
                result_text: None,
                input_text: None,
            },
        ];
        let block = format_artifact_receipts_from_records(&records).expect("receipts render");
        assert_eq!(
            block,
            "<artifact_receipts>\nnotes/a.md 5B sha256=2cf2 exists\nnotes/b.md missing\n</artifact_receipts>"
        );
        // A window with no receipt rows adds no block at all — a round that
        // declared no artifacts must look exactly as it did before.
        assert!(format_artifact_receipts_from_records(&[ok_record("Write", "ok")]).is_none());
        assert!(format_artifact_receipts_from_records(&[]).is_none());
    }

    #[test]
    fn artifact_receipts_block_for_agents_needs_a_window_and_unions_members() {
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            format!(
                "{{\"timestamp\":\"{now}\",\"agent_id\":\"eph-r1-exec\",\"tool_name\":\"{}\",\"success\":true,\"result_text\":\"notes/a.md 5B sha256=2cf2 exists\"}}\n",
                crate::team_composer::ARTIFACT_RECEIPT_TOOL_NAME
            ),
        )
        .unwrap();
        let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        assert!(artifact_receipts_block_for_agents(dir.path(), &["eph-r1-exec"], None).is_none());
        assert!(artifact_receipts_block_for_agents(dir.path(), &["agnes"], Some(&since)).is_none());
        let block =
            artifact_receipts_block_for_agents(dir.path(), &["agnes", "eph-r1-exec"], Some(&since))
                .expect("member receipts must reach the block");
        assert!(
            block.contains("notes/a.md 5B sha256=2cf2 exists"),
            "{block}"
        );
    }

    // ── Live round 8: structured judge output ────────────────────────────

    /// The schema is derived FROM the parser's contract, so the pair cannot
    /// drift: a reply that satisfies the schema must parse.
    #[test]
    fn the_evaluator_schema_matches_what_parse_pre_evaluation_requires() {
        let schema = pre_evaluator_output_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // Strict-schema rule (codex/OpenAI): every declared property must be
        // required when `additionalProperties` is false, so `blocker_key`
        // is required too (empty string when there is no blocker).
        assert_eq!(
            required,
            vec!["decision", "evidence", "next_step", "blocker_key"]
        );
        let decisions: Vec<&str> = schema["properties"]["decision"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(decisions, vec!["continue", "candidate_complete", "blocked"]);
        // Strict-schema rule: `blocker_key` is required but nullable, so a
        // non-blocked decision sends `null`/`""` (the parser reads both as
        // "absent") and a blocked one sends the snake_case key.
        assert!(schema["properties"]["blocker_key"].is_object());
        assert!(required.contains(&"blocker_key"));
        assert_eq!(
            schema["properties"]["blocker_key"]["type"],
            serde_json::json!(["string", "null"])
        );

        // Schema-conforming replies parse: null and empty blocker_key on a
        // non-blocked decision, a real key on a blocked one.
        for reply in [
            r#"{"decision":"candidate_complete","evidence":"notes/a.md exists","next_step":"check the index","blocker_key":null}"#,
            r#"{"decision":"candidate_complete","evidence":"notes/a.md exists","next_step":"check the index","blocker_key":""}"#,
        ] {
            let parsed = parse_pre_evaluation(reply).expect("schema-shaped reply must parse");
            assert_eq!(parsed.decision, PreDecision::CandidateComplete);
            assert!(parsed.blocker_key.is_none());
        }
        let blocked = parse_pre_evaluation(
            r#"{"decision":"blocked","evidence":"no key","next_step":"ask operator","blocker_key":"missing_api_credential"}"#,
        )
        .expect("blocked reply must parse");
        assert_eq!(
            blocked.blocker_key.as_deref(),
            Some("missing_api_credential")
        );
    }

    #[test]
    fn the_panel_schema_follows_the_active_aspect_set() {
        for difficulty in [Difficulty::Simple, Difficulty::Complex] {
            let aspects = panel_aspects(difficulty);
            let schema = panel_output_schema(aspects);
            let required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(required, aspects.to_vec());
            for aspect in aspects {
                let prop = &schema["properties"][*aspect];
                assert_eq!(prop["properties"]["pass"]["type"], "boolean");
                assert_eq!(prop["required"][0], "pass");
            }
            // A schema-shaped panel reply synthesizes to an accept.
            let body: Vec<String> = aspects
                .iter()
                .map(|a| format!("\"{a}\":{{\"pass\":true,\"reason\":\"ok\"}}"))
                .collect();
            let verdict = parse_panel_verdict_for(&format!("{{{}}}", body.join(",")), aspects);
            assert!(verdict.passed, "{difficulty:?}: {verdict:?}");
        }
    }

    /// Outside a stage scope there is no schema at all — every pre-round-8
    /// caller's argv stays byte-identical.
    #[tokio::test]
    async fn no_stage_scope_means_no_schema() {
        assert!(judge_output_schema().is_none());
        let seen = with_judge_output_schema(pre_evaluator_output_schema(), async {
            judge_output_schema()
        })
        .await;
        assert_eq!(seen, Some(pre_evaluator_output_schema()));
        // The scope closes with the future.
        assert!(judge_output_schema().is_none());
    }

    #[test]
    fn read_tool_activity_records_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let records = read_tool_activity_records(
            dir.path(),
            "w",
            "2026-07-11T10:00:00Z",
            "2026-07-11T10:05:00Z",
        );
        assert!(records.is_empty());
        assert!(format_tool_activity(&records, &[]).is_none());
    }

    #[test]
    fn read_tool_activity_records_reads_and_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Read\",\"success\":true}\n",
        )
        .unwrap();
        let records = read_tool_activity_records(
            dir.path(),
            "w",
            "2026-07-11T10:00:00Z",
            "2026-07-11T10:05:00Z",
        );
        assert_eq!(records.len(), 1);
        let block = format_tool_activity(&records, &[]).unwrap();
        assert!(block.contains("Read: 1 ok, 0 err"));
    }

    /// Same fixture as `read_tool_activity_records_reads_and_filters`, but
    /// exercising the `result_text` capture the B3 grounding pre-check
    /// depends on — no production writer sets this key today (see the
    /// `ToolActivityRecord::result_text` doc comment), but the reader is
    /// forward-compatible with a future one.
    #[test]
    fn read_tool_activity_records_captures_optional_result_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true,\"result_text\":\"policy: 30 days\"}\n",
        )
        .unwrap();
        let records = read_tool_activity_records(
            dir.path(),
            "w",
            "2026-07-11T10:00:00Z",
            "2026-07-11T10:05:00Z",
        );
        assert_eq!(records[0].result_text.as_deref(), Some("policy: 30 days"));
    }

