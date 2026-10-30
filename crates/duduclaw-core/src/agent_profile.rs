//! Product-owned interchange format. The employee's existing files remain authoritative.

use std::path::Path;

use serde::{Deserialize, Serialize};

mod export;
mod parser;
#[cfg(test)]
mod tests;

pub use export::{export, load, ExportArtifact, ExportFormat};
pub use parser::{has_schema, parse};

pub const SCHEMA: &str = "duduclaw.agent/v1";
pub const MAX_BYTES: usize = 256 * 1024;
pub const MAX_USER_CHARS: usize = 4_000;
pub const MAX_MARKDOWN_BYTES: usize = 64 * 1024;
pub const MAX_CONTRACT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub role: String,
    pub language: String,
    pub skills: Vec<String>,
    pub settings: toml::Table,
    pub soul: String,
    pub identity: String,
    pub user: String,
    pub rules: String,
    pub contract: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("Agent File exceeds 256 KiB")]
    Oversize,
    #[error("Missing or unsupported Agent File schema")]
    Schema,
    #[error("Invalid Agent File: {0}")]
    Invalid(String),
    #[error("Frozen fields cannot be imported: {0}")]
    Frozen(String),
    #[error("Secret-shaped content in section {0}; export refused")]
    Secret(String),
    #[error("Cannot read profile file {0}")]
    Read(String),
}

/// Keep native import/export within the shared human writer's slot limits.
pub fn validate_sections(profile: &AgentProfile) -> Result<(), ProfileError> {
    validate_sections_with_user(profile, true)
}

fn validate_sections_with_user(profile: &AgentProfile, include_user: bool) -> Result<(), ProfileError> {
    for (section, text) in [
        ("Persona", profile.soul.as_str()),
        ("Identity", profile.identity.as_str()),
        ("Rules", profile.rules.as_str()),
    ] {
        if text.len() > MAX_MARKDOWN_BYTES {
            return Err(ProfileError::Invalid(format!("{section} exceeds 64 KiB")));
        }
    }
    if profile.contract.len() > MAX_CONTRACT_BYTES {
        return Err(ProfileError::Invalid("Contract exceeds 32 KiB".into()));
    }
    if include_user {
        if profile.user.len() > MAX_MARKDOWN_BYTES {
            return Err(ProfileError::Invalid("User exceeds 64 KiB".into()));
        }
        if profile.user.chars().count() > MAX_USER_CHARS {
            return Err(ProfileError::Invalid("User exceeds 4,000 characters".into()));
        }
    }
    Ok(())
}

pub fn validate_id(id: &str) -> Result<(), ProfileError> {
    if !crate::is_valid_agent_id(id) || crate::is_reserved_agent_id(id) {
        return Err(ProfileError::Invalid("invalid employee id".into()));
    }
    Ok(())
}

fn forbidden_key(section: &str, key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.ends_with("_enc")
        || matches!(key.as_str(), "account_pool" | "channels" | "capabilities" | "permissions" | "mcp" | "mcpservers")
        || (section == "agent" && crate::org_field_guard::AGENT_ORG_FIELDS.contains(&key.as_str()))
        || crate::org_field_guard::AGENT_SECURITY_KEYS.iter()
            .any(|(s, keys)| *s == section && keys.contains(&key.as_str()))
}

fn frozen_keys(value: &toml::Value, section: &str, path: &str, out: &mut Vec<String>) {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let next = format!("{path}.{key}");
                if forbidden_key(section, key) {
                    out.push(next);
                } else {
                    frozen_keys(value, section, &next, out);
                }
            }
        }
        toml::Value::Array(values) => {
            for value in values {
                frozen_keys(value, section, path, out);
            }
        }
        _ => {}
    }
}

pub fn validate_settings(settings: &toml::Table) -> Result<(), ProfileError> {
    let mut frozen = Vec::new();
    for (section, value) in settings {
        if !crate::org_field_guard::AGENT_EDITABLE_SECTIONS.contains(&section.as_str()) {
            frozen.push(format!("duduclaw.{section}"));
        } else {
            frozen_keys(value, section, &format!("duduclaw.{section}"), &mut frozen);
        }
    }
    if frozen.is_empty() {
        for (section, value) in settings {
            if !value.is_table() {
                return Err(ProfileError::Invalid(format!("duduclaw.{section} must be a table")));
            }
        }
        Ok(())
    } else {
        Err(ProfileError::Frozen(frozen.join(", ")))
    }
}

fn read_file(dir: &Path, name: &str, required: bool) -> Result<String, ProfileError> {
    use std::io::Read;
    let path = dir.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(String::new()),
        Err(_) => return Err(ProfileError::Read(name.into())),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_BYTES as u64 {
        return Err(ProfileError::Read(name.into()));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&path).map_err(|_| ProfileError::Read(name.into()))?;
    if !file.metadata().map_err(|_| ProfileError::Read(name.into()))?.is_file() {
        return Err(ProfileError::Read(name.into()));
    }
    let mut text = String::new();
    file.take((MAX_BYTES + 1) as u64).read_to_string(&mut text)
        .map_err(|_| ProfileError::Read(name.into()))?;
    if text.len() > MAX_BYTES {
        return Err(ProfileError::Oversize);
    }
    Ok(text)
}
