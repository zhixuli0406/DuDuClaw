//! Deterministic, zero-LLM assertions over a parsed [`EvalTranscript`].
//!
//! Every configured `[expect]` field produces exactly one
//! [`AssertionResult`], so reports stay 1:1 with the case file and a
//! regression diff is readable at a glance.

use super::case::{ExpectSpec, GroundedSpec};
use super::transcript::{EvalTranscript, ToolInvocation};

/// Outcome of one assertion.
#[derive(Debug, serde::Serialize)]
pub struct AssertionResult {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

/// Tool-name matcher: exact equality, or the final `__`-delimited segment
/// (token-anchored — never a raw substring check, per the project's
/// "no unanchored contains for routing decisions" convention). Lets a case
/// write `tasks_create` and match `mcp__duduclaw__tasks_create`.
fn tool_name_matches(actual: &str, expected: &str) -> bool {
    actual == expected || actual.rsplit("__").next() == Some(expected)
}

fn used_tool(t: &EvalTranscript, expected: &str) -> bool {
    t.tool_uses.iter().any(|u| tool_name_matches(&u.name, expected))
}

/// Run every configured assertion; unconfigured fields produce nothing.
pub fn run_assertions(expect: &ExpectSpec, t: &EvalTranscript) -> Vec<AssertionResult> {
    let mut out = Vec::new();
    let used: Vec<&str> = t.tool_uses.iter().map(|u| u.name.as_str()).collect();

    for tool in &expect.must_use_tools {
        let hit = used_tool(t, tool);
        out.push(AssertionResult {
            name: format!("must_use_tools: {tool}"),
            passed: hit,
            detail: if hit {
                "tool was invoked".into()
            } else {
                format!("never invoked; observed tools: {used:?}")
            },
        });
    }

    for tool in &expect.must_not_use_tools {
        let hit = used_tool(t, tool);
        out.push(AssertionResult {
            name: format!("must_not_use_tools: {tool}"),
            passed: !hit,
            detail: if hit {
                "FORBIDDEN tool was invoked".into()
            } else {
                "tool was not invoked".into()
            },
        });
    }

    for needle in &expect.output_contains {
        let hit = t.final_text.contains(needle.as_str());
        out.push(AssertionResult {
            name: format!("output_contains: {needle:?}"),
            passed: hit,
            detail: if hit {
                "found in final answer".into()
            } else {
                format!(
                    "missing from final answer (answer starts: {:?})",
                    duduclaw_core::truncate_chars(&t.final_text, 120)
                )
            },
        });
    }

    for needle in &expect.output_not_contains {
        let hit = t.final_text.contains(needle.as_str());
        out.push(AssertionResult {
            name: format!("output_not_contains: {needle:?}"),
            passed: !hit,
            detail: if hit {
                "FORBIDDEN string present in final answer".into()
            } else {
                "absent from final answer".into()
            },
        });
    }

    if let Some(re_src) = &expect.output_regex {
        // Validated at load time; a compile failure here still fails closed.
        let (passed, detail) = match regex::Regex::new(re_src) {
            Ok(re) => {
                let hit = re.is_match(&t.final_text);
                (
                    hit,
                    if hit {
                        "final answer matches".to_string()
                    } else {
                        "final answer does not match".to_string()
                    },
                )
            }
            Err(e) => (false, format!("regex failed to compile: {e}")),
        };
        out.push(AssertionResult {
            name: format!("output_regex: {re_src:?}"),
            passed,
            detail,
        });
    }

    if let Some(min) = expect.min_text_blocks {
        let passed = t.text_blocks >= min;
        out.push(AssertionResult {
            name: format!("min_text_blocks: {min}"),
            passed,
            detail: format!("observed {} text block(s)", t.text_blocks),
        });
    }

    if let Some(max) = expect.max_tool_calls {
        let n = t.tool_uses.len() as u32;
        out.push(AssertionResult {
            name: format!("max_tool_calls: {max}"),
            passed: n <= max,
            detail: format!("observed {n} tool call(s)"),
        });
    }

    for spec in &expect.grounded {
        out.push(run_grounded(spec, t));
    }

    out
}

/// WP4 GroundEval (arXiv:2606.22737): the final answer must be traceable to
/// actual tool evidence — a non-error call to `spec.tool` whose `result_text`
/// shares a contiguous run of >= `min_overlap_chars` with the final answer
/// (and, when `output_regex` is set, the regex's matched fragment of the
/// final answer must also appear in that result text). Pure and
/// deterministic; never panics on absent evidence — it fails the assertion
/// instead, with a detail that tells the author how to fix it.
fn run_grounded(spec: &GroundedSpec, t: &EvalTranscript) -> AssertionResult {
    let name = format!(
        "grounded: tool={} min_overlap_chars={}",
        spec.tool, spec.min_overlap_chars
    );

    let calls: Vec<&ToolInvocation> = t
        .tool_uses
        .iter()
        .filter(|u| tool_name_matches(&u.name, &spec.tool) && !u.is_error)
        .collect();
    if calls.is_empty() {
        return AssertionResult {
            name,
            passed: false,
            detail: format!("tool {:?} was never invoked without error", spec.tool),
        };
    }

    let with_results: Vec<&&ToolInvocation> =
        calls.iter().filter(|u| u.result_text.is_some()).collect();
    if with_results.is_empty() {
        return AssertionResult {
            name,
            passed: false,
            detail: "transcript lacks tool_result for this tool; re-record with --record".into(),
        };
    }

    let overlap_hit = with_results.iter().any(|u| {
        let result_text = u.result_text.as_deref().unwrap_or("");
        shares_contiguous_run(&t.final_text, result_text, spec.min_overlap_chars)
    });
    if !overlap_hit {
        return AssertionResult {
            name,
            passed: false,
            detail: format!(
                "final answer shares no {}-char run with any {:?} result",
                spec.min_overlap_chars, spec.tool
            ),
        };
    }

    if let Some(re_src) = &spec.output_regex {
        let re = match regex::Regex::new(re_src) {
            Ok(re) => re,
            Err(e) => {
                return AssertionResult {
                    name,
                    passed: false,
                    detail: format!("output_regex failed to compile: {e}"),
                };
            }
        };
        let Some(m) = re.find(&t.final_text) else {
            return AssertionResult {
                name,
                passed: false,
                detail: "output_regex does not match the final answer".into(),
            };
        };
        let matched = m.as_str();
        let regex_grounded = with_results
            .iter()
            .any(|u| u.result_text.as_deref().unwrap_or("").contains(matched));
        if !regex_grounded {
            return AssertionResult {
                name,
                passed: false,
                detail: format!(
                    "output_regex match {matched:?} not found in any {:?} result",
                    spec.tool
                ),
            };
        }
    }

    AssertionResult {
        name,
        passed: true,
        detail: "final answer is grounded in tool result".into(),
    }
}

/// CJK-safe (char-based, never raw byte slicing) check for whether `a` and
/// `b` share a contiguous run of at least `min_len` chars. Slides a
/// `min_len`-char window across `a` and looks it up in `b`; O(|a| * min_len)
/// — acceptable at eval-transcript scale (design ceiling, WP4 spec).
fn shares_contiguous_run(a: &str, b: &str, min_len: usize) -> bool {
    if min_len == 0 {
        return true;
    }
    let a_chars: Vec<char> = a.chars().collect();
    if a_chars.len() < min_len || b.chars().count() < min_len {
        return false;
    }
    for start in 0..=(a_chars.len() - min_len) {
        let window: String = a_chars[start..start + min_len].iter().collect();
        if b.contains(&window) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(text: &str, tools: &[&str]) -> EvalTranscript {
        EvalTranscript {
            final_text: text.to_string(),
            tool_uses: tools
                .iter()
                .map(|n| ToolInvocation {
                    name: n.to_string(),
                    input: serde_json::Value::Null,
                    ..Default::default()
                })
                .collect(),
            text_blocks: 1,
            ..Default::default()
        }
    }

    #[test]
    fn tool_matching_is_token_anchored() {
        assert!(tool_name_matches("Bash", "Bash"));
        assert!(tool_name_matches("mcp__duduclaw__tasks_create", "tasks_create"));
        // No raw substring matching: `create` must not match `tasks_create`.
        assert!(!tool_name_matches("mcp__duduclaw__tasks_create", "create"));
        assert!(!tool_name_matches("BashOutput", "Bash"));
    }

    #[test]
    fn must_use_and_must_not_use() {
        let t = transcript("ok", &["mcp__duduclaw__tasks_create", "Read"]);
        let expect = ExpectSpec {
            must_use_tools: vec!["tasks_create".into()],
            must_not_use_tools: vec!["Bash".into()],
            ..Default::default()
        };
        let results = run_assertions(&expect, &t);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.passed), "{results:?}");

        let expect_fail = ExpectSpec {
            must_use_tools: vec!["Bash".into()],
            must_not_use_tools: vec!["Read".into()],
            ..Default::default()
        };
        let results = run_assertions(&expect_fail, &t);
        assert!(results.iter().all(|r| !r.passed), "{results:?}");
    }

    #[test]
    fn output_string_and_regex_checks() {
        let t = transcript("Refund for order #1234 approved.", &[]);
        let expect = ExpectSpec {
            output_contains: vec!["order #1234".into()],
            output_not_contains: vec!["sk-ant-".into()],
            output_regex: Some(r"(?i)refund.*approved".into()),
            ..Default::default()
        };
        let results = run_assertions(&expect, &t);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.passed), "{results:?}");
    }

    #[test]
    fn budget_and_text_block_checks() {
        let t = transcript("ok", &["Read", "Read", "Read"]);
        let expect = ExpectSpec {
            min_text_blocks: Some(2),
            max_tool_calls: Some(2),
            ..Default::default()
        };
        let results = run_assertions(&expect, &t);
        assert!(!results[0].passed); // only 1 text block
        assert!(!results[1].passed); // 3 > 2 tool calls
    }

    #[test]
    fn empty_expect_produces_no_assertions() {
        let t = transcript("ok", &[]);
        assert!(run_assertions(&ExpectSpec::default(), &t).is_empty());
    }

    // ── WP4 GroundEval: `grounded` assertion ────────────────────

    fn transcript_with_result(text: &str, tool: &str, result_text: Option<&str>, is_error: bool) -> EvalTranscript {
        EvalTranscript {
            final_text: text.to_string(),
            tool_uses: vec![ToolInvocation {
                name: tool.to_string(),
                input: serde_json::Value::Null,
                id: Some("tu_1".into()),
                result_text: result_text.map(String::from),
                is_error,
            }],
            text_blocks: 1,
            ..Default::default()
        }
    }

    #[test]
    fn shares_contiguous_run_is_cjk_safe_and_order_sensitive() {
        // Plain ASCII overlap.
        assert!(shares_contiguous_run("order #1234 confirmed", "we confirmed order #1234 today", 11));
        // CJK: char-counted, not byte-counted (each CJK char is 3 UTF-8 bytes).
        assert!(shares_contiguous_run("退款政策：三十天內可退款", "查詢結果：退款政策：三十天內可退款。", 8));
        // No shared run of the required length.
        assert!(!shares_contiguous_run("abcdefgh", "zzzzzzzz", 4));
        // min_len larger than either string ⇒ false, not a panic.
        assert!(!shares_contiguous_run("ab", "abcdef", 5));
    }

    #[test]
    fn grounded_passes_when_final_text_overlaps_tool_result() {
        let t = transcript_with_result(
            "Refund policy: 30 days from purchase, receipt required.",
            "mcp__duduclaw__memory_search",
            Some("Refund policy: 30 days from purchase, receipt required."),
            false,
        );
        let spec = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 12,
            output_regex: None,
        };
        let r = run_grounded(&spec, &t);
        assert!(r.passed, "{r:?}");
    }

    #[test]
    fn grounded_fails_when_tool_never_called() {
        let t = transcript("some answer", &[]);
        let spec = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 12,
            output_regex: None,
        };
        let r = run_grounded(&spec, &t);
        assert!(!r.passed);
        assert!(r.detail.contains("never invoked"));
    }

    #[test]
    fn grounded_fails_when_only_error_calls_exist() {
        let t = transcript_with_result(
            "some answer",
            "memory_search",
            Some("boom"),
            true, // is_error — must not count as evidence
        );
        let spec = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 4,
            output_regex: None,
        };
        let r = run_grounded(&spec, &t);
        assert!(!r.passed);
        assert!(r.detail.contains("never invoked"));
    }

    #[test]
    fn grounded_fails_closed_on_legacy_transcript_without_result_text() {
        let t = transcript_with_result("some answer", "memory_search", None, false);
        let spec = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 4,
            output_regex: None,
        };
        let r = run_grounded(&spec, &t);
        assert!(!r.passed);
        assert!(r.detail.contains("--record"), "unexpected detail: {}", r.detail);
    }

    #[test]
    fn grounded_fails_when_answer_does_not_overlap_result() {
        let t = transcript_with_result(
            "I handled the request successfully.",
            "memory_search",
            Some("Refund policy: 30 days from purchase."),
            false,
        );
        let spec = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 12,
            output_regex: None,
        };
        let r = run_grounded(&spec, &t);
        assert!(!r.passed);
        assert!(r.detail.contains("shares no"));
    }

    #[test]
    fn grounded_output_regex_must_be_backed_by_tool_result() {
        let t = transcript_with_result(
            "Refund policy: 30 days from purchase.",
            "memory_search",
            Some("Refund policy: 30 days from purchase."),
            false,
        );
        let passing = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 12,
            output_regex: Some(r"\d+ days".into()),
        };
        assert!(run_grounded(&passing, &t).passed);

        // Regex matches the final answer but the matched fragment is not in
        // the tool result ⇒ fabricated detail, must fail.
        let t2 = transcript_with_result(
            "Refund policy: 45 days from purchase.",
            "memory_search",
            Some("Refund policy: 45 days from purchase."), // overlap holds…
            false,
        );
        let mismatched = GroundedSpec {
            tool: "memory_search".into(),
            min_overlap_chars: 12,
            output_regex: Some(r"\d+ years".into()), // …but regex never matches
        };
        let r = run_grounded(&mismatched, &t2);
        assert!(!r.passed);
        assert!(r.detail.contains("does not match"));
    }

    #[test]
    fn grounded_matches_tool_name_by_final_segment() {
        let t = transcript_with_result(
            "policy: 30 days no questions asked",
            "mcp__duduclaw__memory_search",
            Some("policy: 30 days no questions asked"),
            false,
        );
        let spec = GroundedSpec {
            tool: "memory_search".into(), // matches via `__`-segment, not full name
            min_overlap_chars: 10,
            output_regex: None,
        };
        assert!(run_grounded(&spec, &t).passed);
    }

    #[test]
    fn run_assertions_wires_up_grounded() {
        let t = transcript_with_result(
            "policy: 30 days",
            "memory_search",
            Some("policy: 30 days"),
            false,
        );
        let expect = ExpectSpec {
            grounded: vec![GroundedSpec {
                tool: "memory_search".into(),
                min_overlap_chars: 8,
                output_regex: None,
            }],
            ..Default::default()
        };
        let results = run_assertions(&expect, &t);
        assert_eq!(results.len(), 1);
        assert!(results[0].name.starts_with("grounded:"));
        assert!(results[0].passed);
    }
}
