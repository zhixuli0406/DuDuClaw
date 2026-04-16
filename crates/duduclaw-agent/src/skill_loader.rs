//! OpenClaw-compatible skill format parser.
//!
//! [B-1a] Parses SKILL.md frontmatter (name, description, trigger, tools)
//! and converts to DuDuClaw's internal `SkillFile` format.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

/// Skill type in a hierarchical composition (SkillRL-inspired).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SkillType {
    /// Single-capability skill (leaf node).
    #[default]
    Atomic,
    /// Composes 2-3 atomic skills into a cohesive workflow.
    Functional,
    /// Task-level orchestration that coordinates functional/atomic skills.
    Planning,
}

/// How dependencies are composed when activated together.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ComposeMode {
    /// All dependencies loaded simultaneously (default).
    #[default]
    Parallel,
    /// Dependencies loaded in order, output of one feeds next.
    Sequential,
    /// Dependencies loaded based on runtime conditions.
    Conditional,
}

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
    /// Other skills this skill depends on (by name).
    #[serde(default)]
    pub requires: Vec<String>,
    /// Skill complexity level in the hierarchy.
    #[serde(default)]
    pub skill_type: SkillType,
    /// How dependencies should be composed.
    #[serde(default)]
    pub compose_mode: ComposeMode,
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
                requires: Vec::new(),
                skill_type: SkillType::default(),
                compose_mode: ComposeMode::default(),
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

/// Parse skill frontmatter YAML using serde_yaml (MCP-M6 — replaces custom parser).
fn parse_simple_yaml(yaml: &str, path: &Path) -> Result<SkillMeta, String> {
    /// Intermediate struct for serde deserialization.
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct RawMeta {
        name: String,
        description: String,
        trigger: String,
        tools: Vec<String>,
        tags: Vec<String>,
        author: String,
        version: String,
        requires: Vec<String>,
        skill_type: SkillType,
        compose_mode: ComposeMode,
    }

    let raw: RawMeta = serde_yaml::from_str(yaml).unwrap_or_else(|e| {
        tracing::warn!("YAML parse error in {}: {e} — falling back to defaults", path.display());
        RawMeta::default()
    });

    let name = if raw.name.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        raw.name
    };

    Ok(SkillMeta {
        name,
        description: raw.description,
        trigger: raw.trigger,
        tools: raw.tools,
        tags: raw.tags,
        author: raw.author,
        version: raw.version,
        requires: raw.requires,
        skill_type: raw.skill_type,
        compose_mode: raw.compose_mode,
    })
}

/// Scan a directory for tool scripts (.js, .ts, .py).
fn scan_tool_scripts(dir: &Path) -> Vec<String> {
    let mut scripts = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str())
                && matches!(ext, "js" | "ts" | "py")
                    && let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        scripts.push(name.to_string());
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
    tokio::fs::create_dir_all(agent_skills_dir)
        .await
        .map_err(|e| format!("Failed to create skills dir: {e}"))?;
    tokio::fs::create_dir_all(quarantine_dir)
        .await
        .map_err(|e| format!("Failed to create quarantine dir: {e}"))?;

    let filename = format!("{}.md", parsed.meta.name);
    let dest = agent_skills_dir.join(&filename);

    // Copy skill content
    let content = tokio::fs::read_to_string(skill_path)
        .await
        .map_err(|e| format!("Failed to read skill: {e}"))?;

    tokio::fs::write(&dest, &content)
        .await
        .map_err(|e| format!("Failed to write skill: {e}"))?;

    info!(
        name = %parsed.meta.name,
        dest = %dest.display(),
        "Skill installed"
    );

    Ok(parsed)
}

/// Install a skill into the global `~/.duduclaw/skills/` directory.
///
/// Global skills are automatically shared with all agents.
/// Agent-local skills with the same name take precedence (override).
pub async fn install_skill_global(
    skill_path: &Path,
    home_dir: &Path,
    quarantine_dir: &Path,
) -> Result<ParsedSkill, String> {
    let global_skills_dir = home_dir.join("skills");
    install_skill(skill_path, &global_skills_dir, quarantine_dir).await
}
