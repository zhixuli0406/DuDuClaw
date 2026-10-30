use super::{validate_id, validate_sections, validate_settings, AgentProfile, ProfileError, MAX_BYTES, SCHEMA};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Header {
    pub schema: String,
    pub id: String,
    pub name: String,
    pub description: String,
    pub role: String,
    pub language: String,
    pub skills: Vec<String>,
    pub duduclaw: toml::Table,
}

fn frontmatter(input: &str) -> Result<(String, String), ProfileError> {
    if input.len() > MAX_BYTES {
        return Err(ProfileError::Oversize);
    }
    let mut lines = input.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return Err(ProfileError::Schema);
    }
    let mut yaml = String::new();
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            return Ok((yaml, lines.collect()));
        }
        yaml.push_str(line);
    }
    Err(ProfileError::Schema)
}

/// Probe only the bounded header, independently of the body's size. A declared
/// native schema must reach the native parser even if later YAML is malformed.
pub fn has_schema(input: &str) -> bool {
    let mut lines = input.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") { return false; }
    let mut yaml = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            return yaml_has_schema(&yaml);
        }
        if yaml.len().saturating_add(line.len()) > MAX_BYTES { return false; }
        // A malformed later key or duplicate schema must not let a native file
        // downgrade to a filename-based adapter that ignores its frozen fields.
        if !line.starts_with(char::is_whitespace) && yaml_has_schema(line) {
            return true;
        }
        yaml.push_str(line);
    }
    // Prefix probes may end before the closing delimiter; a complete native
    // declaration still pins the adapter and parse() will reject truncation.
    yaml_has_schema(&yaml)
}

fn yaml_has_schema(yaml: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else { return false; };
    value.get("schema").and_then(serde_yaml::Value::as_str) == Some(SCHEMA)
}

pub fn parse(input: &str) -> Result<AgentProfile, ProfileError> {
    let (yaml, body) = frontmatter(input)?;
    let header: Header = serde_yaml::from_str(&yaml)
        .map_err(|_| ProfileError::Invalid("malformed or unknown frontmatter fields".into()))?;
    if header.schema != SCHEMA {
        return Err(ProfileError::Schema);
    }
    validate_id(&header.id)?;
    validate_settings(&header.duduclaw)?;
    let mut result = AgentProfile {
        id: header.id, name: header.name, description: header.description,
        role: header.role, language: header.language, skills: header.skills,
        settings: header.duduclaw, ..Default::default()
    };
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut active = String::new();
    let mut text = String::new();
    let mut fence: Option<(char, usize)> = None;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_end();
        if let Some((character, length)) = fence {
            if trimmed.chars().all(|c| c == character) && trimmed.chars().count() >= length {
                fence = None;
            }
        } else if let Some(heading) = trimmed.strip_prefix("# ") {
            if !active.is_empty() || !text.trim().is_empty() {
                sections.push((std::mem::take(&mut active), std::mem::take(&mut text)));
            }
            active = heading.trim().to_owned();
            continue;
        } else if let Some(first) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) {
            let length = trimmed.chars().take_while(|c| *c == first).count();
            if length >= 3 {
                fence = Some((first, length));
            }
        }
        text.push_str(line);
    }
    if !active.is_empty() || !text.trim().is_empty() {
        sections.push((active, text));
    }
    let mut unknown_rules = String::new();
    let mut seen = std::collections::BTreeSet::new();
    for (heading, text) in sections {
        let key = heading.to_ascii_lowercase();
        if matches!(key.as_str(), "persona" | "user" | "rules" | "contract") && !seen.insert(key.clone()) {
            return Err(ProfileError::Invalid(format!("duplicate section {heading}")));
        }
        match key.as_str() {
            "persona" => {
                if raw_marker(&text, "duduclaw-soul") {
                    let (soul, remaining) = raw_block(&text, "duduclaw-soul")?;
                    let (identity, remaining) = raw_block(&remaining, "duduclaw-identity")?;
                    if !remaining.trim().is_empty() {
                        return Err(ProfileError::Invalid("unexpected Persona content".into()));
                    }
                    result.soul = soul;
                    result.identity = identity;
                } else {
                    result.soul = text.trim().to_owned();
                }
            }
            "user" => result.user = decode_or_plain(&text, "duduclaw-user")?,
            "rules" => result.rules = decode_or_plain(&text, "duduclaw-rules")?,
            "contract" => result.contract = decode_contract(&text)?,
            _ => {
                result.warnings.push(format!("Unknown H1 '{heading}' merged into Rules"));
                if !unknown_rules.is_empty() { unknown_rules.push_str("\n\n"); }
                if !heading.is_empty() { unknown_rules.push_str(&format!("# {heading}\n")); }
                unknown_rules.push_str(text.trim());
            }
        }
    }
    if !unknown_rules.is_empty() {
        if !result.rules.is_empty() { result.rules.push_str("\n\n"); }
        result.rules.push_str(&unknown_rules);
    }
    validate_sections(&result)?;
    Ok(result)
}

fn raw_marker(text: &str, label: &str) -> bool {
    text.trim_start().lines().next()
        .map(|line| line.trim_start_matches('`') == label && line.starts_with("```"))
        .unwrap_or(false)
}

fn decode_or_plain(text: &str, label: &str) -> Result<String, ProfileError> {
    if !raw_marker(text, label) { return Ok(text.trim().to_owned()); }
    let (content, remaining) = raw_block(text, label)?;
    if !remaining.trim().is_empty() {
        return Err(ProfileError::Invalid(format!("unexpected {label} content")));
    }
    Ok(content)
}

fn raw_block(text: &str, label: &str) -> Result<(String, String), ProfileError> {
    let mut lines = text.trim_start_matches(['\r', '\n']).split_inclusive('\n');
    let opening = lines.next().unwrap_or_default().trim_end();
    let fence = opening.strip_suffix(label)
        .filter(|value| value.len() >= 3 && value.chars().all(|c| c == '`'))
        .ok_or_else(|| ProfileError::Invalid(format!("invalid {label} block")))?;
    let mut content = String::new();
    for line in lines.by_ref() {
        if line.trim_end_matches(['\r', '\n']) == fence {
            let content = content.strip_suffix('\n')
                .ok_or_else(|| ProfileError::Invalid(format!("invalid {label} boundary")))?.to_owned();
            return Ok((content, lines.collect()));
        }
        content.push_str(line);
    }
    Err(ProfileError::Invalid(format!("unclosed {label} block")))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    #[serde(default)]
    boundaries: Boundaries,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Boundaries {
    must_not: Vec<String>,
    must_always: Vec<String>,
    max_tool_calls_per_turn: u32,
}

pub(super) fn valid_toml_contract(text: &str) -> bool {
    toml::from_str::<Contract>(text).is_ok()
}

fn decode_contract(text: &str) -> Result<String, ProfileError> {
    if raw_marker(text, "duduclaw-contract") {
        let content = decode_or_plain(text, "duduclaw-contract")?;
        if !valid_toml_contract(&content) {
            return Err(ProfileError::Invalid("invalid native Contract TOML".into()));
        }
        return Ok(content);
    }
    let content = text.trim();
    if valid_toml_contract(content) { return Ok(content.to_owned()); }
    let boundaries: Boundaries = serde_yaml::from_str(content)
        .map_err(|_| ProfileError::Invalid("Contract must be native TOML or YAML boundary lists".into()))?;
    toml::to_string(&Contract { boundaries })
        .map_err(|_| ProfileError::Invalid("cannot encode Contract".into()))
}
