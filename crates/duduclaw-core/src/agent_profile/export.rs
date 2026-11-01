use std::{borrow::Cow, path::Path, str::FromStr};

use serde::Serialize;

use super::{parser::Header, read_file, validate_id, validate_settings, AgentProfile, ProfileError, MAX_BYTES, SCHEMA};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat { Duduclaw, Openclaw, Hermes, AgentsMd, ClaudeSubagent }

impl FromStr for ExportFormat {
    type Err = ProfileError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "duduclaw" => Ok(Self::Duduclaw),
            "openclaw" => Ok(Self::Openclaw),
            "hermes" => Ok(Self::Hermes),
            "agents-md" => Ok(Self::AgentsMd),
            "claude-subagent" => Ok(Self::ClaudeSubagent),
            _ => Err(ProfileError::Invalid("unsupported export format".into())),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportArtifact { pub name: String, pub content: String }

/// Read only known profile files and skill names. Never discover MCP, memory,
/// credential or channel files, and never read skill bodies.
pub fn load(agent_dir: &Path, id: &str) -> Result<AgentProfile, ProfileError> {
    validate_id(id)?;
    let metadata = std::fs::symlink_metadata(agent_dir).map_err(|_| ProfileError::Read("employee directory".into()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ProfileError::Read("employee directory".into()));
    }
    let config: toml::Table = toml::from_str(&read_file(agent_dir, "agent.toml", true)?)
        .map_err(|_| ProfileError::Invalid("malformed employee settings".into()))?;
    let agent = config.get("agent").and_then(toml::Value::as_table);
    let field = |key: &str| agent.and_then(|a| a.get(key)).and_then(toml::Value::as_str).unwrap_or_default().to_owned();
    let mut result = AgentProfile {
        id: id.into(), name: field("display_name"), description: field("description"),
        role: field("role"), language: field("language"),
        soul: read_file(agent_dir, "SOUL.md", false)?,
        identity: read_file(agent_dir, "IDENTITY.md", false)?,
        user: read_file(agent_dir, "USER.md", false)?,
        rules: read_file(agent_dir, "INSTRUCTIONS.md", false)?,
        contract: read_file(agent_dir, "CONTRACT.toml", false)?,
        skills: skill_names(agent_dir)?,
        ..Default::default()
    };
    if result.name.is_empty() { result.name = id.into(); }
    for (section, value) in config {
        if crate::org_field_guard::AGENT_EDITABLE_SECTIONS.contains(&section.as_str()) {
            result.settings.insert(section.clone(), portable_value(value, &section));
        }
    }
    Ok(result)
}

/// Names follow the runtime's legacy flat *.md / nested SKILL.md convention.
/// Metadata only: content and attachments stay outside the export boundary.
fn skill_names(agent_dir: &Path) -> Result<Vec<String>, ProfileError> {
    let root = agent_dir.join("SKILLS");
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ProfileError::Read("skill directory".into())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ProfileError::Read("skill directory".into()));
    }
    let mut queue = std::collections::VecDeque::from([(root, 0usize)]);
    let mut names = std::collections::BTreeMap::new();
    while let Some((dir, depth)) = queue.pop_front() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|_| ProfileError::Read("skill directory".into()))?;
        for entry in entries {
            let entry = entry.map_err(|_| ProfileError::Read("skill directory".into()))?;
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else { continue; };
            if file_name.starts_with('.') { continue; }
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|_| ProfileError::Read("skill entry".into()))?;
            if metadata.file_type().is_symlink() { continue; }
            if metadata.is_dir() {
                if depth < 8 { queue.push_back((path, depth + 1)); }
                continue;
            }
            if !metadata.is_file() || !path.extension().and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md")) { continue; }
            let is_skill = file_name.eq_ignore_ascii_case("SKILL.md");
            let name = if is_skill {
                path.parent().and_then(|parent| parent.file_name()).and_then(|name| name.to_str())
            } else if depth == 0 {
                path.file_stem().and_then(|name| name.to_str())
            } else { None };
            if let Some(name) = name {
                // Keep deterministic names when both legacy and nested forms
                // declare the same skill, including case-only duplicates.
                names.entry(name.to_ascii_lowercase())
                    .and_modify(|old: &mut String| {
                        if name < old.as_str() { *old = name.to_owned(); }
                    }).or_insert_with(|| name.to_owned());
            }
        }
    }
    let mut names: Vec<_> = names.into_values().collect();
    names.sort();
    Ok(names)
}

fn portable_value(value: toml::Value, section: &str) -> toml::Value {
    match value {
        toml::Value::Table(table) => toml::Value::Table(table.into_iter()
            .filter(|(key, _)| !super::forbidden_key(section, key))
            .map(|(key, value)| (key, portable_value(value, section))).collect()),
        toml::Value::Array(values) => toml::Value::Array(values.into_iter().map(|v| portable_value(v, section)).collect()),
        other => other,
    }
}

fn scan(section: &str, text: &str, credential_scan: &impl Fn(&str) -> Result<bool, ()>) -> Result<(), ProfileError> {
    let detected = credential_scan(text).map_err(|_| ProfileError::Invalid(format!("credential scan failed in section {section}; export refused")))?;
    if detected || text.contains("secret://") || text.contains("-----BEGIN PRIVATE KEY-----")
        || text.contains("-----BEGIN RSA PRIVATE KEY-----")
        || text.contains("-----BEGIN EC PRIVATE KEY-----")
        || text.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || matches!(crate::secret_redact::redact_secrets(text), Cow::Owned(_))
        || matches!(crate::secret_redact::redact_secrets(&text.replace(['=', ':'], " ")), Cow::Owned(_))
    {
        return Err(ProfileError::Secret(section.into()));
    }
    Ok(())
}

/// Render only after the caller-provided credential detector accepts every section.
/// The security crate owns the production wrapper; core cannot depend on it.
pub fn export_with_credential_scan(profile: &AgentProfile, format: ExportFormat, include_user: bool, credential_scan: impl Fn(&str) -> Result<bool, ()>) -> Result<Vec<ExportArtifact>, ProfileError> {
    validate_id(&profile.id)?;
    validate_settings(&profile.settings)?;
    if format == ExportFormat::Duduclaw {
        super::validate_sections_with_user(profile, include_user)?;
    }
    if !super::parser::valid_toml_contract(&profile.contract) {
        return Err(ProfileError::Invalid("invalid native Contract TOML".into()));
    }
    for (section, content) in [("Persona", &profile.soul), ("Identity", &profile.identity), ("Rules", &profile.rules), ("Contract", &profile.contract)] {
        scan(section, content, &credential_scan)?;
    }
    if include_user && profile.user.chars().count() > super::MAX_USER_CHARS {
        return Err(ProfileError::Invalid("User exceeds 4,000 characters".into()));
    }
    let user = if include_user { scan("User", &profile.user, &credential_scan)?; profile.user.as_str() } else { "" };
    let header = Header {
        schema: SCHEMA.into(), id: profile.id.clone(), name: profile.name.clone(),
        description: profile.description.clone(), role: profile.role.clone(), language: profile.language.clone(),
        skills: profile.skills.clone(), duduclaw: profile.settings.clone(),
    };
    let yaml = serde_yaml::to_string(&header).map_err(|_| ProfileError::Invalid("cannot encode metadata".into()))?;
    scan("Settings", &yaml, &credential_scan)?;
    let artifact = |name: String, content: String| ExportArtifact { name, content };
    let flat = || {
        let mut content = format!("# Persona\n\n{}\n\n{}\n\n# Rules\n\n{}\n\n# Contract\n\n{}\n", profile.soul, profile.identity, profile.rules, profile.contract);
        if include_user { content.push_str(&format!("\n# User\n\n{user}\n")); }
        content
    };
    let files = match format {
        ExportFormat::Duduclaw => {
            let mut content = format!("---\n{yaml}---\n\n# Persona\n{}
{}\n# User\n{}\n# Rules\n{}\n# Contract\n{}", raw_block("duduclaw-soul", &profile.soul), raw_block("duduclaw-identity", &profile.identity), raw_block("duduclaw-user", user), raw_block("duduclaw-rules", &profile.rules), raw_block("duduclaw-contract", &profile.contract));
            if !content.ends_with('\n') { content.push('\n'); }
            vec![artifact(format!("{}.agent.md", profile.id), content)]
        }
        ExportFormat::Openclaw | ExportFormat::Hermes => {
            let soul = if format == ExportFormat::Hermes && !profile.identity.is_empty() {
                format!("{}\n\n{}", profile.soul, profile.identity)
            } else { profile.soul.clone() };
            let mut files = vec![artifact("SOUL.md".into(), soul), artifact("AGENTS.md".into(), format!("{}\n\n# Contract\n\n{}", profile.rules, profile.contract))];
            if format == ExportFormat::Openclaw { files.push(artifact("IDENTITY.md".into(), profile.identity.clone())); }
            if include_user { files.push(artifact("USER.md".into(), user.into())); }
            files
        }
        ExportFormat::AgentsMd => vec![artifact("AGENTS.md".into(), flat())],
        ExportFormat::ClaudeSubagent => {
            #[derive(Serialize)]
            struct ClaudeHeader<'a> { name: &'a str, description: &'a str }
            let header = serde_yaml::to_string(&ClaudeHeader { name: &profile.id, description: &profile.description })
                .map_err(|_| ProfileError::Invalid("cannot encode metadata".into()))?;
            vec![artifact(format!("{}.md", profile.id), format!("---\n{header}---\n\n{}", flat()))]
        }
    };
    for file in &files {
        if file.content.len() > MAX_BYTES { return Err(ProfileError::Oversize); }
        scan(&file.name, &file.content, &credential_scan)?;
    }
    Ok(files)
}

fn raw_block(label: &str, text: &str) -> String {
    let longest = text.lines().map(|line| line.chars().take_while(|c| *c == '`').count()).max().unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    format!("{fence}{label}\n{text}\n{fence}\n")
}
