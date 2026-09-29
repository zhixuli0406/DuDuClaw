use super::*;

/// Validate that a string is safe to use as a file path component.
/// Prevents path traversal attacks via agent_id or skill_name.
pub(crate) fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains('.')
        && !s.contains('/')
        && !s.contains('\\')
        && !s.contains('\0')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Run a security scan on an agent's installed skill.
pub(crate) async fn handle_skill_security_scan(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let skill_name = params
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_id.is_empty() || skill_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and skill_name are required"}],
            "isError": true
        });
    }

    // Validate inputs to prevent path traversal
    if !is_safe_path_component(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }
    if !is_safe_path_component(skill_name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid skill_name (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }

    // Try agent-local first, then global (read directly to avoid TOCTOU race)
    let agent_path = home_dir
        .join("agents")
        .join(agent_id)
        .join("SKILLS")
        .join(format!("{skill_name}.md"));
    let global_path = home_dir.join("skills").join(format!("{skill_name}.md"));

    let content = match tokio::fs::read_to_string(&agent_path).await {
        Ok(c) => c,
        Err(_) => match tokio::fs::read_to_string(&global_path).await {
            Ok(c) => c,
            Err(_) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: Skill '{skill_name}' not found for agent '{agent_id}'")}],
                    "isError": true
                });
            }
        },
    };

    // Load CONTRACT.toml must_not patterns if available
    let contract_path = home_dir.join("agents").join(agent_id).join("CONTRACT.toml");
    let must_not: Option<Vec<String>> = tokio::fs::read_to_string(&contract_path)
        .await
        .ok()
        .and_then(|c| {
            // Simple extraction of must_not patterns from TOML
            let mut patterns = Vec::new();
            let mut in_must_not = false;
            for line in c.lines() {
                if line.trim().starts_with("must_not") {
                    in_must_not = true;
                    continue;
                }
                if in_must_not {
                    let trimmed = line
                        .trim()
                        .trim_matches(|c: char| c == '"' || c == '\'' || c == ',' || c == ']');
                    if trimmed.is_empty() || trimmed.starts_with('[') {
                        continue;
                    }
                    if line.contains(']') {
                        in_must_not = false;
                    }
                    if !trimmed.is_empty() {
                        patterns.push(trimmed.to_string());
                    }
                }
            }
            if patterns.is_empty() {
                None
            } else {
                Some(patterns)
            }
        });

    use duduclaw_gateway::skill_lifecycle::security_scanner;
    let result = security_scanner::scan_skill(&content, must_not.as_deref());

    // Sprint N P0: emit security_scan audit event (non-blocking, global singleton)
    {
        use duduclaw_gateway::evolution_events::emitter::EvolutionEventEmitter;
        EvolutionEventEmitter::global().emit_security_scan(
            agent_id,
            skill_name,
            result.passed,
            serde_json::json!({
                "risk_level": format!("{:?}", result.risk_level),
                "findings_count": result.findings.len(),
            }),
        );
    }

    let findings_text: Vec<String> = result
        .findings
        .iter()
        .map(|f| {
            format!(
                "- [{:?}] {:?} (line {}): {} [pattern: {}]",
                f.severity,
                f.category,
                f.line_number
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                f.description,
                f.matched_pattern,
            )
        })
        .collect();

    let text = format!(
        "**Security scan: {skill_name}**\n\
         Risk level: {:?}\n\
         Passed: {}\n\
         Findings ({}):\n{}",
        result.risk_level,
        result.passed,
        result.findings.len(),
        if findings_text.is_empty() {
            "  (none)".to_string()
        } else {
            findings_text.join("\n")
        },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Graduate a skill from agent-local to global scope.
pub(crate) async fn handle_skill_graduate(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let skill_name = params
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if agent_id.is_empty() || skill_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and skill_name are required"}],
            "isError": true
        });
    }

    // Validate inputs to prevent path traversal
    if !is_safe_path_component(agent_id) || !is_safe_path_component(skill_name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: invalid agent_id or skill_name (alphanumeric, hyphens, underscores only)"}],
            "isError": true
        });
    }

    let agent_skills_dir = home_dir.join("agents").join(agent_id).join("SKILLS");
    let global_skills_dir = home_dir.join("skills");

    // [H-4] Security scan before graduation to global scope
    let skill_path = agent_skills_dir.join(format!("{skill_name}.md"));
    let content = match tokio::fs::read_to_string(&skill_path).await {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: Failed to read skill: {e}")}],
                "isError": true
            });
        }
    };
    {
        use duduclaw_gateway::skill_lifecycle::security_scanner;
        let scan = security_scanner::scan_skill(&content, None);
        if !scan.passed {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: Security scan failed before graduation (risk: {:?}, {} findings). \
                     Fix the issues or use skill_security_scan for details.",
                    scan.risk_level, scan.findings.len()
                )}],
                "isError": true
            });
        }
    }

    use duduclaw_gateway::skill_lifecycle::graduation;

    let candidate = graduation::GraduationCandidate {
        skill_name: skill_name.to_string(),
        source_agent_id: agent_id.to_string(),
        lift: 0.0, // manual graduation — no lift data
        load_count: 0,
        is_stable: true,
        first_activated: chrono::Utc::now(),
    };

    match graduation::graduate_to_global(&candidate, &agent_skills_dir, &global_skills_dir).await {
        Ok(record) => {
            let home_clone = home_dir.to_path_buf();
            let record_clone = record.clone();
            let _ = tokio::task::spawn_blocking(move || {
                graduation::append_graduation_log(&record_clone, &home_clone);
            })
            .await;
            serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Skill '{skill_name}' graduated from agent '{agent_id}' to global scope.\n\
                     Location: ~/.duduclaw/skills/{skill_name}.md"
                )}]
            })
        }
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: Graduation failed: {e}")}],
            "isError": true
        }),
    }
}

/// Report skill synthesis and sandbox trial status.
pub(crate) async fn handle_skill_synthesis_status(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    // Read recent synthesis events from feedback.jsonl (tail only, max 64KB)
    let feedback_path = home_dir.join("feedback.jsonl");
    let mut synthesis_events = Vec::new();
    let tail_content = {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut buf = String::new();
        if let Ok(mut file) = tokio::fs::File::open(&feedback_path).await {
            let file_len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            const MAX_TAIL: u64 = 64_000;
            if file_len > MAX_TAIL {
                let _ = file.seek(std::io::SeekFrom::End(-(MAX_TAIL as i64))).await;
            }
            let _ = file.read_to_string(&mut buf).await;
        }
        buf
    };
    if !tail_content.is_empty() {
        for line in tail_content.lines().rev().take(50) {
            // H2 (2026-09): the writer moved from `signal_type` to the canonical
            // `type` key; rows written before that fix are still on disk, so
            // both spellings are matched here.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                && val
                    .get("type")
                    .or_else(|| val.get("signal_type"))
                    .and_then(|v| v.as_str())
                    == Some("synthesis_trigger")
                && val.get("agent_id").and_then(|v| v.as_str()) == Some(&agent_name)
            {
                let topic = val.get("topic").and_then(|v| v.as_str()).unwrap_or("?");
                let gaps = val.get("gap_count").and_then(|v| v.as_u64()).unwrap_or(0);
                let err = val
                    .get("avg_composite_error")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                synthesis_events.push(format!("topic: {topic}, gaps: {gaps}, avg_error: {err:.2}"));
            }
        }
    }

    // Read graduation log
    let graduation_records =
        duduclaw_gateway::skill_lifecycle::graduation::load_graduation_log(home_dir);
    let agent_graduations: Vec<_> = graduation_records
        .iter()
        .filter(|r| r.source_agent == agent_name)
        .map(|r| {
            format!(
                "- {} (lift: {:.1}%, at: {})",
                r.skill_name,
                r.lift * 100.0,
                r.graduated_at.format("%Y-%m-%d")
            )
        })
        .collect();

    let text = format!(
        "**Skill Lifecycle Status: {agent_name}**\n\n\
         ## Recent Synthesis Triggers\n{synthesis}\n\n\
         ## Graduated Skills\n{graduated}",
        synthesis = if synthesis_events.is_empty() {
            "  (none)".to_string()
        } else {
            synthesis_events
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
        graduated = if agent_graduations.is_empty() {
            "  (none)".to_string()
        } else {
            agent_graduations.join("\n")
        },
    );

    serde_json::json!({
        "content": [{"type": "text", "text": text}]
    })
}

/// Trigger the Rollout-to-Skill synthesis pipeline (W19-P0).
///
/// Reads `agent_id`, `dry_run`, and `lookback_days` from params.
/// Resolves the Anthropic API key from `ANTHROPIC_API_KEY` env var or
/// the `~/.duduclaw/config.toml` `[api] anthropic_api_key` field.
/// Non-blocking: all errors are captured and returned in the summary.
pub(crate) async fn handle_skill_synthesis_run(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    use duduclaw_gateway::skill_synthesis_pipeline::pipeline::{
        PipelineConfig, run as run_pipeline,
    };

    let agent_id_param = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let target_agent = if agent_id_param.is_empty() {
        default_agent
    } else {
        agent_id_param
    };

    let dry_run = params
        .get("dry_run")
        .and_then(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::String(s) => s.parse::<bool>().ok(),
            _ => None,
        })
        .unwrap_or(true); // Safe default: dry-run

    let lookback_days = params
        .get("lookback_days")
        .and_then(|v| v.as_u64())
        .map(|v| v.min(30) as u32)
        .unwrap_or(1);

    // Resolve API key: env var takes precedence over config file.
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
        .or_else(|| {
            // Fallback: read from config.toml [api] anthropic_api_key
            let config_path = home_dir.join("config.toml");
            std::fs::read_to_string(config_path)
                .ok()
                .and_then(|content| {
                    content
                        .lines()
                        .skip_while(|l| !l.trim().starts_with("[api]"))
                        .find(|l| l.trim().starts_with("anthropic_api_key"))
                        .and_then(|l| l.splitn(2, '=').nth(1))
                        .map(|v| v.trim().trim_matches('"').to_string())
                        .filter(|s| !s.is_empty())
                })
        });

    // Point pipeline at real EvolutionEvents location.
    let events_dir = std::env::var("EVOLUTION_EVENTS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home_dir.join("evolution").join("events"));

    let config = PipelineConfig {
        events_dir,
        lookback_days,
        dry_run,
        api_key,
        home_dir: home_dir.to_path_buf(),
        target_agent_id: target_agent.to_string(),
        ..Default::default()
    };

    let result = run_pipeline(&config).await;

    let mode = if result.dry_run { "DRY RUN" } else { "FULL" };
    let summary = result.summary();

    let detail = format!(
        "## Rollout-to-Skill Pipeline — {mode}\n\n\
         {summary}\n\n\
         **Events scanned:** {events}\n\
         **Trajectory windows:** {traj}\n\
         **Top-20% candidates:** {top}\n\
         **Skills graduated:** {grad}\n\
         **Non-fatal errors:** {errs}",
        events = result.total_events_parsed,
        traj = result.total_trajectories,
        top = result.top_trajectories.len(),
        grad = result.skills_graduated,
        errs = result.errors.len(),
    );

    let error_section = if result.errors.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n**Errors:**\n{}",
            result
                .errors
                .iter()
                .map(|e| format!("- {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("{detail}{error_section}")
        }]
    })
}

/// Resolve the main agent name from the agents directory.
// ── Delegation safety helpers ────────────────────────────────

pub(crate) async fn handle_skill_extract(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return tool_error("Missing required parameter: skill_name"),
    };

    // Validate skill_name to prevent path traversal
    if skill_name.contains("..")
        || skill_name.contains('/')
        || skill_name.contains('\\')
        || skill_name.contains('\0')
        || skill_name.len() > 128
        || !skill_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return tool_error("Invalid skill_name: use alphanumeric, hyphens, underscores only");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    // Check if already extracted
    if duduclaw_gateway::skill_lifecycle::extraction::is_already_extracted(skill_name, &wiki_dir) {
        return tool_text(&format!(
            "Skill '{}' has already been extracted to wiki.",
            skill_name
        ));
    }

    // Find the skill file. Try both layouts (Anthropic spec preferred):
    //   1. <skills>/<skill_name>/SKILL.md   (Anthropic Skills spec)
    //   2. <skills>/<skill_name>.md         (legacy DuDuClaw flat layout)
    //   3. <skills>/<skill_name>            (raw stem already containing .md)
    // W3-3b (a): `agent_id` defaults to the caller — `.ephemeral/` included,
    // and matching the directory `resolve_wiki_dir` just returned.
    let agent_dir = agent_dir_for_id(home_dir, agent_id);
    // `SKILLS` (uppercase) is the directory the loader and every other write
    // path use. The lowercase spelling here only worked because macOS/APFS is
    // case-insensitive by default; on Linux — and on a case-sensitive APFS
    // volume — this missed every skill and reported "not found".
    let skills_dir = agent_dir.join("SKILLS");
    let candidates = [
        skills_dir.join(skill_name).join("SKILL.md"),
        skills_dir.join(format!("{}.md", skill_name)),
        skills_dir.join(skill_name),
    ];
    let skill_content = candidates
        .iter()
        .find(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    if skill_content.is_empty() {
        return tool_error(&format!(
            "Skill '{}' not found under {} (looked for SKILL.md and {}.md forms)",
            skill_name,
            skills_dir.display(),
            skill_name
        ));
    }

    if skill_content.trim().is_empty() {
        return tool_error("Skill file is empty");
    }

    let compressed = duduclaw_gateway::skill_lifecycle::compression::CompressedSkill::compress(
        skill_name,
        &skill_content,
        None,
    );

    let result =
        duduclaw_gateway::skill_lifecycle::extraction::extract_heuristic(&compressed, agent_id);
    let proposals = result.all_proposals();
    let concept_count = result.concepts.len();
    let entity_count = result.entities.len();

    if proposals.is_empty() {
        return tool_text("No extractable knowledge found in skill.");
    }

    // Validate
    if let Err(gradient) = duduclaw_gateway::gvu::verifier::verify_wiki_proposals(&proposals) {
        return tool_error(&format!("Proposals rejected: {}", gradient.critique));
    }

    // Apply
    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    if let Err(e) = store.ensure_scaffold() {
        return tool_error(&format!("Wiki scaffold failed: {e}"));
    }
    match store.apply_proposals(&proposals) {
        Ok(count) => tool_text(&format!(
            "Extracted knowledge from skill '{}':\n- {} concept pages\n- {} entity pages\n- 1 source summary\n- {} total pages written",
            skill_name, concept_count, entity_count, count
        )),
        Err(e) => tool_error(&format!("Failed to apply proposals: {e}")),
    }
}

// ── execute_program handler ─────────────────────────────────────

pub(crate) async fn handle_skill_bank_feedback(args: &Value) -> Value {
    let skill_id = match args.get("skill_id").and_then(|v| v.as_str()) {
        None | Some("") => return tool_error("Missing required parameter: skill_id"),
        Some(id) if id.len() > 128 => return tool_error("skill_id too long (max 128 chars)"),
        Some(id) => id.to_string(),
    };
    let success = match args.get("success") {
        Some(v) if v.is_boolean() => v.as_bool().unwrap(),
        Some(v) if v.is_string() => v.as_str().unwrap().eq_ignore_ascii_case("true"),
        _ => return tool_error("Missing required parameter: success (true/false)"),
    };

    tracing::info!(skill_id, success, "skill_bank_feedback called");

    // Bayesian confidence update (inline — no external dependency)
    // P(skill_works | evidence) using Beta-Bernoulli conjugate prior
    let prior = 0.5_f64;
    let likelihood = if success { 0.9 } else { 0.1 };
    let marginal = likelihood * prior + (1.0 - likelihood) * (1.0 - prior);
    let posterior = (likelihood * prior) / marginal;

    serde_json::json!({
        "content": [{ "type": "text", "text": serde_json::json!({
            "skill_id": skill_id,
            "success": success,
            "prior_confidence": format!("{:.0}%", prior * 100.0),
            "new_confidence": format!("{:.0}%", posterior * 100.0),
            "note": "Bayesian confidence updated. Full SkillBank persistence pending.",
        }).to_string() }],
    })
}

// ── session_restore_context handler ─────────────────────────────
