use super::*;

pub(crate) async fn handle_evolution_toggle(params: &Value, home_dir: &Path) -> Value {
    let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id,
        _ => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: agent_id is required"}],
                "isError": true
            });
        }
    };
    // M4: reject malformed ids (e.g. "../other") so the agents/<id>/agent.toml
    // path can't be traversed out of the agents directory.
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id (lowercase alphanumeric and '-' only, max 64 chars)"}],
            "isError": true
        });
    }
    let field = match params.get("field").and_then(|v| v.as_str()) {
        Some(f) if !f.is_empty() => f,
        _ => {
            return serde_json::json!({
                "content": [{"type": "text", "text": "Error: field is required"}],
                "isError": true
            });
        }
    };
    let value_str = match params.get("value").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => match params.get("value") {
            Some(v) => &v.to_string(),
            None => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": "Error: value is required"}],
                    "isError": true
                });
            }
        },
    };

    // Read current agent.toml
    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
                "isError": true
            });
        }
    };

    let mut doc: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    // Ensure [evolution] section exists
    if !doc.contains_key("evolution") {
        doc.insert(
            "evolution".to_string(),
            toml::Value::Table(toml::Table::new()),
        );
    }
    let evo = doc.get_mut("evolution").unwrap().as_table_mut().unwrap();

    // D7 (2026-08-04): cognitive memory is permanently resident. The field is
    // answered explicitly (rather than falling through to "unknown field") so
    // an agent that still tries to flip it gets told why it no longer exists.
    if field == "cognitive_memory" {
        return serde_json::json!({
            "content": [{"type": "text", "text":
                "cognitive_memory is no longer configurable — the cognitive memory layer \
                 is always on since 2026-08-04. Nothing was changed."}]
        });
    }

    // Validate field name and apply to the correct TOML section.
    let boolean_fields = ["gvu_enabled", "skill_auto_activate", "skill_security_scan"];
    let numeric_fields = [
        "max_silence_hours",
        "skill_token_budget",
        "max_active_skills",
    ];
    // Stagnation-detection sub-section fields (prefix: stagnation_*).
    // These map into [evolution.stagnation_detection] in the TOML.
    let stagnation_bool_fields = ["stagnation_enabled"];
    let stagnation_int_fields = ["stagnation_window_seconds", "stagnation_trigger_threshold"];
    let stagnation_str_fields = ["stagnation_action"];

    let parse_bool = |s: &str| -> std::result::Result<bool, String> {
        match s {
            "true" | "1" | "yes" | "on" => Ok(true),
            "false" | "0" | "no" | "off" => Ok(false),
            _ => Err(format!("invalid boolean value '{s}' — use true/false")),
        }
    };

    if boolean_fields.contains(&field) {
        match parse_bool(value_str) {
            Ok(v) => {
                evo.insert(field.to_string(), toml::Value::Boolean(v));
            }
            Err(e) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {e}")}],
                    "isError": true
                });
            }
        }
    } else if numeric_fields.contains(&field) {
        if let Ok(int_val) = value_str.parse::<i64>() {
            evo.insert(field.to_string(), toml::Value::Integer(int_val));
        } else if let Ok(float_val) = value_str.parse::<f64>() {
            evo.insert(field.to_string(), toml::Value::Float(float_val));
        } else {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: invalid numeric value '{value_str}'")}],
                "isError": true
            });
        }
    } else if stagnation_bool_fields.contains(&field)
        || stagnation_int_fields.contains(&field)
        || stagnation_str_fields.contains(&field)
    {
        // Write into the [evolution.stagnation_detection] sub-table.
        let sd_key = field.trim_start_matches("stagnation_");

        // Ensure [evolution.stagnation_detection] sub-table exists.
        if !evo.contains_key("stagnation_detection") {
            evo.insert(
                "stagnation_detection".to_string(),
                toml::Value::Table(toml::Table::new()),
            );
        }
        let sd = evo
            .get_mut("stagnation_detection")
            .unwrap()
            .as_table_mut()
            .unwrap();

        if stagnation_bool_fields.contains(&field) {
            match parse_bool(value_str) {
                Ok(v) => {
                    sd.insert(sd_key.to_string(), toml::Value::Boolean(v));
                }
                Err(e) => {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": format!("Error: {e}")}],
                        "isError": true
                    });
                }
            }
        } else if stagnation_int_fields.contains(&field) {
            let int_val: i64 = match value_str.parse() {
                Ok(v) => v,
                Err(_) => {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": format!("Error: '{field}' requires an integer value, got '{value_str}'")}],
                        "isError": true
                    });
                }
            };
            // Range validation matching StagnationDetectionConfig::validate()
            let range_err = match sd_key {
                "window_seconds" if !(60..=604_800).contains(&int_val) => Some(format!(
                    "stagnation_window_seconds must be 60–604800, got {int_val}"
                )),
                "trigger_threshold" if !(1..=1000).contains(&int_val) => Some(format!(
                    "stagnation_trigger_threshold must be 1–1000, got {int_val}"
                )),
                _ => None,
            };
            if let Some(e) = range_err {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {e}")}],
                    "isError": true
                });
            }
            sd.insert(sd_key.to_string(), toml::Value::Integer(int_val));
        } else {
            // stagnation_action: "log_only" | "suppress" (P1 reserved)
            match value_str {
                "log_only" | "suppress" => {
                    sd.insert(
                        sd_key.to_string(),
                        toml::Value::String(value_str.to_owned()),
                    );
                }
                other => {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": format!(
                            "Error: stagnation_action must be 'log_only' or 'suppress', got '{other}'"
                        )}],
                        "isError": true
                    });
                }
            }
        }
    } else {
        let all_fields: Vec<&str> = boolean_fields
            .iter()
            .chain(numeric_fields.iter())
            .chain(stagnation_bool_fields.iter())
            .chain(stagnation_int_fields.iter())
            .chain(stagnation_str_fields.iter())
            .copied()
            .collect();
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: unknown field '{field}'. Valid fields: {}",
                all_fields.join(", ")
            )}],
            "isError": true
        });
    }

    // Write back
    let new_content = toml::to_string_pretty(&doc).unwrap_or_default();
    if let Err(e) = tokio::fs::write(&toml_path, &new_content).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Evolution config updated: {agent_id}.evolution.{field} = {value_str}\n\
             Changes take effect within 5 minutes (next heartbeat sync) or immediately on restart."
        )}]
    })
}

pub(crate) async fn handle_evolution_status_tool(
    params: &Value,
    home_dir: &Path,
    default_agent: &str,
) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent);

    let toml_path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found: {e}")}],
                "isError": true
            });
        }
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    let evo = &config.evolution;
    let sd = &evo.stagnation_detection;
    let status = format!(
        "Evolution status for agent '{agent_id}':\n\
         \n\
         GVU self-play:     {}\n\
         Cognitive memory:  always on\n\
         \n\
         Skill auto-activate:  {}\n\
         Skill security scan:  {}\n\
         Skill token budget:   {}\n\
         Max active skills:    {}\n\
         \n\
         Max silence hours:         {:.1}\n\
         \n\
         Stagnation detection:\n\
           enabled:           {}\n\
           window_seconds:    {} ({:.1}h)\n\
           trigger_threshold: {}\n\
           action:            {}",
        evo.gvu_enabled,
        evo.skill_auto_activate,
        evo.skill_security_scan,
        evo.skill_token_budget,
        evo.max_active_skills,
        evo.max_silence_hours,
        sd.enabled,
        sd.window_seconds,
        sd.window_seconds as f64 / 3600.0,
        sd.trigger_threshold,
        sd.action,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

// ── Audit Trail Query handler (W19-P1 M4) ────────────────────
