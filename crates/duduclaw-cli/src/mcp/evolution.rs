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

    // v1.68: `stagnation_*`, `skill_auto_activate` and `skill_security_scan`
    // had no reader (the live stagnation detector reads `gvu_stagnation_*`;
    // the skill scanner always runs) and are no longer writable here.
    if field.starts_with("stagnation_") || field == "skill_auto_activate" || field == "skill_security_scan" {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "{field} is no longer configurable — nothing read it. Nothing was changed."
            )}]
        });
    }

    // Validate field name and apply to the correct TOML section.
    let boolean_fields = ["gvu_enabled"];
    let numeric_fields = [
        "max_silence_hours",
        "skill_token_budget",
        "max_active_skills",
    ];
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
    } else {
        let all_fields: Vec<&str> = boolean_fields
            .iter()
            .chain(numeric_fields.iter())
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
    let status = format!(
        "Evolution status for agent '{agent_id}':\n\
         \n\
         GVU self-play:     {}\n\
         Cognitive memory:  always on\n\
         \n\
         Skill token budget:   {}\n\
         Max active skills:    {}\n\
         \n\
         Max silence hours:         {:.1}",
        evo.gvu_enabled,
        evo.skill_token_budget,
        evo.max_active_skills,
        evo.max_silence_hours,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": status}]
    })
}

// ── Audit Trail Query handler (W19-P1 M4) ────────────────────
