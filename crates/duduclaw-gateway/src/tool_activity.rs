//! WP-A3: shared `tool_calls.jsonl` record shape + window filter.
//!
//! Extracted from `dispatch_engine.rs` (previously `ToolActivityRecord` /
//! `filter_tool_activity` lived there, private to that module) so the A3
//! task-forward-model observation layer (`prediction::task_observe`) can
//! read the exact same evidence shape without a second, drifting
//! reimplementation. Behavior is byte-identical to the pre-extraction
//! version — this is a pure code-motion refactor, `dispatch_engine`'s own
//! tests (which exercise `filter_tool_activity` directly) are unchanged.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §5.3.

/// One `tool_calls.jsonl` line's fields relevant to the review window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ToolActivityRecord {
    pub(crate) tool_name: String,
    pub(crate) success: bool,
    /// The tool's captured result/output text, when the writer recorded
    /// one. Populated by `duduclaw_security::audit::append_tool_call_with_input`
    /// (the MCP dispatch call site, `duduclaw-cli/src/mcp.rs`, B3b) for every
    /// state-changing tool call EXCEPT the ones on
    /// `duduclaw_core::grounding::SELF_ECHO_TOOL_NAMES` (Fix-2 C1a,
    /// 2026-08) — those tools' response envelope is substantially the
    /// caller's own input echoed back, so capturing it as `result_text`
    /// would let a claim "ground" against its own words. `None` covers both
    /// the deliberate C1a suppression and the ordinary case of a writer
    /// that never captured output for this call — both degrade the B3
    /// grounding pre-check to `ResultTextMissing`/skip, never a reject.
    pub(crate) result_text: Option<String>,
    /// The tool CALL's own masked input text, when the writer recorded one
    /// (Fix-2 C1b). Used to subtract self-echoed spans from `result_text`
    /// before comparing against the agent's final claim — see
    /// `duduclaw_core::grounding::shares_contiguous_run_excluding_echo`.
    pub(crate) input_text: Option<String>,
}

/// Filter raw `tool_calls.jsonl` content (one JSON object per line, written
/// by `duduclaw_security::audit::append_tool_call*`) down to the records for
/// `agent_id` whose `timestamp` falls in `[since, until]` inclusive.
/// Malformed lines, other agents, and out-of-window records are silently
/// dropped — this is a best-effort evidence summary, not a second audit
/// trail (the canonical trail is the file itself). A bad `since`/`until`
/// bound (should never happen — both are RFC3339 stamps from the task
/// store / `Utc::now()`) yields an empty result rather than panicking.
pub(crate) fn filter_tool_activity(
    jsonl: &str,
    agent_id: &str,
    since: &str,
    until: &str,
) -> Vec<ToolActivityRecord> {
    let (Ok(since_dt), Ok(until_dt)) = (
        chrono::DateTime::parse_from_rfc3339(since),
        chrono::DateTime::parse_from_rfc3339(until),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("agent_id").and_then(|a| a.as_str()) != Some(agent_id) {
            continue;
        }
        let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()) else {
            continue;
        };
        let Ok(ts_dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
            continue;
        };
        if ts_dt < since_dt || ts_dt > until_dt {
            continue;
        }
        let Some(tool_name) = v.get("tool_name").and_then(|t| t.as_str()) else {
            continue;
        };
        let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
        let result_text = v
            .get("result_text")
            .and_then(|s| s.as_str())
            .map(String::from);
        let input_text = v.get("input").and_then(|s| s.as_str()).map(String::from);
        out.push(ToolActivityRecord {
            tool_name: tool_name.to_string(),
            success,
            result_text,
            input_text,
        });
    }
    out
}

/// Read and filter `tool_calls.jsonl` under `home_dir` for one task's
/// claim→review window. Missing/unreadable/unparseable audit file ⇒ empty
/// (never fails the caller over an observability gap). Shared by the
/// `dispatch_engine` `<tool_activity>` judge-evidence block and the A3
/// task-forward-model observation layer — both read the exact same window
/// shape.
pub(crate) fn read_tool_activity_records(
    home_dir: &std::path::Path,
    agent_id: &str,
    since: &str,
    until: &str,
) -> Vec<ToolActivityRecord> {
    let path = home_dir.join("tool_calls.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    filter_tool_activity(&raw, agent_id, since, until)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_tool_activity_scopes_to_agent_and_window() {
        let jsonl = concat!(
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":true}\n",
            "{\"timestamp\":\"2026-07-11T10:03:00Z\",\"agent_id\":\"w\",\"tool_name\":\"memory_search\",\"success\":false}\n",
            "{\"timestamp\":\"2026-07-11T10:02:30Z\",\"agent_id\":\"other\",\"tool_name\":\"Bash\",\"success\":true}\n",
            "{\"timestamp\":\"2026-07-11T09:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Bash\",\"success\":true}\n",
            "{\"timestamp\":\"2026-07-11T12:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Bash\",\"success\":true}\n",
            "not json\n",
            "{\"agent_id\":\"w\"}\n",
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
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn filter_tool_activity_bad_bounds_yields_empty_not_panic() {
        let jsonl = "{\"timestamp\":\"2026-07-11T10:00:00Z\",\"agent_id\":\"w\",\"tool_name\":\"Read\",\"success\":true}\n";
        assert!(filter_tool_activity(jsonl, "w", "not-a-date", "also-not-a-date").is_empty());
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
    }

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

    #[test]
    fn read_tool_activity_records_captures_optional_input_text() {
        // Fix-2 C1b: the `input` field the audit writer already captures for
        // every state-changing call must also flow into `ToolActivityRecord`
        // so `dispatch_engine::grounding_precheck` can subtract self-echoed
        // spans from `result_text`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            "{\"timestamp\":\"2026-07-11T10:02:00Z\",\"agent_id\":\"w\",\"tool_name\":\"tasks_create\",\"success\":true,\"input\":\"{\\\"title\\\":\\\"x\\\"}\"}\n",
        )
        .unwrap();
        let records = read_tool_activity_records(
            dir.path(),
            "w",
            "2026-07-11T10:00:00Z",
            "2026-07-11T10:05:00Z",
        );
        assert_eq!(records[0].input_text.as_deref(), Some("{\"title\":\"x\"}"));
    }
}
