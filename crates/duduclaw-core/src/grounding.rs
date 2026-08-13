//! Runtime-agnostic trace grounding (GroundEval, arXiv:2606.22737): whether a
//! final answer is traceable to actual tool-result evidence, checked via a
//! CJK-safe contiguous character-run overlap rather than an LLM call.
//!
//! Originally implemented only inside the offline eval harness
//! (`duduclaw-cli/src/eval/assertions.rs`, WP4). B3 (2026-08) lifts the core
//! primitives here so the production goal loop
//! (`duduclaw-gateway/src/dispatch_engine.rs`) can run the same zero-LLM
//! check as a pre-flight gate before the MAV acceptance judge, without a
//! `duduclaw-cli` → `duduclaw-gateway` dependency edge. Both crates already
//! depend on `duduclaw-core`.
//!
//! This module is deliberately **regex-free** — neither the eval nor the
//! gateway crate should have to add a new dependency to `duduclaw-core` just
//! to use it, and the base grounding question ("does the answer overlap with
//! *some* tool evidence?") never needed one. Callers that also want to
//! validate an `output_regex` fragment (the eval `[[expect.grounded]]`
//! extension) keep that logic locally.

/// One piece of tool-result evidence, decoupled from any single runtime's
/// transcript format. Eval's `duduclaw-cli` stream-json parser and the
/// gateway's `tool_calls.jsonl` audit reader both adapt into this shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolEvidence {
    /// Tool name as reported by the source (CLI stream-json `tool_use.name`,
    /// or the audit trail's `tool_name`).
    pub tool_name: String,
    /// The tool's result/output text, when captured. `None` when the source
    /// never recorded output text for this call (e.g. today's
    /// `tool_calls.jsonl` audit rows capture inputs and a success flag, not
    /// output — see `dispatch_engine`'s grounding pre-check for how this is
    /// handled: absent result text degrades to "skip", never "reject").
    pub result_text: Option<String>,
    /// The tool CALL's own input text, when captured (the audit trail's
    /// masked `input` field). Fix-2 C1b: a span shared between `final_text`
    /// and `result_text` proves nothing if that same span is just the
    /// caller's own input echoed back — see
    /// [`shares_contiguous_run_excluding_echo`]. `None` when the source
    /// never captured input (never treated as evidence of an echo — only a
    /// *matched* span is disqualified, so an absent `input_text` behaves
    /// exactly like [`shares_contiguous_run`]).
    pub input_text: Option<String>,
    /// Whether this call errored. An erroring call is never valid grounding
    /// evidence — it proves the tool ran, not that it produced the claimed
    /// fact.
    pub is_error: bool,
}

/// Tool names whose MCP response envelope is substantially the caller's own
/// input echoed back (Fix-2 C1, 2026-08 grounding self-echo audit): e.g.
/// `tasks_complete`'s response embeds `result_summary`, which IS the
/// `summary` argument the caller just supplied. Capturing that as grounding
/// `result_text` let an agent's claim be "verified" against its own words —
/// always Grounded, and — worse — if the surrounding task JSON happened to
/// exceed the audit char cap and truncate exactly the echoed span, the same
/// mechanism could flip to a false `NotGrounded` reject. Single source of
/// truth shared by the MCP dispatch call site (which skips `result_text`
/// capture entirely for these tools, `duduclaw-cli/src/mcp.rs`) and the
/// production grounding pre-check (which never credits `confirmed_facts` to
/// evidence from one of these tools, `duduclaw-gateway/src/dispatch_engine.rs`).
pub const SELF_ECHO_TOOL_NAMES: &[&str] = &[
    "tasks_create",
    "tasks_update",
    "tasks_claim",
    "tasks_renew",
    "tasks_complete",
    "tasks_block",
    "activity_post",
    // Cross-wake working state writes echo the agent's own authored
    // value/reason/note back — they can never ground the agent's claims.
    "working_state_set",
    "working_state_clear",
    "working_state_handoff",
];

/// `true` when `name` (matched via [`tool_name_matches`], so an MCP-prefixed
/// name like `mcp__duduclaw__tasks_complete` still matches) is on the
/// [`SELF_ECHO_TOOL_NAMES`] deny-list.
pub fn is_self_echo_tool(name: &str) -> bool {
    SELF_ECHO_TOOL_NAMES
        .iter()
        .any(|t| tool_name_matches(name, t))
}

/// Tool-name matcher: exact equality, or the final `__`-delimited segment
/// (token-anchored — never a raw substring check, per the project's "no
/// unanchored contains for routing decisions" convention). Lets a caller
/// write `tasks_create` and match `mcp__duduclaw__tasks_create`.
pub fn tool_name_matches(actual: &str, expected: &str) -> bool {
    actual == expected || actual.rsplit("__").next() == Some(expected)
}

/// CJK-safe (char-based, never raw byte slicing) check for whether `a` and
/// `b` share a contiguous run of at least `min_len` chars. Slides a
/// `min_len`-char window across `a` and looks it up in `b`;
/// O(|a| * min_len) — acceptable at eval-transcript / single-task-window
/// scale (design ceiling, WP4 spec).
pub fn shares_contiguous_run(a: &str, b: &str, min_len: usize) -> bool {
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

/// Like [`shares_contiguous_run`], but a candidate window is disqualified
/// when it *also* appears verbatim in `exclude` (Fix-2 C1b). Defense in
/// depth alongside [`is_self_echo_tool`]'s source-level deny-list: even a
/// tool not on that list can incidentally echo part of its own input inside
/// a larger, otherwise-genuine result (e.g. a search tool that restates the
/// query before the findings) — subtracting caller-input spans from what
/// counts as evidence means an agent cannot "ground" a claim merely by
/// having said the same words in its own tool call. `exclude = None`
/// (no input text captured for this evidence item) behaves byte-identically
/// to [`shares_contiguous_run`] — this only tightens the check when input
/// text IS available, never loosens it.
pub fn shares_contiguous_run_excluding_echo(
    a: &str,
    b: &str,
    exclude: Option<&str>,
    min_len: usize,
) -> bool {
    if min_len == 0 {
        return true;
    }
    let a_chars: Vec<char> = a.chars().collect();
    if a_chars.len() < min_len || b.chars().count() < min_len {
        return false;
    }
    for start in 0..=(a_chars.len() - min_len) {
        let window: String = a_chars[start..start + min_len].iter().collect();
        if b.contains(&window) && !exclude.is_some_and(|ex| ex.contains(&window)) {
            return true;
        }
    }
    false
}

/// Outcome of [`check_grounded`]. Deliberately distinguishes "no evidence at
/// all" and "evidence exists but carries no result text" from "evidence
/// exists and does not overlap" — callers need the distinction to decide
/// between a fail-closed reject (evidence contradicts the claim) and a
/// fail-open skip (nothing to check against, or the check ran on incomplete
/// data — see `duduclaw-security` convention 4: quality gates must not
/// reject on missing observability).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroundingOutcome {
    /// At least one non-error evidence item (matching `tool_filter`, when
    /// set) shares a `min_overlap_chars` contiguous run with `final_text`.
    /// Carries the tool name of the first matching evidence for diagnostics.
    Grounded { tool_name: String },
    /// No non-error evidence matched `tool_filter` (or, when `tool_filter`
    /// is `None`, no non-error evidence exists at all).
    NoEvidence,
    /// Matching evidence exists, but none of it carries `result_text` — the
    /// source never captured tool output, so there is nothing to compare
    /// against. Never treat this the same as `NotGrounded`.
    ResultTextMissing,
    /// Matching evidence with `result_text` exists, but none of it shares
    /// the required contiguous run with `final_text`.
    NotGrounded,
}

/// Core grounding check: does `final_text` share a `min_overlap_chars`
/// contiguous run (CJK-safe) with at least one non-error evidence item's
/// `result_text`?
///
/// `tool_filter`:
/// - `Some(name)` restricts evidence to calls matching `name` via
///   [`tool_name_matches`] (eval's per-assertion `[[expect.grounded]]
///   tool = "..."` usage).
/// - `None` checks across *all* non-error evidence (the production
///   pre-check's usage: "was this claim backed by ANY tool result in the
///   task window", not a single named tool).
///
/// `min_overlap_chars == 0` is trivially grounded whenever matching evidence
/// with result text exists (mirrors [`shares_contiguous_run`]'s own
/// zero-length short-circuit).
pub fn check_grounded(
    final_text: &str,
    evidence: &[ToolEvidence],
    tool_filter: Option<&str>,
    min_overlap_chars: usize,
) -> GroundingOutcome {
    let candidates: Vec<&ToolEvidence> = evidence
        .iter()
        .filter(|e| !e.is_error)
        .filter(|e| tool_filter.is_none_or(|t| tool_name_matches(&e.tool_name, t)))
        .collect();
    if candidates.is_empty() {
        return GroundingOutcome::NoEvidence;
    }

    let with_results: Vec<&&ToolEvidence> = candidates
        .iter()
        .filter(|e| e.result_text.is_some())
        .collect();
    if with_results.is_empty() {
        return GroundingOutcome::ResultTextMissing;
    }

    for e in &with_results {
        let result_text = e.result_text.as_deref().unwrap_or("");
        // Fix-2 C1b: a shared span that's also present in this call's own
        // input text is a self-echo, not evidence — see
        // `shares_contiguous_run_excluding_echo`.
        if shares_contiguous_run_excluding_echo(
            final_text,
            result_text,
            e.input_text.as_deref(),
            min_overlap_chars,
        ) {
            return GroundingOutcome::Grounded {
                tool_name: e.tool_name.clone(),
            };
        }
    }
    GroundingOutcome::NotGrounded
}

/// The `result_text` of every non-error evidence item matching `tool_filter`
/// (same semantics as [`check_grounded`]'s filter), in evidence order. Used
/// by callers that need to run a secondary check (e.g. eval's
/// `output_regex` fragment match) over the same candidate set without
/// re-implementing the filter.
pub fn matching_result_texts<'a>(
    evidence: &'a [ToolEvidence],
    tool_filter: Option<&str>,
) -> Vec<&'a str> {
    evidence
        .iter()
        .filter(|e| !e.is_error)
        .filter(|e| tool_filter.is_none_or(|t| tool_name_matches(&e.tool_name, t)))
        .filter_map(|e| e.result_text.as_deref())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(tool: &str, result_text: Option<&str>, is_error: bool) -> ToolEvidence {
        ToolEvidence {
            tool_name: tool.to_string(),
            result_text: result_text.map(String::from),
            input_text: None,
            is_error,
        }
    }

    fn ev_with_input(
        tool: &str,
        result_text: Option<&str>,
        input_text: Option<&str>,
        is_error: bool,
    ) -> ToolEvidence {
        ToolEvidence {
            tool_name: tool.to_string(),
            result_text: result_text.map(String::from),
            input_text: input_text.map(String::from),
            is_error,
        }
    }

    #[test]
    fn tool_matching_is_token_anchored() {
        assert!(tool_name_matches("Bash", "Bash"));
        assert!(tool_name_matches(
            "mcp__duduclaw__tasks_create",
            "tasks_create"
        ));
        assert!(!tool_name_matches("mcp__duduclaw__tasks_create", "create"));
        assert!(!tool_name_matches("BashOutput", "Bash"));
    }

    #[test]
    fn shares_contiguous_run_is_cjk_safe_and_order_sensitive() {
        assert!(shares_contiguous_run(
            "order #1234 confirmed",
            "we confirmed order #1234 today",
            11
        ));
        assert!(shares_contiguous_run(
            "退款政策：三十天內可退款",
            "查詢結果：退款政策：三十天內可退款。",
            8
        ));
        assert!(!shares_contiguous_run("abcdefgh", "zzzzzzzz", 4));
        assert!(!shares_contiguous_run("ab", "abcdef", 5));
    }

    #[test]
    fn check_grounded_passes_on_overlap() {
        let evidence = vec![ev(
            "mcp__duduclaw__memory_search",
            Some("Refund policy: 30 days from purchase, receipt required."),
            false,
        )];
        let outcome = check_grounded(
            "Refund policy: 30 days from purchase, receipt required.",
            &evidence,
            Some("memory_search"),
            12,
        );
        assert_eq!(
            outcome,
            GroundingOutcome::Grounded {
                tool_name: "mcp__duduclaw__memory_search".to_string()
            }
        );
    }

    #[test]
    fn check_grounded_no_evidence_when_tool_never_called() {
        let outcome = check_grounded("some answer", &[], Some("memory_search"), 12);
        assert_eq!(outcome, GroundingOutcome::NoEvidence);
    }

    #[test]
    fn check_grounded_no_evidence_when_only_error_calls_exist() {
        let evidence = vec![ev("memory_search", Some("boom"), true)];
        let outcome = check_grounded("some answer", &evidence, Some("memory_search"), 4);
        assert_eq!(outcome, GroundingOutcome::NoEvidence);
    }

    #[test]
    fn check_grounded_result_text_missing_is_distinct_from_not_grounded() {
        let evidence = vec![ev("memory_search", None, false)];
        let outcome = check_grounded("some answer", &evidence, Some("memory_search"), 4);
        assert_eq!(outcome, GroundingOutcome::ResultTextMissing);
    }

    #[test]
    fn check_grounded_not_grounded_when_no_overlap() {
        let evidence = vec![ev(
            "memory_search",
            Some("Refund policy: 30 days from purchase."),
            false,
        )];
        let outcome = check_grounded(
            "I handled the request successfully.",
            &evidence,
            Some("memory_search"),
            12,
        );
        assert_eq!(outcome, GroundingOutcome::NotGrounded);
    }

    #[test]
    fn check_grounded_tool_filter_none_checks_all_evidence() {
        let evidence = vec![
            ev("Bash", Some("irrelevant output"), false),
            ev(
                "mcp__duduclaw__tasks_create",
                Some("task created: refund #1234 approved"),
                false,
            ),
        ];
        let outcome = check_grounded(
            "Result: refund #1234 approved for the customer.",
            &evidence,
            None,
            12,
        );
        assert_eq!(
            outcome,
            GroundingOutcome::Grounded {
                tool_name: "mcp__duduclaw__tasks_create".to_string()
            }
        );
    }

    #[test]
    fn matching_result_texts_filters_by_tool_and_error() {
        let evidence = vec![
            ev("memory_search", Some("a"), false),
            ev("memory_search", Some("b"), true), // error, excluded
            ev("Bash", Some("c"), false),         // wrong tool, excluded
        ];
        let texts = matching_result_texts(&evidence, Some("memory_search"));
        assert_eq!(texts, vec!["a"]);
    }

    // ── Fix-2 C1: self-echo grounding ────────────────────────────────────

    #[test]
    fn self_echo_tool_names_match_with_mcp_prefix() {
        assert!(is_self_echo_tool("tasks_complete"));
        assert!(is_self_echo_tool("mcp__duduclaw__tasks_complete"));
        assert!(is_self_echo_tool("activity_post"));
        assert!(!is_self_echo_tool("memory_search"));
        assert!(!is_self_echo_tool("tasks_list")); // read-only, not on the list
    }

    #[test]
    fn shares_contiguous_run_excluding_echo_disqualifies_input_only_overlap() {
        // The agent's final claim IS the caller's own input verbatim, and
        // the tool "result" merely wraps that same input — self-echo, not
        // evidence. (Final text kept identical to the excluded input so
        // every candidate window is provably inside the excluded span —
        // an earlier version of this test picked strings whose windows
        // could straddle unrelated context and produce a stray non-echoed
        // match, which is a test-construction bug, not a function bug.)
        assert!(!shares_contiguous_run_excluding_echo(
            "refund for order #1234",
            "task completed: refund for order #1234",
            Some("refund for order #1234"),
            10,
        ));
    }

    #[test]
    fn shares_contiguous_run_excluding_echo_still_passes_on_genuine_evidence() {
        // Overlap exists and is NOT present in the input — genuine evidence,
        // still counts.
        assert!(shares_contiguous_run_excluding_echo(
            "Refund policy: 30 days from purchase, receipt required.",
            "Refund policy: 30 days from purchase, receipt required.",
            Some("policy lookup query"),
            12,
        ));
    }

    #[test]
    fn shares_contiguous_run_excluding_echo_none_exclude_matches_plain_variant() {
        // No input text captured for this evidence item ⇒ identical to
        // `shares_contiguous_run` — never a stricter default than before.
        assert_eq!(
            shares_contiguous_run_excluding_echo("abcdef", "xxabcdefyy", None, 4),
            shares_contiguous_run("abcdef", "xxabcdefyy", 4),
        );
    }

    #[test]
    fn check_grounded_rejects_pure_self_echo_evidence() {
        // Simulates a `tasks_complete` call where `result_text` is (nearly)
        // identical to the caller's own `summary` argument (`input_text`):
        // no independent evidence, must NOT be Grounded.
        let evidence = vec![ev_with_input(
            "tasks_complete",
            Some("Completed: refund #1234 processed for the customer"),
            Some("refund #1234 processed for the customer"),
            false,
        )];
        let outcome = check_grounded(
            "refund #1234 processed for the customer",
            &evidence,
            None,
            12,
        );
        assert_eq!(outcome, GroundingOutcome::NotGrounded);
    }

    #[test]
    fn check_grounded_still_passes_when_result_carries_genuine_new_info() {
        // Same shape, but this time the result text also contains something
        // the caller's input never had (an id assigned by the store) — the
        // overlap on THAT span is genuine evidence.
        let evidence = vec![ev_with_input(
            "tasks_create",
            Some("task created with id task-abc999-independent-store-id"),
            Some("create a follow-up task"),
            false,
        )];
        let outcome = check_grounded(
            "Created it: task-abc999-independent-store-id",
            &evidence,
            None,
            12,
        );
        assert_eq!(
            outcome,
            GroundingOutcome::Grounded {
                tool_name: "tasks_create".to_string()
            }
        );
    }
}
