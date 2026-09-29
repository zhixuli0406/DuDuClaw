//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Shared Skills handlers ──────────────────────────────

    pub(crate) async fn handle_skills_shared_list(&self) -> WsFrame {
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
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let content = tokio::fs::read_to_string(&path).await.unwrap_or_default();
                // Parse frontmatter for metadata
                let description = extract_frontmatter(&content, "description").unwrap_or_default();
                let shared_by = extract_frontmatter(&content, "shared_by").unwrap_or_default();
                let shared_at = extract_frontmatter(&content, "shared_at").unwrap_or_default();
                let tags: Vec<String> = extract_frontmatter(&content, "tags")
                    .map(|t| {
                        t.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let adopted_by: Vec<String> = extract_frontmatter(&content, "adopted_by")
                    .map(|t| {
                        t.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
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

    pub(crate) async fn handle_skills_share(&self, params: Value) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let skill_name = params
            .get("skill_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if agent_id.is_empty() || skill_name.is_empty() {
            return WsFrame::error_response("", "agent_id and skill_name are required");
        }
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Invalid agent_id");
        }
        if !is_valid_skill_name(skill_name) {
            return WsFrame::error_response("", "Invalid skill_name");
        }
        // The my-skills list (`skills.list`) unions the agent's SKILLS dir with
        // the global `<home>/skills` dir, so share must resolve both: agent
        // copy wins, global copy is the fallback.
        let agent_path = self
            .home_dir
            .join("agents")
            .join(agent_id)
            .join("SKILLS")
            .join(format!("{skill_name}.md"));
        let global_path = self
            .home_dir
            .join("skills")
            .join(format!("{skill_name}.md"));
        let skill_path = if agent_path.exists() {
            agent_path
        } else if global_path.exists() {
            global_path
        } else {
            return WsFrame::error_response(
                "",
                &format!("Skill not found: {skill_name} in agent {agent_id}"),
            );
        };
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

    pub(crate) async fn handle_skills_adopt(&self, params: Value) -> WsFrame {
        let skill_name = params
            .get("skill_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let target_agent = params
            .get("target_agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if skill_name.is_empty() || target_agent.is_empty() {
            return WsFrame::error_response("", "skill_name and target_agent_id are required");
        }
        // XC.4: validate target_agent_id (mirror other agent-targeting handlers)
        // — prevents path traversal / writing outside the agents tree.
        if !is_valid_agent_id(target_agent) {
            return WsFrame::error_response("", "Invalid target_agent_id format");
        }
        // skill_name is used in a filename — restrict to a safe charset.
        if !skill_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            || skill_name.len() > 128
        {
            return WsFrame::error_response(
                "",
                "Invalid skill_name (alphanumeric, _, -; ≤128 chars)",
            );
        }
        // Read from shared
        let shared_path = self
            .home_dir
            .join("shared")
            .join("skills")
            .join(format!("{skill_name}.md"));
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
            let mut agents: Vec<&str> = old
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if !agents.contains(&target_agent) {
                agents.push(target_agent);
            }
            agents.join(", ")
        });
        let _ = tokio::fs::write(&shared_path, &updated).await;

        WsFrame::ok_response("", json!({ "success": true }))
    }
}
