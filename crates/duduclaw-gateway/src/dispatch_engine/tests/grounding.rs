use super::*;

    #[test]
    fn grounding_precheck_passes_when_result_overlaps_tool_evidence() {
        let records = vec![ok_record(
            "mcp__duduclaw__tasks_create",
            "task created: refund #4821 approved for customer",
        )];
        let outcome = grounding_precheck(
            "Result: refund #4821 approved for customer, ticket closed.",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Grounded {
                tool_name: "mcp__duduclaw__tasks_create".to_string()
            }
        );
    }

    /// CJK case: char-counted overlap (not byte-counted), traditional
    /// Chinese business text — mirrors the eval suite's CJK grounding case.
    #[test]
    fn grounding_precheck_passes_with_cjk_overlap() {
        let records = vec![ok_record(
            "mcp__duduclaw__memory_search",
            "查詢結果：退款政策為三十天內可全額退款，需出示收據。",
        )];
        let outcome = grounding_precheck(
            "已為您確認：退款政策為三十天內可全額退款。",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 8,
            },
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Grounded {
                tool_name: "mcp__duduclaw__memory_search".to_string()
            }
        );
    }

    #[test]
    fn grounding_precheck_rejects_unsupported_claim() {
        let records = vec![ok_record(
            "mcp__duduclaw__memory_search",
            "policy: refunds within 30 days of purchase",
        )];
        let outcome = grounding_precheck(
            "I have processed a full refund and shipped a replacement unit today.",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 12,
            },
        );
        match outcome {
            GroundingPrecheck::Reject { feedback } => {
                assert!(feedback.contains("grounding"), "{feedback}");
                assert!(feedback.contains("引用"), "{feedback}"); // steers the retry toward quoting evidence
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    /// CJK reject case: a claim whose specific figures do not appear in any
    /// tool result must still be caught with CJK char counting.
    #[test]
    fn grounding_precheck_rejects_unsupported_cjk_claim() {
        let records = vec![ok_record(
            "mcp__duduclaw__memory_search",
            "查詢結果：本月營收為新台幣一百二十萬元整。",
        )];
        let outcome = grounding_precheck(
            "已完成分析，本季獲利創下歷史新高，達五百萬元。",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 8,
            },
        );
        assert!(matches!(outcome, GroundingPrecheck::Reject { .. }));
    }

    /// Fix-2 C1b: even when a call's `result_text` happens to overlap the
    /// final claim, if that overlap is only the caller's OWN input echoed
    /// back, it must not ground the claim — degrades exactly like
    /// "no usable evidence", never a false Grounded.
    #[test]
    fn grounding_precheck_does_not_ground_on_input_echoed_result_text() {
        let records = vec![ok_record_with_input(
            "mcp__duduclaw__tasks_complete",
            "Completed: refund #4821 approved for customer",
            "refund #4821 approved for customer",
        )];
        // Final claim kept identical to the excluded input text so every
        // candidate window is provably inside the excluded span (a claim
        // wrapped in different surrounding prose can incidentally create a
        // stray boundary-straddling window that isn't itself an echo — see
        // the sibling test in `duduclaw-core/src/grounding.rs` for the same
        // note).
        let outcome = grounding_precheck(
            "refund #4821 approved for customer",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert!(
            matches!(outcome, GroundingPrecheck::Reject { .. }),
            "self-echoed overlap must not ground the claim: {outcome:?}"
        );
    }

    /// Companion case: the SAME record also carries genuine new information
    /// (a store-assigned id not present in the input) — grounding on that
    /// span must still work.
    #[test]
    fn grounding_precheck_still_grounds_on_genuine_non_echoed_span() {
        let records = vec![ok_record_with_input(
            "mcp__duduclaw__tasks_create",
            "task created with id task-zx88-store-assigned",
            "create a follow-up task",
        )];
        let outcome = grounding_precheck(
            "Created it: task-zx88-store-assigned",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Grounded {
                tool_name: "mcp__duduclaw__tasks_create".to_string()
            }
        );
    }

    #[test]
    fn grounding_precheck_skips_pure_text_task_with_no_tool_use() {
        let outcome = grounding_precheck(
            "這是一個純文字回覆，沒有呼叫任何工具。",
            &[], // no tool_use at all in the window
            &[], // and no native evidence either
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Skip {
                reason: "no tool_use in claim→review window"
            }
        );
    }

    /// BUG-2 fix (WP-A10 §6 復驗): when there is NO MCP evidence but the
    /// WP-A4 native collector DID see a successful non-self-echo tool call
    /// that carries no `result_text` (the pre-R1 shape), the result must
    /// upgrade from `Skip` (which would falsely imply "no tool_use at all")
    /// to `Degraded` with an accurate reason — and must NOT become
    /// `Grounded`, since there is no text to overlap-check against. See
    /// `grounding_precheck_native_evidence_with_result_text_reaches_grounded`
    /// below for the R1 case where native evidence DOES carry text.
    #[test]
    fn grounding_precheck_degrades_not_skips_when_only_native_evidence_exists() {
        let native_evidence = vec![native("Write", true)];
        let outcome = grounding_precheck(
            "我已經寫入 report.md 檔案。",
            &[], // no MCP tool_use in the window
            &native_evidence,
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Degraded {
                reason: "native tool evidence present but lacks captured result_text for grounding"
            }
        );
    }

    /// A failed (or self-echo) native event must NOT upgrade the reason —
    /// it carries no real "the agent used a tool" signal.
    #[test]
    fn grounding_precheck_still_skips_when_native_evidence_all_failed() {
        let native_evidence = vec![native("Bash", false)];
        let outcome = grounding_precheck(
            "純文字回覆。",
            &[],
            &native_evidence,
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Skip {
                reason: "no tool_use in claim→review window"
            }
        );
    }

    /// Disabled config short-circuits before native evidence is even
    /// consulted — must stay a plain `Skip { reason: "disabled" }`.
    #[test]
    fn grounding_precheck_disabled_ignores_native_evidence() {
        let native_evidence = vec![native("Write", true)];
        let outcome = grounding_precheck(
            "anything",
            &[],
            &native_evidence,
            GroundingPrecheckConfig {
                enabled: false,
                min_overlap_chars: 6,
            },
        );
        assert_eq!(outcome, GroundingPrecheck::Skip { reason: "disabled" });
    }

    // ── R1: native evidence with captured text ──────────────────────────

    /// R1's actual deliverable: a task done entirely with native tools
    /// (Read/Write/Bash — no MCP call at all) whose native evidence DOES
    /// carry masked `result_text` overlapping the claim must reach
    /// `Grounded`, not perpetually `Degraded`.
    #[test]
    fn grounding_precheck_native_evidence_with_result_text_reaches_grounded() {
        let native_evidence = vec![native_with_text(
            "Write",
            true,
            "wrote report.md with quarterly revenue: 1.2M",
            None,
        )];
        let outcome = grounding_precheck(
            "Done — report.md now contains quarterly revenue: 1.2M.",
            &[], // no MCP evidence at all — purely native
            &native_evidence,
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Grounded {
                tool_name: "Write".to_string()
            }
        );
    }

    /// The R1 mirror image: native evidence WITH text, but the claim shares
    /// no overlap with it — must reject, exactly like an MCP-evidence
    /// mismatch would.
    #[test]
    fn grounding_precheck_native_evidence_with_result_text_rejects_unsupported_claim() {
        let native_evidence = vec![native_with_text(
            "Bash",
            true,
            "total: 42 files processed, 0 errors",
            None,
        )];
        let outcome = grounding_precheck(
            "I have refunded the customer and closed the ticket.",
            &[],
            &native_evidence,
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert!(
            matches!(outcome, GroundingPrecheck::Reject { .. }),
            "{outcome:?}"
        );
    }

    /// R1 + Fix-2 C1b: native evidence's own `input_text` still subtracts
    /// self-echoed spans — a native tool has no `SELF_ECHO_TOOL_NAMES` deny
    /// -list membership, but the generic echo-exclusion logic in
    /// `check_grounded` applies uniformly regardless of tool identity.
    #[test]
    fn grounding_precheck_native_evidence_does_not_ground_on_echoed_input() {
        let native_evidence = vec![native_with_text(
            "Bash",
            true,
            "ran: refund for order #1234",
            Some("refund for order #1234"),
        )];
        let outcome = grounding_precheck(
            "refund for order #1234",
            &[],
            &native_evidence,
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert!(
            matches!(outcome, GroundingPrecheck::Reject { .. }),
            "self-echoed native input must not ground the claim: {outcome:?}"
        );
    }

    /// R1: native AND MCP evidence both present, only the native side
    /// actually grounds the claim — the merge must not drop it.
    #[test]
    fn grounding_precheck_grounds_on_native_evidence_when_mcp_evidence_is_unrelated() {
        let records = vec![ToolActivityRecord {
            tool_name: "mcp__duduclaw__memory_search".into(),
            success: true,
            result_text: Some("unrelated policy lookup, no matching content".into()),
            input_text: None,
        }];
        let native_evidence = vec![native_with_text(
            "Write",
            true,
            "wrote quarterly-report.md successfully",
            None,
        )];
        let outcome = grounding_precheck(
            "I wrote quarterly-report.md successfully.",
            &records,
            &native_evidence,
            GroundingPrecheckConfig {
                enabled: true,
                min_overlap_chars: 10,
            },
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Grounded {
                tool_name: "Write".to_string()
            }
        );
    }

    #[test]
    fn grounding_precheck_skips_when_disabled() {
        let records = vec![ok_record("Bash", "irrelevant")];
        let outcome = grounding_precheck(
            "anything",
            &records,
            &[],
            GroundingPrecheckConfig {
                enabled: false,
                min_overlap_chars: 6,
            },
        );
        assert_eq!(outcome, GroundingPrecheck::Skip { reason: "disabled" });
    }

    /// The production degrade case (see the B3 module doc): tool_use
    /// evidence exists but the audit trail never captured `result_text` —
    /// today's universal case for every writer. Must degrade (fall through
    /// to the judge), never reject a task over an observability gap.
    #[test]
    fn grounding_precheck_degrades_when_result_text_missing() {
        let records = vec![ToolActivityRecord {
            tool_name: "mcp__duduclaw__tasks_create".into(),
            success: true,
            // No result_text: either an ordinary writer gap, or (Fix-2 C1a)
            // this tool is on the self-echo deny-list and never gets one.
            result_text: None,
            input_text: None,
        }];
        let outcome = grounding_precheck(
            "Task created and refund issued.",
            &records,
            &[],
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Degraded {
                reason: "tool evidence lacks captured result_text"
            }
        );
    }

    /// Same MCP evidence shape, but native evidence ALSO exists this round
    /// — the reason string must mention it (still `Degraded`, never
    /// `Grounded`, since in THIS fixture neither the MCP record nor the
    /// native event carries `result_text` — see
    /// `grounding_precheck_native_evidence_with_result_text_reaches_grounded`
    /// for the R1 case where native evidence DOES carry text).
    #[test]
    fn grounding_precheck_degrades_with_native_hint_when_result_text_missing() {
        let records = vec![ToolActivityRecord {
            tool_name: "mcp__duduclaw__tasks_create".into(),
            success: true,
            result_text: None,
            input_text: None,
        }];
        let native_evidence = vec![native("Write", true)];
        let outcome = grounding_precheck(
            "Task created and refund issued.",
            &records,
            &native_evidence,
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Degraded {
                reason: "tool evidence lacks captured result_text (native tool evidence also present, same limitation)"
            }
        );
    }

    #[test]
    fn grounding_precheck_degrades_when_every_call_errored() {
        let records = vec![ToolActivityRecord {
            tool_name: "mcp__duduclaw__tasks_create".into(),
            success: false,
            result_text: Some("permission denied".into()),
            input_text: None,
        }];
        let outcome = grounding_precheck(
            "Task created successfully.",
            &records,
            &[],
            GroundingPrecheckConfig::default(),
        );
        assert_eq!(
            outcome,
            GroundingPrecheck::Degraded {
                reason: "no successful tool call in window"
            }
        );
    }

    #[test]
    fn grounding_precheck_config_reads_dispatch_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\ngrounding_precheck_enabled = false\ngrounding_min_overlap_chars = 20\n",
        )
        .unwrap();
        let cfg = GroundingPrecheckConfig::from_home(dir.path());
        assert!(!cfg.enabled);
        assert_eq!(cfg.min_overlap_chars, 20);
    }

    #[test]
    fn grounding_precheck_config_defaults_on_missing_or_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        // No config.toml at all.
        let cfg = GroundingPrecheckConfig::from_home(dir.path());
        assert_eq!(cfg, GroundingPrecheckConfig::default());

        // Malformed section: a non-positive threshold must not disable the
        // overlap requirement (would make every claim trivially "grounded").
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\ngrounding_min_overlap_chars = 0\n",
        )
        .unwrap();
        let cfg = GroundingPrecheckConfig::from_home(dir.path());
        assert_eq!(cfg.min_overlap_chars, DEFAULT_GROUNDING_MIN_OVERLAP_CHARS);
    }

    /// End-to-end wiring: `review_goal_tasks` rejects a goal task whose
    /// result is provably ungrounded in its own tool-call window *before*
    /// ever invoking the judge — the judge stub records whether it was
    /// called at all.
    #[tokio::test]
    async fn review_goal_tasks_rejects_via_grounding_precheck_before_judge() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true,\"result_text\":\"Refund policy: 30 days from purchase, receipt required.\"}\n",
        )
        .unwrap();

        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        let g = pending_goal("g1");
        store.insert_task(&g).await.unwrap();
        store
            .atomic_claim("g1", "w", "2026-07-11T10:00:00Z", "2026-07-11T10:05:00Z")
            .await
            .unwrap()
            .is_claimed();
        // Deliberately shares no >= 6-char run with the tool evidence above
        // (verified: no accidental collision like "refund" would be — that
        // word alone is exactly the default `min_overlap_chars` and bit a
        // first draft of this test).
        store
            .complete_task("g1", "I handled the request successfully.", "w")
            .await
            .unwrap();
        assert_eq!(
            store.get_task("g1").await.unwrap().unwrap().status,
            "review"
        );

        let judge = Arc::new(CapturingJudge {
            outcome: Ok(AcceptanceVerdict {
                passed: true,
                feedback: "should never be reached".into(),
                aspects: None,
            }),
            captured_task: std::sync::Mutex::new(None),
        });
        let engine = DispatchEngine::new(store.clone(), Some(judge.clone()))
            .with_home_dir(dir.path().to_path_buf());

        engine.review_goal_tasks().await.unwrap();

        assert!(
            judge.captured_task.lock().unwrap().is_none(),
            "grounding pre-check must reject before the judge is ever called"
        );
        let row = store.get_task("g1").await.unwrap().unwrap();
        assert_eq!(row.status, "revising");
    }

