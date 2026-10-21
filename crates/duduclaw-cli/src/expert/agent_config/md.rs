//! Small, dependency-free readers shared by the adapters: bounded text
//! reads, lenient frontmatter, OpenClaw-style `- Label: value` identity
//! lines and memory-fact extraction.

use std::collections::BTreeMap;
use std::path::Path;

/// Largest single text file an adapter reads (persona files are tiny; a
/// 27 KB AGENTS.md is the biggest real sample). Larger files are left for
/// the unmapped archive instead of being loaded into a prompt slot.
pub const MAX_TEXT_BYTES: u64 = 512 * 1024;

/// Read a UTF-8 text file (BOM stripped). `None` when absent, too large, or
/// not valid UTF-8 — callers report those as unmapped, never as empty.
pub fn read_text(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_TEXT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(
        text.strip_prefix('\u{feff}')
            .map(str::to_string)
            .unwrap_or(text),
    )
}

/// `path` relative to `root`, `/`-separated, for reports and lineage.
pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Frontmatter as plain string fields plus the raw YAML value (when it
/// parsed). Real-world files are often not valid YAML (豆包 SKILL.md has
/// unbalanced quotes), so a line-based `key: value` reading is always kept.
#[derive(Debug, Clone, Default)]
pub struct FrontMatter {
    pub fields: BTreeMap<String, String>,
    pub yaml: Option<serde_yaml::Value>,
}

impl FrontMatter {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .map(String::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    }

    /// A list-or-comma-string field (`tools: Read, Grep` or a YAML list).
    pub fn list(&self, key: &str) -> Option<Vec<String>> {
        if let Some(serde_yaml::Value::Sequence(seq)) = self.yaml.as_ref().and_then(|y| y.get(key))
        {
            return Some(
                seq.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
        self.get(key).map(|s| {
            s.trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|t| t.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
    }
}

/// Split `---` frontmatter from the body. `(None, whole)` when the document
/// does not open with a fence or never closes it.
pub fn split_frontmatter(text: &str) -> (Option<FrontMatter>, String) {
    let normalized = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = normalized.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, text.to_string());
    }
    let mut head: Vec<&str> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed && line.trim_end() == "---" {
            closed = true;
            continue;
        }
        if closed {
            body.push(line);
        } else {
            head.push(line);
        }
    }
    if !closed {
        return (None, text.to_string());
    }
    let yaml_src = head.join("\n");
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&yaml_src)
        .ok()
        .filter(|v| v.is_mapping());
    let mut fields = BTreeMap::new();
    for line in &head {
        if line.starts_with(' ') || line.starts_with('\t') || line.trim_start().starts_with('-') {
            continue; // nested / list item — only the YAML view has it
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            if key.is_empty() || key.contains(' ') {
                continue;
            }
            fields.insert(key, unquote(v.trim()));
        }
    }
    // Prefer the parsed YAML scalar when there is one (multi-line strings).
    if let Some(serde_yaml::Value::Mapping(map)) = &yaml {
        for (k, v) in map {
            if let (Some(k), Some(v)) = (k.as_str(), v.as_str()) {
                fields.insert(k.to_string(), v.to_string());
            }
        }
    }
    let body = body.join("\n").trim_start_matches('\n').to_string();
    (Some(FrontMatter { fields, yaml }), body)
}

/// Strip one layer of quotes, tolerating a missing closing quote.
fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(rest) = v.strip_prefix(q) {
            return rest.strip_suffix(q).unwrap_or(rest).to_string();
        }
    }
    v.to_string()
}

/// What an IDENTITY.md says about name / emoji / short description.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityFields {
    pub name: Option<String>,
    pub emoji: Option<String>,
    pub vibe: Option<String>,
}

/// Read an IDENTITY.md in either shape seen in the wild:
/// - OpenClaw template lines `- Name: …`, `- Emoji: …`, `- Vibe: …`
///   (labels case-insensitive, placeholder text ignored);
/// - 豆包 persona: `# IDENTITY.md - <name>` heading and a `## 簽名`
///   section whose first token is the emoji.
pub fn parse_identity(text: &str) -> IdentityFields {
    let mut out = IdentityFields::default();
    let mut in_signature = false;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(h) = line.strip_prefix("## ") {
            in_signature = matches!(h.trim(), "簽名" | "签名" | "Signature");
            continue;
        }
        if let Some(h) = line.strip_prefix("# ") {
            if out.name.is_none()
                && let Some((_, name)) = h.split_once(" - ")
                && !name.trim().is_empty()
            {
                out.name = Some(name.trim().to_string());
            }
            continue;
        }
        if in_signature && !line.is_empty() && out.emoji.is_none() {
            if let Some(first) = line.split_whitespace().next()
                && is_emoji_token(first)
            {
                out.emoji = Some(first.to_string());
            }
            continue;
        }
        let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) else {
            continue;
        };
        let Some((label, value)) = item.split_once(':') else {
            continue;
        };
        let label = label.trim().trim_matches('*').to_ascii_lowercase();
        let value = value.trim().trim_matches('*').trim();
        if value.is_empty() || is_placeholder(value) {
            continue;
        }
        match label.as_str() {
            "name" => out.name = Some(value.to_string()),
            "emoji" if is_emoji_token(value.split_whitespace().next().unwrap_or("")) => {
                out.emoji = value.split_whitespace().next().map(str::to_string)
            }
            "vibe" => out.vibe = Some(value.to_string()),
            _ => {}
        }
    }
    out
}

fn is_placeholder(v: &str) -> bool {
    (v.starts_with('(') && v.ends_with(')'))
        || (v.starts_with("_(") && v.ends_with(")_"))
        || (v.starts_with('<') && v.ends_with('>'))
        || v.starts_with("（")
}

/// A short token made only of non-alphanumeric symbols (emoji, possibly
/// with variation selectors / ZWJ). Keeps CJK words out of the icon field.
pub fn is_emoji_token(tok: &str) -> bool {
    let n = tok.chars().count();
    (1..=8).contains(&n)
        && tok.chars().any(|c| (c as u32) >= 0x2190)
        && tok
            .chars()
            .all(|c| !c.is_alphanumeric() && !c.is_whitespace() && !c.is_ascii_punctuation())
}

/// Most facts one memory file contributes (a runaway log stays bounded).
pub const MAX_FACTS_PER_FILE: usize = 300;
/// Longest single fact kept (chars).
pub const MAX_FACT_CHARS: usize = 1000;

/// Memory facts from a Markdown memory file: its bullet lines, or — when the
/// file has none — its paragraphs. Headings and template placeholders are
/// skipped; Hermes' `§` entry separator is honoured.
pub fn extract_facts(text: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(s) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            push_fact(&mut bullets, s);
        }
    }
    if !bullets.is_empty() {
        return bullets;
    }
    let mut paras = Vec::new();
    for block in text.split("\n\n").flat_map(|b| b.split('§')) {
        let lines: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && *l != "---")
            .collect();
        if !lines.is_empty() {
            push_fact(&mut paras, &lines.join(" "));
        }
    }
    paras
}

fn push_fact(out: &mut Vec<String>, raw: &str) {
    let s = raw.trim();
    if s.is_empty() || is_placeholder(s) || out.len() >= MAX_FACTS_PER_FILE {
        return;
    }
    out.push(duduclaw_core::truncate_chars(s, MAX_FACT_CHARS));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_tolerates_broken_yaml_quotes() {
        let (fm, body) = split_frontmatter(
            "---\nname: doubao-book-writer\nlabel: \"長文件寫作\"\nicon: \"公里的工作臺。\n---\n# Body\n",
        );
        let fm = fm.unwrap();
        assert!(fm.yaml.is_none(), "unbalanced quote → YAML fails");
        assert_eq!(fm.get("name"), Some("doubao-book-writer"));
        assert_eq!(fm.get("label"), Some("長文件寫作"));
        assert_eq!(fm.get("icon"), Some("公里的工作臺。"));
        assert_eq!(body, "# Body");
    }

    #[test]
    fn frontmatter_lists_from_string_or_sequence() {
        let (fm, _) = split_frontmatter("---\ntools: Read, Grep\n---\nx");
        assert_eq!(fm.unwrap().list("tools").unwrap(), vec!["Read", "Grep"]);
        let (fm, _) = split_frontmatter("---\ntools:\n  - search\n  - edit\n---\nx");
        assert_eq!(fm.unwrap().list("tools").unwrap(), vec!["search", "edit"]);
        let (none, whole) = split_frontmatter("no fence");
        assert!(none.is_none());
        assert_eq!(whole, "no fence");
    }

    #[test]
    fn identity_openclaw_template_lines() {
        let id = parse_identity(
            "# IDENTITY.md\n- **Name:** Molty\n- Creature: lobster\n- Vibe: dry wit\n- Emoji: 🦞\n- Avatar: _(path)_\n",
        );
        assert_eq!(id.name.as_deref(), Some("Molty"));
        assert_eq!(id.emoji.as_deref(), Some("🦞"));
        assert_eq!(id.vibe.as_deref(), Some("dry wit"));
        let blank = parse_identity("- Name: (pick something)\n- Emoji:\n");
        assert_eq!(blank, IdentityFields::default());
    }

    #[test]
    fn identity_doubao_heading_and_signature() {
        let id = parse_identity("# IDENTITY.md - 報告顧問\n\n## 簽名\n\n🧭 報告顧問\n");
        assert_eq!(id.name.as_deref(), Some("報告顧問"));
        assert_eq!(id.emoji.as_deref(), Some("🧭"));
    }

    #[test]
    fn facts_prefer_bullets_then_paragraphs() {
        assert_eq!(
            extract_facts("# M\n- 喜歡簡短\n- 週一開會\n"),
            vec!["喜歡簡短", "週一開會"]
        );
        assert_eq!(
            extract_facts("# M\n\n客戶在台北。\n\n§ 偏好 LINE 聯絡\n"),
            vec!["客戶在台北。", "偏好 LINE 聯絡"]
        );
        assert!(extract_facts("（使用者是誰…）\n").is_empty());
    }

    #[test]
    fn emoji_tokens_exclude_words() {
        assert!(is_emoji_token("🧭"));
        assert!(is_emoji_token("👩‍💻"));
        assert!(!is_emoji_token("戰略"));
        assert!(!is_emoji_token("abc"));
        assert!(!is_emoji_token(""));
    }
}
