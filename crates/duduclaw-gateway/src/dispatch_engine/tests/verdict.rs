use super::*;

    /// Regression (2026-09-28 review, `review_team.md` §3 "授權／證據"):
    /// `strip_workspace_prefixes` used an **unanchored** `String::replace`, so
    /// a worker line mentioning an unrelated backup path that merely contains
    /// `agents/<id>/` was rewritten into a file that does not exist — and that
    /// rewritten text is what the acceptance judge is shown as evidence.
    /// Stripping now happens only at a path-token boundary.
    #[test]
    fn strip_workspace_prefixes_only_strips_at_a_path_token_boundary() {
        let prefixes =
            workspace_prefixes_for(std::path::Path::new("/home/u/.duduclaw/agents/agnes"));

        // ① What the stripping exists for (live rounds 9–11): an absolute
        //    member path that starts a path token.
        assert_eq!(
            strip_workspace_prefixes(
                "wrote /home/u/.duduclaw/agents/agnes/notes/a.md",
                &prefixes
            ),
            "wrote notes/a.md"
        );
        // ② Home-relative spellings (live round 12) at the boundaries a path
        //    token really starts at: line start, quote, backtick, bracket.
        assert_eq!(
            strip_workspace_prefixes("agents/agnes/notes/a.md", &prefixes),
            "notes/a.md"
        );
        assert_eq!(
            strip_workspace_prefixes("see `/agents/agnes/notes/a.md`", &prefixes),
            "see `notes/a.md`"
        );
        assert_eq!(
            strip_workspace_prefixes("[a](agents/agnes/notes/a.md)", &prefixes),
            "[a](notes/a.md)"
        );
        assert_eq!(
            strip_workspace_prefixes("first\nagents/agnes/notes/a.md\n", &prefixes),
            "first\nnotes/a.md\n"
        );

        // ③ THE REGRESSION. An unrelated absolute path that merely *contains*
        //    `agents/agnes/` is a different file; the judge must read it
        //    byte-identically to what the worker wrote.
        assert_eq!(
            strip_workspace_prefixes("restored /mnt/backup/agents/agnes/old.md", &prefixes),
            "restored /mnt/backup/agents/agnes/old.md"
        );
        assert_eq!(
            strip_workspace_prefixes("xagents/agnes/old.md", &prefixes),
            "xagents/agnes/old.md"
        );
        // A prefix that is not at a boundary anywhere leaves the text alone.
        assert_eq!(
            strip_workspace_prefixes("zzz/home/u/.duduclaw/agents/agnes/a.md", &prefixes),
            "zzz/home/u/.duduclaw/agents/agnes/a.md"
        );

        // Empty prefixes / empty text are inert, and CJK text is never sliced
        // mid-character.
        assert_eq!(strip_workspace_prefixes("報告完成", &prefixes), "報告完成");
        assert_eq!(
            strip_workspace_prefixes("報告 agents/agnes/筆記.md 完成", &prefixes),
            "報告 筆記.md 完成"
        );
        assert_eq!(strip_workspace_prefixes("abc", &[]), "abc");
    }


    #[test]
    fn parse_verdict_reads_pass_fail() {
        let p = parse_verdict("PASS\nlooks good");
        assert!(p.passed);
        assert_eq!(p.feedback, "looks good");

        let f = parse_verdict("FAIL\nmissing tests");
        assert!(!f.passed);
        assert_eq!(f.feedback, "missing tests");

        // Case-insensitive, punctuation-tolerant.
        assert!(parse_verdict("pass.").passed);
        assert!(!parse_verdict("Fail: nope").passed);
    }

    #[test]
    fn parse_verdict_is_conservative_on_ambiguity() {
        // Neither token ⇒ not passed (never auto-accept garbage).
        assert!(!parse_verdict("I think it is okay maybe").passed);
        // Both tokens on the first line ⇒ FAIL wins.
        assert!(!parse_verdict("PASS or FAIL?").passed);
        // A PASS mention only on a later line does NOT flip a non-verdict first line.
        assert!(!parse_verdict("hmm\nPASS").passed);
    }

    #[test]
    fn panel_all_pass_accepts() {
        let raw = r#"{"correctness": {"pass": true, "reason": "meets all criteria"},
                      "completeness": {"pass": true, "reason": "artifact delivered"},
                      "safety": {"pass": true, "reason": "no dangerous ops"}}"#;
        let v = parse_panel_verdict(raw);
        assert!(v.passed);
        // Pass-notes are folded into feedback so accept records rationale.
        assert!(v.feedback.contains("meets all criteria"));
    }

    #[test]
    fn panel_any_fail_rejects_and_combines_reasons() {
        let raw = r#"{"correctness": {"pass": true, "reason": "ok"},
                      "completeness": {"pass": false, "reason": "only promised, not done"},
                      "safety": {"pass": false, "reason": "rm -rf detected"}}"#;
        let v = parse_panel_verdict(raw);
        assert!(!v.passed);
        // Combined feedback carries every failing aspect for the retry Generator.
        assert!(v.feedback.contains("only promised, not done"));
        assert!(v.feedback.contains("rm -rf detected"));
        assert!(v.feedback.contains("completeness"));
        assert!(v.feedback.contains("safety"));
        // A passing aspect is not reported as a failure.
        assert!(!v.feedback.contains("[correctness]"));
    }

    #[test]
    fn panel_tolerates_fences_and_prose() {
        let raw = "Here is my verdict:\n```json\n{\"correctness\": {\"pass\": false, \"reason\": \"wrong\"}, \
                   \"completeness\": {\"pass\": true, \"reason\": \"\"}, \
                   \"safety\": {\"pass\": true, \"reason\": \"\"}}\n```\nThanks.";
        let v = parse_panel_verdict(raw);
        assert!(!v.passed);
        assert!(v.feedback.contains("wrong"));
    }

    #[test]
    fn panel_missing_aspect_is_fail_closed() {
        // `safety` aspect absent ⇒ FAIL, never auto-accept.
        let raw = r#"{"correctness": {"pass": true, "reason": "ok"},
                      "completeness": {"pass": true, "reason": "ok"}}"#;
        let v = parse_panel_verdict(raw);
        assert!(!v.passed);
        assert!(v.feedback.contains("safety"));
    }

    #[test]
    fn panel_invalid_pass_field_is_fail_closed() {
        // Non-boolean / missing `pass` ⇒ that aspect fails.
        let raw = r#"{"correctness": {"reason": "no pass field"},
                      "completeness": {"pass": true, "reason": "ok"},
                      "safety": {"pass": true, "reason": "ok"}}"#;
        let v = parse_panel_verdict(raw);
        assert!(!v.passed);
        assert!(v.feedback.contains("correctness"));
    }

    #[test]
    fn panel_falls_back_to_legacy_verdict() {
        // No JSON object ⇒ legacy single PASS/FAIL parsing still works.
        assert!(parse_panel_verdict("PASS\nlooks good").passed);
        assert!(!parse_panel_verdict("FAIL\nmissing tests").passed);
        // Braces present but not a panel (no aspect keys) ⇒ H3 fail-closed
        // (it never reaches the legacy scanner, which could have tokenized a
        // `"pass"` key into an accept).
        assert!(!parse_panel_verdict("{\"foo\": 1}").passed);
    }

    // ── D4 MaAS dynamic judge depth ─────────────────────────

    #[test]
    fn difficulty_classifies_simple_and_complex() {
        // Short, single-step, tool-light ⇒ Simple.
        assert_eq!(
            classify_goal_difficulty("寄一封提醒信給 Bob"),
            Difficulty::Simple
        );
        assert_eq!(
            classify_goal_difficulty("rename the file to report.md"),
            Difficulty::Simple
        );
        // Keyword-flagged ⇒ Complex (zh + en).
        assert_eq!(
            classify_goal_difficulty("研究三家競品的定價"),
            Difficulty::Complex
        );
        assert_eq!(
            classify_goal_difficulty("比較 A 與 B 兩個方案"),
            Difficulty::Complex
        );
        assert_eq!(
            classify_goal_difficulty("migrate the database to postgres"),
            Difficulty::Complex
        );
        assert_eq!(
            classify_goal_difficulty("deploy the new service"),
            Difficulty::Complex
        );
        assert_eq!(
            classify_goal_difficulty("Research and compare vendors"),
            Difficulty::Complex
        );
        // Long goal (many CJK chars) ⇒ Complex regardless of keywords.
        let long = "把這批客戶資料一筆一筆整理乾淨並依照月份分類然後彙整成一份完整的月度營收報表最後寄給主管確認".repeat(2);
        assert_eq!(classify_goal_difficulty(&long), Difficulty::Complex);
    }

    #[test]
    fn panel_aspects_retains_safety_at_every_depth() {
        let simple = panel_aspects(Difficulty::Simple);
        let complex = panel_aspects(Difficulty::Complex);
        assert_eq!(simple, &["correctness", "safety"]);
        assert_eq!(complex, &["correctness", "completeness", "safety"]);
        // Safety survives the shallow depth (fail-closed invariant).
        assert!(simple.contains(&"safety"));
        assert!(!simple.contains(&"completeness"));
    }

    #[test]
    fn simple_prompt_has_two_aspects_and_omits_completeness() {
        let p = build_acceptance_prompt_for("crit", "task", "result", Difficulty::Simple);
        assert!(p.contains("\"correctness\""));
        assert!(p.contains("\"safety\""));
        assert!(
            !p.contains("completeness"),
            "Simple panel must not mention completeness"
        );
        assert!(p.contains("two independent aspects"));
    }

    #[test]
    fn simple_panel_synthesize_is_fail_closed() {
        let aspects = panel_aspects(Difficulty::Simple);
        // Both aspects pass ⇒ accept.
        let ok = r#"{"correctness": {"pass": true, "reason": "meets criteria"},
                     "safety": {"pass": true, "reason": "no dangerous ops"}}"#;
        assert!(parse_panel_verdict_for(ok, aspects).passed);
        // Missing safety ⇒ fail-closed even at shallow depth.
        let missing_safety = r#"{"correctness": {"pass": true, "reason": "ok"}}"#;
        let v = parse_panel_verdict_for(missing_safety, aspects);
        assert!(!v.passed);
        assert!(v.feedback.contains("safety"));
        // A failing safety aspect rejects.
        let unsafe_result = r#"{"correctness": {"pass": true, "reason": "ok"},
                                "safety": {"pass": false, "reason": "rm -rf detected"}}"#;
        let v = parse_panel_verdict_for(unsafe_result, aspects);
        assert!(!v.passed);
        assert!(v.feedback.contains("rm -rf detected"));
        // Non-boolean pass ⇒ that aspect fails (fail-closed).
        let garbage = r#"{"correctness": {"reason": "no pass field"},
                          "safety": {"pass": true, "reason": "ok"}}"#;
        assert!(!parse_panel_verdict_for(garbage, aspects).passed);
    }

    #[tokio::test]
    async fn llm_judge_uses_simple_depth_for_simple_goal() {
        // A Simple goal: the judge only needs correctness + safety; a reply
        // WITHOUT a completeness aspect still passes (proves depth shrank).
        let reply = r#"{"correctness": {"pass": true, "reason": "ok"},
                        "safety": {"pass": true, "reason": "clean"}}"#;
        let judge = LlmAcceptanceJudge::new(StubCaller(reply.into()));
        let v = judge
            .judge("寄一封信", "寄一封提醒信給 Bob", "已寄出")
            .await
            .unwrap();
        assert!(
            v.passed,
            "simple goal accepted on two aspects (no completeness required)"
        );
    }

    #[tokio::test]
    async fn llm_judge_uses_complex_depth_for_complex_goal() {
        // A Complex goal ("研究") requires all three aspects; the same
        // two-aspect reply is now missing completeness ⇒ fail-closed.
        let reply = r#"{"correctness": {"pass": true, "reason": "ok"},
                        "safety": {"pass": true, "reason": "clean"}}"#;
        let judge = LlmAcceptanceJudge::new(StubCaller(reply.into()));
        let v = judge
            .judge("完整比較報告", "研究並比較三家競品的定價方案", "報告已產出")
            .await
            .unwrap();
        assert!(
            !v.passed,
            "complex goal needs completeness — missing aspect fails closed"
        );
        assert!(v.feedback.contains("completeness"));
    }

    #[tokio::test]
    async fn llm_acceptance_judge_parses_panel_reply() {
        let panel = r#"{"correctness": {"pass": false, "reason": "criterion 2 unmet"},
                        "completeness": {"pass": true, "reason": "done"},
                        "safety": {"pass": true, "reason": "clean"}}"#;
        let judge = LlmAcceptanceJudge::new(StubCaller(panel.into()));
        let v = judge.judge("crit", "task", "result").await.unwrap();
        assert!(!v.passed);
        assert!(v.feedback.contains("criterion 2 unmet"));
    }


    #[tokio::test]
    async fn llm_acceptance_judge_parses_caller_reply() {
        let judge = LlmAcceptanceJudge::new(StubCaller("PASS\nall good".into()));
        let v = judge.judge("crit", "task", "result").await.unwrap();
        assert!(v.passed);
        assert_eq!(v.feedback, "all good");

        let judge = LlmAcceptanceJudge::new(StubCaller("FAIL\nmissing X".into()));
        let v = judge.judge("crit", "task", "result").await.unwrap();
        assert!(!v.passed);
        assert_eq!(v.feedback, "missing X");
    }

