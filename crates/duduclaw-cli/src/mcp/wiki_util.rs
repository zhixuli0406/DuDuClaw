use super::*;

/// Reserved wiki filenames that cannot be overwritten by wiki_write.
pub(crate) const WIKI_RESERVED: &[&str] = &["_schema.md", "_index.md", "_log.md"];

/// Maximum wiki page size (512 KB).
pub(crate) const WIKI_MAX_PAGE_SIZE: usize = 512 * 1024;

/// Default wiki _schema.md content.
pub(crate) const WIKI_DEFAULT_SCHEMA: &str = r#"# Wiki Schema

## Directory Structure
- `entities/` — People, organizations, products, customers
- `concepts/` — Domain concepts, processes, principles
- `sources/` — Summaries of raw source materials
- `synthesis/` — Cross-topic analysis, comparisons, trends

## Page Format
Every page MUST have YAML frontmatter:
```yaml
---
title: <page title>
created: <ISO 8601>
updated: <ISO 8601>
tags: [tag1, tag2]
related: [path/to/related1.md, path/to/related2.md]
sources: [source1, source2]
---
```

## Naming Convention
- Filename: kebab-case (e.g. `wang-ming-customer.md`)
- Entity pages: `entities/{name}.md`
- Concept pages: `concepts/{topic}.md`
- Source summaries: `sources/{date}-{title}.md`
- Synthesis: `synthesis/{topic}.md`

## Cross-Reference Format
Use relative markdown links: `[Display Text](../concepts/topic.md)`

## Operations
### Ingest (adding new source)
1. Read the source material
2. Create `sources/{date}-{title}.md` summary
3. Update or create relevant entity/concept pages
4. Update `_index.md` with new pages
5. Check for contradictions with existing pages

### Query (answering questions)
1. Read `_index.md` to locate relevant pages
2. Read relevant pages
3. Synthesize answer
4. If answer is valuable, file as new `synthesis/` page

### Lint (health check)
1. Find contradictions between pages
2. Find orphan pages (not in _index.md or no inbound links)
3. Find stale pages (not updated in >30 days, related sources newer)
4. Suggest missing pages for mentioned-but-uncreated entities
"#;

/// Resolve the wiki directory for an agent, creating it if needed.
/// Returns `Err` on invalid agent_id or filesystem failures.
///
/// BUG-QA-003: External MCP clients (e.g. claude-desktop) may not have an agent
/// directory on first connect. Auto-create it so wiki operations work immediately
/// without requiring manual provisioning.
///
/// W3-3b (a): `agent_id` is the caller for every `wiki_*` tool. An `eph-*`
/// role member's directory lives under `agents/.ephemeral/<id>/`, so the bare
/// registry path did two wrong things at once — it wrote the member's wiki
/// outside its own scaffold, and, because this function *creates* what it
/// cannot find, it minted a bogus `agents/eph-…/` registry directory that
/// outlived the scaffold's GC. An ephemeral id that does not resolve is now
/// refused rather than materialised: the only legitimate `.ephemeral/` writer
/// is a live scaffold.
pub(crate) fn resolve_wiki_dir(
    home_dir: &Path,
    agent_id: &str,
) -> std::result::Result<std::path::PathBuf, String> {
    if !is_valid_agent_id(agent_id) {
        return Err("Invalid agent_id".to_string());
    }
    let agent_dir = match duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, agent_id) {
        Some(dir) => dir,
        None if duduclaw_gateway::ephemeral::is_ephemeral_id(agent_id) => {
            return Err(format!(
                "ephemeral agent '{agent_id}' has no live scaffold — refusing to create a \
                 registry directory for it"
            ));
        }
        None => {
            let dir = home_dir.join("agents").join(agent_id);
            if !dir.exists() {
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("Failed to create agent dir for '{}': {}", agent_id, e))?;
            }
            dir
        }
    };
    Ok(agent_dir.join("wiki"))
}

/// Check whether `caller_agent` is allowed to read `target_agent`'s wiki.
///
/// Returns `Ok(true)` if access is allowed, `Ok(false)` if denied.
/// Always allows self-access (caller == target).
pub(crate) fn check_wiki_visibility(
    home_dir: &Path,
    target_agent: &str,
    caller_agent: &str,
) -> std::result::Result<bool, String> {
    // Self-access always allowed
    if caller_agent == target_agent {
        return Ok(true);
    }

    // W3-3b (b): the target may be an `eph-*` role member — resolve the same
    // way [`resolve_wiki_dir`] does, so the page's owner and the page's ACL
    // are read from one directory rather than two.
    let agent_toml_path = agent_dir_for_id(home_dir, target_agent).join("agent.toml");
    let toml_content = match std::fs::read_to_string(&agent_toml_path) {
        Ok(c) => c,
        Err(_) => return Ok(true), // If agent.toml unreadable, default to open (backward compat)
    };

    // Parse wiki_visible_to from [capabilities] section
    // Simple TOML field extraction without a full parser
    let visible_to = extract_toml_string_array(&toml_content, "wiki_visible_to");

    // If field is absent, default to ["*"] (backward compatible)
    let visible_to = match visible_to {
        Some(v) => v,
        None => return Ok(true),
    };

    // ["*"] means all agents can read
    if visible_to.iter().any(|v| v == "*") {
        return Ok(true);
    }

    // Empty list means fully private
    if visible_to.is_empty() {
        return Ok(false);
    }

    // Check if caller is in the list
    Ok(visible_to.iter().any(|v| v == caller_agent))
}

/// Extract a string array value from TOML content (simple parser, no dependency).
/// Handles format: `field_name = ["a", "b", "c"]`
pub(crate) fn extract_toml_string_array(content: &str, field: &str) -> Option<Vec<String>> {
    let prefix = format!("{} = ", field);
    let alt_prefix = format!("{}=", field);
    for line in content.lines() {
        let trimmed = line.trim();
        let rest = if let Some(r) = trimmed.strip_prefix(&prefix) {
            r
        } else if let Some(r) = trimmed.strip_prefix(&alt_prefix) {
            r
        } else {
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with('[') && rest.ends_with(']') {
            let inner = &rest[1..rest.len() - 1];
            let items: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return Some(items);
        }
    }
    None
}

/// Ensure the wiki directory structure exists, creating scaffold if needed.
pub(crate) fn ensure_wiki_dir(wiki_dir: &Path) -> std::result::Result<(), String> {
    let subdirs = ["entities", "concepts", "sources", "synthesis"];
    for sub in &subdirs {
        let p = wiki_dir.join(sub);
        if !p.exists() {
            std::fs::create_dir_all(&p)
                .map_err(|e| format!("Failed to create {}: {e}", p.display()))?;
        }
    }

    // Create _schema.md if missing
    let schema_path = wiki_dir.join("_schema.md");
    if !schema_path.exists() {
        std::fs::write(&schema_path, WIKI_DEFAULT_SCHEMA)
            .map_err(|e| format!("Failed to write _schema.md: {e}"))?;
    }

    // Create _index.md if missing
    let index_path = wiki_dir.join("_index.md");
    if !index_path.exists() {
        std::fs::write(
            &index_path,
            "# Wiki Index\n\n<!-- Auto-maintained by wiki_write. One entry per page. -->\n",
        )
        .map_err(|e| format!("Failed to write _index.md: {e}"))?;
    }

    // Create _log.md if missing
    let log_path = wiki_dir.join("_log.md");
    if !log_path.exists() {
        std::fs::write(
            &log_path,
            "# Wiki Log\n\n<!-- Append-only operation log. -->\n",
        )
        .map_err(|e| format!("Failed to write _log.md: {e}"))?;
    }

    Ok(())
}

/// Required frontmatter fields for Karpathy-style LLM wiki pages.
pub(crate) const WIKI_REQUIRED_FIELDS: &[&str] = &["title", "created", "updated", "tags", "layer", "trust"];

/// Regex-free fallback phrases that indicate a page was authored from a stale
/// LLM prior (e.g. web_search tool failure) rather than live evidence. These
/// are noise in the shared wiki per project rule:
///   「有 fallback 的資料不應該混入共用 wiki 中產生雜訊」
pub(crate) const WIKI_FALLBACK_MARKERS: &[&str] = &[
    "無法取得",
    "web_search 失敗",
    "web_search failed",
    "no results found",
    "基於訓練資料",
    "基於我的訓練資料",
    "based on training data",
    "based on my training data",
    "fallback 資料",
    "fallback mode",
    "查無結果",
    "搜尋工具失效",
    "cannot fetch",
    "unable to fetch",
];

/// Scan body for fallback markers. Returns the matched marker, if any.
/// Lowercase comparison for ASCII markers; direct substring for CJK.
pub(crate) fn detect_fallback_content(body: &str) -> Option<&'static str> {
    let lower = body.to_lowercase();
    for marker in WIKI_FALLBACK_MARKERS {
        let marker_lower = marker.to_lowercase();
        if lower.contains(&marker_lower) {
            return Some(marker);
        }
    }
    None
}

/// Validate Karpathy-style frontmatter. Returns a list of missing fields.
/// Caller decides whether missing fields are fatal (shared wiki) or warn-only.
pub(crate) fn validate_wiki_frontmatter(content: &str) -> std::result::Result<(), String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Err(
            "Missing YAML frontmatter. Every page must start with `---` and declare: \
             title, created, updated, tags, layer, trust."
                .to_string(),
        );
    }
    let rest = &trimmed[3..];
    let end = match rest.find("\n---") {
        Some(e) => e,
        None => return Err("Frontmatter is not closed with a trailing `---`.".to_string()),
    };
    let fm = &rest[..end];

    let mut missing: Vec<&str> = Vec::new();
    for field in WIKI_REQUIRED_FIELDS {
        let prefix = format!("{}:", field);
        let found = fm
            .lines()
            .any(|line| line.trim_start().starts_with(&prefix));
        if !found {
            missing.push(field);
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "Frontmatter missing required field(s): {}. \
             See _schema.md; the Karpathy wiki schema requires all of: {}.",
            missing.join(", "),
            WIKI_REQUIRED_FIELDS.join(", ")
        ));
    }

    // Trust must parse as a number in [0.0, 1.0]
    if let Some(raw) = extract_frontmatter_field(content, "trust") {
        match raw.parse::<f32>() {
            Ok(t) if (0.0..=1.0).contains(&t) => {}
            Ok(t) => {
                return Err(format!(
                    "Frontmatter `trust` must be in [0.0, 1.0], got {t}"
                ));
            }
            Err(_) => return Err(format!("Frontmatter `trust` must be a number, got `{raw}`")),
        }
    }
    Ok(())
}

/// Validate a wiki page path: no traversal, must end with .md, not reserved.
pub(crate) fn validate_wiki_page_path(page_path: &str) -> std::result::Result<(), String> {
    if page_path.is_empty() {
        return Err("page_path is required".to_string());
    }
    if page_path.contains("..") {
        return Err("Path traversal (..) is not allowed".to_string());
    }
    if page_path.starts_with('/') || page_path.starts_with('\\') {
        return Err("Absolute paths are not allowed".to_string());
    }
    if !page_path.ends_with(".md") {
        return Err("Page path must end with .md".to_string());
    }
    // Check reserved filenames
    let filename = std::path::Path::new(page_path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    if WIKI_RESERVED.contains(&filename) {
        return Err(format!(
            "'{}' is a reserved wiki file and cannot be overwritten",
            filename
        ));
    }
    Ok(())
}

/// Extract title from YAML frontmatter (best-effort, no YAML parser dependency).
pub(crate) fn extract_frontmatter_title(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    // Find the closing ---
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix("title:") {
            let title = after.trim().trim_matches('"').trim_matches('\'');
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Extract the updated field from YAML frontmatter.
pub(crate) fn extract_frontmatter_updated(content: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix("updated:") {
            let val = after.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Update _index.md with an entry for a page.
/// Format: `- [{title}]({page_path}) — updated {date}`
pub(crate) fn update_wiki_index(
    wiki_dir: &Path,
    page_path: &str,
    title: &str,
) -> std::result::Result<(), String> {
    let index_path = wiki_dir.join("_index.md");
    let existing = std::fs::read_to_string(&index_path).unwrap_or_default();

    // Build the new entry line
    let now = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let entry_line = format!("- [{}]({}) — updated {}", title, page_path, now);

    // Check if this page already has an entry and replace it
    let link_pattern = format!("]({})", page_path);
    let mut lines: Vec<String> = existing.lines().map(String::from).collect();
    let mut found = false;
    for line in &mut lines {
        if line.contains(&link_pattern) {
            *line = entry_line.clone();
            found = true;
            break;
        }
    }

    if !found {
        lines.push(entry_line);
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(&index_path, new_content).map_err(|e| format!("Failed to update _index.md: {e}"))
}

/// Append a log entry to _log.md.
pub(crate) fn append_wiki_log(
    wiki_dir: &Path,
    action: &str,
    page_path: &str,
) -> std::result::Result<(), String> {
    let log_path = wiki_dir.join("_log.md");
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let entry = format!("## [{}] {} | {}\n", now, action, page_path);

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("Failed to open _log.md: {e}"))?;
    f.write_all(entry.as_bytes())
        .map_err(|e| format!("Failed to append to _log.md: {e}"))?;
    Ok(())
}

/// Collect all .md files under `dir` recursively (relative to `base`).
pub(crate) fn collect_md_files(base: &Path, dir: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_md_files(base, &path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Ok(rel) = path.strip_prefix(base)
        {
            // Skip reserved files
            let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            if !WIKI_RESERVED.contains(&fname) {
                result.push(rel.to_path_buf());
            }
        }
    }
    result
}

/// Extract a named field from frontmatter (helper for shared wiki).
pub(crate) fn extract_frontmatter_field(content: &str, field: &str) -> Option<String> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let prefix = format!("{}:", field);
    for line in fm.lines() {
        let line = line.trim();
        if let Some(after) = line.strip_prefix(&prefix) {
            let val = after.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Extract body text (after frontmatter closing `---`).
pub(crate) fn extract_frontmatter_body(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }
    let rest = &trimmed[3..];
    if let Some(end) = rest.find("\n---") {
        let after = &rest[end + 4..];
        after.trim_start_matches('\n').to_string()
    } else {
        content.to_string()
    }
}
