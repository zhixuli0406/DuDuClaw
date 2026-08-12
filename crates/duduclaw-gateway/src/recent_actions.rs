//! Recent self-action feed — cross-invocation action continuity.
//!
//! Different invocations of the same agent (cron / heartbeat / goal-loop vs.
//! channel replies) run in separate sessions and cannot see each other's
//! actions. Asked "did you do X?", an agent consulting only live tool state
//! will flatly deny actions its own audit trail records — blocked orders,
//! rejected calls and failures are invisible to live state. This module
//! compresses the agent's most recent state-changing tool calls from
//! `tool_calls.jsonl` (programmatic evidence, `resolve_audit_agent`
//! attribution across all trigger sources) into a short prompt section
//! injected into both the channel-reply and the dispatch/cron/heartbeat
//! prompt paths.
//!
//! Fail-open by design: missing file, unreadable lines or a broken config
//! all degrade to "no section" — never to an error.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use duduclaw_core::truncate_bytes;

/// Tail window read from `tool_calls.jsonl` — bounded so channel replies
/// never pay for a whole-file scan (the file rotates at 16MB).
const TAIL_READ_BYTES: u64 = 256 * 1024;

/// Only actions younger than this appear in the feed.
const LOOKBACK_HOURS: i64 = 24;

/// Per-line byte cap for the params summary.
const LINE_SUMMARY_MAX_BYTES: usize = 200;

/// Whole-section byte cap (lines only, header excluded).
const SECTION_LINES_MAX_BYTES: usize = 2400;

/// Hard ceiling for the configurable entry count.
const MAX_COUNT: usize = 50;

/// `config.toml [memory] recent_actions_enabled / recent_actions_count`.
#[derive(Debug, Clone)]
pub struct RecentActionsConfig {
    pub enabled: bool,
    pub count: usize,
}

impl Default for RecentActionsConfig {
    fn default() -> Self {
        Self { enabled: true, count: 10 }
    }
}

impl RecentActionsConfig {
    /// Isolation parsing: any failure (missing file, bad TOML, absent keys)
    /// returns the default — mirrors `GroundingPrecheckConfig::from_home`.
    pub fn from_home(home_dir: &Path) -> Self {
        let default = Self::default();
        let path = home_dir.join("config.toml");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return default;
        };
        let Ok(table) = raw.parse::<toml::Table>() else {
            return default;
        };
        let Some(section) = table.get("memory").and_then(|v| v.as_table()) else {
            return default;
        };
        let enabled = section
            .get("recent_actions_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(default.enabled);
        let count = section
            .get("recent_actions_count")
            .and_then(|v| v.as_integer())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n > 0)
            .map(|n| n.min(MAX_COUNT))
            .unwrap_or(default.count);
        Self { enabled, count }
    }
}

/// One audit-log row projected to what the feed needs.
#[derive(Debug, Clone, PartialEq)]
struct ActionRow {
    timestamp: String,
    tool_name: String,
    success: bool,
    summary: String,
}

/// Read the tail of `tool_calls.jsonl` and return this agent's rows within
/// the lookback window, oldest first, capped to the most recent `max`.
fn read_recent_rows(home_dir: &Path, agent_id: &str, max: usize) -> Vec<ActionRow> {
    let path = home_dir.join("tool_calls.jsonl");
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&raw);
    // A mid-file seek lands inside a line — drop the first partial line.
    let text = if start > 0 {
        match text.find('\n') {
            Some(pos) => &text[pos + 1..],
            None => return Vec::new(),
        }
    } else {
        &text[..]
    };

    // Same-format RFC3339 UTC strings compare lexicographically at second
    // granularity, which is all a 24h cutoff needs.
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(LOOKBACK_HOURS)).to_rfc3339();

    let mut rows: Vec<ActionRow> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("agent_id").and_then(|x| x.as_str()) != Some(agent_id) {
            continue;
        }
        let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) else {
            continue;
        };
        if ts < cutoff.as_str() {
            continue;
        }
        let Some(tool_name) = v.get("tool_name").and_then(|x| x.as_str()) else {
            continue;
        };
        let success = v.get("success").and_then(|x| x.as_bool()).unwrap_or(true);
        let summary = v
            .get("params_summary")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        rows.push(ActionRow {
            timestamp: ts.to_string(),
            tool_name: tool_name.to_string(),
            success,
            summary,
        });
    }
    if rows.len() > max {
        rows.drain(..rows.len() - max);
    }
    rows
}

/// Local-time display for a row timestamp ("MM-DD HH:MM"); falls back to a
/// raw prefix when the timestamp doesn't parse.
fn display_time(ts: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string(),
        Err(_) => truncate_bytes(ts, 16).to_string(),
    }
}

/// Render rows as feed lines, collapsing consecutive identical
/// (tool, summary, success) repeats into one line with a repeat count.
fn render_lines(rows: &[ActionRow]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let row = &rows[i];
        let mut repeat = 1;
        while i + repeat < rows.len() {
            let next = &rows[i + repeat];
            if next.tool_name == row.tool_name
                && next.summary == row.summary
                && next.success == row.success
            {
                repeat += 1;
            } else {
                break;
            }
        }
        let mark = if row.success { "✅" } else { "❌" };
        let summary = truncate_bytes(&row.summary, LINE_SUMMARY_MAX_BYTES);
        let mut line = if summary.is_empty() {
            format!("- [{}] {} {mark}", display_time(&row.timestamp), row.tool_name)
        } else {
            format!(
                "- [{}] {} {mark} {summary}",
                display_time(&row.timestamp),
                row.tool_name
            )
        };
        if repeat > 1 {
            line.push_str(&format!("（連續 ×{repeat}）"));
        }
        lines.push(line);
        i += repeat;
    }
    lines
}

/// Build the injectable section, or `None` when disabled / no recent
/// actions (an empty feed must produce zero prompt noise).
pub fn build_recent_actions_section(home_dir: &Path, agent_id: &str) -> Option<String> {
    let cfg = RecentActionsConfig::from_home(home_dir);
    if !cfg.enabled || agent_id.is_empty() {
        return None;
    }
    let rows = read_recent_rows(home_dir, agent_id, cfg.count);
    if rows.is_empty() {
        return None;
    }
    let mut lines = render_lines(&rows);

    // Trim oldest-first until the byte budget holds.
    let mut total: usize = lines.iter().map(|l| l.len() + 1).sum();
    while total > SECTION_LINES_MAX_BYTES && lines.len() > 1 {
        let removed = lines.remove(0);
        total -= removed.len() + 1;
    }

    Some(format!(
        "## 近期自身行動（稽核紀錄）\n\
         以下是你近 24 小時內實際執行過的工具呼叫，來自稽核日誌，涵蓋排程、心跳、目標迴圈、頻道對話等所有觸發來源。\
         回答「你是否做過某件事」時，必須以本紀錄與其他耐久紀錄（稽核日誌、任務 activity、專案日誌）為準；\
         即時工具查詢看不到被攔截、被拒絕或失敗的行動，不可作為唯一依據。\n{}",
        lines.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_log(dir: &Path, lines: &[String]) {
        let mut f = std::fs::File::create(dir.join("tool_calls.jsonl")).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn row_json(ts: &str, agent: &str, tool: &str, success: bool, summary: &str) -> String {
        serde_json::json!({
            "timestamp": ts,
            "agent_id": agent,
            "tool_name": tool,
            "success": success,
            "params_summary": summary,
        })
        .to_string()
    }

    fn recent_ts(mins_ago: i64) -> String {
        (chrono::Utc::now() - chrono::Duration::minutes(mins_ago)).to_rfc3339()
    }

    #[test]
    fn no_file_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(build_recent_actions_section(dir.path(), "trader").is_none());
    }

    #[test]
    fn empty_feed_yields_none_and_other_agents_are_excluded() {
        let dir = tempfile::tempdir().unwrap();
        write_log(
            dir.path(),
            &[row_json(&recent_ts(5), "other-agent", "tasks_create", true, "x")],
        );
        assert!(build_recent_actions_section(dir.path(), "trader").is_none());
    }

    #[test]
    fn recent_own_actions_are_listed_including_failures() {
        let dir = tempfile::tempdir().unwrap();
        write_log(
            dir.path(),
            &[
                row_json(&recent_ts(10), "trader", "execute_trade", false, "2317 8股 @264（被風控攔截）"),
                row_json(&recent_ts(5), "trader", "tasks_complete", true, "盤前計畫完成"),
            ],
        );
        let section = build_recent_actions_section(dir.path(), "trader").unwrap();
        assert!(section.contains("execute_trade"));
        assert!(section.contains("❌"));
        assert!(section.contains("被風控攔截"));
        assert!(section.contains("tasks_complete"));
        // Blocked/failed actions are precisely what live tool state misses.
        assert!(section.contains("近期自身行動"));
    }

    #[test]
    fn stale_rows_outside_lookback_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        write_log(dir.path(), &[row_json(&old, "trader", "execute_trade", true, "old")]);
        assert!(build_recent_actions_section(dir.path(), "trader").is_none());
    }

    #[test]
    fn consecutive_duplicates_collapse() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (0..3)
            .map(|i| row_json(&recent_ts(10 - i), "trader", "web_fetch_cached", true, "poll"))
            .collect();
        write_log(dir.path(), &lines);
        let section = build_recent_actions_section(dir.path(), "trader").unwrap();
        assert!(section.contains("（連續 ×3）"));
        assert_eq!(section.matches("web_fetch_cached").count(), 1);
    }

    #[test]
    fn count_cap_keeps_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let lines: Vec<String> = (0..20)
            .map(|i| row_json(&recent_ts(60 - i), "trader", "tool", true, &format!("call-{i}")))
            .collect();
        write_log(dir.path(), &lines);
        let section = build_recent_actions_section(dir.path(), "trader").unwrap();
        // Default count is 10 — the oldest calls must be gone.
        assert!(!section.contains("call-0 "));
        assert!(section.contains("call-19"));
    }

    #[test]
    fn config_gate_disables_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[memory]\nrecent_actions_enabled = false\n",
        )
        .unwrap();
        write_log(dir.path(), &[row_json(&recent_ts(5), "trader", "tool", true, "x")]);
        assert!(build_recent_actions_section(dir.path(), "trader").is_none());
    }

    #[test]
    fn cjk_summary_truncates_on_char_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let long = "鴻海台積電聯發科".repeat(30);
        write_log(dir.path(), &[row_json(&recent_ts(5), "trader", "tool", true, &long)]);
        // Must not panic on a mid-char byte budget.
        let section = build_recent_actions_section(dir.path(), "trader").unwrap();
        assert!(section.contains("鴻海"));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write_log(
            dir.path(),
            &[
                "not-json".to_string(),
                row_json(&recent_ts(5), "trader", "tool", true, "ok"),
            ],
        );
        let section = build_recent_actions_section(dir.path(), "trader").unwrap();
        assert!(section.contains("ok"));
    }
}
