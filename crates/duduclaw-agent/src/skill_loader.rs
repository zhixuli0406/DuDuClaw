//! OpenClaw-compatible skill format parser.
//!
//! [B-1a] Parses SKILL.md frontmatter (name, description, trigger, tools)
//! and converts to DuDuClaw's internal `SkillFile` format.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

/// Localised display strings for a skill (WP8 — Skill 中文化). Employees who
/// don't read English see the skill's zh-TW name/description; the original
/// `name`/`description` remain the machine-stable identity.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalizedText {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

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
    /// Localised display strings keyed by locale (`zh-TW` / `en` / `ja-JP`).
    /// Empty for skills predating WP8 — the fallback chain then returns the
    /// original `name` / `description`.
    #[serde(default)]
    pub display: HashMap<String, LocalizedText>,
    /// Operator/agent estimate of minutes this skill saves per use. Populated
    /// by the WP8 time-saving approval flow; the WP10 leaderboard aggregates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_minutes_saved: Option<u32>,
}

/// Default UI locale used by the skill display fallback chain.
pub const DEFAULT_SKILL_LOCALE: &str = "zh-TW";

impl SkillMeta {
    /// Display name for `locale`, following the fallback chain
    /// `locale → zh-TW → original name`. Blank localised entries are skipped.
    pub fn display_name(&self, locale: &str) -> &str {
        self.localized_name(locale)
            .or_else(|| self.localized_name(DEFAULT_SKILL_LOCALE))
            .unwrap_or(&self.name)
    }

    /// Display description for `locale`, same fallback chain as [`Self::display_name`].
    pub fn display_description(&self, locale: &str) -> &str {
        self.localized_description(locale)
            .or_else(|| self.localized_description(DEFAULT_SKILL_LOCALE))
            .unwrap_or(&self.description)
    }

    fn localized_name(&self, locale: &str) -> Option<&str> {
        self.display
            .get(locale)
            .map(|t| t.name.trim())
            .filter(|s| !s.is_empty())
    }

    fn localized_description(&self, locale: &str) -> Option<&str> {
        self.display
            .get(locale)
            .map(|t| t.description.trim())
            .filter(|s| !s.is_empty())
    }
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

/// Parse skill metadata from an in-memory SKILL.md string (no filesystem
/// round-trip). Used by presentation paths (`skill_list` / `skill_search`) that
/// already hold the content and need the WP8 `display` map for localisation.
/// `name_hint` fills the name when frontmatter is absent/blank.
pub fn parse_skill_meta_from_content(content: &str, name_hint: &str) -> SkillMeta {
    let path = std::path::PathBuf::from(format!("{name_hint}.md"));
    match parse_frontmatter(content, &path) {
        Ok((mut meta, _)) => {
            if meta.name.trim().is_empty() {
                meta.name = name_hint.to_string();
            }
            meta
        }
        Err(_) => SkillMeta {
            name: name_hint.to_string(),
            description: String::new(),
            trigger: String::new(),
            tools: Vec::new(),
            tags: Vec::new(),
            author: String::new(),
            version: String::new(),
            requires: Vec::new(),
            skill_type: SkillType::default(),
            compose_mode: ComposeMode::default(),
            display: HashMap::new(),
            estimated_minutes_saved: None,
        },
    }
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
                display: HashMap::new(),
                estimated_minutes_saved: None,
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
        display: HashMap<String, LocalizedText>,
        estimated_minutes_saved: Option<u32>,
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
        display: raw.display,
        estimated_minutes_saved: raw.estimated_minutes_saved,
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

/// Validate that a (frontmatter-derived) skill name is a single safe path
/// component. Fail-closed at the install sink: a hostile SKILL.md with
/// `name: ../../agents/<victim>/SOUL` must never escape the skills dir —
/// this guards every install path (hub, dashboard vet, graduate) at once.
///
/// Rejected: empty, > 128 bytes, `/`, `\`, NUL, any `..` sequence, and
/// leading dots (covers `.`, `..`, and hidden-file smuggling).
pub fn is_safe_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.starts_with('.')
        && !name.contains("..")
}

/// Install a skill from a source directory into an agent's SKILLS/ directory.
///
/// [B-1b] Copies the skill file into the target skills directory.
///
/// **Security contract**: this function does NOT scan the content itself (the
/// Rust-native scanner lives in `duduclaw-gateway::skill_lifecycle`, which this
/// crate cannot depend on). Callers MUST gate the content through
/// `security_scanner::scan_skill` *before* calling install — the gateway's
/// hub install path (`skill_lifecycle::hub_install`) and `skill_graduate` do;
/// a caller that skips the scan ships unvetted content into a loader scan root.
///
/// The destination filename derives from frontmatter `meta.name`, which is
/// attacker-controlled DATA — [`is_safe_skill_name`] gates it here at the
/// sink (path traversal in `name:` is rejected regardless of caller).
pub async fn install_skill(
    skill_path: &Path,
    agent_skills_dir: &Path,
    quarantine_dir: &Path,
) -> Result<ParsedSkill, String> {
    let parsed = parse_skill_file(skill_path)?;

    if !is_safe_skill_name(&parsed.meta.name) {
        return Err(format!(
            "install DENIED: skill name '{}' is not a safe path component \
             (no '/', '\\', '..', NUL, or leading dots; max 128 bytes)",
            parsed.meta.name.escape_debug()
        ));
    }

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

#[cfg(test)]
mod display_tests {
    use super::*;

    fn meta_with(display: &[(&str, &str, &str)]) -> SkillMeta {
        let mut map = HashMap::new();
        for (loc, name, desc) in display {
            map.insert(
                loc.to_string(),
                LocalizedText { name: name.to_string(), description: desc.to_string() },
            );
        }
        SkillMeta {
            name: "compress-context".into(),
            description: "Compress the conversation".into(),
            trigger: String::new(),
            tools: Vec::new(),
            tags: Vec::new(),
            author: String::new(),
            version: String::new(),
            requires: Vec::new(),
            skill_type: SkillType::default(),
            compose_mode: ComposeMode::default(),
            display: map,
            estimated_minutes_saved: None,
        }
    }

    #[test]
    fn prefers_requested_locale() {
        let m = meta_with(&[("zh-TW", "壓縮對話", "把對話壓縮"), ("en", "Compress", "Compress it")]);
        assert_eq!(m.display_name("en"), "Compress");
        assert_eq!(m.display_description("zh-TW"), "把對話壓縮");
    }

    #[test]
    fn falls_back_to_zh_tw_then_original() {
        let m = meta_with(&[("zh-TW", "壓縮對話", "把對話壓縮")]);
        // ja-JP absent → zh-TW.
        assert_eq!(m.display_name("ja-JP"), "壓縮對話");
        // No display at all → original name.
        let bare = meta_with(&[]);
        assert_eq!(bare.display_name("zh-TW"), "compress-context");
        assert_eq!(bare.display_description("en"), "Compress the conversation");
    }

    #[test]
    fn blank_localized_entry_is_skipped() {
        let m = meta_with(&[("zh-TW", "  ", "  ")]);
        // Blank zh-TW entry must not shadow the original.
        assert_eq!(m.display_name("zh-TW"), "compress-context");
    }
}

#[cfg(test)]
mod install_safety_tests {
    use super::*;

    #[test]
    fn safe_skill_name_rejects_traversal_shapes() {
        // The CRITICAL fix: frontmatter names must be single path components.
        for bad in [
            "",
            "../../agents/victim/SOUL",
            "..",
            ".",
            ".hidden",
            "a/b",
            "a\\b",
            "a..b",
            "nul\0byte",
        ] {
            assert!(!is_safe_skill_name(bad), "must reject {bad:?}");
        }
        for good in ["notes", "compress-context", "skill_v2", "技能筆記"] {
            assert!(is_safe_skill_name(good), "must accept {good:?}");
        }
        // Length cap.
        assert!(!is_safe_skill_name(&"a".repeat(129)));
        assert!(is_safe_skill_name(&"a".repeat(128)));
    }

    #[tokio::test]
    async fn install_denies_traversal_name_and_accepts_normal() {
        let home = std::env::temp_dir().join(format!("duduclaw-loader-{}", uuid::Uuid::new_v4()));
        let src_dir = home.join("src");
        let skills_dir = home.join("agents").join("a1").join("SKILLS");
        let quarantine = home.join("quarantine");
        std::fs::create_dir_all(&src_dir).unwrap();
        // Victim file a traversal name would try to overwrite.
        let victim = home.join("agents").join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("SOUL.md"), "original soul").unwrap();

        // Hostile frontmatter name escaping the skills dir.
        let evil = src_dir.join("evil.md");
        std::fs::write(
            &evil,
            "---\nname: ../../victim/SOUL\n---\npwned content",
        )
        .unwrap();
        let err = install_skill(&evil, &skills_dir, &quarantine)
            .await
            .unwrap_err();
        assert!(err.contains("DENIED"), "{err}");
        assert_eq!(
            std::fs::read_to_string(victim.join("SOUL.md")).unwrap(),
            "original soul",
            "victim SOUL.md must be untouched"
        );

        // Normal install still works (existing behavior unaffected).
        let good = src_dir.join("good.md");
        std::fs::write(&good, "---\nname: notes\n---\nbody").unwrap();
        let parsed = install_skill(&good, &skills_dir, &quarantine).await.unwrap();
        assert_eq!(parsed.meta.name, "notes");
        assert!(skills_dir.join("notes.md").is_file());

        let _ = std::fs::remove_dir_all(&home);
    }
}
