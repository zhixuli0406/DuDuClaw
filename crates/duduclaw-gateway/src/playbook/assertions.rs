//! WP2.8 (D8=B) — zero-LLM replay of an entry's E1 assertions against a
//! linked eval case's **recorded transcript**.
//!
//! Schema lives in [`super::entry::EntryAssertions`]; write-time structural
//! validation in `super::delta::validate_delta`. This module is the *replay*
//! half: given `<eval_cases_root>/<suite>/<case>.transcript.jsonl` (produced
//! by `duduclaw eval <suite> --record`), extract the facts an assertion can
//! see — which tools were called, what the final reply said — and check the
//! entry's assertions against them. Deterministic, zero LLM cost (§3.1's E1
//! row: "Gate 層直接跑,零 LLM").
//!
//! Degrade contract (documented in the E1 schema doc too): **no transcript ⇒
//! `Unverified`, never a veto.** WP2.1's corpus has no recordings yet;
//! failing closed here would freeze every `Add` until an operator records
//! 360 transcripts, which is a strictly worse outcome than E0. The veto arms
//! itself per-case as recordings land.
//!
//! The JSONL parse below is intentionally permissive and mirrors
//! `duduclaw-cli/src/eval/transcript.rs`'s semantics (assistant `tool_use`
//! blocks by name; final text = the `result` line's text, else the last
//! assistant text block) without depending on the cli crate — the two crates
//! have no dependency edge in that direction. Divergence risk is accepted
//! and bounded: an unparseable line is skipped, and a transcript that yields
//! NO facts at all is reported as `Unverified`, not as a pass.

use std::path::Path;

use super::entry::EntryAssertions;
use super::gene::EvalCaseRef;

/// What a transcript exposes to assertion checking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptFacts {
    /// Tool names in call order (duplicates kept — assertions only test
    /// membership today, but the order/count may serve H1/H3 later).
    pub tool_names: Vec<String>,
    /// The final reply text ("" when the transcript never produced one).
    pub final_text: String,
}

impl TranscriptFacts {
    /// True when the parse produced nothing an assertion could see — the
    /// caller must report `Unverified` rather than vacuous compliance.
    pub fn is_empty(&self) -> bool {
        self.tool_names.is_empty() && self.final_text.trim().is_empty()
    }
}

/// Per-(entry × case) replay verdict.
#[derive(Debug, Clone, PartialEq)]
pub enum AssertionReplay {
    /// Transcript present and every assertion held.
    Pass,
    /// Transcript present and at least one assertion failed.
    Violations(Vec<String>),
    /// No transcript / unparseable transcript — nothing was verified.
    Unverified(String),
}

/// Strip an `mcp__<server>__` prefix so assertions can name the bare tool.
fn bare_tool_name(name: &str) -> &str {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.split_once("__").map(|(_, t)| t).unwrap_or(rest),
        None => name,
    }
}

fn tool_matches(assert_token: &str, called: &str) -> bool {
    let t = assert_token.trim();
    t.eq_ignore_ascii_case(called) || t.eq_ignore_ascii_case(bare_tool_name(called))
}

/// ASCII-case-insensitive, CJK-exact substring test.
fn text_contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.trim().to_lowercase())
}

/// Parse a recorded transcript (stream-json JSONL) into [`TranscriptFacts`].
/// Permissive: unparseable lines are skipped.
pub fn parse_transcript(content: &str) -> TranscriptFacts {
    let mut facts = TranscriptFacts::default();
    let mut last_assistant_text: Option<String> = None;
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => {
                let blocks = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .or_else(|| v.get("content"))
                    .and_then(|c| c.as_array());
                if let Some(blocks) = blocks {
                    let mut text_parts: Vec<&str> = Vec::new();
                    for b in blocks {
                        match b.get("type").and_then(|t| t.as_str()) {
                            Some("tool_use") => {
                                if let Some(name) = b.get("name").and_then(|n| n.as_str()) {
                                    facts.tool_names.push(name.to_string());
                                }
                            }
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                    text_parts.push(t);
                                }
                            }
                            _ => {}
                        }
                    }
                    if !text_parts.is_empty() {
                        last_assistant_text = Some(text_parts.join("\n"));
                    }
                }
            }
            Some("result") => {
                if let Some(t) = v.get("result").and_then(|r| r.as_str()) {
                    if !t.trim().is_empty() {
                        facts.final_text = t.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    if facts.final_text.trim().is_empty() {
        if let Some(t) = last_assistant_text {
            facts.final_text = t;
        }
    }
    facts
}

/// Check `assertions` against already-extracted `facts`. Pure.
pub fn check_facts(assertions: &EntryAssertions, facts: &TranscriptFacts) -> Vec<String> {
    let mut violations = Vec::new();
    for t in &assertions.must_use_tools {
        if !facts.tool_names.iter().any(|c| tool_matches(t, c)) {
            violations.push(format!("must_use_tools: `{t}` was never called"));
        }
    }
    for t in &assertions.must_not_use_tools {
        if facts.tool_names.iter().any(|c| tool_matches(t, c)) {
            violations.push(format!("must_not_use_tools: `{t}` was called"));
        }
    }
    for s in &assertions.output_contains {
        if !text_contains(&facts.final_text, s) {
            violations.push(format!("output_contains: `{s}` missing from the final reply"));
        }
    }
    for s in &assertions.output_not_contains {
        if text_contains(&facts.final_text, s) {
            violations.push(format!("output_not_contains: `{s}` present in the final reply"));
        }
    }
    violations
}

/// Locate a case's transcript file under the suites root. Mirrors
/// `eval_scorer::index_suite`'s naming (`<stem>.transcript.jsonl`, with the
/// case possibly living in a `held-out/` subdirectory).
fn transcript_path(root: &Path, case: &EvalCaseRef) -> Option<std::path::PathBuf> {
    let (suite, name) = case.0.split_once('/')?;
    for dir in [root.join(suite), root.join(suite).join("held-out")] {
        let p = dir.join(format!("{name}.transcript.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Replay `assertions` against every linked case that has a recorded
/// transcript. One combined verdict:
/// - any violation on any recorded case ⇒ `Violations`
/// - no recorded case at all ⇒ `Unverified`
/// - at least one recorded case, all clean ⇒ `Pass`
pub fn replay_assertions(
    root: &Path,
    cases: &[EvalCaseRef],
    assertions: &EntryAssertions,
) -> AssertionReplay {
    if !assertions.is_e1() {
        // E0 entries have nothing to replay — vacuously unverified, and the
        // caller should not even ask.
        return AssertionReplay::Unverified("entry carries no E1 assertions".to_string());
    }
    let mut checked = 0usize;
    let mut violations: Vec<String> = Vec::new();
    for case in cases {
        let Some(path) = transcript_path(root, case) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let facts = parse_transcript(&content);
        if facts.is_empty() {
            continue;
        }
        checked += 1;
        for v in check_facts(assertions, &facts) {
            violations.push(format!("{}: {v}", case.0));
        }
    }
    if checked == 0 {
        AssertionReplay::Unverified(
            "no linked case has a recorded transcript — run `duduclaw eval <suite> --record`"
                .to_string(),
        )
    } else if violations.is_empty() {
        AssertionReplay::Pass
    } else {
        AssertionReplay::Violations(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asserts(must_use: &[&str], not_use: &[&str], contains: &[&str], not_contains: &[&str]) -> EntryAssertions {
        EntryAssertions {
            must_use_tools: must_use.iter().map(|s| s.to_string()).collect(),
            must_not_use_tools: not_use.iter().map(|s| s.to_string()).collect(),
            output_contains: contains.iter().map(|s| s.to_string()).collect(),
            output_not_contains: not_contains.iter().map(|s| s.to_string()).collect(),
        }
    }

    const TRANSCRIPT: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__duduclaw__memory_search","input":{}},{"type":"text","text":"查詢中"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"退款政策為 30 天內憑發票辦理。"}]}}
{"type":"result","result":"退款政策為 30 天內憑發票辦理。"}"#;

    #[test]
    fn parse_extracts_tools_and_final_text() {
        let f = parse_transcript(TRANSCRIPT);
        assert_eq!(f.tool_names, vec!["mcp__duduclaw__memory_search"]);
        assert!(f.final_text.contains("30 天"));
    }

    #[test]
    fn must_use_matches_mcp_stripped_name() {
        let f = parse_transcript(TRANSCRIPT);
        assert!(check_facts(&asserts(&["memory_search"], &[], &[], &[]), &f).is_empty());
        let v = check_facts(&asserts(&["web_fetch"], &[], &[], &[]), &f);
        assert_eq!(v.len(), 1, "{v:?}");
    }

    #[test]
    fn output_contains_is_cjk_safe_and_ascii_case_insensitive() {
        let f = parse_transcript(TRANSCRIPT);
        assert!(check_facts(&asserts(&[], &[], &["30 天內憑發票"], &[]), &f).is_empty());
        let v = check_facts(&asserts(&[], &[], &[], &["30 天"]), &f);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn empty_transcript_reports_unverified_not_pass() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("s")).unwrap();
        // No transcript file at all.
        let out = replay_assertions(
            dir.path(),
            &[EvalCaseRef("s/c".to_string())],
            &asserts(&["memory_search"], &[], &[], &[]),
        );
        assert!(matches!(out, AssertionReplay::Unverified(_)));
        // Present but content-free transcript ⇒ still Unverified.
        std::fs::write(dir.path().join("s").join("c.transcript.jsonl"), "not json\n").unwrap();
        let out = replay_assertions(
            dir.path(),
            &[EvalCaseRef("s/c".to_string())],
            &asserts(&["memory_search"], &[], &[], &[]),
        );
        assert!(matches!(out, AssertionReplay::Unverified(_)), "{out:?}");
    }

    #[test]
    fn recorded_transcript_arms_the_veto() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("s")).unwrap();
        std::fs::write(dir.path().join("s").join("c.transcript.jsonl"), TRANSCRIPT).unwrap();
        let ok = replay_assertions(
            dir.path(),
            &[EvalCaseRef("s/c".to_string())],
            &asserts(&["memory_search"], &[], &["30 天"], &[]),
        );
        assert_eq!(ok, AssertionReplay::Pass);
        let bad = replay_assertions(
            dir.path(),
            &[EvalCaseRef("s/c".to_string())],
            &asserts(&[], &["memory_search"], &[], &[]),
        );
        assert!(matches!(bad, AssertionReplay::Violations(v) if v.len() == 1));
    }

    #[test]
    fn held_out_subdirectory_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("s").join("held-out")).unwrap();
        std::fs::write(
            dir.path().join("s").join("held-out").join("c.transcript.jsonl"),
            TRANSCRIPT,
        )
        .unwrap();
        let out = replay_assertions(
            dir.path(),
            &[EvalCaseRef("s/c".to_string())],
            &asserts(&["memory_search"], &[], &[], &[]),
        );
        assert_eq!(out, AssertionReplay::Pass);
    }
}
