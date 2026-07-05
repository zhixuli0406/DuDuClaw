//! Deterministic, zero-LLM assertions over a parsed [`EvalTranscript`].
//!
//! Every configured `[expect]` field produces exactly one
//! [`AssertionResult`], so reports stay 1:1 with the case file and a
//! regression diff is readable at a glance.

use super::case::ExpectSpec;
use super::transcript::EvalTranscript;

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

    out
}

#[cfg(test)]
mod tests {
    use super::super::transcript::ToolInvocation;
    use super::*;

    fn transcript(text: &str, tools: &[&str]) -> EvalTranscript {
        EvalTranscript {
            final_text: text.to_string(),
            tool_uses: tools
                .iter()
                .map(|n| ToolInvocation {
                    name: n.to_string(),
                    input: serde_json::Value::Null,
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
}
