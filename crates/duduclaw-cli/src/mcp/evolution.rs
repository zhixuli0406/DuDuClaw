use super::*;

pub(crate) async fn handle_evolution_toggle(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    if let Err(reason) = check_actor_identity(home_dir, actor, "", "evolution_toggle") {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }
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

    // Gate before reading the target file. Self-toggle is allowed: these four
    // fields grant no tool, data, org or spend authority, and the evolution
    // master switch still applies.
    if let Some(caller) = actor.agent() {
        if let Err(reason) = check_org_subject_allowed(
            home_dir,
            caller,
            agent_id,
            "調整 AI 員工演化設定",
        )
        .await
        {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {reason}")}],
                "isError": true
            });
        }
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

    // Write back through the gateway's profile file writer: only
    // `[evolution].<field>` changes (format kept), under the shared lock, with
    // a version and the `agent_file_changed` audit row. The value validated
    // above is re-applied to the file as read under the lock.
    let value = doc["evolution"][field].clone();
    if let Err(e) = write_evolution_toggle(home_dir, agent_id, actor, field, value) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Evolution config updated: {agent_id}.evolution.{field} = {value_str}\n\
             A running gateway may only pick it up after its next reload of the employee list (any dashboard save) or a restart."
        )}]
    })
}

/// Kept separate so tests can revoke authority after the front gate and
/// before this same production write path acquires the file lock.
pub(super) fn write_evolution_toggle(
    home_dir: &Path,
    agent_id: &str,
    actor: RecordActor<'_>,
    field: &str,
    value: toml::Value,
) -> std::result::Result<(), String> {
    // The background admission is taken in the writer's final guard and held
    // through commit, so a pause cannot land between the check and the write.
    let mut admission = None;
    let written = duduclaw_gateway::agent_files::external::edit_agent_toml_guarded(
        home_dir,
        agent_id,
        "mcp",
        actor.id(),
        |t| {
            // This closure runs synchronously under the shared agent.toml
            // lock. Re-read authority after waiting for it, before mutation.
            check_evolution_subject_locked(home_dir, actor, agent_id)?;
            let evo = t
                .entry("evolution")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or("[evolution] is not a table")?;
            evo.insert(field.to_string(), value);
            Ok(())
        },
        || {
            admission = admit_background_identity_blocking(home_dir, "", "evolution_toggle")?;
            Ok(())
        },
        false,
    )
    .map(|_| ());
    drop(admission);
    written
}

fn check_evolution_subject_locked(
    home_dir: &Path,
    actor: RecordActor<'_>,
    agent_id: &str,
) -> std::result::Result<(), String> {
    check_actor_identity(home_dir, actor, agent_id, "evolution_toggle")?;
    let Some(caller) = actor.agent() else {
        return Ok(());
    };
    let policy = duduclaw_core::delegation_policy_from_home(home_dir);
    let caller = caller.trim();
    if policy == duduclaw_core::DelegationPolicy::Open || caller == agent_id {
        return Ok(());
    }

    // Synchronous equivalent of org_snapshot: authoritative store first,
    // typed agent.toml fallback only for absent entries, bounded ancestor walk.
    // Do not await while the writer holds the cross-process file lock.
    let store = duduclaw_core::org_store::load(home_dir);
    let mut org = duduclaw_core::MapOrgView::new();
    let mut seen = std::collections::HashSet::new();
    for start in [caller, agent_id] {
        let mut current = start.to_string();
        for _ in 0..=duduclaw_core::MAX_ANCESTOR_HOPS {
            if current.is_empty() || !seen.insert(current.clone()) {
                break;
            }
            let (parent, department) = match store.get(&current) {
                Some(entry) => (entry.reports_to.clone(), entry.department.clone()),
                None => {
                    let path = home_dir.join("agents").join(&current).join("agent.toml");
                    let config = std::fs::read_to_string(path)
                        .ok()
                        .and_then(|text| toml::from_str::<duduclaw_core::types::AgentConfig>(&text).ok());
                    let Some(config) = config else { break };
                    (
                        normalize_reports_to(config.agent.reports_to.trim()).to_string(),
                        config.agent.department.trim().to_string(),
                    )
                }
            };
            org.insert(&current, &parent, &department);
            current = parent;
        }
    }
    if duduclaw_core::is_org_ancestor(&org, caller, agent_id) {
        return Ok(());
    }
    audit_org_placement_denied(home_dir, caller, agent_id, policy, "調整 AI 員工演化設定");
    Err(format!(
        "調整 AI 員工演化設定遭拒：「{}」不在你的團隊之內（委派政策：{}），未做任何變更。",
        duduclaw_core::truncate_chars(agent_id, 64),
        policy.label_zh()
    ))
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
