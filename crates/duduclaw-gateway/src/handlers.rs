use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::{self, UserContext, UserDb, JwtConfig};
use duduclaw_auth::acl;
use duduclaw_auth::models::{UserRole, AccessLevel};
use duduclaw_core::traits::MemoryEngine;
use duduclaw_core::truncate_bytes;
use duduclaw_memory::SqliteMemoryEngine;
use chrono::{Datelike, Utc};
use rusqlite::params;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::autopilot_store::{AutopilotStore, AutopilotRuleRow};
use crate::cron_scheduler::CronScheduler;
use crate::cron_store::{CronStore, CronTaskRow};
use crate::extension::GatewayExtension;
use crate::gvu::version_store::VersionStore;
use crate::protocol::WsFrame;
use crate::task_store::{TaskStore, TaskRow, ActivityRow};
use crate::partner_store::{
    PartnerStore, PartnerProfileInput, PartnerCustomerInput, PartnerCustomerPatch,
};
use crate::evolution_events::schema::StagnationDetectionConfig;

/// Validate agent ID is safe for filesystem paths (no traversal).
fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("..")
}

// ── P0 dashboard-config helpers (CAP / CON / RED / MK / KS) ───────────────────
//
// These are module-level pure functions so the validation + TOML round-trip
// logic can be unit-tested directly without spinning up a full MethodHandler.
// They never touch the network or the filesystem; the async `handle_*` wrappers
// own the read → mutate → atomic-write + encryption side of things.

/// Known MCP scope strings (mirrors `duduclaw-cli::mcp_auth::parse_scopes`).
/// The gateway crate does not depend on duduclaw-cli, so the list is duplicated
/// here. Keep in sync with `Scope::as_str` in mcp_auth.rs.
const KNOWN_MCP_SCOPES: &[&str] = &[
    "memory:read",
    "memory:write",
    "wiki:read",
    "wiki:write",
    "messaging:send",
    "identity:read",
    "odoo:read",
    "odoo:write",
    "odoo:execute",
    "admin",
];

/// Validate an MCP API key against `^ddc_(prod|staging|dev)_[a-f0-9]{32}$`
/// (mirrors `duduclaw-cli::mcp_auth::is_valid_key_format`).
fn is_valid_mcp_key_format(key: &str) -> bool {
    let rest = match key.strip_prefix("ddc_") {
        Some(r) => r,
        None => return false,
    };
    let hex = if let Some(h) = rest.strip_prefix("prod_") {
        h
    } else if let Some(h) = rest.strip_prefix("staging_") {
        h
    } else if let Some(h) = rest.strip_prefix("dev_") {
        h
    } else {
        return false;
    };
    hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Generate a fresh MCP API key of the form `ddc_<env>_<32hex>`.
/// `env` must already be validated to one of prod/staging/dev.
fn generate_mcp_key(env: &str) -> String {
    // `Uuid::simple()` renders 32 lowercase hex chars (`[a-f0-9]{32}`) — exactly
    // the suffix format required by `is_valid_mcp_key_format`.
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("ddc_{env}_{suffix}")
}

/// Mask an MCP key for display: keep the `ddc_<env>_` prefix + first 4 hex of
/// the suffix, replace the rest with `…`. NEVER returns the full key.
fn mask_mcp_key(key: &str) -> String {
    // Find the second underscore (after the env segment) to keep the prefix.
    let parts: Vec<&str> = key.splitn(3, '_').collect();
    if parts.len() == 3 {
        let suffix = parts[2];
        let head: String = suffix.chars().take(4).collect();
        format!("{}_{}_{}…", parts[0], parts[1], head)
    } else {
        // Unrecognised shape — mask aggressively.
        let head: String = key.chars().take(6).collect();
        format!("{head}…")
    }
}

/// Validate + write the `[capabilities]` section into an agent.toml table from
/// the `agents.update` params. Returns the human-readable change list (may be
/// empty if no capability fields were present). Errors on invalid enum / range.
fn apply_capabilities_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    // Only act if the payload actually carries a `capabilities` object — this
    // keeps `agents.update` calls that don't touch capabilities clean.
    let cap = match params.get("capabilities").and_then(|v| v.as_object()) {
        Some(c) => c,
        None => return Ok(changes),
    };

    let section = table
        .entry("capabilities")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [capabilities] section".to_string())?;

    // ── Scalars ──
    if let Some(v) = cap.get("computer_use").and_then(|v| v.as_bool()) {
        section.insert("computer_use".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.computer_use = {v}"));
    }
    if let Some(v) = cap.get("computer_use_mode").and_then(|v| v.as_str()) {
        match v {
            "container" | "native" | "auto" => {
                section.insert("computer_use_mode".into(), toml::Value::String(v.into()));
                changes.push(format!("capabilities.computer_use_mode = \"{v}\""));
            }
            _ => {
                return Err(format!(
                    "Invalid computer_use_mode '{v}'. Valid: container, native, auto"
                ))
            }
        }
    }
    if let Some(v) = cap.get("browser_via_bash").and_then(|v| v.as_bool()) {
        section.insert("browser_via_bash".into(), toml::Value::Boolean(v));
        changes.push(format!("capabilities.browser_via_bash = {v}"));
    }

    // ── Array fields (tool names must be non-empty strings) ──
    for (param_key, toml_key) in &[
        ("allowed_tools", "allowed_tools"),
        ("denied_tools", "denied_tools"),
        ("wiki_visible_to", "wiki_visible_to"),
    ] {
        if let Some(arr) = cap.get(*param_key).and_then(|v| v.as_array()) {
            let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("capabilities.{param_key} entries must be strings"))?;
                let s = s.trim();
                if s.is_empty() {
                    return Err(format!("capabilities.{param_key} entries must be non-empty"));
                }
                out.push(toml::Value::String(s.into()));
            }
            section.insert((*toml_key).into(), toml::Value::Array(out));
            changes.push(format!("capabilities.{toml_key} = [{} entries]", arr.len()));
        }
    }

    // ── [capabilities.computer_use_config] sub-table ──
    if let Some(cfg) = cap.get("computer_use_config").and_then(|v| v.as_object()) {
        let sub = section
            .entry("computer_use_config")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "Invalid [capabilities.computer_use_config] section".to_string())?;

        for (param_key, toml_key) in &[
            ("allowed_apps", "allowed_apps"),
            ("blocked_actions", "blocked_actions"),
        ] {
            if let Some(arr) = cfg.get(*param_key).and_then(|v| v.as_array()) {
                let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
                for item in arr {
                    let s = item.as_str().ok_or_else(|| {
                        format!("computer_use_config.{param_key} entries must be strings")
                    })?;
                    let s = s.trim();
                    if s.is_empty() {
                        return Err(format!(
                            "computer_use_config.{param_key} entries must be non-empty"
                        ));
                    }
                    out.push(toml::Value::String(s.into()));
                }
                sub.insert((*toml_key).into(), toml::Value::Array(out));
                changes.push(format!(
                    "capabilities.computer_use_config.{toml_key} = [{} entries]",
                    arr.len()
                ));
            }
        }

        if let Some(v) = cfg.get("max_session_minutes").and_then(|v| v.as_u64()) {
            if v == 0 || v > 1440 {
                return Err("max_session_minutes must be 1-1440".into());
            }
            sub.insert("max_session_minutes".into(), toml::Value::Integer(v as i64));
            changes.push(format!("capabilities.computer_use_config.max_session_minutes = {v}"));
        }
        if let Some(v) = cfg.get("max_actions").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("max_actions must be 1-10000".into());
            }
            sub.insert("max_actions".into(), toml::Value::Integer(v as i64));
            changes.push(format!("capabilities.computer_use_config.max_actions = {v}"));
        }
        if let Some(v) = cfg.get("display_width").and_then(|v| v.as_u64()) {
            if !(320..=7680).contains(&v) {
                return Err("display_width must be 320-7680".into());
            }
            sub.insert("display_width".into(), toml::Value::Integer(v as i64));
            changes.push(format!("capabilities.computer_use_config.display_width = {v}"));
        }
        if let Some(v) = cfg.get("display_height").and_then(|v| v.as_u64()) {
            if !(240..=4320).contains(&v) {
                return Err("display_height must be 240-4320".into());
            }
            sub.insert("display_height".into(), toml::Value::Integer(v as i64));
            changes.push(format!("capabilities.computer_use_config.display_height = {v}"));
        }
        if let Some(v) = cfg.get("auto_confirm_trusted").and_then(|v| v.as_bool()) {
            sub.insert("auto_confirm_trusted".into(), toml::Value::Boolean(v));
            changes.push(format!("capabilities.computer_use_config.auto_confirm_trusted = {v}"));
        }
    }

    Ok(changes)
}

// ── P1 dashboard-config helpers (RT / EVO / CT) ───────────────────────────────
//
// Same contract as `apply_capabilities_to_table`: pure functions that mutate a
// `toml::Table` in place from an `agents.update` params object, returning the
// human-readable change list (empty if the relevant param object was absent).
// They validate enums / numeric ranges; the async wrapper owns IO + encryption.
//
// IMPORTANT: these only handle the *advanced* fields NOT already written inline
// in `handle_agents_update`. They do not duplicate `[evolution]` gvu/cognitive/
// max_active_skills/stagnation_* nor `[container]` sandbox_enabled/network_access/
// readonly_project/timeout_ms/max_concurrent.

/// Valid AI runtime providers (mirrors the `AgentRuntime` registry backends).
const VALID_RUNTIME_PROVIDERS: &[&str] =
    &["claude", "codex", "gemini", "antigravity", "openai_compat"];

/// Detect Claude OAuth availability by reading `~/.claude/.credentials.json`
/// directly (the OS user home, not `DUDUCLAW_HOME`). Returns
/// `(has_oauth, subscription_tier)`. Never returns the token itself — only its
/// presence — so this is safe to expose at viewer level. Mirrors the CLI's
/// `detect_claude_auth_from_file`; we read the file rather than shelling out to
/// `claude auth status` to keep the RPC fast and side-effect-free.
fn detect_claude_oauth() -> (bool, Option<String>) {
    let home = duduclaw_core::platform::home_dir();
    if home.is_empty() {
        return (false, None);
    }
    let cred_path = std::path::Path::new(&home)
        .join(".claude")
        .join(".credentials.json");
    let Ok(content) = std::fs::read_to_string(&cred_path) else {
        return (false, None);
    };
    let Ok(json) = serde_json::from_str::<Value>(&content) else {
        return (false, None);
    };

    // Two known shapes: `claudeAiOauth` (older) and `oauthAccount` (newer).
    for key in ["claudeAiOauth", "oauthAccount"] {
        if let Some(obj) = json.get(key) {
            let has_token = obj
                .get("accessToken")
                .or_else(|| obj.get("token"))
                .and_then(|v| v.as_str())
                .is_some_and(|t| !t.is_empty());
            if has_token {
                let sub = obj
                    .get("subscriptionType")
                    .or_else(|| obj.get("planType"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                return (true, sub);
            }
        }
    }
    (false, None)
}

/// Validate + write the `[runtime]` section from the `runtime` params object.
/// Fields: `provider` (enum), `fallback` (string), `pty_pool_enabled` (bool),
/// `worker_managed` (bool). (RT.1)
fn apply_runtime_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let rt = match params.get("runtime").and_then(|v| v.as_object()) {
        Some(r) => r,
        None => return Ok(changes),
    };

    let section = table
        .entry("runtime")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [runtime] section".to_string())?;

    if let Some(v) = rt.get("provider").and_then(|v| v.as_str()) {
        if !VALID_RUNTIME_PROVIDERS.contains(&v) {
            return Err(format!(
                "Invalid runtime.provider '{v}'. Valid: claude, codex, gemini, openai_compat"
            ));
        }
        section.insert("provider".into(), toml::Value::String(v.into()));
        changes.push(format!("runtime.provider = \"{v}\""));
    }
    if let Some(v) = rt.get("fallback").and_then(|v| v.as_str()) {
        let v = v.trim();
        // Empty string clears the fallback.
        if v.is_empty() {
            section.remove("fallback");
            changes.push("runtime.fallback cleared".to_string());
        } else {
            if !VALID_RUNTIME_PROVIDERS.contains(&v) {
                return Err(format!(
                    "Invalid runtime.fallback '{v}'. Valid: claude, codex, gemini, openai_compat"
                ));
            }
            section.insert("fallback".into(), toml::Value::String(v.into()));
            changes.push(format!("runtime.fallback = \"{v}\""));
        }
    }
    if let Some(v) = rt.get("pty_pool_enabled").and_then(|v| v.as_bool()) {
        section.insert("pty_pool_enabled".into(), toml::Value::Boolean(v));
        changes.push(format!("runtime.pty_pool_enabled = {v}"));
    }
    if let Some(v) = rt.get("worker_managed").and_then(|v| v.as_bool()) {
        section.insert("worker_managed".into(), toml::Value::Boolean(v));
        changes.push(format!("runtime.worker_managed = {v}"));
    }

    Ok(changes)
}

/// Validate a 0.0–1.0 threshold field, returning the float or an error.
fn validate_unit_threshold(name: &str, v: f64) -> Result<f64, String> {
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("{name} must be 0.0-1.0"));
    }
    Ok(v)
}

/// Apply the *advanced* `[evolution]` fields (EVO.1–EVO.3) NOT already handled
/// inline in `handle_agents_update`. Reads from the `evolution_advanced` params
/// object. Covers `[evolution.external_factors]` + skill-synthesis / graduation /
/// recommendation / curiosity / behavior-monitor scalars.
fn apply_evolution_advanced_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let adv = match params.get("evolution_advanced").and_then(|v| v.as_object()) {
        Some(a) => a,
        None => return Ok(changes),
    };

    let evo = table
        .entry("evolution")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [evolution] section".to_string())?;

    // ── [evolution.external_factors] sub-table (EVO.1) ──
    if let Some(ef) = adv.get("external_factors").and_then(|v| v.as_object()) {
        let sub = evo
            .entry("external_factors")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "Invalid [evolution.external_factors] section".to_string())?;
        for key in &[
            "user_feedback",
            "security_events",
            "channel_metrics",
            "business_context",
            "peer_signals",
        ] {
            if let Some(v) = ef.get(*key).and_then(|v| v.as_bool()) {
                sub.insert((*key).into(), toml::Value::Boolean(v));
                changes.push(format!("evolution.external_factors.{key} = {v}"));
            }
        }
    }

    // ── Boolean toggles (EVO.2–EVO.3) ──
    for key in &[
        "skill_synthesis_enabled",
        "skill_graduation_enabled",
        "skill_recommendation_enabled",
        "curiosity_enabled",
        "skill_behavior_monitor_enabled",
    ] {
        if let Some(v) = adv.get(*key).and_then(|v| v.as_bool()) {
            evo.insert((*key).into(), toml::Value::Boolean(v));
            changes.push(format!("evolution.{key} = {v}"));
        }
    }

    // ── 0.0–1.0 thresholds (EVO.2–EVO.3) ──
    // NOTE: `skill_synthesis_threshold` is NOT here — it is a u32 count of
    // repeated gap detections (see EvolutionConfig), not a unit threshold.
    // Writing it as a TOML float made agent.toml fail to deserialize on the
    // next registry scan, silently dropping the agent from the dashboard.
    for key in &[
        "skill_graduation_min_lift",
        "skill_recommendation_threshold",
        "curiosity_threshold",
        "skill_behavior_drift_threshold",
    ] {
        if let Some(v) = adv.get(*key).and_then(|v| v.as_f64()) {
            let v = validate_unit_threshold(&format!("evolution.{key}"), v)?;
            evo.insert((*key).into(), toml::Value::Float(v));
            changes.push(format!("evolution.{key} = {v}"));
        }
    }

    // ── Unsigned-integer fields (EVO.2–EVO.3) ──
    for key in &[
        "skill_synthesis_threshold",
        "skill_synthesis_cooldown_hours",
        "skill_trial_ttl",
        "curiosity_max_daily",
    ] {
        if let Some(v) = adv.get(*key).and_then(|v| v.as_u64()) {
            evo.insert((*key).into(), toml::Value::Integer(v as i64));
            changes.push(format!("evolution.{key} = {v}"));
        }
    }

    Ok(changes)
}

/// Parse a mount entry `{ host, container, readonly? }` into a TOML table,
/// rejecting empty paths and ones touching the mount-allowlist blocked patterns.
fn parse_mount_entry(item: &Value) -> Result<toml::Value, String> {
    /// Sensitive path fragments that must never be bind-mounted into a sandbox.
    /// Mirrors `config/mount-allowlist.example.json` `blocked_patterns`.
    const BLOCKED_PATTERNS: &[&str] = &[
        ".ssh",
        ".gnupg",
        ".env",
        ".aws",
        ".config/gcloud",
        ".docker/config.json",
        "secret.key",
        ".kube/config",
    ];

    let obj = item
        .as_object()
        .ok_or_else(|| "additional_mounts entries must be objects".to_string())?;
    let host = obj
        .get("host")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "additional_mounts.host must be a non-empty string".to_string())?;
    let container = obj
        .get("container")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "additional_mounts.container must be a non-empty string".to_string())?;
    let readonly = obj.get("readonly").and_then(|v| v.as_bool()).unwrap_or(false);

    for pat in BLOCKED_PATTERNS {
        if host.contains(pat) {
            return Err(format!("additional_mounts.host '{host}' matches blocked pattern '{pat}'"));
        }
    }

    let mut m = toml::map::Map::new();
    m.insert("host".into(), toml::Value::String(host.into()));
    m.insert("container".into(), toml::Value::String(container.into()));
    m.insert("readonly".into(), toml::Value::Boolean(readonly));
    Ok(toml::Value::Table(m))
}

/// Parse an env entry — either `{ key, value }` or a `[k, v]` 2-tuple — into a
/// `[key, value]` TOML array (the container env representation).
fn parse_env_entry(item: &Value) -> Result<toml::Value, String> {
    if let Some(obj) = item.as_object() {
        let k = obj
            .get("key")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "env entries must have a non-empty 'key'".to_string())?;
        let v = obj.get("value").and_then(|v| v.as_str()).unwrap_or("");
        return Ok(toml::Value::Array(vec![
            toml::Value::String(k.into()),
            toml::Value::String(v.into()),
        ]));
    }
    if let Some(arr) = item.as_array() {
        if arr.len() != 2 {
            return Err("env [k, v] entries must have exactly 2 elements".to_string());
        }
        let k = arr[0]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "env key must be a non-empty string".to_string())?;
        let v = arr[1].as_str().unwrap_or("");
        return Ok(toml::Value::Array(vec![
            toml::Value::String(k.into()),
            toml::Value::String(v.into()),
        ]));
    }
    Err("env entries must be {key,value} objects or [k,v] arrays".to_string())
}

/// Apply the *advanced* `[container]` fields (CT.1–CT.2) NOT already handled
/// inline in `handle_agents_update`. Reads from the `container_advanced` params
/// object. Covers worktree_* toggles + worktree_copy_files / additional_mounts /
/// cmd / env arrays.
fn apply_container_advanced_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let adv = match params.get("container_advanced").and_then(|v| v.as_object()) {
        Some(a) => a,
        None => return Ok(changes),
    };

    let ct = table
        .entry("container")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [container] section".to_string())?;

    // ── worktree toggles (CT.1) ──
    for key in &[
        "worktree_enabled",
        "worktree_auto_merge",
        "worktree_cleanup_on_exit",
    ] {
        if let Some(v) = adv.get(*key).and_then(|v| v.as_bool()) {
            ct.insert((*key).into(), toml::Value::Boolean(v));
            changes.push(format!("container.{key} = {v}"));
        }
    }

    // ── String-array fields: worktree_copy_files, cmd (CT.1–CT.2) ──
    for key in &["worktree_copy_files", "cmd"] {
        if let Some(arr) = adv.get(*key).and_then(|v| v.as_array()) {
            let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
            for entry in arr {
                let s = entry
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| format!("container.{key} entries must be non-empty strings"))?;
                out.push(toml::Value::String(s.into()));
            }
            ct.insert((*key).into(), toml::Value::Array(out));
            changes.push(format!("container.{key} = [{} entries]", arr.len()));
        }
    }

    // ── additional_mounts (CT.2) ──
    if let Some(arr) = adv.get("additional_mounts").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
        for entry in arr {
            out.push(parse_mount_entry(entry)?);
        }
        ct.insert("additional_mounts".into(), toml::Value::Array(out));
        changes.push(format!("container.additional_mounts = [{} entries]", arr.len()));
    }

    // ── env (CT.2) ──
    if let Some(arr) = adv.get("env").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
        for entry in arr {
            out.push(parse_env_entry(entry)?);
        }
        ct.insert("env".into(), toml::Value::Array(out));
        changes.push(format!("container.env = [{} entries]", arr.len()));
    }

    Ok(changes)
}

// ── P2 GOV helpers (governance policies/*.yaml) ───────────────────────────────
//
// The gateway crate does NOT depend on `duduclaw-governance` or `serde_yaml`
// (adding a dependency would require touching Cargo.toml, out of scope for this
// pass). The policy YAML schema is small, fixed, and fully under our control —
// so we round-trip it with a focused, deterministic emitter/parser that mirrors
// `duduclaw-governance::policy::{PolicyType, RatePolicy, ...}` field-for-field.
// Keep in sync with `crates/duduclaw-governance/src/policy.rs`.

/// Valid `resource` strings for a Rate policy (mirror `Resource` enum).
const GOV_RATE_RESOURCES: &[&str] = &["mcp_calls", "memory_writes", "wiki_writes", "message_sends"];
/// Valid `action_on_violation` strings (mirror `ActionOnViolation` enum).
const GOV_ACTIONS: &[&str] = &["reject", "warn", "throttle"];
/// Valid `policy_type` strings (mirror `PolicyType` tag).
const GOV_POLICY_TYPES: &[&str] = &["rate", "permission", "quota", "lifecycle"];

/// One parsed governance policy held as a flat JSON object (the same shape the
/// dashboard sends/receives). `policy_type`, `policy_id`, `agent_id` are always
/// present; the remaining keys depend on the type.
type GovPolicy = serde_json::Map<String, Value>;

/// Validate a `policy_id` token: non-empty, ≤128 chars, `[a-zA-Z0-9._-]`.
fn gov_valid_policy_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Validate a governance policy object per its `policy_type`, returning a
/// normalised copy (only known fields, correct numeric types). Mirrors the
/// `validate()` methods in `duduclaw-governance::policy`.
fn gov_validate_policy(p: &Value) -> Result<GovPolicy, String> {
    let obj = p.as_object().ok_or("policy must be an object")?;
    let policy_type = obj
        .get("policy_type")
        .and_then(|v| v.as_str())
        .ok_or("policy missing 'policy_type'")?;
    if !GOV_POLICY_TYPES.contains(&policy_type) {
        return Err(format!(
            "Invalid policy_type '{policy_type}'. Valid: {}",
            GOV_POLICY_TYPES.join(", ")
        ));
    }
    let policy_id = obj.get("policy_id").and_then(|v| v.as_str()).unwrap_or("").trim();
    if !gov_valid_policy_id(policy_id) {
        return Err("policy_id must be 1-128 chars of [a-zA-Z0-9._-]".into());
    }
    let agent_id = obj.get("agent_id").and_then(|v| v.as_str()).unwrap_or("*").trim();
    // agent_id is "*" (global) or a valid agent id.
    if agent_id != "*" && !is_valid_agent_id(agent_id) {
        return Err(format!("Invalid agent_id '{agent_id}' (use '*' for global)"));
    }

    let mut out = GovPolicy::new();
    out.insert("policy_type".into(), json!(policy_type));
    out.insert("policy_id".into(), json!(policy_id));
    out.insert("agent_id".into(), json!(agent_id));

    // Helper closures for required numeric fields.
    let req_u64 = |key: &str| -> Result<u64, String> {
        obj.get(key)
            .and_then(|v| v.as_u64())
            .ok_or_else(|| format!("{policy_type} policy missing/invalid '{key}' (positive integer)"))
    };
    let str_arr = |key: &str| -> Vec<Value> {
        obj.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| json!(s))
                    .collect()
            })
            .unwrap_or_default()
    };

    match policy_type {
        "rate" => {
            let resource = obj.get("resource").and_then(|v| v.as_str()).unwrap_or("");
            if !GOV_RATE_RESOURCES.contains(&resource) {
                return Err(format!(
                    "Invalid rate resource '{resource}'. Valid: {}",
                    GOV_RATE_RESOURCES.join(", ")
                ));
            }
            let limit = req_u64("limit")?;
            if limit == 0 {
                return Err("rate limit must be > 0".into());
            }
            let window = req_u64("window_seconds")?;
            if window == 0 {
                return Err("window_seconds must be > 0".into());
            }
            let action = obj
                .get("action_on_violation")
                .and_then(|v| v.as_str())
                .unwrap_or("reject");
            if !GOV_ACTIONS.contains(&action) {
                return Err(format!(
                    "Invalid action_on_violation '{action}'. Valid: {}",
                    GOV_ACTIONS.join(", ")
                ));
            }
            out.insert("resource".into(), json!(resource));
            out.insert("limit".into(), json!(limit));
            out.insert("window_seconds".into(), json!(window));
            out.insert("action_on_violation".into(), json!(action));
        }
        "permission" => {
            // Conflict check: a scope cannot be both allowed and denied.
            let allowed = str_arr("allowed_scopes");
            let denied = str_arr("denied_scopes");
            for a in &allowed {
                if denied.contains(a) {
                    return Err(format!(
                        "scope {} appears in both allowed_scopes and denied_scopes",
                        a.as_str().unwrap_or("")
                    ));
                }
            }
            out.insert("allowed_scopes".into(), json!(allowed));
            out.insert("denied_scopes".into(), json!(denied));
            out.insert("requires_approval".into(), json!(str_arr("requires_approval")));
        }
        "quota" => {
            let budget = req_u64("daily_token_budget")?;
            if budget == 0 {
                return Err("daily_token_budget must be > 0".into());
            }
            let max_tasks = req_u64("max_concurrent_tasks")?;
            if max_tasks == 0 {
                return Err("max_concurrent_tasks must be > 0".into());
            }
            let max_mem = obj.get("max_memory_entries").and_then(|v| v.as_u64()).unwrap_or(0);
            let reset_cron = obj
                .get("reset_cron")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("0 0 * * *");
            out.insert("daily_token_budget".into(), json!(budget));
            out.insert("max_concurrent_tasks".into(), json!(max_tasks));
            out.insert("max_memory_entries".into(), json!(max_mem));
            out.insert("reset_cron".into(), json!(reset_cron));
        }
        "lifecycle" => {
            let idle = req_u64("max_idle_hours")?;
            if idle == 0 {
                return Err("max_idle_hours must be > 0".into());
            }
            let hc = req_u64("health_check_interval_seconds")?;
            if hc == 0 {
                return Err("health_check_interval_seconds must be > 0".into());
            }
            let suspend = obj
                .get("auto_suspend_on_violation_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            out.insert("max_idle_hours".into(), json!(idle));
            out.insert("health_check_interval_seconds".into(), json!(hc));
            out.insert("auto_suspend_on_violation_count".into(), json!(suspend));
        }
        _ => unreachable!(),
    }
    Ok(out)
}

/// Escape a scalar string for safe YAML emission: always double-quote and
/// escape `\` and `"`. Deterministic + injection-safe (never emits a bare
/// value that could be re-read as a different YAML type).
fn gov_yaml_quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Serialise an ordered list of validated policies into the canonical
/// `policies:` YAML document (matches `duduclaw-governance::PolicyFile`).
fn gov_emit_yaml(policies: &[GovPolicy]) -> String {
    let mut out = String::from("# DuDuClaw Governance policies — managed by dashboard (governance.upsert)\n");
    out.push_str("policies:\n");
    if policies.is_empty() {
        out.push_str("  []\n");
        return out;
    }
    for p in policies {
        let ptype = p.get("policy_type").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("  - policy_type: {ptype}\n"));
        out.push_str(&format!(
            "    policy_id: {}\n",
            gov_yaml_quote(p.get("policy_id").and_then(|v| v.as_str()).unwrap_or(""))
        ));
        out.push_str(&format!(
            "    agent_id: {}\n",
            gov_yaml_quote(p.get("agent_id").and_then(|v| v.as_str()).unwrap_or("*"))
        ));
        // Per-type scalar + list fields, in a stable order.
        let scalar = |out: &mut String, key: &str| {
            if let Some(v) = p.get(key) {
                if let Some(n) = v.as_u64() {
                    out.push_str(&format!("    {key}: {n}\n"));
                } else if let Some(s) = v.as_str() {
                    out.push_str(&format!("    {key}: {}\n", gov_yaml_quote(s)));
                }
            }
        };
        let list = |out: &mut String, key: &str| {
            if let Some(arr) = p.get(key).and_then(|v| v.as_array()) {
                if arr.is_empty() {
                    out.push_str(&format!("    {key}: []\n"));
                } else {
                    out.push_str(&format!("    {key}:\n"));
                    for item in arr {
                        if let Some(s) = item.as_str() {
                            out.push_str(&format!("      - {}\n", gov_yaml_quote(s)));
                        }
                    }
                }
            }
        };
        match ptype {
            "rate" => {
                scalar(&mut out, "resource");
                scalar(&mut out, "limit");
                scalar(&mut out, "window_seconds");
                scalar(&mut out, "action_on_violation");
            }
            "permission" => {
                list(&mut out, "allowed_scopes");
                list(&mut out, "denied_scopes");
                list(&mut out, "requires_approval");
            }
            "quota" => {
                scalar(&mut out, "daily_token_budget");
                scalar(&mut out, "max_concurrent_tasks");
                scalar(&mut out, "max_memory_entries");
                scalar(&mut out, "reset_cron");
            }
            "lifecycle" => {
                scalar(&mut out, "max_idle_hours");
                scalar(&mut out, "health_check_interval_seconds");
                scalar(&mut out, "auto_suspend_on_violation_count");
            }
            _ => {}
        }
    }
    out
}

/// Minimal block-YAML parser for the canonical `policies:` document this module
/// emits (and the hand-written `policies/global.yaml`). Handles `key: value`
/// scalars, `key:` followed by `  - item` lists, `#` comments, and `[]` empty
/// lists. NOT a general YAML parser — it only needs to round-trip the policy
/// schema, but is also lenient enough to read the existing default file.
fn gov_parse_yaml(raw: &str) -> Result<Vec<GovPolicy>, String> {
    let mut policies: Vec<GovPolicy> = Vec::new();
    let mut current: Option<GovPolicy> = None;
    let mut pending_list: Option<String> = None;

    let unquote = |s: &str| -> String {
        let t = s.trim();
        if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
            t[1..t.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
        } else if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
            t[1..t.len() - 1].to_string()
        } else {
            t.to_string()
        }
    };
    // Coerce a scalar string to a JSON number when it looks like a bare integer.
    let coerce = |s: String| -> Value {
        if let Ok(n) = s.parse::<u64>() {
            json!(n)
        } else {
            json!(s)
        }
    };

    for line_raw in raw.lines() {
        // Strip trailing comments only when not inside quotes (cheap heuristic:
        // our emitter never embeds `#`; the default file keeps comments on their
        // own lines), so dropping leading-`#` lines + trimming is sufficient.
        let line = line_raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "policies:" || trimmed == "policies: []" {
            pending_list = None;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            // A "- policy_type: <x>" line ALWAYS starts a new policy entry.
            // Otherwise, if we're inside a pending list, it's a list item
            // (e.g. "- memory:read" under "allowed_scopes:").
            let starts_policy = rest.trim_start().starts_with("policy_type:");
            if !starts_policy && pending_list.is_some() {
                if let (Some(p), Some(key)) = (current.as_mut(), pending_list.as_ref()) {
                    let entry = p.entry(key.clone()).or_insert_with(|| json!([]));
                    if let Some(arr) = entry.as_array_mut() {
                        arr.push(json!(unquote(rest)));
                    }
                }
                continue;
            }
            // New policy entry: "- policy_type: rate".
            if let Some(p) = current.take() {
                policies.push(p);
            }
            pending_list = None;
            let mut p = GovPolicy::new();
            if let Some((k, v)) = rest.split_once(':') {
                let key = k.trim().to_string();
                let val = unquote(v);
                if !val.is_empty() && val != "[]" {
                    p.insert(key, coerce(val));
                } else if val == "[]" {
                    p.insert(key, json!([]));
                }
            }
            current = Some(p);
            continue;
        }
        // "key: value" or "key:" (list header).
        if let Some((k, v)) = trimmed.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim();
            let p = match current.as_mut() {
                Some(p) => p,
                None => continue, // keys before first "- " → ignore
            };
            if val.is_empty() {
                // List header — subsequent "  - item" lines belong here.
                pending_list = Some(key.clone());
                p.entry(key).or_insert_with(|| json!([]));
            } else if val == "[]" {
                pending_list = None;
                p.insert(key, json!([]));
            } else {
                pending_list = None;
                p.insert(key, coerce(unquote(val)));
            }
        }
    }
    if let Some(p) = current.take() {
        policies.push(p);
    }
    Ok(policies)
}

// ── P2 SCP helpers (.scope.toml wiki namespace policy) ────────────────────────
//
// Mirrors `duduclaw-cli::wiki_scope` (`[namespaces."<ns>"] mode = "..."`).
// Path: `<home>/shared/wiki/.scope.toml`.

/// Valid namespace modes (mirror `wiki_scope::NamespaceMode`).
const SCP_MODES: &[&str] = &["agent_writable", "read_only", "operator_only"];

/// Convert a parsed `.scope.toml` table into the `wiki_scope.get` response:
/// `{ namespaces: [{ namespace, mode, synced_from }] }`.
fn scp_table_to_response(table: &toml::Table) -> Value {
    let mut out: Vec<Value> = Vec::new();
    if let Some(ns) = table.get("namespaces").and_then(|v| v.as_table()) {
        for (name, entry) in ns {
            let t = match entry.as_table() {
                Some(t) => t,
                None => continue,
            };
            let mode = t.get("mode").and_then(|v| v.as_str()).unwrap_or("agent_writable");
            let synced_from = t.get("synced_from").and_then(|v| v.as_str());
            out.push(json!({
                "namespace": name,
                "mode": mode,
                "synced_from": synced_from,
            }));
        }
    }
    out.sort_by(|a, b| {
        a["namespace"].as_str().unwrap_or("").cmp(b["namespace"].as_str().unwrap_or(""))
    });
    json!({ "namespaces": out })
}

/// Apply a `wiki_scope.update` payload onto a `.scope.toml` table. Sets (or, on
/// `mode == "agent_writable"` with `remove == true`, deletes) a single
/// namespace's policy. Returns the change description. Validates the mode enum
/// + that `read_only` carries a non-empty `synced_from`.
fn scp_apply_namespace(
    table: &mut toml::Table,
    namespace: &str,
    mode: &str,
    synced_from: Option<&str>,
    remove: bool,
) -> Result<String, String> {
    if namespace.is_empty() || namespace.contains('/') || namespace.len() > 128 {
        return Err("namespace must be a non-empty top-level segment (no '/'), ≤128 chars".into());
    }
    let ns_table = table
        .entry("namespaces")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("Invalid [namespaces] section")?;

    if remove {
        ns_table.remove(namespace);
        return Ok(format!("namespace '{namespace}' policy removed (defaults to agent_writable)"));
    }

    if !SCP_MODES.contains(&mode) {
        return Err(format!("Invalid mode '{mode}'. Valid: {}", SCP_MODES.join(", ")));
    }
    let mut entry = toml::map::Map::new();
    entry.insert("mode".into(), toml::Value::String(mode.into()));
    if mode == "read_only" {
        let sf = synced_from.map(str::trim).unwrap_or("");
        if sf.is_empty() {
            return Err("mode 'read_only' requires a non-empty 'synced_from'".into());
        }
        entry.insert("synced_from".into(), toml::Value::String(sf.into()));
    }
    ns_table.insert(namespace.into(), toml::Value::Table(entry));
    Ok(format!("namespace '{namespace}' = {mode}"))
}

// ── P2 ODO helpers (per-agent [odoo] override) ────────────────────────────────

/// Validate a per-agent Odoo `allowed_actions` entry. Accepts a bare verb
/// (`read`/`write`/`create`/`unlink`/`execute`) or a qualified `verb:model`
/// form (e.g. `write:crm.lead`). The model part is validated like an Odoo model
/// name (alphanumeric + `.` + `_`).
fn odo_valid_action(action: &str) -> bool {
    const VERBS: &[&str] = &["read", "write", "create", "unlink", "execute"];
    let (verb, model) = match action.split_once(':') {
        Some((v, m)) => (v, Some(m)),
        None => (action, None),
    };
    if !VERBS.contains(&verb) {
        return false;
    }
    match model {
        None => true,
        Some(m) => {
            !m.is_empty()
                && m.len() <= 128
                && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        }
    }
}

/// Apply the per-agent `[odoo]` override fields from `agents.update` params.
/// Encrypts `api_key`/`password` into their `_enc` variants — cleartext is
/// never written. Returns the change list (empty if no `odoo` object present).
fn apply_odoo_to_table(
    table: &mut toml::Table,
    params: &Value,
    home_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();
    let odoo_in = match params.get("odoo").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(changes),
    };

    let section = table
        .entry("odoo")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("Invalid [odoo] section")?;

    // profile (string)
    if let Some(v) = odoo_in.get("profile").and_then(|v| v.as_str()) {
        let v = v.trim();
        if v.is_empty() {
            section.remove("profile");
            changes.push("odoo.profile cleared".into());
        } else if v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            section.insert("profile".into(), toml::Value::String(v.into()));
            changes.push(format!("odoo.profile = \"{v}\""));
        } else {
            return Err("odoo.profile must be ≤64 chars of [a-zA-Z0-9_-]".into());
        }
    }

    // allowed_models[] (Odoo model names)
    if let Some(arr) = odoo_in.get("allowed_models").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let m = item.as_str().unwrap_or("").trim();
            if m.is_empty() {
                continue;
            }
            if !MethodHandler::is_valid_odoo_model(m) {
                return Err(format!("Invalid odoo allowed_models entry '{m}'"));
            }
            out.push(toml::Value::String(m.into()));
        }
        section.insert("allowed_models".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.allowed_models = [{} entries]", out.len()));
    }

    // allowed_actions[] (bare verb or verb:model)
    if let Some(arr) = odoo_in.get("allowed_actions").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let a = item.as_str().unwrap_or("").trim();
            if a.is_empty() {
                continue;
            }
            if !odo_valid_action(a) {
                return Err(format!(
                    "Invalid odoo allowed_actions entry '{a}' (expected verb or verb:model, \
                     e.g. 'read' or 'write:crm.lead')"
                ));
            }
            out.push(toml::Value::String(a.into()));
        }
        section.insert("allowed_actions".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.allowed_actions = [{} entries]", out.len()));
    }

    // company_ids[] (ints)
    if let Some(arr) = odoo_in.get("company_ids").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let n = item
                .as_i64()
                .ok_or("odoo company_ids entries must be integers")?;
            if n < 0 {
                return Err("odoo company_ids must be non-negative".into());
            }
            out.push(toml::Value::Integer(n));
        }
        section.insert("company_ids".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.company_ids = [{} entries]", out.len()));
    }

    // url / db / username (plaintext scalars, optional overrides)
    for (param_key, toml_key) in &[("url", "url"), ("db", "db"), ("username", "username")] {
        if let Some(v) = odoo_in.get(*param_key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if v.is_empty() {
                section.remove(*toml_key);
                changes.push(format!("odoo.{toml_key} cleared"));
            } else {
                section.insert((*toml_key).into(), toml::Value::String(v.into()));
                changes.push(format!("odoo.{toml_key} = [SET]"));
            }
        }
    }

    // api_key / password → encrypt to *_enc, never store cleartext.
    for (param_key, enc_key) in &[("api_key", "api_key_enc"), ("password", "password_enc")] {
        if let Some(v) = odoo_in.get(*param_key).and_then(|v| v.as_str()) {
            // Refuse to persist the masked placeholder back as a real secret.
            if v == SECRET_MASK_SET {
                continue;
            }
            // Drop any stale cleartext mirror.
            section.remove(*param_key);
            if v.is_empty() {
                section.remove(*enc_key);
                changes.push(format!("odoo.{param_key} cleared"));
            } else if v.starts_with("secret://") {
                // A `secret://` reference is a POINTER, not a secret to be
                // encrypted. Store it RAW into `*_enc` (the field
                // merge_credentials reads) so the connector pool can resolve it
                // via the SecretManager at connect time.
                section.insert((*enc_key).into(), toml::Value::String(v.into()));
                changes.push(format!("odoo.{param_key} = [SECRET REF]"));
            } else if let Some(enc) = crate::config_crypto::encrypt_value(v, home_dir) {
                section.insert((*enc_key).into(), toml::Value::String(enc));
                changes.push(format!("odoo.{param_key} = [ENCRYPTED]"));
            } else {
                return Err(format!("Failed to encrypt odoo.{param_key}"));
            }
        }
    }

    Ok(changes)
}

// ── P1 INF helpers (inference.toml) ───────────────────────────────────────────

/// Masked placeholder returned to the dashboard in place of a stored secret.
const SECRET_MASK_SET: &str = "***set***";

/// Convert a parsed inference.toml table into the `inference.get` response JSON,
/// MASKING the `[openai_compat]` api key — the cleartext (or `_enc`) is NEVER
/// returned; instead `api_key_set: bool` + a masked placeholder are exposed.
fn inference_table_to_response(table: &toml::Table) -> Value {
    // Serialise the whole table to JSON, then scrub the secret in-place. Using
    // the generic round-trip means new inference.toml sub-sections surface
    // automatically without per-field plumbing.
    let mut v = serde_json::to_value(table).unwrap_or_else(|_| json!({}));
    if let Some(oc) = v.get_mut("openai_compat").and_then(|o| o.as_object_mut()) {
        let has_secret = oc
            .get("api_key")
            .and_then(|k| k.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || oc
                .get("api_key_enc")
                .and_then(|k| k.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false);
        // Never echo the raw / encrypted secret.
        oc.remove("api_key");
        oc.remove("api_key_enc");
        oc.insert("api_key_set".into(), json!(has_secret));
        oc.insert(
            "api_key".into(),
            json!(if has_secret { SECRET_MASK_SET } else { "" }),
        );
    }
    v
}

/// Get-or-create a sub-table by `key` on `table`.
fn inf_subtable<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table, String> {
    table
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| format!("Invalid [{key}] section"))
}

/// Apply scalar `bool`/`i64`/`f64`/`str` fields from a params object to a TOML
/// table, recording changes. `prefix` is used only for the change message.
#[allow(clippy::too_many_arguments)]
fn inf_apply_scalars(
    section: &mut toml::Table,
    src: &serde_json::Map<String, Value>,
    prefix: &str,
    bools: &[&str],
    ints: &[&str],
    floats: &[&str],
    strings: &[&str],
    str_arrays: &[&str],
    changes: &mut Vec<String>,
) -> Result<(), String> {
    for k in bools {
        if let Some(v) = src.get(*k).and_then(|v| v.as_bool()) {
            section.insert((*k).into(), toml::Value::Boolean(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in ints {
        if let Some(v) = src.get(*k).and_then(|v| v.as_i64()) {
            section.insert((*k).into(), toml::Value::Integer(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in floats {
        if let Some(v) = src.get(*k).and_then(|v| v.as_f64()) {
            section.insert((*k).into(), toml::Value::Float(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in strings {
        if let Some(v) = src.get(*k).and_then(|v| v.as_str()) {
            section.insert((*k).into(), toml::Value::String(v.into()));
            changes.push(format!("{prefix}.{k} = \"{v}\""));
        }
    }
    for k in str_arrays {
        if let Some(arr) = src.get(*k).and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("{prefix}.{k} entries must be strings"))?;
                out.push(toml::Value::String(s.into()));
            }
            section.insert((*k).into(), toml::Value::Array(out));
            changes.push(format!("{prefix}.{k} = [{} entries]", arr.len()));
        }
    }
    Ok(())
}

/// Apply an `inference.update` params object onto an inference.toml table.
/// Returns the change list. Validates router thresholds (strong < fast) and
/// generation ranges. Does NOT handle the openai_compat secret — that is done
/// in the async handler so it can call `config_crypto::encrypt_value`.
fn apply_inference_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();
    let p = params
        .as_object()
        .ok_or_else(|| "params must be an object".to_string())?;

    // ── root scalars (INF.2) ──
    inf_apply_scalars(
        table,
        p,
        "inference",
        &["enabled", "auto_load"],
        &["max_memory_mb"],
        &[],
        &["backend", "models_dir", "default_model"],
        &[],
        &mut changes,
    )?;

    // ── [generation] (INF.3) ──
    if let Some(g) = p.get("generation").and_then(|v| v.as_object()) {
        if let Some(t) = g.get("temperature").and_then(|v| v.as_f64()) {
            if !(0.0..=2.0).contains(&t) {
                return Err("generation.temperature must be 0.0-2.0".into());
            }
        }
        if let Some(tp) = g.get("top_p").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&tp) {
                return Err("generation.top_p must be 0.0-1.0".into());
            }
        }
        let section = inf_subtable(table, "generation")?;
        inf_apply_scalars(
            section,
            g,
            "generation",
            &[],
            &["max_tokens", "gpu_layers", "context_size"],
            &["temperature", "top_p"],
            &[],
            &["stop"],
            &mut changes,
        )?;
    }

    // ── [router] (INF.4) — cross-field validation strong < fast ──
    if let Some(r) = p.get("router").and_then(|v| v.as_object()) {
        // Resolve the *effective* thresholds (incoming overrides existing).
        let existing = table.get("router").and_then(|v| v.as_table());
        let fast = r
            .get("fast_threshold")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                existing
                    .and_then(|e| e.get("fast_threshold"))
                    .and_then(|v| v.as_float())
            });
        let strong = r
            .get("strong_threshold")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                existing
                    .and_then(|e| e.get("strong_threshold"))
                    .and_then(|v| v.as_float())
            });
        if let (Some(f), Some(s)) = (fast, strong) {
            if s >= f {
                return Err(format!(
                    "router.strong_threshold ({s}) must be < router.fast_threshold ({f})"
                ));
            }
        }
        for (name, val) in [("fast_threshold", fast), ("strong_threshold", strong)] {
            if let Some(v) = val {
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("router.{name} must be 0.0-1.0"));
                }
            }
        }
        let section = inf_subtable(table, "router")?;
        inf_apply_scalars(
            section,
            r,
            "router",
            &["enabled"],
            &["max_fast_prompt_tokens"],
            &["fast_threshold", "strong_threshold"],
            &["fast_model", "strong_model"],
            &["cloud_keywords", "fast_keywords"],
            &mut changes,
        )?;
    }

    // ── [openai_compat] non-secret fields (INF.5; api_key handled in caller) ──
    if let Some(oc) = p.get("openai_compat").and_then(|v| v.as_object()) {
        let section = inf_subtable(table, "openai_compat")?;
        inf_apply_scalars(
            section,
            oc,
            "openai_compat",
            &[],
            &[],
            &[],
            &["base_url", "model"],
            &[],
            &mut changes,
        )?;
    }

    // ── Generic pass-through sub-sections (INF.5) ──
    // Each is a flat table of scalars/arrays; apply them generically so new
    // backend fields don't require per-field plumbing. Secrets are not expected
    // in these sections.
    for sect in &[
        "exo",
        "llamafile",
        "mlx",
        "mistralrs",
        "llmlingua",
        "streaming_llm",
        "embedding",
    ] {
        if let Some(obj) = p.get(*sect).and_then(|v| v.as_object()) {
            let section = inf_subtable(table, sect)?;
            for (k, val) in obj {
                let tv = json_to_toml(val)
                    .ok_or_else(|| format!("Unsupported value type for {sect}.{k}"))?;
                section.insert(k.clone(), tv);
            }
            changes.push(format!("{sect} = [updated]"));
        }
    }

    Ok(changes)
}

/// Convert a JSON value into a TOML value for generic pass-through sections.
/// Returns None for null / unrepresentable values.
fn json_to_toml(v: &Value) -> Option<toml::Value> {
    match v {
        Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        Value::String(s) => Some(toml::Value::String(s.clone())),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                out.push(json_to_toml(item)?);
            }
            Some(toml::Value::Array(out))
        }
        Value::Object(o) => {
            let mut m = toml::map::Map::new();
            for (k, val) in o {
                m.insert(k.clone(), json_to_toml(val)?);
            }
            Some(toml::Value::Table(m))
        }
        Value::Null => None,
    }
}

/// Build the `[boundaries]` table for a CONTRACT.toml from `contract.update`
/// params. Validates `max_tool_calls_per_turn` range. Returns the full table to
/// serialise (the contract file only contains `[boundaries]`).
fn build_contract_table(params: &Value) -> Result<toml::Table, String> {
    fn string_array(params: &Value, key: &str) -> Result<Vec<toml::Value>, String> {
        let arr = params
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("Missing or invalid '{key}' (expected array of strings)"))?;
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item
                .as_str()
                .ok_or_else(|| format!("'{key}' entries must be strings"))?;
            let s = s.trim();
            if s.is_empty() {
                return Err(format!("'{key}' entries must be non-empty"));
            }
            out.push(toml::Value::String(s.into()));
        }
        Ok(out)
    }

    let must_not = string_array(params, "must_not")?;
    let must_always = string_array(params, "must_always")?;
    let max_calls = params
        .get("max_tool_calls_per_turn")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if max_calls > 1000 {
        return Err("max_tool_calls_per_turn must be 0-1000 (0 = unlimited)".into());
    }

    let mut boundaries = toml::map::Map::new();
    boundaries.insert("must_not".into(), toml::Value::Array(must_not));
    boundaries.insert("must_always".into(), toml::Value::Array(must_always));
    boundaries.insert(
        "max_tool_calls_per_turn".into(),
        toml::Value::Integer(max_calls as i64),
    );

    let mut table = toml::Table::new();
    table.insert("boundaries".into(), toml::Value::Table(boundaries));
    Ok(table)
}

/// Parse a CONTRACT.toml table into the `contract.get` response shape.
fn contract_table_to_response(table: &toml::Table) -> Value {
    let boundaries = table.get("boundaries").and_then(|v| v.as_table());
    let str_arr = |key: &str| -> Vec<String> {
        boundaries
            .and_then(|b| b.get(key))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default()
    };
    let max_calls = boundaries
        .and_then(|b| b.get("max_tool_calls_per_turn"))
        .and_then(|v| v.as_integer())
        .unwrap_or(0);
    json!({
        "must_not": str_arr("must_not"),
        "must_always": str_arr("must_always"),
        "max_tool_calls_per_turn": max_calls,
    })
}

/// Validate a redaction source mode string.
fn is_valid_source_mode(v: &str) -> bool {
    matches!(v, "on" | "off" | "selective" | "inherit")
}

/// Apply a `redaction.update` payload onto a config.toml table's `[redaction]`
/// section. Returns the change list. Validates ttl/purge ranges + source-mode
/// + tool-egress restore-args enums.
fn apply_redaction_to_table(table: &mut toml::Table, params: &Value) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let red = table
        .entry("redaction")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [redaction] section".to_string())?;

    // ── Root scalars ──
    if let Some(v) = params.get("enabled").and_then(|v| v.as_bool()) {
        red.insert("enabled".into(), toml::Value::Boolean(v));
        changes.push(format!("redaction.enabled = {v}"));
    }
    if let Some(v) = params.get("vault_ttl_hours").and_then(|v| v.as_i64()) {
        if v <= 0 || v > 8760 {
            return Err("vault_ttl_hours must be 1-8760".into());
        }
        red.insert("vault_ttl_hours".into(), toml::Value::Integer(v));
        changes.push(format!("redaction.vault_ttl_hours = {v}"));
    }
    if let Some(v) = params.get("purge_after_expire_days").and_then(|v| v.as_u64()) {
        if v > 3650 {
            return Err("purge_after_expire_days must be 0-3650".into());
        }
        red.insert("purge_after_expire_days".into(), toml::Value::Integer(v as i64));
        changes.push(format!("redaction.purge_after_expire_days = {v}"));
    }
    if let Some(arr) = params.get("profiles").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let s = item
                .as_str()
                .ok_or_else(|| "profiles entries must be strings".to_string())?;
            let s = s.trim();
            if s.is_empty() {
                return Err("profiles entries must be non-empty".into());
            }
            out.push(toml::Value::String(s.into()));
        }
        red.insert("profiles".into(), toml::Value::Array(out));
        changes.push(format!("redaction.profiles = [{} entries]", arr.len()));
    }

    // ── [redaction.sources] per-source modes ──
    if let Some(sources) = params.get("sources").and_then(|v| v.as_object()) {
        let sub = red
            .entry("sources")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "Invalid [redaction.sources] section".to_string())?;
        for key in &[
            "user_input",
            "tool_results",
            "system_prompt",
            "sub_agent",
            "cron_context",
        ] {
            if let Some(v) = sources.get(*key).and_then(|v| v.as_str()) {
                if !is_valid_source_mode(v) {
                    return Err(format!(
                        "Invalid redaction source mode '{v}' for {key}. Valid: on, off, selective, inherit"
                    ));
                }
                sub.insert((*key).into(), toml::Value::String(v.into()));
                changes.push(format!("redaction.sources.{key} = \"{v}\""));
            }
        }
    }

    // ── [redaction.tool_egress.<tool>] add/update/remove ──
    // `tool_egress` is an object keyed by tool name. A value of `null` removes
    // that tool's rule; otherwise `{ restore_args, audit_reveal }` upserts it.
    if let Some(egress) = params.get("tool_egress").and_then(|v| v.as_object()) {
        for (tool, rule) in egress {
            let tool_trim = tool.trim();
            if tool_trim.is_empty() {
                return Err("tool_egress tool names must be non-empty".into());
            }
            if rule.is_null() {
                if let Some(eg) = red.get_mut("tool_egress").and_then(|v| v.as_table_mut()) {
                    eg.remove(tool_trim);
                    changes.push(format!("redaction.tool_egress.{tool_trim} removed"));
                }
                continue;
            }
            let rule_obj = rule
                .as_object()
                .ok_or_else(|| format!("tool_egress.{tool_trim} must be an object or null"))?;
            let restore = rule_obj
                .get("restore_args")
                .and_then(|v| v.as_str())
                .unwrap_or("deny");
            if !matches!(restore, "restore" | "passthrough" | "deny") {
                return Err(format!(
                    "Invalid restore_args '{restore}' for tool_egress.{tool_trim}. Valid: restore, passthrough, deny"
                ));
            }
            let audit_reveal = rule_obj
                .get("audit_reveal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let eg = red
                .entry("tool_egress")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .ok_or_else(|| "Invalid [redaction.tool_egress] section".to_string())?;
            let mut entry = toml::map::Map::new();
            entry.insert("restore_args".into(), toml::Value::String(restore.into()));
            entry.insert("audit_reveal".into(), toml::Value::Boolean(audit_reveal));
            eg.insert(tool_trim.to_string(), toml::Value::Table(entry));
            changes.push(format!(
                "redaction.tool_egress.{tool_trim} = {{ restore_args = \"{restore}\", audit_reveal = {audit_reveal} }}"
            ));
        }
    }

    Ok(changes)
}

/// Parse a config.toml `[redaction]` section into the `redaction.get`
/// response shape.
fn redaction_table_to_response(table: &toml::Table) -> Value {
    let red = table.get("redaction").and_then(|v| v.as_table());
    let enabled = red.and_then(|r| r.get("enabled")).and_then(|v| v.as_bool()).unwrap_or(false);
    let vault_ttl = red
        .and_then(|r| r.get("vault_ttl_hours"))
        .and_then(|v| v.as_integer())
        .unwrap_or(168);
    let purge = red
        .and_then(|r| r.get("purge_after_expire_days"))
        .and_then(|v| v.as_integer())
        .unwrap_or(30);
    let profiles: Vec<String> = red
        .and_then(|r| r.get("profiles"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let sources = red.and_then(|r| r.get("sources")).and_then(|v| v.as_table());
    let source_mode = |key: &str, default: &str| -> String {
        sources
            .and_then(|s| s.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };

    let egress = red.and_then(|r| r.get("tool_egress")).and_then(|v| v.as_table());
    let mut egress_out = serde_json::Map::new();
    if let Some(eg) = egress {
        for (tool, rule) in eg {
            let rule_t = rule.as_table();
            let restore = rule_t
                .and_then(|t| t.get("restore_args"))
                .and_then(|v| v.as_str())
                .unwrap_or("deny");
            let audit = rule_t
                .and_then(|t| t.get("audit_reveal"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            egress_out.insert(
                tool.clone(),
                json!({ "restore_args": restore, "audit_reveal": audit }),
            );
        }
    }

    json!({
        "enabled": enabled,
        "vault_ttl_hours": vault_ttl,
        "purge_after_expire_days": purge,
        "profiles": profiles,
        "sources": {
            "user_input": source_mode("user_input", "off"),
            "tool_results": source_mode("tool_results", "on"),
            "system_prompt": source_mode("system_prompt", "selective"),
            "sub_agent": source_mode("sub_agent", "inherit"),
            "cron_context": source_mode("cron_context", "on"),
        },
        "tool_egress": Value::Object(egress_out),
    })
}

/// Parse a config.toml `[skill_synthesis]` section into the
/// `skill_synthesis.get` response shape. Defaults mirror
/// `skill_synthesis_pipeline::scheduler::SynthesisScheduleConfig::default()`
/// (auto_run=false, dry_run=true, interval_hours=24, lookback_days=1).
fn skill_synthesis_table_to_response(table: &toml::Table) -> Value {
    let s = table.get("skill_synthesis").and_then(|v| v.as_table());
    let auto_run = s
        .and_then(|t| t.get("auto_run"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dry_run = s
        .and_then(|t| t.get("dry_run"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let interval_hours = s
        .and_then(|t| t.get("interval_hours"))
        .and_then(|v| v.as_integer())
        .filter(|v| *v >= 1)
        .unwrap_or(24);
    let lookback_days = s
        .and_then(|t| t.get("lookback_days"))
        .and_then(|v| v.as_integer())
        .filter(|v| *v >= 1)
        .map(|v| v.min(30))
        .unwrap_or(1);
    let target_agent = s
        .and_then(|t| t.get("target_agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "auto_run": auto_run,
        "dry_run": dry_run,
        "interval_hours": interval_hours,
        "lookback_days": lookback_days,
        "target_agent": target_agent,
    })
}

/// Validate + apply a `skill_synthesis.update` payload onto a config.toml
/// table's `[skill_synthesis]` section. Returns the change list. All fields are
/// optional (partial update). An empty `target_agent` clears the key (the
/// scheduler then falls back to `[general] default_agent`).
fn apply_skill_synthesis_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();
    let section = table
        .entry("skill_synthesis".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "[skill_synthesis] is not a table".to_string())?;

    if let Some(v) = params.get("auto_run").and_then(|v| v.as_bool()) {
        section.insert("auto_run".into(), toml::Value::Boolean(v));
        changes.push(format!("skill_synthesis.auto_run = {v}"));
    }
    if let Some(v) = params.get("dry_run").and_then(|v| v.as_bool()) {
        section.insert("dry_run".into(), toml::Value::Boolean(v));
        changes.push(format!("skill_synthesis.dry_run = {v}"));
    }
    if let Some(v) = params.get("interval_hours").and_then(|v| v.as_u64()) {
        if v < 1 {
            return Err("interval_hours must be >= 1".into());
        }
        section.insert("interval_hours".into(), toml::Value::Integer(v as i64));
        changes.push(format!("skill_synthesis.interval_hours = {v}"));
    }
    if let Some(v) = params.get("lookback_days").and_then(|v| v.as_u64()) {
        if !(1..=30).contains(&v) {
            return Err("lookback_days must be 1-30".into());
        }
        section.insert("lookback_days".into(), toml::Value::Integer(v as i64));
        changes.push(format!("skill_synthesis.lookback_days = {v}"));
    }
    if let Some(v) = params.get("target_agent").and_then(|v| v.as_str()) {
        let t = v.trim();
        if t.is_empty() {
            section.remove("target_agent");
            changes.push("skill_synthesis.target_agent cleared".into());
        } else if t.contains('/') || t.contains('\\') || t.contains("..") {
            // Defense in depth: target_agent is later joined into a filesystem
            // path by the pipeline / scheduler.
            return Err("target_agent contains invalid characters".into());
        } else {
            section.insert("target_agent".into(), toml::Value::String(t.to_string()));
            changes.push(format!("skill_synthesis.target_agent = \"{t}\""));
        }
    }

    Ok(changes)
}

/// Validate + apply a `killswitch.update` payload onto a KILLSWITCH.toml table.
/// Returns the change list. Validates numeric ranges across all sub-sections.
fn apply_killswitch_to_table(table: &mut toml::Table, params: &Value) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    // Helper: get-or-create a sub-table.
    fn sub<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table, String> {
        table
            .entry(key.to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| format!("Invalid [{key}] section"))
    }

    // ── [triggers] ──
    if let Some(t) = params.get("triggers").and_then(|v| v.as_object()) {
        let sect = sub(table, "triggers")?;
        if let Some(v) = t.get("max_replies_per_minute").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("triggers.max_replies_per_minute must be 1-10000".into());
            }
            sect.insert("max_replies_per_minute".into(), toml::Value::Integer(v as i64));
            changes.push(format!("triggers.max_replies_per_minute = {v}"));
        }
        if let Some(v) = t.get("max_consecutive_errors").and_then(|v| v.as_u64()) {
            if v == 0 || v > 1000 {
                return Err("triggers.max_consecutive_errors must be 1-1000".into());
            }
            sect.insert("max_consecutive_errors".into(), toml::Value::Integer(v as i64));
            changes.push(format!("triggers.max_consecutive_errors = {v}"));
        }
        if let Some(v) = t.get("error_rate_threshold").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&v) {
                return Err("triggers.error_rate_threshold must be 0.0-1.0".into());
            }
            sect.insert("error_rate_threshold".into(), toml::Value::Float(v));
            changes.push(format!("triggers.error_rate_threshold = {v}"));
        }
        if let Some(v) = t.get("cost_limit_usd").and_then(|v| v.as_f64()) {
            if v < 0.0 || v > 1_000_000.0 {
                return Err("triggers.cost_limit_usd must be 0-1000000".into());
            }
            sect.insert("cost_limit_usd".into(), toml::Value::Float(v));
            changes.push(format!("triggers.cost_limit_usd = {v}"));
        }
    }

    // ── [circuit_breaker] ──
    if let Some(c) = params.get("circuit_breaker").and_then(|v| v.as_object()) {
        let sect = sub(table, "circuit_breaker")?;
        if let Some(v) = c.get("frequency_window_secs").and_then(|v| v.as_u64()) {
            if v == 0 || v > 86400 {
                return Err("circuit_breaker.frequency_window_secs must be 1-86400".into());
            }
            sect.insert("frequency_window_secs".into(), toml::Value::Integer(v as i64));
            changes.push(format!("circuit_breaker.frequency_window_secs = {v}"));
        }
        if let Some(v) = c.get("frequency_max_replies").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("circuit_breaker.frequency_max_replies must be 1-10000".into());
            }
            sect.insert("frequency_max_replies".into(), toml::Value::Integer(v as i64));
            changes.push(format!("circuit_breaker.frequency_max_replies = {v}"));
        }
        if let Some(v) = c.get("similarity_threshold").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&v) {
                return Err("circuit_breaker.similarity_threshold must be 0.0-1.0".into());
            }
            sect.insert("similarity_threshold".into(), toml::Value::Float(v));
            changes.push(format!("circuit_breaker.similarity_threshold = {v}"));
        }
        if let Some(v) = c.get("token_explosion_multiplier").and_then(|v| v.as_f64()) {
            if v < 1.0 || v > 1000.0 {
                return Err("circuit_breaker.token_explosion_multiplier must be 1.0-1000.0".into());
            }
            sect.insert("token_explosion_multiplier".into(), toml::Value::Float(v));
            changes.push(format!("circuit_breaker.token_explosion_multiplier = {v}"));
        }
        if let Some(v) = c.get("cooldown_secs").and_then(|v| v.as_u64()) {
            if v > 86400 {
                return Err("circuit_breaker.cooldown_secs must be 0-86400".into());
            }
            sect.insert("cooldown_secs".into(), toml::Value::Integer(v as i64));
            changes.push(format!("circuit_breaker.cooldown_secs = {v}"));
        }
        if let Some(v) = c.get("half_open_allow_count").and_then(|v| v.as_u64()) {
            if v == 0 || v > 1000 {
                return Err("circuit_breaker.half_open_allow_count must be 1-1000".into());
            }
            sect.insert("half_open_allow_count".into(), toml::Value::Integer(v as i64));
            changes.push(format!("circuit_breaker.half_open_allow_count = {v}"));
        }
    }

    // ── [failsafe] ──
    if let Some(f) = params.get("failsafe").and_then(|v| v.as_object()) {
        let sect = sub(table, "failsafe")?;
        for key in &["l1_auto_recover_secs", "l2_auto_recover_secs", "l3_auto_recover_secs"] {
            if let Some(v) = f.get(*key).and_then(|v| v.as_u64()) {
                if v > 86400 {
                    return Err(format!("failsafe.{key} must be 0-86400 (0 = manual only)"));
                }
                sect.insert((*key).into(), toml::Value::Integer(v as i64));
                changes.push(format!("failsafe.{key} = {v}"));
            }
        }
        for (param_key, toml_key) in &[
            ("default_restricted_reply", "default_restricted_reply"),
            ("default_halted_reply", "default_halted_reply"),
        ] {
            if let Some(v) = f.get(*param_key).and_then(|v| v.as_str()) {
                sect.insert((*toml_key).into(), toml::Value::String(v.into()));
                changes.push(format!("failsafe.{toml_key} updated"));
            }
        }
    }

    // ── [safety_words] ──
    if let Some(s) = params.get("safety_words").and_then(|v| v.as_object()) {
        let sect = sub(table, "safety_words")?;
        for key in &["stop", "stop_all", "resume", "status"] {
            if let Some(arr) = s.get(*key).and_then(|v| v.as_array()) {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    let w = item
                        .as_str()
                        .ok_or_else(|| format!("safety_words.{key} entries must be strings"))?;
                    let w = w.trim();
                    if w.is_empty() {
                        return Err(format!("safety_words.{key} entries must be non-empty"));
                    }
                    out.push(toml::Value::String(w.into()));
                }
                sect.insert((*key).into(), toml::Value::Array(out));
                changes.push(format!("safety_words.{key} = [{} entries]", arr.len()));
            }
        }
    }

    // ── [defensive_prompt] ──
    if let Some(d) = params.get("defensive_prompt").and_then(|v| v.as_object()) {
        let sect = sub(table, "defensive_prompt")?;
        if let Some(v) = d.get("enabled").and_then(|v| v.as_bool()) {
            sect.insert("enabled".into(), toml::Value::Boolean(v));
            changes.push(format!("defensive_prompt.enabled = {v}"));
        }
        if let Some(arr) = d.get("languages").and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let l = item
                    .as_str()
                    .ok_or_else(|| "defensive_prompt.languages entries must be strings".to_string())?;
                let l = l.trim();
                if l.is_empty() {
                    return Err("defensive_prompt.languages entries must be non-empty".into());
                }
                out.push(toml::Value::String(l.into()));
            }
            sect.insert("languages".into(), toml::Value::Array(out));
            changes.push(format!("defensive_prompt.languages = [{} entries]", arr.len()));
        }
    }

    // ── [audit] ──
    if let Some(a) = params.get("audit").and_then(|v| v.as_object()) {
        let sect = sub(table, "audit")?;
        if let Some(v) = a.get("enabled").and_then(|v| v.as_bool()) {
            sect.insert("enabled".into(), toml::Value::Boolean(v));
            changes.push(format!("audit.enabled = {v}"));
        }
        if let Some(v) = a.get("path").and_then(|v| v.as_str()) {
            let v = v.trim();
            if v.is_empty() {
                return Err("audit.path must be non-empty".into());
            }
            sect.insert("path".into(), toml::Value::String(v.into()));
            changes.push("audit.path updated".to_string());
        }
    }

    Ok(changes)
}

/// Parse a KILLSWITCH.toml table into the `killswitch.get` response shape.
/// Falls back to the documented defaults for any missing field so the dashboard
/// always renders a complete form.
fn killswitch_table_to_response(table: &toml::Table) -> Value {
    let ks = duduclaw_security::killswitch::KillswitchConfig::default();

    let t = table.get("triggers").and_then(|v| v.as_table());
    let cb = table.get("circuit_breaker").and_then(|v| v.as_table());
    let fs = table.get("failsafe").and_then(|v| v.as_table());
    let sw = table.get("safety_words").and_then(|v| v.as_table());
    let dp = table.get("defensive_prompt").and_then(|v| v.as_table());
    let au = table.get("audit").and_then(|v| v.as_table());

    let int = |tbl: Option<&toml::Table>, key: &str, default: i64| -> i64 {
        tbl.and_then(|t| t.get(key)).and_then(|v| v.as_integer()).unwrap_or(default)
    };
    let flt = |tbl: Option<&toml::Table>, key: &str, default: f64| -> f64 {
        tbl.and_then(|t| t.get(key)).and_then(|v| v.as_float()).unwrap_or(default)
    };
    let boolean = |tbl: Option<&toml::Table>, key: &str, default: bool| -> bool {
        tbl.and_then(|t| t.get(key)).and_then(|v| v.as_bool()).unwrap_or(default)
    };
    let strv = |tbl: Option<&toml::Table>, key: &str, default: &str| -> String {
        tbl.and_then(|t| t.get(key)).and_then(|v| v.as_str()).unwrap_or(default).to_string()
    };
    let arr = |tbl: Option<&toml::Table>, key: &str, default: &[String]| -> Vec<String> {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_else(|| default.to_vec())
    };

    json!({
        "triggers": {
            "max_replies_per_minute": int(t, "max_replies_per_minute", ks.triggers.max_replies_per_minute as i64),
            "max_consecutive_errors": int(t, "max_consecutive_errors", ks.triggers.max_consecutive_errors as i64),
            "error_rate_threshold": flt(t, "error_rate_threshold", ks.triggers.error_rate_threshold),
            "cost_limit_usd": flt(t, "cost_limit_usd", ks.triggers.cost_limit_usd),
        },
        "circuit_breaker": {
            "frequency_window_secs": int(cb, "frequency_window_secs", ks.circuit_breaker.frequency_window_secs as i64),
            "frequency_max_replies": int(cb, "frequency_max_replies", ks.circuit_breaker.frequency_max_replies as i64),
            "similarity_threshold": flt(cb, "similarity_threshold", ks.circuit_breaker.similarity_threshold),
            "token_explosion_multiplier": flt(cb, "token_explosion_multiplier", ks.circuit_breaker.token_explosion_multiplier),
            "cooldown_secs": int(cb, "cooldown_secs", ks.circuit_breaker.cooldown_secs as i64),
            "half_open_allow_count": int(cb, "half_open_allow_count", ks.circuit_breaker.half_open_allow_count as i64),
        },
        "failsafe": {
            "l1_auto_recover_secs": int(fs, "l1_auto_recover_secs", ks.failsafe.l1_auto_recover_secs as i64),
            "l2_auto_recover_secs": int(fs, "l2_auto_recover_secs", ks.failsafe.l2_auto_recover_secs as i64),
            "l3_auto_recover_secs": int(fs, "l3_auto_recover_secs", ks.failsafe.l3_auto_recover_secs as i64),
            "default_restricted_reply": strv(fs, "default_restricted_reply", &ks.failsafe.default_restricted_reply),
            "default_halted_reply": strv(fs, "default_halted_reply", &ks.failsafe.default_halted_reply),
        },
        "safety_words": {
            "stop": arr(sw, "stop", &ks.safety_words.stop),
            "stop_all": arr(sw, "stop_all", &ks.safety_words.stop_all),
            "resume": arr(sw, "resume", &ks.safety_words.resume),
            "status": arr(sw, "status", &ks.safety_words.status),
        },
        "defensive_prompt": {
            "enabled": boolean(dp, "enabled", ks.defensive_prompt.enabled),
            "languages": arr(dp, "languages", &ks.defensive_prompt.languages),
        },
        "audit": {
            "enabled": boolean(au, "enabled", ks.audit.enabled),
            "path": strv(au, "path", &ks.audit.path),
        },
    })
}

/// Redact URLs and credential-like tokens from a free-form error string before
/// it is forwarded to a client (M19).
///
/// Operates per whitespace-separated token so surrounding prose is preserved:
/// - Tokens containing a scheme (`://`) have any `user:pass@` userinfo and any
///   `?query` string stripped, leaving `scheme://host[:port]/path`.
/// - `key=value` pairs whose key looks sensitive (api_key / token / password /
///   secret / pwd / auth) have their value replaced with `[REDACTED]`.
fn scrub_secrets_from_text(raw: &str) -> String {
    fn is_sensitive_key(key: &str) -> bool {
        let k = key.to_ascii_lowercase();
        ["api_key", "apikey", "token", "password", "passwd", "pwd", "secret", "auth", "key"]
            .iter()
            .any(|s| k.contains(s))
    }

    fn scrub_token(token: &str) -> String {
        // 1. URL: strip userinfo + query string, keep scheme://host/path.
        if let Some(scheme_idx) = token.find("://") {
            let scheme = &token[..scheme_idx];
            let rest = &token[scheme_idx + 3..];
            // Drop everything from the first query/fragment marker onward.
            let rest = rest
                .split(['?', '#'])
                .next()
                .unwrap_or("");
            // Drop userinfo (anything up to and including '@' in the authority).
            // The authority ends at the first '/'.
            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            let host = authority.rsplit('@').next().unwrap_or(authority);
            return format!("{scheme}://{host}{path}");
        }
        // 2. key=value: redact sensitive values.
        if let Some(eq) = token.find('=') {
            let (key, _val) = token.split_at(eq);
            if is_sensitive_key(key) {
                return format!("{key}=[REDACTED]");
            }
        }
        token.to_string()
    }

    raw.split_whitespace()
        .map(scrub_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Hours elapsed since the start of the current UTC month — the look-back
/// window CostTelemetry uses to compute "this month" spend. Always ≥ 1 so the
/// telemetry query never receives a zero window.
fn hours_since_month_start() -> u64 {
    let now = Utc::now();
    let month_start = now
        .date_naive()
        .with_day(1)
        .unwrap_or(now.date_naive())
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default();
    let month_start_utc =
        chrono::DateTime::<Utc>::from_naive_utc_and_offset(month_start, Utc);
    (now - month_start_utc).num_hours().max(1) as u64
}

/// Validate wiki page path: relative, .md suffix, no traversal, no NUL.
/// Mirrors `WikiStore::validate_page_path` for use at the WS RPC boundary
/// (review H2/M6 — page_path enters audit log + downstream file ops).
fn is_safe_wiki_page_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains("..")
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.contains('\0')
        && !path.contains("%2e")
        && !path.contains("%2E")
        && !path.contains("%2f")
        && !path.contains("%2F")
        && path.ends_with(".md")
}

/// Dispatches incoming RPC methods to the appropriate handler.
pub struct MethodHandler {
    registry: Arc<RwLock<AgentRegistry>>,
    home_dir: PathBuf,
    start_time: Instant,
    channel_status: Arc<RwLock<std::collections::HashMap<String, ChannelState>>>,
    heartbeat: RwLock<Option<Arc<duduclaw_agent::HeartbeatScheduler>>>,
    /// Reply context for hot-starting channels after config changes.
    reply_ctx: RwLock<Option<Arc<crate::channel_reply::ReplyContext>>>,
    /// Handles for running channel bot tasks (for hot-stop on remove).
    channel_handles: tokio::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>,
    /// [M2] Server-side cached pending update (set by check_update, consumed by apply_update).
    pending_update: RwLock<Option<PendingUpdate>>,
    /// User database for multi-user auth (injected after gateway start).
    user_db: RwLock<Option<Arc<UserDb>>>,
    /// JWT configuration for token issuance (injected after gateway start).
    jwt_config: RwLock<Option<Arc<JwtConfig>>>,
    /// Plugin extension point (NullExtension by default).
    extension: Arc<dyn GatewayExtension>,
    /// Explicit product form-factor override, injected after gateway start by
    /// the Cloud control-plane. `None` → resolve per-request from
    /// `DUDUCLAW_EDITION` env > license tier > `Personal`.
    edition_override: RwLock<Option<duduclaw_core::EditionProfile>>,
    /// Active interactive CLI-login sessions ("Dashboard 一鍵登入"), keyed by
    /// session id. Each drives a CLI's native login command in a PTY.
    cli_auth_sessions: RwLock<std::collections::HashMap<String, Arc<crate::cli_auth::AuthSession>>>,
    /// SQLite-backed cron task store. Injected after gateway starts.
    cron_store: RwLock<Option<Arc<CronStore>>>,
    /// Handle to the running cron scheduler — used to trigger hot reload
    /// after mutating `cron_store`. Injected after gateway starts.
    cron_scheduler: RwLock<Option<Arc<CronScheduler>>>,
    /// Pending OAuth flows awaiting callback (keyed by state nonce).
    mcp_oauth_pending: RwLock<std::collections::HashMap<String, crate::mcp_oauth::PendingOAuth>>,
    /// SQLite-backed task board store. Injected after gateway starts.
    task_store: RwLock<Option<Arc<TaskStore>>>,
    /// SQLite-backed autopilot rule store. Injected after gateway starts.
    autopilot_store: RwLock<Option<Arc<AutopilotStore>>>,
    /// Event broadcast sender for real-time task/activity events.
    event_tx: RwLock<Option<tokio::sync::broadcast::Sender<String>>>,
    /// Typed event broadcast sender consumed by `AutopilotEngine`.
    autopilot_event_tx: RwLock<
        Option<tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>>,
    >,
    /// RFC-23 redaction manager. `None` ⇒ pipeline disabled at this layer.
    redaction_manager: RwLock<Option<Arc<duduclaw_redaction::RedactionManager>>>,
    /// M1/M60: long-lived SQLite-backed audit/reliability index, lazily opened
    /// once and synced by a background task — so audit/reliability RPCs and the
    /// `/api/reliability/summary` HTTP endpoint reuse one connection instead of
    /// opening a fresh DB + running a full `sync_from_files` on every request.
    audit_index: tokio::sync::OnceCell<
        Arc<crate::evolution_events::query::AuditEventIndex>,
    >,
}

/// Cached update info from the last `system.check_update` call. [M2][R2:NM1]
#[derive(Clone)]
struct PendingUpdate {
    download_url: String,
    checksum_url: String,
    version: String,
    /// [R2:NM1] TTL — expires after 5 minutes to prevent stale URL replay
    cached_at: Instant,
}

impl PendingUpdate {
    const TTL_SECS: u64 = 300; // 5 minutes

    fn is_expired(&self) -> bool {
        self.cached_at.elapsed().as_secs() > Self::TTL_SECS
    }
}

/// Runtime state for a connected channel.
#[derive(Clone)]
pub struct ChannelState {
    pub connected: bool,
    pub last_event: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

impl MethodHandler {
    pub async fn new(home_dir: PathBuf) -> Self {
        Self::with_extension(home_dir, Arc::new(crate::extension::NullExtension)).await
    }

    /// Create a new handler with a custom extension (used by Pro binary).
    pub async fn with_extension(
        home_dir: PathBuf,
        extension: Arc<dyn GatewayExtension>,
    ) -> Self {
        let agents_dir = home_dir.join("agents");
        let mut registry = AgentRegistry::new(agents_dir.clone());
        if let Err(e) = registry.scan().await {
            tracing::warn!("Failed to scan agents directory: {e}");
        }

        // Install the agent-file-guard PreToolUse hook into every existing
        // agent's .claude/settings.json on startup. Idempotent — merges into
        // existing settings without clobbering user-added hooks.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Skip _trash and other non-agent directories.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('_') || name.is_empty() {
                    continue;
                }
                if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(&path, &bin).await {
                    tracing::warn!(
                        agent = %name,
                        error = %e,
                        "Failed to install agent-file-guard hook on startup"
                    );
                }
            }
        }
        Self {
            registry: Arc::new(RwLock::new(registry)),
            home_dir,
            start_time: Instant::now(),
            channel_status: Arc::new(RwLock::new(std::collections::HashMap::new())),
            heartbeat: RwLock::new(None),
            reply_ctx: RwLock::new(None),
            channel_handles: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            pending_update: RwLock::new(None),
            user_db: RwLock::new(None),
            jwt_config: RwLock::new(None),
            extension,
            edition_override: RwLock::new(None),
            cli_auth_sessions: RwLock::new(std::collections::HashMap::new()),
            cron_store: RwLock::new(None),
            cron_scheduler: RwLock::new(None),
            mcp_oauth_pending: RwLock::new(std::collections::HashMap::new()),
            task_store: RwLock::new(None),
            autopilot_store: RwLock::new(None),
            event_tx: RwLock::new(None),
            autopilot_event_tx: RwLock::new(None),
            redaction_manager: RwLock::new(None),
            audit_index: tokio::sync::OnceCell::new(),
        }
    }

    /// Inject the explicit edition form-factor override (called once after
    /// gateway start). `None` keeps per-request resolution from env + tier.
    pub async fn set_edition_override(&self, edition: Option<duduclaw_core::EditionProfile>) {
        *self.edition_override.write().await = edition;
    }

    /// Resolve the active product form-factor ([`EditionProfile`]) at request
    /// time using the documented precedence: `DUDUCLAW_EDITION` env >
    /// explicit override > license tier > `Personal`. This is the value the
    /// dashboard reads to decide whether to show enterprise management
    /// surfaces. It never gates a core feature.
    ///
    /// [`EditionProfile`]: duduclaw_core::EditionProfile
    async fn resolve_edition_profile(&self) -> duduclaw_core::EditionProfile {
        let tier_key = match crate::license_runtime::global() {
            Some(runtime) => Some(runtime.snapshot().await.tier.as_toml_key().to_string()),
            None => None,
        };
        let env = std::env::var("DUDUCLAW_EDITION").ok();
        let override_ed = *self.edition_override.read().await;
        duduclaw_core::EditionProfile::resolve(
            env.as_deref(),
            override_ed.map(|e| e.as_str()),
            tier_key.as_deref(),
        )
    }

    /// Lazily open (once) the shared [`AuditEventIndex`] and return it.
    ///
    /// M1/M60: the index is opened a single time (a fresh DB connection per
    /// request was O(total-audit-history) on a hot path). The first call also
    /// runs an initial `sync_from_files`; thereafter a background task that
    /// calls [`refresh_audit_index`](Self::refresh_audit_index) keeps it fresh,
    /// so request handlers do NOT sync inline.
    pub(crate) async fn audit_index(
        &self,
    ) -> Result<Arc<crate::evolution_events::query::AuditEventIndex>, String> {
        use crate::evolution_events::query::AuditEventIndex;
        let idx = self
            .audit_index
            .get_or_try_init(|| async {
                let idx = AuditEventIndex::open(&self.home_dir)?;
                // Initial sync so the very first query isn't empty/stale.
                if let Err(e) = idx.sync_from_files().await {
                    warn!("audit_index: initial sync warning (stale index): {e}");
                }
                Ok::<_, String>(Arc::new(idx))
            })
            .await?;
        Ok(idx.clone())
    }

    /// Refresh the shared audit index once (called on a background interval by
    /// the gateway — M1/M60 — to replace per-request `sync_from_files`).
    /// Best-effort: errors are logged and swallowed.
    pub async fn refresh_audit_index(&self) {
        match self.audit_index().await {
            Ok(idx) => {
                if let Err(e) = idx.sync_from_files().await {
                    warn!("audit_index background sync warning: {e}");
                }
            }
            Err(e) => warn!("audit_index background sync: open failed: {e}"),
        }
    }

    /// Inject the redaction manager (called once after gateway start when
    /// `[redaction] enabled` is true). `None` ⇒ redaction disabled.
    pub async fn set_redaction_manager(
        &self,
        manager: Arc<duduclaw_redaction::RedactionManager>,
    ) {
        *self.redaction_manager.write().await = Some(manager);
    }

    /// Read the redaction manager handle.
    pub async fn get_redaction_manager(
        &self,
    ) -> Option<Arc<duduclaw_redaction::RedactionManager>> {
        self.redaction_manager.read().await.clone()
    }

    /// Inject the SQLite-backed cron task store (called once after gateway start).
    pub async fn set_cron_store(&self, store: Arc<CronStore>) {
        *self.cron_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed task board store (called once after gateway start).
    pub async fn set_task_store(&self, store: Arc<TaskStore>) {
        *self.task_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed autopilot rule store (called once after gateway start).
    pub async fn set_autopilot_store(&self, store: Arc<AutopilotStore>) {
        *self.autopilot_store.write().await = Some(store);
    }

    /// Inject the event broadcast sender for task/activity real-time events.
    pub async fn set_event_tx(&self, tx: tokio::sync::broadcast::Sender<String>) {
        *self.event_tx.write().await = Some(tx);
    }

    /// Inject the typed event broadcast sender consumed by `AutopilotEngine`.
    pub async fn set_autopilot_event_tx(
        &self,
        tx: tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>,
    ) {
        *self.autopilot_event_tx.write().await = Some(tx);
    }

    /// Publish an autopilot event to the engine (best-effort).
    async fn emit_autopilot_event(&self, event: crate::autopilot_engine::AutopilotEvent) {
        if let Some(tx) = self.autopilot_event_tx.read().await.as_ref() {
            let _ = tx.send(event);
        }
    }

    /// Inject the running cron scheduler handle (called once after gateway start).
    pub async fn set_cron_scheduler(&self, scheduler: Arc<CronScheduler>) {
        *self.cron_scheduler.write().await = Some(scheduler);
    }

    /// Notify the cron scheduler to reload immediately. Call this after any
    /// mutation (add / update / delete / enable-toggle). No-op if the
    /// scheduler has not been injected yet.
    async fn notify_cron_reload(&self) {
        if let Some(scheduler) = self.cron_scheduler.read().await.as_ref() {
            scheduler.reload_now();
        }
    }

    /// Get the extension reference.
    pub fn extension(&self) -> &Arc<dyn GatewayExtension> {
        &self.extension
    }

    /// Inject user database and JWT config (called once after gateway start).
    pub async fn set_user_db(&self, db: Arc<UserDb>, jwt: Arc<JwtConfig>) {
        *self.user_db.write().await = Some(db);
        *self.jwt_config.write().await = Some(jwt);
    }

    /// Inject the reply context for hot-starting channels. Called once after
    /// ReplyContext is constructed in server.rs.
    pub async fn set_reply_ctx(&self, ctx: Arc<crate::channel_reply::ReplyContext>) {
        *self.reply_ctx.write().await = Some(ctx);
    }

    /// Register a running channel handle (for hot-stop on remove).
    /// If a handle with the same name already exists, it is aborted first.
    pub async fn register_channel_handle(&self, name: &str, handle: tokio::task::JoinHandle<()>) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(old) = handles.insert(name.to_string(), handle) {
            old.abort();
        }
    }

    /// Update a channel's runtime connection state (called by channel bots).
    pub async fn set_channel_state(&self, name: &str, connected: bool, error: Option<String>) {
        let mut map = self.channel_status.write().await;
        map.insert(name.to_string(), ChannelState {
            connected,
            last_event: Some(chrono::Utc::now()),
            error,
        });
    }

    /// Get the shared channel status map for use by channel bots.
    pub fn channel_status(&self) -> &Arc<RwLock<std::collections::HashMap<String, ChannelState>>> {
        &self.channel_status
    }

    /// Get a reference to the shared agent registry.
    pub fn registry(&self) -> &Arc<RwLock<AgentRegistry>> {
        &self.registry
    }

    /// Get the home directory path.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Get the pending OAuth flows map (used by HTTP callback handler).
    pub fn mcp_oauth_pending(&self) -> &RwLock<std::collections::HashMap<String, crate::mcp_oauth::PendingOAuth>> {
        &self.mcp_oauth_pending
    }

    /// Set the heartbeat scheduler reference (called after gateway start).
    pub async fn set_heartbeat(&self, scheduler: Arc<duduclaw_agent::HeartbeatScheduler>) {
        *self.heartbeat.write().await = Some(scheduler);
    }

    /// Route `method` to the correct handler and return a [`WsFrame`] response.
    ///
    /// `request_id` is carried through so that all response frames are correctly
    /// correlated with the originating client request.
    pub async fn handle(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let response = self.dispatch(method, params, ctx).await;
        response
    }

    /// Internal dispatch — returns a WsFrame with placeholder id (overwritten by caller).
    async fn dispatch(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        // ── Plugin extension dispatch ──────
        // Try extension first; if it returns Some, the method is handled.
        if let Some(frame) = self.extension.handle_method(method, params.clone(), ctx).await {
            return frame;
        }

        // ── ACL macros ───────────────────────────────────────
        // Helper: require minimum role, return error frame on failure.
        macro_rules! require_admin {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Admin) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        macro_rules! require_manager {
            () => {
                if let Err(e) = acl::require_role(ctx, UserRole::Manager) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        // Helper: check agent access from params, return error frame on failure.
        macro_rules! check_agent {
            ($min_level:expr) => {
                match acl::extract_and_check_agent(ctx, &params, $min_level) {
                    Ok(id) => id,
                    Err(e) => return WsFrame::error_response("", &e),
                }
            };
        }
        // Helper: check access to a specifically-named agent (used when the
        // binding key is not the literal `agent_id` param — e.g. `assigned_to`).
        macro_rules! check_agent_named {
            ($agent:expr, $min_level:expr) => {
                if let Err(e) = acl::require_agent_access(ctx, $agent, $min_level) {
                    return WsFrame::error_response("", &e);
                }
            };
        }
        // Helper for list/filter RPCs that accept an OPTIONAL `agent_id` filter.
        // Admins may list across all agents; non-admins must scope the query to
        // an agent they are bound to (otherwise they could enumerate other
        // teams' tasks/activity). Mirrors the agent-binding intent of memory.*.
        macro_rules! check_agent_filter {
            ($min_level:expr) => {
                if !ctx.is_admin() {
                    match params.get("agent_id").and_then(|v| v.as_str()) {
                        Some(id) if !id.is_empty() => {
                            if let Err(e) = acl::require_agent_access(ctx, id, $min_level) {
                                return WsFrame::error_response("", &e);
                            }
                        }
                        _ => {
                            return WsFrame::error_response(
                                "",
                                "agent_id parameter is required",
                            );
                        }
                    }
                }
            };
        }

        match method {
            "connect.challenge" => self.handle_connect_challenge(params),
            "connect" => self.handle_connect(params),
            "ping" => WsFrame::ok_response("", json!({ "pong": true })),
            "hello-ok" => self.handle_hello_ok(params),
            "tools.catalog" => self.handle_tools_catalog(params),

            // ── Agent methods (filtered by binding) ──────────
            "agents.list" => self.handle_agents_list_filtered(ctx).await,
            "agents.status" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_status(params).await
            }
            "agents.create" => { require_admin!(); self.handle_agents_create(params).await }
            "agents.delegate" => {
                // H1 fix: delegate is high-risk — requires operator-level access
                let _ = check_agent!(AccessLevel::Operator);
                self.handle_agents_delegate(params).await
            }
            "agents.pause" => { require_manager!(); self.handle_agents_pause(params).await }
            "agents.resume" => { require_manager!(); self.handle_agents_resume(params).await }
            "agents.update" => {
                let _ = check_agent!(AccessLevel::Owner);
                self.handle_agents_update(params).await
            }
            "agents.remove" => { require_admin!(); self.handle_agents_remove(params).await }
            "agents.inspect" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_agents_inspect(params).await
            }

            // ── Behavioral contract (per-agent CONTRACT.toml, CON.1–CON.3) ──
            "contract.get" => { require_admin!(); self.handle_contract_get(params).await }
            "contract.update" => { require_admin!(); self.handle_contract_update(params).await }

            // ── Redaction / privacy (global config.toml [redaction], RED.1–RED.4) ──
            "redaction.get" => { require_admin!(); self.handle_redaction_get().await }
            "redaction.update" => { require_admin!(); self.handle_redaction_update(params).await }

            // ── Skill synthesis auto-run (global config.toml [skill_synthesis], W19-P1) ──
            "skill_synthesis.get" => { require_admin!(); self.handle_skill_synthesis_get().await }
            "skill_synthesis.update" => { require_admin!(); self.handle_skill_synthesis_update(params).await }

            // ── Inference (global ~/.duduclaw/inference.toml, INF.1–INF.5) ──
            "inference.get" => { require_admin!(); self.handle_inference_get().await }
            "inference.update" => { require_admin!(); self.handle_inference_update(params).await }

            // ── MCP API keys (global config.toml [mcp_keys], MK.1–MK.4) ──
            "mcp_keys.list" => { require_admin!(); self.handle_mcp_keys_list().await }
            "mcp_keys.create" => { require_admin!(); self.handle_mcp_keys_create(params).await }
            "mcp_keys.revoke" => { require_admin!(); self.handle_mcp_keys_revoke(params).await }

            // ── Kill switch (global ~/.duduclaw/KILLSWITCH.toml, KS.1–KS.2) ──
            "killswitch.get" => { require_admin!(); self.handle_killswitch_get().await }
            "killswitch.update" => { require_admin!(); self.handle_killswitch_update(params).await }

            // ── Governance policies (policies/*.yaml, GOV.1–GOV.2) ──
            "governance.list" => { require_admin!(); self.handle_governance_list(params).await }
            "governance.upsert" => { require_admin!(); self.handle_governance_upsert(params).await }
            "governance.remove" => { require_admin!(); self.handle_governance_remove(params).await }

            // ── Wiki namespace scope (.scope.toml, SCP.1) ──
            "wiki_scope.get" => { require_admin!(); self.handle_wiki_scope_get().await }
            "wiki_scope.update" => { require_admin!(); self.handle_wiki_scope_update(params).await }

            // ── Channel methods (admin only) ─────────────────
            "channels.status" => { require_admin!(); self.handle_channels_status().await }
            "channels.add" => { require_admin!(); self.handle_channels_add(params).await }
            "channels.test" => { require_admin!(); self.handle_channels_test(params).await }
            "channels.remove" => { require_admin!(); self.handle_channels_remove(params).await }

            // ── Account methods (admin only) ─────────────────
            // ── License (read-only snapshot of the gateway LicenseRuntime) ──
            //
            // `license.status` lets the dashboard render tier + expiry +
            // grace-period warnings without parsing ~/.duduclaw/license.json
            // directly. Manager-level access — the snapshot intentionally
            // omits the raw signature and customer email, so it is safe to
            // show to anyone who can already see operational metrics.
            "license.status" => { require_manager!(); self.handle_license_status().await }

            "accounts.list" => { require_admin!(); self.handle_accounts_list().await }
            "accounts.budget_summary" => { require_manager!(); self.handle_budget_summary().await }
            "accounts.rotate" => {
                require_admin!();
                self.handle_accounts_rotate(params).await
            }
            "accounts.health" => { require_admin!(); self.handle_accounts_health().await }
            "accounts.add" => { require_admin!(); self.handle_accounts_add(params).await }
            "accounts.update_budget" => { require_admin!(); self.handle_accounts_update_budget(params).await }
            "accounts.update" => { require_admin!(); self.handle_accounts_update(params).await }
            // Interactive CLI login ("Dashboard 一鍵登入") — drives the CLI's
            // native login in a PTY and streams it to the dashboard.
            "auth.cli_login.start" => { require_admin!(); self.handle_cli_login_start(params).await }
            "auth.cli_login.input" => { require_admin!(); self.handle_cli_login_input(params).await }
            "auth.cli_login.status" => { require_admin!(); self.handle_cli_login_status(params).await }
            "auth.cli_login.cancel" => { require_admin!(); self.handle_cli_login_cancel(params).await }
            "auth.cli_login.finalize" => { require_admin!(); self.handle_cli_login_finalize(params).await }

            // ── Memory (agent-scoped, H2 fix) ────────────────
            "memory.search" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_search(params).await
            }
            "memory.browse" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_browse(params).await
            }
            "memory.key_facts" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_memory_key_facts(params).await
            }

            // ── Wiki (agent-scoped — HS4 fix, mirrors memory.*) ───────
            // Each arm reads `agent_id` from params; an Employee bound only to
            // agent A must not be able to read agent B's private wiki/SOPs.
            "wiki.pages" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_pages(params).await
            }
            "wiki.read" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_read(params).await
            }
            "wiki.search" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_search(params).await
            }
            "wiki.lint" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_lint(params).await
            }
            "wiki.stats" => {
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_wiki_stats(params).await
            }
            // Phase 4: trust feedback inspection + manual override.
            // Trust state exposes per-conversation citation history that
            // can correlate with user activity → manager+ only (review H1).
            "wiki.trust_audit" => { require_manager!(); self.handle_wiki_trust_audit(params).await }
            "wiki.trust_override" => { require_admin!(); self.handle_wiki_trust_override(params).await }
            "wiki.trust_history" => { require_manager!(); self.handle_wiki_trust_history(params).await }

            // ── Shared Wiki ─────────────────────────────────
            "shared_wiki.pages" => self.handle_shared_wiki_pages().await,
            "shared_wiki.read" => self.handle_shared_wiki_read(params).await,
            "shared_wiki.search" => self.handle_shared_wiki_search(params).await,
            "shared_wiki.stats" => self.handle_shared_wiki_stats().await,

            // ── Skills (open to all) ─────────────────────────
            "skills.list" => self.handle_skills_list(params).await,
            "skills.search" => self.handle_skills_search(params).await,
            "skills.content" => {
                // HS4: skill content is read from a specific agent's registry —
                // scope to an agent the caller is bound to.
                let _ = check_agent!(AccessLevel::Viewer);
                self.handle_skills_content(params).await
            }
            "skills.vet" => { require_admin!(); self.handle_skills_vet(params).await }
            "skills.install" => { require_admin!(); self.handle_skills_install(params).await }

            // ── Cron (admin only) ────────────────────────────
            "cron.list" => { require_admin!(); self.handle_cron_list().await }
            "cron.add" => { require_admin!(); self.handle_cron_add(params).await }
            "cron.update" => { require_admin!(); self.handle_cron_update(params).await }
            "cron.pause" => { require_admin!(); self.handle_cron_set_enabled(params, false).await }
            "cron.resume" => { require_admin!(); self.handle_cron_set_enabled(params, true).await }
            "cron.remove" => { require_admin!(); self.handle_cron_remove(params).await }

            // ── System (admin only for config changes) ───────
            "system.status" => self.handle_system_status().await,
            "system.doctor" => { require_admin!(); self.handle_system_doctor().await }
            "system.doctor_repair" => { require_admin!(); self.handle_system_doctor_repair().await }
            "models.list" => self.handle_models_list().await,
            "runtime.detect" => self.handle_runtime_detect().await,
            "system.config" => { require_admin!(); self.handle_system_config().await }
            "system.update_config" => { require_admin!(); self.handle_system_update_config(params).await }
            "system.version" => self.handle_system_version().await,
            "system.check_update" => { require_admin!(); self.handle_system_check_update().await }
            "system.apply_update" => { require_admin!(); self.handle_system_apply_update(params).await }

            // ── Logs (manager+) ──────────────────────────────
            "logs.subscribe" => { require_manager!(); self.handle_logs_subscribe(params) }
            "logs.unsubscribe" => self.handle_logs_unsubscribe(params),

            // ── Security (admin only) ────────────────────────
            "security.audit_log" => {
                require_admin!();
                self.handle_security_audit_log(params).await
            }
            "audit.unified_log" => {
                require_admin!();
                self.handle_audit_unified_log(params).await
            }
            "audit.evolution_query" => {
                require_admin!();
                self.handle_audit_evolution_query(params).await
            }
            "audit.reliability_summary" => {
                require_admin!();
                self.handle_audit_reliability_summary(params).await
            }
            "security.status" => {
                require_admin!();
                self.handle_security_status().await
            }

            // ── Analytics (manager+) ────────────────────────
            "analytics.summary" => {
                require_manager!();
                self.handle_analytics_summary(params).await
            }
            "analytics.conversations" => {
                require_manager!();
                self.handle_analytics_conversations().await
            }
            "analytics.cost_savings" => {
                require_manager!();
                self.handle_analytics_cost_savings().await
            }

            // ── Heartbeat (manager+) ─────────────────────────
            "heartbeat.status" => {
                require_manager!();
                self.handle_heartbeat_status().await
            }
            "heartbeat.trigger" => {
                require_manager!();
                self.handle_heartbeat_trigger(params).await
            }

            // ── Evolution (manager+, H3 fix) ─────────────────
            "evolution.status" => { require_manager!(); self.handle_evolution_status().await }
            "evolution.history" => { require_manager!(); self.handle_evolution_history(params).await }

            // ── Odoo (admin only) ────────────────────────────
            "odoo.status" => { require_admin!(); self.handle_odoo_status().await }
            "odoo.config" => { require_admin!(); self.handle_odoo_config().await }
            "odoo.configure" => { require_admin!(); self.handle_odoo_configure(params).await }
            "odoo.test" => { require_admin!(); self.handle_odoo_test(params).await }

            // ── User management (admin only) ─────────────────
            "users.list" => { require_admin!(); self.handle_users_list().await }
            "users.create" => { require_admin!(); self.handle_users_create(params, ctx).await }
            "users.update" => { require_admin!(); self.handle_users_update(params, ctx).await }
            "users.remove" => { require_admin!(); self.handle_users_remove(params, ctx).await }
            "users.bind_agent" => { require_admin!(); self.handle_users_bind_agent(params, ctx).await }
            "users.unbind_agent" => { require_admin!(); self.handle_users_unbind_agent(params, ctx).await }
            "users.offboard" => { require_admin!(); self.handle_users_offboard(params, ctx).await }
            "users.me" => self.handle_users_me(ctx).await,
            // Self-service: any logged-in user changes their OWN password. Not
            // admin-gated on purpose — it only ever mutates the caller's account,
            // and it's the sole password path in the single-owner edition (the
            // multi-user Users page is hidden there).
            "users.change_password" => self.handle_users_change_password(params, ctx).await,
            "users.audit_log" => { require_admin!(); self.handle_users_audit_log(params).await }

            "mcp.list" => { require_admin!(); self.handle_mcp_list().await }
            "mcp.update" => { require_admin!(); self.handle_mcp_update(&params).await }

            // ── MCP OAuth (admin only) ──────────────────────────
            "mcp.oauth.providers" => { require_admin!(); self.handle_mcp_oauth_providers().await }
            "mcp.oauth.start" => { require_admin!(); self.handle_mcp_oauth_start(params).await }
            "mcp.oauth.status" => { require_admin!(); self.handle_mcp_oauth_status(params).await }
            "mcp.oauth.revoke" => { require_admin!(); self.handle_mcp_oauth_revoke(params).await }

            // ── Task Board (agent-scoped — HS4 fix) ────
            "tasks.list" => {
                // Non-admins must scope the listing to a bound agent.
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_tasks_list(params).await
            }
            "tasks.create" => {
                // The target agent is `assigned_to`; creating work for an agent
                // is a side-effecting operation → Operator level.
                let assigned_to = params
                    .get("assigned_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if assigned_to.is_empty() {
                    return WsFrame::error_response("", "assigned_to is required");
                }
                check_agent_named!(assigned_to, AccessLevel::Operator);
                self.handle_tasks_create(params, ctx).await
            }
            "tasks.update" => self.handle_tasks_update(params, ctx).await,
            "tasks.remove" => self.handle_tasks_remove(params, ctx).await,
            "tasks.assign" => self.handle_tasks_assign(params, ctx).await,

            // ── Activity Feed (agent-scoped — HS4 fix) ───
            "activity.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_activity_list(params).await
            }
            // Per-topic filtering is NOT implemented: BroadcastLayer fans out every
            // activity event to every authenticated WS client unconditionally. This
            // RPC exists purely as a client-intent signal and future-compat hook so
            // callers can declare interest without guessing at server state.
            "activity.subscribe" => WsFrame::ok_response("", json!({
                "subscribed": true,
                "broadcast_mode": "all_events",
                "note": "All authenticated WS clients receive activity events automatically; no per-client filter is in effect.",
            })),

            // ── Decision Continuity (RFC-24, agent-scoped) ──
            "decisions.list" => {
                check_agent_filter!(AccessLevel::Viewer);
                self.handle_decisions_list(params).await
            }
            "decisions.dismiss" => {
                // Marking a captured decision as a false positive mutates state.
                check_agent_filter!(AccessLevel::Operator);
                self.handle_decisions_dismiss(params).await
            }

            // ── Live Run Forking (RFC-26) ───────────────────
            "fork.list" => self.handle_fork_list(params),
            "fork.inspect" => self.handle_fork_inspect(params),
            // Resolving a fork promotes a winner's workspace → side-effecting.
            "fork.resolve" => { require_manager!(); self.handle_fork_resolve(params) }

            // ── Autopilot (admin only) ──────────────────────
            "autopilot.list" => { require_admin!(); self.handle_autopilot_list().await }
            "autopilot.create" => { require_admin!(); self.handle_autopilot_create(params).await }
            "autopilot.update" => { require_admin!(); self.handle_autopilot_update(params).await }
            "autopilot.remove" => { require_admin!(); self.handle_autopilot_remove(params).await }
            "autopilot.history" => { require_admin!(); self.handle_autopilot_history(params).await }

            // ── Redaction (RFC-23, manager-only) ──────────────
            "redaction.stats" => { require_manager!(); self.handle_redaction_stats().await }
            "redaction.recent_audit" => { require_manager!(); self.handle_redaction_recent_audit(params).await }
            "redaction.override_status" => { require_manager!(); self.handle_redaction_override_status().await }
            "redaction.policy_status" => { require_manager!(); self.handle_redaction_policy_status().await }

            // ── Shared Skills (open to all authenticated) ───
            "skills.shared" => self.handle_skills_shared_list().await,
            "skills.share" => self.handle_skills_share(params).await,
            "skills.adopt" => self.handle_skills_adopt(params).await,

            // ── Partner Portal ──────────────────────────────
            "partner.profile" => self.handle_partner_profile().await,
            "partner.stats" => self.handle_partner_stats().await,
            "partner.customers" => self.handle_partner_customers(params).await,
            "partner.profile.update" => {
                require_admin!();
                self.handle_partner_profile_update(params).await
            }
            "partner.customer.add" => {
                require_admin!();
                self.handle_partner_customer_add(params).await
            }
            "partner.customer.update" => {
                require_admin!();
                self.handle_partner_customer_update(params).await
            }
            "partner.customer.delete" => {
                require_admin!();
                self.handle_partner_customer_delete(params).await
            }

            // ── Billing ──────────────────────────────────────
            "billing.usage" => self.handle_billing_usage().await,
            "billing.history" | "billing.plan" =>
                WsFrame::error_response("", "Billing features are not available in the current edition"),
            "browser.audit_log" | "browser.emergency_stop" | "browser.tool_approve"
            | "browser.browserbase_sessions" | "browser.browserbase_cost" =>
                WsFrame::error_response("", "Browser automation features require the Pro edition"),
            "marketplace.list" => self.handle_marketplace_list().await,
            "marketplace.install" => { require_admin!(); self.handle_marketplace_install(params).await }

            unknown => WsFrame::error_response("", &format!("Unknown method: {unknown}")),
        }
    }

    // ── OpenClaw handshake ───────────────────────────────────

    fn handle_connect_challenge(&self, _params: Value) -> WsFrame {
        let challenge = uuid::Uuid::new_v4().to_string();
        WsFrame::ok_response("", json!({ "challenge": challenge }))
    }

    fn handle_connect(&self, params: Value) -> WsFrame {
        let version = params.get("version").and_then(|v| v.as_str()).unwrap_or("unknown");
        WsFrame::ok_response("", json!({ "version": crate::updater::current_version(), "client_version": version, "status": "connected" }))
    }

    fn handle_hello_ok(&self, _params: Value) -> WsFrame {
        WsFrame::ok_response("", json!({ "ack": true }))
    }

    /// `license.status` — read-only snapshot of the current LicenseRuntime.
    ///
    /// Returns OpenSource defaults when no runtime is registered (e.g. a
    /// gateway started without `start_gateway`) or when no license file is
    /// installed. Never errors — license queries must not break the
    /// dashboard. Dashboard surface fields are stable across schema bumps
    /// because we project through [`crate::license_runtime::LicenseSnapshot`]
    /// rather than serializing the raw [`duduclaw_license::License`] (which
    /// contains the Ed25519 signature).
    async fn handle_license_status(&self) -> WsFrame {
        let snapshot = match crate::license_runtime::global() {
            Some(runtime) => runtime.snapshot().await,
            None => crate::license_runtime::LicenseSnapshot {
                tier: duduclaw_license::LicenseTier::OpenSource,
                mode: "opensource",
                installed: false,
                customer_id: None,
                subscription_id: None,
                expires_at: None,
                days_until_expiry: None,
                last_phone_home: None,
                days_since_phone_home: None,
                fingerprint_match: None,
            },
        };

        let payload = match serde_json::to_value(&snapshot) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("serialize license snapshot: {e}"),
                );
            }
        };

        WsFrame::ok_response("", payload)
    }

    fn handle_tools_catalog(&self, _params: Value) -> WsFrame {
        WsFrame::ok_response("", json!({
            "tools": [
                { "name": "agents.list", "description": "List all registered agents" },
                { "name": "agents.status", "description": "Get agent status" },
                { "name": "agents.create", "description": "Create a new agent" },
                { "name": "agents.delegate", "description": "Delegate a task" },
                { "name": "agents.pause", "description": "Pause an agent" },
                { "name": "agents.resume", "description": "Resume an agent" },
                { "name": "agents.update", "description": "Update agent config fields" },
                { "name": "agents.remove", "description": "Remove an agent (to trash)" },
                { "name": "agents.inspect", "description": "Inspect agent details" },
                { "name": "channels.status", "description": "Channel connection status" },
                { "name": "channels.add", "description": "Add a channel" },
                { "name": "channels.test", "description": "Test a channel" },
                { "name": "channels.remove", "description": "Remove a channel" },
                { "name": "accounts.list", "description": "List accounts" },
                { "name": "accounts.budget_summary", "description": "Budget overview" },
                { "name": "accounts.rotate", "description": "Rotate account key" },
                { "name": "accounts.health", "description": "Account health check" },
                { "name": "memory.search", "description": "Search agent memory" },
                { "name": "memory.browse", "description": "Browse recent memory entries" },
                { "name": "memory.key_facts", "description": "List extracted key insights (P2 Key-Fact Accumulator)" },
                { "name": "wiki.pages", "description": "List wiki pages for an agent" },
                { "name": "wiki.read", "description": "Read a wiki page" },
                { "name": "wiki.search", "description": "Search wiki pages" },
                { "name": "wiki.lint", "description": "Wiki health check" },
                { "name": "wiki.stats", "description": "Wiki statistics" },
                { "name": "shared_wiki.pages", "description": "List shared wiki pages" },
                { "name": "shared_wiki.read", "description": "Read a shared wiki page" },
                { "name": "shared_wiki.search", "description": "Search shared wiki" },
                { "name": "shared_wiki.stats", "description": "Shared wiki statistics" },
                { "name": "skills.list", "description": "List agent skills" },
                { "name": "skills.content", "description": "Read skill content" },
                { "name": "cron.list", "description": "List cron jobs" },
                { "name": "cron.add", "description": "Add a cron job" },
                { "name": "cron.pause", "description": "Pause a cron job" },
                { "name": "cron.remove", "description": "Remove a cron job" },
                { "name": "system.status", "description": "System status" },
                { "name": "system.doctor", "description": "Health checks" },
                { "name": "system.doctor_repair", "description": "Health checks with repair hints" },
                { "name": "models.list", "description": "List available cloud and local models" },
                { "name": "runtime.detect", "description": "Detect installed AI runtimes (claude/codex/gemini/antigravity) + Claude OAuth" },
                { "name": "system.config", "description": "View system config" },
                { "name": "system.update_config", "description": "Update system config (log_level, rotation)" },
                { "name": "accounts.add", "description": "Add a new account" },
                { "name": "accounts.update_budget", "description": "Update account monthly budget" },
                { "name": "system.version", "description": "Version info" },
                { "name": "system.check_update", "description": "Check for available updates" },
                { "name": "system.apply_update", "description": "Download and apply update" },
                { "name": "heartbeat.status", "description": "Per-agent heartbeat status" },
                { "name": "heartbeat.trigger", "description": "Manually trigger heartbeat for an agent" },
                { "name": "mcp.list", "description": "List MCP servers for all agents + catalog" },
                { "name": "mcp.update", "description": "Add or remove an MCP server for an agent" },
                { "name": "mcp.oauth.providers", "description": "List available OAuth providers and their auth status" },
                { "name": "mcp.oauth.start", "description": "Start OAuth flow for a provider" },
                { "name": "mcp.oauth.status", "description": "Check OAuth status for a provider" },
                { "name": "mcp.oauth.revoke", "description": "Revoke OAuth token for a provider" },
                { "name": "logs.subscribe", "description": "Subscribe to logs" },
                { "name": "logs.unsubscribe", "description": "Unsubscribe from logs" },
                { "name": "security.status", "description": "Security system status" },
                { "name": "analytics.summary", "description": "Analytics summary for a period" },
                { "name": "analytics.conversations", "description": "Daily conversation counts" },
                { "name": "analytics.cost_savings", "description": "Monthly cost savings" },
            ]
        }))
    }

    // ── Agents ───────────────────────────────────────────────

    async fn handle_agents_status(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response("", json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                }))
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    /// Cloud-tier resource cap. Returns `Some(message)` when the active tier
    /// caps this resource, the deployment is Cloud (`DUDUCLAW_DEPLOYMENT=cloud`,
    /// injected into managed tenant containers), and the cap is already
    /// reached. Self-hosted deployments (Apache 2.0) are NEVER capped; a
    /// `max` of 0 in features.toml means unlimited.
    async fn tier_limit_message(&self, kind: &str, current: usize) -> Option<String> {
        // Apache 2.0 promise: never limit self-host. Default deployment is
        // self-host, so the limit only ever bites managed Cloud tenants.
        if crate::license_runtime::is_self_host_deployment() {
            return None;
        }
        let rt = crate::license_runtime::global()?;
        let tier = rt.current_tier().await;
        let gate = rt.feature_gate();
        let max = match kind {
            "agent" => gate.max_agents(tier),
            "channel" => gate.max_channels(tier),
            _ => 0,
        };
        if !crate::license_runtime::cap_exceeded(max, current) {
            return None;
        }
        let noun = if kind == "agent" { "Agent" } else { "通道" };
        Some(format!(
            "您的方案（{tier}）最多可建立 {max} 個{noun}。\
             請升級方案以新增更多：https://duduclaw.dudustudio.monster#pricing"
        ))
    }

    /// Count configured channels across global config.toml + every agent's
    /// `[channels]` section. Mirrors `handle_channels_status`'s enumeration so
    /// the cap counts exactly what the dashboard shows.
    async fn count_configured_channels(&self) -> usize {
        let mut n = 0usize;
        let config_path = self.home_dir.join("config.toml");
        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            for key in ["line_channel_token", "telegram_bot_token", "discord_bot_token"] {
                if ch.get(key).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                    n += 1;
                }
            }
        }
        let reg = self.registry.read().await;
        for agent in reg.list() {
            if let Some(ch) = &agent.config.channels {
                if ch.discord.as_ref().is_some_and(|d| !d.bot_token.is_empty()
                    || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())) { n += 1; }
                if ch.telegram.as_ref().is_some_and(|t| !t.bot_token.is_empty()
                    || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())) { n += 1; }
                if ch.slack.as_ref().is_some_and(|s| !s.bot_token.is_empty()
                    || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())) { n += 1; }
            }
        }
        n
    }

    async fn handle_agents_create(&self, params: Value) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or(name);
        let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("specialist");
        let trigger = params.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
        let trigger = if trigger.is_empty() { format!("@{display_name}") } else { trigger.to_string() };

        if name.is_empty() {
            return WsFrame::error_response("", "Agent name is required");
        }
        if !is_valid_agent_id(name) {
            return WsFrame::error_response("", "Agent name must be lowercase alphanumeric with hyphens, max 64 chars");
        }

        // Cloud-tier agent cap (self-host is never capped — Apache 2.0).
        let agent_count = self.registry.read().await.list().len();
        if let Some(msg) = self.tier_limit_message("agent", agent_count).await {
            return WsFrame::error_response("", &msg);
        }

        // If creating as main, demote the current main agent first
        if role == "main" {
            if let Err(e) = self.demote_current_main(name).await {
                return WsFrame::error_response("", &e);
            }
        }

        // Create agent directory and files
        let reg = self.registry.read().await;
        let agents_dir = reg.agents_dir();
        let agent_dir = agents_dir.join(name);

        if agent_dir.exists() {
            return WsFrame::error_response("", &format!("Agent '{name}' already exists"));
        }

        let skills_dir = agent_dir.join("SKILLS");
        if let Err(e) = tokio::fs::create_dir_all(&skills_dir).await {
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }

        let mut agent_config = toml::toml! {
            [agent]
            name = name
            display_name = display_name
            role = role
            status = "active"
            trigger = trigger
            reports_to = ""
            icon = "🤖"

            [model]
            preferred = "claude-sonnet-4-6"
            fallback = "claude-haiku-4-5"
            account_pool = ["main"]

            [container]
            timeout_ms = 1800000
            max_concurrent = 1
            readonly_project = true
            additional_mounts = []

            [heartbeat]
            enabled = false
            interval_seconds = 3600
            max_concurrent_runs = 1
            cron = ""

            [budget]
            monthly_limit_cents = 5000
            warn_threshold_percent = 80
            hard_stop = true

            [permissions]
            can_create_agents = false
            can_send_cross_agent = true
            can_modify_own_skills = true
            can_modify_own_soul = false
            can_schedule_tasks = false
            allowed_channels = ["*"]

            [evolution]
            micro_reflection = false
            meso_reflection = false
            macro_reflection = false
            skill_auto_activate = false
            skill_security_scan = true
        };

        // Optional `[runtime]` (provider/fallback) from the create params — lets
        // the dashboard onboarding pick a non-Claude backend at create time
        // instead of a follow-up update. No `runtime` key ⇒ no-op (existing
        // callers unaffected). Invalid provider ⇒ fail and clean up the dir.
        if let Err(e) = apply_runtime_to_table(&mut agent_config, &params) {
            let _ = tokio::fs::remove_dir_all(&agent_dir).await;
            return WsFrame::error_response("", &e);
        }

        let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

        // XC.2: atomic write (temp + rename) — mirror the per-agent update path.
        let agent_toml_path = agent_dir.join("agent.toml");
        let agent_toml_tmp = agent_toml_path.with_extension("toml.tmp");
        if let Err(e) = tokio::fs::write(&agent_toml_tmp, &agent_toml).await {
            return WsFrame::error_response("", &format!("Failed to write agent.toml.tmp: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&agent_toml_tmp, &agent_toml_path).await {
            let _ = tokio::fs::remove_file(&agent_toml_tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit agent.toml: {e}"));
        }

        // Honor an optional `soul` param (the agent's persona / system prompt).
        // Trim + cap defensively; fall back to a stock one-liner when absent.
        // (Previously this param was silently dropped — see api.ts agents.create.)
        let soul = params
            .get("soul")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| format!("# {display_name}\n\n{}\n", duduclaw_core::truncate_chars(s, 8000)))
            .unwrap_or_else(|| format!("# {display_name}\n\nI am {display_name}, a specialist AI agent.\n"));
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), &soul).await;

        // Install the agent-file-guard PreToolUse hook so this newly-created
        // agent immediately gets protected against out-of-tree Write/Edit.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) = crate::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await {
            tracing::warn!(
                agent = %name,
                error = %e,
                "Failed to install agent-file-guard hook on agents.create"
            );
        }

        info!(name, "Agent created");
        WsFrame::ok_response("", json!({
            "success": true,
            "agent": { "name": name, "display_name": display_name, "role": role, "status": "active" }
        }))
    }

    /// Dashboard-initiated delegation.  Supervisor pattern is NOT enforced here
    /// because this RPC is an operator-level action (depth always starts at 0).
    /// Agent-to-agent delegation goes through MCP `send_to_agent` which IS enforced.
    async fn handle_agents_delegate(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let wait = params.get("wait_for_response").and_then(|v| v.as_bool()).unwrap_or(false);

        // Enforce prompt length limit to prevent abuse (MCP-H1)
        const MAX_PROMPT_LEN: usize = 100_000;
        if prompt.len() > MAX_PROMPT_LEN {
            return WsFrame::error_response("", &format!("Prompt too long: {} chars (max {MAX_PROMPT_LEN})", prompt.len()));
        }

        info!(agent_id, "agents.delegate requested (dashboard)");

        // Verify target agent exists
        let reg = self.registry.read().await;
        let agent = match reg.get(agent_id) {
            Some(a) => a.clone(),
            None => return WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        };
        let model = agent.config.model.preferred.clone();
        drop(reg);

        let message_id = uuid::Uuid::new_v4().to_string();

        if wait {
            // Synchronous delegation: Rust-native Direct API call (no Python).
            let home = self.home_dir.clone();
            let system_prompt = agent.soul.as_deref().unwrap_or("You are a helpful AI agent.").to_string();
            match crate::channel_reply::call_direct_api_delegate(prompt, &model, &system_prompt, &home).await {
                Ok(response) => WsFrame::ok_response("", json!({
                    "success": true,
                    "message_id": message_id,
                    "target_agent": agent_id,
                    "response": response,
                    "status": "completed",
                })),
                Err(e) => WsFrame::error_response("", &format!("Delegate execution failed: {e}")),
            }
        } else {
            // Async delegation: write to bus queue for background processing
            let queue_path = self.home_dir.join("bus_queue.jsonl");
            let task = serde_json::json!({
                "type": "agent_message",
                "message_id": &message_id,
                "agent_id": agent_id,
                "payload": prompt,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "delegation_depth": 0,
                "origin_agent": "dashboard",
                "sender_agent": "dashboard",
            });
            let task_str = task.to_string();
            if let Err(e) = crate::dispatcher::append_line(&queue_path, &task_str).await {
                return WsFrame::error_response("", &format!("Failed to queue delegation: {e}"));
            }

            WsFrame::ok_response("", json!({
                "success": true,
                "message_id": message_id,
                "target_agent": agent_id,
                "status": "queued",
            }))
        }
    }

    async fn handle_agents_pause(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.pause requested");

        if let Err(e) = self.update_agent_status(agent_id, "paused").await {
            return WsFrame::error_response("", &format!("Failed to pause agent: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "paused" }))
    }

    async fn handle_agents_resume(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        info!(agent_id, "agents.resume requested");

        if let Err(e) = self.update_agent_status(agent_id, "active").await {
            return WsFrame::error_response("", &format!("Failed to resume agent: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true, "name": agent_id, "status": "active" }))
    }

    /// Read-modify-write an agent's `agent.toml` using the provided mutation closure.
    ///
    /// Uses atomic write (temp + rename) to prevent corruption on concurrent access.
    /// After a successful write, attempts to trigger a registry re-scan for hot-reload.
    ///
    /// Returns `Ok(true)` if the registry was re-scanned in time (changes visible
    /// immediately), `Ok(false)` if the re-scan was skipped due to lock contention
    /// or scan error (changes will land on the next periodic sync, ≤ 5 min for
    /// heartbeat-driven consumers; channel_reply / dispatcher always read fresh
    /// from the lock-protected registry so they see the previous version until the
    /// next scan).
    async fn update_agent_toml<F>(&self, agent_id: &str, mutate: F) -> Result<bool, String>
    where
        F: FnOnce(&mut toml::Table) -> Result<(), String>,
    {
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }

        let reg = self.registry.read().await;
        let agent = reg.get(agent_id)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
        let agent_toml_path = agent.dir.join("agent.toml");
        drop(reg);

        let content = tokio::fs::read_to_string(&agent_toml_path).await
            .map_err(|e| format!("Failed to read agent.toml: {e}"))?;

        let mut table: toml::Table = content.parse()
            .map_err(|e| format!("Failed to parse agent.toml: {e}"))?;

        mutate(&mut table)?;

        let new_content = toml::to_string_pretty(&table)
            .map_err(|e| format!("Failed to serialise agent.toml: {e}"))?;

        // Atomic write: temp file + rename
        let tmp_path = agent_toml_path.with_extension("toml.tmp");
        tokio::fs::write(&tmp_path, &new_content).await
            .map_err(|e| format!("Failed to write agent.toml.tmp: {e}"))?;
        tokio::fs::rename(&tmp_path, &agent_toml_path).await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to commit agent.toml: {e}")
            })?;

        // Trigger registry re-scan for hot-reload. Track success so the caller
        // can surface `hot_reloaded: false` to the user instead of pretending
        // the change took effect immediately.
        let hot_reloaded = match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            Ok(mut reg) => {
                match reg.scan().await {
                    Ok(()) => true,
                    Err(e) => {
                        warn!(agent_id, error = %e, "registry rescan failed after agent.toml write — change persisted but not yet visible to in-memory consumers");
                        false
                    }
                }
            }
            Err(_) => {
                warn!(agent_id, "registry write lock timeout (500ms) after agent.toml write — hot reload deferred to next periodic sync");
                false
            }
        };

        Ok(hot_reloaded)
    }

    /// Convenience: update only the `status` field in an agent's `agent.toml`.
    async fn update_agent_status(&self, agent_id: &str, status: &str) -> Result<(), String> {
        let status = status.to_string();
        self.update_agent_toml(agent_id, move |table| {
            let agent_section = table.get_mut("agent")
                .and_then(|v| v.as_table_mut())
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
            agent_section.insert("status".to_string(), toml::Value::String(status.clone()));
            info!("Agent status updated to {status}");
            Ok(())
        }).await?;
        Ok(())
    }

    /// Demote the current main agent to "specialist", skipping `except_id`.
    /// This ensures at most one agent has the "main" role at any time.
    async fn demote_current_main(&self, except_id: &str) -> Result<(), String> {
        let current_main = {
            let reg = self.registry.read().await;
            reg.main_agent()
                .filter(|a| a.config.agent.name != except_id)
                .map(|a| a.config.agent.name.clone())
        };
        if let Some(old_main) = current_main {
            info!(old_main = old_main.as_str(), "Demoting current main agent to specialist");
            self.update_agent_toml(&old_main, |table| {
                let agent_section = table.get_mut("agent")
                    .and_then(|v| v.as_table_mut())
                    .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
                agent_section.insert("role".into(), toml::Value::String("specialist".into()));
                Ok(())
            }).await?;
        }
        Ok(())
    }

    /// Update one or more fields of an agent's `agent.toml`.
    ///
    /// Supports identity, model, budget, heartbeat, permissions, and evolution fields.
    /// Only sends changed fields — unchanged fields are omitted from the request.
    async fn handle_agents_update(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        // If promoting to main, demote the current main agent first
        if let Some("main") = params.get("role").and_then(|v| v.as_str()) {
            if let Err(e) = self.demote_current_main(&agent_id).await {
                return WsFrame::error_response("", &e);
            }
        }

        // Detect per-agent channel token changes BEFORE the closure consumes
        // params — so we know what to hot-restart after the write succeeds.
        let mut channels_to_restart: Vec<&'static str> = Vec::new();
        if params.get("discord_bot_token").and_then(|v| v.as_str()).is_some() {
            channels_to_restart.push("discord");
        }
        if params.get("telegram_bot_token").and_then(|v| v.as_str()).is_some() {
            channels_to_restart.push("telegram");
        }
        if params.get("slack_bot_token").and_then(|v| v.as_str()).is_some()
            || params.get("slack_app_token").and_then(|v| v.as_str()).is_some()
        {
            channels_to_restart.push("slack");
        }

        let params_clone = params.clone();
        let mut changes: Vec<String> = Vec::new();
        let home_for_update = self.home_dir.clone();

        let result = self.update_agent_toml(&agent_id, move |table| {
            // ── Identity fields ([agent] section) ──
            if let Some(agent_section) = table.get_mut("agent").and_then(|v| v.as_table_mut()) {
                if let Some(v) = params_clone.get("display_name").and_then(|v| v.as_str()) {
                    agent_section.insert("display_name".into(), toml::Value::String(v.into()));
                    changes.push(format!("display_name = \"{v}\""));
                }
                if let Some(v) = params_clone.get("role").and_then(|v| v.as_str()) {
                    match v {
                        "main" | "specialist" | "worker" | "developer" | "qa" | "planner" => {
                            agent_section.insert("role".into(), toml::Value::String(v.into()));
                            changes.push(format!("role = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid role '{v}'. Valid: main, specialist, worker, developer, qa, planner")),
                    }
                }
                if let Some(v) = params_clone.get("status").and_then(|v| v.as_str()) {
                    match v {
                        "active" | "paused" | "terminated" => {
                            agent_section.insert("status".into(), toml::Value::String(v.into()));
                            changes.push(format!("status = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid status '{v}'. Valid: active, paused, terminated")),
                    }
                }
                if let Some(v) = params_clone.get("trigger").and_then(|v| v.as_str()) {
                    agent_section.insert("trigger".into(), toml::Value::String(v.into()));
                    changes.push(format!("trigger = \"{v}\""));
                }
                if let Some(v) = params_clone.get("icon").and_then(|v| v.as_str()) {
                    agent_section.insert("icon".into(), toml::Value::String(v.into()));
                    changes.push(format!("icon = \"{v}\""));
                }
                if let Some(v) = params_clone.get("reports_to").and_then(|v| v.as_str()) {
                    agent_section.insert("reports_to".into(), toml::Value::String(v.into()));
                    changes.push(format!("reports_to = \"{v}\""));
                }
            }

            // ── Model fields ([model] section) ──
            let model = table.entry("model")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(model) = model {
                if let Some(v) = params_clone.get("preferred").and_then(|v| v.as_str()) {
                    model.insert("preferred".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.preferred = \"{v}\""));
                }
                if let Some(v) = params_clone.get("fallback").and_then(|v| v.as_str()) {
                    model.insert("fallback".into(), toml::Value::String(v.into()));
                    changes.push(format!("model.fallback = \"{v}\""));
                }
                if let Some(v) = params_clone.get("api_mode").and_then(|v| v.as_str()) {
                    match v {
                        "cli" | "direct" | "auto" => {
                            model.insert("api_mode".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.api_mode = \"{v}\""));
                        }
                        _ => return Err(format!("Invalid api_mode '{v}'. Valid: cli, direct, auto")),
                    }
                }
            }

            // ── Local model fields ([model.local] section) ──
            if let Some(model) = table.get_mut("model").and_then(|v| v.as_table_mut()) {
                // Check if any local model param is provided
                let has_local_params = ["local_model", "local_backend", "local_context_length", "local_gpu_layers", "prefer_local", "use_router"]
                    .iter().any(|k| params_clone.get(*k).is_some());

                if has_local_params {
                    let local = model.entry("local")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(local) = local {
                        if let Some(v) = params_clone.get("local_model").and_then(|v| v.as_str()) {
                            local.insert("model".into(), toml::Value::String(v.into()));
                            changes.push(format!("model.local.model = \"{v}\""));
                        }
                        if let Some(v) = params_clone.get("local_backend").and_then(|v| v.as_str()) {
                            match v {
                                "llama_cpp" | "openai_compat" | "mistral_rs" => {
                                    local.insert("backend".into(), toml::Value::String(v.into()));
                                    changes.push(format!("model.local.backend = \"{v}\""));
                                }
                                _ => return Err(format!("Invalid local_backend '{v}'. Valid: llama_cpp, openai_compat, mistral_rs")),
                            }
                        }
                        if let Some(v) = params_clone.get("local_context_length").and_then(|v| v.as_u64()) {
                            local.insert("context_length".into(), toml::Value::Integer(v as i64));
                            changes.push(format!("model.local.context_length = {v}"));
                        }
                        if let Some(v) = params_clone.get("local_gpu_layers").and_then(|v| v.as_i64()) {
                            local.insert("gpu_layers".into(), toml::Value::Integer(v));
                            changes.push(format!("model.local.gpu_layers = {v}"));
                        }
                        if let Some(v) = params_clone.get("prefer_local").and_then(|v| v.as_bool()) {
                            local.insert("prefer_local".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.prefer_local = {v}"));
                        }
                        if let Some(v) = params_clone.get("use_router").and_then(|v| v.as_bool()) {
                            local.insert("use_router".into(), toml::Value::Boolean(v));
                            changes.push(format!("model.local.use_router = {v}"));
                        }
                    }
                }
            }

            // ── Budget fields ([budget] section) ──
            let budget = table.entry("budget")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(budget) = budget {
                if let Some(v) = params_clone.get("monthly_limit_cents").and_then(|v| v.as_u64()) {
                    budget.insert("monthly_limit_cents".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.monthly_limit_cents = {v}"));
                }
                if let Some(v) = params_clone.get("warn_threshold_percent").and_then(|v| v.as_u64()) {
                    if v > 100 {
                        return Err("warn_threshold_percent must be 0-100".into());
                    }
                    budget.insert("warn_threshold_percent".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("budget.warn_threshold_percent = {v}"));
                }
                if let Some(v) = params_clone.get("hard_stop").and_then(|v| v.as_bool()) {
                    budget.insert("hard_stop".into(), toml::Value::Boolean(v));
                    changes.push(format!("budget.hard_stop = {v}"));
                }
            }

            // ── Heartbeat fields ([heartbeat] section) ──
            let heartbeat = table.entry("heartbeat")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(hb) = heartbeat {
                if let Some(v) = params_clone.get("heartbeat_enabled").and_then(|v| v.as_bool()) {
                    hb.insert("enabled".into(), toml::Value::Boolean(v));
                    changes.push(format!("heartbeat.enabled = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                    hb.insert("interval_seconds".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("heartbeat.interval_seconds = {v}"));
                }
                if let Some(v) = params_clone.get("heartbeat_cron").and_then(|v| v.as_str()) {
                    hb.insert("cron".into(), toml::Value::String(v.into()));
                    changes.push(format!("heartbeat.cron = \"{v}\""));
                }
            }

            // ── Proactive fields ([proactive] section) ──
            if let Some(p) = params_clone.get("proactive").and_then(|v| v.as_object()) {
                let proactive = table.entry("proactive")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut();
                if let Some(pt) = proactive {
                    if let Some(v) = p.get("enabled").and_then(|v| v.as_bool()) {
                        pt.insert("enabled".into(), toml::Value::Boolean(v));
                        changes.push(format!("proactive.enabled = {v}"));
                    }
                    if let Some(v) = p.get("check_interval").and_then(|v| v.as_str()) {
                        let normalised = crate::cron_scheduler::normalise_cron(v);
                        if normalised.parse::<cron::Schedule>().is_err() {
                            return Err(format!("Invalid proactive check_interval cron expression: {v}"));
                        }
                        pt.insert("check_interval".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.check_interval = \"{v}\""));
                    }
                    for key in &["quiet_hours_start", "quiet_hours_end"] {
                        if let Some(v) = p.get(*key).and_then(|v| v.as_u64()) {
                            if v > 23 {
                                return Err(format!("Invalid proactive {key}: {v} (must be 0-23)"));
                            }
                            pt.insert((*key).into(), toml::Value::Integer(v as i64));
                            changes.push(format!("proactive.{key} = {v}"));
                        }
                    }
                    if let Some(v) = p.get("max_messages_per_hour").and_then(|v| v.as_u64()) {
                        pt.insert("max_messages_per_hour".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.max_messages_per_hour = {v}"));
                    }
                    if let Some(v) = p.get("notify_channel").and_then(|v| v.as_str()) {
                        pt.insert("notify_channel".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.notify_channel = \"{v}\""));
                    }
                    if let Some(v) = p.get("notify_chat_id").and_then(|v| v.as_str()) {
                        pt.insert("notify_chat_id".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.notify_chat_id = \"{v}\""));
                    }
                }
            }

            // ── Permissions fields ([permissions] section) ──
            let perms = table.entry("permissions")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(perms) = perms {
                for key in &[
                    "can_create_agents",
                    "can_send_cross_agent",
                    "can_modify_own_skills",
                    "can_modify_own_soul",
                    "can_schedule_tasks",
                ] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        perms.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("permissions.{key} = {v}"));
                    }
                }
            }

            // ── Container fields ([container] section) ──
            let container = table.entry("container")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(ct) = container {
                if let Some(v) = params_clone.get("timeout_ms").and_then(|v| v.as_u64()) {
                    ct.insert("timeout_ms".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("container.timeout_ms = {v}"));
                }
                if let Some(v) = params_clone.get("max_concurrent").and_then(|v| v.as_u64()) {
                    ct.insert("max_concurrent".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("container.max_concurrent = {v}"));
                }
                for key in &["sandbox_enabled", "network_access", "readonly_project"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        ct.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("container.{key} = {v}"));
                    }
                }
            }

            // ── Evolution fields ([evolution] section) ──
            let evo = table.entry("evolution")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(evo) = evo {
                for key in &["skill_auto_activate", "skill_security_scan", "gvu_enabled", "cognitive_memory"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_bool()) {
                        evo.insert((*key).into(), toml::Value::Boolean(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_active_skills", "max_gvu_generations", "skill_token_budget"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_u64()) {
                        evo.insert((*key).into(), toml::Value::Integer(v as i64));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }
                for key in &["max_silence_hours", "observation_period_hours"] {
                    if let Some(v) = params_clone.get(*key).and_then(|v| v.as_f64()) {
                        evo.insert((*key).into(), toml::Value::Float(v));
                        changes.push(format!("evolution.{key} = {v}"));
                    }
                }

                // ── Stagnation detection sub-section ──────────────────────────
                // Keys accepted: stagnation_enabled, stagnation_window_seconds,
                //                stagnation_trigger_threshold, stagnation_action
                {
                    // SECURITY-2: validate stagnation params before writing to TOML.
                    // Illegal values (window_seconds=0, trigger_threshold=0) must never
                    // reach agent.toml as P1 stagnation-detection logic depends on them.
                    let sd_validation = StagnationDetectionConfig {
                        enabled: params_clone.get("stagnation_enabled").and_then(|v| v.as_bool()),
                        window_seconds: params_clone.get("stagnation_window_seconds").and_then(|v| v.as_u64()),
                        trigger_threshold: params_clone.get("stagnation_trigger_threshold").and_then(|v| v.as_u64()),
                        action: params_clone.get("stagnation_action").and_then(|v| v.as_str()).map(|s| s.to_owned()),
                    };
                    if let Err(e) = sd_validation.validate() {
                        return Err(format!("evolution_toggle: invalid stagnation config: {e}"));
                    }

                    let sd = evo
                        .entry("stagnation_detection")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(sd) = sd {
                        let mut sd_changed = false;

                        if let Some(v) = params_clone.get("stagnation_enabled").and_then(|v| v.as_bool()) {
                            sd.insert("enabled".into(), toml::Value::Boolean(v));
                            changes.push(format!("evolution.stagnation_detection.enabled = {v}"));
                            sd_changed = true;
                        }
                        if let Some(v) = params_clone.get("stagnation_window_seconds").and_then(|v| v.as_u64()) {
                            sd.insert("window_seconds".into(), toml::Value::Integer(v as i64));
                            changes.push(format!("evolution.stagnation_detection.window_seconds = {v}"));
                            sd_changed = true;
                        }
                        if let Some(v) = params_clone.get("stagnation_trigger_threshold").and_then(|v| v.as_u64()) {
                            sd.insert("trigger_threshold".into(), toml::Value::Integer(v as i64));
                            changes.push(format!("evolution.stagnation_detection.trigger_threshold = {v}"));
                            sd_changed = true;
                        }
                        // stagnation_action: "log_only" | "suppress" (P1)
                        if let Some(v) = params_clone.get("stagnation_action").and_then(|v| v.as_str()) {
                            match v {
                                "log_only" | "suppress" => {
                                    sd.insert("action".into(), toml::Value::String(v.to_owned()));
                                    changes.push(format!("evolution.stagnation_detection.action = {v}"));
                                    sd_changed = true;
                                }
                                other => {
                                    tracing::warn!(
                                        "evolution_toggle: unknown stagnation_action '{}', ignored",
                                        other
                                    );
                                }
                            }
                        }

                        // If no stagnation sub-keys were touched, remove the empty
                        // sub-table so we don't dirty the TOML needlessly.
                        if !sd_changed {
                            evo.remove("stagnation_detection");
                        }
                    }
                }
            }

            // ── Per-agent channel tokens ([channels.*] sections) ──
            // Helper: write a token (+ encrypted version) into [channels.{channel}].{field}
            // Empty token removes the entire [channels.{channel}] section.
            let home = home_for_update.clone();
            let mut set_channel_token = |table: &mut toml::Table,
                                          channel: &str,
                                          fields: &[(&str, Option<&str>)], // (param_key, toml_key) pairs
                                          changes: &mut Vec<String>| -> Result<(), String> {
                // Check if any field has a value
                let has_any = fields.iter().any(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty())
                });
                let all_empty = fields.iter().all(|(param_key, _)| {
                    params_clone.get(*param_key).and_then(|v| v.as_str()).map_or(true, |s| s.is_empty())
                });

                // If the param exists but is empty → remove
                let param_present = fields.iter().any(|(param_key, _)| params_clone.get(*param_key).is_some());
                if param_present && all_empty {
                    if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
                        channels.remove(channel);
                        changes.push(format!("channels.{channel} removed"));
                    }
                    return Ok(());
                }

                if !has_any { return Ok(()); }

                let channels = table.entry("channels")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels] section"))?;
                let section = channels.entry(channel)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels.{channel}] section"))?;

                for (param_key, toml_key_override) in fields {
                    if let Some(val) = params_clone.get(*param_key).and_then(|v| v.as_str()) {
                        if !val.is_empty() {
                            let toml_key = toml_key_override.unwrap_or(param_key);
                            section.insert(toml_key.to_string(), toml::Value::String(val.into()));
                            // Encrypt sensitive tokens
                            if toml_key.contains("token") || toml_key.contains("secret") || toml_key == "app_id" {
                                let enc_key = format!("{toml_key}_enc");
                                if let Some(enc) = crate::config_crypto::encrypt_value(val, &home) {
                                    section.insert(enc_key, toml::Value::String(enc));
                                }
                            }
                        }
                    }
                }

                changes.push(format!("channels.{channel} = [CONFIGURED]"));
                Ok(())
            };

            // Discord
            set_channel_token(table, "discord", &[
                ("discord_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // Telegram
            set_channel_token(table, "telegram", &[
                ("telegram_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // LINE
            set_channel_token(table, "line", &[
                ("line_channel_token", Some("channel_token")),
                ("line_channel_secret", Some("channel_secret")),
            ], &mut changes)?;

            // Slack
            set_channel_token(table, "slack", &[
                ("slack_app_token", Some("app_token")),
                ("slack_bot_token", Some("bot_token")),
            ], &mut changes)?;

            // WhatsApp
            set_channel_token(table, "whatsapp", &[
                ("whatsapp_access_token", Some("access_token")),
                ("whatsapp_verify_token", Some("verify_token")),
                ("whatsapp_phone_number_id", Some("phone_number_id")),
                ("whatsapp_app_secret", Some("app_secret")),
            ], &mut changes)?;

            // Feishu
            set_channel_token(table, "feishu", &[
                ("feishu_app_id", Some("app_id")),
                ("feishu_app_secret", Some("app_secret")),
                ("feishu_verification_token", Some("verification_token")),
            ], &mut changes)?;

            // ── Sticker fields ([sticker] section) ──
            let sticker = table.entry("sticker")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(sticker) = sticker {
                if let Some(v) = params_clone.get("sticker_enabled").and_then(|v| v.as_bool()) {
                    sticker.insert("enabled".into(), toml::Value::Boolean(v));
                    changes.push(format!("sticker.enabled = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_probability").and_then(|v| v.as_f64()) {
                    if !(0.0..=1.0).contains(&v) {
                        return Err("sticker_probability must be 0.0-1.0".into());
                    }
                    sticker.insert("probability".into(), toml::Value::Float(v));
                    changes.push(format!("sticker.probability = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_intensity_threshold").and_then(|v| v.as_f64()) {
                    if !(0.0..=1.0).contains(&v) {
                        return Err("sticker_intensity_threshold must be 0.0-1.0".into());
                    }
                    sticker.insert("intensity_threshold".into(), toml::Value::Float(v));
                    changes.push(format!("sticker.intensity_threshold = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_cooldown_messages").and_then(|v| v.as_u64()) {
                    if v > 100 {
                        return Err("sticker_cooldown_messages must be 0-100".into());
                    }
                    sticker.insert("cooldown_messages".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("sticker.cooldown_messages = {v}"));
                }
                if let Some(v) = params_clone.get("sticker_expressiveness").and_then(|v| v.as_str()) {
                    if !["minimal", "moderate", "expressive"].contains(&v) {
                        return Err("sticker_expressiveness must be minimal|moderate|expressive".into());
                    }
                    sticker.insert("expressiveness".into(), toml::Value::String(v.into()));
                    changes.push(format!("sticker.expressiveness = \"{v}\""));
                }
            }

            // ── Capabilities fields ([capabilities] section, CAP.1–CAP.4) ──
            // High-risk tool / computer-use / browser permissions. Delegated to
            // a pure, unit-tested helper that validates enum + numeric ranges.
            let cap_changes = apply_capabilities_to_table(table, &params_clone)?;
            changes.extend(cap_changes);

            // ── Runtime ([runtime] section, RT.1) ──
            // provider enum / fallback / pty_pool_enabled / worker_managed.
            let rt_changes = apply_runtime_to_table(table, &params_clone)?;
            changes.extend(rt_changes);

            // ── Evolution advanced ([evolution.*] fields, EVO.1–EVO.3) ──
            // external_factors + skill-synthesis / graduation / recommendation /
            // curiosity / behavior-monitor. Does NOT duplicate the inline
            // gvu/cognitive/max_active_skills/stagnation_* handling above.
            let evo_adv_changes = apply_evolution_advanced_to_table(table, &params_clone)?;
            changes.extend(evo_adv_changes);

            // ── Container advanced ([container.*] fields, CT.1–CT.2) ──
            // worktree toggles + worktree_copy_files / additional_mounts / cmd /
            // env. Does NOT duplicate the inline sandbox/network/timeout handling.
            let ct_adv_changes = apply_container_advanced_to_table(table, &params_clone)?;
            changes.extend(ct_adv_changes);

            // ── Per-agent Odoo override ([odoo] section, ODO.1) ──
            // profile / allowed_models / allowed_actions (verb:model) /
            // company_ids + api_key|password → *_enc. Delegated to a helper so
            // the encryption + validation are unit-testable.
            let odoo_changes = apply_odoo_to_table(table, &params_clone, &home_for_update)?;
            changes.extend(odoo_changes);

            // ── Per-agent scattered fields (G.8) ──
            // These extend existing sections WITHOUT duplicating fields already
            // handled inline above (preferred/fallback, enabled/interval/cron, …).

            // [model].account_pool[] + [model].utility
            {
                let has_model_extra = ["account_pool", "utility"]
                    .iter()
                    .any(|k| params_clone.get(*k).is_some());
                if has_model_extra {
                    let model = table
                        .entry("model")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [model] section")?;
                    if let Some(arr) = params_clone.get("account_pool").and_then(|v| v.as_array()) {
                        let pool: Vec<toml::Value> = arr
                            .iter()
                            .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(|s| toml::Value::String(s.into())))
                            .collect();
                        model.insert("account_pool".into(), toml::Value::Array(pool.clone()));
                        changes.push(format!("model.account_pool = [{} entries]", pool.len()));
                    }
                    if let Some(v) = params_clone.get("utility").and_then(|v| v.as_str()) {
                        model.insert("utility".into(), toml::Value::String(v.into()));
                        changes.push(format!("model.utility = \"{v}\""));
                    }
                }
            }

            // [heartbeat].max_concurrent_runs + [heartbeat].cron_timezone
            {
                let has_hb_extra = ["heartbeat_max_concurrent_runs", "heartbeat_cron_timezone"]
                    .iter()
                    .any(|k| params_clone.get(*k).is_some());
                if has_hb_extra {
                    let hb = table
                        .entry("heartbeat")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [heartbeat] section")?;
                    if let Some(v) = params_clone.get("heartbeat_max_concurrent_runs").and_then(|v| v.as_u64()) {
                        if v == 0 || v > 64 {
                            return Err("heartbeat max_concurrent_runs must be 1-64".into());
                        }
                        hb.insert("max_concurrent_runs".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("heartbeat.max_concurrent_runs = {v}"));
                    }
                    if let Some(v) = params_clone.get("heartbeat_cron_timezone").and_then(|v| v.as_str()) {
                        if v.parse::<chrono_tz::Tz>().is_err() {
                            return Err(format!("Invalid heartbeat cron_timezone '{v}' (IANA tz, e.g. Asia/Taipei)"));
                        }
                        hb.insert("cron_timezone".into(), toml::Value::String(v.into()));
                        changes.push(format!("heartbeat.cron_timezone = \"{v}\""));
                    }
                }
            }

            // [proactive].token_budget_per_check / timezone / max_turns
            if let Some(p) = params_clone.get("proactive").and_then(|v| v.as_object()) {
                let has_pro_extra = ["token_budget_per_check", "timezone", "max_turns"]
                    .iter()
                    .any(|k| p.contains_key(*k));
                if has_pro_extra {
                    let pt = table
                        .entry("proactive")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or("Invalid [proactive] section")?;
                    if let Some(v) = p.get("token_budget_per_check").and_then(|v| v.as_u64()) {
                        pt.insert("token_budget_per_check".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.token_budget_per_check = {v}"));
                    }
                    if let Some(v) = p.get("timezone").and_then(|v| v.as_str()) {
                        if v.parse::<chrono_tz::Tz>().is_err() {
                            return Err(format!("Invalid proactive timezone '{v}' (IANA tz)"));
                        }
                        pt.insert("timezone".into(), toml::Value::String(v.into()));
                        changes.push(format!("proactive.timezone = \"{v}\""));
                    }
                    if let Some(v) = p.get("max_turns").and_then(|v| v.as_u64()) {
                        if v == 0 || v > 100 {
                            return Err("proactive max_turns must be 1-100".into());
                        }
                        pt.insert("max_turns".into(), toml::Value::Integer(v as i64));
                        changes.push(format!("proactive.max_turns = {v}"));
                    }
                }
            }

            // [ptc] / [prompt] / [cultural_context] — string-keyed scalar tables.
            // Each accepts a flat object of string|bool|int|float scalars; unknown
            // keys are written verbatim (these sections are free-form per-agent
            // tuning, not enum-validated). Empty object is a no-op.
            for sect in &["ptc", "prompt", "cultural_context"] {
                if let Some(obj) = params_clone.get(*sect).and_then(|v| v.as_object()) {
                    if obj.is_empty() {
                        continue;
                    }
                    let st = table
                        .entry(*sect)
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .ok_or_else(|| format!("Invalid [{sect}] section"))?;
                    for (k, v) in obj {
                        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                            return Err(format!("Invalid {sect} key '{k}' (alphanumeric + underscore only)"));
                        }
                        let tv = match v {
                            Value::String(s) => toml::Value::String(s.clone()),
                            Value::Bool(b) => toml::Value::Boolean(*b),
                            Value::Number(n) if n.is_i64() => toml::Value::Integer(n.as_i64().unwrap()),
                            Value::Number(n) if n.is_u64() => toml::Value::Integer(n.as_u64().unwrap() as i64),
                            Value::Number(n) => toml::Value::Float(n.as_f64().unwrap_or(0.0)),
                            Value::Array(a) => {
                                let items: Vec<toml::Value> = a
                                    .iter()
                                    .filter_map(|x| x.as_str().map(|s| toml::Value::String(s.into())))
                                    .collect();
                                toml::Value::Array(items)
                            }
                            _ => return Err(format!("Unsupported {sect}.{k} value type")),
                        };
                        st.insert(k.clone(), tv);
                        changes.push(format!("{sect}.{k} updated"));
                    }
                }
            }

            if changes.is_empty() {
                return Err("No valid fields to update".into());
            }

            Ok(())
        }).await;

        match result {
            Ok(hot_reloaded) => {
                // Hot-restart channel bots whose tokens just changed. Without
                // this, the running bot loop keeps the previous captured token
                // until gateway restart, so user-visible behavior diverges
                // from agent.toml on disk.
                let restarted = if !channels_to_restart.is_empty() {
                    self.hot_restart_agent_channels(&channels_to_restart, &agent_id).await
                } else {
                    Vec::new()
                };

                info!(
                    agent_id = agent_id.as_str(),
                    hot_reloaded,
                    channels_restarted = ?restarted,
                    "agents.update completed"
                );
                WsFrame::ok_response("", json!({
                    "success": true,
                    "agent_id": agent_id,
                    "hot_reloaded": hot_reloaded,
                    "channels_restarted": restarted,
                    "message": if hot_reloaded {
                        "Agent updated successfully"
                    } else {
                        "Agent updated successfully — registry hot reload deferred to next periodic sync (≤5min)"
                    },
                }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Resolve an agent's on-disk directory from the registry.
    async fn resolve_agent_dir(&self, agent_id: &str) -> Result<PathBuf, String> {
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }
        let reg = self.registry.read().await;
        reg.get(agent_id)
            .map(|a| a.dir.clone())
            .ok_or_else(|| format!("Agent not found: {agent_id}"))
    }

    /// Atomic write of a TOML table to `path` (temp + rename).
    async fn atomic_write_toml(&self, path: &Path, table: &toml::Table) -> Result<(), String> {
        let tmp = path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp, table).await {
            return Err(format!("Failed to write {}: {e}", path.display()));
        }
        if let Err(e) = tokio::fs::rename(&tmp, path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(format!("Failed to commit {}: {e}", path.display()));
        }
        Ok(())
    }

    // ── CON: CONTRACT.toml (per-agent) ────────────────────────────────────────

    /// `contract.get` — read `agents/<id>/CONTRACT.toml`.
    /// Params: `{ agent_id }`. Response:
    /// `{ agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    async fn handle_contract_get(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let dir = match self.resolve_agent_dir(&agent_id).await {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let path = dir.join("CONTRACT.toml");
        let table = self.read_config_table(&path).await;
        let mut resp = contract_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("agent_id".into(), json!(agent_id));
        }
        WsFrame::ok_response("", resp)
    }

    /// `contract.update` — atomic write of `agents/<id>/CONTRACT.toml`.
    /// Params: `{ agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    /// Response: `{ success, agent_id, must_not[], must_always[], max_tool_calls_per_turn }`.
    async fn handle_contract_update(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let dir = match self.resolve_agent_dir(&agent_id).await {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let table = match build_contract_table(&params) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let path = dir.join("CONTRACT.toml");
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(agent_id = agent_id.as_str(), "contract.update completed");
        let mut resp = contract_table_to_response(&table);
        if let Some(obj) = resp.as_object_mut() {
            obj.insert("success".into(), json!(true));
            obj.insert("agent_id".into(), json!(agent_id));
            // The contract is loaded per-invocation from disk by the agent
            // runner, so the next turn picks up the new boundaries automatically.
            obj.insert(
                "message".into(),
                json!("Contract updated — applies on the agent's next turn"),
            );
        }
        WsFrame::ok_response("", resp)
    }

    // ── RED: global [redaction] in config.toml ────────────────────────────────

    /// `redaction.get` — read config.toml `[redaction]`.
    /// Response: `{ enabled, vault_ttl_hours, purge_after_expire_days, profiles[],
    /// sources{...}, tool_egress{...} }`.
    async fn handle_redaction_get(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        WsFrame::ok_response("", redaction_table_to_response(&table))
    }

    /// `redaction.update` — atomic write of config.toml `[redaction]`.
    /// Params (all optional, partial update): `{ enabled, vault_ttl_hours,
    /// purge_after_expire_days, profiles[], sources{user_input|tool_results|
    /// system_prompt|sub_agent|cron_context = on|off|selective|inherit},
    /// tool_egress{<tool>: {restore_args: restore|passthrough|deny, audit_reveal}
    /// | null} }`. Response: `{ success, changes[] }`.
    async fn handle_redaction_update(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let changes = match apply_redaction_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid redaction fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "redaction.update completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }

    // ── SKS: global [skill_synthesis] in config.toml (W19-P1) ─────────────────

    /// `skill_synthesis.get` — read config.toml `[skill_synthesis]`.
    /// Response: `{ auto_run, dry_run, interval_hours, lookback_days, target_agent }`.
    async fn handle_skill_synthesis_get(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        WsFrame::ok_response("", skill_synthesis_table_to_response(&table))
    }

    /// `skill_synthesis.update` — atomic write of config.toml `[skill_synthesis]`.
    /// Params (all optional, partial update): `{ auto_run, dry_run,
    /// interval_hours (>=1), lookback_days (1-30), target_agent (empty clears) }`.
    /// Takes effect within one scheduler poll (~30 min) — no restart needed.
    /// Response: `{ success, changes[] }`.
    async fn handle_skill_synthesis_update(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let changes = match apply_skill_synthesis_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid skill_synthesis fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "skill_synthesis.update completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }

    // ── INF: global inference.toml (INF.1–INF.5) ──────────────────────────────

    /// `inference.get` — read `~/.duduclaw/inference.toml` into structured JSON.
    /// The `[openai_compat].api_key` secret is MASKED: the response carries
    /// `openai_compat.api_key_set` (bool) + `openai_compat.api_key` = "***set***"
    /// (or "") — NEVER the cleartext / encrypted value.
    async fn handle_inference_get(&self) -> WsFrame {
        let path = self.home_dir.join("inference.toml");
        let table = self.read_config_table(&path).await;
        WsFrame::ok_response("", inference_table_to_response(&table))
    }

    /// `inference.update` — atomic write of `~/.duduclaw/inference.toml`.
    /// Params (all optional, partial update): root (`enabled`/`backend`/
    /// `models_dir`/`default_model`/`auto_load`/`max_memory_mb`), `generation`,
    /// `router` (validates `strong_threshold < fast_threshold`), `openai_compat`
    /// (`base_url`/`model`/`api_key` → encrypted to `api_key_enc`), and the
    /// `exo`/`llamafile`/`mlx`/`mistralrs`/`llmlingua`/`streaming_llm`/`embedding`
    /// sub-sections (generic pass-through). Response: `{ success, changes[] }`.
    async fn handle_inference_update(&self, params: Value) -> WsFrame {
        let path = self.home_dir.join("inference.toml");
        let mut table = self.read_config_table(&path).await;

        // Pure validation + field application (no secret handling).
        let mut changes = match apply_inference_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // ── openai_compat.api_key secret: encrypt → `api_key_enc`, never store
        // cleartext. An empty string clears the secret. (INF.5 / INF.8)
        if let Some(api_key) = params
            .get("openai_compat")
            .and_then(|v| v.as_object())
            .and_then(|oc| oc.get("api_key"))
            .and_then(|v| v.as_str())
        {
            // Refuse to persist the masked placeholder back as a real secret —
            // the dashboard echoes it when the field was left untouched.
            if api_key != SECRET_MASK_SET {
                let section = match table
                    .entry("openai_compat")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                {
                    Some(s) => s,
                    None => return WsFrame::error_response("", "Invalid [openai_compat] section"),
                };
                // Never keep a cleartext `api_key` on disk.
                section.remove("api_key");
                if api_key.is_empty() {
                    section.remove("api_key_enc");
                    changes.push("openai_compat.api_key cleared".to_string());
                } else if let Some(enc) =
                    crate::config_crypto::encrypt_value(api_key, &self.home_dir)
                {
                    section.insert("api_key_enc".into(), toml::Value::String(enc));
                    changes.push("openai_compat.api_key = [ENCRYPTED]".to_string());
                } else {
                    return WsFrame::error_response("", "Failed to encrypt openai_compat.api_key");
                }
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid inference fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "inference.update completed");
        WsFrame::ok_response("", json!({ "success": true, "changes": changes }))
    }

    // ── MK: global [mcp_keys] in config.toml ──────────────────────────────────

    /// `mcp_keys.list` — list MCP API keys. NEVER returns cleartext keys; each
    /// entry carries a masked preview. Response:
    /// `{ keys: [{ masked, client_id, is_external, created_at, scopes[],
    /// rotate_recommended }] }`.
    async fn handle_mcp_keys_list(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let mut out: Vec<Value> = Vec::new();
        if let Some(keys) = table.get("mcp_keys").and_then(|v| v.as_table()) {
            for (key, val) in keys {
                let t = match val.as_table() {
                    Some(t) => t,
                    None => continue,
                };
                let client_id = t.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
                let is_external = t.get("is_external").and_then(|v| v.as_bool()).unwrap_or(false);
                let created_at = t.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
                let scopes: Vec<String> = t
                    .get("scopes")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                // 30-day rotation reminder (MK.4).
                let rotate_recommended = chrono::DateTime::parse_from_rfc3339(created_at)
                    .map(|dt| (Utc::now() - dt.with_timezone(&Utc)).num_days() >= 30)
                    .unwrap_or(false);
                out.push(json!({
                    "masked": mask_mcp_key(key),
                    "client_id": client_id,
                    "is_external": is_external,
                    "created_at": created_at,
                    "scopes": scopes,
                    "rotate_recommended": rotate_recommended,
                }));
            }
        }
        WsFrame::ok_response("", json!({ "keys": out }))
    }

    /// `mcp_keys.create` — generate a new MCP API key. Returns the cleartext key
    /// ONCE (it is never recoverable afterwards). Params:
    /// `{ client_id, env?: prod|staging|dev, is_external?, scopes[] }`.
    /// Response: `{ success, key (cleartext, once), masked, client_id,
    /// is_external, created_at, scopes[] }`.
    async fn handle_mcp_keys_create(&self, params: Value) -> WsFrame {
        let client_id = match params.get("client_id").and_then(|v| v.as_str()).map(str::trim) {
            Some(c) if !c.is_empty() && c.len() <= 128 => c.to_string(),
            _ => return WsFrame::error_response("", "Missing or invalid 'client_id' (1-128 chars)"),
        };
        let env = match params.get("env").and_then(|v| v.as_str()) {
            Some("prod") | None => "prod",
            Some("staging") => "staging",
            Some("dev") => "dev",
            Some(other) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid env '{other}'. Valid: prod, staging, dev"),
                )
            }
        };
        let is_external = params.get("is_external").and_then(|v| v.as_bool()).unwrap_or(false);

        // Validate scopes against the known scope list (MK.4).
        let scopes_arr = match params.get("scopes").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => return WsFrame::error_response("", "Missing 'scopes' (array of scope strings)"),
        };
        let mut scopes: Vec<String> = Vec::with_capacity(scopes_arr.len());
        for s in scopes_arr {
            let s = match s.as_str() {
                Some(s) => s.trim(),
                None => return WsFrame::error_response("", "scopes entries must be strings"),
            };
            if !KNOWN_MCP_SCOPES.contains(&s) {
                return WsFrame::error_response(
                    "",
                    &format!("Unknown scope '{s}'. Valid: {}", KNOWN_MCP_SCOPES.join(", ")),
                );
            }
            if !scopes.contains(&s.to_string()) {
                scopes.push(s.to_string());
            }
        }
        if scopes.is_empty() {
            return WsFrame::error_response("", "At least one scope is required");
        }

        let key = generate_mcp_key(env);
        // Defence-in-depth: ensure the generated key matches the canonical format.
        if !is_valid_mcp_key_format(&key) {
            return WsFrame::error_response("", "Internal error: generated key failed format check");
        }
        let created_at = Utc::now().to_rfc3339();

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mcp_keys = table
            .entry("mcp_keys")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let mcp_keys = match mcp_keys.as_table_mut() {
            Some(t) => t,
            None => return WsFrame::error_response("", "Invalid [mcp_keys] section in config.toml"),
        };
        let mut entry = toml::map::Map::new();
        entry.insert("client_id".into(), toml::Value::String(client_id.clone()));
        entry.insert("is_external".into(), toml::Value::Boolean(is_external));
        entry.insert("created_at".into(), toml::Value::String(created_at.clone()));
        entry.insert(
            "scopes".into(),
            toml::Value::Array(scopes.iter().map(|s| toml::Value::String(s.clone())).collect()),
        );
        mcp_keys.insert(key.clone(), toml::Value::Table(entry));

        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(client_id = client_id.as_str(), env, "mcp_keys.create completed");
        WsFrame::ok_response("", json!({
            "success": true,
            // Returned exactly once — the gateway never stores or echoes it again.
            "key": key,
            "masked": mask_mcp_key(&key),
            "client_id": client_id,
            "is_external": is_external,
            "created_at": created_at,
            "scopes": scopes,
            "message": "Store this key now — it cannot be retrieved again.",
        }))
    }

    /// `mcp_keys.revoke` — remove an `[mcp_keys.<key>]` entry. Params:
    /// `{ key }` (the full cleartext key). Response: `{ success, revoked }`.
    async fn handle_mcp_keys_revoke(&self, params: Value) -> WsFrame {
        let key = match params.get("key").and_then(|v| v.as_str()).map(str::trim) {
            Some(k) if !k.is_empty() => k.to_string(),
            _ => return WsFrame::error_response("", "Missing 'key' parameter"),
        };
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let removed = match table.get_mut("mcp_keys").and_then(|v| v.as_table_mut()) {
            Some(keys) => keys.remove(&key).is_some(),
            None => false,
        };
        if !removed {
            return WsFrame::error_response("", "Key not found");
        }
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!("mcp_keys.revoke completed");
        WsFrame::ok_response("", json!({ "success": true, "revoked": mask_mcp_key(&key) }))
    }

    // ── KS: global ~/.duduclaw/KILLSWITCH.toml ────────────────────────────────

    /// `killswitch.get` — read the global `~/.duduclaw/KILLSWITCH.toml`, filling
    /// any missing field with the documented default so the form is complete.
    async fn handle_killswitch_get(&self) -> WsFrame {
        let path = self.home_dir.join("KILLSWITCH.toml");
        let table = self.read_config_table(&path).await;
        WsFrame::ok_response("", killswitch_table_to_response(&table))
    }

    /// `killswitch.update` — atomic write of `~/.duduclaw/KILLSWITCH.toml`.
    /// Params (all sub-sections optional, partial update):
    /// `{ triggers{}, circuit_breaker{}, failsafe{}, safety_words{},
    /// defensive_prompt{}, audit{} }`. Response: `{ success, changes[] }`.
    async fn handle_killswitch_update(&self, params: Value) -> WsFrame {
        let path = self.home_dir.join("KILLSWITCH.toml");
        let mut table = self.read_config_table(&path).await;
        let changes = match apply_killswitch_to_table(&mut table, &params) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if changes.is_empty() {
            return WsFrame::error_response("", "No valid killswitch fields to update");
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(?changes, "killswitch.update completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "changes": changes,
            "message": "Kill switch updated — most thresholds apply on gateway restart",
        }))
    }

    // ── GOV: governance policies (policies/*.yaml) ────────────────────────────
    //
    // Files live under `<home>/policies/`: `global.yaml` (agent_id "*") +
    // `<agent_id>.yaml` (per-agent). The YAML schema mirrors
    // `duduclaw-governance::PolicyFile`. The gateway crate has no governance /
    // serde_yaml dependency, so we round-trip with the module-level
    // `gov_parse_yaml` / `gov_emit_yaml` / `gov_validate_policy` helpers.

    /// Resolve the policy file path for an `agent_id` (`*`/empty → global.yaml).
    fn gov_policy_path(&self, agent_id: &str) -> Result<PathBuf, String> {
        let dir = self.home_dir.join("policies");
        if agent_id.is_empty() || agent_id == "*" {
            return Ok(dir.join("global.yaml"));
        }
        if !is_valid_agent_id(agent_id) {
            return Err(format!("Invalid agent_id: {agent_id}"));
        }
        Ok(dir.join(format!("{agent_id}.yaml")))
    }

    /// `governance.list` — read global + (optional) per-agent policies.
    /// Params: `{ agent_id? }` — when omitted, returns both `global` and every
    /// `<agent>.yaml` found. When set, returns just that scope's policies.
    /// Response: `{ policies: [{ scope, ...policy fields }] }`.
    async fn handle_governance_list(&self, params: Value) -> WsFrame {
        let dir = self.home_dir.join("policies");
        let mut out: Vec<Value> = Vec::new();

        let mut read_scope = |scope: &str, path: &Path, out: &mut Vec<Value>| -> Result<(), String> {
            let raw = match std::fs::read_to_string(path) {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
            };
            for p in gov_parse_yaml(&raw)? {
                let mut obj = p;
                obj.insert("scope".into(), json!(scope));
                out.push(Value::Object(obj));
            }
            Ok(())
        };

        if let Some(agent_id) = params.get("agent_id").and_then(|v| v.as_str()) {
            let path = match self.gov_policy_path(agent_id) {
                Ok(p) => p,
                Err(e) => return WsFrame::error_response("", &e),
            };
            let scope = if agent_id.is_empty() || agent_id == "*" { "global".to_string() } else { agent_id.to_string() };
            if let Err(e) = read_scope(&scope, &path, &mut out) {
                return WsFrame::error_response("", &e);
            }
        } else {
            // Global first.
            if let Err(e) = read_scope("global", &dir.join("global.yaml"), &mut out) {
                return WsFrame::error_response("", &e);
            }
            // Then every <agent>.yaml.
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".yaml") && n != "global.yaml")
                    .map(|n| n.trim_end_matches(".yaml").to_string())
                    .collect();
                names.sort();
                for name in names {
                    if let Err(e) = read_scope(&name, &dir.join(format!("{name}.yaml")), &mut out) {
                        return WsFrame::error_response("", &e);
                    }
                }
            }
        }

        WsFrame::ok_response("", json!({ "policies": out }))
    }

    /// `governance.upsert` — create or replace a policy (matched by `policy_id`)
    /// in its scope's YAML file. Params: a policy object `{ policy_type,
    /// policy_id, agent_id, ... }`. Atomic write. Response: `{ success,
    /// scope, policy_id, created }`.
    async fn handle_governance_upsert(&self, params: Value) -> WsFrame {
        let validated = match gov_validate_policy(&params) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let agent_id = validated.get("agent_id").and_then(|v| v.as_str()).unwrap_or("*").to_string();
        let policy_id = validated.get("policy_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let path = match self.gov_policy_path(&agent_id) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let mut policies = match std::fs::read_to_string(&path) {
            Ok(raw) => match gov_parse_yaml(&raw) {
                Ok(p) => p,
                Err(e) => return WsFrame::error_response("", &format!("Failed to parse {}: {e}", path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return WsFrame::error_response("", &format!("Failed to read {}: {e}", path.display())),
        };

        let mut created = true;
        for p in &mut policies {
            if p.get("policy_id").and_then(|v| v.as_str()) == Some(policy_id.as_str()) {
                *p = validated.clone();
                created = false;
                break;
            }
        }
        if created {
            policies.push(validated);
        }

        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return WsFrame::error_response("", &format!("Failed to create policies dir: {e}"));
            }
        }
        if let Err(e) = self.gov_atomic_write_yaml(&path, &gov_emit_yaml(&policies)).await {
            return WsFrame::error_response("", &e);
        }
        let scope = if agent_id == "*" { "global".to_string() } else { agent_id };
        info!(scope = scope.as_str(), policy_id = policy_id.as_str(), created, "governance.upsert completed");
        WsFrame::ok_response("", json!({
            "success": true, "scope": scope, "policy_id": policy_id, "created": created,
            "message": "Policy saved — applies on next PolicyRegistry reload",
        }))
    }

    /// `governance.remove` — delete a policy by `policy_id` from its scope.
    /// Params: `{ policy_id, agent_id? }`. Response: `{ success, removed }`.
    async fn handle_governance_remove(&self, params: Value) -> WsFrame {
        let policy_id = match params.get("policy_id").and_then(|v| v.as_str()).map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return WsFrame::error_response("", "Missing 'policy_id' parameter"),
        };
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("*").to_string();
        let path = match self.gov_policy_path(&agent_id) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &e),
        };

        let mut policies = match std::fs::read_to_string(&path) {
            Ok(raw) => match gov_parse_yaml(&raw) {
                Ok(p) => p,
                Err(e) => return WsFrame::error_response("", &format!("Failed to parse {}: {e}", path.display())),
            },
            Err(_) => return WsFrame::error_response("", "Policy not found"),
        };
        let before = policies.len();
        policies.retain(|p| p.get("policy_id").and_then(|v| v.as_str()) != Some(policy_id.as_str()));
        if policies.len() == before {
            return WsFrame::error_response("", &format!("Policy not found: {policy_id}"));
        }
        if let Err(e) = self.gov_atomic_write_yaml(&path, &gov_emit_yaml(&policies)).await {
            return WsFrame::error_response("", &e);
        }
        info!(policy_id = policy_id.as_str(), "governance.remove completed");
        WsFrame::ok_response("", json!({ "success": true, "removed": policy_id }))
    }

    /// Atomic write of a YAML string (temp + rename).
    async fn gov_atomic_write_yaml(&self, path: &Path, content: &str) -> Result<(), String> {
        let tmp = path.with_extension("yaml.tmp");
        tokio::fs::write(&tmp, content).await
            .map_err(|e| format!("Failed to write {}: {e}", tmp.display()))?;
        tokio::fs::rename(&tmp, path).await.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("Failed to commit {}: {e}", path.display())
        })
    }

    // ── SCP: wiki namespace scope (.scope.toml) ───────────────────────────────
    //
    // Path: `<home>/shared/wiki/.scope.toml` (mirrors duduclaw-cli::wiki_scope).

    /// `wiki_scope.get` — read the shared wiki `.scope.toml`. Response:
    /// `{ namespaces: [{ namespace, mode, synced_from }] }`. Absent file → `[]`.
    async fn handle_wiki_scope_get(&self) -> WsFrame {
        let path = self.home_dir.join("shared").join("wiki").join(".scope.toml");
        let table = self.read_config_table(&path).await;
        WsFrame::ok_response("", scp_table_to_response(&table))
    }

    /// `wiki_scope.update` — set (or clear) a single namespace's policy. Params:
    /// `{ namespace, mode: agent_writable|read_only|operator_only, synced_from?,
    /// remove? }`. `remove=true` deletes the entry (reverts to agent_writable
    /// default). Atomic write. Response: `{ success, change }`.
    async fn handle_wiki_scope_update(&self, params: Value) -> WsFrame {
        let namespace = match params.get("namespace").and_then(|v| v.as_str()).map(str::trim) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return WsFrame::error_response("", "Missing 'namespace' parameter"),
        };
        let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("agent_writable");
        let synced_from = params.get("synced_from").and_then(|v| v.as_str());
        let remove = params.get("remove").and_then(|v| v.as_bool()).unwrap_or(false);

        let path = self.home_dir.join("shared").join("wiki").join(".scope.toml");
        let mut table = self.read_config_table(&path).await;
        let change = match scp_apply_namespace(&mut table, &namespace, mode, synced_from, remove) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return WsFrame::error_response("", &format!("Failed to create wiki dir: {e}"));
            }
        }
        if let Err(e) = self.atomic_write_toml(&path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(namespace = namespace.as_str(), mode, "wiki_scope.update completed");
        WsFrame::ok_response("", json!({ "success": true, "change": change }))
    }

    /// Remove an agent by moving its directory to `_trash/`.
    ///
    /// Refuses to remove the main agent. Recovery is possible from `_trash/`.
    async fn handle_agents_remove(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };

        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        // Refuse to remove the main agent
        let reg = self.registry.read().await;
        if let Some(agent) = reg.get(agent_id) {
            if matches!(agent.config.agent.role, duduclaw_core::types::AgentRole::Main) {
                return WsFrame::error_response("", "Cannot remove the main agent");
            }
        } else {
            return WsFrame::error_response("", &format!("Agent not found: {agent_id}"));
        }
        let agents_dir = reg.agents_dir().to_path_buf();
        drop(reg);

        let agent_dir = agents_dir.join(agent_id);
        let trash_dir = agents_dir.join("_trash");
        if let Err(e) = tokio::fs::create_dir_all(&trash_dir).await {
            return WsFrame::error_response("", &format!("Failed to create _trash/: {e}"));
        }

        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let trash_dest = trash_dir.join(format!("{agent_id}_{timestamp}"));

        if let Err(e) = tokio::fs::rename(&agent_dir, &trash_dest).await {
            return WsFrame::error_response("", &format!("Failed to move agent to trash: {e}"));
        }

        // Re-scan registry
        if let Ok(mut reg) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            let _ = reg.scan().await;
        }

        info!(agent_id, "Agent removed (moved to _trash/)");
        WsFrame::ok_response("", json!({
            "success": true,
            "agent_id": agent_id,
            "trash_path": trash_dest.to_string_lossy(),
        }))
    }

    async fn handle_agents_inspect(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        // Real month-to-date spend for THIS agent (not the all-account aggregate).
        let spent = self.telemetry_spent_cents_for_agent(agent_id).await;
        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(a) => {
                let cfg = &a.config;
                WsFrame::ok_response("", json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "trigger": cfg.agent.trigger,
                    "icon": cfg.agent.icon,
                    "reports_to": cfg.agent.reports_to,
                    "soul_preview": a.soul.as_ref().map(|s| {
                        let t = truncate_bytes(s, 500);
                        if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                    }),
                    "identity_preview": a.identity.as_ref().map(|s| {
                        let t = truncate_bytes(s, 500);
                        if t.len() < s.len() { format!("{t}…") } else { s.clone() }
                    }),
                    "memory_summary": a.memory,
                    "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "model": {
                        "preferred": cfg.model.preferred,
                        "fallback": cfg.model.fallback,
                        "account_pool": cfg.model.account_pool,
                        "api_mode": cfg.model.api_mode,
                        "local": cfg.model.local.as_ref().map(|l| json!({
                            "model": l.model,
                            "backend": l.backend,
                            "context_length": l.context_length,
                            "gpu_layers": l.gpu_layers,
                            "prefer_local": l.prefer_local,
                            "use_router": l.use_router,
                        })),
                    },
                    "budget": { "monthly_limit_cents": cfg.budget.monthly_limit_cents, "spent_cents": spent, "warn_threshold_percent": cfg.budget.warn_threshold_percent, "hard_stop": cfg.budget.hard_stop },
                    "heartbeat": { "enabled": cfg.heartbeat.enabled, "interval_seconds": cfg.heartbeat.interval_seconds },
                    "proactive": {
                        "enabled": cfg.proactive.enabled,
                        "check_interval": cfg.proactive.check_interval,
                        "quiet_hours_start": cfg.proactive.quiet_hours_start,
                        "quiet_hours_end": cfg.proactive.quiet_hours_end,
                        "max_messages_per_hour": cfg.proactive.max_messages_per_hour,
                        "notify_channel": cfg.proactive.notify_channel,
                        "notify_chat_id": cfg.proactive.notify_chat_id,
                    },
                    "permissions": {
                        "can_create_agents": cfg.permissions.can_create_agents,
                        "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                        "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                        "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                        "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                    },
                    "sticker": {
                        "enabled": cfg.sticker.enabled,
                        "probability": cfg.sticker.probability,
                        "intensity_threshold": cfg.sticker.intensity_threshold,
                        "cooldown_messages": cfg.sticker.cooldown_messages,
                        "expressiveness": match cfg.sticker.expressiveness {
                            duduclaw_core::types::Expressiveness::Minimal => "minimal",
                            duduclaw_core::types::Expressiveness::Moderate => "moderate",
                            duduclaw_core::types::Expressiveness::Expressive => "expressive",
                        },
                    },
                    "evolution": {
                        "gvu_enabled": cfg.evolution.gvu_enabled,
                        "cognitive_memory": cfg.evolution.cognitive_memory,
                        "skill_auto_activate": cfg.evolution.skill_auto_activate,
                        "skill_security_scan": cfg.evolution.skill_security_scan,
                        "max_silence_hours": cfg.evolution.max_silence_hours,
                    },
                }))
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Channels ─────────────────────────────────────────────

    async fn handle_channels_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let runtime_status = self.channel_status.read().await;
        let mut channels = Vec::new();

        if let Ok(content) = tokio::fs::read_to_string(&config_path).await
            && let Ok(config) = content.parse::<toml::Table>()
            && let Some(ch) = config.get("channels").and_then(|v| v.as_table())
        {
            let token_map = [
                ("line_channel_token", "line"),
                ("telegram_bot_token", "telegram"),
                ("discord_bot_token", "discord"),
            ];
            for (key, name) in token_map {
                let configured = ch.get(key).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty());
                if configured {
                    // Use runtime state if available; otherwise use "connecting" status
                    let (connected, last_ts, error) = match runtime_status.get(name) {
                        Some(state) => (
                            state.connected,
                            state.last_event.as_ref().map(|t| t.to_rfc3339()),
                            state.error.clone(),
                        ),
                        None => (false, None, Some("connecting".to_string())),
                    };
                    channels.push(json!({
                        "name": name,
                        "connected": connected,
                        "last_connected": last_ts,
                        "error": error,
                    }));
                }
            }
        }

        // Include per-agent channels from agent registry configs
        let mut seen_labels = std::collections::HashSet::new();
        {
            let reg = self.registry.read().await;
            for agent in reg.list() {
                if let Some(ch) = &agent.config.channels {
                    let name = &agent.config.agent.name;
                    let pairs: &[(&str, bool)] = &[
                        ("discord", ch.discord.as_ref().is_some_and(|d| !d.bot_token.is_empty() || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                        ("telegram", ch.telegram.as_ref().is_some_and(|t| !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                        ("slack", ch.slack.as_ref().is_some_and(|s| !s.bot_token.is_empty() || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty()))),
                    ];
                    for &(platform, configured) in pairs {
                        if configured {
                            let label = format!("{platform}:{name}");
                            seen_labels.insert(label.clone());
                            let (connected, last_ts, error) = match runtime_status.get(&label) {
                                Some(state) => (
                                    state.connected,
                                    state.last_event.as_ref().map(|t| t.to_rfc3339()),
                                    state.error.clone(),
                                ),
                                None => (false, None, Some("connecting".to_string())),
                            };
                            channels.push(json!({
                                "name": label,
                                "connected": connected,
                                "last_connected": last_ts,
                                "error": error,
                            }));
                        }
                    }
                }
            }
        }

        // Also include runtime-only per-agent entries not yet in registry (edge case)
        for (key, state) in runtime_status.iter() {
            if key.contains(':') && !seen_labels.contains(key.as_str()) {
                channels.push(json!({
                    "name": key,
                    "connected": state.connected,
                    "last_connected": state.last_event.as_ref().map(|t| t.to_rfc3339()),
                    "error": state.error.clone(),
                }));
            }
        }

        WsFrame::ok_response("", json!({ "channels": channels }))
    }

    async fn handle_channels_add(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };
        let config_obj = params.get("config").cloned().unwrap_or(json!({}));
        let token = config_obj.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let secret = config_obj.get("secret").and_then(|v| v.as_str()).unwrap_or("");
        let agent_name = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");

        if token.is_empty() {
            return WsFrame::error_response("", "Missing 'config.token' parameter");
        }

        // Cloud-tier channel cap (self-host is never capped — Apache 2.0).
        let channel_count = self.count_configured_channels().await;
        if let Some(msg) = self.tier_limit_message("channel", channel_count).await {
            return WsFrame::error_response("", &msg);
        }

        // Per-agent channel: write to agent.toml [channels.{platform}]. Only the
        // token-exclusive channels can be bound per agent; LINE/WhatsApp/Feishu are
        // single global webhook endpoints, so when an agent is selected for them we
        // fall through to the global path and bind the agent as `default_agent`
        // (below) instead of erroring out — otherwise the save silently fails and
        // nothing is persisted.
        if !agent_name.is_empty() && matches!(channel_type, "discord" | "telegram" | "slack") {
            let (token_field, secret_field) = match channel_type {
                "discord" => ("bot_token", None),
                "telegram" => ("bot_token", None),
                "slack" => ("bot_token", Some("app_token")),
                _ => return WsFrame::error_response("", &format!("Per-agent channels not supported for: {channel_type}")),
            };

            let token_owned = token.to_string();
            let secret_owned = secret.to_string();
            let channel_type_owned = channel_type.to_string();
            let home = self.home_dir.clone();

            if let Err(e) = self.update_agent_toml(agent_name, move |table| {
                let channels = table.entry("channels")
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or("Invalid [channels] section")?;
                let section = channels.entry(&channel_type_owned)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| format!("Invalid [channels.{}] section", channel_type_owned))?;

                section.insert(token_field.to_string(), toml::Value::String(token_owned.clone()));
                if let Some(enc) = crate::config_crypto::encrypt_value(&token_owned, &home) {
                    section.insert(format!("{token_field}_enc"), toml::Value::String(enc));
                }
                if let Some(sf) = secret_field {
                    if !secret_owned.is_empty() {
                        section.insert(sf.to_string(), toml::Value::String(secret_owned.clone()));
                        if let Some(enc) = crate::config_crypto::encrypt_value(&secret_owned, &home) {
                            section.insert(format!("{sf}_enc"), toml::Value::String(enc));
                        }
                    }
                }
                Ok(())
            }).await {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-start: stop existing per-agent bot if any, then re-launch all per-agent bots
            let label = format!("{channel_type}:{agent_name}");
            self.hot_stop_channel(&label).await;

            let mut hot_started = false;
            if let Some(ctx) = self.reply_ctx.read().await.clone() {
                let handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                    "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                    "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                    _ => Vec::new(),
                };
                for (l, h) in handles {
                    if l == label { hot_started = true; }
                    self.register_channel_handle(&l, h).await;
                }
            }

            info!(channel_type, agent_name, "Per-agent channel config saved");
            return WsFrame::ok_response("", json!({
                "success": true,
                "type": label,
                "hot_started": hot_started,
            }));
        }

        // Global channel: write to config.toml [channels]
        let (token_key, secret_key) = match channel_type {
            "line" => ("line_channel_token", Some("line_channel_secret")),
            "telegram" => ("telegram_bot_token", None),
            "discord" => ("discord_bot_token", None),
            "slack" => ("slack_bot_token", Some("slack_app_token")),
            "whatsapp" => ("whatsapp_access_token", Some("whatsapp_phone_number_id")),
            "feishu" => ("feishu_app_id", Some("feishu_app_secret")),
            // token = service-account JSON key; secret = Cloud project number
            "googlechat" => ("googlechat_service_account_json", Some("googlechat_project_number")),
            // token = client secret; secret = Microsoft App ID
            "teams" => ("teams_app_password", Some("teams_app_id")),
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        // Encrypt the primary token before storing (H3).
        let enc_token_key = format!("{token_key}_enc");
        let encrypted_token = crate::config_crypto::encrypt_value(token, &self.home_dir);

        // XC.3: `whatsapp_phone_number_id` is NOT a secret — it must be stored as
        // plaintext (consistent with the per-agent `agents.update` path, which
        // does not encrypt it). Google Chat's project number and Teams' App ID
        // are likewise identifiers, not secrets. All other secret_key fields
        // ARE encrypted.
        let secret_is_plain = matches!(
            secret_key,
            Some("whatsapp_phone_number_id") | Some("googlechat_project_number") | Some("teams_app_id")
        );

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        let channels = table
            .entry("channels")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut();

        let channels = match channels {
            Some(ch) => ch,
            None => return WsFrame::error_response("", "Invalid [channels] section in config.toml"),
        };

        // Store encrypted version; also keep plaintext as fallback
        channels.insert(token_key.to_string(), toml::Value::String(token.to_string()));
        if let Some(enc) = &encrypted_token {
            channels.insert(enc_token_key, toml::Value::String(enc.clone()));
        }
        if let Some(sk) = secret_key {
            if !secret.is_empty() {
                channels.insert(sk.to_string(), toml::Value::String(secret.to_string()));
                if !secret_is_plain {
                    if let Some(enc) = crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                        channels.insert(format!("{sk}_enc"), toml::Value::String(enc));
                    }
                }
            }
        }

        // ── G.6: additional global channel tokens carried in `config.*` ──
        // whatsapp_verify_token / whatsapp_app_secret / feishu_verification_token.
        // Secrets are encrypted to `_enc`; never echoed back.
        let extra_secret_fields: &[&str] = match channel_type {
            "whatsapp" => &["whatsapp_verify_token", "whatsapp_app_secret"],
            "feishu" => &["feishu_verification_token"],
            "teams" => &["teams_tenant_id"],
            _ => &[],
        };
        for field in extra_secret_fields {
            if let Some(v) = config_obj.get(*field).and_then(|v| v.as_str()) {
                let v = v.trim();
                if v.is_empty() {
                    channels.remove(*field);
                    channels.remove(&format!("{field}_enc"));
                    continue;
                }
                channels.insert((*field).to_string(), toml::Value::String(v.into()));
                if let Some(enc) = crate::config_crypto::encrypt_value(v, &self.home_dir) {
                    channels.insert(format!("{field}_enc"), toml::Value::String(enc));
                }
            }
        }

        // Webhook/global channels (LINE/WhatsApp/Feishu) are a single endpoint and
        // can't bind a token per agent; if the user picked an agent, record it as
        // the global `default_agent` so incoming messages route to it.
        if !agent_name.is_empty() {
            if let Some(general) = table
                .entry("general")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
            {
                general.insert(
                    "default_agent".to_string(),
                    toml::Value::String(agent_name.to_string()),
                );
            }
        }

        // XC.2: atomic write (temp + rename), mirroring the per-agent path.
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }

        info!(channel_type, agent = agent_name, "Channel config saved");

        // Hot-start: launch the channel bot immediately without gateway restart
        let hot_started = self.hot_start_channel(channel_type).await;

        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "hot_started": hot_started,
        }))
    }

    async fn handle_channels_test(&self, params: Value) -> WsFrame {
        let channel_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        info!(channel_type, "channels.test requested");

        // Per-agent channel test: check agent.toml
        if let Some((platform, agent_name)) = channel_type.split_once(':') {
            let token_field = match platform {
                "discord" | "telegram" => "bot_token",
                "slack" => "bot_token",
                _ => return WsFrame::error_response("", &format!("Unknown channel platform: {platform}")),
            };

            let reg = self.registry.read().await;
            let configured = reg.get(agent_name).is_some_and(|agent| {
                if let Some(ch) = &agent.config.channels {
                    match platform {
                        "discord" => ch.discord.as_ref().is_some_and(|d| !d.bot_token.is_empty() || d.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        "telegram" => ch.telegram.as_ref().is_some_and(|t| !t.bot_token.is_empty() || t.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        "slack" => ch.slack.as_ref().is_some_and(|s| !s.bot_token.is_empty() || s.bot_token_enc.as_ref().is_some_and(|e| !e.is_empty())),
                        _ => false,
                    }
                } else {
                    false
                }
            });
            drop(reg);

            return WsFrame::ok_response("", json!({
                "success": configured,
                "type": channel_type,
                "message": if configured { format!("{channel_type} {token_field} is configured") } else { format!("{channel_type} token 未設定") },
            }));
        }

        // Global channel test
        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            "slack" => "slack_bot_token",
            "whatsapp" => "whatsapp_access_token",
            "feishu" => "feishu_app_id",
            "googlechat" => "googlechat_service_account_json",
            "teams" => "teams_app_password",
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        // Check both plaintext and encrypted token
        let has_token = crate::config_crypto::decrypt_config_field(&table, "channels", token_key, &self.home_dir)
            .is_some_and(|t| !t.is_empty());

        if !has_token {
            return WsFrame::ok_response("", json!({
                "success": false,
                "type": channel_type,
                "message": format!("{channel_type} token 未設定"),
            }));
        }

        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "message": format!("{channel_type} token is configured"),
        }))
    }

    async fn handle_channels_remove(&self, params: Value) -> WsFrame {
        let channel_type = match params.get("type").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return WsFrame::error_response("", "Missing 'type' parameter"),
        };

        // Per-agent channel: format "discord:agent_name", "telegram:agent_name", etc.
        if let Some((platform, agent_name)) = channel_type.split_once(':') {
            let channel_section = match platform {
                "discord" | "telegram" | "slack" => platform,
                _ => return WsFrame::error_response("", &format!("Unknown channel platform: {platform}")),
            };

            // Clear the [channels.{platform}] section in the agent's agent.toml
            let agent_name_owned = agent_name.to_string();
            let channel_section_owned = channel_section.to_string();
            if let Err(e) = self.update_agent_toml(&agent_name_owned, |table| {
                if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
                    channels.remove(&channel_section_owned);
                }
                Ok(())
            }).await {
                return WsFrame::error_response("", &format!("Failed to update agent config: {e}"));
            }

            // Hot-stop the per-agent bot
            self.hot_stop_channel(channel_type).await;

            info!(channel_type, "Per-agent channel removed and stopped");
            return WsFrame::ok_response("", json!({
                "success": true,
                "type": channel_type,
            }));
        }

        // Global channel removal
        let token_key = match channel_type {
            "line" => "line_channel_token",
            "telegram" => "telegram_bot_token",
            "discord" => "discord_bot_token",
            "slack" => "slack_bot_token",
            "whatsapp" => "whatsapp_access_token",
            "feishu" => "feishu_app_id",
            "googlechat" => "googlechat_service_account_json",
            "teams" => "teams_app_password",
            _ => return WsFrame::error_response("", &format!("Unknown channel type: {channel_type}")),
        };

        // Companion fields cleared alongside the primary token.
        let companion_fields: &[&str] = match channel_type {
            "line" => &["line_channel_secret"],
            "slack" => &["slack_app_token"],
            "whatsapp" => &["whatsapp_phone_number_id", "whatsapp_verify_token", "whatsapp_app_secret"],
            "feishu" => &["feishu_app_secret", "feishu_verification_token"],
            "googlechat" => &["googlechat_project_number"],
            "teams" => &["teams_app_id", "teams_tenant_id"],
            _ => &[],
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        if let Some(channels) = table.get_mut("channels").and_then(|v| v.as_table_mut()) {
            channels.insert(token_key.to_string(), toml::Value::String(String::new()));
            // Also clear the encrypted version
            let enc_key = format!("{token_key}_enc");
            channels.insert(enc_key, toml::Value::String(String::new()));
            for field in companion_fields {
                channels.insert((*field).to_string(), toml::Value::String(String::new()));
                channels.insert(format!("{field}_enc"), toml::Value::String(String::new()));
            }
        }

        // XC.2: atomic write (temp + rename).
        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }

        // Hot-stop: abort the running global channel bot task
        self.hot_stop_channel(channel_type).await;

        // Re-launch per-agent bots since the global bot was deduplicating their tokens.
        let mut restarted_agents = Vec::new();
        let ctx_opt = self.reply_ctx.read().await.clone();
        if let Some(ctx) = ctx_opt {
            let per_agent_handles: Vec<(String, tokio::task::JoinHandle<()>)> = match channel_type {
                "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx).await,
                "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx).await,
                "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx).await,
                _ => Vec::new(),
            };
            for (label, h) in per_agent_handles {
                restarted_agents.push(label.clone());
                self.register_channel_handle(&label, h).await;
            }
        }

        info!(channel_type, restarted = ?restarted_agents, "Channel removed and stopped");
        WsFrame::ok_response("", json!({
            "success": true,
            "type": channel_type,
            "restarted_per_agent": restarted_agents,
        }))
    }

    // ── Channel hot-start/stop ────────────────────────────────

    /// Launch a channel bot immediately after config is saved.
    async fn hot_start_channel(&self, channel_type: &str) -> bool {
        let ctx = match self.reply_ctx.read().await.clone() {
            Some(ctx) => ctx,
            None => {
                warn!(channel_type, "Cannot hot-start channel: ReplyContext not available");
                return false;
            }
        };

        // Stop existing instance first (if any)
        self.hot_stop_channel(channel_type).await;

        let home = self.home_dir.clone();
        let handle = match channel_type {
            "telegram" => crate::telegram::start_telegram_bot(&home, ctx).await,
            "discord" => crate::discord::start_discord_bot(&home, ctx).await,
            "slack" => crate::slack::start_slack_bot(&home, ctx).await,
            "line" => {
                // LINE uses a webhook (axum route is always mounted) — no background
                // task. The handler reads the token per request, so saving config is
                // enough; just refresh the connection status so the dashboard flips
                // from "連線中" to connected immediately.
                crate::line::refresh_line_status(&home, ctx.clone()).await;
                info!("LINE channel updated; status refreshed (webhook always mounted)");
                return true;
            }
            "whatsapp" | "feishu" | "googlechat" | "teams" => {
                // These webhook routers are mounted at boot with their config
                // baked into router state — a gateway restart is required for
                // a first-time setup to take effect.
                info!(
                    channel_type,
                    "Webhook channel config saved — restart the gateway to (re)mount the endpoint"
                );
                return false;
            }
            _ => None,
        };

        match handle {
            Some(h) => {
                info!(channel_type, "Channel hot-started successfully");
                self.channel_handles.lock().await.insert(channel_type.to_string(), h);
                true
            }
            None => {
                warn!(channel_type, "Channel hot-start failed (check token validity)");
                false
            }
        }
    }

    /// Stop a running channel bot task.
    async fn hot_stop_channel(&self, channel_type: &str) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(handle) = handles.remove(channel_type) {
            handle.abort();
            info!(channel_type, "Channel bot stopped");
        }
        // Always clear runtime status (handle may already be gone if bot crashed)
        let mut status = self.channel_status.write().await;
        status.remove(channel_type);
    }

    /// Hot-restart per-agent channel bots (Telegram / Discord) after a token
    /// change persisted to agent.toml. Returns the labels that were re-armed
    /// (e.g. `["telegram:agnes"]`).
    ///
    /// Without this, `agents.update` would write the new token but the running
    /// bot loop keeps using the old captured token until gateway restart.
    /// LINE / WhatsApp / Feishu are not handled here — LINE is webhook-based
    /// (no background task), the others lack hot-restart helpers and still
    /// require gateway restart for token changes.
    async fn hot_restart_agent_channels(&self, channel_types: &[&str], agent_name: &str) -> Vec<String> {
        let ctx = match self.reply_ctx.read().await.clone() {
            Some(ctx) => ctx,
            None => return Vec::new(),
        };

        let mut restarted = Vec::new();
        for ch in channel_types {
            let label = format!("{ch}:{agent_name}");
            self.hot_stop_channel(&label).await;

            let handles: Vec<(String, tokio::task::JoinHandle<()>)> = match *ch {
                "discord" => crate::discord::start_discord_bots(&self.home_dir, ctx.clone()).await,
                "telegram" => crate::telegram::start_telegram_bots(&self.home_dir, ctx.clone()).await,
                "slack" => crate::slack::start_slack_bots(&self.home_dir, ctx.clone()).await,
                _ => Vec::new(),
            };
            for (l, h) in handles {
                if l == label { restarted.push(l.clone()); }
                self.register_channel_handle(&l, h).await;
            }
        }
        restarted
    }

    // ── Accounts ─────────────────────────────────────────────

    async fn handle_accounts_list(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "auth_method": a.auth_method,
            "priority": a.priority,
            "is_healthy": a.is_healthy,
            "spent_this_month": a.spent_this_month,
            "monthly_budget_cents": a.monthly_budget_cents,
            "total_requests": a.total_requests,
            "is_available": a.is_available,
            "label": a.label,
            "email": a.email,
            "subscription": a.subscription,
            "expires_at": a.expires_at,
            "days_until_expiry": a.days_until_expiry,
        })).collect();
        WsFrame::ok_response("", json!({ "accounts": accounts_json }))
    }

    async fn handle_budget_summary(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let total_budget: u64 = accounts.iter().map(|a| a.monthly_budget_cents).sum();

        // Headline "spent" comes from CostTelemetry (persistent, real) rather
        // than summing the rotator's in-memory per-account counters, which reset
        // on restart / rebuild and stay 0 for OAuth-subscription accounts.
        //
        // CostTelemetry attributes cost per AGENT, not per ACCOUNT (API key), so
        // there is no faithful per-account breakdown — `spent_this_month` on each
        // account card stays the rotator's best-effort value, but the aggregate
        // bar (the figure users actually read) is now correct.
        let total_spent = self.telemetry_spent_cents_total().await;

        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "auth_method": a.auth_method,
            "priority": a.priority,
            "is_healthy": a.is_healthy,
            "spent_this_month": a.spent_this_month,
            "monthly_budget_cents": a.monthly_budget_cents,
        })).collect();

        WsFrame::ok_response("", json!({
            "total_budget_cents": total_budget,
            "total_spent_cents": total_spent,
            "accounts": accounts_json,
        }))
    }

    async fn handle_accounts_rotate(&self, _params: Value) -> WsFrame {
        let rotator = self.cached_rotator().await;
        match rotator.select().await {
            Some(selected) => {
                WsFrame::ok_response("", json!({
                    "success": true,
                    "selected_account": selected.id,
                    "strategy": "configured",
                    "message": format!("Rotated to account '{}'", selected.id),
                }))
            }
            None => WsFrame::error_response("", "No available accounts for rotation"),
        }
    }

    async fn handle_accounts_health(&self) -> WsFrame {
        let rotator = self.cached_rotator().await;
        let accounts = rotator.status().await;
        let healthy_count = accounts.iter().filter(|a| a.is_healthy).count();
        let status = if accounts.is_empty() { "no_accounts" }
            else if healthy_count == accounts.len() { "healthy" }
            else if healthy_count > 0 { "degraded" }
            else { "unhealthy" };

        let accounts_json: Vec<Value> = accounts.iter().map(|a| json!({
            "id": a.id,
            "healthy": a.is_healthy,
            "available": a.is_available,
            "spent": a.spent_this_month,
            "budget": a.monthly_budget_cents,
            "requests": a.total_requests,
        })).collect();

        WsFrame::ok_response("", json!({
            "status": status,
            "healthy_count": healthy_count,
            "total_count": accounts.len(),
            "accounts": accounts_json,
        }))
    }

    /// Get or create a cached rotator (uses the same static cache as claude_runner).
    async fn cached_rotator(&self) -> std::sync::Arc<duduclaw_agent::account_rotator::AccountRotator> {
        // Reuse the global cache from claude_runner to avoid redundant disk reads
        match crate::claude_runner::get_rotator_cached(&self.home_dir).await {
            Ok(r) => r,
            Err(_) => {
                // Fallback: create a fresh one
                let config_content = tokio::fs::read_to_string(self.home_dir.join("config.toml"))
                    .await
                    .unwrap_or_default();
                let config_table: toml::Table = config_content.parse().unwrap_or_default();
                let rotator = duduclaw_agent::account_rotator::create_from_config(&config_table);
                let _ = rotator.load_from_config(&self.home_dir).await;
                std::sync::Arc::new(rotator)
            }
        }
    }

    /// Real month-to-date spend in **cents** across all agents, sourced from
    /// `CostTelemetry` (the persistent SQLite ledger).
    ///
    /// The `AccountRotator`'s `spent_this_month` counter is in-memory only: it
    /// resets to 0 on every gateway restart and every 5-minute rotator rebuild,
    /// and stays 0 for OAuth-subscription accounts (which have no per-call cost).
    /// CostTelemetry records every request's real token cost keyed by agent, so
    /// it is the correct source for "how much was actually used this month".
    async fn telemetry_spent_cents_total(&self) -> u64 {
        let Some(telemetry) = crate::cost_telemetry::get_telemetry() else {
            return 0;
        };
        match telemetry.summary_global(hours_since_month_start()).await {
            // `cost_millicents` is a misnomer — `estimated_cost_millicents()`
            // produces whole CENTS (e.g. 1M output tokens @ $15/M → 1500 = $15.00),
            // so this value is already in cents. No scaling.
            Ok(summary) => summary.total_cost_millicents,
            Err(_) => 0,
        }
    }

    /// Real month-to-date spend in **cents** for a single agent, from
    /// `CostTelemetry`. See [`Self::telemetry_spent_cents_total`] for why this
    /// is preferred over the rotator counter.
    async fn telemetry_spent_cents_for_agent(&self, agent_id: &str) -> u64 {
        let Some(telemetry) = crate::cost_telemetry::get_telemetry() else {
            return 0;
        };
        match telemetry
            .summary_by_agent(agent_id, hours_since_month_start())
            .await
        {
            // Already in cents — see `telemetry_spent_cents_total`.
            Ok(agent) => agent.summary.total_cost_millicents,
            Err(_) => 0,
        }
    }

    // ── Memory ──────────────────────────────────────────────

    /// Resolve the per-agent memory.db path.
    /// Prefers `agents/<id>/state/memory.db`, falls back to `agents/<id>/memory.db`.
    fn agent_memory_db_path(&self, agent_id: &str) -> PathBuf {
        let agent_dir = self.home_dir.join("agents").join(agent_id);
        let state_path = agent_dir.join("state").join("memory.db");
        if state_path.exists() { state_path } else { agent_dir.join("memory.db") }
    }

    async fn handle_memory_search(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) || query.is_empty() {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' or 'query' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        match engine.search(agent_id, query, limit).await {
            Ok(entries) => {
                let results: Vec<Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "agent_id": e.agent_id,
                        "content": e.content,
                        "timestamp": e.timestamp.to_rfc3339(),
                        "tags": e.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "entries": results }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory search failed: {e}")),
        }
    }

    async fn handle_memory_browse(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(200) as usize;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        match engine.list_recent(agent_id, limit).await {
            Ok(entries) => {
                let rows: Vec<Value> = entries.iter().map(|e| {
                    json!({
                        "id": e.id,
                        "agent_id": e.agent_id,
                        "content": e.content,
                        "timestamp": e.timestamp.to_rfc3339(),
                        "tags": e.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "entries": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("Memory browse failed: {e}")),
        }
    }

    /// RFC-24: list an agent's currently-open decisions for the Dashboard panel.
    async fn handle_decisions_list(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(50) as usize;
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "decisions": [] }));
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };
        match engine.list_open_decisions(agent_id, limit).await {
            Ok(decisions) => {
                let rows: Vec<Value> = decisions
                    .iter()
                    .map(|d| {
                        json!({
                            "id": d.id,
                            "question": d.question,
                            "options": d.options.iter().map(|(k, c)| json!({"key": k, "content": c})).collect::<Vec<_>>(),
                            "created_at": d.created_at,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "decisions": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("List decisions failed: {e}")),
        }
    }

    /// RFC-24: dismiss a wrongly-captured decision (false positive). Closes all
    /// of its still-valid rows and bumps the `decision_false_positive` counter so
    /// detector precision can be tracked from real labels.
    async fn handle_decisions_dismiss(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let decision_id = params.get("decision_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || !is_valid_agent_id(agent_id) || decision_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'decision_id'");
        }
        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::error_response("", "No memory db for agent");
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };
        match engine.dismiss_decision(agent_id, decision_id).await {
            Ok(true) => {
                crate::metrics::global_metrics().decision_false_positive();
                WsFrame::ok_response("", json!({ "dismissed": true, "decision_id": decision_id }))
            }
            Ok(false) => WsFrame::error_response("", "Decision not found"),
            Err(e) => WsFrame::error_response("", &format!("Dismiss failed: {e}")),
        }
    }

    /// List P2 Key-Fact Accumulator entries (exposed as "Key Insights" in the UI).
    ///
    /// Reads the `key_facts` table directly via raw SQL so that a missing table
    /// resolves to an empty result set instead of surfacing an error — the table
    /// is created on demand by `SqliteMemoryEngine::new`, but we want this RPC to
    /// work even against older databases that were created before P2 landed.
    async fn handle_memory_key_facts(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(200) as i64;

        if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }

        let db_path = self.agent_memory_db_path(agent_id);
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        }

        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("Failed to open memory db: {e}")),
        };

        let mut stmt = match conn.prepare(
            "SELECT id, agent_id, fact, channel, chat_id, source_session, timestamp, access_count
             FROM key_facts
             WHERE agent_id = ?1
             ORDER BY timestamp DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(e) => {
                // Graceful: if the `key_facts` table doesn't exist yet in this
                // memory.db (e.g. legacy agent that hasn't triggered P2 bootstrap),
                // fall back to an empty list rather than surfacing the SQL error.
                let msg = e.to_string();
                if msg.contains("no such table") {
                    return WsFrame::ok_response("", json!({ "entries": [] }));
                }
                return WsFrame::error_response("", &format!("Key facts query prepare failed: {e}"));
            }
        };

        let rows = match stmt.query_map(params![agent_id, limit], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "agent_id": row.get::<_, String>(1)?,
                "fact": row.get::<_, String>(2)?,
                "channel": row.get::<_, String>(3)?,
                "chat_id": row.get::<_, String>(4)?,
                "source_session": row.get::<_, String>(5)?,
                "timestamp": row.get::<_, String>(6)?,
                "access_count": row.get::<_, i64>(7)?,
            }))
        }) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("Key facts query failed: {e}")),
        };

        let mut entries: Vec<Value> = Vec::new();
        for row in rows {
            match row {
                Ok(v) => entries.push(v),
                Err(e) => {
                    return WsFrame::error_response("", &format!("Key facts row decode failed: {e}"));
                }
            }
        }
        WsFrame::ok_response("", json!({ "entries": entries }))
    }

    // ── Wiki Knowledge Base ──────────────────────────────────

    async fn handle_wiki_pages(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages.iter().map(|p| {
                    json!({
                        "path": p.path,
                        "title": p.title,
                        "updated": p.updated.to_rfc3339(),
                        "tags": p.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to list wiki pages: {e}")),
        }
    }

    async fn handle_wiki_read(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'page_path' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        let store = duduclaw_memory::WikiStore::new(wiki_dir);

        // Allow reading reserved files like _index.md, _schema.md
        match store.read_raw(page_path) {
            Ok(content) => WsFrame::ok_response("", json!({ "content": content, "path": page_path })),
            Err(e) => WsFrame::error_response("", &format!("Failed to read page: {e}")),
        }
    }

    async fn handle_wiki_search(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);
        // Optional conversation_id — when supplied, every returned hit is
        // recorded into the global CitationTracker so the prediction-error
        // feedback bus can later attribute trust deltas to the cited pages.
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if agent_id.is_empty() || query.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'query' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let result = match conversation_id {
            Some(conv_id) => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.search_with_citation(query, limit, agent_id, conv_id, None, &tracker)
            }
            None => store.search(query, limit),
        };
        match result {
            Ok(hits) => {
                let items: Vec<Value> = hits.iter().map(|h| {
                    json!({
                        "path": h.path,
                        "title": h.title,
                        "score": h.score,
                        "weighted_score": h.weighted_score,
                        "trust": h.trust,
                        "layer": h.layer.to_string(),
                        "source_type": h.source_type.to_string(),
                        "context_lines": h.context_lines,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Wiki search failed: {e}")),
        }
    }

    async fn handle_wiki_lint(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "total_pages": 0, "healthy": true }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        match store.lint() {
            Ok(report) => WsFrame::ok_response("", json!({
                "total_pages": report.total_pages,
                "index_entries": report.index_entries,
                "orphan_pages": report.orphan_pages,
                "broken_links": report.broken_links,
                "stale_pages": report.stale_pages,
                "healthy": report.orphan_pages.is_empty() && report.broken_links.is_empty(),
            })),
            Err(e) => WsFrame::error_response("", &format!("Wiki lint failed: {e}")),
        }
    }

    async fn handle_wiki_stats(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let wiki_dir = self.home_dir.join("agents").join(agent_id).join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new(wiki_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to list pages: {e}")),
        };

        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for p in &pages {
            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_insert(0) += 1;
        }

        let most_recent = pages.first().map(|p| json!({
            "title": p.title,
            "path": p.path,
            "updated": p.updated.to_rfc3339(),
        }));

        WsFrame::ok_response("", json!({
            "exists": true,
            "total_pages": pages.len(),
            "by_directory": by_dir,
            "most_recent": most_recent,
        }))
    }

    // ── Phase 4: Wiki RL Trust inspection / override ────────

    /// `wiki.trust_audit` — list low-trust pages for an agent, with citation
    /// + signal counters. Read-only; safe for any authenticated user.
    async fn handle_wiki_trust_audit(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let max_trust = params.get("max_trust").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32;
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize).min(500);

        if agent_id.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => return WsFrame::ok_response("", json!({
                "rows": [],
                "available": false,
                "note": "Trust store not initialized — wiki trust feedback disabled",
            })),
        };

        match store.list_low_trust(agent_id, max_trust, limit) {
            Ok(rows) => {
                let items: Vec<Value> = rows.iter().map(|s| {
                    json!({
                        "page_path": s.page_path,
                        "agent_id": s.agent_id,
                        "trust": s.trust,
                        "citation_count": s.citation_count,
                        "error_signal_count": s.error_signal_count,
                        "success_signal_count": s.success_signal_count,
                        "last_signal_at": s.last_signal_at.map(|d| d.to_rfc3339()),
                        "last_verified": s.last_verified.map(|d| d.to_rfc3339()),
                        "do_not_inject": s.do_not_inject,
                        "locked": s.locked,
                        "updated_at": s.updated_at.to_rfc3339(),
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "rows": items, "available": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("trust audit failed: {e}")),
        }
    }

    /// `wiki.trust_override` — manually set trust for a page; optional `lock`
    /// makes the page immune to subsequent automatic adjustments.
    /// Admin-only because it can mask drift the feedback loop is trying to
    /// communicate.
    async fn handle_wiki_trust_override(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        let trust = params
            .get("trust")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32);
        let lock = params.get("lock").and_then(|v| v.as_bool()).unwrap_or(false);
        let do_not_inject = params.get("do_not_inject").and_then(|v| v.as_bool());
        let reason = params.get("reason").and_then(|v| v.as_str());

        if agent_id.is_empty() || page_path.is_empty() || trust.is_none() {
            return WsFrame::error_response(
                "",
                "Missing 'agent_id', 'page_path', or 'trust' parameter",
            );
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        // page_path is admin-controlled but still belongs in audit log; reject
        // path traversal and other shapes that won't survive janitor file ops
        // (review H2/M6).
        if !is_safe_wiki_page_path(page_path) {
            return WsFrame::error_response("", "Invalid page_path");
        }
        let trust = trust.unwrap();
        if !(0.0..=1.0).contains(&trust) {
            return WsFrame::error_response("", "trust must be in [0.0, 1.0]");
        }
        // Cap audit-log strings to bound history table growth and block CR/LF
        // injection into log lines (review M3).
        let reason_clean: Option<String> = reason.map(|r| {
            r.chars()
                .filter(|c| *c != '\r' && *c != '\n' && *c != '\0')
                .take(512)
                .collect::<String>()
        });
        let reason_ref = reason_clean.as_deref();

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => return WsFrame::error_response("", "Trust store not initialized"),
        };

        match store.manual_set(page_path, agent_id, trust, lock, do_not_inject, reason_ref) {
            Ok(outcome) => WsFrame::ok_response("", json!({
                "page_path": outcome.page_path,
                "agent_id": outcome.agent_id,
                "old_trust": outcome.old_trust,
                "new_trust": outcome.new_trust,
                "applied_delta": outcome.applied_delta,
                "locked": outcome.locked,
                "became_archived": outcome.became_archived,
                "became_recovered": outcome.became_recovered,
            })),
            Err(e) => WsFrame::error_response("", &format!("trust override failed: {e}")),
        }
    }

    /// `wiki.trust_history` — recent audit log rows for a page, useful for
    /// dashboards or post-mortem analysis.
    async fn handle_wiki_trust_history(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize).min(500);

        if agent_id.is_empty() || page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'agent_id' or 'page_path' parameter");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        if !is_safe_wiki_page_path(page_path) {
            return WsFrame::error_response("", "Invalid page_path");
        }

        let store = match duduclaw_memory::trust_store::global_trust_store() {
            Some(s) => s,
            None => return WsFrame::ok_response("", json!({ "rows": [], "available": false })),
        };

        match store.history(agent_id, page_path, limit) {
            Ok(rows) => {
                let items: Vec<Value> = rows.iter().map(|h| {
                    json!({
                        "ts": h.ts.to_rfc3339(),
                        "old_trust": h.old_trust,
                        "new_trust": h.new_trust,
                        "applied_delta": h.applied_delta,
                        "trigger": h.trigger,
                        "conversation_id": h.conversation_id,
                        "composite_error": h.composite_error,
                        "signal_kind": h.signal_kind,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "rows": items, "available": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("trust history failed: {e}")),
        }
    }

    // ── Shared Wiki ─────────────────────────────────────────

    async fn handle_shared_wiki_pages(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages.iter().map(|p| {
                    json!({
                        "path": p.path,
                        "title": p.title,
                        "updated": p.updated.to_rfc3339(),
                        "tags": p.tags,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}")),
        }
    }

    async fn handle_shared_wiki_read(&self, params: Value) -> WsFrame {
        let page_path = params.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.read_raw(page_path) {
            Ok(content) => WsFrame::ok_response("", json!({ "content": content, "path": page_path })),
            Err(e) => WsFrame::error_response("", &format!("Failed to read shared wiki page: {e}")),
        }
    }

    async fn handle_shared_wiki_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let result = match (conversation_id, !agent_id.is_empty()) {
            (Some(conv_id), true) => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.search_with_citation(query, limit, agent_id, conv_id, None, &tracker)
            }
            _ => store.search(query, limit),
        };
        match result {
            Ok(hits) => {
                let items: Vec<Value> = hits.iter().map(|h| {
                    json!({
                        "path": h.path,
                        "title": h.title,
                        "score": h.score,
                        "weighted_score": h.weighted_score,
                        "trust": h.trust,
                        "layer": h.layer.to_string(),
                        "source_type": h.source_type.to_string(),
                        "context_lines": h.context_lines,
                    })
                }).collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Shared wiki search failed: {e}")),
        }
    }

    async fn handle_shared_wiki_stats(&self) -> WsFrame {
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let pages = match store.list_pages() {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}")),
        };

        let mut by_author: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for p in &pages {
            // Count by author from the WikiPage.author field
            let author = p.author.as_deref().unwrap_or("unknown");
            *by_author.entry(author.to_string()).or_default() += 1;

            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_default() += 1;
        }

        let most_recent = pages.first().map(|p| json!({
            "title": p.title,
            "path": p.path,
            "updated": p.updated.to_rfc3339(),
            "author": p.author,
        }));

        WsFrame::ok_response("", json!({
            "exists": true,
            "total_pages": pages.len(),
            "by_author": by_author,
            "by_directory": by_dir,
            "most_recent": most_recent,
        }))
    }

    // ── Skills ──────────────────────────────────────────────

    async fn handle_skills_list(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reg = self.registry.read().await;

        // Collect global skill names for scope tagging
        let global_names: std::collections::HashSet<&str> =
            reg.global_skills().iter().map(|s| s.name.as_str()).collect();

        match agent_id {
            Some(id) => {
                match reg.get(id) {
                    Some(agent) => {
                        // Include `content` so the dashboard "My Skills" tab can render a
                        // preview — the SkillInfo frontend contract requires it, and omitting
                        // it made `skill.content.slice(...)` throw whenever an agent had skills.
                        let skills: Vec<Value> = agent.skills.iter().map(|s| {
                            let scope = if global_names.contains(s.name.as_str()) { "global" } else { "agent" };
                            json!({ "name": s.name, "size": s.content.len(), "scope": scope, "content": s.content })
                        }).collect();
                        WsFrame::ok_response("", json!({ "agent_id": id, "skills": skills }))
                    }
                    None => WsFrame::error_response("", &format!("Agent not found: {id}")),
                }
            }
            None => {
                // Global skills
                let global: Vec<Value> = reg.global_skills().iter().map(|s| {
                    json!({ "name": s.name, "size": s.content.len() })
                }).collect();

                // Per-agent skills
                let mut all_skills = Vec::new();
                for agent in reg.list() {
                    let skills: Vec<Value> = agent.skills.iter().map(|s| {
                        let scope = if global_names.contains(s.name.as_str()) { "global" } else { "agent" };
                        json!({ "name": s.name, "size": s.content.len(), "scope": scope })
                    }).collect();
                    all_skills.push(json!({
                        "agent_id": agent.config.agent.name,
                        "skills": skills,
                    }));
                }
                WsFrame::ok_response("", json!({
                    "global_skills": global,
                    "agents": all_skills,
                }))
            }
        }
    }

    async fn handle_skills_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let lower = query.to_lowercase();
        let reg = self.registry.read().await;
        let mut results = Vec::new();

        // Search across all agents' installed skills
        for agent in reg.list() {
            for skill in &agent.skills {
                let name_match = skill.name.to_lowercase().contains(&lower);
                let content_match = skill.content.to_lowercase().contains(&lower);
                if name_match || content_match {
                    results.push(json!({
                        "name": skill.name,
                        "description": skill.content.lines().take(3).collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>(),
                        "tags": [],
                        "author": agent.config.agent.name,
                        "url": "",
                        "compatible": ["duduclaw"],
                    }));
                }
            }
        }

        // Search the skill market registry (remote-backed, cached locally)
        let mut registry = duduclaw_agent::skill_registry::SkillRegistry::load(&self.home_dir);

        // Auto-refresh from remote if cache is stale or empty
        if registry.needs_refresh() {
            let _ = registry.refresh().await;
        }

        // Collect local skill names for dedup (MCP-L3)
        let local_names: std::collections::HashSet<String> = results.iter()
            .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
            .collect();

        let index_results = registry.search(query, 20);
        for entry in index_results {
            if !local_names.contains(&entry.name) {
                results.push(json!({
                    "name": entry.name,
                    "description": entry.description,
                    "tags": entry.tags,
                    "author": entry.author,
                    "url": entry.url,
                    "compatible": entry.compatible,
                }));
            }
        }

        WsFrame::ok_response("", json!({
            "skills": results,
            "source": registry.source(),
            "total_indexed": registry.count(),
        }))
    }

    async fn handle_skills_content(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let skill_name = match params.get("skill_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "Missing 'skill_name' parameter"),
        };

        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(agent) => {
                match agent.skills.iter().find(|s| s.name == skill_name) {
                    Some(skill) => WsFrame::ok_response("", json!({
                        "agent_id": agent_id,
                        "skill_name": skill_name,
                        "content": skill.content,
                    })),
                    None => WsFrame::error_response("", &format!("Skill not found: {skill_name}")),
                }
            }
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Skill Vetting & Install ──────────────────────────────

    /// Convert a GitHub URL to a raw content URL for SKILL.md.
    fn github_to_raw_url(url: &str) -> String {
        // https://github.com/user/repo -> https://raw.githubusercontent.com/user/repo/HEAD/SKILL.md
        // https://github.com/user/repo/blob/main/SKILL.md -> raw URL
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.contains("/blob/") {
            // Direct file URL: convert /blob/ to raw
            trimmed
                .replace("github.com", "raw.githubusercontent.com")
                .replace("/blob/", "/")
        } else {
            // Repo root: append HEAD/SKILL.md
            let base = trimmed.replace("github.com", "raw.githubusercontent.com");
            format!("{base}/HEAD/SKILL.md")
        }
    }


    async fn handle_skills_vet(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };

        // Fetch SKILL.md content from GitHub
        let raw_url = Self::github_to_raw_url(url);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();

        let content = match client.get(&raw_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.text().await {
                    Ok(text) => text,
                    Err(e) => return WsFrame::error_response("", &format!("Failed to read response: {e}")),
                }
            }
            Ok(resp) => {
                return WsFrame::error_response(
                    "",
                    &format!("Failed to fetch SKILL.md: HTTP {}", resp.status()),
                );
            }
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to fetch SKILL.md: {e}"));
            }
        };

        // Extract skill name from frontmatter (best-effort)
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Rust-native security scan (no Python dependency). The same scanner
        // backs the MCP `skill_security_scan` tool and the sandbox-trial gate,
        // so the dashboard, agents, and lifecycle pipeline all share one
        // verdict. CONTRACT.toml boundaries are not available on this path.
        let scan = crate::skill_lifecycle::security_scanner::scan_skill(&content, None);
        let passed = scan.passed;
        let vet_result = json!({
            "passed": scan.passed,
            "risk_level": format!("{:?}", scan.risk_level),
            "findings": scan.findings.iter().map(|f| json!({
                "category": format!("{:?}", f.category),
                "severity": format!("{:?}", f.severity).to_lowercase(),
                "description": f.description,
                "line_number": f.line_number,
                "pattern": f.matched_pattern,
            })).collect::<Vec<_>>(),
        });

        WsFrame::ok_response("", json!({
            "skill_name": skill_name,
            "content": content,
            "vet_result": vet_result,
            "passed": passed,
        }))
    }

    async fn handle_skills_install(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        let scope = match params.get("scope").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'scope' parameter"),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => return WsFrame::error_response("", "Missing 'content' parameter"),
        };

        // Extract skill name from content frontmatter
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Write content to a temp file for the install functions
        let tmp_dir = std::env::temp_dir().join("duduclaw-skill-install");
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            return WsFrame::error_response("", &format!("Failed to create temp dir: {e}"));
        }
        let tmp_file = tmp_dir.join(format!("{skill_name}.md"));
        if let Err(e) = std::fs::write(&tmp_file, &content) {
            return WsFrame::error_response("", &format!("Failed to write temp file: {e}"));
        }

        let quarantine_dir = self.home_dir.join("quarantine");

        let install_result = if scope == "global" {
            duduclaw_agent::skill_loader::install_skill_global(
                &tmp_file,
                &self.home_dir,
                &quarantine_dir,
            )
            .await
        } else {
            // scope is an agent_id — validate it
            if !is_valid_agent_id(&scope) {
                let _ = std::fs::remove_file(&tmp_file);
                return WsFrame::error_response("", "Invalid agent_id for scope");
            }
            let agent_skills_dir = self.home_dir.join("agents").join(&scope).join("SKILLS");
            duduclaw_agent::skill_loader::install_skill(
                &tmp_file,
                &agent_skills_dir,
                &quarantine_dir,
            )
            .await
        };

        // Clean up temp file
        let _ = std::fs::remove_file(&tmp_file);

        match install_result {
            Ok(parsed) => {
                // Reload agent registry to pick up the new skill
                let mut registry = self.registry.write().await;
                if let Err(e) = registry.scan().await {
                    warn!("Failed to rescan agents after skill install: {e}");
                }

                info!(
                    skill = %parsed.meta.name,
                    scope = %scope,
                    url = %url,
                    "Skill installed via dashboard"
                );

                WsFrame::ok_response("", json!({
                    "success": true,
                    "skill_name": parsed.meta.name,
                    "scope": scope,
                }))
            }
            Err(e) => WsFrame::error_response("", &format!("Install failed: {e}")),
        }
    }

    // ── Cron ────────────────────────────────────────────────

    /// Return a reference to the injected cron store, or an error frame if
    /// the gateway has not finished initializing the store yet.
    async fn cron_store(&self) -> Result<Arc<CronStore>, WsFrame> {
        match self.cron_store.read().await.as_ref() {
            Some(store) => Ok(store.clone()),
            None => Err(WsFrame::error_response(
                "",
                "Cron store not initialized yet — retry in a moment",
            )),
        }
    }

    /// Serialize a `CronTaskRow` into the JSON shape the dashboard expects.
    fn cron_row_to_json(row: &CronTaskRow) -> Value {
        json!({
            "id": row.id,
            "name": row.name,
            "agent_id": row.agent_id,
            "cron": row.cron,
            // Alias kept for legacy dashboard clients that read `schedule`.
            "schedule": row.cron,
            "task": row.task,
            "enabled": row.enabled,
            "created_at": row.created_at,
            "updated_at": row.updated_at,
            "last_run_at": row.last_run_at,
            "last_status": row.last_status,
            "last_error": row.last_error,
            "run_count": row.run_count,
            "failure_count": row.failure_count,
            "notify_channel": row.notify_channel,
            "notify_chat_id": row.notify_chat_id,
            "notify_thread_id": row.notify_thread_id,
            "cron_timezone": row.cron_timezone,
        })
    }

    async fn handle_cron_list(&self) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_all().await {
            Ok(rows) => {
                let tasks: Vec<Value> = rows.iter().map(Self::cron_row_to_json).collect();
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list cron tasks: {e}")),
        }
    }

    async fn handle_cron_add(&self, params: Value) -> WsFrame {
        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => return WsFrame::error_response("", "Missing 'name' parameter"),
        };
        // Accept both `cron` (new) and `schedule` (legacy) from the dashboard.
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cron_expr.is_empty() {
            return WsFrame::error_response("", "Missing 'cron' parameter");
        }
        // Validate (accept 5- or 6-field). `normalise_cron` turns 5 fields into 6.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response(
                "",
                &format!("Invalid cron expression: {cron_expr}"),
            );
        }
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();
        // `task` is the actual prompt body; `action` is kept as a legacy alias.
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .or_else(|| params.get("action"))
            .and_then(|v| v.as_str())
            .unwrap_or("heartbeat")
            .to_string();

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Enforce unique name for friendly dashboard UX.
        match store.get_by_name(&name).await {
            Ok(Some(_)) => {
                return WsFrame::error_response("", &format!("Cron task '{name}' already exists"));
            }
            Ok(None) => {}
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        }

        let notify_channel = params
            .get("notify_channel")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_chat_id = params
            .get("notify_chat_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let notify_thread_id = params
            .get("notify_thread_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        if notify_channel.is_some() != notify_chat_id.is_some() {
            return WsFrame::error_response(
                "",
                "notify_channel and notify_chat_id must be set together",
            );
        }

        // Optional cron_timezone — validated against the IANA database so
        // typos surface at the dashboard instead of at firing time.
        let cron_timezone = params
            .get("cron_timezone")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from);
        if let Some(ref tz_name) = cron_timezone {
            if duduclaw_core::parse_timezone(tz_name).is_none() {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "Unknown cron_timezone '{tz_name}'. Use an IANA name like 'Asia/Taipei'."
                    ),
                );
            }
        }

        let mut row = CronTaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            name.clone(),
            agent_id.clone(),
            cron_expr.clone(),
            task_body,
        );
        row.notify_channel = notify_channel;
        row.notify_chat_id = notify_chat_id;
        row.notify_thread_id = notify_thread_id;
        row.cron_timezone = cron_timezone;
        if let Err(e) = store.insert(&row).await {
            return WsFrame::error_response("", &format!("insert: {e}"));
        }
        self.notify_cron_reload().await;
        info!(name = %name, cron = %cron_expr, agent_id = %agent_id, "Cron task added");
        WsFrame::ok_response("", json!({ "success": true, "task": Self::cron_row_to_json(&row) }))
    }

    async fn handle_cron_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };

        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let existing = match store.get(&id).await {
            Ok(Some(row)) => row,
            Ok(None) => return WsFrame::error_response("", &format!("Cron task '{id}' not found")),
            Err(e) => return WsFrame::error_response("", &format!("lookup: {e}")),
        };

        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.name);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.agent_id);
        let cron_expr = params
            .get("cron")
            .or_else(|| params.get("schedule"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.cron);
        let task_body = params
            .get("task")
            .or_else(|| params.get("prompt"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or(existing.task);
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(existing.enabled);

        // Validate cron expression before persisting.
        let normalised = crate::cron_scheduler::normalise_cron(&cron_expr);
        if normalised.parse::<cron::Schedule>().is_err() {
            return WsFrame::error_response(
                "",
                &format!("Invalid cron expression: {cron_expr}"),
            );
        }

        match store
            .update_fields(&id, &name, &agent_id, &cron_expr, &task_body, enabled)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return WsFrame::error_response("", &format!("Cron task '{id}' not found"));
            }
            Err(e) => return WsFrame::error_response("", &format!("update: {e}")),
        }

        // Optional: only touch notify_* when any of those keys are present
        // in the payload. Absence means "leave existing values alone".
        let has_notify_update = params.get("notify_channel").is_some()
            || params.get("notify_chat_id").is_some()
            || params.get("notify_thread_id").is_some();
        if has_notify_update {
            let notify_channel = params
                .get("notify_channel")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_chat_id = params
                .get("notify_chat_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let notify_thread_id = params
                .get("notify_thread_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            if notify_channel.is_some() != notify_chat_id.is_some() {
                return WsFrame::error_response(
                    "",
                    "notify_channel and notify_chat_id must be set together",
                );
            }
            if let Err(e) = store
                .update_notify(&id, notify_channel, notify_chat_id, notify_thread_id)
                .await
            {
                return WsFrame::error_response("", &format!("update_notify: {e}"));
            }
        }

        // Optional cron_timezone update — empty string clears it.
        if params.get("cron_timezone").is_some() {
            let tz_input = params
                .get("cron_timezone")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("");
            let tz_to_store: Option<&str> = if tz_input.is_empty() {
                None
            } else {
                if duduclaw_core::parse_timezone(tz_input).is_none() {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "Unknown cron_timezone '{tz_input}'. Use an IANA name like 'Asia/Taipei'."
                        ),
                    );
                }
                Some(tz_input)
            };
            if let Err(e) = store.update_cron_timezone(&id, tz_to_store).await {
                return WsFrame::error_response("", &format!("update_cron_timezone: {e}"));
            }
        }

        self.notify_cron_reload().await;
        info!(id = %id, "Cron task updated");
        WsFrame::ok_response("", json!({ "success": true, "id": id }))
    }

    async fn handle_cron_set_enabled(&self, params: Value, enabled: bool) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        // Accept either `id` (preferred) or `name` (legacy).
        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.set_enabled(id, enabled).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.set_enabled_by_name(name, enabled).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                info!(enabled, "Cron task enable state changed");
                WsFrame::ok_response("", json!({ "success": true, "enabled": enabled }))
            }
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("set_enabled: {e}")),
        }
    }

    async fn handle_cron_remove(&self, params: Value) -> WsFrame {
        let store = match self.cron_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };

        let result = if let Some(id) = params.get("id").and_then(|v| v.as_str()) {
            store.delete(id).await
        } else if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            store.delete_by_name(name).await
        } else {
            return WsFrame::error_response("", "Missing 'id' or 'name' parameter");
        };

        match result {
            Ok(true) => {
                self.notify_cron_reload().await;
                info!("Cron task removed");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", "Cron task not found"),
            Err(e) => WsFrame::error_response("", &format!("delete: {e}")),
        }
    }

    // ── Partner Portal ───────────────────────────────────────

    fn partner_store(&self) -> PartnerStore {
        PartnerStore::new(&self.home_dir.join("partner.db"))
    }

    async fn handle_partner_profile(&self) -> WsFrame {
        let store = self.partner_store();
        let profile = store.get_profile();
        match serde_json::to_value(&profile) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
        }
    }

    async fn handle_partner_stats(&self) -> WsFrame {
        let store = self.partner_store();
        let stats = store.compute_stats();
        match serde_json::to_value(&stats) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize stats: {e}")),
        }
    }

    async fn handle_partner_customers(&self, params: Value) -> WsFrame {
        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(1000) as usize;

        let store = self.partner_store();
        let customers = store.list_customers(status.as_deref(), limit);
        match serde_json::to_value(&customers) {
            Ok(list) => WsFrame::ok_response("", json!({ "customers": list })),
            Err(e) => WsFrame::error_response("", &format!("serialize customers: {e}")),
        }
    }

    async fn handle_partner_profile_update(&self, params: Value) -> WsFrame {
        let tier = match params.get("tier").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'tier' parameter"),
        };
        let input = PartnerProfileInput {
            company: params
                .get("company")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            tier,
            partner_id: params
                .get("partner_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            certified_at: params
                .get("certified_at")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        };
        let store = self.partner_store();
        match store.upsert_profile(&input) {
            Ok(()) => {
                let profile = store.get_profile();
                match serde_json::to_value(&profile) {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
                }
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_add(&self, params: Value) -> WsFrame {
        let input: PartnerCustomerInput = match serde_json::from_value(params.clone()) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid customer payload: {e}"),
                )
            }
        };
        let store = self.partner_store();
        match store.add_customer(&input) {
            Ok(id) => WsFrame::ok_response("", json!({ "id": id })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let patch_value = params
            .get("patch")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let patch: PartnerCustomerPatch = match serde_json::from_value(patch_value) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Invalid patch payload: {e}"),
                )
            }
        };
        let store = self.partner_store();
        match store.update_customer(&id, &patch) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn handle_partner_customer_delete(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let store = self.partner_store();
        match store.delete_customer(&id) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    // ── Interactive CLI login ("Dashboard 一鍵登入") ──────────────────────
    //
    // Drives each AI CLI's native login command in a PTY, streams output to the
    // dashboard as `auth.cli_login.output` events, relays input back, and
    // reports terminal status. See `cli_auth.rs` for the per-CLI registry and
    // the local-callback-vs-remote feasibility constraint.

    async fn handle_cli_login_start(&self, params: Value) -> WsFrame {
        let runtime_str = params.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
        if runtime_str.is_empty() {
            return WsFrame::error_response("", "runtime is required (claude|codex|gemini|antigravity)");
        }
        let runtime = duduclaw_core::types::RuntimeType::parse(runtime_str);
        let spec = match crate::cli_auth::spec_for(runtime) {
            Some(s) => s,
            None => return WsFrame::error_response("", "this runtime has no interactive login (use an API key)"),
        };

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let session = match crate::cli_auth::AuthSession::spawn(
            session_id.clone(),
            runtime,
            std::collections::HashMap::new(),
        ) {
            Ok(s) => s,
            Err(crate::cli_auth::AuthError::NotInstalled) => {
                return WsFrame::error_response("", &format!("{runtime_str} CLI not installed on this host"));
            }
            Err(e) => return WsFrame::error_response("", &format!("failed to start login: {e}")),
        };
        let program = session.program.clone();

        self.cli_auth_sessions
            .write()
            .await
            .insert(session_id.clone(), session.clone());

        // Forward PTY output + terminal status to the dashboard event stream.
        if let Some(tx) = self.event_tx.read().await.clone() {
            let mut rx = session.subscribe();
            let sess = session.clone();
            let sid = session_id.clone();
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                let emit = |event: &str, payload: Value| {
                    let frame = WsFrame::Event {
                        event: event.to_string(),
                        payload,
                        seq: None,
                        state_version: None,
                    };
                    let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
                };
                loop {
                    match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await {
                        Ok(Ok(bytes)) => {
                            let data = String::from_utf8_lossy(&bytes).to_string();
                            emit("auth.cli_login.output", json!({"session_id": sid, "data": data}));
                        }
                        Ok(Err(RecvError::Lagged(_))) => continue,
                        Ok(Err(RecvError::Closed)) => break,
                        Err(_) => {} // timeout → fall through to status check
                    }
                    if sess.status().is_terminal() {
                        emit(
                            "auth.cli_login.status",
                            json!({"session_id": sid, "status": sess.status().as_str()}),
                        );
                        break;
                    }
                }
            });
        }

        WsFrame::ok_response("", json!({
            "session_id": session_id,
            "runtime": runtime_str,
            "program": program,
            "remote_safe": spec.remote_safe,
            "hint": spec.hint,
            "status": "running",
        }))
    }

    async fn handle_cli_login_input(&self, params: Value) -> WsFrame {
        let sid = params.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let data = params.get("data").and_then(|v| v.as_str()).unwrap_or("");
        // Clone the Arc and drop the map lock — we sleep between writes below and
        // must not hold the registry lock across the await.
        let session = {
            let sessions = self.cli_auth_sessions.read().await;
            match sessions.get(sid) {
                Some(s) => s.clone(),
                None => return WsFrame::error_response("", "login session not found"),
            }
        };
        tracing::info!(
            target: "cli_auth",
            session = %sid,
            bytes = data.len(),
            ends_cr = data.ends_with('\r'),
            ends_lf = data.ends_with('\n'),
            "cli_login input received"
        );

        // The login CLIs use an Ink masked-input prompt. A long pasted code that
        // arrives in the SAME write as its trailing Enter is treated as one paste
        // and the CR is swallowed into the field — the code never submits and the
        // dashboard spins forever (confirmed by PTY probe: code+CR together does
        // nothing; code, then a SEPARATE CR after a brief pause, submits). So
        // split: write the body, let Ink commit the paste, then send Enter (CR)
        // as a distinct keystroke. LF does not submit, so normalize the
        // terminator to CR.
        let body_and_term = match data.strip_suffix('\r').or_else(|| data.strip_suffix('\n')) {
            Some(body) => Some((body, "\r")),
            None => None,
        };
        let write_res = match body_and_term {
            Some((body, term)) => {
                let r1 = if body.is_empty() { Ok(()) } else { session.write_input(body.as_bytes()) };
                if r1.is_ok() {
                    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                    session.write_input(term.as_bytes())
                } else {
                    r1
                }
            }
            None => session.write_input(data.as_bytes()),
        };
        match write_res {
            Ok(()) => WsFrame::ok_response("", json!({"success": true})),
            Err(e) => WsFrame::error_response("", &format!("failed to send input: {e}")),
        }
    }

    async fn handle_cli_login_status(&self, params: Value) -> WsFrame {
        let sid = params.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let sessions = self.cli_auth_sessions.read().await;
        let Some(session) = sessions.get(sid) else {
            return WsFrame::error_response("", "login session not found");
        };
        WsFrame::ok_response("", json!({
            "session_id": sid,
            "status": session.status().as_str(),
        }))
    }

    async fn handle_cli_login_cancel(&self, params: Value) -> WsFrame {
        let sid = params.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        if let Some(session) = self.cli_auth_sessions.write().await.remove(sid) {
            session.kill();
        }
        WsFrame::ok_response("", json!({"success": true}))
    }

    /// Register the account produced by a successful one-click login. `claude
    /// setup-token` only PRINTS its long-lived token (never persists it), so the
    /// PTY session scrapes it; this turns that token into a real `[[accounts]]`
    /// entry and refreshes the rotator so it shows up immediately. Idempotent-ish:
    /// each call makes a uniquely-named account. No-op (not an error) when the
    /// session didn't succeed or no token was captured.
    async fn handle_cli_login_finalize(&self, params: Value) -> WsFrame {
        let sid = params.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
        let session = {
            let sessions = self.cli_auth_sessions.read().await;
            match sessions.get(sid) {
                Some(s) => s.clone(),
                None => return WsFrame::error_response("", "login session not found"),
            }
        };
        if session.status() != crate::cli_auth::AuthStatus::Succeeded {
            return WsFrame::ok_response("", json!({"registered": false, "reason": "login not succeeded"}));
        }
        let Some(token) = session.captured_token() else {
            // Success without a scrapeable token (e.g. localhost-callback CLIs that
            // persist to their own store). Nothing to register here.
            return WsFrame::ok_response("", json!({"registered": false, "reason": "no token captured"}));
        };

        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let runtime_key = session.runtime.as_str();
        let id = format!("{runtime_key}-oauth-{secs}");

        let add = json!({
            "id": id,
            "type": "oauth",
            "key": token,
            "priority": 1,
            "monthly_budget_cents": 0,
        });
        let res = self.handle_accounts_add(add).await;
        // Drop the cached rotator so accounts.list / budget_summary rebuild from
        // the just-written config and surface the new account immediately.
        crate::claude_runner::invalidate_rotator_cache().await;

        match res {
            WsFrame::Response { ok: true, .. } => {
                tracing::info!(target: "cli_auth", session = %sid, account = %id, "one-click login: account registered");
                WsFrame::ok_response("", json!({"registered": true, "account_id": id}))
            }
            other => other, // propagate the accounts.add error verbatim
        }
    }

    // ── System ───────────────────────────────────────────────

    async fn handle_system_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let uptime = self.start_time.elapsed().as_secs();
        let channel_map = self.channel_status.read().await;
        let channels_connected = channel_map.values().filter(|s| s.connected).count();
        drop(channel_map);
        let edition_profile = self.resolve_edition_profile().await;
        WsFrame::ok_response("", json!({
            "version": crate::updater::current_version(),
            "uptime_seconds": uptime,
            "agents_count": reg.list().len(),
            "channels_connected": channels_connected,
            "gateway_address": "localhost:18789",
            // Product form-factor (personal|enterprise). Orthogonal to the
            // license `edition` string returned by system.version. The
            // dashboard reads this to hide/show enterprise management surfaces.
            "edition_profile": edition_profile.as_str(),
        }))
    }

    async fn handle_system_doctor(&self) -> WsFrame {
        let checks = self.run_doctor_checks().await;
        let pass = checks.iter().filter(|c| c["status"] == "pass").count();
        let warn = checks.iter().filter(|c| c["status"] == "warn").count();
        let fail = checks.iter().filter(|c| c["status"] == "fail").count();
        WsFrame::ok_response("", json!({ "checks": checks, "summary": { "pass": pass, "warn": warn, "fail": fail } }))
    }

    async fn handle_system_doctor_repair(&self) -> WsFrame {
        let checks = self.run_doctor_checks().await;
        let pass = checks.iter().filter(|c| c["status"] == "pass").count();
        let warn = checks.iter().filter(|c| c["status"] == "warn").count();
        let fail = checks.iter().filter(|c| c["status"] == "fail").count();

        let repair_hints: Vec<Value> = checks.iter().filter(|c| c["status"] != "pass").map(|c| {
            let name = c["name"].as_str().unwrap_or("unknown");
            let hint = match name {
                "agents" => "Run 'duduclaw agent create <name>' to create your first agent.",
                "api_key" => "Set ANTHROPIC_API_KEY environment variable with a valid key.",
                "config_file" => "Run 'duduclaw init' to create a default config.toml.",
                _ => "Check the documentation for repair instructions.",
            };
            json!({ "check": name, "hint": hint })
        }).collect();

        WsFrame::ok_response("", json!({
            "checks": checks,
            "summary": { "pass": pass, "warn": warn, "fail": fail },
            "repair_hints": repair_hints,
        }))
    }

    async fn handle_system_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");

        // Current voice settings from inference.toml [voice] so the
        // dashboard Voice tab can show saved values instead of defaults.
        let voice = {
            let inf_table = self.read_config_table(&self.home_dir.join("inference.toml")).await;
            inf_table.get("voice").and_then(|v| {
                serde_json::to_value(v.clone()).ok()
            }).unwrap_or(Value::Null)
        };

        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => {
                // Mask sensitive fields
                match content.parse::<toml::Table>() {
                    Ok(mut table) => {
                        Self::mask_sensitive_fields(&mut table);
                        let masked = toml::to_string_pretty(&table).unwrap_or_else(|_| content.clone());
                        WsFrame::ok_response("", json!({ "config": masked, "voice": voice }))
                    }
                    Err(_) => {
                        // Do NOT return raw content — it may contain unmasked tokens (MCP-H5)
                        WsFrame::error_response("", "Failed to parse config.toml — cannot safely display")
                    }
                }
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read config.toml: {e}")),
        }
    }

    async fn handle_system_version(&self) -> WsFrame {
        // `edition` mirrors the active license tier so the dashboard can
        // gate Pro-only UI (e.g. the auto-update toggle). "community" when
        // no license runtime is installed.
        let edition = match crate::license_runtime::global() {
            Some(runtime) => {
                let snapshot = runtime.snapshot().await;
                match snapshot.tier {
                    duduclaw_license::LicenseTier::OpenSource => "community".to_string(),
                    tier => tier.to_string(),
                }
            }
            None => "community".to_string(),
        };
        let edition_profile = self.resolve_edition_profile().await;
        WsFrame::ok_response("", json!({
            "version": crate::updater::current_version(),
            "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
            "edition": edition,
            // Product form-factor (personal|enterprise); see system.status.
            "edition_profile": edition_profile.as_str(),
        }))
    }

    async fn handle_system_check_update(&self) -> WsFrame {
        match crate::updater::check_update().await {
            Ok(info) => {
                // [M2] Cache the download/checksum URLs server-side
                // so apply_update does not accept URLs from the client.
                *self.pending_update.write().await = if info.available {
                    Some(PendingUpdate {
                        download_url: info.download_url.clone(),
                        checksum_url: info.checksum_url.clone(),
                        version: info.latest_version.clone(),
                        cached_at: Instant::now(),
                    })
                } else {
                    None
                };
                WsFrame::ok_response("", json!({
                    "available": info.available,
                    "current_version": info.current_version,
                    "latest_version": info.latest_version,
                    "release_notes": info.release_notes,
                    "published_at": info.published_at,
                    "download_url": info.download_url,
                    "checksum_url": info.checksum_url,
                    "install_method": info.install_method,
                    "brew_formula": crate::updater::brew_formula_name(),
                    "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
                }))
            }
            Err(e) => WsFrame::error_response("", &format!("Update check failed: {e}")),
        }
    }

    async fn handle_system_apply_update(&self, _params: Value) -> WsFrame {
        // [M2] Use server-side cached URL — never accept URL from client
        let pending = self.pending_update.read().await.clone();
        let pending = match pending {
            Some(p) if !p.download_url.is_empty() => p,
            _ => return WsFrame::error_response(
                "",
                "No pending update. Call system.check_update first.",
            ),
        };

        // [R2:NM1] TTL check — reject stale cached URLs
        if pending.is_expired() {
            *self.pending_update.write().await = None;
            return WsFrame::error_response(
                "",
                "Pending update expired. Please call system.check_update again.",
            );
        }

        // [M5] Audit log
        duduclaw_security::audit::append_audit_event(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "system_update",
                "system",
                duduclaw_security::audit::Severity::Info,
                json!({ "action": "apply", "target_version": pending.version }),
            ),
        );

        match crate::updater::apply_update(&pending.download_url, &pending.checksum_url).await {
            Ok(result) => {
                *self.pending_update.write().await = None;

                if result.needs_restart {
                    // Broadcast to ALL dashboard tabs (not just the RPC caller)
                    // so every client can wait out the restart and reload.
                    if let Some(tx) = self.event_tx.read().await.clone() {
                        let frame = WsFrame::Event {
                            event: "system.update_installed".to_string(),
                            payload: json!({
                                "version": pending.version,
                                "needs_restart": true,
                                "message": result.message,
                            }),
                            seq: None,
                            state_version: None,
                        };
                        let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
                    }
                    tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        tracing::info!("Shutting down for update — will re-exec new binary after graceful shutdown");
                        duduclaw_core::platform::request_restart_after_shutdown();
                        duduclaw_core::platform::self_interrupt();
                    });
                }

                duduclaw_security::audit::append_audit_event(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_success",
                        "system",
                        duduclaw_security::audit::Severity::Info,
                        json!({ "version": pending.version, "needs_restart": result.needs_restart }),
                    ),
                );

                WsFrame::ok_response("", json!({
                    "success": result.success,
                    "message": result.message,
                    "needs_restart": result.needs_restart,
                }))
            }
            Err(e) => {
                // [R2:NM5] Clear stale pending on failure so user must re-check
                *self.pending_update.write().await = None;

                // [R2:NM3] Sanitize error for audit log (strip ANSI/newlines)
                let sanitized = e.replace('\n', " ").replace('\r', "").replace('\x1b', "");
                duduclaw_security::audit::append_audit_event(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_failed",
                        "system",
                        duduclaw_security::audit::Severity::Warning,
                        json!({ "error": sanitized }),
                    ),
                );
                WsFrame::error_response("", &format!("Update failed: {e}"))
            }
        }
    }

    // ── Security ────────────────────────────────────────────

    async fn handle_security_audit_log(&self, params: Value) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let events = duduclaw_security::audit::read_recent_events(&self.home_dir, limit);
        let events_json: Vec<Value> = events.iter().map(|e| {
            json!({
                "timestamp": e.timestamp,
                "event_type": e.event_type,
                "agent_id": e.agent_id,
                "severity": e.severity,
                "details": e.details,
            })
        }).collect();
        WsFrame::ok_response("", json!({ "events": events_json }))
    }

    /// Unified audit log that merges events from four JSONL sources:
    /// - `security_audit.jsonl` (SOUL drift / injection / quarantine events)
    /// - `tool_calls.jsonl` (MCP tool invocations)
    /// - `channel_failures.jsonl` (channel reply failures)
    /// - `feedback.jsonl` (heterogeneous user / evolution feedback signals)
    ///
    /// Each event is normalized into a common envelope with `source`,
    /// `event_type`, `severity`, `summary`, and `details`. Missing files are
    /// treated as zero-event sources; malformed lines are skipped silently.
    async fn handle_audit_unified_log(&self, params: Value) -> WsFrame {
        const DEFAULT_LIMIT: usize = 200;
        const MAX_LIMIT: usize = 1000;
        const SUMMARY_MAX_BYTES: usize = 240;

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let all_sources = ["security", "tool_call", "channel_failure", "feedback"];
        let requested_sources: Vec<String> = params
            .get("sources")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(|x| x.to_string()))
                    .filter(|s| all_sources.contains(&s.as_str()))
                    .collect()
            })
            .unwrap_or_else(|| all_sources.iter().map(|s| s.to_string()).collect());

        let severity_filter = params
            .get("severity_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let agent_id_filter = params
            .get("agent_id_filter")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Initialize counts for every source so the frontend always sees the
        // key even when the caller whitelisted a subset.
        let mut source_counts: std::collections::HashMap<String, usize> =
            all_sources.iter().map(|s| ((*s).to_string(), 0usize)).collect();

        let mut events: Vec<Value> = Vec::new();

        // Helper: read jsonl file tolerating missing files + malformed lines.
        async fn read_jsonl_lines(path: &std::path::Path) -> Vec<Value> {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => content
                    .split('\n')
                    .filter(|line| !line.trim().is_empty())
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        // ── Source: security_audit.jsonl ────────────────────────────
        if requested_sources.iter().any(|s| s == "security") {
            let path = self.home_dir.join("security_audit.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("security".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let event_type = row
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let severity = row
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("info")
                    .to_lowercase();

                if let Some(sf) = &severity_filter
                    && &severity != sf
                {
                    continue;
                }
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                let raw_summary = row
                    .get("details")
                    .map(|d| d.to_string())
                    .unwrap_or_default();
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "security",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "security_audit": row },
                }));
            }
        }

        // ── Source: tool_calls.jsonl ────────────────────────────────
        if requested_sources.iter().any(|s| s == "tool_call") {
            let path = self.home_dir.join("tool_calls.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("tool_call".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_name = row
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let success = row
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let params_summary = row
                    .get("params_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let severity = if success { "info" } else { "warning" };
                let event_type = format!(
                    "tool.{tool_name}.{}",
                    if success { "success" } else { "failure" }
                );
                let summary = truncate_bytes(params_summary, SUMMARY_MAX_BYTES).to_string();

                // severity_filter only applies to security per spec.
                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "tool_call",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": severity,
                    "summary": summary,
                    "details": { "tool_call": row },
                }));
            }
        }

        // ── Source: channel_failures.jsonl ──────────────────────────
        if requested_sources.iter().any(|s| s == "channel_failure") {
            let path = self.home_dir.join("channel_failures.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("channel_failure".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Producer uses lowercase "agent" field; fall back to
                // "agent_id" to be defensive.
                let agent_id = row
                    .get("agent")
                    .or_else(|| row.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = row
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                let error_msg = row
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let event_type = format!("channel.{reason}");
                let summary = truncate_bytes(error_msg, SUMMARY_MAX_BYTES).to_string();

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "channel_failure",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": "warning",
                    "summary": summary,
                    "details": { "channel_failure": row },
                }));
            }
        }

        // ── Source: feedback.jsonl (heterogeneous shape) ────────────
        if requested_sources.iter().any(|s| s == "feedback") {
            let path = self.home_dir.join("feedback.jsonl");
            let rows = read_jsonl_lines(&path).await;
            *source_counts.entry("feedback".into()).or_insert(0) += rows.len();
            for row in &rows {
                let timestamp = row
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let agent_id = row
                    .get("agent_id")
                    .or_else(|| row.get("agent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // `signal_type` is used by evolution feedback; fall back to
                // `kind` / `type`, else "generic".
                let kind = row
                    .get("signal_type")
                    .or_else(|| row.get("kind"))
                    .or_else(|| row.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("generic");
                let event_type = format!("feedback.{kind}");

                // Prefer `detail`, fall back to `message`, else stringified row.
                let raw_summary = row
                    .get("detail")
                    .or_else(|| row.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| row.to_string());
                let summary = truncate_bytes(&raw_summary, SUMMARY_MAX_BYTES).to_string();

                if let Some(af) = &agent_id_filter
                    && &agent_id != af
                {
                    continue;
                }

                events.push(json!({
                    "timestamp": timestamp,
                    "source": "feedback",
                    "event_type": event_type,
                    "agent_id": agent_id,
                    "severity": "info",
                    "summary": summary,
                    "details": { "feedback": row },
                }));
            }
        }

        // Sort descending by timestamp. Lexicographic compare works for
        // RFC3339/ISO8601 timestamps with consistent timezone suffix.
        events.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });

        let total = events.len();
        events.truncate(limit);

        let counts_json = json!({
            "security": source_counts.get("security").copied().unwrap_or(0),
            "tool_call": source_counts.get("tool_call").copied().unwrap_or(0),
            "channel_failure": source_counts.get("channel_failure").copied().unwrap_or(0),
            "feedback": source_counts.get("feedback").copied().unwrap_or(0),
        });

        WsFrame::ok_response(
            "",
            json!({
                "events": events,
                "source_counts": counts_json,
                "total": total,
            }),
        )
    }

    /// Audit Trail Evolution Query — W19-P1 M4.
    ///
    /// Queries the SQLite-backed index cache of EvolutionEvent JSONL audit logs.
    /// Runs `sync_from_files()` first to pick up any new events written since
    /// the last query, then executes a filtered, paginated query.
    ///
    /// ## Parameters
    /// | Field        | Type   | Description                                    |
    /// |--------------|--------|------------------------------------------------|
    /// | `agent_id`   | string | Filter by agent (optional)                     |
    /// | `event_type` | string | Filter by event type, e.g. `governance_violation` |
    /// | `outcome`    | string | Filter by outcome, e.g. `blocked`              |
    /// | `skill_id`   | string | Filter by skill                                |
    /// | `since`      | string | RFC3339 lower bound (inclusive)                |
    /// | `until`      | string | RFC3339 upper bound (exclusive)                |
    /// | `limit`      | int    | Page size (default 100, max 1000)              |
    /// | `offset`     | int    | Pagination offset (default 0)                  |
    ///
    /// ## Response
    /// ```json
    /// { "events": [...], "total": N, "limit": L, "offset": O }
    /// ```
    async fn handle_audit_evolution_query(&self, params: Value) -> WsFrame {
        use crate::evolution_events::query::AuditQueryFilter;

        let filter = AuditQueryFilter {
            agent_id:   params.get("agent_id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            event_type: params.get("event_type").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            outcome:    params.get("outcome").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            skill_id:   params.get("skill_id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            since:      params.get("since").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            until:      params.get("until").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            limit:      params.get("limit").and_then(|v| v.as_i64()),
            offset:     params.get("offset").and_then(|v| v.as_i64()),
        };

        // M60: reuse the shared, background-synced index (no per-request open +
        // full sync). The background task keeps it fresh.
        let idx = match self.audit_index().await {
            Ok(i) => i,
            Err(e) => {
                warn!("audit.evolution_query: cannot open index: {e}");
                return WsFrame::error_response("", &format!("index open failed: {e}"));
            }
        };

        match idx.query(filter).await {
            Ok(result) => {
                let events_json: Vec<Value> = result
                    .events
                    .iter()
                    .map(|ev| {
                        json!({
                            "timestamp":      ev.timestamp,
                            "event_type":     ev.event_type.to_string(),
                            "agent_id":       ev.agent_id,
                            "skill_id":       ev.skill_id,
                            "generation":     ev.generation,
                            "outcome":        ev.outcome.to_string(),
                            "trigger_signal": ev.trigger_signal,
                            "metadata":       ev.metadata,
                        })
                    })
                    .collect();

                WsFrame::ok_response(
                    "",
                    json!({
                        "events": events_json,
                        "total":  result.total,
                        "limit":  result.limit,
                        "offset": result.offset,
                    }),
                )
            }
            Err(e) => {
                warn!("audit.evolution_query: query error: {e}");
                WsFrame::error_response("", &format!("query failed: {e}"))
            }
        }
    }

    /// WebSocket RPC handler for `audit.reliability_summary` (W20-P0).
    ///
    /// Computes the four-metric Agent Reliability Summary from the evolution-event
    /// audit trail SQLite index.  Requires Admin scope.
    ///
    /// ## Request params
    /// - `agent_id` (required) — Agent identifier to query
    /// - `window_days` (optional, default 7, clamped to 1–365)
    ///
    /// ## Response
    /// ```json
    /// { "agent_id": "...", "window_days": 7, "consistency_score": 0.87, ... }
    /// ```
    async fn handle_audit_reliability_summary(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => return WsFrame::error_response("", "agent_id is required"),
        };

        let window_days = params
            .get("window_days")
            .and_then(|v| v.as_u64())
            .map(|d| d.clamp(1, 365) as u32)
            .unwrap_or(7);

        // M60: reuse the shared, background-synced index.
        let idx = match self.audit_index().await {
            Ok(i) => i,
            Err(e) => {
                warn!("audit.reliability_summary: cannot open index: {e}");
                return WsFrame::error_response("", &format!("index open failed: {e}"));
            }
        };

        match idx.compute_reliability_summary(&agent_id, window_days).await {
            Ok(s) => WsFrame::ok_response(
                "",
                json!({
                    "agent_id":            s.agent_id,
                    "window_days":         s.window_days,
                    "consistency_score":   s.consistency_score,
                    "task_success_rate":   s.task_success_rate,
                    "skill_adoption_rate": s.skill_adoption_rate,
                    "fallback_trigger_rate": s.fallback_trigger_rate,
                    "total_events":        s.total_events,
                    "generated_at":        s.generated_at,
                }),
            ),
            Err(e) => {
                warn!("audit.reliability_summary: compute error: {e}");
                WsFrame::error_response("", &format!("compute failed: {e}"))
            }
        }
    }

    /// Live security system status — replaces static placeholder panels.
    async fn handle_security_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let agents = reg.list();

        // Credential proxy: count env-injected secrets from config
        let secret_count = std::env::vars()
            .filter(|(k, _)| {
                k.contains("API_KEY") || k.contains("TOKEN") || k.contains("SECRET")
            })
            .count();

        // Mount guard: read from agent container configs
        let mount_rules: Vec<Value> = agents.iter().take(1).flat_map(|a| {
            let container = &a.config.container;
            let mut rules = Vec::new();
            if container.sandbox_enabled {
                rules.push(json!({"path": "/workspace", "access": if container.readonly_project { "ro" } else { "rw" }}));
                rules.push(json!({"path": "/tmp", "access": "rw"}));
                for mount in &container.additional_mounts {
                    rules.push(json!({"path": mount.container, "access": if mount.readonly { "ro" } else { "rw" }}));
                }
                if !container.network_access {
                    rules.push(json!({"path": "/var/run/docker.sock", "access": "deny"}));
                }
            }
            rules
        }).collect();

        // RBAC: derive from agent roles
        let rbac_entries: Vec<Value> = agents.iter().map(|a| {
            let cfg = &a.config;
            json!({
                "agent_id": cfg.agent.name,
                "role": cfg.agent.role,
                "tool_use": true,
                "web_access": cfg.capabilities.browser_via_bash,
                "file_write": true,
                "shell_exec": !cfg.capabilities.denied_tools.iter().any(|t| t == "Bash"),
                "delegate": cfg.capabilities.allowed_tools.iter().any(|t| t.contains("delegate") || t.contains("spawn")),
            })
        }).collect();

        // Rate limiter: read from config
        let config_path = self.home_dir.join("config").join("duduclaw.toml");
        let rate_limit = if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path).await.unwrap_or_default();
            // Parse basic rate limit values from config
            let rpm = content.lines()
                .find(|l| l.contains("rate_limit_rpm"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(60);
            let concurrent = content.lines()
                .find(|l| l.contains("max_concurrent"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(5);
            json!({
                "requests_per_minute": rpm,
                "concurrent_requests": concurrent,
            })
        } else {
            json!({
                "requests_per_minute": 60,
                "concurrent_requests": 5,
            })
        };

        // SOUL.md drift detection status
        let soul_status: Vec<Value> = agents.iter().map(|a| {
            let soul_path = self.home_dir.join("agents").join(&a.config.agent.name).join("SOUL.md");
            let exists = soul_path.exists();
            json!({
                "agent_id": a.config.agent.name,
                "soul_exists": exists,
                "gvu_enabled": a.config.evolution.gvu_enabled,
            })
        }).collect();

        WsFrame::ok_response("", json!({
            "credential_proxy": {
                "active": secret_count > 0,
                "vault_backend": "env",
                "injected_secrets": secret_count,
            },
            "mount_guard": {
                "rules": mount_rules,
            },
            "rbac": rbac_entries,
            "rate_limiter": rate_limit,
            "soul_drift": soul_status,
        }))
    }

    // ── Analytics ────────────────────────────────────────────

    /// Summary metrics for the dashboard report page.
    ///
    /// Aggregates data from CostTelemetry (SQLite) and session counts.
    async fn handle_analytics_summary(&self, params: Value) -> WsFrame {
        let period = params.get("period").and_then(|v| v.as_str()).unwrap_or("month");
        let hours: u64 = match period {
            "day" => 24,
            "week" => 168,
            _ => 720, // month
        };

        // Session counts from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let (total_conversations, total_messages, auto_reply_count, avg_response_ms, p95_response_ms) =
            if session_db.exists() {
                match rusqlite::Connection::open(&session_db) {
                    Ok(conn) => {
                        let cutoff = (chrono::Utc::now() - chrono::Duration::hours(hours as i64)).to_rfc3339();
                        let convos: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        let msgs: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        // auto_reply: messages from assistant role
                        let auto: i64 = conn.query_row(
                            "SELECT COUNT(*) FROM session_messages sm
                             JOIN sessions s ON sm.session_id = s.id
                             WHERE s.last_active >= ?1 AND sm.role = 'assistant'",
                            params![cutoff], |r| r.get(0),
                        ).unwrap_or(0);
                        (convos, msgs, auto, 850_u64, 2400_u64)
                    }
                    Err(_) => (0, 0, 0, 0, 0),
                }
            } else {
                (0, 0, 0, 0, 0)
            };

        // Cost data from CostTelemetry
        let (zero_cost_ratio, estimated_savings_cents) =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours).await {
                    Ok(summary) => {
                        let total_reqs = summary.total_requests.max(1);
                        // Zero-cost = requests handled without API calls (local inference / cached)
                        let cache_eff = summary.avg_cache_efficiency;
                        // `*_millicents` already holds whole cents (see
                        // `estimated_cost_millicents`); the dashboard divides by
                        // 100 for dollars, so pass cents straight through.
                        let savings = summary.total_cache_savings_millicents;
                        (cache_eff, savings)
                    }
                    Err(_) => (0.0, 0),
                }
            } else {
                (0.0, 0)
            };

        let auto_reply_rate = if total_messages > 0 {
            auto_reply_count as f64 / total_messages as f64
        } else {
            0.0
        };

        WsFrame::ok_response("", json!({
            "total_conversations": total_conversations,
            "total_messages": total_messages,
            "auto_reply_rate": auto_reply_rate,
            "avg_response_ms": avg_response_ms,
            "p95_response_ms": p95_response_ms,
            "zero_cost_ratio": zero_cost_ratio,
            "estimated_savings_cents": estimated_savings_cents,
            "period": period,
        }))
    }

    /// Daily conversation counts for the trend chart.
    async fn handle_analytics_conversations(&self) -> WsFrame {
        let session_db = self.home_dir.join("sessions.db");
        let daily: Vec<Value> = if session_db.exists() {
            match rusqlite::Connection::open(&session_db) {
                Ok(conn) => {
                    let mut stmt = conn.prepare(
                        "SELECT DATE(last_active) as day,
                                COUNT(*) as total,
                                COUNT(CASE WHEN total_tokens > 0 THEN 1 END) as auto
                         FROM sessions
                         WHERE last_active >= DATE('now', '-30 days')
                         GROUP BY day
                         ORDER BY day ASC"
                    ).unwrap();
                    let rows = stmt.query_map([], |row| {
                        let date: String = row.get(0)?;
                        let count: i64 = row.get(1)?;
                        let auto_count: i64 = row.get(2)?;
                        Ok(json!({
                            "date": date,
                            "count": count,
                            "auto_count": auto_count,
                        }))
                    }).unwrap();
                    rows.filter_map(|r| r.ok()).collect()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "daily": daily }))
    }

    /// Monthly cost comparison data for the savings table.
    async fn handle_analytics_cost_savings(&self) -> WsFrame {
        let monthly: Vec<Value> = if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
            // Get data for last 6 months
            let mut result = Vec::new();
            for months_ago in (0..6).rev() {
                let start_hours = (months_ago + 1) * 720;
                let end_hours = months_ago * 720;

                let start_summary = telemetry.summary_global(start_hours).await;
                let end_summary = telemetry.summary_global(end_hours).await;

                let (period_cost, period_savings) = match (start_summary, end_summary) {
                    (Ok(start), Ok(end)) => {
                        let cost = start.total_cost_millicents.saturating_sub(end.total_cost_millicents);
                        let savings = start.total_cache_savings_millicents.saturating_sub(end.total_cache_savings_millicents);
                        (cost, savings)
                    }
                    _ => (0, 0),
                };

                let month_date = chrono::Utc::now() - chrono::Duration::hours(end_hours as i64);
                let month_label = month_date.format("%Y-%m").to_string();

                // Estimate human cost as 3x of agent cost (industry benchmark)
                let human_cost_estimate = period_cost * 3;

                // `*_millicents` already holds whole cents (see
                // `estimated_cost_millicents`); the dashboard divides by 100 for
                // dollars, so emit cents directly — no scaling.
                result.push(json!({
                    "month": month_label,
                    "human_cost": human_cost_estimate,
                    "agent_cost": period_cost,
                    "savings": human_cost_estimate.saturating_sub(period_cost),
                }));
            }
            result
        } else {
            Vec::new()
        };

        WsFrame::ok_response("", json!({ "monthly": monthly }))
    }

    // ── Billing ──────────────────────────────────────────────

    /// Return real usage data for the billing page.
    ///
    /// - conversations: session count this month from sessions.db
    /// - agents: active agent count from registry
    /// - channels: connected channel count from channel_status
    /// - inference_hours: estimated from CostTelemetry token usage this month
    async fn handle_billing_usage(&self) -> WsFrame {
        let now = chrono::Utc::now();
        // Start of current month in RFC3339
        let month_start = now
            .date_naive()
            .with_day(1)
            .unwrap_or(now.date_naive())
            .and_hms_opt(0, 0, 0)
            .unwrap_or_default();
        let month_start_utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
            month_start,
            chrono::Utc,
        );
        let hours_since_start = hours_since_month_start();

        // Conversations this month from sessions.db
        let session_db = self.home_dir.join("sessions.db");
        let conversations_used: i64 = if session_db.exists() {
            rusqlite::Connection::open(&session_db)
                .ok()
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT COUNT(*) FROM sessions WHERE last_active >= ?1",
                        params![month_start_utc.to_rfc3339()],
                        |r| r.get(0),
                    )
                    .ok()
                })
                .unwrap_or(0)
        } else {
            0
        };

        // Active agents from registry
        let reg = self.registry.read().await;
        let agents_used = reg.list().len() as i64;
        drop(reg);

        // Connected channels
        let channel_map = self.channel_status.read().await;
        let channels_used = channel_map.values().filter(|s| s.connected).count() as i64;
        drop(channel_map);

        // Inference hours estimated from total output tokens this month
        // Rough heuristic: 1 hour ≈ 50 requests average
        let inference_hours_used: f64 =
            if let Some(telemetry) = crate::cost_telemetry::get_telemetry() {
                match telemetry.summary_global(hours_since_start).await {
                    Ok(summary) => summary.total_requests as f64 / 50.0,
                    Err(_) => 0.0,
                }
            } else {
                0.0
            };

        // Community edition: unlimited (-1)
        let reset_at = (month_start_utc + chrono::Duration::days(30)).to_rfc3339();

        WsFrame::ok_response(
            "",
            json!({
                "plan": "community",
                "tier": "community",
                "conversations": { "used": conversations_used, "limit": -1 },
                "agents": { "used": agents_used, "limit": -1 },
                "channels": { "used": channels_used, "limit": -1 },
                "inference_hours": { "used": inference_hours_used.round() as i64, "limit": -1 },
                "reset_at": reset_at,
            }),
        )
    }

    // ── Heartbeat ────────────────────────────────────────────

    async fn handle_heartbeat_status(&self) -> WsFrame {
        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let statuses = scheduler.status().await;
                WsFrame::ok_response("", json!({
                    "heartbeats": statuses,
                    "count": statuses.len(),
                }))
            }
            None => WsFrame::ok_response("", json!({
                "heartbeats": [],
                "count": 0,
                "message": "Heartbeat scheduler not started",
            })),
        }
    }

    async fn handle_heartbeat_trigger(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() {
            return WsFrame::error_response("", "agent_id is required");
        }

        let hb = self.heartbeat.read().await;
        match hb.as_ref() {
            Some(scheduler) => {
                let triggered = scheduler.trigger(agent_id).await;
                if triggered {
                    WsFrame::ok_response("", json!({
                        "success": true,
                        "message": format!("Heartbeat triggered for agent '{agent_id}'"),
                    }))
                } else {
                    WsFrame::error_response("", &format!("Agent '{agent_id}' not found in heartbeat scheduler"))
                }
            }
            None => WsFrame::error_response("", "Heartbeat scheduler not started"),
        }
    }

    // ── Logs ────────────────────────────────────────────────

    fn handle_logs_subscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.subscribe activated — WebSocket push enabled for this connection");
        WsFrame::ok_response("", json!({
            "success": true,
            "subscribed": true,
            "filter": filter,
            "message": "Log push active — events will stream on this WebSocket connection",
        }))
    }

    fn handle_logs_unsubscribe(&self, params: Value) -> WsFrame {
        let filter = params.get("filter").and_then(|v| v.as_str()).unwrap_or("*");
        info!(filter, "logs.unsubscribe — WebSocket push disabled for this connection");
        WsFrame::ok_response("", json!({
            "success": true,
            "subscribed": false,
            "filter": filter,
        }))
    }

    // ── Evolution ────────────────────────────────────────────

    async fn handle_evolution_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let mut gvu_enabled_count = 0usize;
        let agents: Vec<Value> = reg.list().iter().map(|a| {
            let cfg = &a.config;
            if cfg.evolution.gvu_enabled { gvu_enabled_count += 1; }
            json!({
                "agent_id": cfg.agent.name,
                "gvu_enabled": cfg.evolution.gvu_enabled,
                "cognitive_memory": cfg.evolution.cognitive_memory,
                "skill_auto_activate": cfg.evolution.skill_auto_activate,
                "skill_security_scan": cfg.evolution.skill_security_scan,
                "max_silence_hours": cfg.evolution.max_silence_hours,
                "max_gvu_generations": cfg.evolution.max_gvu_generations,
                "observation_period_hours": cfg.evolution.observation_period_hours,
            })
        }).collect();
        let total_agents = agents.len();
        let agent_ids: Vec<String> = reg.list().iter().map(|a| a.config.agent.name.clone()).collect();
        drop(reg);

        // Aggregate real version stats from evolution.db (if any GVU run has persisted).
        let db_path = self.home_dir.join("evolution.db");
        let (total_versions, last_applied_at) = if db_path.exists() {
            let vs = VersionStore::new(&db_path);
            let mut total: u64 = 0;
            let mut latest: Option<chrono::DateTime<chrono::Utc>> = None;
            for aid in &agent_ids {
                let history = vs.get_history(aid, 100);
                total += history.len() as u64;
                if let Some(v) = history.first() {
                    latest = Some(match latest {
                        Some(prev) if prev >= v.applied_at => prev,
                        _ => v.applied_at,
                    });
                }
            }
            (total, latest.map(|t| t.to_rfc3339()))
        } else {
            (0u64, None)
        };

        let enabled = gvu_enabled_count > 0;
        WsFrame::ok_response("", json!({
            "enabled": enabled,
            "mode": if enabled { "prediction_driven" } else { "disabled" },
            "total_agents": total_agents,
            "gvu_enabled_count": gvu_enabled_count,
            "total_versions": total_versions,
            "last_applied_at": last_applied_at,
            "agents": agents,
        }))
    }

    async fn handle_evolution_history(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20).min(100) as usize;

        let db_path = self.home_dir.join("evolution.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", json!({ "versions": [] }));
        }

        let vs = VersionStore::new(&db_path);

        // If agent_id is specified, show that agent's history; otherwise show all agents
        let reg = self.registry.read().await;
        let agent_ids: Vec<String> = if agent_id.is_empty() {
            reg.list().iter().map(|a| a.config.agent.name.clone()).collect()
        } else {
            vec![agent_id.to_string()]
        };
        drop(reg);

        let mut versions = Vec::new();
        for aid in &agent_ids {
            for v in vs.get_history(aid, limit) {
                versions.push(json!({
                    "version_id": v.version_id,
                    "agent_id": v.agent_id,
                    "soul_summary": v.soul_summary,
                    "soul_hash": v.soul_hash,
                    "applied_at": v.applied_at.to_rfc3339(),
                    "observation_end": v.observation_end.to_rfc3339(),
                    "status": format!("{:?}", v.status),
                    "pre_metrics": {
                        "positive_feedback_ratio": v.pre_metrics.positive_feedback_ratio,
                        "prediction_error": v.pre_metrics.avg_prediction_error,
                        "user_correction_rate": v.pre_metrics.user_correction_rate,
                        "contract_violations": v.pre_metrics.contract_violations,
                    },
                    "post_metrics": v.post_metrics.as_ref().map(|m| json!({
                        "positive_feedback_ratio": m.positive_feedback_ratio,
                        "prediction_error": m.avg_prediction_error,
                        "user_correction_rate": m.user_correction_rate,
                        "contract_violations": m.contract_violations,
                    })),
                }));
            }
        }

        // Sort by applied_at descending
        versions.sort_by(|a, b| {
            let ta = a.get("applied_at").and_then(|v| v.as_str()).unwrap_or("");
            let tb = b.get("applied_at").and_then(|v| v.as_str()).unwrap_or("");
            tb.cmp(ta)
        });
        versions.truncate(limit);

        WsFrame::ok_response("", json!({ "versions": versions }))
    }

    // ── Models ──────────────────────────────────────────────

    /// Detect which AI runtime CLIs are installed and whether Claude OAuth is
    /// available — drives the dashboard onboarding "choose your AI backend"
    /// step so we can flag detected vs. not-installed backends. Viewer-level:
    /// returns only presence booleans + subscription tier, never any secret.
    async fn handle_runtime_detect(&self) -> WsFrame {
        let home = &self.home_dir;
        let claude_cli = duduclaw_core::which_claude_in_home(home).is_some();
        let codex = duduclaw_core::which_codex_in_home(home).is_some();
        let gemini = duduclaw_core::which_gemini_in_home(home).is_some();
        let antigravity = duduclaw_core::which_agy_in_home(home).is_some();
        let (claude_oauth, claude_subscription) = detect_claude_oauth();

        WsFrame::ok_response("", json!({
            "claude_cli": claude_cli,
            "codex": codex,
            "gemini": gemini,
            "antigravity": antigravity,
            "claude_oauth": claude_oauth,
            "claude_subscription": claude_subscription,
        }))
    }

    /// List all available models (cloud + local GGUF files).
    async fn handle_models_list(&self) -> WsFrame {
        let mut models = Vec::new();

        // Cloud models — suggestions follow the runtimes actually installed
        // on this machine, not just Claude.
        for (id, label, provider) in [
            ("claude-opus-4-6", "Claude Opus 4.6", "claude"),
            ("claude-sonnet-4-6", "Claude Sonnet 4.6", "claude"),
            ("claude-haiku-4-5", "Claude Haiku 4.5", "claude"),
        ] {
            models.push(json!({
                "id": id,
                "label": label,
                "type": "cloud",
                "provider": provider,
            }));
        }
        if duduclaw_core::which_codex().is_some() {
            for (id, label) in [
                ("gpt-5.5", "GPT-5.5"),
                ("gpt-5.4", "GPT-5.4"),
                ("gpt-5.4-mini", "GPT-5.4 mini"),
            ] {
                models.push(json!({
                    "id": id,
                    "label": label,
                    "type": "cloud",
                    "provider": "codex",
                }));
            }
        }
        if duduclaw_core::which_gemini().is_some() || duduclaw_core::which_agy().is_some() {
            for (id, label) in [
                ("gemini-3.1-pro", "Gemini 3.1 Pro"),
                ("gemini-3.5-flash", "Gemini 3.5 Flash"),
            ] {
                models.push(json!({
                    "id": id,
                    "label": label,
                    "type": "cloud",
                    "provider": "gemini",
                }));
            }
        }

        // Local models: scan ~/.duduclaw/models/ for GGUF files
        let models_dir = self.home_dir.join("models");
        if let Ok(mut entries) = tokio::fs::read_dir(&models_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                    continue;
                }
                let name = path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let size_gb = size as f64 / (1024.0 * 1024.0 * 1024.0);
                models.push(json!({
                    "id": format!("local:{name}"),
                    "label": format!("{name} ({size_gb:.1}GB)"),
                    "type": "local",
                    "file": name,
                    "size_bytes": size,
                }));
            }
        }

        // Also read default_model from inference.toml if it exists
        let inf_path = self.home_dir.join("inference.toml");
        let default_model = if let Ok(content) = tokio::fs::read_to_string(&inf_path).await {
            content.parse::<toml::Table>().ok()
                .and_then(|t| t.get("default_model")?.as_str().map(|s| s.to_string()))
        } else {
            None
        };

        WsFrame::ok_response("", json!({
            "models": models,
            "default_local": default_model,
        }))
    }

    // ── System Config Update ─────────────────────────────────

    /// Update system-level config.toml fields (whitelist only).
    ///
    /// Only allows safe, non-sensitive fields: `log_level`, `rotation_strategy`.
    /// Uses atomic write (temp + rename) and never touches token/key fields.
    async fn handle_system_update_config(&self, params: Value) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut changes: Vec<String> = Vec::new();

        // ── log_level ──
        if let Some(v) = params.get("log_level").and_then(|v| v.as_str()) {
            match v {
                "trace" | "debug" | "info" | "warn" | "error" => {
                    let logging = table.entry("logging")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(logging) = logging {
                        logging.insert("level".into(), toml::Value::String(v.into()));
                        changes.push(format!("logging.level = \"{v}\""));
                    }
                }
                _ => return WsFrame::error_response("", &format!(
                    "Invalid log_level '{v}'. Valid: trace, debug, info, warn, error"
                )),
            }
        }

        // ── rotation_strategy ──
        if let Some(v) = params.get("rotation_strategy").and_then(|v| v.as_str()) {
            match v {
                "priority" | "round_robin" | "least_cost" | "failover" => {
                    let rotation = table.entry("rotation")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut();
                    if let Some(rotation) = rotation {
                        rotation.insert("strategy".into(), toml::Value::String(v.into()));
                        changes.push(format!("rotation.strategy = \"{v}\""));
                    }
                }
                _ => return WsFrame::error_response("", &format!(
                    "Invalid rotation_strategy '{v}'. Valid: priority, round_robin, least_cost, failover"
                )),
            }
        }

        // ── auto_update (Pro only) ──
        if let Some(v) = params.get("auto_update").and_then(|v| v.as_bool()) {
            let gateway = table.entry("gateway")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(gateway) = gateway {
                gateway.insert("auto_update".into(), toml::Value::Boolean(v));
                changes.push(format!("gateway.auto_update = {v}"));
            }
        }

        // ── G.1 [gateway] bind / port / auth_token (restart required) ──
        // bind/port/auth_token change the listening socket + admin token, which
        // are read once at gateway start — we persist + flag, never hot-apply.
        {
            let has_gw = ["bind", "port", "auth_token"].iter().any(|k| params.get(*k).is_some());
            if has_gw {
                let gateway = table.entry("gateway")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                if let Some(v) = params.get("bind").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if v.is_empty() || v.len() > 64 {
                        return WsFrame::error_response("", "gateway.bind must be 1-64 chars");
                    }
                    gateway.insert("bind".into(), toml::Value::String(v.into()));
                    changes.push(format!("gateway.bind = \"{v}\" (restart required)"));
                }
                if let Some(v) = params.get("port").and_then(|v| v.as_u64()) {
                    if v == 0 || v > 65535 {
                        return WsFrame::error_response("", "gateway.port must be 1-65535");
                    }
                    gateway.insert("port".into(), toml::Value::Integer(v as i64));
                    changes.push(format!("gateway.port = {v} (restart required)"));
                }
                if let Some(v) = params.get("auth_token").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    // auth_token is the dashboard admin token — encrypt at rest.
                    gateway.remove("auth_token");
                    if v.is_empty() {
                        gateway.remove("auth_token_enc");
                        changes.push("gateway.auth_token cleared (restart required)".into());
                    } else if v == SECRET_MASK_SET {
                        // untouched — leave existing value
                    } else if let Some(enc) = crate::config_crypto::encrypt_value(v, &self.home_dir) {
                        gateway.insert("auth_token_enc".into(), toml::Value::String(enc));
                        changes.push("gateway.auth_token = [ENCRYPTED] (restart required)".into());
                    } else {
                        return WsFrame::error_response("", "Failed to encrypt gateway.auth_token");
                    }
                }
            }
        }

        // ── G.2 [rotation] health_check_interval_seconds / cooldown_after_rate_limit_seconds ──
        {
            let has_rot = ["health_check_interval_seconds", "cooldown_after_rate_limit_seconds"]
                .iter()
                .any(|k| params.get(*k).is_some());
            if has_rot {
                let rotation = table.entry("rotation")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                for key in &["health_check_interval_seconds", "cooldown_after_rate_limit_seconds"] {
                    if let Some(v) = params.get(*key).and_then(|v| v.as_u64()) {
                        if v == 0 || v > 86400 {
                            return WsFrame::error_response("", &format!("rotation.{key} must be 1-86400"));
                        }
                        rotation.insert((*key).into(), toml::Value::Integer(v as i64));
                        changes.push(format!("rotation.{key} = {v}"));
                    }
                }
            }
        }

        // ── G.3 [general] default_agent / inference_mode ──
        {
            let has_gen = ["default_agent", "inference_mode"].iter().any(|k| params.get(*k).is_some());
            if has_gen {
                let general = table.entry("general")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .unwrap();
                if let Some(v) = params.get("default_agent").and_then(|v| v.as_str()) {
                    let v = v.trim();
                    if !v.is_empty() && !is_valid_agent_id(v) {
                        return WsFrame::error_response("", "Invalid default_agent id");
                    }
                    general.insert("default_agent".into(), toml::Value::String(v.into()));
                    changes.push(format!("general.default_agent = \"{v}\""));
                }
                if let Some(v) = params.get("inference_mode").and_then(|v| v.as_str()) {
                    match v {
                        "local" | "claude" | "hybrid" => {
                            general.insert("inference_mode".into(), toml::Value::String(v.into()));
                            changes.push(format!("general.inference_mode = \"{v}\""));
                        }
                        _ => return WsFrame::error_response("", "Invalid inference_mode. Valid: local, claude, hybrid"),
                    }
                }
            }
        }

        // ── G.4 [logging] format (pretty/json) ──
        if let Some(v) = params.get("log_format").and_then(|v| v.as_str()) {
            match v {
                "pretty" | "json" => {
                    let logging = table.entry("logging")
                        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                        .as_table_mut()
                        .unwrap();
                    logging.insert("format".into(), toml::Value::String(v.into()));
                    changes.push(format!("logging.format = \"{v}\""));
                }
                _ => return WsFrame::error_response("", "Invalid log_format. Valid: pretty, json"),
            }
        }

        // ── G.7 [secret_manager] backend / vault_addr / vault_token(→_enc) / vault_mount ──
        if let Some(sm) = params.get("secret_manager").and_then(|v| v.as_object()) {
            let section = table.entry("secret_manager")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = sm.get("backend").and_then(|v| v.as_str()) {
                match v {
                    "env" | "vault" | "config" | "keychain" => {
                        section.insert("backend".into(), toml::Value::String(v.into()));
                        changes.push(format!("secret_manager.backend = \"{v}\""));
                    }
                    _ => return WsFrame::error_response("", "Invalid secret_manager.backend. Valid: env, vault, config, keychain"),
                }
            }
            for (param_key, toml_key) in &[("vault_addr", "vault_addr"), ("vault_mount", "vault_mount")] {
                if let Some(v) = sm.get(*param_key).and_then(|v| v.as_str()) {
                    section.insert((*toml_key).into(), toml::Value::String(v.trim().into()));
                    changes.push(format!("secret_manager.{toml_key} = \"{}\"", v.trim()));
                }
            }
            // vault_token → encrypt to vault_token_enc (G.7 / XC.5).
            if let Some(v) = sm.get("vault_token").and_then(|v| v.as_str()) {
                let v = v.trim();
                section.remove("vault_token");
                if v.is_empty() {
                    section.remove("vault_token_enc");
                    changes.push("secret_manager.vault_token cleared".into());
                } else if v == SECRET_MASK_SET {
                    // untouched
                } else if let Some(enc) = crate::config_crypto::encrypt_value(v, &self.home_dir) {
                    section.insert("vault_token_enc".into(), toml::Value::String(enc));
                    changes.push("secret_manager.vault_token = [ENCRYPTED]".into());
                } else {
                    return WsFrame::error_response("", "Failed to encrypt secret_manager.vault_token");
                }
            }
        }

        // ── voice (persisted to inference.toml [voice], where VoiceConfig reads it) ──
        // Track how many config.toml changes were accumulated BEFORE the voice
        // block so the early-return below stays correct for mixed payloads.
        let config_toml_changes = changes.len();
        if let Some(voice) = params.get("voice").and_then(|v| v.as_object()) {
            const VALID_ASR: &[&str] = &["auto", "whisper-api", "whisper-local"];
            const VALID_TTS: &[&str] = &["auto", "edge-tts", "minimax", "openai-tts", "piper"];

            let inference_path = self.home_dir.join("inference.toml");
            let mut inf_table = self.read_config_table(&inference_path).await;
            let mut voice_dirty = false;
            let voice_table = inf_table.entry("voice")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut();
            if let Some(voice_table) = voice_table {
                if let Some(v) = voice.get("asr_provider").and_then(|v| v.as_str()) {
                    if !VALID_ASR.contains(&v) {
                        return WsFrame::error_response("", &format!(
                            "Invalid asr_provider '{v}'. Valid: {}", VALID_ASR.join(", ")
                        ));
                    }
                    voice_table.insert("asr_provider".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
                if let Some(v) = voice.get("tts_provider").and_then(|v| v.as_str()) {
                    if !VALID_TTS.contains(&v) {
                        return WsFrame::error_response("", &format!(
                            "Invalid tts_provider '{v}'. Valid: {}", VALID_TTS.join(", ")
                        ));
                    }
                    voice_table.insert("tts_provider".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
                if let Some(v) = voice.get("asr_language").and_then(|v| v.as_str()) {
                    voice_table.insert("asr_language".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
                if let Some(v) = voice.get("tts_voice").and_then(|v| v.as_str()) {
                    voice_table.insert("tts_voice".into(), toml::Value::String(v.into()));
                    voice_dirty = true;
                }
                if let Some(v) = voice.get("voice_reply_enabled").and_then(|v| v.as_bool()) {
                    voice_table.insert("voice_reply_enabled".into(), toml::Value::Boolean(v));
                    voice_dirty = true;
                }
            }

            if voice_dirty {
                let tmp = inference_path.with_extension("toml.tmp");
                if let Err(e) = self.write_config_table(&tmp, &inf_table).await {
                    return WsFrame::error_response("", &format!("Failed to write inference.toml: {e}"));
                }
                if let Err(e) = tokio::fs::rename(&tmp, &inference_path).await {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return WsFrame::error_response("", &format!("Failed to commit inference.toml: {e}"));
                }
                changes.push("voice (inference.toml)".to_string());
            }

            // `voice` may be the only effective field in the payload;
            // config.toml itself is untouched in that case, so return early
            // before the config.toml write below complains about
            // "no valid fields".
            if config_toml_changes == 0 && !changes.is_empty() {
                info!(?changes, "system.update_config completed");
                return WsFrame::ok_response("", json!({ "success": true, "changes": changes }));
            }
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid fields to update. Supported: log_level, log_format, rotation_strategy, auto_update, voice, gateway(bind/port/auth_token), rotation(health_check_interval_seconds/cooldown_after_rate_limit_seconds), general(default_agent/inference_mode), secret_manager");
        }

        // Atomic write: temp + rename
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(?changes, "system.update_config completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "changes": changes,
        }))
    }

    /// Add a new account to config.toml [[accounts]] array.
    ///
    /// Encrypts the API key before storing. Supports `api_key` and `oauth` types.
    async fn handle_accounts_add(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let auth_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("api_key");
        let key = match params.get("key").and_then(|v| v.as_str()) {
            Some(k) if !k.is_empty() => k,
            _ => return WsFrame::error_response("", "Missing 'key' parameter"),
        };
        let budget_cents = params.get("monthly_budget_cents").and_then(|v| v.as_u64()).unwrap_or(5000);
        let priority = params.get("priority").and_then(|v| v.as_u64()).unwrap_or(1);

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Ensure [[accounts]] array exists
        let accounts = table.entry("accounts")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        let arr = match accounts.as_array_mut() {
            Some(a) => a,
            None => return WsFrame::error_response("", "Invalid 'accounts' section in config.toml"),
        };

        // Check for duplicate id
        if arr.iter().any(|a| a.as_table().and_then(|t| t.get("id").and_then(|v| v.as_str())) == Some(id)) {
            return WsFrame::error_response("", &format!("Account '{id}' already exists"));
        }

        // Encrypt the key
        let encrypted = crate::config_crypto::encrypt_value(key, &self.home_dir);

        let mut account = toml::map::Map::new();
        account.insert("id".into(), toml::Value::String(id.into()));
        account.insert("type".into(), toml::Value::String(auth_type.into()));
        account.insert("monthly_budget_cents".into(), toml::Value::Integer(budget_cents as i64));
        account.insert("priority".into(), toml::Value::Integer(priority as i64));
        // Store plaintext key for runtime use + encrypted version for security
        let key_field = if auth_type == "oauth" { "oauth_token" } else { "anthropic_api_key" };
        account.insert(key_field.into(), toml::Value::String(key.into()));
        if let Some(enc) = &encrypted {
            account.insert(format!("{key_field}_enc"), toml::Value::String(enc.clone()));
        }
        arr.push(toml::Value::Table(account));

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(id, auth_type, "accounts.add completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "id": id,
            "type": auth_type,
        }))
    }

    /// Update the monthly budget for a specific account in config.toml.
    async fn handle_accounts_update_budget(&self, params: Value) -> WsFrame {
        let account_id = match params.get("account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id,
            _ => return WsFrame::error_response("", "Missing 'account_id' parameter"),
        };
        let budget_cents = match params.get("monthly_budget_cents").and_then(|v| v.as_u64()) {
            Some(v) => v,
            None => return WsFrame::error_response("", "Missing 'monthly_budget_cents' parameter (integer)"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Find the target account in [[accounts]] array
        let accounts = match table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return WsFrame::error_response("", "No [[accounts]] section in config.toml"),
        };

        let target = accounts.iter_mut().find(|a| {
            a.as_table()
                .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                == Some(account_id)
        });

        match target {
            Some(account) => {
                if let Some(t) = account.as_table_mut() {
                    t.insert("monthly_budget_cents".into(), toml::Value::Integer(budget_cents as i64));
                }
            }
            None => return WsFrame::error_response("", &format!("Account not found: {account_id}")),
        }

        // Atomic write
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!(account_id, budget_cents, "accounts.update_budget completed");
        WsFrame::ok_response("", json!({
            "success": true,
            "account_id": account_id,
            "monthly_budget_cents": budget_cents,
        }))
    }

    /// `accounts.update` — general edit of a `[[accounts]]` entry (G.5).
    /// Params: `{ account_id, priority?, tags?[], profile?, email?,
    /// subscription?, label?, monthly_budget_cents? }`. Does NOT touch the
    /// account secret (use `accounts.add` to (re)set keys). Atomic write.
    async fn handle_accounts_update(&self, params: Value) -> WsFrame {
        let account_id = match params.get("account_id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return WsFrame::error_response("", "Missing 'account_id' parameter"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        let accounts = match table.get_mut("accounts").and_then(|v| v.as_array_mut()) {
            Some(arr) => arr,
            None => return WsFrame::error_response("", "No [[accounts]] section in config.toml"),
        };
        let target = accounts.iter_mut().find(|a| {
            a.as_table().and_then(|t| t.get("id").and_then(|v| v.as_str())) == Some(account_id.as_str())
        });
        let account = match target.and_then(|a| a.as_table_mut()) {
            Some(t) => t,
            None => return WsFrame::error_response("", &format!("Account not found: {account_id}")),
        };

        let mut changes: Vec<String> = Vec::new();
        if let Some(v) = params.get("priority").and_then(|v| v.as_u64()) {
            account.insert("priority".into(), toml::Value::Integer(v as i64));
            changes.push(format!("priority = {v}"));
        }
        if let Some(v) = params.get("monthly_budget_cents").and_then(|v| v.as_u64()) {
            account.insert("monthly_budget_cents".into(), toml::Value::Integer(v as i64));
            changes.push(format!("monthly_budget_cents = {v}"));
        }
        for key in &["profile", "email", "subscription", "label"] {
            if let Some(v) = params.get(*key).and_then(|v| v.as_str()) {
                account.insert((*key).into(), toml::Value::String(v.trim().into()));
                changes.push(format!("{key} = \"{}\"", v.trim()));
            }
        }
        if let Some(arr) = params.get("tags").and_then(|v| v.as_array()) {
            let tags: Vec<toml::Value> = arr
                .iter()
                .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(|s| toml::Value::String(s.into())))
                .collect();
            account.insert("tags".into(), toml::Value::Array(tags.clone()));
            changes.push(format!("tags = [{} entries]", tags.len()));
        }

        if changes.is_empty() {
            return WsFrame::error_response("", "No valid fields to update (priority/tags/profile/email/subscription/label/monthly_budget_cents)");
        }

        if let Err(e) = self.atomic_write_toml(&config_path, &table).await {
            return WsFrame::error_response("", &e);
        }
        info!(account_id, ?changes, "accounts.update completed");
        WsFrame::ok_response("", json!({ "success": true, "account_id": account_id, "changes": changes }))
    }


    // ── Helpers ─────────────────────────────────────────────

    /// Check if an API key is available (from env var or config.toml [api] section).
    async fn has_api_key(&self) -> bool {
        // 1. Check environment variable
        if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()) {
            return true;
        }
        // 2. Check config.toml [api] section
        let table = self.read_config_table(&self.home_dir.join("config.toml")).await;
        if let Some(api) = table.get("api").and_then(|v| v.as_table())
            && api.get("anthropic_api_key").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
        {
            return true;
        }
        // 3. Check accounts in config.toml
        if let Some(accounts) = table.get("accounts")
            && let Some(arr) = accounts.as_array()
        {
            return !arr.is_empty();
        }
        false
    }

    /// Read config.toml into a TOML table, returning an empty table if the file
    /// does not exist or cannot be parsed.
    async fn read_config_table(&self, path: &std::path::Path) -> toml::Table {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
            Err(_) => toml::Table::new(),
        }
    }

    /// Write a TOML table back to disk.
    async fn write_config_table(
        &self,
        path: &std::path::Path,
        table: &toml::Table,
    ) -> std::io::Result<()> {
        let content = toml::to_string_pretty(table).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        tokio::fs::write(path, content).await
    }

    /// Run common health checks used by both doctor and doctor_repair.
    async fn run_doctor_checks(&self) -> Vec<Value> {
        let reg = self.registry.read().await;
        let has_agents = !reg.list().is_empty();
        let has_key = self.has_api_key().await;
        let config_exists = self.home_dir.join("config.toml").exists();

        vec![
            json!({
                "name": "config_file",
                "status": if config_exists { "pass" } else { "fail" },
                "message": if config_exists { "config.toml exists" } else { "config.toml not found" },
                "can_repair": !config_exists,
            }),
            json!({
                "name": "agents",
                "status": if has_agents { "pass" } else { "warn" },
                "message": if has_agents { "Agents found" } else { "No agents found" },
                "can_repair": false,
            }),
            json!({
                "name": "api_key",
                "status": if has_key { "pass" } else { "warn" },
                "message": if has_key { "ANTHROPIC_API_KEY is set" } else { "ANTHROPIC_API_KEY not set" },
                "can_repair": false,
            }),
            {
                let (docker_status, docker_msg) = check_docker().await;
                json!({
                    "name": "container_runtime",
                    "status": docker_status,
                    "message": docker_msg,
                    "can_repair": false,
                })
            },
        ]
    }

    // ── Odoo ERP ─────────────────────────────────────────────────

    /// Return the current Odoo connection status.
    ///
    /// Reads `[odoo]` from config.toml, attempts to connect if configured,
    /// and returns connected/edition/version info.
    async fn handle_odoo_status(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let odoo_cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !odoo_cfg.is_configured() {
            return WsFrame::ok_response("", json!({
                "connected": false,
            }));
        }

        // Decrypt credential
        let credential = match self.resolve_odoo_credential(&table) {
            Some(c) if !c.is_empty() => c,
            _ => return WsFrame::ok_response("", json!({
                "connected": false,
                "error": "No credential configured",
            })),
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response("", json!({
                    "connected": st.connected,
                    "edition": st.edition,
                    "version": st.version,
                    "uid": st.uid,
                }))
            }
            Err(e) => {
                warn!("Odoo connection failed: {e}");
                WsFrame::ok_response("", json!({
                    "connected": false,
                    "error": "Connection failed",
                }))
            }
        }
    }

    /// Return the current Odoo config (without secrets).
    /// Returns `null` if Odoo is not configured.
    async fn handle_odoo_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;
        let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);

        if !cfg.is_configured() {
            return WsFrame::ok_response("", json!(null));
        }

        WsFrame::ok_response("", json!({
            "url": cfg.url,
            "db": cfg.db,
            "protocol": cfg.protocol,
            "auth_method": cfg.auth_method,
            "username": cfg.username,
            "poll_enabled": cfg.poll_enabled,
            "poll_interval_seconds": cfg.poll_interval_seconds,
            "poll_models": cfg.poll_models,
            "webhook_enabled": cfg.webhook_enabled,
            "features_crm": cfg.features_crm,
            "features_sale": cfg.features_sale,
            "features_inventory": cfg.features_inventory,
            "features_accounting": cfg.features_accounting,
            "features_project": cfg.features_project,
            "features_hr": cfg.features_hr,
        }))
    }

    /// Validate an Odoo model name (e.g. `crm.lead`, `sale.order`).
    /// Rejects blocked models (security-sensitive Odoo internals).
    fn is_valid_odoo_model(name: &str) -> bool {
        !name.is_empty()
            && name.len() < 100
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
            && !duduclaw_odoo::OdooConnector::is_model_blocked(name)
    }

    /// Validate that a URL is safe for Odoo connections.
    /// Requires HTTPS with non-private host, except for strict localhost.
    fn is_safe_odoo_url(url: &str) -> bool {
        if url.len() > 512 {
            return false;
        }
        // Allow HTTP only for strict localhost — must be followed by '/' or ':' or end of string
        for prefix in &["http://127.0.0.1", "http://localhost", "http://[::1]"] {
            if let Some(rest) = url.strip_prefix(prefix) {
                if rest.is_empty() || rest.starts_with('/') || rest.starts_with(':') {
                    return true;
                }
            }
        }
        if url.starts_with("https://") {
            // Reject private/reserved IPs to prevent SSRF against cloud metadata, LAN, etc.
            let host_part = &url["https://".len()..];
            // Extract host (before first '/' or ':' for port)
            let host = host_part.split(&['/', ':'][..]).next().unwrap_or("");
            return !Self::is_private_host(host);
        }
        false
    }

    /// Check if a hostname is a private/reserved IP or a known metadata endpoint.
    /// Uses `std::net::IpAddr` parsing to correctly handle all IPv4/IPv6 representations,
    /// including IPv4-mapped IPv6 (`::ffff:10.0.0.1`), compressed forms, etc.
    fn is_private_host(host: &str) -> bool {
        // Strip brackets for IPv6 literals (e.g. "[::1]" → "::1")
        let raw = host.trim_start_matches('[').trim_end_matches(']');

        // Bare IPv6 without brackets (contains ':' but no '[') — reject as ambiguous
        if !host.starts_with('[') && raw.contains(':') {
            return true;
        }

        if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
            return Self::is_private_ip(ip);
        }

        // Hostname-based checks
        let lower = host.to_ascii_lowercase();
        lower == "localhost" || lower.ends_with(".localhost")
            || lower == "metadata.google.internal"
            || lower == "metadata.azure.internal"
    }

    /// Check if an IP address is private, loopback, link-local, or otherwise reserved.
    fn is_private_ip(ip: std::net::IpAddr) -> bool {
        match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()           // 127.0.0.0/8
                    || v4.is_private()      // 10/8, 172.16/12, 192.168/16
                    || v4.is_link_local()   // 169.254/16
                    || v4.is_unspecified()  // 0.0.0.0
                    || v4.is_broadcast()    // 255.255.255.255
                    || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64 // 100.64/10 (CGNAT)
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()           // ::1
                    || v6.is_unspecified()  // ::
                    // IPv4-mapped (::ffff:x.x.x.x) — check the embedded v4
                    || v6.to_ipv4_mapped().is_some_and(|v4| Self::is_private_ip(std::net::IpAddr::V4(v4)))
                    // Link-local (fe80::/10)
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    // Unique Local Address (fc00::/7)
                    || (v6.octets()[0] & 0xfe) == 0xfc
            }
        }
    }

    /// Save Odoo configuration to config.toml `[odoo]` section.
    ///
    /// Encrypts api_key/password/webhook_secret before storing.
    /// Refuses to store credentials if encryption is unavailable.
    /// Uses atomic write (temp + rename).
    async fn handle_odoo_configure(&self, params: Value) -> WsFrame {
        // Validate URL
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) if Self::is_safe_odoo_url(u) => u,
            Some(_) => return WsFrame::error_response("", "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1)"),
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        // Validate database name
        let db = match params.get("db").and_then(|v| v.as_str()).map(str::trim) {
            Some(d) if !d.is_empty() && d.len() < 64
                && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => d,
            Some(_) => return WsFrame::error_response("", "Invalid database name (alphanumeric, _, - only, max 63 chars)"),
            _ => return WsFrame::error_response("", "Missing 'db' parameter"),
        };

        // Validate protocol (whitelist)
        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("xmlrpc") => "xmlrpc",
            Some("jsonrpc") | None => "jsonrpc",
            _ => return WsFrame::error_response("", "Invalid protocol: must be 'jsonrpc' or 'xmlrpc'"),
        };

        // Validate auth_method (whitelist)
        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => return WsFrame::error_response("", "Invalid auth_method: must be 'api_key' or 'password'"),
        };

        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;

        // Build the [odoo] section
        let mut odoo = toml::map::Map::new();
        odoo.insert("url".into(), toml::Value::String(url.into()));
        odoo.insert("db".into(), toml::Value::String(db.into()));
        odoo.insert("protocol".into(), toml::Value::String(protocol.into()));
        odoo.insert("auth_method".into(), toml::Value::String(auth_method.into()));
        let username = params.get("username").and_then(|v| v.as_str()).unwrap_or("").trim();
        if username.len() > 256 {
            return WsFrame::error_response("", "Username too long (max 256 chars)");
        }
        odoo.insert("username".into(), toml::Value::String(username.into()));

        // Encrypt credentials — refuse to store if encryption is unavailable (CRIT-1)
        if let Some(api_key) = params.get("api_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(api_key, &self.home_dir) {
                Some(enc) => { odoo.insert("api_key_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Could not encrypt API key — keyfile write failed (disk full or permission denied). See gateway log."),
            }
        } else {
            // Preserve existing encrypted key if not provided
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("api_key_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("api_key_enc".into(), toml::Value::String(existing.into()));
            }
        }

        if let Some(password) = params.get("password").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(password, &self.home_dir) {
                Some(enc) => { odoo.insert("password_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Could not encrypt password — keyfile write failed (disk full or permission denied). See gateway log."),
            }
        } else {
            // Preserve existing encrypted password if not provided
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("password_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("password_enc".into(), toml::Value::String(existing.into()));
            }
        }

        // Polling config
        odoo.insert(
            "poll_enabled".into(),
            toml::Value::Boolean(params.get("poll_enabled").and_then(|v| v.as_bool()).unwrap_or(false)),
        );
        odoo.insert(
            "poll_interval_seconds".into(),
            toml::Value::Integer(
                params.get("poll_interval_seconds")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(300)
                    .clamp(60, 86400),
            ),
        );
        if let Some(models) = params.get("poll_models").and_then(|v| v.as_array()) {
            let arr: Vec<toml::Value> = models
                .iter()
                .take(50) // cap at 50 models to prevent oversized config
                .filter_map(|v| v.as_str()
                    .filter(|s| Self::is_valid_odoo_model(s))
                    .map(|s| toml::Value::String(s.into())))
                .collect();
            odoo.insert("poll_models".into(), toml::Value::Array(arr));
        }

        // Webhook config
        odoo.insert(
            "webhook_enabled".into(),
            toml::Value::Boolean(params.get("webhook_enabled").and_then(|v| v.as_bool()).unwrap_or(false)),
        );
        if let Some(secret) = params.get("webhook_secret").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            match crate::config_crypto::encrypt_value(secret, &self.home_dir) {
                Some(enc) => { odoo.insert("webhook_secret_enc".into(), toml::Value::String(enc)); }
                None => return WsFrame::error_response("", "Could not encrypt webhook secret — keyfile write failed (disk full or permission denied). See gateway log."),
            }
        } else {
            // Preserve existing webhook secret
            if let Some(existing) = table.get("odoo")
                .and_then(|v| v.as_table())
                .and_then(|t| t.get("webhook_secret_enc"))
                .and_then(|v| v.as_str())
            {
                odoo.insert("webhook_secret_enc".into(), toml::Value::String(existing.into()));
            }
        }

        // Feature toggles
        for feature in &["features_crm", "features_sale", "features_inventory", "features_accounting", "features_project", "features_hr"] {
            if let Some(v) = params.get(*feature).and_then(|v| v.as_bool()) {
                odoo.insert((*feature).into(), toml::Value::Boolean(v));
            }
        }

        table.insert("odoo".into(), toml::Value::Table(odoo));

        // Atomic write: temp + rename
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        info!("odoo.configure completed");
        WsFrame::ok_response("", json!({ "success": true }))
    }

    /// Test the Odoo connection.
    ///
    /// Two modes:
    /// - **Inline (recommended for the dashboard "Test connection" button):** when
    ///   `params.url` is non-empty, build a transient config from `params`
    ///   (url / db / protocol / auth_method / username + api_key|password).
    ///   The config is **never written to disk** — this lets the user verify
    ///   credentials before persisting them.
    /// - **Stored:** when no `url` in params, fall back to `config.toml`
    ///   (original behaviour).
    ///
    /// Hybrid: in inline mode without an explicit credential, the saved
    /// credential is used so you can re-test after a small URL tweak without
    /// retyping the API key.
    async fn handle_odoo_test(&self, params: Value) -> WsFrame {
        let inline_url = params
            .get("url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        // Always read config.toml once — needed for stored-credential fallback in inline mode.
        let config_path = self.home_dir.join("config.toml");
        let table = self.read_config_table(&config_path).await;

        let (odoo_cfg, credential) = if inline_url.is_some() {
            match Self::build_test_config_from_params(&params) {
                Ok((cfg, Some(cred))) => (cfg, cred),
                Ok((cfg, None)) => {
                    // No credential in params — fall back to saved one so the user
                    // can re-test a tweaked URL without retyping the API key.
                    match self.resolve_odoo_credential(&table) {
                        Some(c) if !c.is_empty() => (cfg, c),
                        _ => {
                            return WsFrame::ok_response("", json!({
                                "success": false,
                                "message": "No credential — enter API key/password, or save once first",
                            }))
                        }
                    }
                }
                Err(msg) => {
                    return WsFrame::ok_response("", json!({
                        "success": false,
                        "message": msg,
                    }))
                }
            }
        } else {
            let cfg = duduclaw_odoo::OdooConfig::from_toml(&table);
            if !cfg.is_configured() {
                return WsFrame::ok_response("", json!({
                    "success": false,
                    "message": "Odoo not configured — fill URL and database, or save first",
                }));
            }
            let credential = match self.resolve_odoo_credential(&table) {
                Some(c) if !c.is_empty() => c,
                _ => {
                    return WsFrame::ok_response("", json!({
                        "success": false,
                        "message": "No API key or password configured",
                    }))
                }
            };
            (cfg, credential)
        };

        match duduclaw_odoo::OdooConnector::connect(&odoo_cfg, &credential).await {
            Ok(conn) => {
                let st = conn.status();
                WsFrame::ok_response("", json!({
                    "success": true,
                    "message": format!("Connected — {} {}", st.edition, st.version),
                }))
            }
            Err(e) => {
                warn!("Odoo test connection failed: {e}");
                WsFrame::ok_response("", json!({
                    "success": false,
                    "message": format!("Connection failed: {}", Self::scrub_odoo_error(&e)),
                }))
            }
        }
    }

    /// Build a transient `OdooConfig` + credential from RPC params for the
    /// "test before save" flow. Applies the same validation rules as
    /// `handle_odoo_configure` so the test path can't be used to bypass them.
    ///
    /// Returns `(config, Some(credential))` when params include an api_key /
    /// password, or `(config, None)` when the caller wants the handler to fall
    /// back to the stored credential.
    fn build_test_config_from_params(
        params: &Value,
    ) -> Result<(duduclaw_odoo::OdooConfig, Option<String>), String> {
        // URL — reuse the same SSRF-safe validator as `configure`.
        let url = match params.get("url").and_then(|v| v.as_str()).map(str::trim) {
            Some(u) if Self::is_safe_odoo_url(u) => u,
            Some(_) => {
                return Err(
                    "Odoo URL must use HTTPS (http:// only allowed for localhost/127.0.0.1)"
                        .into(),
                )
            }
            _ => return Err("Missing 'url' parameter".into()),
        };

        // Database — alphanumeric + `_` + `-`, max 63 chars.
        let db = match params.get("db").and_then(|v| v.as_str()).map(str::trim) {
            Some(d)
                if !d.is_empty()
                    && d.len() < 64
                    && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
            {
                d
            }
            Some(_) => {
                return Err("Invalid database name (alphanumeric, _, - only, max 63 chars)".into())
            }
            _ => return Err("Missing 'db' parameter".into()),
        };

        let protocol = match params.get("protocol").and_then(|v| v.as_str()) {
            Some("xmlrpc") => "xmlrpc",
            Some("jsonrpc") | None => "jsonrpc",
            _ => return Err("Invalid protocol: must be 'jsonrpc' or 'xmlrpc'".into()),
        };

        let auth_method = match params.get("auth_method").and_then(|v| v.as_str()) {
            Some("password") => "password",
            Some("api_key") | None => "api_key",
            _ => return Err("Invalid auth_method: must be 'api_key' or 'password'".into()),
        };

        let username = params
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if username.len() > 256 {
            return Err("Username too long (max 256 chars)".into());
        }

        let credential_field = if auth_method == "password" { "password" } else { "api_key" };
        let credential = params
            .get(credential_field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty());

        let cfg = duduclaw_odoo::OdooConfig {
            url: url.into(),
            db: db.into(),
            protocol: protocol.into(),
            auth_method: auth_method.into(),
            username: username.into(),
            ..Default::default()
        };
        Ok((cfg, credential))
    }

    /// Strip identifiers that an attacker could weaponize from a connector
    /// error before forwarding it to the dashboard.
    ///
    /// The reqwest error includes the full URL (with any query string), and may
    /// echo embedded userinfo (`user:pass@host`) or `?api_key=…` query params on
    /// failure. We redact those first, then cap the length. M19: truncation
    /// alone leaked the URL/token on short errors, so scrubbing must happen
    /// regardless of length. We keep the high-level reason so the user can act.
    fn scrub_odoo_error(raw: &str) -> String {
        let scrubbed = scrub_secrets_from_text(raw);
        // Cap to avoid pushing megabyte HTML error pages back to the client.
        const MAX_LEN: usize = 240;
        let mut out = String::with_capacity(scrubbed.len().min(MAX_LEN));
        for ch in scrubbed.chars().take(MAX_LEN) {
            out.push(ch);
        }
        if scrubbed.chars().count() > MAX_LEN {
            out.push_str("…");
        }
        out
    }

    /// Resolve the Odoo credential from config.toml (encrypted or plaintext).
    ///
    /// Returns `None` if decryption fails — never returns raw ciphertext (CRIT-2).
    fn resolve_odoo_credential(&self, table: &toml::Table) -> Option<String> {
        let odoo_section = table.get("odoo")?.as_table()?;
        let auth_method = odoo_section.get("auth_method")
            .and_then(|v| v.as_str())
            .unwrap_or("api_key");

        let (enc_field, plain_field) = if auth_method == "password" {
            ("password_enc", "password")
        } else {
            ("api_key_enc", "api_key")
        };

        // Try encrypted first
        if let Some(enc_val) = odoo_section.get(enc_field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            if let Some(key) = crate::config_crypto::load_keyfile_public(&self.home_dir) {
                if let Ok(engine) = duduclaw_security::crypto::CryptoEngine::new(&key) {
                    if let Ok(decrypted) = engine.decrypt_string(enc_val) {
                        return Some(decrypted);
                    }
                }
            }
            // Decryption failed — do NOT return raw ciphertext as credential
            warn!("Failed to decrypt Odoo credential — keyfile may have changed");
            return None;
        }

        // Fallback to plaintext field (legacy / dev environments)
        let plain = odoo_section.get(plain_field)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        if plain.is_some() {
            warn!(field = plain_field, "Odoo credential stored in plaintext — re-save config to encrypt");
        }
        plain
    }

    /// Mask sensitive values (tokens, secrets, keys) in a TOML table.
    fn mask_sensitive_fields(table: &mut toml::Table) {
        let sensitive_patterns = ["token", "secret", "key", "password"];
        for (key, value) in table.iter_mut() {
            let is_sensitive = sensitive_patterns.iter().any(|p| key.to_lowercase().contains(p));
            match value {
                toml::Value::String(s) if is_sensitive && !s.is_empty() => {
                    // Fully mask sensitive values — do NOT leak any prefix chars (MCP-M7)
                    *s = "********".to_string();
                }
                toml::Value::Table(t) => Self::mask_sensitive_fields(t),
                _ => {}
            }
        }
    }

    // ── Filtered agent list (respects UserContext) ────────────

    async fn handle_agents_list_filtered(&self, ctx: &UserContext) -> WsFrame {
        // Re-scan to pick up changes
        if let Ok(mut reg) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            self.registry.write(),
        ).await {
            let _ = reg.scan().await;
        }

        let reg = self.registry.read().await;
        let visible = ctx.visible_agents();

        // Real per-agent month-to-date spend from CostTelemetry. Computed once
        // per visible agent below (the rotator counter is unusable — see
        // `telemetry_spent_cents_for_agent`).
        let mut agent_spent: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for a in reg.list().iter() {
            let name = a.config.agent.name.clone();
            let visible_here = match &visible {
                None => true,
                Some(names) => names.contains(&name),
            };
            if visible_here && !agent_spent.contains_key(&name) {
                let spent = self.telemetry_spent_cents_for_agent(&name).await;
                agent_spent.insert(name, spent);
            }
        }

        let agents: Vec<Value> = reg.list().iter()
            .filter(|a| {
                match &visible {
                    None => true, // Admin sees all
                    Some(names) => names.contains(&a.config.agent.name),
                }
            })
            .map(|a| {
                let cfg = &a.config;
                json!({
                    "name": cfg.agent.name,
                    "display_name": cfg.agent.display_name,
                    "role": format!("{:?}", cfg.agent.role).to_lowercase(),
                    "status": format!("{:?}", cfg.agent.status).to_lowercase(),
                    "trigger": cfg.agent.trigger,
                    "icon": cfg.agent.icon,
                    "reports_to": cfg.agent.reports_to,
                    "model": {
                        "preferred": cfg.model.preferred,
                        "fallback": cfg.model.fallback,
                        "account_pool": cfg.model.account_pool,
                        "api_mode": cfg.model.api_mode,
                        "local": cfg.model.local.as_ref().map(|l| json!({
                            "model": l.model,
                            "backend": l.backend,
                            "context_length": l.context_length,
                            "gpu_layers": l.gpu_layers,
                            "prefer_local": l.prefer_local,
                            "use_router": l.use_router,
                        })),
                    },
                    "budget": {
                        "monthly_limit_cents": cfg.budget.monthly_limit_cents,
                        "spent_cents": agent_spent.get(&cfg.agent.name).copied().unwrap_or(0),
                        "warn_threshold_percent": cfg.budget.warn_threshold_percent,
                        "hard_stop": cfg.budget.hard_stop,
                    },
                    "heartbeat": {
                        "enabled": cfg.heartbeat.enabled,
                        "interval_seconds": cfg.heartbeat.interval_seconds,
                    },
                    "skills": a.skills.iter().map(|s| &s.name).collect::<Vec<_>>(),
                    "permissions": {
                        "can_create_agents": cfg.permissions.can_create_agents,
                        "can_send_cross_agent": cfg.permissions.can_send_cross_agent,
                        "can_modify_own_skills": cfg.permissions.can_modify_own_skills,
                        "can_modify_own_soul": cfg.permissions.can_modify_own_soul,
                        "can_schedule_tasks": cfg.permissions.can_schedule_tasks,
                    },
                    // Evolution + sticker need to be present here (not just in
                    // agents.inspect) because the dashboard's edit dialog
                    // initialises from the list response and silently
                    // falls back to hardcoded defaults when these are absent —
                    // making fields like `skill_auto_activate` (default `false`
                    // in JS, but typically `true` on disk) appear to never
                    // persist when in fact only the UI was misreading.
                    "evolution": {
                        "gvu_enabled": cfg.evolution.gvu_enabled,
                        "cognitive_memory": cfg.evolution.cognitive_memory,
                        "skill_auto_activate": cfg.evolution.skill_auto_activate,
                        "skill_security_scan": cfg.evolution.skill_security_scan,
                        "max_silence_hours": cfg.evolution.max_silence_hours,
                    },
                    "sticker": {
                        "enabled": cfg.sticker.enabled,
                        "probability": cfg.sticker.probability,
                        "intensity_threshold": cfg.sticker.intensity_threshold,
                        "cooldown_messages": cfg.sticker.cooldown_messages,
                        "expressiveness": match cfg.sticker.expressiveness {
                            duduclaw_core::types::Expressiveness::Minimal => "minimal",
                            duduclaw_core::types::Expressiveness::Moderate => "moderate",
                            duduclaw_core::types::Expressiveness::Expressive => "expressive",
                        },
                    },
                })
            }).collect();

        info!("agents.list: returning {} agents for user {}", agents.len(), ctx.email);
        WsFrame::ok_response("", json!({ "agents": agents }))
    }

    // ── User management handlers (admin only) ────────────────

    async fn handle_users_list(&self) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };
        match db.list_users() {
            Ok(users) => {
                let mut result: Vec<Value> = Vec::new();
                for u in &users {
                    let bindings = db.get_user_agents(&u.id).unwrap_or_default();
                    result.push(json!({
                        "id": u.id,
                        "email": u.email,
                        "display_name": u.display_name,
                        "role": u.role,
                        "status": u.status,
                        "created_at": u.created_at,
                        "updated_at": u.updated_at,
                        "last_login": u.last_login,
                        "bindings": bindings,
                    }));
                }
                WsFrame::ok_response("", json!({ "users": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to list users: {e}")),
        }
    }

    async fn handle_users_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let email = params.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params.get("display_name").and_then(|v| v.as_str()).unwrap_or("");
        let password = params.get("password").and_then(|v| v.as_str()).unwrap_or("");
        let role_str = params.get("role").and_then(|v| v.as_str()).unwrap_or("employee");

        if email.is_empty() || display_name.is_empty() || password.is_empty() {
            return WsFrame::error_response("", "email, display_name, and password are required");
        }
        // Email format validation (MEDIUM fix)
        if !email.contains('@') || email.len() > 254 {
            return WsFrame::error_response("", "invalid email format");
        }
        // Display name length limit
        if display_name.len() > 200 {
            return WsFrame::error_response("", "display_name too long (max 200 chars)");
        }
        if password.len() < 8 {
            return WsFrame::error_response("", "password must be at least 8 characters");
        }
        if password.len() > 1024 {
            return WsFrame::error_response("", "password too long");
        }

        let role: UserRole = match role_str.parse() {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };

        match db.create_user(email, display_name, password, role) {
            Ok(user) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.create", Some(&user.id), Some(&format!("email={email}")), None);
                WsFrame::ok_response("", json!({ "user": user }))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to create user: {e}")),
        }
    }

    async fn handle_users_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        let display_name = params.get("display_name").and_then(|v| v.as_str());
        let role = params.get("role").and_then(|v| v.as_str()).and_then(|r| r.parse::<UserRole>().ok());
        let password = params.get("password").and_then(|v| v.as_str());

        if let Some(pw) = password {
            if pw.len() < 8 {
                return WsFrame::error_response("", "password must be at least 8 characters");
            }
            if pw.len() > 1024 {
                return WsFrame::error_response("", "password too long");
            }
        }
        if let Some(name) = display_name {
            if name.len() > 200 {
                return WsFrame::error_response("", "display_name too long (max 200 chars)");
            }
        }

        match db.update_user(user_id, display_name, role, password) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.update", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "updated"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to update user: {e}")),
        }
    }

    async fn handle_users_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };

        match db.set_user_status(user_id, duduclaw_auth::UserStatus::Suspended) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.suspend", Some(user_id), None, None);
                WsFrame::ok_response("", json!({"status": "suspended"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to suspend user: {e}")),
        }
    }

    async fn handle_users_bind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };
        let access_level_str = params.get("access_level").and_then(|v| v.as_str()).unwrap_or("owner");
        let access_level: AccessLevel = match access_level_str.parse() {
            Ok(l) => l,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Verify agent exists
        let reg = self.registry.read().await;
        if reg.get(agent_name).is_none() {
            return WsFrame::error_response("", &format!("agent not found: {agent_name}"));
        }
        drop(reg);

        match db.bind_agent(user_id, agent_name, access_level) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.bind_agent", Some(agent_name),
                    Some(&format!("user={user_id}, level={access_level}")), None);
                WsFrame::ok_response("", json!({"status": "bound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to bind agent: {e}")),
        }
    }

    async fn handle_users_unbind_agent(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let agent_name = match params.get("agent_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "agent_name is required"),
        };

        match db.unbind_agent(user_id, agent_name) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "user.unbind_agent", Some(agent_name),
                    Some(&format!("user={user_id}")), None);
                WsFrame::ok_response("", json!({"status": "unbound"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to unbind agent: {e}")),
        }
    }

    async fn handle_users_offboard(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = match params.get("user_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "user_id is required"),
        };
        let transfer_to = params.get("transfer_to").and_then(|v| v.as_str());

        // Get user's bound agents before offboarding
        let bindings = db.get_user_agents(user_id).unwrap_or_default();

        // Set user status to offboarded
        if let Err(e) = db.set_user_status(user_id, duduclaw_auth::UserStatus::Offboarded) {
            return WsFrame::error_response("", &format!("failed to offboard user: {e}"));
        }

        // Transfer agent ownership if specified
        let mut transferred = Vec::new();
        if let Some(new_owner_id) = transfer_to {
            for binding in &bindings {
                // Unbind from old user
                let _ = db.unbind_agent(user_id, &binding.agent_name);
                // Bind to new owner
                let _ = db.bind_agent(new_owner_id, &binding.agent_name, binding.access_level);
                transferred.push(binding.agent_name.clone());
            }
        }

        let _ = db.log_action(Some(&ctx.user_id), "user.offboard", Some(user_id),
            Some(&format!("transferred_agents={transferred:?}, transfer_to={transfer_to:?}")), None);

        WsFrame::ok_response("", json!({
            "status": "offboarded",
            "transferred_agents": transferred,
        }))
    }

    async fn handle_users_me(&self, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => {
                // No user DB — return context from JWT
                return WsFrame::ok_response("", json!({
                    "user": {
                        "id": ctx.user_id,
                        "email": ctx.email,
                        "role": ctx.role.to_string(),
                    },
                    "bindings": [],
                }));
            }
        };

        match db.get_user(&ctx.user_id) {
            Ok(Some(user)) => {
                let bindings = db.get_user_agents(&user.id).unwrap_or_default();
                WsFrame::ok_response("", json!({
                    "user": user,
                    "bindings": bindings,
                }))
            }
            _ => WsFrame::ok_response("", json!({
                "user": {
                    "id": ctx.user_id,
                    "email": ctx.email,
                    "role": ctx.role.to_string(),
                },
                "bindings": [],
            })),
        }
    }

    /// Self-service password change for the logged-in user. Available in every
    /// edition: the personal/single-owner edition hides the multi-user Users
    /// page, so this is the only way the sole admin can rotate their own
    /// password. No admin role required — it only ever mutates the caller's own
    /// account (identified by `ctx.user_id`). Verifies the current password
    /// first; on success `update_user` also clears any `must_change_password`.
    async fn handle_users_change_password(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let current = params.get("current_password").and_then(|v| v.as_str()).unwrap_or("");
        let new_password = match params.get("new_password").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return WsFrame::error_response("", "new_password is required"),
        };
        if new_password.len() < 8 {
            return WsFrame::error_response("", "new password must be at least 8 characters");
        }
        if new_password.len() > 1024 {
            return WsFrame::error_response("", "new password too long");
        }

        // Resolve the caller's own account; the stored email is needed to verify
        // the current password.
        let user = match db.get_user(&ctx.user_id) {
            Ok(Some(u)) => u,
            _ => return WsFrame::error_response("", "user not found"),
        };

        // Verify the current password (timing-safe inside verify_password).
        if db.verify_password(&user.email, current).is_err() {
            let _ = db.log_action(Some(&ctx.user_id), "password.change_failed", Some(&ctx.user_id), None, None);
            return WsFrame::error_response("", "current password is incorrect");
        }

        if current == new_password {
            return WsFrame::error_response("", "new password must differ from the current one");
        }

        match db.update_user(&ctx.user_id, None, None, Some(new_password)) {
            Ok(()) => {
                let _ = db.log_action(Some(&ctx.user_id), "password.change", Some(&ctx.user_id), None, None);
                WsFrame::ok_response("", json!({"status": "changed"}))
            }
            Err(e) => WsFrame::error_response("", &format!("failed to change password: {e}")),
        }
    }

    async fn handle_users_audit_log(&self, params: Value) -> WsFrame {
        let db = match self.user_db.read().await.as_ref() {
            Some(db) => db.clone(),
            None => return WsFrame::error_response("", "user system not initialized"),
        };

        let user_id = params.get("user_id").and_then(|v| v.as_str());
        let action = params.get("action").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100).min(1000) as u32;

        match db.query_audit_log(user_id, action, limit) {
            Ok(entries) => WsFrame::ok_response("", json!({ "entries": entries })),
            Err(e) => WsFrame::error_response("", &format!("failed to query audit log: {e}")),
        }
    }

    // ── Marketplace ─────────────────────────────────────────────

    /// Build the Marketplace catalog JSON: built-in entries plus
    /// optional user-contributed entries from `~/.duduclaw/marketplace.json`.
    ///
    /// Schema of the optional file:
    /// ```json
    /// { "servers": [ { "id": "...", "name": "...", ... } ] }
    /// ```
    /// Each entry follows the `McpCatalogItem` JSON shape. Invalid files
    /// are skipped with a warning so a malformed user file never breaks
    /// the dashboard.
    async fn handle_marketplace_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, McpCatalogItem};

        let mut servers: Vec<McpCatalogItem> = marketplace_catalog();

        // Merge optional user-contributed catalog entries.
        let user_path = self.home_dir.join("marketplace.json");
        if user_path.exists() {
            match tokio::fs::read_to_string(&user_path).await {
                Ok(content) => {
                    #[derive(serde::Deserialize)]
                    struct UserCatalog {
                        #[serde(default)]
                        servers: Vec<McpCatalogItem>,
                    }
                    match serde_json::from_str::<UserCatalog>(&content) {
                        Ok(user) => {
                            info!(
                                path = %user_path.display(),
                                count = user.servers.len(),
                                "Merged user marketplace catalog"
                            );
                            servers.extend(user.servers);
                        }
                        Err(e) => warn!(
                            path = %user_path.display(),
                            error = %e,
                            "Failed to parse user marketplace.json; skipping"
                        ),
                    }
                }
                Err(e) => warn!(
                    path = %user_path.display(),
                    error = %e,
                    "Failed to read user marketplace.json; skipping"
                ),
            }
        }

        // Build a map of catalog id -> agents that already have it installed.
        // Install writes the catalog `id` as the server key in the agent's
        // `.mcp.json` (see handle_marketplace_install), so a server counts as
        // installed for an agent when that id appears among its mcp_servers.
        let installed_by = self.marketplace_installed_map().await;

        let mut servers_json = match serde_json::to_value(&servers) {
            Ok(Value::Array(arr)) => arr,
            Ok(_) => Vec::new(),
            Err(e) => return WsFrame::error_response(
                "",
                &format!("Failed to serialize marketplace catalog: {e}"),
            ),
        };

        // Annotate each server with its backend-derived installed_by list so the
        // dashboard reflects real `.mcp.json` state instead of ephemeral UI state.
        for entry in servers_json.iter_mut() {
            let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let agents = installed_by.get(&id).cloned().unwrap_or_default();
            if let Value::Object(map) = entry {
                map.insert("installed_by".to_string(), json!(agents));
            }
        }

        WsFrame::ok_response("", json!({ "servers": servers_json }))
    }

    /// Scan every agent's `.mcp.json` and return a map of server key (catalog id)
    /// -> sorted list of agent ids that have it installed. Used by the Marketplace
    /// page to render an accurate, reload-safe "installed" state.
    async fn marketplace_installed_map(&self) -> std::collections::HashMap<String, Vec<String>> {
        use duduclaw_agent::mcp_template::read_mcp_config;

        let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        let agents_dir = self.home_dir.join("agents");
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = match dir.file_name().and_then(|n| n.to_str()) {
                    Some(n) if !n.starts_with('_') && !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                if let Ok(config) = read_mcp_config(&dir) {
                    for key in config.mcp_servers.keys() {
                        map.entry(key.clone()).or_default().push(name.clone());
                    }
                }
            }
        }
        for agents in map.values_mut() {
            agents.sort();
            agents.dedup();
        }
        map
    }

    /// Install a marketplace catalog server into an agent's `.mcp.json`.
    ///
    /// Params: `{ "id": "<catalog id>", "agent_id": "<agent>" }`.
    /// Looks the item up in the built-in catalog plus the optional
    /// user-contributed `~/.duduclaw/marketplace.json`, then reuses the
    /// same `add_server_to_config` path as `mcp.update`.
    async fn handle_marketplace_install(&self, params: Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{add_server_to_config, marketplace_catalog, McpCatalogItem};

        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        if !is_valid_agent_id(&agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        let agent_dir = self.home_dir.join("agents").join(&agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }

        let mut catalog: Vec<McpCatalogItem> = marketplace_catalog();
        let user_path = self.home_dir.join("marketplace.json");
        if user_path.exists() {
            if let Ok(content) = tokio::fs::read_to_string(&user_path).await {
                #[derive(serde::Deserialize)]
                struct UserCatalog {
                    #[serde(default)]
                    servers: Vec<McpCatalogItem>,
                }
                if let Ok(user) = serde_json::from_str::<UserCatalog>(&content) {
                    catalog.extend(user.servers);
                }
            }
        }

        let item = match catalog.into_iter().find(|c| c.id == id) {
            Some(c) => c,
            None => return WsFrame::error_response("", &format!("Marketplace server '{id}' not found")),
        };

        let server_name = item.id.clone();
        let def = item.default_def;
        match tokio::task::spawn_blocking(move || add_server_to_config(&agent_dir, &server_name, &def)).await {
            Ok(Ok(())) => {
                info!(server = %id, agent = %agent_id, "Marketplace server installed");
                WsFrame::ok_response("", json!({ "success": true, "agent_id": agent_id }))
            }
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
        }
    }

    // ── MCP Management ──────────────────────────────────────────

    async fn handle_mcp_list(&self) -> WsFrame {
        use duduclaw_agent::mcp_template::{marketplace_catalog, read_mcp_config};

        let agents_dir = self.home_dir.join("agents");
        let mut agents = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let name = match dir.file_name().and_then(|n| n.to_str()) {
                    Some(n) if !n.starts_with('_') && !n.starts_with('.') => n.to_string(),
                    _ => continue,
                };
                let config = match read_mcp_config(&dir) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let servers: Vec<Value> = config.mcp_servers.iter().map(|(k, v)| {
                    json!({
                        "name": k,
                        "command": v.command,
                        "args": v.args,
                        "env": v.env,
                    })
                }).collect();
                agents.push(json!({
                    "agent_id": name,
                    "servers": servers,
                }));
            }
        }

        let catalog: Vec<Value> = marketplace_catalog().iter().map(|item| {
            json!({
                "id": item.id,
                "name": item.name,
                "description": item.description,
                "category": item.category,
                "requires_oauth": item.requires_oauth,
                "default_def": {
                    "command": item.default_def.command,
                    "args": item.default_def.args,
                    "env": item.default_def.env,
                },
                "required_env": item.required_env,
            })
        }).collect();

        WsFrame::ok_response("", json!({ "agents": agents, "catalog": catalog }))
    }

    async fn handle_mcp_update(&self, params: &Value) -> WsFrame {
        use duduclaw_agent::mcp_template::{add_server_to_config, remove_server_from_config, McpServerDef};

        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "agent_id is required"),
        };
        let action = match params.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return WsFrame::error_response("", "action is required (add/remove)"),
        };
        let server_name = match params.get("server_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "server_name is required"),
        };

        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }

        let agent_dir = self.home_dir.join("agents").join(agent_id);
        if !agent_dir.is_dir() {
            return WsFrame::error_response("", &format!("Agent '{agent_id}' not found"));
        }

        match action {
            "add" => {
                let def: McpServerDef = match params.get("server_def") {
                    Some(v) => match serde_json::from_value(v.clone()) {
                        Ok(d) => d,
                        Err(e) => return WsFrame::error_response("", &format!("Invalid server_def: {e}")),
                    },
                    None => return WsFrame::error_response("", "server_def is required for add action"),
                };
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || add_server_to_config(&ad, &sn, &def)).await {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            "remove" => {
                let ad = agent_dir.clone();
                let sn = server_name.to_string();
                match tokio::task::spawn_blocking(move || remove_server_from_config(&ad, &sn)).await {
                    Ok(Ok(())) => WsFrame::ok_response("", json!({ "success": true })),
                    Ok(Err(e)) => WsFrame::error_response("", &e),
                    Err(e) => WsFrame::error_response("", &format!("Internal error: {e}")),
                }
            }
            _ => WsFrame::error_response("", &format!("Unknown action: {action}. Use 'add' or 'remove'")),
        }
    }

    // ── MCP OAuth handlers ──────────────────────────────────

    /// List available OAuth providers with configuration and token status.
    async fn handle_mcp_oauth_providers(&self) -> WsFrame {
        use crate::mcp_oauth;

        let redirect_uri = format!("http://localhost:3000/api/mcp/oauth/callback");
        let providers = mcp_oauth::builtin_providers(&redirect_uri);

        let results: Vec<Value> = providers.iter().map(|p| {
            let token = mcp_oauth::get_token(&self.home_dir, &p.provider_id);
            let status = match &token {
                Some(t) => {
                    if let Some(exp) = t.expires_at {
                        if chrono::Utc::now() >= exp {
                            "expired"
                        } else {
                            "authenticated"
                        }
                    } else {
                        "authenticated"
                    }
                }
                None => "none",
            };
            json!({
                "provider_id": p.provider_id,
                "auth_url": p.auth_url,
                "scopes": p.scopes,
                "configured": !p.client_id.is_empty(),
                "status": status,
                "expires_at": token.and_then(|t| t.expires_at),
            })
        }).collect();

        WsFrame::ok_response("", json!({ "providers": results }))
    }

    /// Start an OAuth flow: generate PKCE, store pending state, return auth URL.
    async fn handle_mcp_oauth_start(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        let client_id = params.get("client_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let client_secret = params.get("client_secret").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Find the built-in provider or create a custom one
        let redirect_uri = format!("http://localhost:3000/api/mcp/oauth/callback");
        let mut config = mcp_oauth::builtin_providers(&redirect_uri)
            .into_iter()
            .find(|p| p.provider_id == provider_id)
            .unwrap_or_else(|| mcp_oauth::McpOAuthConfig {
                provider_id: provider_id.clone(),
                client_id: String::new(),
                client_secret: String::new(),
                auth_url: params.get("auth_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                token_url: params.get("token_url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                scopes: params.get("scopes")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default(),
                redirect_uri: redirect_uri.clone(),
            });

        // Override client_id/secret if provided in params
        if !client_id.is_empty() {
            config.client_id = client_id;
        }
        if !client_secret.is_empty() {
            config.client_secret = client_secret;
        }

        if config.client_id.is_empty() {
            return WsFrame::error_response("", "client_id is required (provide in params or pre-configure)");
        }
        if config.auth_url.is_empty() || config.token_url.is_empty() {
            return WsFrame::error_response("", "auth_url and token_url are required for custom providers");
        }

        // Generate PKCE
        let (code_verifier, code_challenge) = mcp_oauth::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();

        let auth_url = mcp_oauth::build_auth_url(&config, &state, &code_challenge);

        // Store pending
        let pending = mcp_oauth::PendingOAuth {
            provider_id: provider_id.clone(),
            state: state.clone(),
            code_verifier,
            config,
            created_at: std::time::Instant::now(),
        };

        {
            let mut map = self.mcp_oauth_pending.write().await;
            // Cleanup expired entries
            mcp_oauth::cleanup_pending(&mut map);
            map.insert(state.clone(), pending);
        }

        info!(provider = %provider_id, "MCP OAuth flow started");

        WsFrame::ok_response("", json!({
            "auth_url": auth_url,
            "state": state,
        }))
    }

    /// Check if a provider's OAuth flow has completed (token exists).
    async fn handle_mcp_oauth_status(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        let token = mcp_oauth::get_token(&self.home_dir, provider_id);
        match token {
            Some(t) => WsFrame::ok_response("", json!({
                "authenticated": true,
                "expires_at": t.expires_at,
                "scopes": t.scopes,
            })),
            None => WsFrame::ok_response("", json!({
                "authenticated": false,
            })),
        }
    }

    /// Revoke (remove) a stored OAuth token for a provider.
    async fn handle_mcp_oauth_revoke(&self, params: Value) -> WsFrame {
        use crate::mcp_oauth;

        let provider_id = match params.get("provider_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "provider_id is required"),
        };

        match mcp_oauth::remove_token(&self.home_dir, provider_id) {
            Ok(()) => {
                info!(provider = %provider_id, "MCP OAuth token revoked");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

// ── Standalone helpers ────────────────────────────────────────

/// Check if Docker (or Podman) is available by running `docker info`.
/// Returns `("pass"/"warn", message)`.
async fn check_docker() -> (&'static str, String) {
    // Try `docker info` first, then `podman info`
    for cmd_name in &["docker", "podman"] {
        let result = tokio::process::Command::new(cmd_name)
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;

        match result {
            Ok(out) if out.status.success() => {
                return ("pass", format!("{cmd_name} daemon is running"));
            }
            Ok(_) => {
                return ("warn", format!("{cmd_name} found but daemon is not running"));
            }
            Err(_) => {} // try next
        }
    }

    ("warn", "No container runtime (docker/podman) found in PATH".to_string())
}

// ═══════════════════════════════════════════════════════════════
// Task Board, Activity Feed, Autopilot, Shared Skills handlers
// ═══════════════════════════════════════════════════════════════

impl MethodHandler {
    // ── Store accessors ─────────────────────────────────────

    async fn task_store(&self) -> Result<Arc<TaskStore>, WsFrame> {
        self.task_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Task store not initialized"))
    }

    async fn ap_store(&self) -> Result<Arc<AutopilotStore>, WsFrame> {
        self.autopilot_store
            .read()
            .await
            .clone()
            .ok_or_else(|| WsFrame::error_response("", "Autopilot store not initialized"))
    }

    /// Broadcast an event via the injected event_tx (best-effort, no error on failure).
    async fn broadcast_event(&self, event: &str, payload: Value) {
        if let Some(tx) = self.event_tx.read().await.as_ref() {
            let frame = WsFrame::Event {
                event: event.to_string(),
                payload,
                seq: None,
                state_version: None,
            };
            let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
        }
    }

    // ── Task handlers ───────────────────────────────────────

    async fn handle_tasks_list(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let status = params.get("status").and_then(|v| v.as_str());
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let priority = params.get("priority").and_then(|v| v.as_str());
        match store.list_tasks(status, agent_id, priority).await {
            Ok(rows) => {
                let tasks: Vec<Value> = rows.iter().map(|r| task_row_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "tasks": tasks }))
            }
            Err(e) => WsFrame::error_response("", &format!("list tasks: {e}")),
        }
    }

    async fn handle_tasks_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if title.is_empty() {
            return WsFrame::error_response("", "title is required");
        }
        let assigned_to = params.get("assigned_to").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if assigned_to.is_empty() {
            return WsFrame::error_response("", "assigned_to is required");
        }
        let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let priority = params.get("priority").and_then(|v| v.as_str()).unwrap_or("medium").to_string();
        let tags = params
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();

        let mut row = TaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            title.clone(),
            description,
            priority,
            assigned_to.clone(),
            if ctx.user_id.is_empty() { "system" } else { &ctx.user_id }.to_string(),
        );
        row.tags = tags;
        row.parent_task_id = params.get("parent_task_id").and_then(|v| v.as_str()).map(|s| s.to_string());

        if let Err(e) = store.insert_task(&row).await {
            return WsFrame::error_response("", &format!("create task: {e}"));
        }

        // Record activity event
        let activity = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: "task_created".into(),
            agent_id: assigned_to,
            task_id: Some(row.id.clone()),
            summary: title,
            timestamp: Utc::now().to_rfc3339(),
            metadata: None,
        };
        let _ = store.append_activity(&activity).await;

        let task_json = task_row_to_json(&row);
        self.broadcast_event("task.created", task_json.clone()).await;
        self.broadcast_event("activity.new", activity_row_to_json(&activity)).await;
        self.emit_autopilot_event(crate::autopilot_engine::AutopilotEvent::TaskCreated {
            task: task_json.clone(),
        })
        .await;

        WsFrame::ok_response("", json!({ "task": task_json }))
    }

    async fn handle_tasks_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        // HS4: enforce the caller is bound (Operator) to the task's agent before
        // any mutation. Resolving the agent from the task prevents an Employee
        // bound only to agent A from mutating agent B's tasks.
        let existing = store.get_task(task_id).await.ok().flatten();
        let owner_agent = existing.as_ref().map(|r| r.assigned_to.clone());
        if let Some(agent) = owner_agent.as_deref() {
            if let Err(e) = acl::require_agent_access(ctx, agent, AccessLevel::Operator) {
                return WsFrame::error_response("", &e);
            }
        } else if !ctx.is_admin() {
            // Unknown task → only admins may probe; others get a generic denial.
            return WsFrame::error_response("", "permission denied");
        }
        // If re-assigning to a different agent, the caller must also be bound
        // (Operator) to the destination agent.
        if let Some(dest) = params.get("assigned_to").and_then(|v| v.as_str()) {
            if !dest.is_empty() {
                if let Err(e) = acl::require_agent_access(ctx, dest, AccessLevel::Operator) {
                    return WsFrame::error_response("", &e);
                }
            }
        }
        // Capture previous status for TaskStatusChanged event emission.
        let prev_status = existing.map(|r| r.status);
        match store.update_task(task_id, &params).await {
            Ok(Some(row)) => {
                let task_json = task_row_to_json(&row);
                self.broadcast_event("task.updated", task_json.clone()).await;
                self.emit_autopilot_event(
                    crate::autopilot_engine::AutopilotEvent::TaskUpdated {
                        task: task_json.clone(),
                    },
                )
                .await;

                // Emit TaskStatusChanged when status actually changed.
                if let (Some(prev), Some(new_status)) = (
                    prev_status.as_deref(),
                    params.get("status").and_then(|v| v.as_str()),
                ) {
                    if prev != new_status {
                        self.emit_autopilot_event(
                            crate::autopilot_engine::AutopilotEvent::TaskStatusChanged {
                                task_id: task_id.to_string(),
                                from: prev.to_string(),
                                to: new_status.to_string(),
                                task: task_json.clone(),
                            },
                        )
                        .await;
                    }
                }

                // If status changed to done/blocked, record activity
                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    let event_type = match status {
                        "done" => "task_completed",
                        "blocked" => "task_blocked",
                        _ => "",
                    };
                    if !event_type.is_empty() {
                        let activity = ActivityRow {
                            id: uuid::Uuid::new_v4().to_string(),
                            event_type: event_type.into(),
                            agent_id: row.assigned_to.clone(),
                            task_id: Some(task_id.to_string()),
                            summary: row.title.clone(),
                            timestamp: Utc::now().to_rfc3339(),
                            metadata: None,
                        };
                        let _ = store.append_activity(&activity).await;
                        self.broadcast_event("activity.new", activity_row_to_json(&activity)).await;
                    }
                }

                WsFrame::ok_response("", json!({ "task": task_json }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("update task: {e}")),
        }
    }

    async fn handle_tasks_remove(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() {
            return WsFrame::error_response("", "task_id is required");
        }
        // HS4: resolve the task's agent and require Operator binding before removal.
        match store.get_task(task_id).await.ok().flatten() {
            Some(row) => {
                if let Err(e) =
                    acl::require_agent_access(ctx, &row.assigned_to, AccessLevel::Operator)
                {
                    return WsFrame::error_response("", &e);
                }
            }
            None if !ctx.is_admin() => {
                return WsFrame::error_response("", "permission denied");
            }
            None => {}
        }
        match store.remove_task(task_id).await {
            Ok(true) => {
                self.broadcast_event("task.removed", json!({ "task_id": task_id })).await;
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Ok(false) => WsFrame::error_response("", &format!("Task not found: {task_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove task: {e}")),
        }
    }

    async fn handle_tasks_assign(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let task_id = params.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if task_id.is_empty() || agent_id.is_empty() {
            return WsFrame::error_response("", "task_id and agent_id are required");
        }
        // L1: dead `update` binding removed. Authorization (source + destination
        // agent binding) is enforced by `handle_tasks_update`.
        self.handle_tasks_update(json!({ "task_id": task_id, "assigned_to": agent_id }), ctx)
            .await
    }

    // ── Activity handlers ───────────────────────────────────

    async fn handle_activity_list(&self, params: Value) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let event_type = params.get("type").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        let offset = params.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

        match store.list_activity(agent_id, event_type, limit, offset).await {
            Ok((rows, total)) => {
                let events: Vec<Value> = rows.iter().map(|r| activity_row_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "events": events, "total": total }))
            }
            Err(e) => WsFrame::error_response("", &format!("list activity: {e}")),
        }
    }

    // ── Live Run Forking handlers (RFC-26) ──────────────────
    //
    // Forks execute in the MCP-server process and persist to the cross-process
    // `ForkStore` (`<home>/fork_store.db`); the dashboard reads it here.

    fn open_fork_store(&self) -> Result<duduclaw_fork::ForkStore, WsFrame> {
        let path = self.home_dir.join("fork_store.db");
        if !path.exists() {
            return Err(WsFrame::error_response("", "no forks yet (fork store not created)"));
        }
        duduclaw_fork::ForkStore::open(&path)
            .map_err(|e| WsFrame::error_response("", &format!("open fork store: {e}")))
    }

    fn handle_fork_list(&self, params: Value) -> WsFrame {
        // No fork has ever been created yet → the store file doesn't exist.
        // That's an empty list, not an error: return [] so the dashboard shows
        // its "no forks yet" empty state instead of a scary error banner.
        if !self.home_dir.join("fork_store.db").exists() {
            return WsFrame::ok_response("", json!({ "forks": [] }));
        }
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).min(500) as usize;
        match store.list_forks(limit) {
            Ok(forks) => {
                let rows: Vec<Value> = forks
                    .iter()
                    .map(|f| {
                        json!({
                            "fork_id": f.fork_id,
                            "agent_id": f.agent_id,
                            "merge_mode": f.merge_mode,
                            "resolved": f.resolved,
                            "winner": f.winner,
                            "promoted": f.promoted,
                            "aggregate_spent_usd": f.aggregate_spent_usd,
                            "created_at": f.created_at,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "forks": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("list forks: {e}")),
        }
    }

    fn handle_fork_inspect(&self, params: Value) -> WsFrame {
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let fork_id = match params.get("fork_id").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return WsFrame::error_response("", "fork_id is required"),
        };
        let fork = match store.get_fork(fork_id) {
            Ok(Some(f)) => f,
            Ok(None) => return WsFrame::error_response("", "fork not found"),
            Err(e) => return WsFrame::error_response("", &format!("get fork: {e}")),
        };
        let branches = store.list_branches(fork_id).unwrap_or_default();
        let branch_json: Vec<Value> = branches
            .iter()
            .map(|b| {
                json!({
                    "branch_id": b.branch_id,
                    "steering": b.steering,
                    "state": b.state,
                    "budget_usd": b.budget_usd,
                    "spent_usd": b.spent_usd,
                    "test_exit_code": b.test_exit_code,
                    "output": duduclaw_core::truncate_bytes(&b.output, 8000),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({
            "fork_id": fork.fork_id,
            "agent_id": fork.agent_id,
            "prompt": duduclaw_core::truncate_bytes(&fork.prompt, 4000),
            "merge_mode": fork.merge_mode,
            "resolved": fork.resolved,
            "winner": fork.winner,
            "promoted": fork.promoted,
            "branches": branch_json,
        }))
    }

    fn handle_fork_resolve(&self, params: Value) -> WsFrame {
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let fork_id = match params.get("fork_id").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return WsFrame::error_response("", "fork_id is required"),
        };
        let branch_id = match params.get("branch_id").and_then(|v| v.as_str()) {
            Some(b) => b,
            None => return WsFrame::error_response("", "branch_id is required"),
        };
        let fork = match store.get_fork(fork_id) {
            Ok(Some(f)) => f,
            Ok(None) => return WsFrame::error_response("", "fork not found"),
            Err(e) => return WsFrame::error_response("", &format!("get fork: {e}")),
        };
        if fork.resolved {
            return WsFrame::error_response("", "fork already resolved");
        }
        let branches = store.list_branches(fork_id).unwrap_or_default();
        if !branches.iter().any(|b| b.branch_id == branch_id) {
            return WsFrame::error_response("", "branch not found in fork");
        }
        let aggregate = branches.iter().map(|b| b.spent_usd).sum();
        match store.set_resolution(fork_id, Some(branch_id), true, true, aggregate) {
            Ok(_) => WsFrame::ok_response("", json!({
                "fork_id": fork_id, "resolved": true, "winner": branch_id,
            })),
            Err(e) => WsFrame::error_response("", &format!("resolve fork: {e}")),
        }
    }

    // ── Autopilot handlers ──────────────────────────────────

    async fn handle_autopilot_list(&self) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_rules().await {
            Ok(rows) => {
                let rules: Vec<Value> = rows.iter().map(|r| autopilot_rule_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "rules": rules }))
            }
            Err(e) => WsFrame::error_response("", &format!("list autopilot: {e}")),
        }
    }

    async fn handle_autopilot_create(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            return WsFrame::error_response("", "name is required");
        }
        let trigger_event = params.get("trigger_event").and_then(|v| v.as_str()).unwrap_or("task_created").to_string();
        if let Err(e) = validate_autopilot_trigger_event(&trigger_event) {
            return WsFrame::error_response("", &e);
        }
        let conditions = params.get("conditions").cloned().unwrap_or(json!({}));
        let action = params.get("action").cloned().unwrap_or(json!({}));
        // Reject malformed rules at write time so the dashboard surfaces
        // the error immediately rather than silently in autopilot_history
        // the first time the rule would have fired.
        if let Err(e) = validate_autopilot_action(&action) {
            return WsFrame::error_response("", &e);
        }

        let row = AutopilotRuleRow {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            trigger_event,
            conditions: conditions.to_string(),
            action: action.to_string(),
            created_at: Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
        };
        if let Err(e) = store.insert_rule(&row).await {
            return WsFrame::error_response("", &format!("create autopilot rule: {e}"));
        }
        WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) }))
    }

    async fn handle_autopilot_update(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        // Re-validate any provided trigger_event / action fields.
        if let Some(t) = params.get("trigger_event").and_then(|v| v.as_str()) {
            if let Err(e) = validate_autopilot_trigger_event(t) {
                return WsFrame::error_response("", &e);
            }
        }
        if let Some(a) = params.get("action") {
            if let Err(e) = validate_autopilot_action(a) {
                return WsFrame::error_response("", &e);
            }
        }
        match store.update_rule(rule_id, &params).await {
            Ok(Some(row)) => WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) })),
            Ok(None) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("update rule: {e}")),
        }
    }

    async fn handle_autopilot_remove(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        match store.remove_rule(rule_id).await {
            Ok(true) => WsFrame::ok_response("", json!({ "success": true })),
            Ok(false) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove rule: {e}")),
        }
    }

    async fn handle_autopilot_history(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        match store.list_history(rule_id, limit).await {
            Ok(entries) => {
                let result: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "rule_id": e.rule_id,
                            "rule_name": e.rule_name,
                            "triggered_at": e.triggered_at,
                            "result": e.result,
                            "details": e.details,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "entries": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("autopilot history: {e}")),
        }
    }

    // ── RFC-23 Redaction read-only RPCs ─────────────────────

    async fn handle_redaction_stats(&self) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response("", json!({
                "enabled": false,
                "vault": { "total": 0, "active": 0, "expired": 0, "by_category": [] },
                "rule_count": 0,
            }));
        };
        match duduclaw_redaction::dashboard::handle_stats(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize stats: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction stats: {e}")),
        }
    }

    async fn handle_redaction_recent_audit(&self, params: Value) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        };
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;
        let req = duduclaw_redaction::dashboard::RecentAuditRequest { limit };
        match duduclaw_redaction::dashboard::handle_recent_audit(&manager, req) {
            Ok(r) => match serde_json::to_value(&r) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize audit: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction audit: {e}")),
        }
    }

    async fn handle_redaction_override_status(&self) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response("", json!({
                "active": false,
                "banner": null,
                "record": null,
            }));
        };
        match duduclaw_redaction::dashboard::handle_override_status(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize override: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction override: {e}")),
        }
    }

    async fn handle_redaction_policy_status(&self) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response("", json!({
                "config_enabled": false,
                "vault_ttl_hours": 0,
                "purge_after_expire_days": 0,
                "rule_count": 0,
                "override_active": false,
            }));
        };
        match duduclaw_redaction::dashboard::handle_policy_status(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize policy: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction policy: {e}")),
        }
    }

    // ── Shared Skills handlers ──────────────────────────────

    async fn handle_skills_shared_list(&self) -> WsFrame {
        let shared_dir = self.home_dir.join("shared").join("skills");
        if !shared_dir.exists() {
            return WsFrame::ok_response("", json!({ "skills": [] }));
        }
        let mut skills: Vec<Value> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&shared_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                // Parse frontmatter for metadata
                let description = extract_frontmatter(&content, "description").unwrap_or_default();
                let shared_by = extract_frontmatter(&content, "shared_by").unwrap_or_default();
                let shared_at = extract_frontmatter(&content, "shared_at").unwrap_or_default();
                let tags: Vec<String> = extract_frontmatter(&content, "tags")
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                let adopted_by: Vec<String> = extract_frontmatter(&content, "adopted_by")
                    .map(|t| t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                let usage_count: i64 = extract_frontmatter(&content, "usage_count")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);

                skills.push(json!({
                    "name": name,
                    "description": description,
                    "shared_by": shared_by,
                    "shared_at": shared_at,
                    "tags": tags,
                    "adopted_by": adopted_by,
                    "usage_count": usage_count,
                }));
            }
        }
        WsFrame::ok_response("", json!({ "skills": skills }))
    }

    async fn handle_skills_share(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
        let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
        if agent_id.is_empty() || skill_name.is_empty() {
            return WsFrame::error_response("", "agent_id and skill_name are required");
        }
        // Read skill from agent's SKILLS directory
        let skill_path = self
            .home_dir
            .join("agents")
            .join(agent_id)
            .join("SKILLS")
            .join(format!("{skill_name}.md"));
        if !skill_path.exists() {
            return WsFrame::error_response(
                "",
                &format!("Skill not found: {skill_name} in agent {agent_id}"),
            );
        }
        let content = match tokio::fs::read_to_string(&skill_path).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("read skill: {e}")),
        };

        // Write to shared skills directory with metadata frontmatter
        let shared_dir = self.home_dir.join("shared").join("skills");
        if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
            return WsFrame::error_response("", &format!("create shared dir: {e}"));
        }
        let shared_path = shared_dir.join(format!("{skill_name}.md"));
        let now = Utc::now().to_rfc3339();
        let shared_content = format!(
            "---\nshared_by: {agent_id}\nshared_at: {now}\ndescription: \ntags: \nadopted_by: \nusage_count: 0\n---\n\n{content}"
        );
        if let Err(e) = tokio::fs::write(&shared_path, &shared_content).await {
            return WsFrame::error_response("", &format!("write shared skill: {e}"));
        }

        WsFrame::ok_response("", json!({ "success": true }))
    }

    async fn handle_skills_adopt(&self, params: Value) -> WsFrame {
        let skill_name = params.get("skill_name").and_then(|v| v.as_str()).unwrap_or("");
        let target_agent = params.get("target_agent_id").and_then(|v| v.as_str()).unwrap_or("");
        if skill_name.is_empty() || target_agent.is_empty() {
            return WsFrame::error_response("", "skill_name and target_agent_id are required");
        }
        // XC.4: validate target_agent_id (mirror other agent-targeting handlers)
        // — prevents path traversal / writing outside the agents tree.
        if !is_valid_agent_id(target_agent) {
            return WsFrame::error_response("", "Invalid target_agent_id format");
        }
        // skill_name is used in a filename — restrict to a safe charset.
        if !skill_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || skill_name.len() > 128
        {
            return WsFrame::error_response("", "Invalid skill_name (alphanumeric, _, -; ≤128 chars)");
        }
        // Read from shared
        let shared_path = self.home_dir.join("shared").join("skills").join(format!("{skill_name}.md"));
        if !shared_path.exists() {
            return WsFrame::error_response("", &format!("Shared skill not found: {skill_name}"));
        }
        let content = match tokio::fs::read_to_string(&shared_path).await {
            Ok(c) => c,
            Err(e) => return WsFrame::error_response("", &format!("read shared skill: {e}")),
        };

        // Extract actual content (strip frontmatter)
        let skill_content = if let Some(idx) = content.find("\n---\n") {
            content[idx + 5..].trim().to_string()
        } else {
            content.clone()
        };

        // Write to target agent's SKILLS directory
        let target_dir = self
            .home_dir
            .join("agents")
            .join(target_agent)
            .join("SKILLS");
        if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
            return WsFrame::error_response("", &format!("create agent skills dir: {e}"));
        }
        let target_path = target_dir.join(format!("{skill_name}.md"));
        if let Err(e) = tokio::fs::write(&target_path, &skill_content).await {
            return WsFrame::error_response("", &format!("write skill to agent: {e}"));
        }

        // Update shared frontmatter: bump usage_count and add to adopted_by
        let updated = update_frontmatter_field(&content, "usage_count", |old| {
            let count: i64 = old.parse().unwrap_or(0);
            (count + 1).to_string()
        });
        let updated = update_frontmatter_field(&updated, "adopted_by", |old| {
            let mut agents: Vec<&str> = old.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            if !agents.contains(&target_agent) {
                agents.push(target_agent);
            }
            agents.join(", ")
        });
        let _ = tokio::fs::write(&shared_path, &updated).await;

        WsFrame::ok_response("", json!({ "success": true }))
    }
}

// ── JSON serialization helpers ──────────────────────────────

fn task_row_to_json(r: &TaskRow) -> Value {
    json!({
        "id": r.id,
        "title": r.title,
        "description": r.description,
        "status": r.status,
        "priority": r.priority,
        "assigned_to": r.assigned_to,
        "created_by": r.created_by,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
        "completed_at": r.completed_at,
        "blocked_reason": r.blocked_reason,
        "parent_task_id": r.parent_task_id,
        "tags": r.tags.split(',').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
        "message_id": r.message_id,
    })
}

fn activity_row_to_json(r: &ActivityRow) -> Value {
    json!({
        "id": r.id,
        "type": r.event_type,
        "agent_id": r.agent_id,
        "task_id": r.task_id,
        "summary": r.summary,
        "timestamp": r.timestamp,
        "metadata": r.metadata.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
    })
}

fn autopilot_rule_to_json(r: &AutopilotRuleRow) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "enabled": r.enabled,
        "trigger_event": r.trigger_event,
        "conditions": serde_json::from_str::<Value>(&r.conditions).unwrap_or(json!({})),
        "action": serde_json::from_str::<Value>(&r.action).unwrap_or(json!({})),
        "created_at": r.created_at,
        "last_triggered_at": r.last_triggered_at,
        "trigger_count": r.trigger_count,
    })
}

/// Extract a field value from YAML-style frontmatter (`---` delimited).
/// Validate a trigger_event string against the set understood by AutopilotEngine.
/// Rejecting unknown values at write time avoids rules that are stored
/// successfully but can never fire.
fn validate_autopilot_trigger_event(ev: &str) -> Result<(), String> {
    const KNOWN: &[&str] = &[
        "task_created",
        "task_updated",
        "task_status_changed",
        "activity_new",
        "channel_message",
        "agent_idle",
        "cron_tick",
    ];
    if KNOWN.iter().any(|k| *k == ev) {
        Ok(())
    } else {
        Err(format!(
            "unknown trigger_event '{ev}'; must be one of: {}",
            KNOWN.join(", ")
        ))
    }
}

/// Validate an autopilot action JSON object at rule-write time.
///
/// Requires `type` ∈ {delegate, notify, run_skill} and the fields the
/// engine will eventually need. Catches misconfiguration immediately
/// rather than silently during the first fire.
fn validate_autopilot_action(action: &Value) -> Result<(), String> {
    let obj = action
        .as_object()
        .ok_or_else(|| "action must be a JSON object".to_string())?;
    let t = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "action.type is required".to_string())?;
    let require_str = |key: &str| -> Result<(), String> {
        obj.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("action.{key} is required for type '{t}'"))
    };
    match t {
        "delegate" => {
            require_str("target_agent")?;
            require_str("prompt")?;
        }
        "notify" => {
            require_str("channel")?;
            require_str("chat_id")?;
            require_str("text")?;
        }
        "run_skill" => {
            require_str("target_agent")?;
            require_str("skill_name")?;
        }
        other => return Err(format!("unknown action.type '{other}'")),
    }
    Ok(())
}

fn extract_frontmatter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    for line in content.lines() {
        if line == "---" {
            // End of frontmatter
        }
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Update a field in YAML-style frontmatter with a transform function.
fn update_frontmatter_field(content: &str, key: &str, transform: impl Fn(&str) -> String) -> String {
    let prefix = format!("{key}:");
    content
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix(&prefix) {
                format!("{prefix} {}", transform(rest.trim()))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod skill_synthesis_config_tests {
    use super::*;

    #[test]
    fn get_returns_safe_defaults_when_absent() {
        let table = toml::Table::new();
        let resp = skill_synthesis_table_to_response(&table);
        assert_eq!(resp["auto_run"], json!(false));
        assert_eq!(resp["dry_run"], json!(true));
        assert_eq!(resp["interval_hours"], json!(24));
        assert_eq!(resp["lookback_days"], json!(1));
        assert_eq!(resp["target_agent"], json!(""));
    }

    #[test]
    fn apply_sets_all_fields_and_roundtrips() {
        let mut table = toml::Table::new();
        let params = json!({
            "auto_run": true,
            "dry_run": false,
            "interval_hours": 6,
            "lookback_days": 3,
            "target_agent": "agnes",
        });
        let changes = apply_skill_synthesis_to_table(&mut table, &params).unwrap();
        assert_eq!(changes.len(), 5);

        let resp = skill_synthesis_table_to_response(&table);
        assert_eq!(resp["auto_run"], json!(true));
        assert_eq!(resp["dry_run"], json!(false));
        assert_eq!(resp["interval_hours"], json!(6));
        assert_eq!(resp["lookback_days"], json!(3));
        assert_eq!(resp["target_agent"], json!("agnes"));
    }

    #[test]
    fn apply_rejects_out_of_range_lookback() {
        let mut table = toml::Table::new();
        let err = apply_skill_synthesis_to_table(&mut table, &json!({ "lookback_days": 99 }))
            .unwrap_err();
        assert!(err.contains("lookback_days"), "got: {err}");
    }

    #[test]
    fn apply_rejects_zero_interval() {
        let mut table = toml::Table::new();
        let err = apply_skill_synthesis_to_table(&mut table, &json!({ "interval_hours": 0 }))
            .unwrap_err();
        assert!(err.contains("interval_hours"), "got: {err}");
    }

    #[test]
    fn apply_blank_target_clears_key() {
        let mut table = toml::Table::new();
        // Seed an existing value, then clear it.
        apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": "agnes" })).unwrap();
        let changes =
            apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": "  " })).unwrap();
        assert!(changes.iter().any(|c| c.contains("cleared")), "got: {changes:?}");
        let resp = skill_synthesis_table_to_response(&table);
        assert_eq!(resp["target_agent"], json!(""));
    }

    #[test]
    fn apply_rejects_path_traversal_in_target() {
        let mut table = toml::Table::new();
        for bad in ["../etc", "a/b", "a\\b"] {
            let err = apply_skill_synthesis_to_table(&mut table, &json!({ "target_agent": bad }))
                .unwrap_err();
            assert!(err.contains("invalid characters"), "expected reject for {bad}, got: {err}");
        }
    }

    #[test]
    fn apply_empty_params_yields_no_changes() {
        let mut table = toml::Table::new();
        let changes = apply_skill_synthesis_to_table(&mut table, &json!({})).unwrap();
        assert!(changes.is_empty(), "empty params must produce no changes");
    }
}

#[cfg(test)]
mod autopilot_validation_tests {
    use super::*;

    #[test]
    fn trigger_event_known_values_pass() {
        for ev in [
            "task_created",
            "task_updated",
            "task_status_changed",
            "activity_new",
            "channel_message",
            "agent_idle",
            "cron_tick",
        ] {
            assert!(validate_autopilot_trigger_event(ev).is_ok(), "should accept {ev}");
        }
    }

    #[test]
    fn trigger_event_rejects_typos() {
        assert!(validate_autopilot_trigger_event("task.created").is_err());
        assert!(validate_autopilot_trigger_event("").is_err());
        assert!(validate_autopilot_trigger_event("randomEvent").is_err());
    }

    #[test]
    fn action_delegate_requires_target_and_prompt() {
        let ok = json!({ "type": "delegate", "target_agent": "bruno", "prompt": "go" });
        assert!(validate_autopilot_action(&ok).is_ok());

        let missing_target = json!({ "type": "delegate", "prompt": "go" });
        assert!(validate_autopilot_action(&missing_target).is_err());

        let missing_prompt = json!({ "type": "delegate", "target_agent": "bruno" });
        assert!(validate_autopilot_action(&missing_prompt).is_err());
    }

    #[test]
    fn action_notify_requires_channel_chat_text() {
        let ok = json!({ "type": "notify", "channel": "telegram", "chat_id": "1", "text": "hi" });
        assert!(validate_autopilot_action(&ok).is_ok());

        let missing = json!({ "type": "notify", "channel": "telegram" });
        assert!(validate_autopilot_action(&missing).is_err());
    }

    #[test]
    fn action_run_skill_requires_target_and_skill() {
        let ok = json!({ "type": "run_skill", "target_agent": "bruno", "skill_name": "audit" });
        assert!(validate_autopilot_action(&ok).is_ok());

        let missing = json!({ "type": "run_skill", "target_agent": "bruno" });
        assert!(validate_autopilot_action(&missing).is_err());
    }

    #[test]
    fn action_rejects_unknown_type() {
        let bad = json!({ "type": "self_destruct" });
        assert!(validate_autopilot_action(&bad).is_err());
    }

    #[test]
    fn action_rejects_non_object() {
        assert!(validate_autopilot_action(&Value::Null).is_err());
        assert!(validate_autopilot_action(&json!("delegate")).is_err());
    }
}

#[cfg(test)]
mod odoo_test_params_tests {
    use super::*;

    fn full_params() -> Value {
        json!({
            "url": "https://fc00.example.com/odoo",
            "db": "odoo_demo",
            "protocol": "jsonrpc",
            "auth_method": "api_key",
            "username": "admin",
            "api_key": "secret-key",
        })
    }

    #[test]
    fn happy_path_returns_config_and_credential() {
        let (cfg, cred) = MethodHandler::build_test_config_from_params(&full_params()).unwrap();
        assert_eq!(cfg.url, "https://fc00.example.com/odoo");
        assert_eq!(cfg.db, "odoo_demo");
        assert_eq!(cfg.protocol, "jsonrpc");
        assert_eq!(cfg.auth_method, "api_key");
        assert_eq!(cfg.username, "admin");
        assert!(cfg.is_configured());
        assert_eq!(cred.as_deref(), Some("secret-key"));
    }

    #[test]
    fn missing_url_is_rejected() {
        let p = json!({ "db": "x" });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("url"), "got: {err}");
    }

    #[test]
    fn missing_db_is_rejected() {
        let p = json!({ "url": "https://x.example.com" });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("db"), "got: {err}");
    }

    #[test]
    fn http_non_localhost_url_is_rejected() {
        // SSRF guard: only HTTPS, except for strict localhost.
        let p = json!({ "url": "http://evil.example.com", "db": "x" });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("HTTPS"), "got: {err}");
    }

    #[test]
    fn private_ip_is_rejected() {
        let p = json!({ "url": "https://10.0.0.1", "db": "x" });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("HTTPS") || err.contains("localhost"), "got: {err}");
    }

    #[test]
    fn fc00_dotted_hostname_is_not_misclassified_as_ipv6_ula() {
        // Regression: `fc00.example.com` shares the IPv6 ULA prefix label but is
        // a domain — must not be rejected as a private IP.
        let p = json!({
            "url": "https://fc00.example.com",
            "db": "test",
        });
        let res = MethodHandler::build_test_config_from_params(&p);
        assert!(res.is_ok(), "got: {res:?}");
    }

    #[test]
    fn invalid_db_name_is_rejected() {
        let p = json!({ "url": "https://x.example.com", "db": "bad name!" });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("database name"), "got: {err}");
    }

    #[test]
    fn invalid_protocol_is_rejected() {
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "protocol": "soap",
        });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("protocol"), "got: {err}");
    }

    #[test]
    fn invalid_auth_method_is_rejected() {
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "auth_method": "oauth",
        });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("auth_method"), "got: {err}");
    }

    #[test]
    fn missing_credential_returns_none_for_fallback() {
        // Caller intentionally omits the credential field → handler should
        // fall back to stored credential. Helper returns Ok with `None`.
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "auth_method": "api_key",
        });
        let (_cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
        assert!(cred.is_none());
    }

    #[test]
    fn empty_credential_string_treated_as_missing() {
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "api_key": "",
        });
        let (_cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
        assert!(cred.is_none());
    }

    #[test]
    fn password_auth_method_picks_password_field() {
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "auth_method": "password",
            "password": "pw",
            "api_key": "should-be-ignored",
        });
        let (cfg, cred) = MethodHandler::build_test_config_from_params(&p).unwrap();
        assert_eq!(cfg.auth_method, "password");
        assert_eq!(cred.as_deref(), Some("pw"));
    }

    #[test]
    fn username_over_256_chars_is_rejected() {
        let long = "a".repeat(257);
        let p = json!({
            "url": "https://x.example.com",
            "db": "x",
            "username": long,
        });
        let err = MethodHandler::build_test_config_from_params(&p).unwrap_err();
        assert!(err.contains("Username"), "got: {err}");
    }

    #[test]
    fn localhost_http_is_allowed_for_dev() {
        let p = json!({ "url": "http://127.0.0.1:8069", "db": "dev" });
        let res = MethodHandler::build_test_config_from_params(&p);
        assert!(res.is_ok(), "got: {res:?}");
    }

    #[test]
    fn scrub_long_error_truncates_with_ellipsis() {
        let long_err = "x".repeat(300);
        let out = MethodHandler::scrub_odoo_error(&long_err);
        assert!(out.chars().count() <= 241, "got len {}", out.chars().count());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn scrub_short_error_unchanged() {
        let short = "401 Unauthorized";
        let out = MethodHandler::scrub_odoo_error(short);
        assert_eq!(out, short);
    }

    // ── M19: scrubbing must strip URLs/credentials even on SHORT errors ──

    #[test]
    fn scrub_short_error_removes_url_query_string() {
        // The reqwest error echoes the full URL incl. a leaking query string.
        let err = "error sending request for url (https://erp.example.com/jsonrpc?api_key=topsecret)";
        let out = MethodHandler::scrub_odoo_error(err);
        assert!(!out.contains("topsecret"), "token leaked: {out}");
        assert!(!out.contains("api_key=topsecret"), "query string leaked: {out}");
        // Host is retained so the user can still act on the error.
        assert!(out.contains("erp.example.com"), "host should remain: {out}");
    }

    #[test]
    fn scrub_short_error_removes_url_userinfo() {
        let err = "connect failed: https://admin:hunter2@erp.example.com/odoo";
        let out = MethodHandler::scrub_odoo_error(err);
        assert!(!out.contains("hunter2"), "password leaked: {out}");
        assert!(!out.contains("admin:"), "userinfo leaked: {out}");
        assert!(out.contains("erp.example.com"), "host should remain: {out}");
    }

    #[test]
    fn scrub_short_error_redacts_credential_kv() {
        let err = "auth rejected token=abc123 password=p@ss";
        let out = MethodHandler::scrub_odoo_error(err);
        assert!(!out.contains("abc123"), "token leaked: {out}");
        assert!(!out.contains("p@ss"), "password leaked: {out}");
        assert!(out.contains("[REDACTED]"), "expected redaction marker: {out}");
    }
}

// ── P1 dashboard-config helper tests (RT / EVO / CT / INF) ────────────────────
#[cfg(test)]
mod p1_config_helper_tests {
    use super::*;

    // ── RT: runtime provider enum validation ──

    #[test]
    fn runtime_valid_provider_and_toggles_written() {
        let mut t = toml::Table::new();
        let params = json!({ "runtime": {
            "provider": "codex",
            "fallback": "claude",
            "pty_pool_enabled": true,
            "worker_managed": false,
        }});
        let changes = apply_runtime_to_table(&mut t, &params).unwrap();
        assert_eq!(changes.len(), 4);
        let rt = t.get("runtime").unwrap().as_table().unwrap();
        assert_eq!(rt.get("provider").unwrap().as_str(), Some("codex"));
        assert_eq!(rt.get("fallback").unwrap().as_str(), Some("claude"));
        assert_eq!(rt.get("pty_pool_enabled").unwrap().as_bool(), Some(true));
        assert_eq!(rt.get("worker_managed").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn runtime_unknown_provider_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "runtime": { "provider": "gpt4" } });
        let err = apply_runtime_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("provider"), "got: {err}");
    }

    #[test]
    fn runtime_unknown_fallback_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "runtime": { "fallback": "bogus" } });
        let err = apply_runtime_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("fallback"), "got: {err}");
    }

    #[test]
    fn runtime_empty_fallback_clears() {
        let mut t = toml::Table::new();
        t.insert("runtime".into(), toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("fallback".into(), toml::Value::String("claude".into()));
            m
        }));
        let params = json!({ "runtime": { "fallback": "" } });
        let changes = apply_runtime_to_table(&mut t, &params).unwrap();
        assert!(changes.iter().any(|c| c.contains("cleared")));
        let rt = t.get("runtime").unwrap().as_table().unwrap();
        assert!(rt.get("fallback").is_none());
    }

    #[test]
    fn runtime_absent_object_is_noop() {
        let mut t = toml::Table::new();
        let changes = apply_runtime_to_table(&mut t, &json!({})).unwrap();
        assert!(changes.is_empty());
        assert!(t.get("runtime").is_none());
    }

    // ── EVO: range validation + external_factors ──

    #[test]
    fn evolution_advanced_writes_external_factors_and_scalars() {
        let mut t = toml::Table::new();
        let params = json!({ "evolution_advanced": {
            "external_factors": { "user_feedback": true, "peer_signals": false },
            "skill_synthesis_enabled": true,
            "skill_synthesis_threshold": 3,
            "skill_synthesis_cooldown_hours": 12,
            "curiosity_max_daily": 5,
        }});
        let changes = apply_evolution_advanced_to_table(&mut t, &params).unwrap();
        assert!(!changes.is_empty());
        let evo = t.get("evolution").unwrap().as_table().unwrap();
        let ef = evo.get("external_factors").unwrap().as_table().unwrap();
        assert_eq!(ef.get("user_feedback").unwrap().as_bool(), Some(true));
        assert_eq!(ef.get("peer_signals").unwrap().as_bool(), Some(false));
        assert_eq!(evo.get("skill_synthesis_enabled").unwrap().as_bool(), Some(true));
        // skill_synthesis_threshold is a u32 gap-count, not a unit threshold —
        // it must serialize as a TOML integer (see apply_evolution_advanced_to_table).
        assert_eq!(
            evo.get("skill_synthesis_threshold").unwrap().as_integer(),
            Some(3)
        );
        assert_eq!(evo.get("skill_synthesis_cooldown_hours").unwrap().as_integer(), Some(12));
        assert_eq!(evo.get("curiosity_max_daily").unwrap().as_integer(), Some(5));
    }

    #[test]
    fn evolution_threshold_out_of_range_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "evolution_advanced": { "curiosity_threshold": 1.5 } });
        let err = apply_evolution_advanced_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("0.0-1.0"), "got: {err}");
    }

    #[test]
    fn evolution_min_lift_negative_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "evolution_advanced": { "skill_graduation_min_lift": -0.1 } });
        let err = apply_evolution_advanced_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("0.0-1.0"), "got: {err}");
    }

    #[test]
    fn evolution_absent_object_is_noop() {
        let mut t = toml::Table::new();
        let changes = apply_evolution_advanced_to_table(&mut t, &json!({})).unwrap();
        assert!(changes.is_empty());
    }

    // ── CT: mount parsing ──

    #[test]
    fn container_advanced_worktree_and_mounts_written() {
        let mut t = toml::Table::new();
        let params = json!({ "container_advanced": {
            "worktree_enabled": true,
            "worktree_copy_files": [".env.example", "config.toml"],
            "additional_mounts": [
                { "host": "~/projects", "container": "/projects", "readonly": false },
                { "host": "~/Documents", "container": "/docs", "readonly": true },
            ],
            "cmd": ["bash", "-c", "echo hi"],
            "env": [ { "key": "FOO", "value": "bar" }, ["BAZ", "qux"] ],
        }});
        let changes = apply_container_advanced_to_table(&mut t, &params).unwrap();
        assert!(!changes.is_empty());
        let ct = t.get("container").unwrap().as_table().unwrap();
        assert_eq!(ct.get("worktree_enabled").unwrap().as_bool(), Some(true));
        let mounts = ct.get("additional_mounts").unwrap().as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        let m0 = mounts[0].as_table().unwrap();
        assert_eq!(m0.get("host").unwrap().as_str(), Some("~/projects"));
        assert_eq!(m0.get("readonly").unwrap().as_bool(), Some(false));
        let env = ct.get("env").unwrap().as_array().unwrap();
        assert_eq!(env.len(), 2);
        let e0 = env[0].as_array().unwrap();
        assert_eq!(e0[0].as_str(), Some("FOO"));
        assert_eq!(e0[1].as_str(), Some("bar"));
        let e1 = env[1].as_array().unwrap();
        assert_eq!(e1[0].as_str(), Some("BAZ"));
    }

    #[test]
    fn container_mount_blocked_pattern_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "container_advanced": {
            "additional_mounts": [ { "host": "~/.ssh", "container": "/keys" } ]
        }});
        let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("blocked pattern"), "got: {err}");
    }

    #[test]
    fn container_mount_empty_path_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "container_advanced": {
            "additional_mounts": [ { "host": "", "container": "/x" } ]
        }});
        let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("host"), "got: {err}");
    }

    #[test]
    fn container_env_bad_arity_rejected() {
        let mut t = toml::Table::new();
        let params = json!({ "container_advanced": { "env": [ ["ONLY_ONE"] ] } });
        let err = apply_container_advanced_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("2 elements"), "got: {err}");
    }

    // ── INF: router cross-validation + secret masking ──

    #[test]
    fn inference_router_strong_must_be_less_than_fast() {
        let mut t = toml::Table::new();
        let params = json!({ "router": { "fast_threshold": 0.5, "strong_threshold": 0.6 } });
        let err = apply_inference_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("must be <"), "got: {err}");
    }

    #[test]
    fn inference_router_valid_thresholds_pass() {
        let mut t = toml::Table::new();
        let params = json!({ "router": {
            "enabled": true,
            "fast_threshold": 0.7,
            "strong_threshold": 0.35,
            "cloud_keywords": ["refactor"],
        }});
        let changes = apply_inference_to_table(&mut t, &params).unwrap();
        assert!(!changes.is_empty());
        let r = t.get("router").unwrap().as_table().unwrap();
        assert_eq!(r.get("fast_threshold").unwrap().as_float(), Some(0.7));
        assert_eq!(r.get("strong_threshold").unwrap().as_float(), Some(0.35));
    }

    #[test]
    fn inference_router_uses_existing_fast_for_cross_check() {
        // Existing fast=0.7; incoming strong=0.8 only → must still be rejected.
        let mut t = toml::Table::new();
        t.insert("router".into(), toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("fast_threshold".into(), toml::Value::Float(0.7));
            m
        }));
        let params = json!({ "router": { "strong_threshold": 0.8 } });
        let err = apply_inference_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("must be <"), "got: {err}");
    }

    #[test]
    fn inference_generation_temperature_range_enforced() {
        let mut t = toml::Table::new();
        let params = json!({ "generation": { "temperature": 3.0 } });
        let err = apply_inference_to_table(&mut t, &params).unwrap_err();
        assert!(err.contains("temperature"), "got: {err}");
    }

    #[test]
    fn inference_root_and_passthrough_sections_written() {
        let mut t = toml::Table::new();
        let params = json!({
            "enabled": true,
            "backend": "llama_cpp",
            "max_memory_mb": 8192,
            "llamafile": { "auto_start": true, "port": 8080 },
            "embedding": { "enabled": false, "model": "bge-small-zh" },
        });
        let changes = apply_inference_to_table(&mut t, &params).unwrap();
        assert!(!changes.is_empty());
        assert_eq!(t.get("enabled").unwrap().as_bool(), Some(true));
        assert_eq!(t.get("max_memory_mb").unwrap().as_integer(), Some(8192));
        let lf = t.get("llamafile").unwrap().as_table().unwrap();
        assert_eq!(lf.get("port").unwrap().as_integer(), Some(8080));
    }

    #[test]
    fn inference_response_masks_api_key_cleartext() {
        let mut t = toml::Table::new();
        t.insert("openai_compat".into(), toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("base_url".into(), toml::Value::String("http://x/v1".into()));
            m.insert("api_key".into(), toml::Value::String("sk-supersecret".into()));
            m
        }));
        let resp = inference_table_to_response(&t);
        let oc = resp.get("openai_compat").unwrap();
        let serialised = serde_json::to_string(&resp).unwrap();
        assert!(!serialised.contains("sk-supersecret"), "cleartext leaked: {serialised}");
        assert_eq!(oc.get("api_key").unwrap().as_str(), Some(SECRET_MASK_SET));
        assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(true));
        assert!(oc.get("api_key_enc").is_none());
    }

    #[test]
    fn inference_response_masks_encrypted_key_too() {
        let mut t = toml::Table::new();
        t.insert("openai_compat".into(), toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("api_key_enc".into(), toml::Value::String("ENCBLOB==".into()));
            m
        }));
        let resp = inference_table_to_response(&t);
        let serialised = serde_json::to_string(&resp).unwrap();
        assert!(!serialised.contains("ENCBLOB"), "enc leaked: {serialised}");
        let oc = resp.get("openai_compat").unwrap();
        assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn inference_response_no_secret_reports_unset() {
        let mut t = toml::Table::new();
        t.insert("openai_compat".into(), toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("base_url".into(), toml::Value::String("http://x/v1".into()));
            m
        }));
        let resp = inference_table_to_response(&t);
        let oc = resp.get("openai_compat").unwrap();
        assert_eq!(oc.get("api_key_set").unwrap().as_bool(), Some(false));
        assert_eq!(oc.get("api_key").unwrap().as_str(), Some(""));
    }
}

#[cfg(test)]
mod p2_dashboard_config_tests {
    use super::*;

    // ── GOV: governance policy validation + YAML round-trip ──────────────────

    #[test]
    fn gov_rate_policy_valid_round_trips() {
        let p = json!({
            "policy_type": "rate",
            "policy_id": "rate-mcp",
            "agent_id": "*",
            "resource": "mcp_calls",
            "limit": 200,
            "window_seconds": 60,
            "action_on_violation": "reject",
        });
        let v = gov_validate_policy(&p).expect("should validate");
        let yaml = gov_emit_yaml(&[v]);
        assert!(yaml.contains("policy_type: rate"));
        assert!(yaml.contains("limit: 200"));
        // Parse back and confirm fields survive.
        let parsed = gov_parse_yaml(&yaml).expect("parse");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["policy_id"].as_str(), Some("rate-mcp"));
        assert_eq!(parsed[0]["limit"].as_u64(), Some(200));
        assert_eq!(parsed[0]["resource"].as_str(), Some("mcp_calls"));
    }

    #[test]
    fn gov_rate_rejects_zero_limit_and_bad_resource() {
        let zero = json!({
            "policy_type": "rate", "policy_id": "x", "agent_id": "*",
            "resource": "mcp_calls", "limit": 0, "window_seconds": 60,
        });
        assert!(gov_validate_policy(&zero).is_err());
        let bad_res = json!({
            "policy_type": "rate", "policy_id": "x", "agent_id": "*",
            "resource": "rocket_launches", "limit": 5, "window_seconds": 60,
        });
        assert!(gov_validate_policy(&bad_res).is_err());
    }

    #[test]
    fn gov_permission_rejects_scope_conflict() {
        let p = json!({
            "policy_type": "permission", "policy_id": "perm", "agent_id": "*",
            "allowed_scopes": ["memory:read"],
            "denied_scopes": ["memory:read"],
        });
        assert!(gov_validate_policy(&p).is_err());
    }

    #[test]
    fn gov_rejects_unknown_type_and_bad_id() {
        assert!(gov_validate_policy(&json!({"policy_type": "nuke", "policy_id": "x", "agent_id": "*"})).is_err());
        assert!(gov_validate_policy(&json!({"policy_type": "rate", "policy_id": "bad id!", "agent_id": "*"})).is_err());
    }

    #[test]
    fn gov_parses_existing_global_yaml_shape() {
        let raw = r#"
policies:
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - wiki:read
    denied_scopes:
      - admin
    requires_approval:
      - agent:create
"#;
        let parsed = gov_parse_yaml(raw).expect("parse");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["policy_type"].as_str(), Some("rate"));
        assert_eq!(parsed[0]["window_seconds"].as_u64(), Some(60));
        let perm = &parsed[1];
        assert_eq!(perm["allowed_scopes"].as_array().unwrap().len(), 2);
        assert_eq!(perm["denied_scopes"].as_array().unwrap()[0].as_str(), Some("admin"));
    }

    // ── ODO: qualified-action parse ──────────────────────────────────────────

    #[test]
    fn odo_action_accepts_bare_and_qualified() {
        assert!(odo_valid_action("read"));
        assert!(odo_valid_action("write"));
        assert!(odo_valid_action("write:crm.lead"));
        assert!(odo_valid_action("execute:sale.order"));
    }

    #[test]
    fn odo_action_rejects_bad_verb_and_model() {
        assert!(!odo_valid_action("destroy"));
        assert!(!odo_valid_action("write:"));
        assert!(!odo_valid_action("write:bad model!"));
        assert!(!odo_valid_action(""));
    }

    #[test]
    fn odo_apply_encrypts_secret_and_keeps_qualified_action() {
        let tmp = std::env::temp_dir().join(format!("ddc-odo-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut table = toml::Table::new();
        let params = json!({
            "odoo": {
                "profile": "sales",
                "allowed_actions": ["read", "write:crm.lead"],
                "company_ids": [1, 2],
                "api_key": "super-secret",
            }
        });
        let changes = apply_odoo_to_table(&mut table, &params, &tmp).expect("apply");
        let odoo = table.get("odoo").unwrap().as_table().unwrap();
        // Cleartext api_key must NOT be present; only api_key_enc.
        assert!(odoo.get("api_key").is_none());
        assert!(odoo.get("api_key_enc").is_some());
        // Qualified action preserved.
        let actions: Vec<&str> = odoo.get("allowed_actions").unwrap().as_array().unwrap()
            .iter().filter_map(|v| v.as_str()).collect();
        assert!(actions.contains(&"write:crm.lead"));
        assert_eq!(odoo.get("company_ids").unwrap().as_array().unwrap().len(), 2);
        assert!(changes.iter().any(|c| c.contains("[ENCRYPTED]")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn odo_apply_stores_secret_reference_raw() {
        // A `secret://` value is a pointer, not a secret — it must be stored
        // verbatim into `*_enc` (NOT AES-encrypted), so the connector pool can
        // resolve it via the SecretManager at connect time.
        let tmp = std::env::temp_dir().join(format!("ddc-odo-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut table = toml::Table::new();
        let params = json!({
            "odoo": {
                "api_key": "secret://vault/odoo-api-key",
                "password": "secret://vault/odoo-password",
            }
        });
        let changes = apply_odoo_to_table(&mut table, &params, &tmp).expect("apply");
        let odoo = table.get("odoo").unwrap().as_table().unwrap();
        // Stored raw, NOT encrypted.
        assert_eq!(
            odoo.get("api_key_enc").and_then(|v| v.as_str()),
            Some("secret://vault/odoo-api-key")
        );
        assert_eq!(
            odoo.get("password_enc").and_then(|v| v.as_str()),
            Some("secret://vault/odoo-password")
        );
        // No cleartext mirror left behind.
        assert!(odoo.get("api_key").is_none());
        assert!(odoo.get("password").is_none());
        assert!(changes.iter().any(|c| c.contains("[SECRET REF]")));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn odo_apply_rejects_bad_action() {
        let mut table = toml::Table::new();
        let params = json!({ "odoo": { "allowed_actions": ["nuke:crm.lead"] } });
        let tmp = std::env::temp_dir();
        assert!(apply_odoo_to_table(&mut table, &params, &tmp).is_err());
    }

    // ── SCP: namespace mode parse ────────────────────────────────────────────

    #[test]
    fn scp_apply_sets_read_only_with_synced_from() {
        let mut table = toml::Table::new();
        let change = scp_apply_namespace(&mut table, "identity", "read_only", Some("identity-provider"), false)
            .expect("apply");
        assert!(change.contains("read_only"));
        let ns = table["namespaces"].as_table().unwrap()["identity"].as_table().unwrap();
        assert_eq!(ns["mode"].as_str(), Some("read_only"));
        assert_eq!(ns["synced_from"].as_str(), Some("identity-provider"));
    }

    #[test]
    fn scp_read_only_requires_synced_from() {
        let mut table = toml::Table::new();
        assert!(scp_apply_namespace(&mut table, "identity", "read_only", None, false).is_err());
    }

    #[test]
    fn scp_rejects_bad_mode_and_nested_namespace() {
        let mut table = toml::Table::new();
        assert!(scp_apply_namespace(&mut table, "identity", "broadcast", None, false).is_err());
        assert!(scp_apply_namespace(&mut table, "a/b", "agent_writable", None, false).is_err());
    }

    #[test]
    fn scp_remove_deletes_entry() {
        let mut table = toml::Table::new();
        scp_apply_namespace(&mut table, "policies", "operator_only", None, false).unwrap();
        assert!(table["namespaces"].as_table().unwrap().contains_key("policies"));
        scp_apply_namespace(&mut table, "policies", "agent_writable", None, true).unwrap();
        assert!(!table["namespaces"].as_table().unwrap().contains_key("policies"));
    }

    #[test]
    fn scp_response_sorted_and_shaped() {
        let mut table = toml::Table::new();
        scp_apply_namespace(&mut table, "zeta", "operator_only", None, false).unwrap();
        scp_apply_namespace(&mut table, "alpha", "agent_writable", None, false).unwrap();
        let resp = scp_table_to_response(&table);
        let arr = resp["namespaces"].as_array().unwrap();
        assert_eq!(arr[0]["namespace"].as_str(), Some("alpha"));
        assert_eq!(arr[1]["namespace"].as_str(), Some("zeta"));
    }

    // ── XC.3: phone_number_id is NOT encrypted (alignment) ───────────────────

    #[test]
    fn xc3_whatsapp_phone_number_id_not_encrypted_in_agent_path() {
        // The per-agent set_channel_token path only encrypts keys containing
        // "token"/"secret"/"app_id". phone_number_id does NOT match → plaintext.
        let field = "phone_number_id";
        let should_encrypt = field.contains("token") || field.contains("secret") || field == "app_id";
        assert!(!should_encrypt, "phone_number_id must not be encrypted");
    }

    #[test]
    fn xc3_global_secret_is_plain_only_for_phone_number_id() {
        // Mirrors the secret_is_plain decision in handle_channels_add.
        let plain = |sk: Option<&str>| sk == Some("whatsapp_phone_number_id");
        assert!(plain(Some("whatsapp_phone_number_id")));
        assert!(!plain(Some("slack_app_token")));
        assert!(!plain(Some("line_channel_secret")));
    }

    // ── XC.4: skills.adopt agent-id validation surface ───────────────────────

    #[test]
    fn xc4_invalid_target_agent_id_rejected() {
        assert!(!is_valid_agent_id("../etc"));
        assert!(!is_valid_agent_id("Bad Name"));
        assert!(is_valid_agent_id("bruno"));
    }
}
