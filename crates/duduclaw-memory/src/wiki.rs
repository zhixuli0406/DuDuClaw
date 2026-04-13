//! Wiki Knowledge Base — structured markdown page management.
//!
//! Based on [Karpathy's LLM Wiki pattern](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f).
//! Each agent maintains a `wiki/` directory of interlinked markdown files.
//! The `WikiStore` handles reading, writing, indexing, and health-checking pages.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_core::error::{DuDuClawError, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A parsed wiki page with frontmatter and body separated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    /// Path relative to `wiki/` (e.g. `entities/wang-ming.md`).
    pub path: String,
    pub title: String,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub tags: Vec<String>,
    /// Paths to related pages (relative to `wiki/`).
    pub related: Vec<String>,
    /// Source identifiers (free-form).
    pub sources: Vec<String>,
    /// Markdown body (without frontmatter).
    pub body: String,
}

/// Minimal metadata extracted from a page's frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageMeta {
    pub path: String,
    pub title: String,
    pub updated: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// A search hit with relevance score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub score: usize,
    /// Up to 3 matching lines for context.
    pub context_lines: Vec<String>,
}

/// A contradiction detected between two pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub page_a: String,
    pub page_b: String,
    pub description: String,
}

/// Result of a wiki lint operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LintReport {
    pub orphan_pages: Vec<String>,
    pub broken_links: Vec<(String, String)>,
    pub stale_pages: Vec<String>,
    pub total_pages: usize,
    pub index_entries: usize,
}

/// Proposed wiki change (used by GVU integration in Phase 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiProposal {
    pub page_path: String,
    pub action: WikiAction,
    pub content: Option<String>,
    pub rationale: String,
    pub related_pages: Vec<String>,
}

/// Action type for a wiki proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WikiAction {
    Create,
    Update,
    Delete,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Reserved wiki filenames that cannot be overwritten as regular pages.
const RESERVED_FILES: &[&str] = &["_schema.md", "_index.md", "_log.md"];

/// Maximum page content size (512 KB).
const MAX_PAGE_SIZE: usize = 512 * 1024;

/// Standard subdirectories for a wiki.
const WIKI_SUBDIRS: &[&str] = &["entities", "concepts", "sources", "synthesis"];

/// Maximum directory recursion depth for file scanning.
const MAX_RECURSION_DEPTH: usize = 20;

// ---------------------------------------------------------------------------
// WikiStore
// ---------------------------------------------------------------------------

/// File-system backed wiki store for a single agent.
pub struct WikiStore {
    /// Root wiki directory (e.g. `~/.duduclaw/agents/agnes/wiki/`).
    wiki_dir: PathBuf,
}

impl WikiStore {
    /// Open a wiki store at the given directory.
    /// Does NOT create the directory — call `ensure_scaffold()` first if needed.
    pub fn new(wiki_dir: PathBuf) -> Self {
        Self { wiki_dir }
    }

    /// Return the wiki root directory.
    pub fn wiki_dir(&self) -> &Path {
        &self.wiki_dir
    }

    /// Create the wiki directory scaffold (subdirs + reserved files) if missing.
    pub fn ensure_scaffold(&self) -> Result<()> {
        // create_dir_all is idempotent — safe for concurrent callers
        for sub in WIKI_SUBDIRS {
            let p = self.wiki_dir.join(sub);
            std::fs::create_dir_all(&p)
                .map_err(|e| DuDuClawError::Memory(format!("create dir {}: {e}", p.display())))?;
        }

        // Use create_new to avoid TOCTOU — only the first caller writes, others get AlreadyExists (ignored)
        let scaffold_files: &[(&str, &str)] = &[
            ("_schema.md", default_schema()),
            ("_index.md", "# Wiki Index\n\n<!-- Auto-maintained by WikiStore. One entry per page. -->\n"),
            ("_log.md", "# Wiki Log\n\n<!-- Append-only operation log. -->\n"),
        ];
        for (name, content) in scaffold_files {
            let path = self.wiki_dir.join(name);
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(content.as_bytes())
                        .map_err(|e| DuDuClawError::Memory(format!("write {name}: {e}")))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Another caller already created it — expected in concurrent scenarios
                }
                Err(e) => {
                    return Err(DuDuClawError::Memory(format!("create {name}: {e}")));
                }
            }
        }

        info!(wiki_dir = %self.wiki_dir.display(), "Wiki scaffold ensured");
        Ok(())
    }

    // ── Read operations ─────────────────────────────────────────

    /// Read and parse a wiki page from disk.
    pub fn read_page(&self, path: &str) -> Result<WikiPage> {
        self.validate_page_path(path)?;
        let full = self.wiki_dir.join(path);
        let content = std::fs::read_to_string(&full)
            .map_err(|e| DuDuClawError::Memory(format!("read {path}: {e}")))?;
        parse_wiki_page(path, &content)
    }

    /// Read raw content of any wiki file (including reserved files).
    pub fn read_raw(&self, path: &str) -> Result<String> {
        if path.is_empty() || path.contains("..") || path.starts_with('/') || path.starts_with('\\') || path.contains('\0')
            || path.contains("%2e") || path.contains("%2E") || path.contains("%2f") || path.contains("%2F")
        {
            return Err(DuDuClawError::Memory("path traversal not allowed".into()));
        }
        let full = self.wiki_dir.join(path);
        // Canonicalize check — prevent symlink escape
        if full.exists()
            && let (Ok(canon_full), Ok(canon_wiki)) = (full.canonicalize(), self.wiki_dir.canonicalize())
                && !canon_full.starts_with(&canon_wiki) {
                    return Err(DuDuClawError::Memory("resolved path escapes wiki directory".into()));
                }
        std::fs::read_to_string(&full)
            .map_err(|e| DuDuClawError::Memory(format!("read {path}: {e}")))
    }

    /// List metadata for all pages (excluding reserved files).
    pub fn list_pages(&self) -> Result<Vec<PageMeta>> {
        let md_files = collect_md_files_recursive(&self.wiki_dir, &self.wiki_dir);
        let mut pages = Vec::with_capacity(md_files.len());

        for rel in md_files {
            let rel_str = rel.to_string_lossy().to_string();
            let full = self.wiki_dir.join(&rel);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let title = extract_title(&content).unwrap_or_else(|| rel_str.clone());
            let updated = extract_datetime_field(&content, "updated")
                .unwrap_or_else(Utc::now);
            let tags = extract_string_list(&content, "tags");

            pages.push(PageMeta {
                path: rel_str,
                title,
                updated,
                tags,
            });
        }

        pages.sort_by(|a, b| b.updated.cmp(&a.updated));
        Ok(pages)
    }

    // ── Write operations ────────────────────────────────────────

    /// Write a page to disk with atomic write (temp + rename).
    /// Automatically updates `_index.md` and appends to `_log.md`.
    pub fn write_page(&self, path: &str, content: &str) -> Result<()> {
        self.validate_page_path(path)?;

        if content.len() > MAX_PAGE_SIZE {
            return Err(DuDuClawError::Memory(format!(
                "page too large: {} bytes (max {})",
                content.len(),
                MAX_PAGE_SIZE
            )));
        }

        let full = self.wiki_dir.join(path);

        // Ensure parent directory
        if let Some(parent) = full.parent()
            && !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DuDuClawError::Memory(format!("create dir: {e}")))?;
            }

        let is_new = !full.exists();

        // Atomic write
        let tmp = full.with_extension("md.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| DuDuClawError::Memory(format!("write temp: {e}")))?;
        std::fs::rename(&tmp, &full).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            DuDuClawError::Memory(format!("rename temp: {e}"))
        })?;

        // Update index
        let title = extract_title(content).unwrap_or_else(|| path.to_string());
        if let Err(e) = self.update_index(path, &title) {
            warn!("Failed to update index for {path}: {e}");
        }

        // Append log
        let action = if is_new { "create" } else { "update" };
        if let Err(e) = self.append_log(action, path) {
            warn!("Failed to log {action} {path}: {e}");
        }

        info!(page = path, action, "Wiki page written");
        Ok(())
    }

    /// Delete a page from disk.
    pub fn delete_page(&self, path: &str) -> Result<()> {
        self.validate_page_path(path)?;
        let full = self.wiki_dir.join(path);
        if full.exists() {
            std::fs::remove_file(&full)
                .map_err(|e| DuDuClawError::Memory(format!("delete {path}: {e}")))?;
            if let Err(e) = self.remove_from_index(path) {
                warn!("Failed to remove {path} from index: {e}");
            }
            if let Err(e) = self.append_log("delete", path) {
                warn!("Failed to log delete {path}: {e}");
            }
            info!(page = path, "Wiki page deleted");
        }
        Ok(())
    }

    // ── Search ──────────────────────────────────────────────────

    /// Full-text keyword search across all wiki pages.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let md_files = collect_md_files_recursive(&self.wiki_dir, &self.wiki_dir);
        let mut hits: Vec<SearchHit> = Vec::new();

        for rel in &md_files {
            let full = self.wiki_dir.join(rel);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let content_lower = content.to_lowercase();
            let score: usize = terms.iter().filter(|t| content_lower.contains(t.as_str())).count();
            if score == 0 {
                continue;
            }

            let title = extract_title(&content)
                .unwrap_or_else(|| rel.to_string_lossy().to_string());

            let context_lines: Vec<String> = content
                .lines()
                .filter(|line| {
                    let ll = line.to_lowercase();
                    terms.iter().any(|t| ll.contains(t.as_str()))
                })
                .take(3)
                .map(|l| {
                    let trimmed = l.trim();
                    // Use char-based truncation to avoid UTF-8 boundary panic
                    if trimmed.chars().count() > 150 {
                        let truncated: String = trimmed.chars().take(147).collect();
                        format!("{truncated}...")
                    } else {
                        trimmed.to_string()
                    }
                })
                .collect();

            hits.push(SearchHit {
                path: rel.to_string_lossy().to_string(),
                title,
                score,
                context_lines,
            });
        }

        hits.sort_by(|a, b| b.score.cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }

    // ── Index management ────────────────────────────────────────

    /// Rebuild `_index.md` from scratch by scanning all pages.
    pub fn rebuild_index(&self) -> Result<usize> {
        let pages = self.list_pages()?;
        let mut lines = vec!["# Wiki Index".to_string(), String::new()];
        lines.push("<!-- Auto-maintained by WikiStore. One entry per page. -->".to_string());
        lines.push(String::new());

        for page in &pages {
            let date = page.updated.format("%Y-%m-%d").to_string();
            lines.push(format!("- [{}]({}) — updated {}", page.title, page.path, date));
        }

        let content = lines.join("\n") + "\n";
        let index_path = self.wiki_dir.join("_index.md");
        std::fs::write(&index_path, content)
            .map_err(|e| DuDuClawError::Memory(format!("rebuild index: {e}")))?;

        info!(pages = pages.len(), "Wiki index rebuilt");
        Ok(pages.len())
    }

    // ── Export ───────────────────────────────────────────────────

    /// Export the wiki as an Obsidian-compatible vault to `output_dir`.
    ///
    /// Copies all pages + reserved files. Converts `related:` frontmatter
    /// into Obsidian `[[wikilinks]]` in the body for graph view compatibility.
    pub fn export_obsidian(&self, output_dir: &Path) -> Result<usize> {
        let pages = self.list_pages()?;
        let mut exported = 0;

        // Copy reserved files
        for reserved in &["_schema.md", "_index.md", "_log.md"] {
            let src = self.wiki_dir.join(reserved);
            if src.exists() {
                let dst = output_dir.join(reserved);
                std::fs::copy(&src, &dst)
                    .map_err(|e| DuDuClawError::Memory(format!("copy {reserved}: {e}")))?;
            }
        }

        for page in &pages {
            let src = self.wiki_dir.join(&page.path);
            let dst = output_dir.join(&page.path);

            // Ensure parent dir
            if let Some(parent) = dst.parent()
                && !parent.exists() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| DuDuClawError::Memory(format!("mkdir: {e}")))?;
                }

            let content = std::fs::read_to_string(&src)
                .map_err(|e| DuDuClawError::Memory(format!("read {}: {e}", page.path)))?;

            // Append Obsidian wikilinks for related pages
            let related = extract_string_list(&content, "related");
            let _body = extract_body(&content);
            if related.is_empty() {
                std::fs::write(&dst, &content)
                    .map_err(|e| DuDuClawError::Memory(format!("write export: {e}")))?;
            } else {
                let wikilinks: Vec<String> = related.iter().map(|r| {
                    let name = Path::new(r)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(r);
                    format!("[[{}]]", name)
                }).collect();
                let augmented = format!(
                    "{}\n\n---\n**Related:** {}\n",
                    content.trim_end(),
                    wikilinks.join(", ")
                );
                std::fs::write(&dst, augmented)
                    .map_err(|e| DuDuClawError::Memory(format!("write export: {e}")))?;
            }

            exported += 1;
        }

        info!(exported, output = %output_dir.display(), "Wiki exported as Obsidian vault");
        Ok(exported)
    }

    /// Export the wiki as a single HTML file.
    ///
    /// Produces a self-contained HTML document with basic styling.
    pub fn export_html(&self) -> Result<String> {
        let pages = self.list_pages()?;
        let mut html = String::from(
            "<!DOCTYPE html>\n<html lang=\"zh-TW\">\n<head>\n\
             <meta charset=\"utf-8\">\n\
             <title>Wiki Export</title>\n\
             <style>\n\
             body { font-family: -apple-system, system-ui, sans-serif; max-width: 900px; margin: 0 auto; padding: 2rem; color: #1c1917; }\n\
             h1 { border-bottom: 2px solid #f59e0b; padding-bottom: 0.5rem; }\n\
             h2 { color: #78716c; margin-top: 2rem; }\n\
             .page { border: 1px solid #e7e5e4; border-radius: 12px; padding: 1.5rem; margin: 1rem 0; }\n\
             .page h3 { margin-top: 0; color: #1c1917; }\n\
             .meta { font-size: 0.8rem; color: #a8a29e; margin-bottom: 0.5rem; }\n\
             .tags span { background: #fef3c7; color: #92400e; padding: 2px 8px; border-radius: 9999px; font-size: 0.75rem; margin-right: 4px; }\n\
             pre { background: #fafaf9; padding: 1rem; border-radius: 8px; overflow-x: auto; }\n\
             </style>\n</head>\n<body>\n\
             <h1>Wiki Knowledge Base</h1>\n"
        );

        // Group by directory
        let mut by_dir: HashMap<String, Vec<&PageMeta>> = HashMap::new();
        for page in &pages {
            let dir = Path::new(&page.path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or("root")
                .to_string();
            by_dir.entry(dir).or_default().push(page);
        }

        let mut dirs: Vec<_> = by_dir.into_iter().collect();
        dirs.sort_by(|a, b| a.0.cmp(&b.0));

        for (dir, dir_pages) in &dirs {
            html.push_str(&format!("<h2>{}/</h2>\n", html_escape(dir)));
            for page in dir_pages {
                let raw = self.read_raw(&page.path).unwrap_or_default();
                let body = extract_body(&raw);
                let tags_html: String = page.tags.iter()
                    .map(|t| format!("<span>{}</span>", html_escape(t)))
                    .collect();

                html.push_str(&format!(
                    "<div class=\"page\">\n\
                     <h3>{}</h3>\n\
                     <div class=\"meta\">{} | {}</div>\n\
                     {}\n\
                     <div>{}</div>\n\
                     </div>\n",
                    html_escape(&page.title),
                    page.updated.format("%Y-%m-%d"),
                    if tags_html.is_empty() { String::new() } else { format!("<div class=\"tags\">{}</div>", tags_html) },
                    "",
                    html_escape_body(&body),
                ));
            }
        }

        html.push_str("</body>\n</html>\n");
        Ok(html)
    }

    // ── Lint / health check ─────────────────────────────────────

    /// Run a health check on the wiki. Returns a lint report.
    pub fn lint(&self) -> Result<LintReport> {
        let pages = self.list_pages()?;
        let page_paths: HashSet<String> = pages.iter().map(|p| p.path.clone()).collect();

        // Parse _index.md for indexed pages
        let index_content = self.read_raw("_index.md").unwrap_or_default();
        let indexed_pages: HashSet<String> = index_content
            .lines()
            .filter_map(|line| {
                let start = line.find("](")?;
                let rest = &line[start + 2..];
                let end = rest.find(')')?;
                Some(rest[..end].to_string())
            })
            .collect();

        // Orphan pages: exist on disk but not in _index.md
        let orphan_pages: Vec<String> = page_paths
            .difference(&indexed_pages)
            .cloned()
            .collect();

        // Broken links: referenced in index but file missing
        let broken_links: Vec<(String, String)> = indexed_pages
            .difference(&page_paths)
            .map(|p| ("_index.md".to_string(), p.clone()))
            .collect();

        // Collect all cross-references from page frontmatter `related` fields
        let mut all_references: Vec<(String, String)> = Vec::new();
        for page in &pages {
            let full = self.wiki_dir.join(&page.path);
            if let Ok(content) = std::fs::read_to_string(&full) {
                let related = extract_string_list(&content, "related");
                for r in related {
                    if !page_paths.contains(&r) {
                        all_references.push((page.path.clone(), r));
                    }
                }
            }
        }

        let mut broken_links = broken_links;
        broken_links.extend(all_references);

        // Stale pages: not updated in 30+ days
        let thirty_days_ago = Utc::now() - chrono::Duration::days(30);
        let stale_pages: Vec<String> = pages
            .iter()
            .filter(|p| p.updated < thirty_days_ago)
            .map(|p| p.path.clone())
            .collect();

        // Sort for deterministic output
        let mut orphan_pages = orphan_pages;
        orphan_pages.sort();
        broken_links.sort();
        let mut stale_pages = stale_pages;
        stale_pages.sort();

        Ok(LintReport {
            orphan_pages,
            broken_links,
            stale_pages,
            total_pages: pages.len(),
            index_entries: indexed_pages.len(),
        })
    }

    /// Find pages with no inbound links (orphans in the graph sense).
    pub fn find_orphans(&self) -> Result<Vec<String>> {
        let pages = self.list_pages()?;
        let page_paths: HashSet<String> = pages.iter().map(|p| p.path.clone()).collect();

        // Collect all outbound links from each page
        let mut inbound: HashMap<String, usize> = HashMap::new();
        for path in &page_paths {
            inbound.entry(path.clone()).or_insert(0);
        }

        for page in &pages {
            let full = self.wiki_dir.join(&page.path);
            if let Ok(content) = std::fs::read_to_string(&full) {
                let related = extract_string_list(&content, "related");
                for r in related {
                    if let Some(count) = inbound.get_mut(&r) {
                        *count += 1;
                    }
                }
                // Also scan body for markdown links to wiki pages
                for link in extract_body_links(&content) {
                    if let Some(count) = inbound.get_mut(&link) {
                        *count += 1;
                    }
                }
            }
        }

        let orphans: Vec<String> = inbound
            .into_iter()
            .filter(|(_, count)| *count == 0)
            .map(|(path, _)| path)
            .collect();

        Ok(orphans)
    }

    // ── Apply proposals (for GVU integration) ─────────────────��─

    /// Apply a batch of wiki proposals. Returns number of successfully applied changes.
    pub fn apply_proposals(&self, proposals: &[WikiProposal]) -> Result<usize> {
        let mut applied = 0;
        for proposal in proposals {
            match proposal.action {
                WikiAction::Create | WikiAction::Update => {
                    if let Some(content) = &proposal.content {
                        match self.write_page(&proposal.page_path, content) {
                            Ok(()) => applied += 1,
                            Err(e) => warn!(
                                page = %proposal.page_path,
                                error = %e,
                                "Failed to apply wiki proposal"
                            ),
                        }
                    }
                }
                WikiAction::Delete => {
                    match self.delete_page(&proposal.page_path) {
                        Ok(()) => applied += 1,
                        Err(e) => warn!(
                            page = %proposal.page_path,
                            error = %e,
                            "Failed to delete wiki page"
                        ),
                    }
                }
            }
        }
        info!(applied, total = proposals.len(), "Wiki proposals applied");
        Ok(applied)
    }

    // ── Private helpers ─────────────────────────────────────────

    fn validate_page_path(&self, path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(DuDuClawError::Memory("empty page path".into()));
        }
        // String-level checks (first line of defense)
        if path.contains("..") || path.starts_with('/') || path.starts_with('\\') {
            return Err(DuDuClawError::Memory("path traversal not allowed".into()));
        }
        // Block null bytes and percent-encoded traversal
        if path.contains('\0') || path.contains("%2e") || path.contains("%2E") || path.contains("%2f") || path.contains("%2F") {
            return Err(DuDuClawError::Memory("encoded path traversal not allowed".into()));
        }
        if !path.ends_with(".md") {
            return Err(DuDuClawError::Memory("page path must end with .md".into()));
        }
        let filename = Path::new(path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");
        if RESERVED_FILES.contains(&filename) {
            return Err(DuDuClawError::Memory(format!(
                "'{}' is a reserved file",
                filename
            )));
        }
        // Component-level check — reject any ParentDir or RootDir components
        for component in Path::new(path).components() {
            if matches!(component, std::path::Component::ParentDir | std::path::Component::RootDir) {
                return Err(DuDuClawError::Memory("path component traversal not allowed".into()));
            }
        }
        // Canonicalize check — verify resolved path stays within wiki_dir
        let full = self.wiki_dir.join(path);
        if full.exists()
            && let (Ok(canon_full), Ok(canon_wiki)) = (full.canonicalize(), self.wiki_dir.canonicalize())
                && !canon_full.starts_with(&canon_wiki) {
                    return Err(DuDuClawError::Memory("resolved path escapes wiki directory".into()));
                }
        Ok(())
    }

    fn update_index(&self, page_path: &str, title: &str) -> std::result::Result<(), String> {
        let index_path = self.wiki_dir.join("_index.md");

        // Use flock for atomicity across concurrent callers
        let lock_path = self.wiki_dir.join("_index.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("open lock: {e}"))?;
        // Advisory lock — blocks until acquired
        fs_lock(&lock_file).map_err(|e| format!("acquire lock: {e}"))?;

        let existing = std::fs::read_to_string(&index_path).unwrap_or_default();
        let now = Utc::now().format("%Y-%m-%d").to_string();
        // Escape title chars that break markdown link syntax
        let safe_title = title.replace('[', "\\[").replace(']', "\\]");
        let entry = format!("- [{}]({}) — updated {}", safe_title, page_path, now);
        let link_pattern = format!("]({})", page_path);

        let mut lines: Vec<String> = existing.lines().map(String::from).collect();
        let mut found = false;
        for line in &mut lines {
            if line.contains(&link_pattern) {
                *line = entry.clone();
                found = true;
                break;
            }
        }
        if !found {
            lines.push(entry);
        }

        // Atomic write: temp + rename under lock
        let tmp_path = index_path.with_extension("md.idx_tmp");
        let content = lines.join("\n") + "\n";
        std::fs::write(&tmp_path, &content)
            .map_err(|e| format!("write index tmp: {e}"))?;
        std::fs::rename(&tmp_path, &index_path)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("rename index: {e}")
            })?;

        // Lock released when lock_file is dropped
        Ok(())
    }

    fn remove_from_index(&self, page_path: &str) -> std::result::Result<(), String> {
        let index_path = self.wiki_dir.join("_index.md");

        let lock_path = self.wiki_dir.join("_index.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("open lock: {e}"))?;
        fs_lock(&lock_file).map_err(|e| format!("acquire lock: {e}"))?;

        let existing = std::fs::read_to_string(&index_path).unwrap_or_default();
        let link_pattern = format!("]({})", page_path);

        let lines: Vec<&str> = existing
            .lines()
            .filter(|line| !line.contains(&link_pattern))
            .collect();

        let tmp_path = index_path.with_extension("md.idx_tmp");
        let content = lines.join("\n") + "\n";
        std::fs::write(&tmp_path, &content)
            .map_err(|e| format!("write index tmp: {e}"))?;
        std::fs::rename(&tmp_path, &index_path)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("rename index: {e}")
            })
    }

    fn append_log(&self, action: &str, page_path: &str) -> std::result::Result<(), String> {
        let log_path = self.wiki_dir.join("_log.md");
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let entry = format!("## [{}] {} | {}\n", now, action, page_path);

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("open log: {e}"))?;
        f.write_all(entry.as_bytes())
            .map_err(|e| format!("write log: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a wiki page from raw content.
fn parse_wiki_page(path: &str, content: &str) -> Result<WikiPage> {
    let title = extract_title(content).unwrap_or_else(|| path.to_string());
    let created = extract_datetime_field(content, "created").unwrap_or_else(Utc::now);
    let updated = extract_datetime_field(content, "updated").unwrap_or_else(Utc::now);
    let tags = extract_string_list(content, "tags");
    let related = extract_string_list(content, "related");
    let sources = extract_string_list(content, "sources");
    let body = extract_body(content);

    Ok(WikiPage {
        path: path.to_string(),
        title,
        created,
        updated,
        tags,
        related,
        sources,
        body,
    })
}

/// Extract a string field from YAML frontmatter (best-effort, no YAML parser).
fn extract_field(content: &str, field: &str) -> Option<String> {
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

/// Extract title from frontmatter.
fn extract_title(content: &str) -> Option<String> {
    extract_field(content, "title")
}

/// Extract a datetime field from frontmatter.
fn extract_datetime_field(content: &str, field: &str) -> Option<DateTime<Utc>> {
    let val = extract_field(content, field)?;
    // Try full ISO 8601 first, then date-only
    if let Ok(dt) = val.parse::<DateTime<Utc>>() {
        return Some(dt);
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(&val, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// Extract a list field from frontmatter.
///
/// Supports both YAML inline format (`tags: [a, b, c]`) and block list format:
/// ```yaml
/// tags:
///   - a
///   - b
/// ```
fn extract_string_list(content: &str, field: &str) -> Vec<String> {
    // Try inline format first via extract_field
    if let Some(val) = extract_field(content, field) {
        let inner = val.trim_start_matches('[').trim_end_matches(']');
        if !inner.is_empty() {
            return inner
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    // Try block list format: field key on its own line, items indented with "- "
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Vec::new();
    }
    let rest = &trimmed[3..];
    let end = match rest.find("\n---") {
        Some(e) => e,
        None => return Vec::new(),
    };
    let fm = &rest[..end];
    let prefix = format!("{}:", field);
    let mut found_key = false;
    let mut items = Vec::new();

    for line in fm.lines() {
        let trimmed_line = line.trim();
        if found_key {
            // Block list items are indented lines starting with "- "
            if let Some(item) = trimmed_line.strip_prefix("- ") {
                let val = item.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    items.push(val.to_string());
                }
            } else if !trimmed_line.is_empty() {
                // Hit a non-list-item, non-empty line — end of block list
                break;
            }
        } else if trimmed_line.strip_prefix(&prefix).map(|a| a.trim().is_empty()).unwrap_or(false) {
            // Found "field:" with empty value — start of block list
            found_key = true;
        }
    }

    items
}

/// Extract the body (everything after the closing `---` of frontmatter).
fn extract_body(content: &str) -> String {
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

/// Extract markdown links from page body (e.g. `[text](path.md)`).
fn extract_body_links(content: &str) -> Vec<String> {
    let body = extract_body(content);
    let mut links = Vec::new();
    let mut remaining = body.as_str();

    while let Some(start) = remaining.find("](") {
        let after = &remaining[start + 2..];
        if let Some(end) = after.find(')') {
            let link = &after[..end];
            // Only include relative .md links (not http, not anchors)
            if link.ends_with(".md") && !link.starts_with("http") {
                // Strip only a single leading "../" to resolve one level up.
                // Deeper traversals (../../..) are kept as-is — they won't match
                // any page_path and will show up as broken links in lint().
                let normalized = link.strip_prefix("../").unwrap_or(link);
                links.push(normalized.to_string());
            }
            remaining = &after[end..];
        } else {
            break;
        }
    }

    links
}

/// Advisory file lock. Blocks until acquired.
fn fs_lock(file: &std::fs::File) -> std::io::Result<()> {
    duduclaw_core::platform::flock_exclusive(file)
}

/// Collect all `.md` files recursively under `dir`, relative to `base`.
/// Skips reserved files. Respects depth limit and does not follow symlinks.
fn collect_md_files_recursive(base: &Path, dir: &Path) -> Vec<PathBuf> {
    collect_md_files_inner(base, dir, 0)
}

fn collect_md_files_inner(base: &Path, dir: &Path, depth: usize) -> Vec<PathBuf> {
    if depth > MAX_RECURSION_DEPTH {
        warn!(dir = %dir.display(), depth, "Wiki directory scan exceeded max depth, skipping");
        return Vec::new();
    }
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        // Use DirEntry::file_type() which does NOT follow symlinks
        let ftype = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let path = entry.path();
        if ftype.is_dir() {
            result.extend(collect_md_files_inner(base, &path, depth + 1));
        } else if ftype.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            let fname = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
            if !RESERVED_FILES.contains(&fname)
                && let Ok(rel) = path.strip_prefix(base) {
                    result.push(rel.to_path_buf());
                }
        }
        // Symlinks are silently skipped (neither is_dir nor is_file on DirEntry::file_type)
    }
    result
}

/// Escape text for safe HTML embedding.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Escape body text and convert basic markdown to HTML.
fn html_escape_body(body: &str) -> String {
    let mut html = String::new();
    for line in body.lines() {
        if let Some(stripped) = line.strip_prefix("# ") {
            html.push_str(&format!("<h3>{}</h3>\n", html_escape(stripped)));
        } else if let Some(stripped) = line.strip_prefix("## ") {
            html.push_str(&format!("<h4>{}</h4>\n", html_escape(stripped)));
        } else if let Some(stripped) = line.strip_prefix("- ") {
            html.push_str(&format!("<li>{}</li>\n", html_escape(stripped)));
        } else if line.trim().is_empty() {
            html.push_str("<br>\n");
        } else {
            html.push_str(&format!("<p>{}</p>\n", html_escape(line)));
        }
    }
    html
}

/// Default wiki schema content.
fn default_schema() -> &'static str {
    include_str!("../../../templates/wiki/_schema.md")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_page(title: &str) -> String {
        format!(
            r#"---
title: {}
created: 2026-04-07
updated: 2026-04-07
tags: [test, sample]
related: []
sources: []
---

This is the body of the page.
"#,
            title
        )
    }

    #[test]
    fn test_extract_title() {
        let content = sample_page("Hello World");
        assert_eq!(extract_title(&content), Some("Hello World".to_string()));
    }

    #[test]
    fn test_extract_title_no_frontmatter() {
        assert_eq!(extract_title("Just plain text"), None);
    }

    #[test]
    fn test_extract_string_list() {
        let content = sample_page("Test");
        let tags = extract_string_list(&content, "tags");
        assert_eq!(tags, vec!["test", "sample"]);
    }

    #[test]
    fn test_extract_body() {
        let content = sample_page("Test");
        let body = extract_body(&content);
        assert!(body.contains("This is the body"));
        assert!(!body.contains("title:"));
    }

    #[test]
    fn test_extract_body_links() {
        let content = "---\ntitle: T\n---\nSee [foo](entities/foo.md) and [bar](../concepts/bar.md) and [ext](https://example.com).";
        let links = extract_body_links(content);
        assert_eq!(links, vec!["entities/foo.md", "concepts/bar.md"]);
    }

    #[test]
    fn test_wiki_store_scaffold() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir.clone());
        store.ensure_scaffold().unwrap();

        assert!(wiki_dir.join("entities").exists());
        assert!(wiki_dir.join("concepts").exists());
        assert!(wiki_dir.join("sources").exists());
        assert!(wiki_dir.join("synthesis").exists());
        assert!(wiki_dir.join("_schema.md").exists());
        assert!(wiki_dir.join("_index.md").exists());
        assert!(wiki_dir.join("_log.md").exists());
    }

    #[test]
    fn test_wiki_store_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let content = sample_page("Test Page");
        store.write_page("concepts/test.md", &content).unwrap();

        let page = store.read_page("concepts/test.md").unwrap();
        assert_eq!(page.title, "Test Page");
        assert!(page.body.contains("This is the body"));

        // Check index updated
        let index = store.read_raw("_index.md").unwrap();
        assert!(index.contains("concepts/test.md"));
        assert!(index.contains("Test Page"));

        // Check log
        let log = store.read_raw("_log.md").unwrap();
        assert!(log.contains("create | concepts/test.md"));
    }

    #[test]
    fn test_wiki_store_search() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/rust.md", &sample_page("Rust Language")).unwrap();
        store.write_page("concepts/python.md", "---\ntitle: Python\nupdated: 2026-04-07\n---\nPython is great for data science.\n").unwrap();

        let hits = store.search("body page", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "concepts/rust.md");

        let hits = store.search("python data", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "concepts/python.md");
    }

    #[test]
    fn test_wiki_store_delete() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("entities/alice.md", &sample_page("Alice")).unwrap();
        assert!(store.read_page("entities/alice.md").is_ok());

        store.delete_page("entities/alice.md").unwrap();
        assert!(store.read_page("entities/alice.md").is_err());

        let index = store.read_raw("_index.md").unwrap();
        assert!(!index.contains("alice.md"));
    }

    #[test]
    fn test_wiki_store_lint() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir.clone());
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/a.md", &sample_page("Page A")).unwrap();
        // Manually add broken link to index
        let index_path = wiki_dir.join("_index.md");
        let mut index = std::fs::read_to_string(&index_path).unwrap();
        index.push_str("- [Ghost](concepts/ghost.md) — updated 2026-04-07\n");
        std::fs::write(&index_path, index).unwrap();

        let report = store.lint().unwrap();
        assert_eq!(report.total_pages, 1);
        assert!(!report.broken_links.is_empty());
    }

    #[test]
    fn test_path_traversal_blocked() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        assert!(store.write_page("../escape.md", "bad").is_err());
        assert!(store.write_page("/etc/passwd", "bad").is_err());
        assert!(store.write_page("_index.md", "bad").is_err());
        assert!(store.write_page("_log.md", "bad").is_err());
    }

    #[test]
    fn test_rebuild_index() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("entities/a.md", &sample_page("Entity A")).unwrap();
        store.write_page("concepts/b.md", &sample_page("Concept B")).unwrap();

        let count = store.rebuild_index().unwrap();
        assert_eq!(count, 2);

        let index = store.read_raw("_index.md").unwrap();
        assert!(index.contains("Entity A"));
        assert!(index.contains("Concept B"));
    }

    #[test]
    fn test_apply_proposals() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let proposals = vec![
            WikiProposal {
                page_path: "entities/bob.md".to_string(),
                action: WikiAction::Create,
                content: Some(sample_page("Bob")),
                rationale: "New customer".to_string(),
                related_pages: vec![],
            },
            WikiProposal {
                page_path: "concepts/greeting.md".to_string(),
                action: WikiAction::Create,
                content: Some(sample_page("Greeting")),
                rationale: "Common pattern".to_string(),
                related_pages: vec!["entities/bob.md".to_string()],
            },
        ];

        let applied = store.apply_proposals(&proposals).unwrap();
        assert_eq!(applied, 2);
        assert!(store.read_page("entities/bob.md").is_ok());
        assert!(store.read_page("concepts/greeting.md").is_ok());
    }
}
