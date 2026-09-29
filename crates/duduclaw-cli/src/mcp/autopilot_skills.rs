use super::*;

pub(crate) async fn handle_autopilot_list(args: &Value, home_dir: &Path) -> Value {
    let enabled_only = args
        .get("enabled_only")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let store = match duduclaw_gateway::autopilot_store::AutopilotStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("open autopilot store: {e}")),
    };
    let rules = match store.list_rules().await {
        Ok(r) => r,
        Err(e) => return tool_error(&format!("list rules: {e}")),
    };
    let items: Vec<Value> = rules
        .iter()
        .filter(|r| !enabled_only || r.enabled)
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "enabled": r.enabled,
                "trigger_event": r.trigger_event,
                "conditions": serde_json::from_str::<Value>(&r.conditions).unwrap_or(Value::Null),
                "action": serde_json::from_str::<Value>(&r.action).unwrap_or(Value::Null),
                "created_at": r.created_at,
                "last_triggered_at": r.last_triggered_at,
                "trigger_count": r.trigger_count,
                // P3-3: present only for CEP sequence rules; null for ordinary rules.
                "sequence": r.sequence.as_ref().and_then(|s| serde_json::from_str::<Value>(s).ok()),
            })
        })
        .collect();
    tool_text(&serde_json::json!({ "rules": items }).to_string())
}

pub(crate) async fn handle_shared_skill_list(args: &Value, home_dir: &Path) -> Value {
    let tag_filter = args
        .get("tag")
        .and_then(|v| v.as_str())
        .map(str::to_lowercase);
    let shared_dir = home_dir.join("shared").join("skills");
    if !shared_dir.exists() {
        return tool_text(&serde_json::json!({ "skills": [] }).to_string());
    }
    let mut skills: Vec<Value> = Vec::new();
    let mut entries = match tokio::fs::read_dir(&shared_dir).await {
        Ok(e) => e,
        Err(e) => return tool_error(&format!("read shared skills dir: {e}")),
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
        let tags_raw = extract_frontmatter(&content, "tags").unwrap_or_default();
        let tags: Vec<String> = tags_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(ref needle) = tag_filter {
            if !tags.iter().any(|t| t.to_lowercase().contains(needle)) {
                continue;
            }
        }
        let description = extract_frontmatter(&content, "description").unwrap_or_default();
        let shared_by = extract_frontmatter(&content, "shared_by").unwrap_or_default();
        let shared_at = extract_frontmatter(&content, "shared_at").unwrap_or_default();
        let usage_count: i64 = extract_frontmatter(&content, "usage_count")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let adopted_by: Vec<String> = extract_frontmatter(&content, "adopted_by")
            .map(|s| {
                s.split(',')
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        skills.push(serde_json::json!({
            "name": name,
            "description": description,
            "shared_by": shared_by,
            "shared_at": shared_at,
            "tags": tags,
            "usage_count": usage_count,
            "adopted_by": adopted_by,
        }));
    }
    tool_text(&serde_json::json!({ "skills": skills }).to_string())
}

pub(crate) async fn handle_shared_skill_share(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let skill_name = args
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if skill_name.is_empty() {
        return tool_error("skill_name is required");
    }
    if !is_valid_agent_id(default_agent) {
        return tool_error("invalid caller agent id");
    }
    // W3-3b (a): the sharer's own SKILLS/ — `.ephemeral/` included.
    let skill_path = caller_agent_dir(home_dir, default_agent)
        .join("SKILLS")
        .join(format!("{skill_name}.md"));
    if !skill_path.exists() {
        return tool_error(&format!("skill not found in your SKILLS/: {skill_name}"));
    }
    let content = match tokio::fs::read_to_string(&skill_path).await {
        Ok(c) => c,
        Err(e) => return tool_error(&format!("read skill: {e}")),
    };
    let shared_dir = home_dir.join("shared").join("skills");
    if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
        return tool_error(&format!("create shared dir: {e}"));
    }
    let shared_path = shared_dir.join(format!("{skill_name}.md"));
    let now = chrono::Utc::now().to_rfc3339();
    let shared_content = format!(
        "---\nshared_by: {default_agent}\nshared_at: {now}\ndescription: \ntags: \nadopted_by: \nusage_count: 0\n---\n\n{content}"
    );
    if let Err(e) = tokio::fs::write(&shared_path, &shared_content).await {
        return tool_error(&format!("write shared skill: {e}"));
    }
    tool_text(&serde_json::json!({ "success": true, "skill": skill_name }).to_string())
}

pub(crate) async fn handle_shared_skill_adopt(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let skill_name = args
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if skill_name.is_empty() {
        return tool_error("skill_name is required");
    }
    let target_agent = args
        .get("target_agent")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(target_agent) {
        return tool_error("invalid target_agent id");
    }
    let shared_path = home_dir
        .join("shared")
        .join("skills")
        .join(format!("{skill_name}.md"));
    if !shared_path.exists() {
        return tool_error(&format!("shared skill not found: {skill_name}"));
    }
    let content = match tokio::fs::read_to_string(&shared_path).await {
        Ok(c) => c,
        Err(e) => return tool_error(&format!("read shared skill: {e}")),
    };
    // Strip frontmatter (up to second "---")
    let skill_content = strip_frontmatter(&content);

    let target_dir = home_dir.join("agents").join(target_agent).join("SKILLS");
    if let Err(e) = tokio::fs::create_dir_all(&target_dir).await {
        return tool_error(&format!("create agent SKILLS dir: {e}"));
    }
    let target_path = target_dir.join(format!("{skill_name}.md"));
    if let Err(e) = tokio::fs::write(&target_path, &skill_content).await {
        return tool_error(&format!("write skill to agent: {e}"));
    }

    // Bump usage_count and adopted_by in shared frontmatter
    let updated = update_frontmatter_field(&content, "usage_count", |old| {
        let n: i64 = old.parse().unwrap_or(0);
        (n + 1).to_string()
    });
    let updated = update_frontmatter_field(&updated, "adopted_by", |old| {
        let mut agents: Vec<String> = old
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !agents.iter().any(|a| a == target_agent) {
            agents.push(target_agent.to_string());
        }
        agents.join(", ")
    });
    let _ = tokio::fs::write(&shared_path, &updated).await;

    tool_text(
        &serde_json::json!({
            "success": true,
            "skill": skill_name,
            "adopted_to": target_agent,
        })
        .to_string(),
    )
}

/// Extract a top-level YAML frontmatter field value.
/// Scans only within the first `---` fenced block.
pub(crate) fn extract_frontmatter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut in_front = false;
    for (i, line) in content.lines().enumerate() {
        if i == 0 && line.trim() == "---" {
            in_front = true;
            continue;
        }
        if in_front && line.trim() == "---" {
            break;
        }
        if in_front {
            if let Some(rest) = line.strip_prefix(&prefix) {
                return Some(rest.trim().to_string());
            }
        }
    }
    None
}

/// Rewrite a single top-level frontmatter field using `transform`.
pub(crate) fn update_frontmatter_field(
    content: &str,
    key: &str,
    transform: impl Fn(&str) -> String,
) -> String {
    let prefix = format!("{key}:");
    let mut in_front = false;
    content
        .lines()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 && line.trim() == "---" {
                in_front = true;
                return line.to_string();
            }
            if in_front && line.trim() == "---" {
                in_front = false;
                return line.to_string();
            }
            if in_front {
                if let Some(rest) = line.strip_prefix(&prefix) {
                    return format!("{prefix} {}", transform(rest.trim()));
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip the leading `---...---` YAML frontmatter block (if any) and
/// return the body, trimmed of leading whitespace.
pub(crate) fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return trimmed.to_string();
    }
    // After the opening ---, find the next line that is exactly "---".
    // Collect lines after that as the body.
    let mut saw_open = false;
    let mut body_lines: Vec<&str> = Vec::new();
    let mut collecting = false;
    for line in trimmed.lines() {
        if collecting {
            body_lines.push(line);
            continue;
        }
        if !saw_open {
            if line.trim() == "---" {
                saw_open = true;
            }
            continue;
        }
        // saw_open && !collecting
        if line.trim() == "---" {
            collecting = true;
        }
    }
    if collecting {
        body_lines.join("\n").trim_start().to_string()
    } else {
        trimmed.to_string()
    }
}

/// Append an event to the SQLite event bus (`~/.duduclaw/events.db`).
///
/// Replaces the legacy `events.jsonl` file bus (removed in v1.8.28).
/// Row inserts are atomic under SQLite WAL with a 5-second
/// `busy_timeout`, so concurrent writers from multiple MCP subprocesses
/// and the gateway reader stay consistent without file-bus hazards
/// (rotation races, partial writes, permission concerns, or unbounded
/// growth — the gateway prunes old rows on a schedule).
///
/// Best-effort: failures are logged but never fatal — the caller has
/// already persisted the authoritative row in `tasks.db` / `activity`.
pub(crate) async fn append_bus_event(home_dir: &Path, event: &str, payload: &Value) {
    let bus = match duduclaw_gateway::events_store::EventBusStore::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "open events.db");
            return;
        }
    };
    let payload_str = payload.to_string();
    if let Err(e) = bus.append(event, &payload_str).await {
        warn!(error = %e, event = %event, "append events.db");
    }
}
