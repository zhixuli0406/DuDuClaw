//! Pure skill frontmatter transformations; publication remains a host decision.
use duduclaw_isolation::worker::{ParseError, SkillAction};

pub(super) fn prepare(content: &str, action: SkillAction, agent: &str, at: &str)
    -> Result<String, ParseError> {
    match action {
        SkillAction::Adopt => {
            let body = strip_frontmatter(content);
            if body.trim().is_empty() { return Err(ParseError::InvalidContent); }
            Ok(body)
        }
        SkillAction::Graduate => {
            // Identifiers and timestamps are host-selected bounded data; refuse
            // any value that could inject extra YAML fields.
            if !agent.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || !at.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'.' | b'+')) {
                return Err(ParseError::InvalidContent);
            }
            let metadata = format!("graduated_from: \"{agent}\"\ngraduated_at: \"{at}\"\nvalidated_lift: 0.000");
            let trimmed = content.trim();
            let text = if let Some(after_first) = trimmed.strip_prefix("---") {
                let after_first = after_first.trim_start_matches(['\r', '\n']);
                let mut yaml = Vec::new(); let mut body = Vec::new(); let mut closed = false;
                for line in after_first.lines() {
                    if !closed && line.trim() == "---" { closed = true; continue; }
                    if closed { body.push(line); } else { yaml.push(line); }
                }
                if !closed { return Err(ParseError::InvalidContent); }
                format!("---\n{}\n{metadata}\n---\n{}", yaml.join("\n"), body.join("\n"))
            } else { format!("---\n{metadata}\n---\n\n{content}") };
            Ok(text)
        }
    }
}
fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if trimmed.lines().next().map(str::trim) != Some("---") { return trimmed.to_owned(); }
    let mut lines = trimmed.lines().skip(1);
    if !lines.by_ref().any(|line| line.trim() == "---") { return String::new(); }
    lines.collect::<Vec<_>>().join("\n").trim_start().to_owned()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn i1c_adoption_returns_only_own_skill_body_without_shared_write_authority() {
        let content = "---\nusage_count: 2\nadopted_by: bob\n---\n\n## 中文\ncontent";
        let body = prepare(content, SkillAction::Adopt, "alice", "2026-11-05T00:00:00Z").unwrap();
        assert_eq!(body, "## 中文\ncontent");
        assert!(prepare("---\nunclosed", SkillAction::Adopt, "alice", "now").is_err());
    }
    #[test]
    fn i1c_graduation_metadata_is_data_and_unclosed_frontmatter_is_refused() {
        let text = prepare("## 中文\nbody", SkillAction::Graduate, "alice", "2026-11-05T00:00:00Z").unwrap();
        assert!(text.contains("graduated_from: \"alice\"")); assert!(text.ends_with("## 中文\nbody"));
        assert!(prepare("body", SkillAction::Graduate, "alice", "now\nadmin: true").is_err());
        assert!(prepare("---\nunclosed", SkillAction::Graduate, "alice", "now").is_err());
    }
}

/// Deliberately strict install subset. A worker cannot redirect publication by
/// renaming a frontmatter field, duplicate name, YAML alias or nested mapping.
pub(super) fn validate_manifest(content: &str, expected: &str) -> Result<(), ParseError> {
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Localized { name: String, description: String }
    #[derive(Default, serde::Deserialize)]
    #[serde(default)]
    struct Metadata {
        name: String, description: String, trigger: String, author: String, version: String,
        tools: Vec<String>, tags: Vec<String>, requires: Vec<String>,
        skill_type: Option<String>, compose_mode: Option<String>,
        display: std::collections::HashMap<String, Localized>, estimated_minutes_saved: Option<u32>,
    }
    let mut lines = content.trim_start().lines();
    if lines.next().map(str::trim) != Some("---") { return Err(ParseError::InvalidContent); }
    let mut header = Vec::new(); let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" { closed = true; break; }
        header.push(line);
    }
    if !closed || lines.all(|line| line.trim().is_empty()) { return Err(ParseError::InvalidContent); }
    // Parse the full YAML in E, including unknown fields; validate the same
    // field types used by the loader instead of relying on line heuristics.
    let value: serde_yaml::Value = serde_yaml::from_str(&header.join("\n")).map_err(|_| ParseError::InvalidContent)?;
    if !value.is_mapping() { return Err(ParseError::InvalidContent); }
    for field in ["skill_type", "compose_mode"] {
        if value.get(field).is_some_and(serde_yaml::Value::is_null) { return Err(ParseError::InvalidContent); }
    }
    let metadata: Metadata = serde_yaml::from_value(value).map_err(|_| ParseError::InvalidContent)?;
    if metadata.name != expected
        || metadata.skill_type.as_deref().is_some_and(|s| !matches!(s, "atomic" | "functional" | "planning"))
        || metadata.compose_mode.as_deref().is_some_and(|s| !matches!(s, "parallel" | "sequential" | "conditional")) {
        return Err(ParseError::InvalidContent);
    }
    Ok(())
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    #[test]
    fn i1c_manifest_matches_exact_name_and_refuses_alias_duplicate_and_unclosed() {
        assert!(validate_manifest("---\nname: notes\ndescription: Notes\n---\n# Notes", "notes").is_ok());
        for text in ["---\nname: other\n---\nbody", "---\nname: notes\nname: other\n---\nbody", "---\nname: notes", "---\nname: notes\n---\n", "---\nname: notes\ndescription: [unterminated\n---\nbody", "---\nname: notes\ntools: wrong-type\n---\nbody", "---\nname: notes\nskill_type: null\n---\nbody", "---\nname: notes\ncompose_mode: null\n---\nbody"] {
            assert!(validate_manifest(text, "notes").is_err(), "{text}");
        }
    }
}

/// Parse loader identity in E. A name is data and never a host pathname.
pub(super) fn identity(bytes: &[u8]) -> Result<String, ParseError> {
    if bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
    let content = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
    let mut lines = content.trim_start().lines();
    if lines.next().map(str::trim) != Some("---") { return Err(ParseError::InvalidContent); }
    let mut header = Vec::new(); let mut closed = false;
    for line in lines {
        if line.trim() == "---" { closed = true; break; }
        header.push(line);
    }
    if !closed { return Err(ParseError::InvalidContent); }
    let value: serde_yaml::Value = serde_yaml::from_str(&header.join("\n")).map_err(|_| ParseError::InvalidContent)?;
    let name = value.get("name").and_then(|v| v.as_str()).ok_or(ParseError::InvalidContent)?;
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(ParseError::InvalidContent);
    }
    Ok(name.to_owned())
}

/// Model bytes are parsed, validated, transformed and scanned before returning
/// an immutable publication preview. No model credentials or paths enter E.
pub(super) fn synthesize(bytes: &[u8], agent: &str, at: &str) -> Result<String, ParseError> {
    if bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
    let raw = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?.trim();
    let content = if let Some(rest) = raw.strip_prefix("```markdown\n").or_else(|| raw.strip_prefix("```\n")) {
        rest.strip_suffix("```").ok_or(ParseError::InvalidContent)?.trim()
    } else { raw };
    let name = identity(content.as_bytes())?;
    if name.len() > 50 || name.starts_with('-') || name.ends_with('-') || name.contains("--")
        || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || strip_frontmatter(content).len() > 5000 {
        return Err(ParseError::InvalidContent);
    }
    validate_manifest(content, &name)?;
    let prepared = prepare(content, SkillAction::Graduate, agent, at)?;
    // Validate the effective artifact, including duplicate reserved metadata.
    validate_manifest(&prepared, &name)?;
    if !super::security_scanner::scan_skill(&prepared, None).passed { return Err(ParseError::InvalidContent); }
    Ok(prepared)
}

/// Loader identity comes from a host-selected layout, never from YAML name.
pub(super) fn inspect_identity(bytes: &[u8], name: &str) -> Result<String, ParseError> {
    if bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
    if name.is_empty() || name.len() > 64 || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(ParseError::InvalidContent);
    }
    let content = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
    let mut lines = content.trim_start().lines();
    if lines.next().map(str::trim) == Some("---") {
        let mut header = vec![]; let mut closed = false;
        for line in lines {
            if line.trim() == "---" { closed = true; break; }
            header.push(line);
        }
        if !closed { return Err(ParseError::InvalidContent); }
        let _: serde_yaml::Value = serde_yaml::from_str(&header.join("\n")).map_err(|_| ParseError::InvalidContent)?;
    }
    Ok(name.to_owned())
}
