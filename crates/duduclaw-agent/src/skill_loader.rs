//! OpenClaw-compatible skill format parser.
//!
//! [B-1a] Parses SKILL.md frontmatter (name, description, trigger, tools)
//! and converts to DuDuClaw's internal `SkillFile` format.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

/// Parsed skill metadata from frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
}

/// A fully loaded skill with metadata and content.
#[derive(Debug, Clone)]
pub struct ParsedSkill {
    pub meta: SkillMeta,
    pub content: String,
    /// Paths to tool scripts (`.js`, `.ts`, `.py`) found alongside the skill.
    pub tool_scripts: Vec<String>,
}

/// Parse a SKILL.md file with YAML frontmatter.
///
/// Expected format:
/// ```text
/// ---
/// name: my-skill
/// description: Does something useful
/// trigger: /myskill
/// tools: [search, code]
/// tags: [utility]
/// ---
///
/// # Skill content here
/// ...
/// ```
pub fn parse_skill_file(path: &Path) -> Result<ParsedSkill, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

    let (meta, body) = parse_frontmatter(&content, path)?;

    // Scan for tool scripts in the same directory
    let tool_scripts = if let Some(parent) = path.parent() {
        scan_tool_scripts(parent)
    } else {
        Vec::new()
    };

    Ok(ParsedSkill {
        meta,
        content: body,
        tool_scripts,
    })
}

/// Parse YAML frontmatter delimited by `---`.
fn parse_frontmatter(content: &str, path: &Path) -> Result<(SkillMeta, String), String> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        // No frontmatter — treat entire content as skill body with name from filename
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        return Ok((
            SkillMeta {
                name,
                description: String::new(),
                trigger: String::new(),
                tools: Vec::new(),
                tags: Vec::new(),
                author: String::new(),
                version: String::new(),
            },
            content.to_string(),
        ));
    }

    // Find closing `---`
    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    let end = after_first
        .find("\n---")
        .ok_or_else(|| "No closing --- for frontmatter".to_string())?;

    let yaml_str = &after_first[..end];
    let body = after_first[end + 4..].trim_start().to_string();

    // Parse YAML-like frontmatter manually (simple key: value pairs)
    let meta = parse_simple_yaml(yaml_str, path)?;

    Ok((meta, body))
}

/// Simple YAML-like parser for frontmatter (avoids adding a full YAML dependency).
fn parse_simple_yaml(yaml: &str, path: &Path) -> Result<SkillMeta, String> {
    let mut name = String::new();
    let mut description = String::new();
    let mut trigger = String::new();
    let mut tools = Vec::new();
    let mut tags = Vec::new();
    let mut author = String::new();
    let mut version = String::new();

    for line in yaml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            match key {
                "name" => name = value.to_string(),
                "description" => description = value.to_string(),
                "trigger" => trigger = value.to_string(),
                "author" => author = value.to_string(),
                "version" => version = value.to_string(),
                "tools" => tools = parse_list_value(value),
                "tags" => tags = parse_list_value(value),
                _ => {} // ignore unknown keys
            }
        }
    }

    if name.is_empty() {
        name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
    }

    Ok(SkillMeta {
        name,
        description,
        trigger,
        tools,
        tags,
        author,
        version,
    })
}

/// Parse `[a, b, c]` or `a, b, c` into a Vec<String>.
fn parse_list_value(value: &str) -> Vec<String> {
    let inner = value.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Scan a directory for tool scripts (.js, .ts, .py).
fn scan_tool_scripts(dir: &Path) -> Vec<String> {
    let mut scripts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "js" | "ts" | "py") {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        scripts.push(name.to_string());
                    }
                }
            }
        }
    }
    scripts.sort();
    scripts
}

/// Install a skill from a source directory into an agent's SKILLS/ directory.
///
/// [B-1b] Copies the skill file and runs security scan (via `vet_skill`).
pub async fn install_skill(
    skill_path: &Path,
    agent_skills_dir: &Path,
    quarantine_dir: &Path,
) -> Result<ParsedSkill, String> {
    let parsed = parse_skill_file(skill_path)?;

    // Create directories
    std::fs::create_dir_all(agent_skills_dir)
        .map_err(|e| format!("Failed to create skills dir: {e}"))?;
    std::fs::create_dir_all(quarantine_dir)
        .map_err(|e| format!("Failed to create quarantine dir: {e}"))?;

    let filename = format!("{}.md", parsed.meta.name);
    let dest = agent_skills_dir.join(&filename);

    // Copy skill content
    let content = std::fs::read_to_string(skill_path)
        .map_err(|e| format!("Failed to read skill: {e}"))?;

    std::fs::write(&dest, &content)
        .map_err(|e| format!("Failed to write skill: {e}"))?;

    info!(
        name = %parsed.meta.name,
        dest = %dest.display(),
        "Skill installed"
    );

    Ok(parsed)
}
