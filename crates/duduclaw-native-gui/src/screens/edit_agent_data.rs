// Data model + pure parsing for the "編輯員工" (EditAgent) page — WP-S6b2-N,
// S6b 第二波, 2026-08-21. Sibling of `edit_agent.rs`/`edit_agent_tabs_a.rs`/
// `edit_agent_tabs_b.rs`, split off for the same file-size reason
// `agents.rs`/`agents_data.rs` are split (see `edit_agent.rs`'s own header
// comment for the full module-family rationale).
//
// ── Data source (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed) ─────────────────────────────────────────
//   `agents.inspect {"agent_id"}` → `handle_agents_inspect` (L11494), a FLAT
//   object (not `{"agent": {...}}`), `json!` block at L11536-11659. Every
//   field this parser reads is quoted from that block directly — see the
//   inline comments on `parse_edit_agent_detail` below for the exact path.
//   NOT `agents_data::AgentDetailData`/`parse_agent_detail` — that struct is
//   a curated subset built for the OTHER (read-only, capability-summary)
//   detail page and drops most of the fields this page's 8 tabs need (model.
//   fallback, budget, heartbeat, proactive, sticker, evolution, research,
//   permissions, ...). This is a second, fuller parser over the SAME RPC,
//   not a duplicate concept.
//   `contract.get {"agent_id"}` → `handle_contract_get` (L9740) →
//   `contract_table_to_response` (L3366): `{must_not[], must_always[],
//   max_tool_calls_per_turn}` (`agent_id` is inserted by the handler on top,
//   read separately since this struct is scoped to one already-known agent).
//   `must_not`/`must_always` are always arrays (possibly empty — TOML
//   `boundaries` table absent ⇒ `str_arr` defaults to `[]`, never omitted).
//
// ── Why some web-form fields have NO corresponding struct field here ─────
// `agents.inspect` is a read-back of `agent.toml`'s CURRENT state, not a
// mirror of every field `web/src/pages/agent-form/EditAgentPage.tsx` lets an
// operator type into a form. Fields the web form writes but the inspect
// handler never reads back (Odoo per-agent override, `utility` model,
// `max_active_skills`/`skill_token_budget`, the "進階模型參數" ptc/prompt/
// cultural_context table) have NO field on this struct — see each tab
// render function in `edit_agent_tabs_a.rs`/`edit_agent_tabs_b.rs` for a
// one-line comment at the exact point each omission was decided, rather
// than fabricating a "current value" this parser cannot honestly produce.

use serde_json::Value;

fn opt_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

fn str_arr(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

// ── agents.inspect (full-tab superset) ────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditAgentDetail {
    // ── identity (一般 tab, full 1:1 coverage) ────────────────────────
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub department: String,
    pub status: String,
    pub icon: Option<String>,
    pub trigger: Option<String>,
    pub reports_to: Option<String>,

    // ── 技能 tab ────────────────────────────────────────────────────────
    pub skills: Vec<String>,
    pub skill_auto_activate: bool,
    pub skill_security_scan: bool,

    // ── 工具 tab ────────────────────────────────────────────────────────
    pub can_create_agents: bool,
    pub can_send_cross_agent: bool,
    pub can_modify_own_skills: bool,
    pub can_modify_own_soul: bool,
    pub can_schedule_tasks: bool,
    pub autonomy_level: Option<String>,
    pub denied_tools_count: usize,
    pub allowed_tools_count: usize,

    // ── 大腦 tab ────────────────────────────────────────────────────────
    pub model_preferred: Option<String>,
    pub model_fallback: Option<String>,
    pub model_api_mode: Option<String>,
    pub account_pool: Option<String>,
    pub runtime_provider: Option<String>,
    pub runtime_fallback: Option<String>,
    pub pty_pool_enabled: Option<bool>,

    // ── 預算 tab ────────────────────────────────────────────────────────
    pub budget_monthly_limit_cents: i64,
    pub budget_spent_cents: i64,
    pub budget_warn_threshold_percent: i64,
    pub budget_hard_stop: bool,

    // ── 自動化 tab ──────────────────────────────────────────────────────
    pub heartbeat_enabled: bool,
    pub heartbeat_interval_seconds: i64,
    pub proactive_enabled: bool,
    pub proactive_notify_channel: Option<String>,
    pub proactive_notify_chat_id: Option<String>,
    pub proactive_quiet_hours: Option<String>,
    pub proactive_quiet_hours_note: Option<String>,
    pub gvu_enabled: bool,
    pub max_silence_hours: f64,
    pub research_self_study: bool,
    pub research_self_study_hour: Option<i64>,

    // ── 進階 tab ────────────────────────────────────────────────────────
    pub sticker_enabled: bool,
    pub sticker_probability: f64,
    pub sticker_intensity_threshold: f64,
    pub sticker_cooldown_messages: i64,
    pub sticker_expressiveness: String,
}

/// Parses the flat `agents.inspect` response object directly. `None` only
/// when the payload carries no `name` at all (e.g. an error response the
/// caller failed to route to the error branch) — same contract
/// `agents_data::parse_agent_detail` already establishes for the sibling
/// detail page.
pub fn parse_edit_agent_detail(payload: &Value) -> Option<EditAgentDetail> {
    let id = payload.get("name")?.as_str()?.to_string();
    let display_name = opt_string(payload, "display_name").unwrap_or_else(|| id.clone());
    let role = payload.get("role").and_then(Value::as_str).unwrap_or("").to_string();
    let department = payload.get("department").and_then(Value::as_str).unwrap_or("").to_string();
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("active").to_string();
    let icon = opt_string(payload, "icon");
    let trigger = opt_string(payload, "trigger");
    let reports_to = opt_string(payload, "reports_to");

    let skills = payload
        .get("skills")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let evolution = payload.get("evolution");
    let skill_auto_activate = evolution.and_then(|e| e.get("skill_auto_activate")).and_then(Value::as_bool).unwrap_or(false);
    let skill_security_scan = evolution.and_then(|e| e.get("skill_security_scan")).and_then(Value::as_bool).unwrap_or(false);
    let gvu_enabled = evolution.and_then(|e| e.get("gvu_enabled")).and_then(Value::as_bool).unwrap_or(false);
    let max_silence_hours = evolution.and_then(|e| e.get("max_silence_hours")).and_then(Value::as_f64).unwrap_or(0.0);

    let permissions = payload.get("permissions");
    let can_create_agents = permissions.and_then(|p| p.get("can_create_agents")).and_then(Value::as_bool).unwrap_or(false);
    let can_send_cross_agent = permissions.and_then(|p| p.get("can_send_cross_agent")).and_then(Value::as_bool).unwrap_or(false);
    let can_modify_own_skills = permissions.and_then(|p| p.get("can_modify_own_skills")).and_then(Value::as_bool).unwrap_or(false);
    let can_modify_own_soul = permissions.and_then(|p| p.get("can_modify_own_soul")).and_then(Value::as_bool).unwrap_or(false);
    let can_schedule_tasks = permissions.and_then(|p| p.get("can_schedule_tasks")).and_then(Value::as_bool).unwrap_or(false);

    let caps = payload.get("capabilities");
    let autonomy_level = caps.and_then(|c| opt_string(c, "autonomy_level"));
    let denied_tools_count = caps.and_then(|c| c.get("denied_tools")).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    let allowed_tools_count = caps.and_then(|c| c.get("allowed_tools")).and_then(Value::as_array).map(Vec::len).unwrap_or(0);

    let model = payload.get("model");
    let model_preferred = model.and_then(|m| opt_string(m, "preferred"));
    let model_fallback = model.and_then(|m| opt_string(m, "fallback"));
    let model_api_mode = model.and_then(|m| opt_string(m, "api_mode"));
    let account_pool = model.and_then(|m| opt_string(m, "account_pool"));

    let runtime = payload.get("runtime");
    let runtime_provider = runtime.and_then(|r| opt_string(r, "provider"));
    let runtime_fallback = runtime.and_then(|r| opt_string(r, "fallback"));
    let pty_pool_enabled = runtime.and_then(|r| r.get("pty_pool_enabled")).and_then(Value::as_bool);

    let budget = payload.get("budget");
    let budget_monthly_limit_cents = budget.and_then(|b| b.get("monthly_limit_cents")).and_then(Value::as_i64).unwrap_or(0);
    let budget_spent_cents = budget.and_then(|b| b.get("spent_cents")).and_then(Value::as_i64).unwrap_or(0);
    let budget_warn_threshold_percent = budget.and_then(|b| b.get("warn_threshold_percent")).and_then(Value::as_i64).unwrap_or(0);
    let budget_hard_stop = budget.and_then(|b| b.get("hard_stop")).and_then(Value::as_bool).unwrap_or(false);

    let heartbeat = payload.get("heartbeat");
    let heartbeat_enabled = heartbeat.and_then(|h| h.get("enabled")).and_then(Value::as_bool).unwrap_or(false);
    let heartbeat_interval_seconds = heartbeat.and_then(|h| h.get("interval_seconds")).and_then(Value::as_i64).unwrap_or(0);

    let proactive = payload.get("proactive");
    let proactive_enabled = proactive.and_then(|p| p.get("enabled")).and_then(Value::as_bool).unwrap_or(false);
    let proactive_notify_channel = proactive.and_then(|p| opt_string(p, "notify_channel"));
    let proactive_notify_chat_id = proactive.and_then(|p| opt_string(p, "notify_chat_id"));
    let proactive_quiet_hours = proactive.and_then(|p| opt_string(p, "quiet_hours"));
    let proactive_quiet_hours_note = proactive.and_then(|p| opt_string(p, "quiet_hours_note"));

    let research = payload.get("research");
    let research_self_study = research.and_then(|r| r.get("self_study")).and_then(Value::as_bool).unwrap_or(false);
    let research_self_study_hour = research.and_then(|r| r.get("self_study_hour")).and_then(Value::as_i64);

    let sticker = payload.get("sticker");
    let sticker_enabled = sticker.and_then(|s| s.get("enabled")).and_then(Value::as_bool).unwrap_or(false);
    let sticker_probability = sticker.and_then(|s| s.get("probability")).and_then(Value::as_f64).unwrap_or(0.0);
    let sticker_intensity_threshold = sticker.and_then(|s| s.get("intensity_threshold")).and_then(Value::as_f64).unwrap_or(0.0);
    let sticker_cooldown_messages = sticker.and_then(|s| s.get("cooldown_messages")).and_then(Value::as_i64).unwrap_or(0);
    let sticker_expressiveness = sticker.and_then(|s| s.get("expressiveness")).and_then(Value::as_str).unwrap_or("moderate").to_string();

    Some(EditAgentDetail {
        id,
        display_name,
        role,
        department,
        status,
        icon,
        trigger,
        reports_to,
        skills,
        skill_auto_activate,
        skill_security_scan,
        can_create_agents,
        can_send_cross_agent,
        can_modify_own_skills,
        can_modify_own_soul,
        can_schedule_tasks,
        autonomy_level,
        denied_tools_count,
        allowed_tools_count,
        model_preferred,
        model_fallback,
        model_api_mode,
        account_pool,
        runtime_provider,
        runtime_fallback,
        pty_pool_enabled,
        budget_monthly_limit_cents,
        budget_spent_cents,
        budget_warn_threshold_percent,
        budget_hard_stop,
        heartbeat_enabled,
        heartbeat_interval_seconds,
        proactive_enabled,
        proactive_notify_channel,
        proactive_notify_chat_id,
        proactive_quiet_hours,
        proactive_quiet_hours_note,
        gvu_enabled,
        max_silence_hours,
        research_self_study,
        research_self_study_hour,
        sticker_enabled,
        sticker_probability,
        sticker_intensity_threshold,
        sticker_cooldown_messages,
        sticker_expressiveness,
    })
}

// ── contract.get (工具 tab's lazily-fetched CONTRACT.toml section) ────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContractData {
    pub must_not: Vec<String>,
    pub must_always: Vec<String>,
    pub max_tool_calls_per_turn: i64,
}

/// `contract_table_to_response`'s three keys are always present (never
/// `skip_serializing_if`-guarded — an absent `[boundaries]` table just
/// yields empty arrays / `0`), so this never needs an `Option` return the
/// way `parse_edit_agent_detail` does.
pub fn parse_contract(payload: &Value) -> ContractData {
    ContractData {
        must_not: str_arr(payload, "must_not"),
        must_always: str_arr(payload, "must_always"),
        max_tool_calls_per_turn: payload.get("max_tool_calls_per_turn").and_then(Value::as_i64).unwrap_or(0),
    }
}

// ── Display helpers ────────────────────────────────────────────────────

/// Integer cents → "N,NNN" (no decimals) — same recipe `screens::billing::
/// format_dollars`/`screens::accounts::format_dollars`/`screens::
/// dashboard_cards::format_dollars` all independently establish (each
/// `fn`-private to its own file — duplicated locally per this crate's
/// established "local copy over widened visibility" convention, not a guess
/// at behavior).
pub fn format_dollars(cents: i64) -> String {
    let dollars = cents / 100;
    let sign = if dollars < 0 { "-" } else { "" };
    let digits = dollars.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    format!("{sign}{}", grouped.chars().rev().collect::<String>())
}

/// `f32` probability/threshold (0.0-1.0) → whole-number percent string, no
/// trailing ".0" for the common case (`0.3` → `"30%"`, `0.125` → `"12.5%"`).
pub fn format_percent(fraction: f64) -> String {
    let pct = fraction * 100.0;
    if (pct.round() - pct).abs() < 0.001 {
        format!("{}%", pct.round() as i64)
    } else {
        format!("{pct:.1}%")
    }
}

/// `f64` hours → a display string with no trailing ".0" for whole numbers
/// (`12.0` → `"12"`, `1.5` → `"1.5"`) — `max_silence_hours` is a free-form
/// `f64` on disk (see `duduclaw-core::types::EvolutionConfig`), not always
/// an integer.
pub fn format_hours(hours: f64) -> String {
    if (hours.round() - hours).abs() < 0.001 {
        format!("{}", hours.round() as i64)
    } else {
        format!("{hours:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_payload() -> Value {
        json!({
            "name": "biz_dev", "display_name": "業務開發專員", "role": "specialist",
            "status": "active", "department": "業務部", "icon": "💼",
            "trigger": "@biz", "reports_to": "dudu",
            "skills": ["web_search", "email_draft"],
            "model": { "preferred": "claude-sonnet-4-6", "fallback": null, "api_mode": "cli", "account_pool": "primary,backupA" },
            "runtime": { "provider": "claude", "pty_pool_enabled": false },
            "budget": { "monthly_limit_cents": 500000, "spent_cents": 12345, "warn_threshold_percent": 80, "hard_stop": true },
            "heartbeat": { "enabled": true, "interval_seconds": 1800 },
            "proactive": { "enabled": true, "notify_channel": "telegram", "notify_chat_id": "123", "quiet_hours": "22:00-08:00", "quiet_hours_note": "夜間不打擾" },
            "permissions": { "can_create_agents": false, "can_send_cross_agent": true, "can_modify_own_skills": false, "can_modify_own_soul": false, "can_schedule_tasks": true },
            "sticker": { "enabled": true, "probability": 0.3, "intensity_threshold": 0.7, "cooldown_messages": 5, "expressiveness": "moderate" },
            "evolution": { "gvu_enabled": false, "skill_auto_activate": true, "skill_security_scan": true, "max_silence_hours": 12.0 },
            "capabilities": { "autonomy_level": "collaborator", "denied_tools": ["Bash"], "allowed_tools": [] },
            "research": { "self_study": true, "self_study_hour": 3 },
        })
    }

    #[test]
    fn parse_edit_agent_detail_reads_every_tab_field_from_the_handle_agents_inspect_shape() {
        let d = parse_edit_agent_detail(&full_payload()).unwrap();
        assert_eq!(d.id, "biz_dev");
        assert_eq!(d.display_name, "業務開發專員");
        assert_eq!(d.role, "specialist");
        assert_eq!(d.icon.as_deref(), Some("💼"));
        assert_eq!(d.trigger.as_deref(), Some("@biz"));
        assert_eq!(d.reports_to.as_deref(), Some("dudu"));
        assert_eq!(d.skills, vec!["web_search".to_string(), "email_draft".to_string()]);
        assert_eq!(d.model_preferred.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(d.model_fallback, None);
        assert_eq!(d.account_pool.as_deref(), Some("primary,backupA"));
        assert_eq!(d.runtime_provider.as_deref(), Some("claude"));
        assert_eq!(d.pty_pool_enabled, Some(false));
        assert_eq!(d.budget_monthly_limit_cents, 500000);
        assert_eq!(d.budget_spent_cents, 12345);
        assert_eq!(d.budget_warn_threshold_percent, 80);
        assert!(d.budget_hard_stop);
        assert!(d.heartbeat_enabled);
        assert_eq!(d.heartbeat_interval_seconds, 1800);
        assert!(d.proactive_enabled);
        assert_eq!(d.proactive_notify_channel.as_deref(), Some("telegram"));
        assert_eq!(d.proactive_quiet_hours.as_deref(), Some("22:00-08:00"));
        assert!(d.can_send_cross_agent);
        assert!(!d.can_create_agents);
        assert!(d.sticker_enabled);
        assert!((d.sticker_probability - 0.3).abs() < 1e-9);
        assert_eq!(d.sticker_expressiveness, "moderate");
        assert!(!d.gvu_enabled);
        assert!(d.skill_auto_activate);
        assert_eq!(d.max_silence_hours, 12.0);
        assert_eq!(d.autonomy_level.as_deref(), Some("collaborator"));
        assert_eq!(d.denied_tools_count, 1);
        assert_eq!(d.allowed_tools_count, 0);
        assert!(d.research_self_study);
        assert_eq!(d.research_self_study_hour, Some(3));
    }

    #[test]
    fn parse_edit_agent_detail_missing_name_is_none_not_panicking() {
        assert!(parse_edit_agent_detail(&json!({"display_name": "no id"})).is_none());
        assert!(parse_edit_agent_detail(&json!(null)).is_none());
    }

    #[test]
    fn parse_edit_agent_detail_bare_payload_degrades_honestly_not_panicking() {
        let d = parse_edit_agent_detail(&json!({ "name": "bare" })).unwrap();
        assert_eq!(d.display_name, "bare"); // falls back to id
        assert_eq!(d.status, "active"); // documented default
        assert!(d.skills.is_empty());
        assert_eq!(d.model_preferred, None);
        assert_eq!(d.runtime_provider, None);
        assert_eq!(d.pty_pool_enabled, None);
        assert_eq!(d.budget_monthly_limit_cents, 0);
        assert!(!d.heartbeat_enabled);
        assert_eq!(d.autonomy_level, None);
        assert_eq!(d.denied_tools_count, 0);
        assert_eq!(d.research_self_study_hour, None);
    }

    #[test]
    fn parse_contract_reads_the_handle_contract_get_shape() {
        let v = json!({ "agent_id": "biz_dev", "must_not": ["洩漏客戶資料"], "must_always": ["先確認再送出"], "max_tool_calls_per_turn": 12 });
        let c = parse_contract(&v);
        assert_eq!(c.must_not, vec!["洩漏客戶資料".to_string()]);
        assert_eq!(c.must_always, vec!["先確認再送出".to_string()]);
        assert_eq!(c.max_tool_calls_per_turn, 12);
    }

    #[test]
    fn parse_contract_malformed_payload_defaults_honestly_not_panicking() {
        let c = parse_contract(&json!(null));
        assert!(c.must_not.is_empty());
        assert!(c.must_always.is_empty());
        assert_eq!(c.max_tool_calls_per_turn, 0);
    }

    #[test]
    fn format_dollars_groups_thousands() {
        assert_eq!(format_dollars(500000), "5,000");
        assert_eq!(format_dollars(12345), "123");
        assert_eq!(format_dollars(0), "0");
    }

    #[test]
    fn format_percent_drops_trailing_zero_for_whole_numbers() {
        assert_eq!(format_percent(0.3), "30%");
        assert_eq!(format_percent(0.7), "70%");
        assert_eq!(format_percent(0.125), "12.5%");
    }

    #[test]
    fn format_hours_drops_trailing_zero_for_whole_numbers() {
        assert_eq!(format_hours(12.0), "12");
        assert_eq!(format_hours(1.5), "1.5");
    }
}
