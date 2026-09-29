//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Parse a config.toml `[skill_synthesis]` section into the
/// `skill_synthesis.get` response shape. Defaults mirror
/// `skill_synthesis_pipeline::scheduler::SynthesisScheduleConfig::default()`
/// (auto_run=false, dry_run=true, interval_hours=24, lookback_days=1).
pub(crate) fn skill_synthesis_table_to_response(table: &toml::Table) -> Value {
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
pub(crate) fn apply_skill_synthesis_to_table(
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
