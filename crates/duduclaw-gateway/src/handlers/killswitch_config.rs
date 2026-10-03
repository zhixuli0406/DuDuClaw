//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Validate + apply a `killswitch.update` payload onto a KILLSWITCH.toml table.
/// Returns the change list. Validates numeric ranges across all sub-sections.
pub(crate) fn apply_killswitch_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
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
        // v1.68.0: `null` disarms a trigger — the key is removed, and the
        // reply path (`killswitch_triggers`) enforces only keys written in
        // the file. An absent key stays unchanged.
        for key in [
            "max_replies_per_minute",
            "max_consecutive_errors",
            "error_rate_threshold",
            "cost_limit_usd",
        ] {
            if t.get(key).is_some_and(|v| v.is_null()) && sect.remove(key).is_some() {
                changes.push(format!("triggers.{key} removed (not enforced)"));
            }
        }
        if let Some(v) = t.get("max_replies_per_minute").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("triggers.max_replies_per_minute must be 1-10000".into());
            }
            sect.insert(
                "max_replies_per_minute".into(),
                toml::Value::Integer(v as i64),
            );
            changes.push(format!("triggers.max_replies_per_minute = {v}"));
        }
        if let Some(v) = t.get("max_consecutive_errors").and_then(|v| v.as_u64()) {
            if v == 0 || v > 1000 {
                return Err("triggers.max_consecutive_errors must be 1-1000".into());
            }
            sect.insert(
                "max_consecutive_errors".into(),
                toml::Value::Integer(v as i64),
            );
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
            sect.insert(
                "frequency_window_secs".into(),
                toml::Value::Integer(v as i64),
            );
            changes.push(format!("circuit_breaker.frequency_window_secs = {v}"));
        }
        if let Some(v) = c.get("frequency_max_replies").and_then(|v| v.as_u64()) {
            if v == 0 || v > 10000 {
                return Err("circuit_breaker.frequency_max_replies must be 1-10000".into());
            }
            sect.insert(
                "frequency_max_replies".into(),
                toml::Value::Integer(v as i64),
            );
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
            sect.insert(
                "half_open_allow_count".into(),
                toml::Value::Integer(v as i64),
            );
            changes.push(format!("circuit_breaker.half_open_allow_count = {v}"));
        }
    }

    // ── [failsafe] ──
    if let Some(f) = params.get("failsafe").and_then(|v| v.as_object()) {
        let sect = sub(table, "failsafe")?;
        for key in &[
            "l1_auto_recover_secs",
            "l2_auto_recover_secs",
            "l3_auto_recover_secs",
        ] {
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
                let l = item.as_str().ok_or_else(|| {
                    "defensive_prompt.languages entries must be strings".to_string()
                })?;
                let l = l.trim();
                if l.is_empty() {
                    return Err("defensive_prompt.languages entries must be non-empty".into());
                }
                out.push(toml::Value::String(l.into()));
            }
            sect.insert("languages".into(), toml::Value::Array(out));
            changes.push(format!(
                "defensive_prompt.languages = [{} entries]",
                arr.len()
            ));
        }
    }

    // `[audit]` (v1.68.0): removed — no reader. An `audit` object in the
    // payload is ignored rather than written.

    Ok(changes)
}

/// Parse a KILLSWITCH.toml table into the `killswitch.get` response shape.
/// Falls back to the documented defaults for any missing field so the dashboard
/// always renders a complete form.
pub(crate) fn killswitch_table_to_response(table: &toml::Table) -> Value {
    let ks = duduclaw_security::killswitch::KillswitchConfig::default();

    let t = table.get("triggers").and_then(|v| v.as_table());
    let cb = table.get("circuit_breaker").and_then(|v| v.as_table());
    let fs = table.get("failsafe").and_then(|v| v.as_table());
    let sw = table.get("safety_words").and_then(|v| v.as_table());
    let dp = table.get("defensive_prompt").and_then(|v| v.as_table());

    let int = |tbl: Option<&toml::Table>, key: &str, default: i64| -> i64 {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_integer())
            .unwrap_or(default)
    };
    let flt = |tbl: Option<&toml::Table>, key: &str, default: f64| -> f64 {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_float())
            .unwrap_or(default)
    };
    let boolean = |tbl: Option<&toml::Table>, key: &str, default: bool| -> bool {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };
    let strv = |tbl: Option<&toml::Table>, key: &str, default: &str| -> String {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };
    let arr = |tbl: Option<&toml::Table>, key: &str, default: &[String]| -> Vec<String> {
        tbl.and_then(|t| t.get(key))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_else(|| default.to_vec())
    };

    // v1.68.0: which trigger keys the reply path actually enforces (only the
    // ones written in the file; defaults above are display-only).
    let enforced = crate::killswitch_triggers::EnforcedTriggers::from_table(table);

    json!({
        "triggers_enforced": {
            "max_replies_per_minute": enforced.max_replies_per_minute.is_some(),
            "max_consecutive_errors": enforced.max_consecutive_errors.is_some(),
            "error_rate_threshold": enforced.error_rate_threshold.is_some(),
            "cost_limit_usd": enforced.cost_limit_usd.is_some(),
        },
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
    })
}
