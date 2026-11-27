//! Bind importer-generated Markdown to its signed source, not just its filename.
use std::collections::BTreeMap;
use super::DESCRIPTION;

/// Minimal port of CLI agent_config::md::split_frontmatter. Keep its lenient
/// scalar fallback: imported packs can contain broken quotes and block scalars.
fn split_frontmatter(text: &str) -> (Option<BTreeMap<String, String>>, String) {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = normalized.lines();
    if lines.next().map(str::trim_end) != Some("---") { return (None, text.to_owned()); }
    let mut head = Vec::new();
    let mut body = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed && line.trim_end() == "---" { closed = true; continue; }
        if closed { body.push(line); } else { head.push(line); }
    }
    if !closed { return (None, text.to_owned()); }
    let mut fields = BTreeMap::new();
    for line in &head {
        if line.starts_with(' ') || line.starts_with('\t') || line.trim_start().starts_with('-') { continue; }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            if key.is_empty() || key.contains(' ') { continue; }
            let value = value.trim();
            let unquoted = ['"', '\''].into_iter().find_map(|q|
                value.strip_prefix(q).map(|rest| rest.strip_suffix(q).unwrap_or(rest))).unwrap_or(value);
            fields.insert(key.to_owned(), unquoted.to_owned());
        }
    }
    if let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str(&head.join("\n")) {
        for (key, value) in map {
            if let (Some(key), Some(value)) = (key.as_str(), value.as_str()) {
                fields.insert(key.to_owned(), value.to_owned());
            }
        }
    }
    (Some(fields), body.join("\n").trim_start_matches('\n').to_owned())
}

pub(super) fn verify(effective: &[u8], source: &[u8], name: &str, version: &str) -> Result<(), String> {
    let source = std::str::from_utf8(source).map_err(|_| DESCRIPTION)?;
    // CLI read_text removes the BOM even when there is no frontmatter.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let (fields, body) = split_frontmatter(source);
    let fields = fields.unwrap_or_default();
    let get = |key: &str| fields.get(key).map(String::as_str).map(str::trim).filter(|v| !v.is_empty());
    let description = get("description").or_else(|| get("icon")).or_else(|| get("label")).unwrap_or(name);
    let description = duduclaw_core::truncate_chars(description.trim(), 1024);
    let effective = std::str::from_utf8(effective).map_err(|_| DESCRIPTION)?;
    let (effective_fields, effective_body) = split_frontmatter(effective);
    if effective_fields.is_none() || effective_body.trim_end() != body.trim_end().trim_start_matches('\n') {
        return Err(DESCRIPTION.into());
    }
    let (yaml, rendered_body) = effective.strip_prefix("---\n").and_then(|s| s.split_once("\n---\n\n"))
        .ok_or(DESCRIPTION)?;
    // Exact renderer layout, including the final newline. This also rejects
    // extra separators/leading body lines that the lenient splitter discards.
    if rendered_body != format!("{}\n", body.trim_end())
        || effective != format!("---\n{yaml}\n---\n\n{}\n", body.trim_end())
    { return Err(DESCRIPTION.into()); }
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).map_err(|_| DESCRIPTION)?;
    let map = value.as_mapping().ok_or(DESCRIPTION)?;
    if map.len() != 3 || map.keys().any(|k| !matches!(k.as_str(), Some("name" | "description" | "metadata")))
        || value.get("name").and_then(|v| v.as_str()) != Some(name)
        || value.get("description").and_then(|v| v.as_str()) != Some(description.as_str())
    { return Err(DESCRIPTION.into()); }
    let metadata = value.get("metadata").and_then(|v| v.as_mapping()).ok_or(DESCRIPTION)?;
    for (key, value) in metadata {
        let text = value.as_str().ok_or(DESCRIPTION)?;
        let valid = match key.as_str() {
            Some("label") => Some(text) == get("label"),
            Some("original_name") => Some(text) == get("name"),
            Some("version") => text == version,
            Some("imported_from" | "source_path") => (1..=256).contains(&text.len())
                && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:/-".contains(&b)),
            _ => false,
        };
        if !valid { return Err(DESCRIPTION.into()); }
    }
    // The renderer records a nonempty original name when renaming it.
    if let Some(original) = get("name").filter(|original| *original != name) {
        if value.get("metadata").and_then(|m| m.get("original_name")).and_then(|v| v.as_str()) != Some(original) {
            return Err(DESCRIPTION.into());
        }
    }
    Ok(())
}

// Compile the actual CLI parser into these tests so the compatibility contract
// cannot silently drift when the importer changes. It has no CLI-only imports.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../duduclaw-cli/src/expert/agent_config/md.rs"]
mod cli_md;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitter_matches_cli_for_imported_formats() {
        for text in [
            "No frontmatter\n\n", "---\nname: unclosed\nbody\n", "\u{feff}no fence\n",
            "---\nname: helper\ndescription: Help\n---\n\n# Body\n  tail  \n\n",
            "\u{feff}---  \r\nname: helper\r\n--- \r\n\r\nBody\r\n",
            "---\nname: doubao-book-writer\nlabel: \"長文件寫作\"\nicon: \"公里的工作臺。\n---\n# Body\n",
            "---\ndescription: |\n  First\n  Second\nmetadata:\n  label: nested\ntools:\n  - search\n---\n\nBody\n",
            "---\nname: first\nname: second\n---\nBody\n",
        ] {
            let (expected, body) = cli_md::split_frontmatter(text);
            let (actual, actual_body) = split_frontmatter(text);
            assert_eq!(actual, expected.map(|fm| fm.fields), "{text:?}");
            assert_eq!(actual_body, body, "{text:?}");
        }
    }

    #[test]
    fn description_uses_cli_trim_and_unicode_character_limit() {
        let description = "說明🦀".repeat(400);
        let source = format!("---\nname: original\ndescription: '  {description}  '\n---\nBody  \n\n");
        let short = duduclaw_core::truncate_chars(description.trim(), 1024);
        let effective = format!("---\nname: helper\ndescription: '{short}'\nmetadata:\n  original_name: original\n  version: 1.2.0\n---\n\nBody\n");
        verify(effective.as_bytes(), source.as_bytes(), "helper", "1.2.0").unwrap();
        assert!(verify(effective.replace("  original_name: original\n", "").as_bytes(),
            source.as_bytes(), "helper", "1.2.0").is_err());
        assert!(verify(effective.trim_end().as_bytes(), source.as_bytes(), "helper", "1.2.0").is_err());
    }

    #[test]
    fn description_precedence_and_metadata_are_bound() {
        for (source, description) in [
            ("---\ndescription: Help\nicon: Icon\nlabel: Label\n---\nBody", "Help"),
            ("---\nicon: \"Icon\nlabel: Label\n---\nBody", "Icon"),
            ("---\nlabel: Label\n---\nBody", "Label"),
            ("Body", "helper"),
        ] {
            let effective = format!("---\nname: helper\ndescription: {description}\nmetadata: {{}}\n---\n\nBody\n");
            verify(effective.as_bytes(), source.as_bytes(), "helper", "1.2.0").unwrap();
            for metadata in ["{extra: instruction}", "{label: forged}", "{version: 9}", "{original_name: forged}",
                "{imported_from: 'two words'}", "{source_path: \"line\\nnext\"}"] {
                assert!(verify(effective.replace("metadata: {}", &format!("metadata: {metadata}")).as_bytes(),
                    source.as_bytes(), "helper", "1.2.0").is_err());
            }
        }
    }
}
