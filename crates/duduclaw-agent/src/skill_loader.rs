//! OpenClaw-compatible skill format parser.
//!
//! [B-1a] Parses SKILL.md frontmatter (name, description, trigger, tools)
//! and converts to DuDuClaw's internal `SkillFile` format.

pub mod safe_dir;

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

/// The frontmatter fields as the loader reads them.
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

/// Parse skill frontmatter YAML using serde_yaml (MCP-M6 — replaces custom parser).
fn parse_simple_yaml(yaml: &str, path: &Path) -> Result<SkillMeta, String> {
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

/// Strict check of a SKILL.md's frontmatter for a person editing it: when
/// the text starts with `---`, the block must be closed, its YAML must be a
/// mapping, and every field the loader knows must have the right type (other
/// keys are allowed; the loader itself falls back to empty metadata on any
/// error, which would silently drop the skill's name and description). A
/// `name`, when given, must be a single safe path component. Text without
/// frontmatter is fine.
pub fn check_frontmatter(content: &str) -> Result<(), String> {
    let Some(rest) = content.trim_start().strip_prefix("---") else {
        return Ok(());
    };
    let after_first = rest.trim_start_matches(['\r', '\n']);
    let end = after_first
        .find("\n---")
        .ok_or_else(|| "the opening --- has no closing ---".to_string())?;
    let yaml = after_first.get(..end).unwrap_or("");
    if yaml.trim().is_empty() {
        return Ok(());
    }
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|e| format!("YAML: {e}"))?;
    if !value.is_mapping() {
        return Err("the block must be a list of `key: value` lines".into());
    }
    let raw: RawMeta = serde_yaml::from_value(value).map_err(|e| format!("YAML: {e}"))?;
    if !raw.name.is_empty() && !is_safe_skill_name(&raw.name) {
        return Err("`name` must be a single name without `/`, `..` or a leading dot".into());
    }
    Ok(())
}

/// Resolve the actual install filename using the loader parser, never presentation defaults.
pub fn install_skill_name(content: &str, name_hint: &str) -> Result<String, String> {
    if !is_safe_skill_name(name_hint) {
        return Err("install DENIED: unsafe skill name hint".into());
    }
    check_frontmatter(content).map_err(|error| format!("install DENIED: {error}"))?;
    let path = std::path::PathBuf::from(format!("{name_hint}.md"));
    let (meta, _) = parse_frontmatter(content, &path)?;
    if !is_safe_skill_name(&meta.name) {
        return Err("install DENIED: unsafe installed skill name".into());
    }
    Ok(meta.name)
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
    install_skill_at(skill_path, agent_skills_dir, quarantine_dir, SkillsDir::CreateAll).await
}

/// How the destination skills directory may be created.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SkillsDir {
    /// Org-wide layers (global / department): every missing ancestor.
    CreateAll,
    /// SR5 round 3 N8: only the final `SKILLS/` component, inside an
    /// already-existing real directory; never an ancestor.
    LeafOnly,
}

/// Refusal when the checked employee tree is gone (tombstoned, moved or
/// replaced) between the live check and the write.
pub const EMPLOYEE_TREE_GONE: &str =
    "install DENIED: the employee directory is no longer present; nothing was installed";

/// SR5 round 3 N8: install into a live employee tree the caller has ALREADY
/// checked (`agents/<label>`, e.g. the gateway's `live_employee_dir`). Only
/// `<employee_dir>/SKILLS` is created; `employee_dir` itself is never
/// created, so a tree moved away by a concurrent tombstone stays gone (the
/// install fails instead of conjuring `agents/<label>`). A symlinked or
/// non-directory `employee_dir` / `SKILLS` refuses.
///
/// SR5 round 3 follow-up R3-2: the write is descriptor-relative and never
/// follows a link (Unix, via [`safe_dir::SafeDir`]): `agents/<label>` is
/// opened `O_DIRECTORY|O_NOFOLLOW` (every ancestor too), `SKILLS` is
/// `mkdirat`/`openat`ed with `O_NOFOLLOW`, the content goes to a fresh temp
/// file (`O_CREAT|O_EXCL|O_NOFOLLOW`) that is fsynced and `renameat`ed over
/// `<name>.md`. A symlink pre-planted at `SKILLS/<name>.md` is replaced as an
/// entry, never written through. Elsewhere a fallback refuses a symlink at
/// every component via `symlink_metadata`.
pub async fn install_skill_into_employee(
    skill_path: &Path,
    employee_dir: &Path,
    quarantine_dir: &Path,
) -> Result<ParsedSkill, String> {
    install_skill_at(skill_path, &employee_dir.join("SKILLS"), quarantine_dir, SkillsDir::LeafOnly).await
}

const SKILLS_NOT_A_DIR: &str = "install DENIED: the employee SKILLS path is not a directory";

/// R3-2 (Unix): the no-follow employee write. Returns the destination path
/// (for logging only; never reused for I/O).
#[cfg(unix)]
fn write_into_employee(
    employee_dir: &Path,
    quarantine_dir: &Path,
    filename: &str,
    content: &[u8],
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    let absolute = std::path::absolute(employee_dir).map_err(|_| EMPLOYEE_TREE_GONE.to_string())?;
    // Missing, symlinked (at any component) or non-directory: refused.
    let employee = safe_dir::SafeDir::open(&absolute).map_err(|_| EMPLOYEE_TREE_GONE.to_string())?;
    let identity = employee.metadata().map(|meta| (meta.dev(), meta.ino()))
        .map_err(|_| EMPLOYEE_TREE_GONE.to_string())?;
    let skills = match employee.mkdir_child("SKILLS", 0o755) {
        Ok(skills) => skills,
        // The tree was unlinked after the open: never recreated.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(EMPLOYEE_TREE_GONE.into()),
        Err(e) => return Err(format!("{SKILLS_NOT_A_DIR} ({e})")),
    };
    std::fs::create_dir_all(quarantine_dir).map_err(|e| format!("Failed to create quarantine dir: {e}"))?;
    let temp = format!(".{filename}.{}.tmp", uuid::Uuid::new_v4().simple());
    let mut file = skills.create_new(&temp, 0o644).map_err(|e| format!("Failed to write skill: {e}"))?;
    let written = file.write_all(content).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = skills.remove_file(&temp);
        return Err(format!("Failed to write skill: {e}"));
    }
    // A tree moved away (tombstoned / replaced) while we wrote: the path no
    // longer names the directory we hold, so do not publish into it.
    let still_live = safe_dir::SafeDir::open(&absolute).ok()
        .and_then(|dir| dir.metadata().ok())
        .is_some_and(|meta| (meta.dev(), meta.ino()) == identity);
    if !still_live {
        let _ = skills.remove_file(&temp);
        return Err(EMPLOYEE_TREE_GONE.into());
    }
    if let Err(e) = skills.replace(&temp, filename) {
        let _ = skills.remove_file(&temp);
        return Err(format!("Failed to write skill: {e}"));
    }
    Ok(absolute.join("SKILLS").join(filename))
}

/// R3-2 (non-Unix fallback): refuse a symlink / reparse point at the
/// employee tree, `SKILLS` and the destination via `symlink_metadata`, write
/// a fresh temp file (`create_new`) and rename it into place.
#[cfg(not(unix))]
fn write_into_employee(
    employee_dir: &Path,
    quarantine_dir: &Path,
    filename: &str,
    content: &[u8],
) -> Result<std::path::PathBuf, String> {
    use std::io::Write;
    let real_dir = |path: &Path| std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_dir() && !meta.file_type().is_symlink());
    if !real_dir(employee_dir) {
        return Err(EMPLOYEE_TREE_GONE.into());
    }
    let skills = employee_dir.join("SKILLS");
    match std::fs::create_dir(&skills) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(EMPLOYEE_TREE_GONE.into()),
        Err(e) => return Err(format!("Failed to create skills dir: {e}")),
    }
    if !real_dir(&skills) {
        return Err(SKILLS_NOT_A_DIR.into());
    }
    std::fs::create_dir_all(quarantine_dir).map_err(|e| format!("Failed to create quarantine dir: {e}"))?;
    let dest = skills.join(filename);
    match std::fs::symlink_metadata(&dest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(meta) if meta.file_type().is_file() && !meta.file_type().is_symlink() => {}
        Ok(_) => return Err("install DENIED: the existing skill entry is not a regular file".into()),
        Err(e) => return Err(format!("Failed to write skill: {e}")),
    }
    let temp = skills.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)
        .and_then(|mut file| file.write_all(content).and_then(|()| file.sync_all()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Failed to write skill: {e}"));
    }
    if let Err(e) = std::fs::rename(&temp, &dest) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Failed to write skill: {e}"));
    }
    Ok(dest)
}

async fn install_skill_at(
    skill_path: &Path,
    agent_skills_dir: &Path,
    quarantine_dir: &Path,
    mode: SkillsDir,
) -> Result<ParsedSkill, String> {
    let content = tokio::fs::read_to_string(skill_path)
        .await
        .map_err(|e| format!("Failed to read skill: {e}"))?;
    check_frontmatter(&content).map_err(|error| format!("install DENIED: {error}"))?;
    let (meta, body) = parse_frontmatter(&content, skill_path)?;
    let parsed = ParsedSkill {
        meta,
        content: body,
        tool_scripts: skill_path.parent().map(scan_tool_scripts).unwrap_or_default(),
    };

    if !is_safe_skill_name(&parsed.meta.name) {
        return Err(format!(
            "install DENIED: skill name '{}' is not a safe path component \
             (no '/', '\\', '..', NUL, or leading dots; max 128 bytes)",
            parsed.meta.name.escape_debug()
        ));
    }

    let filename = format!("{}.md", parsed.meta.name);
    let dest = match mode {
        SkillsDir::CreateAll => {
            tokio::fs::create_dir_all(agent_skills_dir)
                .await
                .map_err(|e| format!("Failed to create skills dir: {e}"))?;
            tokio::fs::create_dir_all(quarantine_dir)
                .await
                .map_err(|e| format!("Failed to create quarantine dir: {e}"))?;
            let dest = agent_skills_dir.join(&filename);
            tokio::fs::write(&dest, &content)
                .await
                .map_err(|e| format!("Failed to write skill: {e}"))?;
            dest
        }
        SkillsDir::LeafOnly => {
            // R3-2: one no-follow, descriptor-relative write (see
            // `install_skill_into_employee`).
            let employee_dir = agent_skills_dir.parent()
                .ok_or_else(|| EMPLOYEE_TREE_GONE.to_string())?.to_path_buf();
            let quarantine_dir = quarantine_dir.to_path_buf();
            let filename = filename.clone();
            let bytes = content.clone().into_bytes();
            tokio::task::spawn_blocking(move || {
                write_into_employee(&employee_dir, &quarantine_dir, &filename, &bytes)
            })
            .await
            .map_err(|e| format!("Failed to write skill: {e}"))??
        }
    };

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

/// WP7: install a skill into a department's shared skill directory
/// `~/.duduclaw/shared/skills/departments/<dept>/`.
///
/// Department skills sit between global and per-agent in the lookup order
/// (per-agent > department > global; nearest wins on a name collision). The
/// department name is DATA and is validated against the
/// [`duduclaw_core::is_valid_department`] allowlist here at the sink, so a
/// hostile `../..` department can never escape the shared skills tree.
pub async fn install_skill_department(
    skill_path: &Path,
    home_dir: &Path,
    department: &str,
    quarantine_dir: &Path,
) -> Result<ParsedSkill, String> {
    if !duduclaw_core::is_valid_department(department) {
        return Err(format!(
            "install DENIED: department '{}' is not a valid name \
             (ASCII alphanumeric, '-', '_'; 1..=64 bytes)",
            department.escape_debug()
        ));
    }
    let dept_skills_dir = department_skills_dir(home_dir, department);
    install_skill(skill_path, &dept_skills_dir, quarantine_dir).await
}

/// Department prompt access requires an authoritative entry and a matching
/// display mirror. An unavailable authority never falls back to self-assertion.
pub fn authoritative_department(home: &Path, agent: &str) -> Option<String> {
    use std::io::Read;
    if !duduclaw_core::is_valid_agent_id(agent) { return None; }
    let read = |path: &Path| -> Option<toml::Value> {
        let dir = safe_dir::SafeDir::open(path.parent()?).ok()?;
        let file = dir.read_file(path.file_name()?.to_str()?).ok()?;
        if file.metadata().ok()?.len() > 1024 * 1024 { return None; }
        let mut content = String::new();
        file.take(1024 * 1024 + 1).read_to_string(&mut content).ok()?;
        if content.len() > 1024 * 1024 { return None; }
        content.parse().ok()
    };
    let authority = read(&home.join("org.toml"))?;
    if let Some(schema) = authority.get("schema") {
        if schema.as_integer()? != 1 { return None; }
    }
    let dept = authority.get("agents")?.get(agent)?.get("department")?.as_str()?.trim();
    if dept.is_empty() || !duduclaw_core::is_valid_department(dept) { return None; }
    let agent_dir = home.join("agents").join(agent);
    let mirror = read(&agent_dir.join("agent.toml"))?;
    if mirror.get("agent")?.get("department")?.as_str()?.trim() != dept {
        tracing::warn!(agent, "department skill layer refused: org.toml mirror drift");
        return None;
    }
    Some(dept.to_string())
}

/// Path to a department's shared skill directory. `department` MUST already be
/// validated via [`duduclaw_core::is_valid_department`].
pub fn department_skills_dir(home_dir: &Path, department: &str) -> std::path::PathBuf {
    home_dir
        .join("shared")
        .join("skills")
        .join("departments")
        .join(department)
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
    fn frontmatter_check_is_strict() {
        assert!(check_frontmatter("plain body, no frontmatter").is_ok());
        assert!(check_frontmatter("---\nname: quote\ndescription: 報價\ntools: [a, b]\n---\nbody").is_ok());
        assert!(check_frontmatter("---\n---\nbody").is_err(), "the loader cannot read an empty block either");
        assert!(check_frontmatter("---\nname: quote\nbody without a closing line").is_err());
        assert!(check_frontmatter("---\nname: [unclosed\n---\nbody").is_err(), "bad YAML");
        assert!(check_frontmatter("---\ntools: just-a-string\n---\nbody").is_err(), "wrong type");
        assert!(check_frontmatter("---\n- a\n- b\n---\nbody").is_err(), "not a mapping");
        assert!(check_frontmatter("---\nname: ../../victim/SOUL\n---\nbody").is_err(), "unsafe name");
    }

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

    #[tokio::test]
    async fn invalid_frontmatter_cannot_fall_back_and_overwrite_an_existing_skill() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("notes.md");
        let skills = home.path().join("skills");
        std::fs::create_dir(&skills).unwrap();
        std::fs::write(skills.join("notes.md"), "original").unwrap();
        std::fs::write(&source, "---\nname: [bad, type]\n---\nreplacement").unwrap();
        let error = install_skill(&source, &skills, &home.path().join("quarantine")).await.unwrap_err();
        assert!(error.contains("DENIED"), "{error}");
        assert_eq!(std::fs::read_to_string(skills.join("notes.md")).unwrap(), "original");
    }

    #[tokio::test]
    async fn sr5r3_employee_install_creates_only_skills_and_never_the_tree() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("notes.md");
        std::fs::write(&source, "---\nname: notes\n---\nbody").unwrap();
        let quarantine = home.path().join("quarantine");
        let employee = home.path().join("agents").join("alice");
        // The checked tree vanished (tombstone moved it) before the install:
        // refused, and neither agents/alice nor agents/ is conjured.
        let error = install_skill_into_employee(&source, &employee, &quarantine).await.unwrap_err();
        assert_eq!(error, EMPLOYEE_TREE_GONE);
        assert!(!home.path().join("agents").exists());
        // A live tree: only SKILLS/ is created inside it.
        std::fs::create_dir_all(&employee).unwrap();
        let parsed = install_skill_into_employee(&source, &employee, &quarantine).await.unwrap();
        assert_eq!(parsed.meta.name, "notes");
        assert!(employee.join("SKILLS").join("notes.md").is_file());
        // An existing SKILLS/ is reused.
        assert!(install_skill_into_employee(&source, &employee, &quarantine).await.is_ok());
        // The tree is gone again (SKILLS included): never recreated.
        std::fs::remove_dir_all(&employee).unwrap();
        assert_eq!(install_skill_into_employee(&source, &employee, &quarantine).await.unwrap_err(), EMPLOYEE_TREE_GONE);
        assert!(!employee.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sr5r3_employee_install_refuses_a_symlinked_tree_or_skills_dir() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("notes.md");
        std::fs::write(&source, "---\nname: notes\n---\nbody").unwrap();
        let quarantine = home.path().join("quarantine");
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(home.path().join("agents")).unwrap();
        let employee = home.path().join("agents").join("alice");
        std::os::unix::fs::symlink(&elsewhere, &employee).unwrap();
        assert_eq!(install_skill_into_employee(&source, &employee, &quarantine).await.unwrap_err(), EMPLOYEE_TREE_GONE);
        assert!(!elsewhere.join("SKILLS").exists());
        let real = home.path().join("agents").join("bob");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&elsewhere, real.join("SKILLS")).unwrap();
        assert!(install_skill_into_employee(&source, &real, &quarantine).await.is_err());
        assert!(!elsewhere.join("notes.md").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sr5r3f_preplanted_skill_symlink_is_never_written_through() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("notes.md");
        std::fs::write(&source, "---\nname: notes\n---\nbody").unwrap();
        let quarantine = home.path().join("quarantine");
        let victim = home.path().join("victim.txt");
        std::fs::write(&victim, "host secret").unwrap();
        let employee = home.path().join("agents").join("alice");
        std::fs::create_dir_all(employee.join("SKILLS")).unwrap();
        // The employee pre-plants SKILLS/notes.md -> a host file.
        std::os::unix::fs::symlink(&victim, employee.join("SKILLS").join("notes.md")).unwrap();
        install_skill_into_employee(&source, &employee, &quarantine).await.unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "host secret", "the link was followed");
        let entry = std::fs::symlink_metadata(employee.join("SKILLS").join("notes.md")).unwrap();
        assert!(entry.file_type().is_file(), "the planted link is replaced by a regular file");
        assert_eq!(std::fs::read_to_string(employee.join("SKILLS").join("notes.md")).unwrap(),
            "---\nname: notes\n---\nbody");
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(employee.join("SKILLS")).unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        assert_eq!(names, vec!["notes.md".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sr5r3f_skills_symlink_is_refused_and_target_untouched() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("notes.md");
        std::fs::write(&source, "---\nname: notes\n---\nbody").unwrap();
        let quarantine = home.path().join("quarantine");
        let elsewhere = home.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("notes.md"), "untouched").unwrap();
        let employee = home.path().join("agents").join("bob");
        std::fs::create_dir_all(&employee).unwrap();
        std::os::unix::fs::symlink(&elsewhere, employee.join("SKILLS")).unwrap();
        let error = install_skill_into_employee(&source, &employee, &quarantine).await.unwrap_err();
        assert!(error.contains("not a directory"), "{error}");
        assert_eq!(std::fs::read_to_string(elsewhere.join("notes.md")).unwrap(), "untouched");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 1, "no temp file in the link target");
        // A symlinked ancestor of the employee tree is refused as well.
        let real_agents = home.path().join("real-agents");
        std::fs::create_dir_all(real_agents.join("carol")).unwrap();
        let linked_home = home.path().join("linked");
        std::fs::create_dir_all(&linked_home).unwrap();
        std::os::unix::fs::symlink(&real_agents, linked_home.join("agents")).unwrap();
        assert_eq!(
            install_skill_into_employee(&source, &linked_home.join("agents").join("carol"), &quarantine)
                .await.unwrap_err(),
            EMPLOYEE_TREE_GONE
        );
        assert!(!real_agents.join("carol").join("SKILLS").exists());
    }

    #[tokio::test]
    async fn install_department_validates_name_and_targets_shared_dir() {
        let home = std::env::temp_dir().join(format!("duduclaw-dept-{}", uuid::Uuid::new_v4()));
        let src_dir = home.join("src");
        let quarantine = home.join("quarantine");
        std::fs::create_dir_all(&src_dir).unwrap();
        let skill = src_dir.join("s.md");
        std::fs::write(&skill, "---\nname: art-brief\n---\nbody").unwrap();

        // Traversal-shaped department is rejected at the sink.
        for bad in ["../evil", "a/b", ".", ""] {
            let err = install_skill_department(&skill, &home, bad, &quarantine)
                .await
                .unwrap_err();
            assert!(err.contains("DENIED"), "must reject department {bad:?}: {err}");
        }

        // Valid install lands under shared/skills/departments/<dept>/.
        let parsed = install_skill_department(&skill, &home, "art", &quarantine)
            .await
            .unwrap();
        assert_eq!(parsed.meta.name, "art-brief");
        assert!(department_skills_dir(&home, "art").join("art-brief.md").is_file());

        let _ = std::fs::remove_dir_all(&home);
    }
}

#[cfg(all(test, unix))]
mod authority_security_tests {
    use super::*;
    #[test]
    fn department_requires_authority_and_matching_mirror() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents/alice")).unwrap();
        let mirror = home.join("agents/alice/agent.toml");
        std::fs::write(&mirror, "[agent]\ndepartment = 'sales'\n").unwrap();
        assert_eq!(authoritative_department(&home, "alice"), None);
        std::fs::write(home.join("org.toml"), "schema = 1\n[agents.alice]\ndepartment = 'sales'\n").unwrap();
        assert_eq!(authoritative_department(&home, "alice"), Some("sales".into()));
        std::fs::write(&mirror, "[agent]\ndepartment = 'finance'\n").unwrap();
        assert_eq!(authoritative_department(&home, "alice"), None);
        std::fs::remove_file(&mirror).unwrap();
        assert_eq!(authoritative_department(&home, "alice"), None);
        std::fs::write(&mirror, "[agent]\ndepartment = 'sales'\n").unwrap();
        std::fs::write(home.join("org.toml"), "broken TOML").unwrap();
        assert_eq!(authoritative_department(&home, "alice"), None);
    }

    #[test]
    fn department_refuses_symlinked_authority() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents/alice")).unwrap();
        std::fs::write(home.join("agents/alice/agent.toml"), "[agent]\ndepartment = 'sales'\n").unwrap();
        std::fs::write(home.join("authority"), "[agents.alice]\ndepartment = 'sales'\n").unwrap();
        std::os::unix::fs::symlink("authority", home.join("org.toml")).unwrap();
        assert_eq!(authoritative_department(&home, "alice"), None);
    }
}
