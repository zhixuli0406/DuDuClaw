//! v1.68.0 — the keys `system.update_config` gained from the dashboard
//! switch audit (`commercial/docs/TODO-dashboard-switches-v1.68-2026-10.md`,
//! RPC contract table).
//!
//! Pure: [`apply_v168_keys`] mutates a parsed `config.toml` table and reports
//! what it did; the handler owns IO, the commit lock, the audit rows and the
//! hot reloads. Every parameter is named after its TOML path and may be sent
//! nested or flat (see `config_commit::param_at`). A present parameter with
//! the wrong type or an out-of-range value rejects the whole update.
//!
//! Hot vs restart, as verified against each reader (cited in the report):
//! everything here is re-read per use except `telemetry.otlp_endpoint`
//! (OTLP exporter built once at logger init) and `[tick]` (source tasks
//! spawned at boot — respawned by the handler when the tick runtime is
//! installed, otherwise reported in `restart_required`).

#[allow(unused_imports)]
use super::*;

use super::config_commit::{
    bool_param, is_plain_text, is_secret_placeholder, param_at, str_array_param, str_param,
    table_at_mut, toml_at, toml_at_json, u64_param,
};

/// A change to a key that widens what the system may do. Each one becomes a
/// `config_protected_key_changed` security-audit row after the commit.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProtectedChange {
    pub key: String,
    pub before: Value,
    pub after: Value,
}

/// What [`apply_v168_keys`] did to the table.
#[derive(Debug, Default)]
pub(crate) struct V168Outcome {
    pub changes: Vec<String>,
    /// Keys whose new value takes effect only after a gateway restart.
    pub restart_required: Vec<String>,
    /// At least one key whose reader re-reads `config.toml` per use.
    pub applied_immediate: bool,
    /// A `[tick]` key changed: the handler respawns the tick source tasks.
    pub reload_ticks: bool,
    /// `[redaction] purge_after_expire_days` changed: the handler rebuilds
    /// the redaction pipeline (which restarts the vault GC with it).
    pub reload_redaction: bool,
    pub protected: Vec<ProtectedChange>,
}

/// Keys whose change is audited as `config_protected_key_changed`: each
/// widens delegation, execution or memory-trust reach.
const PROTECTED_KEYS: &[&str] = &[
    "acp.trusted",
    "tick.allow_command_sources",
    "container.sandbox.when_unavailable",
    "container.sandbox.script_when_unavailable",
    "memory.supersession_trust_guard",
    // 2026-10: turning the action review off removes a gate.
    "action_review.mode",
];

/// Team role slots. The dashboard contract says `synthesizer`; the config
/// key (`TeamRoles`) is `utility` — both spellings land on `utility`.
const TEAM_ROLES: &[(&str, &str)] = &[
    ("planner", "planner"),
    ("executor", "executor"),
    ("verifier", "verifier"),
    ("utility", "utility"),
    ("synthesizer", "utility"),
];

const SANDBOX_INT_KEYS: &[&str] = &[
    "memory_bytes",
    "pids",
    "cpu_millis",
    "tmp_bytes",
    "workspace_bytes",
    "max_turns",
];

/// Translate nested / contract spellings of the pre-1.68 parameters into the
/// flat names the original handler reads (`general.log_level` → `log_level`,
/// `rotation.strategy` → `rotation_strategy`, `logging.format` →
/// `log_format` …). A flat name already present wins. Returns a new value.
pub(crate) fn normalize_legacy_aliases(params: &Value) -> Value {
    const ALIASES: &[(&str, &str)] = &[
        ("general.log_level", "log_level"),
        ("general.name", "name"),
        ("general.default_agent", "default_agent"),
        ("general.inference_mode", "inference_mode"),
        ("general.default_language", "default_language"),
        ("rotation.strategy", "rotation_strategy"),
        ("rotation.cooldown_after_rate_limit_seconds", "cooldown_after_rate_limit_seconds"),
        ("rotation.health_check_interval_seconds", "health_check_interval_seconds"),
        ("gateway.bind", "bind"),
        ("gateway.port", "port"),
        ("gateway.auth_token", "auth_token"),
        ("gateway.allowed_origins", "allowed_origins"),
        ("gateway.auto_update", "auto_update"),
        ("server.mdns_advertise", "mdns_advertise"),
        ("notify.daily_digest", "daily_digest"),
        ("notify.daily_digest_at", "daily_digest_at"),
        ("memory.novelty_gate", "novelty_gate_enabled"),
        ("miniapp.enabled", "miniapp_enabled"),
        ("skills.gap_digest_enabled", "gap_digest_enabled"),
        ("logging.format", "log_format"),
    ];
    let Some(obj) = params.as_object() else {
        return params.clone();
    };
    let mut out = obj.clone();
    for (path, flat) in ALIASES {
        if out.contains_key(*flat) {
            continue;
        }
        if let Some(v) = param_at(params, path) {
            // `logging.format` is `plain|json` in the contract; the original
            // handler (and the reader, which treats non-json as plain) spells
            // the plain value `pretty`.
            let v = match (*flat, v.as_str()) {
                ("log_format", Some("plain")) => Value::String("pretty".into()),
                _ => v.clone(),
            };
            out.insert((*flat).to_string(), v);
        }
    }
    Value::Object(out)
}

/// Keys among the pre-1.68 parameters that only take effect after a restart,
/// as `(flat param, TOML key)`. `log_level` is handled by the caller: it is
/// hot when the reload handle applies it.
pub(crate) const LEGACY_RESTART_KEYS: &[(&str, &str)] = &[
    ("bind", "gateway.bind"),
    ("port", "gateway.port"),
    ("auth_token", "gateway.auth_token"),
    ("health_check_interval_seconds", "rotation.health_check_interval_seconds"),
    // The name is read per request by `/healthz` and the picker, but the
    // mDNS advertisement is built once at boot.
    ("name", "general.name"),
    ("mdns_advertise", "server.mdns_advertise"),
    ("log_format", "logging.format"),
];

/// Apply every v1.68 contract key present in `params` to `table`.
pub(crate) fn apply_v168_keys(table: &mut toml::Table, params: &Value) -> Result<V168Outcome, String> {
    let before = table.clone();
    let mut out = V168Outcome::default();

    apply_takeover(table, params, &mut out)?;
    apply_mail(table, params, &mut out)?;
    apply_webchat(table, params, &mut out)?;
    apply_tick(table, params, &mut out)?;
    apply_files(table, params, &mut out)?;
    apply_simple_bools(table, params, &mut out)?;
    apply_judge_model(table, params, &mut out)?;
    apply_telemetry(table, params, &mut out)?;
    apply_sandbox(table, params, &mut out)?;
    apply_computer_use(table, params, &mut out)?;
    apply_team(table, params, &mut out)?;
    apply_redaction_purge(table, params, &mut out)?;

    for key in PROTECTED_KEYS {
        let (b, a) = (toml_at_json(&before, key), toml_at_json(table, key));
        if b != a {
            out.protected.push(ProtectedChange { key: (*key).to_string(), before: b, after: a });
        }
    }
    Ok(out)
}

fn set_bool(table: &mut toml::Table, path: &[&str], key: &str, v: bool, out: &mut V168Outcome) -> Result<(), String> {
    table_at_mut(table, path)?.insert(key.into(), toml::Value::Boolean(v));
    out.changes.push(format!("{}.{key} = {v}", path.join(".")));
    Ok(())
}

fn set_int(table: &mut toml::Table, path: &[&str], key: &str, v: u64, out: &mut V168Outcome) -> Result<(), String> {
    let n = i64::try_from(v).map_err(|_| format!("{}.{key} is too large", path.join(".")))?;
    table_at_mut(table, path)?.insert(key.into(), toml::Value::Integer(n));
    out.changes.push(format!("{}.{key} = {v}", path.join(".")));
    Ok(())
}

/// `""` removes the key; anything else is written verbatim.
fn set_or_clear_str(table: &mut toml::Table, path: &[&str], key: &str, v: &str, out: &mut V168Outcome) -> Result<(), String> {
    let section = table_at_mut(table, path)?;
    if v.is_empty() {
        section.remove(key);
        out.changes.push(format!("{}.{key} cleared", path.join(".")));
    } else {
        section.insert(key.into(), toml::Value::String(v.into()));
        out.changes.push(format!("{}.{key} = \"{v}\"", path.join(".")));
    }
    Ok(())
}

fn apply_takeover(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    let cap = duduclaw_core::takeover_state::HARD_MAX_DURATION_MINUTES as u64;
    if let Some(v) = bool_param(params, "takeover.enabled")? {
        set_bool(table, &["takeover"], "enabled", v, out)?;
        out.applied_immediate = true;
    }
    for key in ["duration_minutes", "max_duration_minutes"] {
        if let Some(v) = u64_param(params, &format!("takeover.{key}"), 1, cap)? {
            set_int(table, &["takeover"], key, v, out)?;
            out.applied_immediate = true;
        }
    }
    let dur = toml_at(table, "takeover.duration_minutes").and_then(|v| v.as_integer());
    let max = toml_at(table, "takeover.max_duration_minutes").and_then(|v| v.as_integer());
    if let (Some(d), Some(m)) = (dur, max)
        && d > m
    {
        return Err(format!(
            "takeover.duration_minutes ({d}) must not exceed takeover.max_duration_minutes ({m})"
        ));
    }
    Ok(())
}

fn apply_mail(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    for key in ["enabled", "gmail_enabled", "dropfolder_enabled", "auto_trigger"] {
        if let Some(v) = bool_param(params, &format!("mail.{key}"))? {
            set_bool(table, &["mail"], key, v, out)?;
            out.applied_immediate = true;
        }
    }
    if let Some(v) = str_param(params, "mail.default_agent")? {
        let v = v.trim();
        if !v.is_empty() && !duduclaw_core::is_valid_agent_id(v) {
            return Err(format!("mail.default_agent '{v}' is not a valid agent id"));
        }
        set_or_clear_str(table, &["mail"], "default_agent", v, out)?;
        out.applied_immediate = true;
    }
    Ok(())
}

/// The widget key ships inside the public page, so it is not a secret in the
/// usual sense, but it is never echoed back either: the dashboard gets only
/// whether one is set.
fn apply_webchat(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    if let Some(v) = str_param(params, "webchat.widget_key")? {
        let v = v.trim();
        if is_secret_placeholder(v) {
            // untouched
        } else if v.is_empty() {
            table_at_mut(table, &["webchat"])?.remove("widget_key");
            out.changes.push("webchat.widget_key cleared".into());
            out.applied_immediate = true;
        } else {
            if v.len() < 16 || v.len() > 256 {
                return Err("webchat.widget_key must be 16-256 characters".into());
            }
            if !v.chars().all(|c| c.is_ascii_graphic()) {
                return Err("webchat.widget_key may contain only visible ASCII characters (no spaces)".into());
            }
            table_at_mut(table, &["webchat"])?.insert("widget_key".into(), toml::Value::String(v.into()));
            out.changes.push("webchat.widget_key = [SET]".into());
            out.applied_immediate = true;
        }
    }
    if let Some(v) = bool_param(params, "webchat.public_widget")? {
        set_bool(table, &["webchat"], "public_widget", v, out)?;
        out.applied_immediate = true;
    }
    let widget_on = toml_at(table, "webchat.public_widget").and_then(|v| v.as_bool()) == Some(true);
    let key_ok = toml_at(table, "webchat.widget_key")
        .and_then(|v| v.as_str())
        .is_some_and(|k| k.len() >= 16);
    let touched = param_at(params, "webchat.public_widget").is_some()
        || param_at(params, "webchat.widget_key").is_some();
    if touched && widget_on && !key_ok {
        return Err("webchat.public_widget needs a widget_key of at least 16 characters".into());
    }
    Ok(())
}

fn apply_tick(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    for key in ["enabled", "allow_command_sources"] {
        if let Some(v) = bool_param(params, &format!("tick.{key}"))? {
            set_bool(table, &["tick"], key, v, out)?;
            out.reload_ticks = true;
        }
    }
    if let Some(v) = str_param(params, "tick.preset")? {
        let v = v.trim();
        if !v.is_empty() && crate::tick_config::TickPreset::parse(v).is_none() {
            return Err("tick.preset must be \"conservative\", \"aggressive\" or \"\" (none)".into());
        }
        set_or_clear_str(table, &["tick"], "preset", v, out)?;
        out.reload_ticks = true;
    }
    if let Some(v) = u64_param(params, "tick.dns_ttl_secs", 0, 86_400)? {
        set_int(table, &["tick"], "dns_ttl_secs", v, out)?;
        out.reload_ticks = true;
    }
    Ok(())
}

const MAX_ALLOWED_ROOTS: usize = 64;

/// `[files] allowed_roots`: absolute paths only (the reader drops relative
/// ones silently, so they are refused here), never a filesystem root.
fn apply_files(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    let Some(roots) = str_array_param(params, "files.allowed_roots")? else {
        return Ok(());
    };
    if roots.len() > MAX_ALLOWED_ROOTS {
        return Err(format!("files.allowed_roots supports at most {MAX_ALLOWED_ROOTS} entries"));
    }
    let mut cleaned: Vec<String> = Vec::with_capacity(roots.len());
    for r in roots {
        if r.is_empty() {
            continue;
        }
        let p = std::path::Path::new(&r);
        if r.len() > 4096 || r.contains('\0') || !p.is_absolute() {
            return Err(format!("files.allowed_roots entry '{r}' must be an absolute path"));
        }
        if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("files.allowed_roots entry '{r}' may not contain `..`"));
        }
        // A symlink (or `/./`) can still resolve to the root.
        let canonical_root = std::fs::canonicalize(p).ok().is_some_and(|c| c.parent().is_none());
        if p.parent().is_none() || canonical_root {
            return Err(format!("files.allowed_roots entry '{r}' is a filesystem root, which would expose every file"));
        }
        if !cleaned.contains(&r) {
            cleaned.push(r);
        }
    }
    let n = cleaned.len();
    table_at_mut(table, &["files"])?.insert(
        "allowed_roots".into(),
        toml::Value::Array(cleaned.into_iter().map(toml::Value::String).collect()),
    );
    out.changes.push(format!("files.allowed_roots = [{n} entries]"));
    out.applied_immediate = true;
    Ok(())
}

/// Plain booleans whose readers re-read `config.toml` per use.
fn apply_simple_bools(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    const KEYS: &[(&str, &[&str], &str)] = &[
        ("night.llm_enabled", &["night"], "llm_enabled"),
        ("acp.trusted", &["acp"], "trusted"),
        ("integrations.github", &["integrations"], "github"),
        // P2-A: both default off, both re-read on every driver tick / RPC.
        ("responsibilities.enabled", &["responsibilities"], "enabled"),
        ("goal_loop.steering_enabled", &["goal_loop"], "steering_enabled"),
    ];
    for (path, section, key) in KEYS {
        if let Some(v) = bool_param(params, path)? {
            set_bool(table, section, key, v, out)?;
            out.applied_immediate = true;
        }
    }
    // 2026-10: `[action_review] mode`, read on every tool call.
    if let Some(v) = str_param(params, "action_review.mode")? {
        let v = v.trim().to_ascii_lowercase();
        if !matches!(v.as_str(), "off" | "shadow" | "enforce") {
            return Err("action_review.mode must be one of: off, shadow, enforce".into());
        }
        table_at_mut(table, &["action_review"])?.insert("mode".into(), toml::Value::String(v.clone()));
        out.changes.push(format!("action_review.mode = \"{v}\""));
        out.applied_immediate = true;
    }
    // Read when a memory engine is built, i.e. by the next agent session.
    if let Some(v) = bool_param(params, "memory.supersession_trust_guard")? {
        table_at_mut(table, &["memory"])?.insert("supersession_trust_guard".into(), toml::Value::Boolean(v));
        out.changes.push(format!("memory.supersession_trust_guard = {v} (applies to new sessions)"));
    }
    Ok(())
}

fn apply_judge_model(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    if let Some(v) = str_param(params, "dispatch.judge_provider")? {
        let v = v.trim();
        let canonical = if v.is_empty() {
            String::new()
        } else {
            duduclaw_core::types::RuntimeType::parse(v)
                .map(|rt| rt.as_str().to_string())
                .ok_or_else(|| {
                    format!(
                        "dispatch.judge_provider '{v}' is not a runtime id. Valid: {}",
                        duduclaw_core::types::RuntimeType::valid_values()
                    )
                })?
        };
        set_or_clear_str(table, &["dispatch"], "judge_provider", &canonical, out)?;
        out.applied_immediate = true;
    }
    if let Some(v) = str_param(params, "dispatch.judge_model")? {
        let v = v.trim();
        if !v.is_empty() && !is_plain_text(v, 200) {
            return Err("dispatch.judge_model must be at most 200 characters with no control characters".into());
        }
        set_or_clear_str(table, &["dispatch"], "judge_model", v, out)?;
        out.applied_immediate = true;
    }
    Ok(())
}

fn apply_telemetry(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    let Some(v) = str_param(params, "telemetry.otlp_endpoint")? else {
        return Ok(());
    };
    let v = v.trim();
    if !v.is_empty() {
        let ok = url::Url::parse(v)
            .map(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .unwrap_or(false);
        if !ok || v.len() > 2048 {
            return Err("telemetry.otlp_endpoint must be an http(s) URL".into());
        }
    }
    set_or_clear_str(table, &["telemetry"], "otlp_endpoint", v, out)?;
    out.restart_required.push("telemetry.otlp_endpoint".into());
    Ok(())
}

/// `[container.sandbox]` is validated as a whole section by the same parser
/// the task sandbox uses per task (`task_sandbox::settings::parse`), after the
/// incoming keys are applied — so `tmp_bytes + workspace_bytes <=
/// memory_bytes` and every bound is checked against the resulting section,
/// not key by key. The two escape hatches are additionally held to their two
/// legal values here (the runtime parser degrades an unknown value to
/// `fail` with a warning; a dashboard write should never rely on that).
fn apply_sandbox(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    let mut touched = false;
    if let Some(v) = str_param(params, "container.sandbox.image")? {
        let v = v.trim();
        set_or_clear_str(table, &["container", "sandbox"], "image", v, out)?;
        touched = true;
    }
    for key in ["when_unavailable", "script_when_unavailable"] {
        if let Some(v) = str_param(params, &format!("container.sandbox.{key}"))? {
            if !matches!(v, "fail" | "run_unsandboxed") {
                return Err(format!("container.sandbox.{key} must be \"fail\" or \"run_unsandboxed\""));
            }
            set_or_clear_str(table, &["container", "sandbox"], key, v, out)?;
            touched = true;
        }
    }
    for key in SANDBOX_INT_KEYS {
        if let Some(v) = u64_param(params, &format!("container.sandbox.{key}"), 1, u64::MAX >> 1)? {
            set_int(table, &["container", "sandbox"], key, v, out)?;
            touched = true;
        }
    }
    if touched {
        crate::task_sandbox::settings::parse(table).map_err(|e| format!("invalid sandbox settings: {e}"))?;
        out.applied_immediate = true;
    }
    Ok(())
}

fn apply_computer_use(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    let Some(v) = str_param(params, "computer_use.image")? else {
        return Ok(());
    };
    set_or_clear_str(table, &["computer_use"], "image", v.trim(), out)?;
    crate::computer_use_image::parse(table).map_err(|e| format!("invalid computer_use.image: {e}"))?;
    out.applied_immediate = true;
    Ok(())
}

/// `[team]` — read per task (`team_composer::read_global_team_config`), so a
/// change applies to the next goal. Field-level validation only: whether the
/// resulting roles form a valid team (family de-correlation) is decided per
/// task and a refusal there falls back to Solo, which is not a write error.
fn apply_team(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    if let Some(v) = bool_param(params, "team.enabled")? {
        set_bool(table, &["team"], "enabled", v, out)?;
        out.applied_immediate = true;
    }
    if let Some(v) = str_param(params, "team.gate")? {
        let v = v.trim();
        let canonical = if v.is_empty() {
            String::new()
        } else {
            v.parse::<duduclaw_core::types::TeamGateMode>()
                .map(|m| m.as_str().to_string())
                .map_err(|_| "team.gate must be \"auto\", \"always_solo\" or \"always_team\"".to_string())?
        };
        set_or_clear_str(table, &["team"], "gate", &canonical, out)?;
        out.applied_immediate = true;
    }
    apply_team_roles(table, params, "team.roles", &["team", "roles"], out)
}

/// Shared by the global `[team.roles]` and the per-agent one.
pub(crate) fn apply_team_roles(
    table: &mut toml::Table,
    params: &Value,
    param_prefix: &str,
    table_path: &[&str],
    out: &mut V168Outcome,
) -> Result<(), String> {
    for (wire, key) in TEAM_ROLES {
        let base = format!("{param_prefix}.{wire}");
        let mut path: Vec<&str> = table_path.to_vec();
        path.push(key);
        if let Some(v) = str_param(params, &format!("{base}.runtime"))? {
            let v = v.trim();
            let canonical = if v.is_empty() {
                String::new()
            } else {
                duduclaw_core::types::RuntimeType::parse(v)
                    .map(|rt| rt.as_str().to_string())
                    .ok_or_else(|| format!("{base}.runtime '{v}' is not a runtime id"))?
            };
            set_or_clear_str(table, &path, "runtime", &canonical, out)?;
            out.applied_immediate = true;
        }
        if let Some(v) = str_param(params, &format!("{base}.model"))? {
            let v = v.trim();
            if !v.is_empty() && !is_plain_text(v, 200) {
                return Err(format!("{base}.model must be at most 200 characters"));
            }
            set_or_clear_str(table, &path, "model", v, out)?;
            out.applied_immediate = true;
        }
        if let Some(v) = str_param(params, &format!("{base}.effort"))? {
            let v = v.trim();
            let canonical = if v.is_empty() {
                String::new()
            } else {
                v.parse::<duduclaw_core::effort::Effort>()
                    .map(|e| e.as_str().to_string())
                    .map_err(|_| format!("{base}.effort must be low, medium, high, xhigh or max"))?
            };
            set_or_clear_str(table, &path, "effort", &canonical, out)?;
            out.applied_immediate = true;
        }
        prune_empty_table(table, &path);
    }
    prune_empty_table(table, table_path);
    Ok(())
}

/// Remove the table at `path` when it ended up empty (a cleared role must not
/// leave `[team.roles.planner]` behind).
fn prune_empty_table(table: &mut toml::Table, path: &[&str]) {
    let Some((last, parents)) = path.split_last() else { return };
    let mut cur = table;
    for seg in parents {
        match cur.get_mut(*seg).and_then(|v| v.as_table_mut()) {
            Some(t) => cur = t,
            None => return,
        }
    }
    if cur.get(*last).and_then(|v| v.as_table()).is_some_and(|t| t.is_empty()) {
        cur.remove(*last);
    }
}

fn apply_redaction_purge(table: &mut toml::Table, params: &Value, out: &mut V168Outcome) -> Result<(), String> {
    if let Some(v) = u64_param(params, "redaction.purge_after_expire_days", 0, 3650)? {
        set_int(table, &["redaction"], "purge_after_expire_days", v, out)?;
        out.reload_redaction = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(params: Value) -> Result<(toml::Table, V168Outcome), String> {
        let mut t = toml::Table::new();
        let o = apply_v168_keys(&mut t, &params)?;
        Ok((t, o))
    }

    #[test]
    fn takeover_and_mail_round_trip() {
        let (t, o) = apply(json!({
            "takeover": { "enabled": true, "duration_minutes": 30, "max_duration_minutes": 120 },
            "mail.default_agent": "dudu",
            "mail": { "enabled": true, "auto_trigger": false },
        }))
        .unwrap();
        assert_eq!(toml_at(&t, "takeover.duration_minutes").unwrap().as_integer(), Some(30));
        assert_eq!(toml_at(&t, "mail.default_agent").unwrap().as_str(), Some("dudu"));
        assert!(o.applied_immediate && o.restart_required.is_empty());
    }

    #[test]
    fn takeover_duration_above_max_is_refused() {
        assert!(apply(json!({"takeover": {"duration_minutes": 200, "max_duration_minutes": 100}})).is_err());
        assert!(apply(json!({"takeover": {"duration_minutes": 0}})).is_err());
        assert!(apply(json!({"takeover": {"enabled": "yes"}})).is_err());
    }

    #[test]
    fn widget_key_rules() {
        assert!(apply(json!({"webchat": {"widget_key": "short"}})).is_err());
        assert!(apply(json!({"webchat": {"public_widget": true}})).is_err(), "widget without key");
        let (t, o) = apply(json!({"webchat": {"public_widget": true, "widget_key": "0123456789abcdef"}})).unwrap();
        assert_eq!(toml_at(&t, "webchat.public_widget").unwrap().as_bool(), Some(true));
        assert!(o.changes.iter().all(|c| !c.contains("0123456789abcdef")), "key never in changes");
        // The placeholder leaves the stored key alone.
        let mut t2 = t.clone();
        apply_v168_keys(&mut t2, &json!({"webchat": {"widget_key": "«set»"}})).unwrap();
        assert_eq!(toml_at(&t2, "webchat.widget_key").unwrap().as_str(), Some("0123456789abcdef"));
    }

    #[test]
    fn allowed_roots_must_be_absolute_and_not_root() {
        assert!(apply(json!({"files": {"allowed_roots": ["relative/dir"]}})).is_err());
        assert!(apply(json!({"files": {"allowed_roots": ["/"]}})).is_err());
        assert!(apply(json!({"files": {"allowed_roots": ["/srv/../etc"]}})).is_err());
        assert!(apply(json!({"files": {"allowed_roots": ["/./"]}})).is_err());
        // `/srv/a` has no drive letter, so it is not absolute on Windows.
        let abs = if cfg!(windows) { r"C:\srv\a" } else { "/srv/a" };
        let (t, _) = apply(json!({"files": {"allowed_roots": [abs, abs, ""]}})).unwrap();
        assert_eq!(toml_at(&t, "files.allowed_roots").unwrap().as_array().unwrap().len(), 1);
        if cfg!(windows) {
            assert!(apply(json!({"files": {"allowed_roots": [r"C:\"]}})).is_err(), "drive root");
        }
    }

    #[test]
    fn tick_keys_request_a_respawn() {
        let (t, o) = apply(json!({"tick": {"enabled": true, "preset": "conservative", "dns_ttl_secs": 0}})).unwrap();
        assert!(o.reload_ticks);
        assert_eq!(toml_at(&t, "tick.preset").unwrap().as_str(), Some("conservative"));
        assert!(apply(json!({"tick": {"preset": "turbo"}})).is_err());
        let (t, _) = apply(json!({"tick": {"preset": ""}})).unwrap();
        assert!(toml_at(&t, "tick.preset").is_none());
    }

    #[test]
    fn sandbox_section_is_validated_as_a_whole() {
        // tmp + workspace above memory
        assert!(apply(json!({"container": {"sandbox": {"memory_bytes": 1000, "tmp_bytes": 600, "workspace_bytes": 600}}})).is_err());
        assert!(apply(json!({"container.sandbox.when_unavailable": "maybe"})).is_err());
        assert!(apply(json!({"container.sandbox.image": "--privileged"})).is_err());
        let (t, o) = apply(json!({"container": {"sandbox": {"when_unavailable": "run_unsandboxed", "pids": 64}}})).unwrap();
        assert_eq!(toml_at(&t, "container.sandbox.pids").unwrap().as_integer(), Some(64));
        assert_eq!(o.protected.len(), 1);
        assert_eq!(o.protected[0].key, "container.sandbox.when_unavailable");
    }

    #[test]
    fn acp_trusted_is_protected_and_unchanged_value_is_not() {
        let (mut t, o) = apply(json!({"acp": {"trusted": true}})).unwrap();
        assert_eq!(o.protected, vec![ProtectedChange { key: "acp.trusted".into(), before: Value::Null, after: json!(true) }]);
        let o2 = apply_v168_keys(&mut t, &json!({"acp": {"trusted": true}})).unwrap();
        assert!(o2.protected.is_empty());
    }

    #[test]
    fn judge_provider_is_canonicalised_or_refused() {
        let (t, _) = apply(json!({"dispatch": {"judge_provider": "agy", "judge_model": "x"}})).unwrap();
        assert_eq!(toml_at(&t, "dispatch.judge_provider").unwrap().as_str(), Some("antigravity"));
        assert!(apply(json!({"dispatch": {"judge_provider": "not-a-runtime"}})).is_err());
    }

    #[test]
    fn telemetry_is_restart_only() {
        let (_, o) = apply(json!({"telemetry": {"otlp_endpoint": "http://127.0.0.1:4317"}})).unwrap();
        assert_eq!(o.restart_required, vec!["telemetry.otlp_endpoint".to_string()]);
        assert!(apply(json!({"telemetry": {"otlp_endpoint": "ftp://x"}})).is_err());
    }

    #[test]
    fn team_roles_accept_synthesizer_and_clear_cleanly() {
        let (mut t, _) = apply(json!({"team": {"enabled": true, "gate": "always-solo",
            "roles": {"synthesizer": {"runtime": "claude", "effort": "HIGH"}}}}))
        .unwrap();
        assert_eq!(toml_at(&t, "team.gate").unwrap().as_str(), Some("always_solo"));
        assert_eq!(toml_at(&t, "team.roles.utility.effort").unwrap().as_str(), Some("high"));
        apply_v168_keys(&mut t, &json!({"team": {"roles": {"utility": {"runtime": "", "effort": ""}}}})).unwrap();
        assert!(toml_at(&t, "team.roles").is_none(), "{t:?}");
        assert!(apply(json!({"team": {"roles": {"planner": {"effort": "huge"}}}})).is_err());
    }

    #[test]
    fn aliases_map_contract_names_to_legacy_params() {
        let p = normalize_legacy_aliases(&json!({
            "general": {"log_level": "info"},
            "rotation": {"strategy": "failover"},
            "logging": {"format": "plain"},
            "rotation_strategy": "priority"
        }));
        assert_eq!(p["log_level"], "info");
        assert_eq!(p["rotation_strategy"], "priority", "flat name wins");
        assert_eq!(p["log_format"], "pretty");
    }
}
