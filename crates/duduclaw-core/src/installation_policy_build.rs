//! Shared build-input validation; it never includes customer identities or defaults.
use serde::Deserialize;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 32 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyManifest {
    pub schema_version: u32,
    pub blocked_channels: Vec<String>,
    pub blocked_runtimes: Vec<String>,
    pub blocked_providers: Vec<String>,
}

pub fn parse_policy(text: &str, format: &str) -> Result<PolicyManifest, &'static str> {
    let policy: PolicyManifest = match format {
        "json" => serde_json::from_str(text).map_err(|_| "Invalid policy JSON or schema")?,
        "toml" => toml::from_str(text).map_err(|_| "Invalid policy TOML or schema")?,
        _ => return Err("Policy input must be JSON or TOML"),
    };
    if policy.schema_version != 1 {
        return Err("Unsupported policy schema version");
    }
    for values in [
        &policy.blocked_channels,
        &policy.blocked_runtimes,
        &policy.blocked_providers,
    ] {
        if values.len() > 128 {
            return Err("Policy selector limit exceeded");
        }
        let mut seen = HashSet::new();
        for value in values {
            if value.is_empty()
                || value.len() > 64
                || !value.as_bytes()[0].is_ascii_lowercase()
                || !value.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')
                })
            {
                return Err("Policy selectors must be canonical lowercase ASCII identifiers");
            }
            if !seen.insert(value) {
                return Err("Duplicate policy selector");
            }
        }
    }
    // This is the public canonical CLI inventory, not an installation matrix.
    const RUNTIME_IDS: &[&str] = &[
        "claude",
        "codex",
        "gemini",
        "antigravity",
        "grok",
        "qwen",
        "kimi",
        "copilot",
        "kiro",
        "cursor",
        "vibe",
        "opencode",
        "openai_compat",
    ];
    if policy
        .blocked_runtimes
        .iter()
        .any(|id| !RUNTIME_IDS.contains(&id.as_str()))
    {
        return Err("Policy runtime selectors must match the canonical CLI catalog");
    }
    Ok(policy)
}

pub fn read_policy(path: &Path) -> Result<PolicyManifest, &'static str> {
    if !std::fs::symlink_metadata(path)
        .map_err(|_| "Policy input metadata is unreadable")?
        .is_file()
    {
        return Err("Policy input must be a regular nonsymlink file");
    }
    let file = std::fs::File::open(path).map_err(|_| "Policy input is unreadable")?;
    if !file
        .metadata()
        .map_err(|_| "Policy input metadata is unreadable")?
        .is_file()
    {
        return Err("Policy input must be a regular file");
    }
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Policy input must be UTF-8")?;
    if text.len() as u64 > MAX_BYTES {
        return Err("Policy input size limit exceeded");
    }
    let format = path
        .extension()
        .and_then(|s| s.to_str())
        .ok_or("Policy input must have a format extension")?;
    parse_policy(&text, format)
}

pub fn generate(policy: Option<&PolicyManifest>) -> String {
    let required = policy.is_some();
    let empty = Vec::new();
    let channels = policy.map_or(&empty, |p| &p.blocked_channels);
    let runtimes = policy.map_or(&empty, |p| &p.blocked_runtimes);
    let providers = policy.map_or(&empty, |p| &p.blocked_providers);
    // Debug formatting escapes every string; manifest text is never Rust code.
    format!("pub const REQUIRED: bool = {required};\npub const AVAILABLE: bool = {required};\npub const CHANNELS: &[&str] = &{channels:?};\npub const RUNTIMES: &[&str] = &{runtimes:?};\npub const PROVIDERS: &[&str] = &{providers:?};\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    const VALID: &str = r#"{"schema_version":1,"blocked_channels":["slack"],"blocked_runtimes":["claude"],"blocked_providers":["openai"]}"#;

    #[test]
    fn valid_synthetic_input_generates_required_policy_without_identity() {
        let manifest = parse_policy(VALID, "json").unwrap();
        let output = generate(Some(&manifest));
        assert!(output.contains("REQUIRED: bool = true"));
        assert!(output.contains("CHANNELS: &[&str] = &[\"slack\"]"));
        assert!(generate(None).contains("REQUIRED: bool = false"));
    }

    #[test]
    fn every_public_runtime_catalog_id_is_accepted_by_the_build_schema() {
        for id in crate::runtime_catalog::all_ids() {
            let mut value: serde_json::Value = serde_json::from_str(VALID).unwrap();
            value["blocked_runtimes"] = serde_json::json!([id]);
            assert!(parse_policy(&value.to_string(), "json").is_ok(), "{id}");
        }
    }

    #[test]
    fn malformed_unknown_missing_duplicate_and_injection_inputs_are_refused() {
        for input in [
            String::new(),
            VALID.replace("\"schema_version\":1", "\"schema_version\":2"),
            VALID.replace(
                "\"schema_version\":1",
                "\"schema_version\":1,\"customer\":\"example\"",
            ),
            VALID.replace("\"blocked_channels\":[\"slack\"],", ""),
            VALID.replace("[\"slack\"]", "[\"slack\",\"slack\"]"),
            VALID.replace("slack", "SLACK"),
            VALID.replace("slack", "x;panic!()"),
            VALID.replace("claude", "unknown-runtime"),
        ] {
            assert!(parse_policy(&input, "json").is_err());
        }
        assert!(parse_policy(VALID, "txt").is_err());
    }

    #[test]
    fn missing_and_oversized_files_never_generate_permissive_policy() {
        let directory = tempfile::tempdir().unwrap();
        assert!(read_policy(&directory.path().join("missing.json")).is_err());
        let path = directory.path().join("policy.json");
        std::fs::write(&path, " ".repeat(MAX_BYTES as usize + 1)).unwrap();
        assert!(read_policy(&path).is_err());
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_policy(&path).is_err());
    }
}
