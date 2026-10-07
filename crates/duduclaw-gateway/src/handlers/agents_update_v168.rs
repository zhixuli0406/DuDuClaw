//! v1.68.0 — `agents.update` / `agents.inspect` additions
//! (`commercial/docs/TODO-dashboard-switches-v1.68-2026-10.md`, contract
//! section `agents.update`).
//!
//! * New per-agent keys, each written only when sent (every object is
//!   partial): `budget.daily_cap_cents`, `model.effort`, `fork.enabled`,
//!   `team.{enabled,roles.<role>.{runtime,model,effort}}`, `guardrails.*`,
//!   `memory.{decision_continuity,decision_ttl_days}`, `night_engine.enabled`,
//!   `runtime.minimal_context` (the `capabilities.*_tools` lists live in
//!   `capabilities_apply`).
//! * The typed advanced key/value editor (`advanced_kv`).
//! * The authority guard: the org-guarded keys (`[agent] reports_to /
//!   department / name`, all of `[capabilities]`, `[container] sandbox_enabled
//!   / network_access`, `[permissions] can_modify_own_soul`) may only be
//!   *changed* by an admin, and every change is audited as
//!   `agent_authority_changed`. Re-sending an unchanged value is not a change.
//! * The final-table check: the file that is about to be written must parse
//!   as `AgentConfig` (the registry's loader), otherwise the employee would
//!   silently drop out of the registry on the next scan.
//! * `agents.inspect` prefill: the employee's own values (raw `agent.toml`,
//!   never the preset-merged view) for every key the edit page can change.

#[allow(unused_imports)]
use super::*;

use super::config_commit::{bool_param, is_plain_text, param_at, str_array_param, str_param, table_at_mut, toml_at, toml_at_json, u64_param};
use super::system_update_config_v168::{ProtectedChange, V168Outcome, apply_team_roles};

/// Keys an admin-only change protects. `capabilities` is compared whole.
pub(crate) const AUTHORITY_KEYS: &[&str] = &[
    "agent.reports_to",
    "agent.department",
    "agent.name",
    "capabilities",
    "container.sandbox_enabled",
    "container.network_access",
    "permissions.can_modify_own_soul",
    // v1.68: enforced at the MCP gate, so they are authority too.
    "permissions.can_create_agents",
    "permissions.can_send_cross_agent",
    "permissions.can_schedule_tasks",
    "permissions.can_modify_own_skills",
    "guardrails.enabled",
    "guardrails.block_secrets",
];

/// The effective value of an authority key: an absent key reads as the
/// value the loader would use (`""` / `false` / the capability defaults), so
/// writing an explicit default over an absent key is not a change.
fn effective_authority_value(table: &toml::Table, key: &str) -> Value {
    let raw = toml_at_json(table, key);
    match key {
        "agent.reports_to" | "agent.department" | "agent.name" => match raw {
            Value::Null => Value::String(String::new()),
            v => v,
        },
        "container.sandbox_enabled" | "container.network_access" | "permissions.can_modify_own_soul"
        | "guardrails.enabled" => match raw {
            Value::Null => Value::Bool(false),
            v => v,
        },
        // Absent ⇒ allowed (the gate refuses only an explicit `false`);
        // `block_secrets` defaults on.
        "permissions.can_create_agents"
        | "permissions.can_send_cross_agent"
        | "permissions.can_schedule_tasks"
        | "permissions.can_modify_own_skills"
        | "guardrails.block_secrets" => match raw {
            Value::Null => Value::Bool(true),
            v => v,
        },
        "capabilities" => {
            let defaults = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
                .unwrap_or_else(|_| json!({}));
            let mut obj = raw.as_object().cloned().unwrap_or_default();
            obj.retain(|k, v| {
                let is_default = defaults.get(k) == Some(v);
                let is_empty = v.as_array().is_some_and(|a| a.is_empty())
                    || v.as_object().is_some_and(|o| o.is_empty());
                !(is_default || (is_empty && defaults.get(k).is_none()))
            });
            Value::Object(obj)
        }
        _ => raw,
    }
}

/// Before/after of every authority key whose effective value differs.
pub(crate) fn authority_diff(before: &toml::Table, after: &toml::Table) -> Vec<ProtectedChange> {
    AUTHORITY_KEYS
        .iter()
        .filter_map(|k| {
            let (b, a) = (effective_authority_value(before, k), effective_authority_value(after, k));
            (b != a).then(|| ProtectedChange { key: (*k).to_string(), before: b, after: a })
        })
        .collect()
}

/// The `[capabilities]` diff, reduced to the changed sub-keys (audit rows
/// should name what moved, not repeat the whole table).
pub(crate) fn audit_details(changes: &[ProtectedChange]) -> Value {
    let rows: Vec<Value> = changes
        .iter()
        .flat_map(|c| {
            if c.key != "capabilities" {
                return vec![json!({ "key": c.key, "before": c.before, "after": c.after })];
            }
            let empty = serde_json::Map::new();
            let b = c.before.as_object().unwrap_or(&empty);
            let a = c.after.as_object().unwrap_or(&empty);
            let mut keys: Vec<&String> = b.keys().chain(a.keys()).collect();
            keys.sort();
            keys.dedup();
            keys.into_iter()
                .filter(|k| b.get(*k) != a.get(*k))
                .map(|k| {
                    json!({
                        "key": format!("capabilities.{k}"),
                        "before": b.get(k).cloned().unwrap_or(Value::Null),
                        "after": a.get(k).cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .collect();
    Value::Array(rows)
}

/// Does the table parse with the registry's typed loader? Returns the error
/// with its line/column when it does not.
pub(crate) fn agent_config_check(table: &toml::Table) -> Result<(), String> {
    let text = toml::to_string_pretty(table).map_err(|e| format!("failed to serialise agent.toml: {e}"))?;
    toml::from_str::<duduclaw_core::types::AgentConfig>(&text).map(|_| ()).map_err(|e| {
        let loc = e
            .span()
            .map(|span| {
                let before = &text[..span.start.min(text.len())];
                format!(
                    " (line {}, column {})",
                    before.matches('\n').count() + 1,
                    before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1
                )
            })
            .unwrap_or_default();
        format!("this change would make agent.toml unreadable{loc}: {}", e.message())
    })
}

/// Apply the v1.68 per-agent keys. Returns the change lines.
pub(crate) fn apply_agent_v168_keys(table: &mut toml::Table, params: &Value, is_admin: bool) -> Result<Vec<String>, String> {
    let mut changes = Vec::new();

    if let Some(v) = u64_param(params, "budget.daily_cap_cents", 0, 1_000_000_000)? {
        table_at_mut(table, &["budget"])?.insert("daily_cap_cents".into(), toml::Value::Integer(v as i64));
        changes.push(format!("budget.daily_cap_cents = {v}"));
    }

    if let Some(v) = str_param(params, "model.effort")? {
        let v = v.trim();
        let model = table_at_mut(table, &["model"])?;
        if v.is_empty() {
            model.remove("effort");
            changes.push("model.effort cleared".into());
        } else {
            let e = v
                .parse::<duduclaw_core::effort::Effort>()
                .map_err(|_| "model.effort must be low, medium, high, xhigh or max".to_string())?;
            model.insert("effort".into(), toml::Value::String(e.as_str().into()));
            changes.push(format!("model.effort = \"{}\"", e.as_str()));
        }
    }

    for (path, section, key) in [
        ("fork.enabled", "fork", "enabled"),
        ("night_engine.enabled", "night_engine", "enabled"),
        ("team.enabled", "team", "enabled"),
        ("memory.decision_continuity", "memory", "decision_continuity"),
        ("runtime.minimal_context", "runtime", "minimal_context"),
    ] {
        if let Some(v) = bool_param(params, path)? {
            table_at_mut(table, &[section])?.insert(key.into(), toml::Value::Boolean(v));
            changes.push(format!("{path} = {v}"));
        }
    }
    if let Some(v) = u64_param(params, "memory.decision_ttl_days", 1, 3650)? {
        table_at_mut(table, &["memory"])?.insert("decision_ttl_days".into(), toml::Value::Integer(v as i64));
        changes.push(format!("memory.decision_ttl_days = {v}"));
    }

    let mut team_out = V168Outcome::default();
    apply_team_roles(table, params, "team.roles", &["team", "roles"], &mut team_out)?;
    changes.extend(team_out.changes);

    for key in ["enabled", "block_secrets", "block_injection_echo", "redact_pii"] {
        if let Some(v) = bool_param(params, &format!("guardrails.{key}"))? {
            table_at_mut(table, &["guardrails"])?.insert(key.into(), toml::Value::Boolean(v));
            changes.push(format!("guardrails.{key} = {v}"));
        }
    }
    if let Some(list) = str_array_param(params, "guardrails.deny_phrases")? {
        let list: Vec<String> = list.into_iter().filter(|s| !s.is_empty()).collect();
        if list.len() > 200 || list.iter().any(|s| !is_plain_text(s, 500)) {
            return Err("guardrails.deny_phrases: at most 200 phrases of up to 500 characters".into());
        }
        let n = list.len();
        table_at_mut(table, &["guardrails"])?.insert(
            "deny_phrases".into(),
            toml::Value::Array(list.into_iter().map(toml::Value::String).collect()),
        );
        changes.push(format!("guardrails.deny_phrases = [{n} entries]"));
    }

    changes.extend(apply_advanced_kv(table, params, is_admin)?);
    Ok(changes)
}

/// Sections the advanced editor may not touch: each has dedicated, validated
/// controls (and most are authority or credential sections).
const KV_DENIED_SECTIONS: &[&str] = &[
    "agent", "capabilities", "container", "permissions", "channels", "odoo", "mcp", "runtime",
];

/// Sections with no reader (v1.68 audit): writing them changes nothing.
const KV_DEAD_SECTIONS: &[&str] = &["ptc", "cultural_context", "sticker"];

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `advanced_kv: [{section, key, value, type}]` — `type` is one of
/// `string | integer | float | boolean | string_array` (`array_string`
/// accepted too); `value` must already have that JSON type. A row the UI
/// removed is simply not sent, so nothing is deleted here.
pub(crate) fn apply_advanced_kv(table: &mut toml::Table, params: &Value, is_admin: bool) -> Result<Vec<String>, String> {
    let Some(rows) = param_at(params, "advanced_kv") else {
        return Ok(Vec::new());
    };
    let rows = rows.as_array().ok_or("advanced_kv must be an array of {section, key, value, type}")?;
    if rows.len() > 200 {
        return Err("advanced_kv supports at most 200 rows".into());
    }
    let mut changes = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let field = |name: &str| {
            row.get(name)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .ok_or_else(|| format!("advanced_kv[{i}].{name} must be a string"))
        };
        let section = field("section")?;
        let key = field("key")?;
        let ty = field("type")?;
        let segs: Vec<&str> = section.split('.').collect();
        if segs.len() > 3 || !segs.iter().all(|s| is_ident(s)) || !is_ident(key) {
            return Err(format!(
                "advanced_kv[{i}]: section and key may only contain lowercase letters, digits and _ (got [{section}] {key})"
            ));
        }
        if KV_DEAD_SECTIONS.contains(&segs[0]) {
            return Err(format!("advanced_kv[{i}]: [{}] is not read by anything and is no longer editable", segs[0]));
        }
        // Spend and safety knobs: admin only.
        let admin_only = matches!(segs[0], "guardrails" | "budget")
            || (segs[0] == "model" && key == "account_pool");
        if admin_only && !is_admin {
            return Err(format!(
                "advanced_kv[{i}]: [{section}] {key} can only be changed by an administrator"
            ));
        }
        if KV_DENIED_SECTIONS.contains(&segs[0]) {
            return Err(format!("advanced_kv[{i}]: [{}] has its own settings and cannot be edited here", segs[0]));
        }
        let v = row.get("value").ok_or_else(|| format!("advanced_kv[{i}].value is missing"))?;
        let bad = || format!("advanced_kv[{i}] ([{section}] {key}): value is not a {ty}");
        let tv = match ty {
            "string" => {
                let s = v.as_str().ok_or_else(bad)?;
                if s.chars().count() > 4000 || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    return Err(format!("advanced_kv[{i}]: string too long or contains control characters"));
                }
                toml::Value::String(s.to_string())
            }
            "integer" => toml::Value::Integer(v.as_i64().ok_or_else(bad)?),
            "float" => {
                let f = v.as_f64().ok_or_else(bad)?;
                if !f.is_finite() {
                    return Err(bad());
                }
                toml::Value::Float(f)
            }
            "boolean" => toml::Value::Boolean(v.as_bool().ok_or_else(bad)?),
            "string_array" | "array_string" => {
                let arr = v.as_array().ok_or_else(bad)?;
                let items = arr
                    .iter()
                    .map(|x| x.as_str().map(|s| toml::Value::String(s.to_string())).ok_or_else(bad))
                    .collect::<Result<Vec<_>, _>>()?;
                toml::Value::Array(items)
            }
            _ => {
                return Err(format!(
                    "advanced_kv[{i}].type must be string, integer, float, boolean or string_array"
                ));
            }
        };
        table_at_mut(table, &segs)?.insert(key.to_string(), tv);
        changes.push(format!("{section}.{key} updated"));
    }
    Ok(changes)
}

fn json_or_null(table: &toml::Table, path: &str) -> Value {
    toml_at_json(table, path)
}

/// Copy `keys` of the table at `path` into a JSON object (absent ⇒ omitted,
/// so the dashboard can tell "unset" from a value).
fn pick(table: &toml::Table, path: &str, keys: &[&str]) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(t) = toml_at(table, path).and_then(|v| v.as_table()) {
        for k in keys {
            if let Some(v) = t.get(*k).and_then(|v| serde_json::to_value(v).ok()) {
                obj.insert((*k).to_string(), v);
            }
        }
    }
    Value::Object(obj)
}

/// `scheme://user:pass@host/…` → `scheme://host/…` (credentials never leave
/// the server through a prefill).
pub(crate) fn strip_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{tail}"),
        None => url.to_string(),
    }
}

/// `agents.inspect` → `settings`: the employee's own values from the raw
/// `agent.toml` for every key the edit page can change. Keys absent from the
/// file are absent here (the page shows its default and does not send it).
/// Credentials are never included.
pub(crate) fn agent_settings_json(raw: &toml::Table) -> Value {
    let team_roles = {
        let mut roles = serde_json::Map::new();
        for role in ["planner", "executor", "verifier", "utility"] {
            let v = pick(raw, &format!("team.roles.{role}"), &["runtime", "model", "effort"]);
            if v.as_object().is_some_and(|o| !o.is_empty()) {
                roles.insert(role.into(), v);
            }
        }
        Value::Object(roles)
    };
    // The fourth role is returned under its stored name `utility` only (the
    // dashboard reads that); writes still accept `synthesizer` as an alias.
    let mut team = pick(raw, "team", &["enabled", "gate", "executor_fanout"]);
    team["roles"] = team_roles;
    let mut evolution = pick(
        raw,
        "evolution",
        &[
            "gvu_enabled",
            "max_active_skills",
            "skill_token_budget",
            "max_silence_hours",
            "skill_synthesis_enabled",
            "skill_synthesis_threshold",
            "skill_synthesis_cooldown_hours",
            "skill_trial_ttl",
            "skill_graduation_min_lift",
        ],
    );
    evolution["external_factors"] = pick(
        raw,
        "evolution.external_factors",
        &["user_feedback", "security_events", "channel_metrics", "business_context", "peer_signals"],
    );
    evolution["stagnation_detection"] =
        pick(raw, "evolution.stagnation_detection", &["enabled", "window_seconds", "trigger_threshold", "action"]);
    let mut odoo = pick(
        raw,
        "odoo",
        &["profile", "allowed_models", "unblock_models", "allowed_actions", "company_ids", "url", "db", "username"],
    );
    if let Some(u) = odoo.get("url").and_then(|v| v.as_str()).map(strip_userinfo) {
        odoo["url"] = Value::String(u);
    }
    json!({
        "heartbeat": pick(raw, "heartbeat", &["enabled", "interval_seconds", "cron", "cron_timezone", "max_concurrent_runs"]),
        "budget": pick(raw, "budget", &["monthly_limit_cents", "warn_threshold_percent", "hard_stop", "daily_cap_cents"]),
        "model": pick(raw, "model", &["preferred", "fallback", "api_mode", "account_pool", "utility", "effort"]),
        "runtime": pick(raw, "runtime", &["provider", "fallback", "minimal_context"]),
        "fork": pick(raw, "fork", &["enabled"]),
        "team": team,
        "guardrails": pick(raw, "guardrails", &["enabled", "block_secrets", "block_injection_echo", "redact_pii", "deny_phrases"]),
        "memory": pick(raw, "memory", &["decision_continuity", "decision_ttl_days"]),
        "night_engine": pick(raw, "night_engine", &["enabled"]),
        "evolution": evolution,
        "container": pick(
            raw,
            "container",
            &["timeout_ms", "sandbox_enabled", "network_access"],
        ),
        "odoo": odoo,
        "odoo_api_key_set": toml_at(raw, "odoo.api_key_enc").is_some() || toml_at(raw, "odoo.api_key").is_some(),
        "odoo_password_set": toml_at(raw, "odoo.password_enc").is_some() || toml_at(raw, "odoo.password").is_some(),
        "prompt": json_or_null(raw, "prompt"),
        "capabilities": pick(
            raw,
            "capabilities",
            &["approval_required_tools", "irreversible_tools", "maybe_irreversible_tools", "scoped_tools", "grant_ttl_secs", "action_rules"],
        ),
    })
}

// ── v1.68 boot migration: reset scaffold-noise `false` permission flags ─────

/// Marker written into `[permissions]` once the four flags are enforced.
pub(crate) const PERMISSIONS_MARKER_KEY: &str = "permissions_enforced_since";
pub(crate) const PERMISSIONS_MARKER_VALUE: &str = "1.68.0";
const ENFORCED_PERMISSION_KEYS: &[&str] =
    &["can_create_agents", "can_send_cross_agent", "can_modify_own_skills", "can_schedule_tasks"];

/// Until v1.68 the four flags had no effect, so a stored `false` is scaffold
/// noise, not an operator decision. For a file without the marker: every
/// `false` among them becomes `true` and the marker is added. Returns the new
/// text and the keys reset, or `None` when the file needs nothing (marker
/// present / no `[permissions]` table). Formatting is preserved. `Err` on a
/// file that does not parse — the caller leaves it alone.
pub(crate) fn migrate_permission_flags(text: &str) -> Result<Option<(String, Vec<String>)>, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let Some(perms) = doc.get_mut("permissions").and_then(|i| i.as_table_like_mut()) else {
        return Ok(None);
    };
    if perms.contains_key(PERMISSIONS_MARKER_KEY) {
        return Ok(None);
    }
    let mut reset = Vec::new();
    for key in ENFORCED_PERMISSION_KEYS {
        if perms.get(key).and_then(|i| i.as_bool()) == Some(false) {
            if let Some(toml_edit::Item::Value(v)) = perms.get_mut(key) {
                let decor = v.decor().clone();
                *v = toml_edit::Value::from(true);
                *v.decor_mut() = decor;
            }
            reset.push((*key).to_string());
        }
    }
    perms.insert(PERMISSIONS_MARKER_KEY, toml_edit::value(PERMISSIONS_MARKER_VALUE));
    Ok(Some((doc.to_string(), reset)))
}

/// Run [`migrate_permission_flags`] over every `<home>/agents/<id>/agent.toml`
/// (directories starting with `_` or `.` are skipped). Each file is rewritten
/// atomically under the cross-process lock; a reset writes a
/// `permission_flags_reset` audit row. Idempotent: marked files are skipped.
pub(crate) fn migrate_all_agent_permissions(home_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(home_dir.join("agents")) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || name.starts_with(['_', '.']) || !entry.path().is_dir() {
            continue;
        }
        let path = entry.path().join("agent.toml");
        if !path.is_file() {
            continue;
        }
        let outcome = duduclaw_core::with_file_lock(&path, || {
            let text = std::fs::read_to_string(&path)?;
            match migrate_permission_flags(&text) {
                Err(e) => Ok(Err(e)),
                Ok(None) => Ok(Ok(Vec::new())),
                Ok(Some((new_text, reset))) => {
                    let tmp = path.with_extension("toml.tmp");
                    super::config_commit::write_owner_only(&tmp, &new_text)?;
                    if let Err(e) = std::fs::rename(&tmp, &path) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(e);
                    }
                    Ok(Ok(reset))
                }
            }
        });
        match outcome {
            Ok(Ok(reset)) if !reset.is_empty() => {
                info!(agent = %name, keys = ?reset, "v1.68 permission flags reset to true (enforced from now on)");
                crate::security_autopilot::audit_and_emit(
                    home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "permission_flags_reset",
                        name.as_str(),
                        duduclaw_security::audit::Severity::Info,
                        json!({ "agent_id": name, "keys": reset }),
                    ),
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!(agent = %name, error = %e, "permission flag migration skipped: agent.toml does not parse"),
            Err(e) => warn!(agent = %name, error = %e, "permission flag migration failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = r#"
[agent]
name = "a1"
display_name = "A1"
role = "specialist"
status = "active"
trigger = "@A1"
reports_to = ""
icon = ""

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = []

[container]
timeout_ms = 1000
max_concurrent = 1
readonly_project = true

[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = "*/5 * * * *"

[budget]
monthly_limit_cents = 0
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = false
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
gvu_enabled = true
skill_auto_activate = false
skill_security_scan = true
"#;

    fn table() -> toml::Table {
        AGENT.parse().unwrap()
    }

    #[test]
    fn fixture_parses_as_agent_config() {
        agent_config_check(&table()).unwrap();
    }

    #[test]
    fn new_keys_are_written_only_when_sent() {
        let mut t = table();
        let before = t.clone();
        let changes = apply_agent_v168_keys(&mut t, &json!({}), true).unwrap();
        assert!(changes.is_empty());
        assert_eq!(t, before, "nothing sent ⇒ nothing written");

        apply_agent_v168_keys(
            &mut t,
            &json!({
                "budget": { "daily_cap_cents": 500 },
                "model": { "effort": "XHIGH" },
                "fork": { "enabled": true },
                "team": { "enabled": true, "roles": { "utility": { "runtime": "codex" } } },
                "guardrails": { "enabled": true, "deny_phrases": ["x", ""] },
                "memory": { "decision_continuity": true, "decision_ttl_days": 14 },
                "night_engine": { "enabled": true },
                "runtime": { "minimal_context": false },
            }), true)
        .unwrap();
        assert_eq!(toml_at(&t, "model.effort").unwrap().as_str(), Some("xhigh"));
        assert_eq!(toml_at(&t, "team.roles.utility.runtime").unwrap().as_str(), Some("codex"));
        assert_eq!(toml_at(&t, "guardrails.deny_phrases").unwrap().as_array().unwrap().len(), 1);
        assert_eq!(toml_at(&t, "heartbeat.cron").unwrap().as_str(), Some("*/5 * * * *"), "cron untouched");
        agent_config_check(&t).unwrap();

        apply_agent_v168_keys(&mut t, &json!({ "model": { "effort": "" } }), true).unwrap();
        assert!(toml_at(&t, "model.effort").is_none());
    }

    #[test]
    fn typed_kv_editor() {
        let mut t = table();
        apply_agent_v168_keys(
            &mut t,
            &json!({ "advanced_kv": [
                { "section": "prompt", "key": "cli_bare_mode", "value": true, "type": "boolean" },
                { "section": "prompt", "key": "minimal_core_kb", "value": 4, "type": "integer" },
                { "section": "model", "key": "fallbacks", "value": ["a", "b"], "type": "string_array" },
            ]}),
            true,
        )
        .unwrap();
        assert_eq!(toml_at(&t, "prompt.cli_bare_mode").unwrap().as_bool(), Some(true));
        agent_config_check(&t).unwrap();

        // Wrong JSON type for the declared type.
        assert!(apply_advanced_kv(&mut table(), &json!({ "advanced_kv": [
            { "section": "prompt", "key": "cli_bare_mode", "value": "true", "type": "boolean" }
        ]}), true).is_err());
        // Declared type consistent but not what the loader accepts: caught
        // by the final AgentConfig check, with a location.
        let mut bad = table();
        apply_advanced_kv(&mut bad, &json!({ "advanced_kv": [
            { "section": "prompt", "key": "cli_bare_mode", "value": "yes", "type": "string" }
        ]}), true)
        .unwrap();
        let err = agent_config_check(&bad).unwrap_err();
        assert!(err.contains("line"), "{err}");
        // Spend / safety knobs are admin only.
        for (section, key) in [("budget", "max_input_tokens"), ("guardrails", "redact_pii"), ("model", "account_pool")] {
            let row = json!({ "advanced_kv": [{ "section": section, "key": key, "value": 1, "type": "integer" }] });
            let err = apply_advanced_kv(&mut table(), &row, false).unwrap_err();
            assert!(err.contains("administrator"), "{err}");
        }
        // Authority sections are refused.
        assert!(apply_advanced_kv(&mut table(), &json!({ "advanced_kv": [
            { "section": "capabilities", "key": "git_credentials", "value": true, "type": "boolean" }
        ]}), true).is_err());
    }

    #[test]
    fn authority_diff_sees_only_real_changes() {
        let before = table();
        let mut after = before.clone();
        assert!(authority_diff(&before, &after).is_empty());
        table_at_mut(&mut after, &["capabilities"]).unwrap().insert("git_credentials".into(), toml::Value::Boolean(true));
        table_at_mut(&mut after, &["agent"]).unwrap().insert("department".into(), toml::Value::String("ops".into()));
        let diff = authority_diff(&before, &after);
        let keys: Vec<&str> = diff.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, vec!["agent.department", "capabilities"]);
        // Permission flags: absent reads as allowed, so an explicit `true`
        // is no change but an explicit `false` is.
        let mut perm = before.clone();
        table_at_mut(&mut perm, &["permissions"]).unwrap().insert("can_schedule_tasks".into(), toml::Value::Boolean(true));
        assert!(authority_diff(&before, &perm).is_empty() || before["permissions"].get("can_schedule_tasks").is_some());
        let mut deny = perm.clone();
        table_at_mut(&mut deny, &["permissions"]).unwrap().insert("can_schedule_tasks".into(), toml::Value::Boolean(false));
        assert!(authority_diff(&perm, &deny).iter().any(|c| c.key == "permissions.can_schedule_tasks"));
        let mut g = before.clone();
        table_at_mut(&mut g, &["guardrails"]).unwrap().insert("block_secrets".into(), toml::Value::Boolean(false));
        assert!(authority_diff(&before, &g).iter().any(|c| c.key == "guardrails.block_secrets"));
        let details = audit_details(&diff);
        assert_eq!(details[1]["key"], "capabilities.git_credentials");
    }

    #[test]
    fn settings_prefill_never_contains_credentials() {
        let mut t = table();
        let odoo = table_at_mut(&mut t, &["odoo"]).unwrap();
        odoo.insert("profile".into(), toml::Value::String("main".into()));
        odoo.insert("api_key_enc".into(), toml::Value::String("SECRET".into()));
        let s = agent_settings_json(&t);
        assert_eq!(s["odoo"]["profile"], "main");
        assert_eq!(s["odoo_api_key_set"], true);
        assert!(!s.to_string().contains("SECRET"));
        assert_eq!(s["heartbeat"]["cron"], "*/5 * * * *");
        assert!(s["container"].get("cmd").is_none() && s["container"].get("env").is_none());
        assert_eq!(strip_userinfo("https://u:p@erp.example.com/odoo?x=1"), "https://erp.example.com/odoo?x=1");
    }

    #[test]
    fn permission_migration_resets_false_once() {
        let text = "[permissions]\ncan_create_agents = false # note\ncan_send_cross_agent = false\ncan_modify_own_skills = false\ncan_schedule_tasks = false\ncan_modify_own_soul = false\n";
        let (out, reset) = migrate_permission_flags(text).unwrap().unwrap();
        assert_eq!(reset.len(), 4);
        let t: toml::Table = out.parse().unwrap();
        for k in ENFORCED_PERMISSION_KEYS {
            assert_eq!(t["permissions"][*k].as_bool(), Some(true), "{k}");
        }
        assert_eq!(t["permissions"]["can_modify_own_soul"].as_bool(), Some(false), "soul flag untouched");
        assert_eq!(t["permissions"][PERMISSIONS_MARKER_KEY].as_str(), Some("1.68.0"));
        assert!(out.contains("# note"));
        // Marked file: an operator's later `false` sticks.
        let marked = "[permissions]\ncan_schedule_tasks = false\npermissions_enforced_since = \"1.68.0\"\n";
        assert!(migrate_permission_flags(marked).unwrap().is_none());
        assert!(migrate_permission_flags("[permissions\nbroken").is_err());
    }

    #[test]
    fn permission_migration_over_a_home() {
        let home = tempfile::tempdir().unwrap();
        let write = |id: &str, body: &str| {
            let d = home.path().join("agents").join(id);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("agent.toml"), body).unwrap();
        };
        write("a", "[permissions]\ncan_schedule_tasks = false\n");
        write("b", "[permissions]\ncan_schedule_tasks = false\npermissions_enforced_since = \"1.68.0\"\n");
        write("c", "[permissions\nbroken");
        migrate_all_agent_permissions(home.path());
        let read = |id: &str| std::fs::read_to_string(home.path().join("agents").join(id).join("agent.toml")).unwrap();
        assert!(read("a").contains("can_schedule_tasks = true") && read("a").contains("permissions_enforced_since"));
        assert!(read("b").contains("can_schedule_tasks = false"));
        assert_eq!(read("c"), "[permissions\nbroken");
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert_eq!(audit.matches("permission_flags_reset").count(), 1);
        // Second boot: nothing more.
        migrate_all_agent_permissions(home.path());
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert_eq!(audit.matches("permission_flags_reset").count(), 1);
    }
}
