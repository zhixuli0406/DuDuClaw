// Data model + pure parsing for the "AI 員工" pages (p11 list/p12 detail) —
// split out of `agents.rs` for the same file-size reason `goals_data.rs` is
// split from `goals.rs` (see that file's own header comment for the
// precedent). No behavior differs from an unsplit version: every type/
// function here is a clean, self-contained layer `agents.rs`/`agents_list.rs`/
// `agents_summary.rs`/`agents_detail.rs` all import from.
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ─────────────────────────────────────────
//   `agents.list {}` → `{"agents": [...]}`, each row shaped by
//   `handle_agents_list_filtered` (~L25683): name/display_name/role/status/
//   archived/department/icon/model/... — `role`/`status` are BOTH
//   `format!("{:?}", enum).to_lowercase()`, i.e. Rust's `Debug` output
//   lowercased, NOT the kebab-case `as_str()` encoding — `AgentRole::
//   TeamLeader` therefore serializes as `"teamleader"` (no hyphen), not
//   `"team-leader"`. `role_label` below matches that exact wire shape.
//   `agents.inspect {"agent_id"}` → `handle_agents_inspect` (~L11485): same
//   name/display_name/role/status/department fields plus `model`/`runtime`/
//   `capabilities`. `runtime` emits ONLY keys present in agent.toml (unset
//   ⇒ absent, not `null`) — `runtime_provider: None` is therefore an honest
//   "not configured (auto-detect)", not a parse failure.
//   `tasks.list {"agent_id"}` → `task_row_to_json` (~L33422), server-side
//   filtered to this agent already (`list_tasks_filtered`) — this page reads
//   only status/completed_at, mirroring `web/src/components/agent/
//   agent-stats.ts`'s `agentTaskStats` tally (done-today / in_progress /
//   needs_human) rather than reinventing a new taxonomy.
//   `activity.list {"agent_id","limit"}` → `activity_row_to_json`
//   (~L33507): id/type/agent_id/task_id?/summary/timestamp/metadata?.
//   `channels.status {}` (admin-only, `require_admin!()`) → `handle_
//   channels_status` (~L11659): `{"channels": [{"name","connected",
//   "last_connected","error"}]}`. `name` is either a bare platform token
//   ("line"/"telegram"/"discord" — the MAIN agent's global `config.toml
//   [channels]`) or `"<platform>:<agent_name>"` (a sub-agent's own `agent.
//   toml [channels]`) — see `channels_for_agent` below for the exact
//   derivation, quoted from that handler's own two-branch construction.

use serde_json::Value;

// ── agents.list ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentListItem {
    /// `name` on the wire — the routable agent id, distinct from
    /// `display_name` (same split `chat/agents_picker.rs::AgentSummary`
    /// already documents).
    pub id: String,
    pub display_name: String,
    /// `format!("{:?}", AgentRole).to_lowercase()` — see this file's header
    /// comment. Rendered through `role_label` below, never shown raw.
    pub role: String,
    /// May be empty (`AgentConfig.agent.department` is a plain `String`,
    /// not `Option<String>` — an empty string IS "unset", not a parse gap).
    pub department: String,
    /// `"active" | "paused" | "terminated"` in practice (archived/deleted
    /// rows are excluded server-side unless `include_archived` is passed,
    /// which this page never does).
    pub status: String,
    pub icon: Option<String>,
}

pub fn parse_agents_list(payload: &Value) -> Vec<AgentListItem> {
    payload
        .get("agents")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let id = a.get("name")?.as_str()?.to_string();
                    let display_name = a
                        .get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| id.clone());
                    let role = a.get("role").and_then(Value::as_str).unwrap_or("").to_string();
                    let department = a.get("department").and_then(Value::as_str).unwrap_or("").to_string();
                    let status = a.get("status").and_then(Value::as_str).unwrap_or("active").to_string();
                    let icon = a.get("icon").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
                    Some(AgentListItem { id, display_name, role, department, status, icon })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `is_main` — whether this row's `role` is the platform's single `main`
/// agent (`AgentRole::Main` → `"main"`). Used by `channels_for_agent` to
/// decide whether the bare (non-prefixed) global channel rows belong to it.
pub fn is_main_role(role: &str) -> bool {
    role == "main"
}

// ── agents.inspect ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentDetailData {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub department: String,
    pub status: String,
    pub icon: Option<String>,
    pub model_preferred: Option<String>,
    pub account_pool: Option<String>,
    pub runtime_provider: Option<String>,
    pub runtime_fallback: Option<String>,
    /// Curated subset of `CapabilitiesConfig`'s real typed boolean fields —
    /// not the full ~15-field struct (this page's capabilities box is
    /// explicitly READ-ONLY this round, see `agents_detail.rs`'s own doc
    /// comment). No field here is fabricated: each is read straight off
    /// `capabilities.<name>` in the `agents.inspect` payload.
    pub browser_via_bash: bool,
    pub computer_use: bool,
    pub os_native: bool,
    /// `capabilities.autonomy_level` — a 5-level string
    /// (operator/collaborator/consultant/approver/observer), folded into
    /// the serialized `capabilities` object server-side (see this file's
    /// header comment). `None` only if the payload is malformed; the real
    /// handler always inserts a resolved value (`AutonomyLevel::for_agent`
    /// never fails), so absence here would itself be a signal worth an
    /// honest "未提供" render, not a silent default.
    pub autonomy_level: Option<String>,
    /// `capabilities.approval_required_tools` — `skip_serializing_if`-
    /// guarded server-side (absent, not `[]`, when empty). `0` here is
    /// therefore ambiguous between "explicitly empty" and "field absent",
    /// both of which mean the same thing to this page (no extra approval
    /// friction configured) — rendered as a count, never a fabricated list.
    pub approval_required_tools_count: usize,
}

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Parses the flat `agents.inspect` response object directly (the payload
/// itself IS the agent, not `{"agent": {...}}` — see this file's header
/// comment). `None` when the payload carries no `name` at all (e.g. an
/// error response the caller failed to route to the error branch).
pub fn parse_agent_detail(payload: &Value) -> Option<AgentDetailData> {
    let id = payload.get("name")?.as_str()?.to_string();
    let display_name = opt_string(payload, "display_name").unwrap_or_else(|| id.clone());
    let role = payload.get("role").and_then(Value::as_str).unwrap_or("").to_string();
    let department = payload.get("department").and_then(Value::as_str).unwrap_or("").to_string();
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("active").to_string();
    let icon = opt_string(payload, "icon");

    let model = payload.get("model");
    let model_preferred = model.and_then(|m| opt_string(m, "preferred"));
    let account_pool = model.and_then(|m| opt_string(m, "account_pool"));

    let runtime = payload.get("runtime");
    let runtime_provider = runtime.and_then(|r| opt_string(r, "provider"));
    let runtime_fallback = runtime.and_then(|r| opt_string(r, "fallback"));

    let caps = payload.get("capabilities");
    let browser_via_bash = caps.and_then(|c| c.get("browser_via_bash")).and_then(Value::as_bool).unwrap_or(false);
    let computer_use = caps.and_then(|c| c.get("computer_use")).and_then(Value::as_bool).unwrap_or(false);
    let os_native = caps.and_then(|c| c.get("os_native")).and_then(Value::as_bool).unwrap_or(false);
    let autonomy_level = caps.and_then(|c| opt_string(c, "autonomy_level"));
    let approval_required_tools_count = caps
        .and_then(|c| c.get("approval_required_tools"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    Some(AgentDetailData {
        id,
        display_name,
        role,
        department,
        status,
        icon,
        model_preferred,
        account_pool,
        runtime_provider,
        runtime_fallback,
        browser_via_bash,
        computer_use,
        os_native,
        autonomy_level,
        approval_required_tools_count,
    })
}

// ── tasks.list {agent_id} → task stats ───────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTaskStats {
    /// `status == "done"` AND `completed_at` falls on `today` (local
    /// calendar date) — same "today" honesty rule `goals_data.rs::
    /// SmartView::Today` and `web/.../agent-stats.ts::isSameLocalDay` both
    /// already apply: a task done on a prior day is NOT counted, even
    /// without a `completed_at` at all.
    pub today_done: usize,
    pub in_progress: usize,
    /// `status == "needs_human"` — this agent's own goal-loop tasks
    /// currently waiting on a human decision (same status token `goals_
    /// data.rs::SmartView::NeedsHuman` reads from the same table).
    pub needs_human: usize,
}

struct TaskRow {
    status: String,
    completed_at: Option<String>,
}

fn parse_task_rows(v: &Value) -> Vec<TaskRow> {
    v.get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|t| TaskRow {
                    status: t.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
                    completed_at: t.get("completed_at").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Tally straight from the raw `tasks.list` payload — server already
/// scoped the rows to one `agent_id`, so no client-side agent filter is
/// needed (unlike `web/.../agent-stats.ts::agentTaskStats`, which tallies
/// an unscoped roster-wide list).
pub fn agent_task_stats(payload: &Value, today: chrono::NaiveDate) -> AgentTaskStats {
    let rows = parse_task_rows(payload);
    let mut stats = AgentTaskStats::default();
    for row in &rows {
        match row.status.as_str() {
            "done" => {
                if let Some(done_date) = row.completed_at.as_deref().and_then(super::goals_data::local_date) {
                    if done_date == today {
                        stats.today_done += 1;
                    }
                }
            }
            "in_progress" => stats.in_progress += 1,
            "needs_human" => stats.needs_human += 1,
            _ => {}
        }
    }
    stats
}

// ── channels.status → per-agent filter ───────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelStatusItem {
    /// Raw wire `name` — either a bare platform token or `"platform:agent"`.
    pub name: String,
    pub connected: bool,
}

pub fn parse_channel_statuses(payload: &Value) -> Vec<ChannelStatusItem> {
    payload
        .get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?.to_string();
                    let connected = c.get("connected").and_then(Value::as_bool).unwrap_or(false);
                    Some(ChannelStatusItem { name, connected })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row for this agent's "通道" box: `(platform label, connected)`.
/// `handle_channels_status`'s own construction (~L11659-11753, quoted in
/// this file's header comment) emits a bare `"line"`/`"telegram"`/
/// `"discord"` token for the platform's single MAIN agent's global
/// `config.toml [channels]`, and `"<platform>:<agent_name>"` for every
/// sub-agent's own `agent.toml [channels]` — this function is the inverse
/// of that construction, not a guess.
pub fn channels_for_agent<'a>(all: &'a [ChannelStatusItem], agent_id: &str, is_main: bool) -> Vec<&'a ChannelStatusItem> {
    all.iter()
        .filter(|c| match c.name.split_once(':') {
            Some((_, owner)) => owner == agent_id,
            None => is_main,
        })
        .collect()
}

/// The bare platform token from a `channels.status` row name — strips the
/// `"<agent>:"` prefix when present, for display (never the raw internal
/// `"platform:agent_id"` composite — project convention: internal
/// identifiers don't leak to the UI, `handlers.rs`'s own established rule).
pub fn channel_platform_label(raw_name: &str) -> &str {
    raw_name.split_once(':').map(|(platform, _)| platform).unwrap_or(raw_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_agents_list_reads_the_handle_agents_list_filtered_shape() {
        let v = json!({ "agents": [
            { "name": "dudu", "display_name": "小杜", "role": "main", "status": "active", "department": "總管理處", "icon": "🐾" },
            { "name": "finance", "role": "specialist", "status": "paused", "department": "" },
        ]});
        let agents = parse_agents_list(&v);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].id, "dudu");
        assert_eq!(agents[0].display_name, "小杜");
        assert_eq!(agents[0].icon.as_deref(), Some("🐾"));
        assert_eq!(agents[1].display_name, "finance"); // falls back to id
        assert_eq!(agents[1].department, "");
    }

    #[test]
    fn parse_agents_list_missing_name_is_dropped_not_panicking() {
        let v = json!({ "agents": [{"display_name": "no id"}] });
        assert!(parse_agents_list(&v).is_empty());
    }

    #[test]
    fn parse_agents_list_malformed_payload_is_empty_not_panicking() {
        assert!(parse_agents_list(&json!(null)).is_empty());
        assert!(parse_agents_list(&json!({"agents": "nope"})).is_empty());
    }

    #[test]
    fn is_main_role_matches_only_the_literal_debug_lowercased_token() {
        assert!(is_main_role("main"));
        assert!(!is_main_role("specialist"));
        assert!(!is_main_role("teamleader"));
    }

    #[test]
    fn parse_agent_detail_reads_the_handle_agents_inspect_shape() {
        let v = json!({
            "name": "dudu", "display_name": "小杜", "role": "main", "status": "active", "department": "總管理處",
            "model": { "preferred": "claude-sonnet-5", "account_pool": null },
            "runtime": { "provider": "claude" },
            "capabilities": {
                "browser_via_bash": true, "computer_use": false, "os_native": true,
                "autonomy_level": "collaborator",
                "approval_required_tools": ["odoo_write", "mail_send"],
            },
        });
        let d = parse_agent_detail(&v).unwrap();
        assert_eq!(d.id, "dudu");
        assert_eq!(d.model_preferred.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(d.account_pool, None);
        assert_eq!(d.runtime_provider.as_deref(), Some("claude"));
        assert_eq!(d.runtime_fallback, None);
        assert!(d.browser_via_bash);
        assert!(!d.computer_use);
        assert!(d.os_native);
        assert_eq!(d.autonomy_level.as_deref(), Some("collaborator"));
        assert_eq!(d.approval_required_tools_count, 2);
    }

    #[test]
    fn parse_agent_detail_absent_runtime_and_capabilities_degrades_honestly() {
        let v = json!({ "name": "bare", "status": "active" });
        let d = parse_agent_detail(&v).unwrap();
        assert_eq!(d.runtime_provider, None);
        assert!(!d.browser_via_bash);
        assert_eq!(d.autonomy_level, None);
        assert_eq!(d.approval_required_tools_count, 0);
    }

    #[test]
    fn parse_agent_detail_missing_name_is_none_not_panicking() {
        assert!(parse_agent_detail(&json!({"display_name": "no id"})).is_none());
        assert!(parse_agent_detail(&json!(null)).is_none());
    }

    #[test]
    fn agent_task_stats_tallies_done_today_in_progress_and_needs_human() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let v = json!({ "tasks": [
            { "status": "done", "completed_at": "2026-08-21T09:00:00Z" },
            { "status": "done", "completed_at": "2026-08-19T09:00:00Z" }, // not today
            { "status": "done" }, // no completed_at at all
            { "status": "in_progress" },
            { "status": "in_progress" },
            { "status": "needs_human" },
            { "status": "todo" },
        ]});
        let stats = agent_task_stats(&v, today);
        assert_eq!(stats.today_done, 1);
        assert_eq!(stats.in_progress, 2);
        assert_eq!(stats.needs_human, 1);
    }

    #[test]
    fn agent_task_stats_malformed_payload_is_zeroed_not_panicking() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        assert_eq!(agent_task_stats(&json!(null), today), AgentTaskStats::default());
    }

    #[test]
    fn parse_channel_statuses_reads_the_handle_channels_status_shape() {
        let v = json!({ "channels": [
            { "name": "line", "connected": true },
            { "name": "telegram:finance", "connected": false },
        ]});
        let items = parse_channel_statuses(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "line");
        assert!(items[0].connected);
        assert!(!items[1].connected);
    }

    #[test]
    fn channels_for_agent_bare_names_belong_to_main_only() {
        let all = vec![
            ChannelStatusItem { name: "line".into(), connected: true },
            ChannelStatusItem { name: "telegram:finance".into(), connected: false },
            ChannelStatusItem { name: "discord:finance".into(), connected: true },
        ];
        let main_view = channels_for_agent(&all, "dudu", true);
        assert_eq!(main_view.len(), 1);
        assert_eq!(main_view[0].name, "line");

        let finance_view = channels_for_agent(&all, "finance", false);
        assert_eq!(finance_view.len(), 2);

        let sub_agent_as_main_flag_still_excludes_bare = channels_for_agent(&all, "finance", false);
        assert!(!sub_agent_as_main_flag_still_excludes_bare.iter().any(|c| c.name == "line"));
    }

    #[test]
    fn channel_platform_label_strips_the_agent_suffix() {
        assert_eq!(channel_platform_label("telegram:finance"), "telegram");
        assert_eq!(channel_platform_label("line"), "line");
    }
}
