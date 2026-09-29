//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Validate + write the top-level `[os_watch]` table into an agent.toml from the
/// `agents.update` `os_watch` param object. Returns the change list (empty if no
/// `os_watch` object was present). Path values are format-checked only (non-empty
/// strings); existence is left to the watcher's `canonicalize` warning at start.
///
/// Mirrors the additive raw-TOML shape that `os_events::read_os_watch_config`
/// parses: `paths` (string[]), `ignore` (string[]), `debounce_ms` (int ≥1),
/// `max_events_per_min` (int ≥1). Also carries `frontmost_poll_secs` (int ≥0,
/// P2-4, `os_frontmost::read_frontmost_poll_secs`), `goal_template` /
/// `goal_acceptance` (P3-4, `os_events::read_goal_template_config`) and
/// `footprint` (P4-4, `footprint_distill::read_footprint_enabled`) — a past
/// audit flagged this function and its `[os_watch]` readers as a pair that must
/// gain/lose fields together, or a dashboard edit silently can't reach a field
/// the reader honors.
pub(crate) fn apply_os_watch_to_table(table: &mut toml::Table, params: &Value) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let ow = match params.get("os_watch").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(changes),
    };

    let section = table
        .entry("os_watch")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [os_watch] section".to_string())?;

    // ── Array fields (non-empty strings) ──
    for (param_key, toml_key) in &[("paths", "paths"), ("ignore", "ignore")] {
        if let Some(arr) = ow.get(*param_key).and_then(|v| v.as_array()) {
            let mut out: Vec<toml::Value> = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("os_watch.{param_key} entries must be strings"))?;
                let s = s.trim();
                if s.is_empty() {
                    return Err(format!("os_watch.{param_key} entries must be non-empty"));
                }
                out.push(toml::Value::String(s.into()));
            }
            section.insert((*toml_key).into(), toml::Value::Array(out));
            changes.push(format!("os_watch.{toml_key} = [{} entries]", arr.len()));
        }
    }

    // ── debounce_ms (int ≥1) ──
    if let Some(v) = ow.get("debounce_ms").and_then(|v| v.as_u64()) {
        if v == 0 || v > 3_600_000 {
            return Err("os_watch.debounce_ms must be 1-3600000".into());
        }
        section.insert("debounce_ms".into(), toml::Value::Integer(v as i64));
        changes.push(format!("os_watch.debounce_ms = {v}"));
    }

    // ── max_events_per_min (int ≥1) ──
    if let Some(v) = ow.get("max_events_per_min").and_then(|v| v.as_u64()) {
        if v == 0 || v > 1_000_000 {
            return Err("os_watch.max_events_per_min must be 1-1000000".into());
        }
        section.insert("max_events_per_min".into(), toml::Value::Integer(v as i64));
        changes.push(format!("os_watch.max_events_per_min = {v}"));
    }

    // ── frontmost_poll_secs (int ≥0; P2-4, `os_frontmost::read_frontmost_poll_secs`) ──
    // 0 is the explicit "disabled" value (mirrors the reader's `secs <= 0 =>
    // None`); a positive value starts low-frequency foreground polling. Cap at
    // one hour — anything larger is a config mistake, not a real interval.
    if let Some(v) = ow.get("frontmost_poll_secs").and_then(|v| v.as_u64()) {
        if v > 3600 {
            return Err("os_watch.frontmost_poll_secs must be 0-3600 (0 = disabled)".into());
        }
        section.insert("frontmost_poll_secs".into(), toml::Value::Integer(v as i64));
        changes.push(format!("os_watch.frontmost_poll_secs = {v}"));
    }

    // ── goal_template / goal_acceptance (string, P3-4) ──
    // Mirrors `os_events::read_goal_template_config`: a non-empty string sets
    // the field; an explicit JSON `null` clears it (same convention as the
    // outfit-clear RPC elsewhere in this file) so the dashboard can remove a
    // previously-configured template without hand-editing agent.toml.
    for (param_key, toml_key) in &[
        ("goal_template", "goal_template"),
        ("goal_acceptance", "goal_acceptance"),
    ] {
        let Some(v) = ow.get(*param_key) else {
            continue;
        };
        if v.is_null() {
            if section.remove(*toml_key).is_some() {
                changes.push(format!("os_watch.{toml_key} cleared"));
            }
            continue;
        }
        let s = v
            .as_str()
            .ok_or_else(|| format!("os_watch.{param_key} must be a string or null"))?;
        let s = s.trim();
        if s.is_empty() {
            return Err(format!(
                "os_watch.{param_key} must be non-empty (use null to clear)"
            ));
        }
        if s.chars().count() > 2000 {
            return Err(format!("os_watch.{param_key} must be ≤2000 characters"));
        }
        section.insert((*toml_key).into(), toml::Value::String(s.into()));
        changes.push(format!("os_watch.{toml_key} = set"));
    }

    // ── footprint (bool, P4-4) ──
    // Deny-by-default digital-footprint memory distillation, layered on top
    // of `os_native` / `[os_watch] paths` — mirrors the `capabilities.os_native`
    // bool-write pattern in `apply_capabilities_to_table` above.
    if let Some(v) = ow.get("footprint").and_then(|v| v.as_bool()) {
        section.insert("footprint".into(), toml::Value::Boolean(v));
        changes.push(format!("os_watch.footprint = {v}"));
    }

    Ok(changes)
}

/// Validate + write the top-level `[research]` table into an agent.toml from
/// the `agents.update` `research` param object. Returns the change list
/// (empty if no `research` object was present). Mirrors
/// [`apply_os_watch_to_table`]'s shape: a small additive top-level table read
/// directly by `self_study::ResearchConfig::from_agent_dir` (bypasses
/// `duduclaw_core::types::AgentConfig` entirely, same as `[os_watch]`), so
/// this helper is the one and only writer that must gain/lose fields in step
/// with that reader.
///
/// Belief loop × goal contract gap 2
/// (design-market-belief-loop-2026-08.md §3 「自主研究」).
pub(crate) fn apply_research_to_table(table: &mut toml::Table, params: &Value) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();

    let research = match params.get("research").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(changes),
    };

    let section = table
        .entry("research")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [research] section".to_string())?;

    if let Some(v) = research.get("self_study").and_then(|v| v.as_bool()) {
        section.insert("self_study".into(), toml::Value::Boolean(v));
        changes.push(format!("research.self_study = {v}"));
    }
    if let Some(v) = research.get("self_study_hour").and_then(|v| v.as_u64()) {
        if v > 23 {
            return Err("research.self_study_hour must be 0-23".into());
        }
        section.insert("self_study_hour".into(), toml::Value::Integer(v as i64));
        changes.push(format!("research.self_study_hour = {v}"));
    }

    Ok(changes)
}
