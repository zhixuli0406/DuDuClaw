//! Premature-stop ("bail") detection for the goal loop — **H5**
//! (`commercial/docs/DESIGN-harness-borrowings-2026-08.md` WP-B, borrowed
//! from grok-build's `goal_stop_detector.rs`, nine `^`-anchored regexes
//! compared only against the turn's last non-empty paragraph).
//!
//! ## Design
//!
//! An agent working an autonomous goal round sometimes ends its turn with
//! language that *sounds* like a wrap-up but is not judge-verified
//! completion: "I'll continue this later", a self-signed `VERDICT: PASS`,
//! deflecting the decision back to a human that is not actually in the
//! loop, and so on. None of these are failures the MAV judge would
//! necessarily catch by content alone (the claimed work may even be
//! correct) — they are a *process* smell worth surfacing as telemetry and
//! as a hint for the next round, independent of whatever the judge decides.
//!
//! [`detect_bail_pattern`] runs a fixed panel of nine named zh+en anchored
//! patterns against ONLY the last non-empty paragraph of the agent's
//! completion text (`^`-anchored, mirroring grok's own anti-false-positive
//! design — a stray mid-paragraph mention like "I don't want to give up on
//! this" must not trigger `giving_up`). Patterns are deliberately narrow:
//! per the source design, a broad "deferral" style was intentionally
//! excluded because its false-positive rate would drown the signal.
//!
//! Each pattern is a source-level constant with its own regression test —
//! see the `tests` module below — so a future edit to the phrasing can't
//! silently regress recall for an existing pattern without a red test.

use std::sync::LazyLock;

use regex::Regex;

/// One named bail pattern: `(name, regex source)`. `name` is the stable
/// telemetry label (Prometheus label value + activity metadata) — never
/// renamed without also updating any dashboard query against it.
///
/// Each regex is `(?i)^...` — case-insensitive, anchored to the START of
/// the (already-trimmed) last non-empty paragraph. A short, common
/// lead-in (punctuation/whitespace) is tolerated by `^\s*` where natural
/// phrasing calls for it; the anchor's job is to reject the pattern
/// appearing buried mid-paragraph, not to require the literal first
/// character to match.
// NOTE: every pattern is written on a SINGLE line deliberately. Raw string
// literals (`r"..."`) do not support Rust's `\<newline>` line-continuation
// (that only applies to normal, non-raw strings) and regex's verbose `(?x)`
// mode would strip the literal spaces inside multi-word phrases like
// "check back later" — so a multi-line raw string here would silently
// embed literal newline/indentation characters into the compiled pattern
// instead of just being source formatting. Single-line keeps the compiled
// pattern exactly what it looks like.
const BAIL_PATTERNS_SRC: &[(&str, &str)] = &[
    // 1. unable_to_proceed — agent states it literally cannot continue.
    (
        "unable_to_proceed",
        r"(?i)^\s*(i\s*(?:am|'m)?\s*(?:currently\s+)?unable to (?:proceed|continue|complete this)\b|i\s*can(?:not|'t)\s*(?:proceed|continue)(?:\s+(?:with this|further))?\b|(?:我)?(?:目前)?無法(?:繼續|再繼續|完成)(?:這(?:個|項)?)?(?:任務|工作)?)",
    ),
    // 2. giving_up — agent explicitly gives up.
    (
        "giving_up",
        r"(?i)^\s*(i\s*(?:am|'m)?\s*giving up\b|i give up\b|我(?:決定)?放棄(?:了)?(?:這個)?(?:任務)?)",
    ),
    // 3. stopping_here — agent declares an unprompted stop point.
    (
        "stopping_here",
        r"(?i)^\s*(i(?:'ll| will)? stop here\b|stopping here\b|我(?:先)?(?:做|停)到這裡(?:好了)?|到此為止)",
    ),
    // 4. agents_in_flight — deferring because of other (sub)agents still working.
    (
        "agents_in_flight",
        r"(?i)^\s*(waiting for (?:the )?other agents?\b|another agent is (?:still )?(?:working|running)\b|等(?:其他|另一個)\s*agent(?:s)?\s*(?:完成|處理完)|其他(?:任務|代理)(?:仍在|還在)(?:執行|進行)中)",
    ),
    // 5. check_back_later — asks the human to come back later.
    (
        "check_back_later",
        r"(?i)^\s*(please check back later\b|i(?:'ll| will) continue (?:this )?later\b|check back (?:with me )?later\b|請(?:你)?(?:稍後|晚點|等等)再(?:來)?(?:查看|回來|試)|晚點(?:再)?回來)",
    ),
    // 6. verdict_line — agent self-signs a VERDICT line that should only
    //    ever come from the judge.
    (
        "verdict_line",
        r"(?i)^\s*verdict\s*[:：]\s*(?:pass|complete|done|accepted)\b",
    ),
    // 7. commit_push_pr — treats "committed/pushed/opened a PR" as the
    //    finish line instead of actual verified completion.
    (
        "commit_push_pr",
        r"(?i)^\s*(i(?:'ve| have)? committed (?:and )?pushed\b|changes (?:have been )?committed and pushed\b|(?:i(?:'ve| have)? )?opened a pr\b|已(?:經)?\s*commit\s*(?:並|和)?\s*push|已(?:提交|推送)(?:了)?\s*(?:pr|變更|程式碼))",
    ),
    // 8. ready_for_review — announces done and hands off to a reviewer
    //    without further verification, unprompted.
    (
        "ready_for_review",
        r"(?i)^\s*(this is ready for (?:your )?review\b|ready for (?:your )?review\b|please review(?: this)?\b|(?:已(?:經)?完成)?[,，]?\s*請(?:你)?(?:幫忙)?審(?:核|查)(?:一下)?)",
    ),
    // 9. please_deflection — deflects the "should I continue" decision back
    //    to a human who is not actually present in an autonomous loop.
    (
        "please_deflection",
        r"(?i)^\s*(let me know if you(?:'d| would) like me to continue\b|please let me know how (?:you'd like me )?to proceed\b|shall i continue\??|請(?:告訴我|問)(?:是否)?要(?:我)?繼續(?:嗎)?[?？]?|如(?:有)?需要(?:的話)?我可以繼續)",
    ),
];

static BAIL_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    BAIL_PATTERNS_SRC
        .iter()
        .map(|(name, src)| {
            (
                *name,
                Regex::new(src)
                    .unwrap_or_else(|e| panic!("bail pattern {name:?} must compile: {e}")),
            )
        })
        .collect()
});

/// All pattern names, in panel order — used by telemetry render code and
/// tests that want to assert coverage over the full panel.
pub fn pattern_names() -> impl Iterator<Item = &'static str> {
    BAIL_PATTERNS_SRC.iter().map(|(name, _)| *name)
}

/// The last non-empty, trimmed paragraph of `text` (paragraphs split on a
/// blank line). Falls back to the whole trimmed text when there is no blank
/// line separator (a single-paragraph completion is common). Never panics
/// on empty input — returns `""`.
pub fn last_nonempty_paragraph(text: &str) -> &str {
    // `str::split` on a `&str` pattern is a forward-only `Split` (its
    // `Searcher` is not `DoubleEndedSearcher`), so `Map`/`Filter` over it
    // cannot implement `DoubleEndedIterator` and `.next_back()` does not
    // type-check here. `.last()` is the semantically-identical fix (same
    // "last paragraph satisfying the filter" result, just a forward scan) —
    // trivial cost for a single completion's paragraph list.
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .last()
        .unwrap_or("")
}

/// Match the fixed bail-pattern panel against the last non-empty paragraph
/// of an agent's completion text. Returns the FIRST matching pattern's
/// stable name (panel order), or `None` if nothing matched (the overwhelming
/// common case — most completions are not a premature stop).
pub fn detect_bail_pattern(text: &str) -> Option<&'static str> {
    let last = last_nonempty_paragraph(text);
    if last.is_empty() {
        return None;
    }
    BAIL_PATTERNS
        .iter()
        .find(|(_, re)| re.is_match(last))
        .map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

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
