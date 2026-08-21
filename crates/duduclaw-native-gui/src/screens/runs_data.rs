// WP-S5b2-F — data model + pure parsing/formatting for the "執行紀錄" page
// (`screens/runs.rs`). Split out for the same file-size reason
// `files_data.rs`/`goals_data.rs`/`tasks_data.rs` are split from their
// sibling UI files.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ────────────────────────────────────────────
//   `runs.list` (dispatch ~L6601, handler `handle_runs_list` ~L30545,
//   `check_agent_filter!(AccessLevel::Viewer)` — non-admin callers MUST
//   pass `agent_id` or the call fails closed with "agent_id parameter is
//   required") → `{"runs": [{"id","session_id","agent_id","channel",
//   "started_at","ended_at","status","step_count","preview"}]}`, newest
//   `started_at` first. `status` is one of exactly THREE real values —
//   `"completed"|"running"|"no_reply"` (`web/src/lib/run-transcript.ts`'s
//   own `RunStatus` union; the gateway never emits a fourth) — see
//   `screens::runs`'s module doc comment for why the design canvas's
//   amber/red "需要人工"/"失敗" dots are honestly dropped in favor of these
//   three.
//   `runs.get` (dispatch ~L6608, handler `handle_runs_get` ~L30646, gated
//   by the run's OWN owning agent — Viewer — resolved server-side, fails
//   closed for an unknown run) → `{"run": {...same shape minus step_count/
//   preview...}, "events": [{"kind","role"?,"tool"?,"ok"?,"label"?,"ts",
//   "preview"}], "event_sources": {...}, "not_persisted": [...]}`. Event
//   kinds: `"text"` (role user/assistant), `"tool_use"` (real MCP receipt,
//   carries `ok`), `"tool_step"` (CLI-native boundary, NO `ok` — never
//   invented, mirrors `web/src/lib/run-transcript.ts`'s own comment),
//   `"todo_update"` (label is literally `"<done>/<total>"` — the ONE piece
//   of real data behind the canvas's "工作清單快照：完成 2/3" element, see
//   `last_todo_snapshot` below).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub agent_id: String,
    pub channel: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub step_count: u64,
    pub preview: String,
}

pub fn parse_runs_list(v: &Value) -> Vec<RunSummary> {
    v.get("runs")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let id = r.get("id")?.as_str()?.to_string();
                    let agent_id = r.get("agent_id").and_then(Value::as_str).unwrap_or("").to_string();
                    let channel = r.get("channel").and_then(Value::as_str).unwrap_or("other").to_string();
                    let started_at = r.get("started_at").and_then(Value::as_str).unwrap_or("").to_string();
                    let ended_at = r.get("ended_at").and_then(Value::as_str).map(str::to_string);
                    let status = r.get("status").and_then(Value::as_str).unwrap_or("no_reply").to_string();
                    let step_count = r.get("step_count").and_then(Value::as_u64).unwrap_or(0);
                    let preview = r.get("preview").and_then(Value::as_str).unwrap_or("").to_string();
                    Some(RunSummary { id, agent_id, channel, started_at, ended_at, status, step_count, preview })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEvent {
    pub kind: String,
    pub role: Option<String>,
    pub tool: Option<String>,
    pub ok: Option<bool>,
    pub label: Option<String>,
    pub ts: String,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunDetail {
    pub events: Vec<RunEvent>,
}

pub fn parse_run_detail(v: &Value) -> RunDetail {
    let events = v
        .get("events")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|e| RunEvent {
                    kind: e.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                    role: e.get("role").and_then(Value::as_str).map(str::to_string),
                    tool: e.get("tool").and_then(Value::as_str).map(str::to_string),
                    ok: e.get("ok").and_then(Value::as_bool),
                    label: e.get("label").and_then(Value::as_str).map(str::to_string),
                    ts: e.get("ts").and_then(Value::as_str).unwrap_or("").to_string(),
                    preview: e.get("preview").and_then(Value::as_str).unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    RunDetail { events }
}

/// Tool/step/todo events only — the inline-expand row's "工具步驟" list.
/// Prose (`kind == "text"`) events are handled separately by
/// `last_assistant_text`; this mirrors `web/src/lib/run-transcript.ts`'s
/// `cardsForEvents` minus the prose branch (the inline-expand design, B4,
/// has no room for a full transcript — only the tool trail + the final
/// reply, per the canvas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStepCard {
    pub label: String,
    pub ok: Option<bool>,
}

pub fn tool_steps(events: &[RunEvent]) -> Vec<ToolStepCard> {
    events
        .iter()
        .filter_map(|e| match e.kind.as_str() {
            "tool_use" if e.tool.as_deref().is_some_and(|t| !t.is_empty()) => {
                Some(ToolStepCard { label: e.tool.clone().unwrap_or_default(), ok: Some(e.ok != Some(false)) })
            }
            "tool_step" if e.label.as_deref().is_some_and(|l| !l.is_empty()) => {
                Some(ToolStepCard { label: e.label.clone().unwrap_or_default(), ok: None })
            }
            _ => None,
        })
        .collect()
}

/// The LAST `todo_update` event's label (`"<done>/<total>"`, real data —
/// see this module's header comment) — `None` when this run never touched
/// TodoWrite, in which case the "工作清單快照" element is dropped entirely
/// rather than shown empty.
pub fn last_todo_snapshot(events: &[RunEvent]) -> Option<String> {
    events.iter().rev().find(|e| e.kind == "todo_update").and_then(|e| e.label.clone())
}

/// The LAST assistant `"text"` event's preview — the closing quoted box the
/// canvas shows ("已確認電費單金額…"). `None` when this run has no
/// recorded assistant turn (still running, or a tool-only invocation).
pub fn last_assistant_text(events: &[RunEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find(|e| e.kind == "text" && e.role.as_deref() == Some("assistant") && !e.preview.is_empty())
        .map(|e| e.preview.clone())
}

/// Status → (theme color token, i18n key suffix `runs.status.<suffix>`) —
/// the three REAL statuses only, port of `web/src/lib/run-transcript.ts`'s
/// `runStatusMeta`.
pub fn status_meta(status: &str) -> (u32, &'static str) {
    match status {
        "running" => (crate::theme::BRAND, "running"),
        "completed" => (crate::theme::SUCCESS, "completed"),
        _ => (crate::theme::MUTED_FOREGROUND, "no_reply"),
    }
}

/// Wall-clock duration in whole seconds — `None` while the run has no real
/// end (never invented), port of `runDurationSecs`.
pub fn duration_secs(started_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let ended_at = ended_at?;
    let a = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let b = chrono::DateTime::parse_from_rfc3339(ended_at).ok()?;
    let secs = (b - a).num_seconds();
    if secs < 0 {
        None
    } else {
        Some(secs)
    }
}

/// `"12 秒"` / `"2 分 03 秒"` — port of `fmtDuration`. Zh-only phrasing
/// lives in the i18n catalog (`runs.duration.sec`/`runs.duration.minSec`);
/// this fn only computes the numbers, the caller substitutes them in.
pub fn format_duration_parts(secs: i64) -> (i64, i64) {
    if secs < 60 {
        (0, secs)
    } else {
        (secs / 60, secs % 60)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    All,
    Today,
    Last7Days,
}

/// Client-side time-range filter (no server-side param exists on
/// `runs.list`) — `now`/`started_at` compared in the LOCAL timezone so
/// "today" matches what the row's own HH:MM label shows.
pub fn matches_time_range(started_at: &str, range: TimeRange, now: chrono::DateTime<chrono::Local>) -> bool {
    if range == TimeRange::All {
        return true;
    }
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return true; // unparseable timestamp: never hide a row over a formatting gap
    };
    let local = parsed.with_timezone(&chrono::Local);
    match range {
        TimeRange::All => true,
        TimeRange::Today => local.date_naive() == now.date_naive(),
        TimeRange::Last7Days => (now - local).num_hours() < 24 * 7,
    }
}

/// Client-side text search over preview/agent-display-name/channel — no
/// `q` param exists on `runs.list` server-side, matches web's own
/// task-filter precedent of deriving client-side rather than inventing an
/// RPC param.
pub fn matches_search(r: &RunSummary, agent_display_name: &str, channel_label: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    let q = q.to_lowercase();
    r.preview.to_lowercase().contains(&q)
        || agent_display_name.to_lowercase().contains(&q)
        || channel_label.to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_runs_list_reads_every_field() {
        let v = json!({ "runs": [
            { "id": "r1", "agent_id": "duduclaw", "channel": "telegram", "started_at": "2026-08-21T14:32:00Z",
              "ended_at": "2026-08-21T14:32:12Z", "status": "completed", "step_count": 3, "preview": "hi" },
        ]});
        let rows = parse_runs_list(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "r1");
        assert_eq!(rows[0].step_count, 3);
    }

    #[test]
    fn parse_runs_list_missing_array_is_empty() {
        assert!(parse_runs_list(&json!({})).is_empty());
    }

    #[test]
    fn parse_runs_list_skips_entries_without_id() {
        let v = json!({ "runs": [{ "agent_id": "x" }] });
        assert!(parse_runs_list(&v).is_empty());
    }

    #[test]
    fn tool_steps_filters_to_tool_and_step_kinds_only() {
        let events = vec![
            RunEvent { kind: "text".into(), role: Some("user".into()), tool: None, ok: None, label: None, ts: "t".into(), preview: "hi".into() },
            RunEvent { kind: "tool_use".into(), role: None, tool: Some("web_fetch".into()), ok: Some(true), label: None, ts: "t".into(), preview: "".into() },
            RunEvent { kind: "tool_use".into(), role: None, tool: Some("web_fetch".into()), ok: Some(false), label: None, ts: "t".into(), preview: "".into() },
            RunEvent { kind: "tool_step".into(), role: None, tool: None, ok: None, label: Some("讀取帳單附件".into()), ts: "t".into(), preview: "".into() },
        ];
        let steps = tool_steps(&events);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].ok, Some(true));
        assert_eq!(steps[1].ok, Some(false));
        assert_eq!(steps[2].ok, None);
        assert_eq!(steps[2].label, "讀取帳單附件");
    }

    #[test]
    fn last_todo_snapshot_picks_the_last_one() {
        let events = vec![
            RunEvent { kind: "todo_update".into(), role: None, tool: None, ok: None, label: Some("1/3".into()), ts: "t".into(), preview: "".into() },
            RunEvent { kind: "todo_update".into(), role: None, tool: None, ok: None, label: Some("2/3".into()), ts: "t".into(), preview: "".into() },
        ];
        assert_eq!(last_todo_snapshot(&events), Some("2/3".to_string()));
        assert_eq!(last_todo_snapshot(&[]), None);
    }

    #[test]
    fn last_assistant_text_ignores_empty_previews_and_user_turns() {
        let events = vec![
            RunEvent { kind: "text".into(), role: Some("assistant".into()), tool: None, ok: None, label: None, ts: "t".into(), preview: "".into() },
            RunEvent { kind: "text".into(), role: Some("user".into()), tool: None, ok: None, label: None, ts: "t".into(), preview: "ignore me".into() },
            RunEvent { kind: "text".into(), role: Some("assistant".into()), tool: None, ok: None, label: None, ts: "t".into(), preview: "已確認".into() },
        ];
        assert_eq!(last_assistant_text(&events), Some("已確認".to_string()));
    }

    #[test]
    fn status_meta_covers_all_three_real_statuses() {
        assert_eq!(status_meta("running").1, "running");
        assert_eq!(status_meta("completed").1, "completed");
        assert_eq!(status_meta("no_reply").1, "no_reply");
        assert_eq!(status_meta("bogus").1, "no_reply");
    }

    #[test]
    fn duration_secs_none_while_running_or_malformed() {
        assert_eq!(duration_secs("2026-08-21T14:00:00Z", None), None);
        assert_eq!(duration_secs("not-a-date", Some("2026-08-21T14:00:00Z")), None);
        assert_eq!(duration_secs("2026-08-21T14:00:00Z", Some("2026-08-21T14:00:12Z")), Some(12));
    }

    #[test]
    fn format_duration_parts_matches_web_thresholds() {
        assert_eq!(format_duration_parts(12), (0, 12));
        assert_eq!(format_duration_parts(123), (2, 3));
    }

    #[test]
    fn matches_search_checks_preview_agent_and_channel() {
        let r = RunSummary {
            id: "r1".into(), agent_id: "duduclaw".into(), channel: "telegram".into(),
            started_at: "t".into(), ended_at: None, status: "completed".into(), step_count: 0,
            preview: "已回覆客戶退貨詢問".into(),
        };
        assert!(matches_search(&r, "小杜", "Telegram", ""));
        assert!(matches_search(&r, "小杜", "Telegram", "退貨"));
        assert!(matches_search(&r, "小杜", "Telegram", "小杜"));
        assert!(!matches_search(&r, "小杜", "Telegram", "報價"));
    }
}
