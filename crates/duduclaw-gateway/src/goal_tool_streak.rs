//! Tool-call streak advisory for the goal loop — **H10**
//! (`research/harness-2026-08/deepseek-harness.md` §2.16
//! `repeat-tool-reminder`, alongside grok-build's §2.2 "action
//! stationarity" framing).
//!
//! ## What this catches that A2 (`goal_visit_graph.rs`) doesn't
//!
//! A2 flags a round that repeats a PRIOR round's whole `(state, action)`
//! pair — it needs at least two full dispatch/judge cycles to notice.
//! This module looks INSIDE a single round's tool activity: an agent that
//! calls the exact same tool with the exact same (masked) arguments three,
//! five, eight times in a row within one round is stuck well before the
//! judge ever sees a result. Zero LLM cost, purely advisory — dsh's own
//! framing is "the decision stays with the model", so this only ever
//! injects a text hint into the NEXT dispatch round's `<state>` block
//! (`goal_state::StateBlock::tool_streak_hint`); it never blocks, retries,
//! or vetoes a dispatch.
//!
//! ## Signal source
//!
//! Reuses [`crate::tool_activity::read_tool_activity_records`] — the same
//! `tool_calls.jsonl` tail-scoped-by-`(agent_id, since, until)` reader every
//! other goal-loop evidence consumer (`goal_visit_graph::action_digest`,
//! the A3 forward model) already shares, so there is no second audit-log
//! parser. Records come back in file order, which is chronological (the
//! audit log is append-only — see `duduclaw_security::audit`).
//!
//! ## Normalized parameter signature
//!
//! `ToolActivityRecord::input_text` is the audit writer's own MASKED input
//! capture (secrets already redacted upstream). This module folds it
//! through [`crate::goal_state::short_hash`] — the same CJK-safe
//! NFKC-normalize + whitespace-collapse + SHA-256-prefix primitive the A2
//! visit graph's `state_hash` already uses — so two calls whose masked
//! input differs only by incidental whitespace/fullwidth-punctuation still
//! count as "the same call", while a genuinely different argument breaks
//! the streak. A call with no captured input text (writer never recorded
//! one) normalizes to the empty string, which is still a stable, comparable
//! signature — repeated argument-less calls to the same tool (e.g. a
//! polling tool with no params) legitimately form a streak too.
//!
//! ## Threshold ladder
//!
//! `[3, 5, 8]` — identical to dsh's `repeat-tool-reminder`. Each tier's text
//! is stricter than the last but always advisory: 3 asks the agent to
//! re-read its last result before calling again, 5 suggests changing
//! approach, 8 strongly suggests converging or asking for human help via
//! `tasks_block`.

use std::path::Path;

use crate::goal_state::short_hash;
use crate::tool_activity::{read_tool_activity_records, ToolActivityRecord};

/// Escalating advisory thresholds — mirrors dsh's `[3, 5, 8]` ladder.
pub const THRESHOLDS: [u32; 3] = [3, 5, 8];

/// One round's longest same-tool/same-params streak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreakHit {
    pub tool_name: String,
    pub len: u32,
}

/// Longest run of consecutive calls sharing the same `(tool_name,
/// normalized_input_signature)` pair, scanning `records` in order (assumed
/// chronological — true for anything sourced from the append-only
/// `tool_calls.jsonl`). Ties are broken in favor of the LATEST run: the most
/// recent repeated behavior is the one most relevant to advise on for the
/// NEXT round, not necessarily the first one that happened to be longest.
///
/// Returns `None` for an empty input (nothing to report — never fabricates
/// a streak of length 0).
pub fn longest_streak(records: &[ToolActivityRecord]) -> Option<StreakHit> {
    let mut best: Option<StreakHit> = None;

    let mut current_tool: Option<&str> = None;
    let mut current_sig: Option<String> = None;
    let mut current_len: u32 = 0;

    for record in records {
        let sig = short_hash(record.input_text.as_deref().unwrap_or("").trim());
        let same_as_current =
            current_tool == Some(record.tool_name.as_str()) && current_sig.as_deref() == Some(sig.as_str());
        if same_as_current {
            current_len += 1;
        } else {
            current_tool = Some(&record.tool_name);
            current_sig = Some(sig);
            current_len = 1;
        }
        let ge_best = best.as_ref().map(|b| current_len >= b.len).unwrap_or(true);
        if ge_best {
            best = Some(StreakHit { tool_name: record.tool_name.clone(), len: current_len });
        }
    }

    best
}

/// Escalating zh-TW advisory text for one streak hit, or `None` when the
/// streak has not yet crossed the lowest threshold (3) — advisory-only, no
/// injection at all below that bar.
pub fn advisory_text(hit: &StreakHit) -> Option<String> {
    // Highest threshold reached, not exact match — a streak of 6 is still
    // "past 5", not "waiting for 8".
    let tier = THRESHOLDS.iter().copied().filter(|&t| hit.len >= t).max()?;
    let text = match tier {
        3 => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,建議先重讀上一次的執行結果,\
             確認是否已經取得所需資訊,避免重複呼叫浪費輪次。",
            hit.len, hit.tool_name
        ),
        5 => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,目前的做法可能沒有進展,\
             建議換一個方法或角度切入,而不是繼續重複同樣的呼叫。",
            hit.len, hit.tool_name
        ),
        _ => format!(
            "已連續 {} 次呼叫同一工具「{}」且參數相同,強烈建議停止重複嘗試:\
             直接收斂目前已取得的結果回報,或改用 tasks_block 說明受阻原因並求助。",
            hit.len, hit.tool_name
        ),
    };
    Some(text)
}

/// Read `tool_calls.jsonl` for `agent_id` in `[since, until]` and compute
/// this round's longest same-tool/same-params streak. Missing/unreadable/
/// unparseable audit file ⇒ `None` (never fails the caller over an
/// observability gap — same fail-open contract as
/// `tool_activity::read_tool_activity_records` and
/// `goal_visit_graph::action_digest`).
pub fn detect_tool_streak(home_dir: &Path, agent_id: &str, since: &str, until: &str) -> Option<StreakHit> {
    let records = read_tool_activity_records(home_dir, agent_id, since, until);
    longest_streak(&records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tool: &str, input: Option<&str>) -> ToolActivityRecord {
        ToolActivityRecord {
            tool_name: tool.to_string(),
            success: true,
            result_text: None,
            input_text: input.map(String::from),
        }
    }

    // ── longest_streak: pure computation ─────────────────────

    #[test]
    fn empty_input_yields_none() {
        assert!(longest_streak(&[]).is_none());
    }

    #[test]
    fn single_call_is_a_streak_of_one() {
        let hit = longest_streak(&[rec("bash", Some("ls"))]).unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 1);
    }

    #[test]
    fn same_tool_same_params_consecutive_streak() {
        let records = vec![
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "web_fetch");
        assert_eq!(hit.len, 4);
    }

    #[test]
    fn different_params_interrupt_the_streak() {
        let records = vec![
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
            // Different params — breaks the run even though the tool name matches.
            rec("web_fetch", Some("{\"url\":\"https://y.com\"}")),
            rec("web_fetch", Some("{\"url\":\"https://x.com\"}")),
        ];
        let hit = longest_streak(&records).unwrap();
        // Longest run anywhere is 2 (the first two), not 4.
        assert_eq!(hit.len, 2);
    }

    #[test]
    fn different_tool_interrupts_the_streak_even_with_same_params() {
        let records = vec![
            rec("bash", Some("ls")),
            rec("bash", Some("ls")),
            rec("bash", Some("ls")),
            rec("Read", Some("ls")), // same "params" text, different tool
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 3);
    }

    #[test]
    fn later_run_wins_the_tie() {
        let records = vec![
            rec("bash", Some("a")),
            rec("bash", Some("a")),
            rec("bash", Some("a")), // run of 3
            rec("Read", Some("b")),
            rec("web_fetch", Some("c")),
            rec("web_fetch", Some("c")),
            rec("web_fetch", Some("c")), // also a run of 3, later
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "web_fetch", "on a tie, the most recent run is the more actionable signal");
        assert_eq!(hit.len, 3);
    }

    #[test]
    fn masked_params_stable_compare_ignores_incidental_whitespace() {
        // Same masked input modulo whitespace/formatting must still count as
        // "the same call" — mirrors goal_state::short_hash's own contract
        // (NFKC-normalize + collapse whitespace), which this module reuses
        // rather than re-deriving.
        let records = vec![
            rec("bash", Some("ls  -la")),
            rec("bash", Some("ls -la")),
            rec("bash", Some("  ls -la  ")),
        ];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.len, 3, "whitespace-only differences must not break the streak");
    }

    #[test]
    fn missing_input_text_still_forms_a_stable_streak() {
        // A tool the audit writer never captured input for (`None`)
        // normalizes to the empty-string signature — repeated calls with no
        // captured params still legitimately stream together.
        let records = vec![rec("poll_tool", None), rec("poll_tool", None), rec("poll_tool", None)];
        let hit = longest_streak(&records).unwrap();
        assert_eq!(hit.tool_name, "poll_tool");
        assert_eq!(hit.len, 3);
    }

    // ── advisory_text: threshold ladder ───────────────────────

    #[test]
    fn below_lowest_threshold_yields_no_advisory() {
        assert!(advisory_text(&StreakHit { tool_name: "bash".into(), len: 2 }).is_none());
    }

    #[test]
    fn tier_3_text_suggests_rereading_the_result() {
        let text = advisory_text(&StreakHit { tool_name: "bash".into(), len: 3 }).unwrap();
        assert!(text.contains("3"));
        assert!(text.contains("bash"));
        assert!(text.contains("重讀"), "tier 3 must nudge toward re-reading the last result");
    }

    #[test]
    fn tier_4_still_reads_as_tier_3_not_the_next_rung() {
        // 4 has crossed 3 but not yet 5 — must render the tier-3 text, not
        // silently jump ahead or fall back to nothing.
        let text = advisory_text(&StreakHit { tool_name: "bash".into(), len: 4 }).unwrap();
        assert!(text.contains("重讀"));
    }

    #[test]
    fn tier_5_text_suggests_changing_approach() {
        let text = advisory_text(&StreakHit { tool_name: "bash".into(), len: 5 }).unwrap();
        assert!(text.contains("換一個方法") || text.contains("換個方法"));
    }

    #[test]
    fn tier_8_text_strongly_suggests_converging_or_asking_for_help() {
        let text = advisory_text(&StreakHit { tool_name: "bash".into(), len: 8 }).unwrap();
        assert!(text.contains("tasks_block"));
        assert!(text.contains("強烈建議"));
    }

    #[test]
    fn tier_beyond_8_still_renders_the_tier_8_text() {
        let text = advisory_text(&StreakHit { tool_name: "bash".into(), len: 20 }).unwrap();
        assert!(text.contains("tasks_block"));
    }

    // ── detect_tool_streak: filesystem-backed integration ─────

    #[test]
    fn detect_tool_streak_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(detect_tool_streak(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z").is_none());
    }

    #[test]
    fn detect_tool_streak_scopes_to_agent_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = format!(
            "{}\n{}\n{}\n{}\n",
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:10:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:11:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            serde_json::json!({"agent_id": "alice", "timestamp": "2026-01-01T00:12:00Z", "tool_name": "bash", "success": true, "input": "ls"}),
            // Different agent — must not count toward alice's streak.
            serde_json::json!({"agent_id": "bob", "timestamp": "2026-01-01T00:12:30Z", "tool_name": "bash", "success": true, "input": "ls"}),
        );
        std::fs::write(dir.path().join("tool_calls.jsonl"), &jsonl).unwrap();
        let hit =
            detect_tool_streak(dir.path(), "alice", "2026-01-01T00:00:00Z", "2026-01-01T01:00:00Z").unwrap();
        assert_eq!(hit.tool_name, "bash");
        assert_eq!(hit.len, 3);
    }
}
