//! Text-derived signal tests: gap fingerprinting (H4) and the
//! premature-stop regex panel (H5).

mod goal_gap_fingerprint {
    use super::super::*;

    // ── DoD: same gap reworded → same fingerprint ────────────

    #[test]
    fn same_citation_different_wording_same_fingerprint() {
        let a = "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120, please add a check.";
        let b = "You forgot proper error handling at crates/duduclaw-gateway/src/goal_loop.rs:120 — add validation.";
        let fp_a = gap_fingerprint(a).expect("a has a citation");
        let fp_b = gap_fingerprint(b).expect("b has a citation");
        assert_eq!(
            fp_a, fp_b,
            "reworded feedback citing the same path:line must fingerprint identically"
        );
    }

    #[test]
    fn same_key_token_different_wording_same_fingerprint() {
        let a = "The function `parse_state_update` does not validate its input.";
        let b = "Please validate input inside `parse_state_update` before use.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── DoD: different gap → different fingerprint ───────────

    #[test]
    fn different_citation_different_fingerprint() {
        let a = "Missing error handling in crates/duduclaw-gateway/src/goal_loop.rs:120.";
        let b = "Missing error handling in crates/duduclaw-gateway/src/goal_state.rs:42.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn different_line_same_file_different_fingerprint() {
        let a = "See crates/duduclaw-gateway/src/goal_loop.rs:120 for the missing check.";
        let b = "See crates/duduclaw-gateway/src/goal_loop.rs:999 for the missing check.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── DoD: no citation at all → None (caller falls back to string compare) ──

    #[test]
    fn no_citation_returns_none() {
        assert_eq!(
            gap_fingerprint("The summary is too vague, please clarify what you did."),
            None
        );
        assert_eq!(
            gap_fingerprint("驗收未通過,說明太模糊,請補充你實際做了什麼。"),
            None
        );
    }

    #[test]
    fn empty_feedback_returns_none() {
        assert_eq!(gap_fingerprint(""), None);
        assert_eq!(gap_fingerprint("   "), None);
    }

    // ── Scratch/temp path normalization ───────────────────────

    #[test]
    fn scratch_uuid_dirs_collapse_to_same_fingerprint() {
        let a =
            "Output written to /tmp/9f8c1e2a-1111-2222-3333-444455556666/output.log:12 is wrong.";
        let b =
            "Output written to /tmp/aa11bb22-9999-8888-7777-666655554444/output.log:12 is wrong.";
        let fp_a = gap_fingerprint(a).expect("a has a citation");
        let fp_b = gap_fingerprint(b).expect("b has a citation");
        assert_eq!(
            fp_a, fp_b,
            "two different random scratch dirs citing the same logical file:line must collapse"
        );
    }

    #[test]
    fn scratch_hex_hash_dirs_collapse_to_same_fingerprint() {
        let a = "See /scratch/9f8c1e2a3b4d5e6f/report.json:5.";
        let b = "See /scratch/deadbeefcafebabe/report.json:5.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn non_scratch_paths_are_not_collapsed() {
        // Real, meaningful directory names must survive unnormalized —
        // only tmp/temp/scratch/UUID/hex-hash segments get replaced.
        let a = "See crates/duduclaw-gateway/src/goal_loop.rs:120.";
        let b = "See crates/duduclaw-security/src/goal_loop.rs:120.";
        assert_ne!(gap_fingerprint(a), gap_fingerprint(b));
    }

    // ── Normalization: lowercase / dedupe / sort (order-independent) ──

    #[test]
    fn fingerprint_is_case_insensitive() {
        let a = "See CRATES/DuDuClaw-Gateway/SRC/Goal_Loop.rs:120.";
        let b = "see crates/duduclaw-gateway/src/goal_loop.rs:120.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = "First check goal_loop.rs:10, then check goal_state.rs:20.";
        let b = "First check goal_state.rs:20, then check goal_loop.rs:10.";
        assert_eq!(gap_fingerprint(a), gap_fingerprint(b));
    }

    #[test]
    fn duplicate_citations_dedupe() {
        let single = "See goal_loop.rs:120.";
        let doubled = "See goal_loop.rs:120 — really, goal_loop.rs:120 is broken.";
        assert_eq!(gap_fingerprint(single), gap_fingerprint(doubled));
    }

    #[test]
    fn citation_with_column_is_captured() {
        let fp = gap_fingerprint("Error at goal_loop.rs:120:5 — fix the type.").unwrap();
        assert!(fp.contains("120:5"));
    }
}

mod goal_bail_detect {
    use super::super::*;

    // ── last_nonempty_paragraph ───────────────────────────────

    #[test]
    fn last_paragraph_picks_final_block() {
        let text = "First I did X.\n\nThen I did Y.\n\nI'll stop here for now.";
        assert_eq!(last_nonempty_paragraph(text), "I'll stop here for now.");
    }

    #[test]
    fn last_paragraph_falls_back_to_whole_text_without_blank_lines() {
        let text = "single block, no blank-line separators at all";
        assert_eq!(last_nonempty_paragraph(text), text);
    }

    #[test]
    fn last_paragraph_ignores_trailing_blank_paragraphs() {
        let text = "Did the work.\n\n   \n\n";
        assert_eq!(last_nonempty_paragraph(text), "Did the work.");
    }

    #[test]
    fn last_paragraph_of_empty_text_is_empty() {
        assert_eq!(last_nonempty_paragraph(""), "");
        assert_eq!(last_nonempty_paragraph("   \n\n  "), "");
    }

    // ── per-pattern positive + negative regression (H5 DoD) ───

    #[test]
    fn unable_to_proceed_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("I am unable to proceed with this task."),
            Some("unable_to_proceed")
        );
        assert_eq!(
            detect_bail_pattern("我目前無法繼續完成這個任務。"),
            Some("unable_to_proceed")
        );
    }
    #[test]
    fn unable_to_proceed_does_not_match_ordinary_continuation() {
        assert_eq!(
            detect_bail_pattern("I fixed the bug and re-ran the tests, all green."),
            None
        );
    }

    #[test]
    fn giving_up_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("I give up on this approach."),
            Some("giving_up")
        );
        assert_eq!(detect_bail_pattern("我決定放棄了。"), Some("giving_up"));
    }
    #[test]
    fn giving_up_does_not_match_mid_paragraph_mention() {
        // The word "give up" mid-paragraph (not at the anchor) must not fire —
        // exactly the false-positive grok's `^`-anchoring exists to avoid.
        assert_eq!(
            detect_bail_pattern("I refactored the retry loop so it never has to give up early."),
            None
        );
    }

    #[test]
    fn stopping_here_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("I'll stop here for now and wait."),
            Some("stopping_here")
        );
        assert_eq!(
            detect_bail_pattern("我先做到這裡好了。"),
            Some("stopping_here")
        );
    }
    #[test]
    fn stopping_here_does_not_match_ordinary_text() {
        assert_eq!(
            detect_bail_pattern("The function stops here when the queue is empty."),
            None
        );
    }

    #[test]
    fn agents_in_flight_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("Waiting for the other agent to finish its part first."),
            Some("agents_in_flight")
        );
        assert_eq!(
            detect_bail_pattern("等其他 agent 完成後再繼續處理。"),
            Some("agents_in_flight")
        );
    }
    #[test]
    fn agents_in_flight_does_not_match_unrelated_agent_mention() {
        assert_eq!(
            detect_bail_pattern("The agent field on TaskRow was renamed to assigned_to."),
            None
        );
    }

    #[test]
    fn check_back_later_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("Please check back later for the final result."),
            Some("check_back_later")
        );
        assert_eq!(
            detect_bail_pattern("請你稍後再來查看結果。"),
            Some("check_back_later")
        );
    }
    #[test]
    fn check_back_later_does_not_match_bare_later_mention() {
        // Deliberate: a bare "later" without the full deflection shape must
        // not fire — the design explicitly excludes a broad "延後" style.
        assert_eq!(
            detect_bail_pattern("I will handle the edge case later in this same round."),
            None
        );
    }

    #[test]
    fn verdict_line_matches_self_signed_verdict() {
        assert_eq!(detect_bail_pattern("VERDICT: PASS"), Some("verdict_line"));
        assert_eq!(
            detect_bail_pattern("verdict: complete"),
            Some("verdict_line")
        );
    }
    #[test]
    fn verdict_line_does_not_match_prose_mentioning_verdict() {
        assert_eq!(
            detect_bail_pattern("The judge's verdict will determine the next step."),
            None
        );
    }

    #[test]
    fn commit_push_pr_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("I've committed and pushed the changes to the branch."),
            Some("commit_push_pr")
        );
        assert_eq!(
            detect_bail_pattern("已經 commit 並 push 完成。"),
            Some("commit_push_pr")
        );
    }
    #[test]
    fn commit_push_pr_does_not_match_unrelated_git_mention() {
        assert_eq!(
            detect_bail_pattern("Run git log to see the commit history for this file."),
            None
        );
    }

    #[test]
    fn ready_for_review_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("This is ready for your review now."),
            Some("ready_for_review")
        );
        assert_eq!(
            detect_bail_pattern("請你審核一下。"),
            Some("ready_for_review")
        );
    }
    #[test]
    fn ready_for_review_does_not_match_unrelated_review_mention() {
        assert_eq!(
            detect_bail_pattern("The code review checklist has six items."),
            None
        );
    }

    #[test]
    fn please_deflection_matches_en_and_zh() {
        assert_eq!(
            detect_bail_pattern("Let me know if you'd like me to continue with the next step."),
            Some("please_deflection")
        );
        assert_eq!(
            detect_bail_pattern("請告訴我是否要我繼續？"),
            Some("please_deflection")
        );
    }
    #[test]
    fn please_deflection_does_not_match_ordinary_question() {
        assert_eq!(
            detect_bail_pattern("Should the timeout be 30s or 60s? I used 30s for now."),
            None
        );
    }

    // ── panel-level behavior ───────────────────────────────────

    #[test]
    fn ordinary_completion_text_matches_nothing() {
        let text = "Implemented the feature, ran cargo test, all 42 tests pass. \
                     Result summary posted via tasks_complete.";
        assert_eq!(detect_bail_pattern(text), None);
    }

    #[test]
    fn only_the_last_paragraph_is_considered() {
        // A bail phrase in an EARLIER paragraph must not fire — only the
        // final paragraph is the agent's actual last word.
        let text = "I give up.\n\nActually, never mind — I found the fix and finished the task.";
        assert_eq!(detect_bail_pattern(text), None);
    }

    #[test]
    fn empty_completion_matches_nothing() {
        assert_eq!(detect_bail_pattern(""), None);
    }

    #[test]
    fn pattern_panel_has_nine_patterns() {
        assert_eq!(pattern_names().count(), 9);
    }

    #[test]
    fn pattern_names_are_unique() {
        let names: Vec<&str> = pattern_names().collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            names.len(),
            sorted.len(),
            "pattern names must be unique (telemetry labels)"
        );
    }
}
