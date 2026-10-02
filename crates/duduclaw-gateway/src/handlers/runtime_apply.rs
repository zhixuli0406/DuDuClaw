//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

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

/// Is `v` a valid AI runtime provider for `agent.toml [runtime]`?
///
/// WP-B: reads `duduclaw_core::runtime_catalog` instead of a hand-written
/// mirror of the registry backends — the list and the registry can no longer
/// disagree, and a new runtime is accepted the moment its catalog entry lands.
/// Canonical ids only (no aliases): what gets written into `agent.toml` should
/// be the canonical value, so `agy` is rejected here even though
/// `RuntimeType::parse` accepts it when reading.
pub(crate) fn is_valid_runtime_provider(v: &str) -> bool {
    duduclaw_core::types::RuntimeType::from_id(v).is_some()
}

/// `claude|codex|gemini|…` for the error message. Built from the catalog so it
/// cannot go stale.
pub(crate) fn valid_runtime_providers_display() -> String {
    duduclaw_core::types::RuntimeType::valid_values()
}

/// Detect a Claude Code OAuth session. Returns `(has_oauth, subscription_tier)`.
/// Never returns the token itself — only its presence — so this is safe to
/// expose at viewer level.
///
/// Fast path reads `~/.claude/.credentials.json` (Linux keeps plaintext
/// credentials there). On macOS the CLI stores OAuth in the **Keychain**, so a
/// missing file proves nothing — when the file probe misses and a `claude`
/// binary exists, ask `claude auth status` directly (read-only JSON, ~1s,
/// hard 8s timeout). The old file-only probe reported "not logged in" for
/// every macOS user.
pub(crate) async fn detect_claude_oauth(claude_bin: Option<&str>) -> (bool, Option<String>) {
    if let Some(found) = detect_claude_oauth_from_file() {
        return found;
    }
    let Some(bin) = claude_bin else {
        return (false, None);
    };
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::process::Command::new(bin)
            .args(["auth", "status"])
            .output(),
    )
    .await;
    let Ok(Ok(out)) = output else {
        return (false, None);
    };
    let Ok(json) = serde_json::from_slice::<Value>(&out.stdout) else {
        return (false, None);
    };
    let logged_in = json
        .get("loggedIn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let sub = json
        .get("subscriptionType")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (logged_in, sub)
}

/// File-based OAuth probe (`~/.claude/.credentials.json`, OS user home — not
/// `DUDUCLAW_HOME`). `Some` only on a positive hit; a missing/unparsable file
/// or token-less content falls through to the CLI probe (macOS Keychain case).
pub(crate) fn detect_claude_oauth_from_file() -> Option<(bool, Option<String>)> {
    let home = duduclaw_core::platform::home_dir();
    if home.is_empty() {
        return None;
    }
    let cred_path = std::path::Path::new(&home)
        .join(".claude")
        .join(".credentials.json");
    let content = std::fs::read_to_string(&cred_path).ok()?;
    let json = serde_json::from_str::<Value>(&content).ok()?;

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
                return Some((true, sub));
            }
        }
    }
    None
}

/// One deprecated runtime value that a dashboard write put into
/// `agent.toml [runtime]` (R1, 2026-10). The write itself succeeds; the
/// caller turns each of these into a `runtime_provider_deprecated` audit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeprecatedRuntimeWrite {
    /// `"provider"` or `"fallback"`.
    pub field: &'static str,
    /// Canonical runtime id that was written.
    pub value: &'static str,
    pub replacement: &'static str,
    pub remove_in: &'static str,
}

impl DeprecatedRuntimeWrite {
    fn for_value(field: &'static str, v: &str) -> Option<Self> {
        let rt = duduclaw_core::types::RuntimeType::from_id(v)?;
        let dep = rt.deprecation()?;
        Some(Self {
            field,
            value: rt.as_str(),
            replacement: dep.replacement,
            remove_in: dep.remove_in,
        })
    }
}

/// [`apply_runtime_to_table_reporting`]'s result: the human-readable change
/// list plus every deprecated value written.
#[derive(Debug, Default)]
pub(crate) struct RuntimeApplyOutcome {
    pub changes: Vec<String>,
    pub deprecated: Vec<DeprecatedRuntimeWrite>,
}

/// Validate + write the `[runtime]` section from the `runtime` params object.
/// Fields: `provider` (enum), `fallback` (string). (RT.1)
///
/// Test-facing shorthand; production calls
/// [`apply_runtime_to_table_reporting`] so deprecated writes get audited.
#[cfg(test)]
pub(crate) fn apply_runtime_to_table(table: &mut toml::Table, params: &Value) -> Result<Vec<String>, String> {
    apply_runtime_to_table_reporting(table, params).map(|o| o.changes)
}

/// [`apply_runtime_to_table`], additionally reporting which written values
/// newly name a deprecated runtime (a value equal to the stored one is not
/// reported). The write is identical either way — a
/// deprecated runtime is still accepted (deprecation policy: old values keep
/// working until the removal version).
pub(crate) fn apply_runtime_to_table_reporting(
    table: &mut toml::Table,
    params: &Value,
) -> Result<RuntimeApplyOutcome, String> {
    let mut changes: Vec<String> = Vec::new();
    let mut deprecated: Vec<DeprecatedRuntimeWrite> = Vec::new();

    let rt = match params.get("runtime").and_then(|v| v.as_object()) {
        Some(r) => r,
        None => return Ok(RuntimeApplyOutcome::default()),
    };

    let section = table
        .entry("runtime")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [runtime] section".to_string())?;
    // A deprecated value is reported only when this write introduces it
    // (new or changed): re-saving an agent that already runs `gemini` while
    // editing an unrelated field is not a fresh decision to audit.
    let stored = |section: &toml::Table, key: &str| {
        section
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(duduclaw_core::types::RuntimeType::parse)
            .map(|rt| rt.as_str())
    };
    let introduces = |previous: Option<&'static str>, v: &str| {
        duduclaw_core::types::RuntimeType::from_id(v).map(|rt| rt.as_str()) != previous
    };

    if let Some(v) = rt.get("provider").and_then(|v| v.as_str()) {
        if !is_valid_runtime_provider(v) {
            return Err(format!(
                "Invalid runtime.provider '{v}'. Valid: {}",
                valid_runtime_providers_display()
            ));
        }
        let previous = stored(section, "provider");
        section.insert("provider".into(), toml::Value::String(v.into()));
        changes.push(format!("runtime.provider = \"{v}\""));
        if introduces(previous, v) {
            deprecated.extend(DeprecatedRuntimeWrite::for_value("provider", v));
        }
    }
    if let Some(v) = rt.get("fallback").and_then(|v| v.as_str()) {
        let v = v.trim();
        // Empty string clears the fallback.
        if v.is_empty() {
            section.remove("fallback");
            changes.push("runtime.fallback cleared".to_string());
        } else {
            if !is_valid_runtime_provider(v) {
                return Err(format!(
                    "Invalid runtime.fallback '{v}'. Valid: {}",
                    valid_runtime_providers_display()
                ));
            }
            let previous = stored(section, "fallback");
            section.insert("fallback".into(), toml::Value::String(v.into()));
            changes.push(format!("runtime.fallback = \"{v}\""));
            if introduces(previous, v) {
                deprecated.extend(DeprecatedRuntimeWrite::for_value("fallback", v));
            }
        }
    }
    Ok(RuntimeApplyOutcome { changes, deprecated })
}

/// Emit one `runtime_provider_deprecated` audit event (and a `warn!`) per
/// deprecated value a dashboard RPC wrote. Mirrors `judge_mode_deprecated`
/// in `system.update_config`. `user_id` is the authenticated caller;
/// `"unknown"` only when a path genuinely carries no identity.
pub(crate) fn audit_deprecated_runtime_writes(
    home_dir: &std::path::Path,
    agent_id: &str,
    source_rpc: &str,
    user_id: &str,
    writes: &[DeprecatedRuntimeWrite],
) {
    for w in writes {
        warn!(
            agent = %agent_id,
            field = w.field,
            value = w.value,
            replacement = w.replacement,
            remove_in = w.remove_in,
            source = source_rpc,
            "agent runtime set to a deprecated runtime via {source_rpc} — see docs/guides/deprecations.md"
        );
        crate::security_autopilot::audit_and_emit(
            home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "runtime_provider_deprecated",
                agent_id,
                duduclaw_security::audit::Severity::Warning,
                deprecated_runtime_audit_details(agent_id, source_rpc, user_id, w),
            ),
        );
    }
}

/// The audit payload, split out so its shape is unit-testable.
pub(crate) fn deprecated_runtime_audit_details(
    agent_id: &str,
    source_rpc: &str,
    user_id: &str,
    w: &DeprecatedRuntimeWrite,
) -> Value {
    json!({
        "agent_id": agent_id,
        "field": format!("runtime.{}", w.field),
        "value": w.value,
        "replacement": w.replacement,
        "remove_in": w.remove_in,
        "source": source_rpc,
        "user_id": user_id,
    })
}

/// Validate a 0.0–1.0 threshold field, returning the float or an error.
pub(crate) fn validate_unit_threshold(name: &str, v: f64) -> Result<f64, String> {
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("{name} must be 0.0-1.0"));
    }
    Ok(v)
}

/// Apply the *advanced* `[evolution]` fields (EVO.1–EVO.3) NOT already handled
/// inline in `handle_agents_update`. Reads from the `evolution_advanced` params
/// object. Covers `[evolution.external_factors]` + the skill-synthesis and
/// graduation scalars that have live readers (H3, 2026-09-29).
pub(crate) fn apply_evolution_advanced_to_table(
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

    // ── Boolean toggles (EVO.2) ──
    //
    // H3 (2026-09-29): `skill_graduation_enabled` /
    // `skill_recommendation_enabled` / `curiosity_enabled` /
    // `skill_behavior_monitor_enabled` were dropped from this form — the
    // gateway never read them, so the dashboard was writing settings that
    // could not take effect.
    for key in &["skill_synthesis_enabled"] {
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
    for key in &["skill_graduation_min_lift"] {
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
pub(crate) fn parse_mount_entry(item: &Value) -> Result<toml::Value, String> {
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
    let readonly = obj
        .get("readonly")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    for pat in BLOCKED_PATTERNS {
        if host.contains(pat) {
            return Err(format!(
                "additional_mounts.host '{host}' matches blocked pattern '{pat}'"
            ));
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
pub(crate) fn parse_env_entry(item: &Value) -> Result<toml::Value, String> {
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
/// object. Covers additional_mounts / cmd / env arrays.
pub(crate) fn apply_container_advanced_to_table(
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

    // ── String-array fields: cmd (CT.2) ──
    for key in &["cmd"] {
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
        changes.push(format!(
            "container.additional_mounts = [{} entries]",
            arr.len()
        ));
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
