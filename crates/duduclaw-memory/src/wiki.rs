//! Wiki Knowledge Base — structured markdown page management.
//!
//! Based on [Karpathy's LLM Wiki pattern](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f).
//! Each agent maintains a `wiki/` directory of interlinked markdown files.
//! The `WikiStore` handles reading, writing, indexing, and health-checking pages.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_core::error::{DuDuClawError, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Knowledge layer — controls injection frequency into LLM context.
///
/// Inspired by Vault-for-LLM's 4-layer architecture:
/// - L0 Identity: always injected (agent/user identity)
/// - L1 Core: always injected (environment, active projects)
/// - L2 Context: daily refresh (recent decisions, debug logs)
/// - L3 Deep: on-demand search only (deep knowledge archive)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WikiLayer {
    /// L0: Identity — always injected into every conversation.
    Identity,
    /// L1: Core facts — always injected (environment, active projects).
    Core,
    /// L2: Context — daily refresh (recent decisions, debugging).
    Context,
    /// L3: Deep knowledge — on-demand search only.
    #[default]
    Deep,
}

impl WikiLayer {
    /// Numeric priority for sorting (lower = higher priority for injection).
    pub fn priority(self) -> u8 {
        match self {
            Self::Identity => 0,
            Self::Core => 1,
            Self::Context => 2,
            Self::Deep => 3,
        }
    }

    /// Whether this layer should be auto-injected into system prompt.
    pub fn auto_inject(self) -> bool {
        matches!(self, Self::Identity | Self::Core)
    }
}

impl fmt::Display for WikiLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity => write!(f, "identity"),
            Self::Core => write!(f, "core"),
            Self::Context => write!(f, "context"),
            Self::Deep => write!(f, "deep"),
        }
    }
}

impl FromStr for WikiLayer {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "identity" | "l0" => Ok(Self::Identity),
            "core" | "l1" => Ok(Self::Core),
            "context" | "l2" => Ok(Self::Context),
            "deep" | "l3" => Ok(Self::Deep),
            _ => Err(format!("unknown wiki layer: '{s}'")),
        }
    }
}

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
    /// Author agent ID (used in shared wiki to track who wrote the page).
    pub author: Option<String>,
    /// Knowledge layer (L0-L3) controlling injection frequency.
    #[serde(default)]
    pub layer: WikiLayer,
    /// Trust score (0.0-1.0). Higher = more reliable.
    /// Default 0.5 for unrated pages.
    #[serde(default = "default_trust")]
    pub trust: f32,
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
    /// Author agent ID (populated for shared wiki pages).
    pub author: Option<String>,
    /// Knowledge layer (L0-L3).
    #[serde(default)]
    pub layer: WikiLayer,
    /// Trust score (0.0-1.0).
    #[serde(default = "default_trust")]
    pub trust: f32,
}

/// A search hit with relevance score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub score: usize,
    /// Trust-weighted score for ranking: `score * (0.5 + trust)`.
    pub weighted_score: f64,
    /// Trust score of the matched page.
    pub trust: f32,
    /// Knowledge layer of the matched page.
    pub layer: WikiLayer,
    /// Up to 3 matching lines for context.
    pub context_lines: Vec<String>,
}

fn default_trust() -> f32 {
    0.5
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

/// Target for a wiki write — agent-private or shared knowledge base.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WikiTarget {
    /// Write to the agent's own wiki (default).
    #[default]
    Agent,
    /// Write to the shared wiki (`~/.duduclaw/shared/wiki/`).
    Shared,
    /// Write to both agent and shared wiki.
    Both,
}

/// Proposed wiki change (used by GVU integration in Phase 2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiProposal {
    pub page_path: String,
    pub action: WikiAction,
    pub content: Option<String>,
    pub rationale: String,
    pub related_pages: Vec<String>,
    /// Where to apply this proposal. Defaults to the agent's own wiki.
    #[serde(default)]
    pub target: WikiTarget,
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

/// File-system backed wiki store for a single agent or the shared knowledge base.
pub struct WikiStore {
    /// Root wiki directory (e.g. `~/.duduclaw/agents/agnes/wiki/` or `~/.duduclaw/shared/wiki/`).
    wiki_dir: PathBuf,
    /// Whether this is the shared wiki (`~/.duduclaw/shared/wiki/`).
    shared: bool,
}

impl WikiStore {
    /// Open a wiki store at the given directory.
    /// Does NOT create the directory — call `ensure_scaffold()` first if needed.
    pub fn new(wiki_dir: PathBuf) -> Self {
        Self { wiki_dir, shared: false }
    }

    /// Open the shared wiki store at `home_dir/shared/wiki/`.
    pub fn new_shared(home_dir: &Path) -> Self {
        Self {
            wiki_dir: home_dir.join("shared").join("wiki"),
            shared: true,
        }
    }

    /// Whether this is the shared wiki.
    pub fn is_shared(&self) -> bool {
        self.shared
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
            let author = extract_field(&content, "author");
            let layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let trust = extract_field(&content, "trust")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(default_trust());

            pages.push(PageMeta {
                path: rel_str,
                title,
                updated,
                tags,
                author,
                layer,
                trust,
            });
        }

        pages.sort_by_key(|p| std::cmp::Reverse(p.updated));
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

        // Check for missing reciprocal backlinks in related pages
        let related = extract_string_list(content, "related");
        for rel_path in &related {
            let rel_full = self.wiki_dir.join(rel_path);
            if rel_full.exists() {
                if let Ok(rel_content) = std::fs::read_to_string(&rel_full) {
                    let rel_related = extract_string_list(&rel_content, "related");
                    if !rel_related.iter().any(|r| r == path) {
                        info!(
                            page = path,
                            related = rel_path.as_str(),
                            "Backlink suggestion: '{}' references '{}' but not vice versa",
                            path, rel_path
                        );
                    }
                }
            }
        }

        // Best-effort FTS sync
        self.fts_sync_upsert(path, content);

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
            // Best-effort FTS sync
            self.fts_sync_remove(path);
            info!(page = path, "Wiki page deleted");
        }
        Ok(())
    }

    // ── Search ──────────────────────────────────────────────────

    /// Full-text keyword search across all wiki pages.
    ///
    /// Results are ranked by trust-weighted score: `score * (0.5 + trust)`.
    /// Pages with higher trust scores are ranked higher when keyword matches are equal.
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
            let trust = extract_field(&content, "trust")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(default_trust());
            let layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let weighted_score = score as f64 * (0.5 + trust as f64);

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
                weighted_score,
                trust,
                layer,
                context_lines,
            });
        }

        // Sort by weighted_score descending (trust-aware ranking)
        hits.sort_by(|a, b| b.weighted_score.partial_cmp(&a.weighted_score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }

    /// Search with optional filters: minimum trust, layer filter, and 1-hop expand.
    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        min_trust: Option<f32>,
        layer_filter: Option<WikiLayer>,
        expand: bool,
    ) -> Result<Vec<SearchHit>> {
        let mut hits = self.search(query, limit * 2)?; // over-fetch for filtering

        // Apply filters
        if let Some(mt) = min_trust {
            hits.retain(|h| h.trust >= mt);
        }
        if let Some(lf) = layer_filter {
            hits.retain(|h| h.layer == lf);
        }

        // 1-hop expand: add related pages of direct hits
        if expand && !hits.is_empty() {
            let backlinks = self.build_backlink_index()?;
            let direct_paths: HashSet<String> = hits.iter().map(|h| h.path.clone()).collect();
            let mut expanded_paths: HashSet<String> = HashSet::new();

            for hit in &hits {
                // Outbound: read the page's `related` field
                let full = self.wiki_dir.join(&hit.path);
                if let Ok(content) = std::fs::read_to_string(&full) {
                    for rel in extract_string_list(&content, "related") {
                        if !direct_paths.contains(&rel) {
                            expanded_paths.insert(rel);
                        }
                    }
                }
                // Inbound: backlinks
                if let Some(blinks) = backlinks.get(&hit.path) {
                    for bl in blinks {
                        if !direct_paths.contains(bl) {
                            expanded_paths.insert(bl.clone());
                        }
                    }
                }
            }

            // Read expanded pages and create SearchHits with score=0
            for exp_path in expanded_paths {
                let full = self.wiki_dir.join(&exp_path);
                let content = match std::fs::read_to_string(&full) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let title = extract_title(&content).unwrap_or_else(|| exp_path.clone());
                let trust = extract_field(&content, "trust")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(default_trust());
                let layer = extract_field(&content, "layer")
                    .and_then(|v| WikiLayer::from_str(&v).ok())
                    .unwrap_or_default();

                // Apply same filters to expanded results
                if let Some(mt) = min_trust {
                    if trust < mt { continue; }
                }
                if let Some(lf) = layer_filter {
                    if layer != lf { continue; }
                }

                hits.push(SearchHit {
                    path: exp_path,
                    title,
                    score: 0,
                    weighted_score: 0.0,
                    trust,
                    layer,
                    context_lines: vec!["(expanded via related/backlink)".to_string()],
                });
            }
        }

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

    // ── Backlink index ──────────────────────────────────────────

    /// Build a reverse-link index: target page → list of source pages that reference it.
    ///
    /// Scans both `related:` frontmatter and markdown links in page body.
    pub fn build_backlink_index(&self) -> Result<HashMap<String, Vec<String>>> {
        let pages = self.list_pages()?;
        let page_paths: HashSet<String> = pages.iter().map(|p| p.path.clone()).collect();
        let mut backlinks: HashMap<String, Vec<String>> = HashMap::new();

        for page in &pages {
            let full = self.wiki_dir.join(&page.path);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Collect all outbound links (related + body links)
            let related = extract_string_list(&content, "related");
            let body_links = extract_body_links(&content);

            for target in related.into_iter().chain(body_links) {
                if page_paths.contains(&target) && target != page.path {
                    backlinks
                        .entry(target)
                        .or_default()
                        .push(page.path.clone());
                }
            }
        }

        // Deduplicate backlink lists
        for links in backlinks.values_mut() {
            links.sort();
            links.dedup();
        }

        Ok(backlinks)
    }

    // ── Layer-aware context injection ──────────────────────────

    /// Collect all pages belonging to a specific knowledge layer.
    ///
    /// Returns (path, body) pairs sorted by title.
    pub fn collect_by_layer(&self, layer: WikiLayer) -> Result<Vec<(String, String)>> {
        let md_files = collect_md_files_recursive(&self.wiki_dir, &self.wiki_dir);
        let mut results = Vec::new();

        for rel in &md_files {
            let full = self.wiki_dir.join(rel);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let page_layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            if page_layer == layer {
                let body = extract_body(&content);
                let title = extract_title(&content)
                    .unwrap_or_else(|| rel.to_string_lossy().to_string());
                results.push((rel.to_string_lossy().to_string(), format!("## {title}\n\n{body}")));
            }
        }

        results.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(results)
    }

    /// Build injection context from L0 (Identity) + L1 (Core) pages,
    /// respecting a character budget.
    ///
    /// Returns combined text suitable for system prompt injection.
    /// L0 pages are injected first (highest priority), then L1.
    /// L2/L3 are excluded — they are retrieved on-demand via search.
    pub fn build_injection_context(&self, max_chars: usize) -> Result<String> {
        let mut output = String::new();
        let mut remaining = max_chars;

        for layer in [WikiLayer::Identity, WikiLayer::Core] {
            let pages = self.collect_by_layer(layer)?;
            if pages.is_empty() {
                continue;
            }

            let header = format!("### Wiki — {layer}\n\n");
            if header.len() >= remaining {
                break;
            }
            output.push_str(&header);
            remaining -= header.len();

            for (_path, body) in &pages {
                let needed = body.len() + 2; // +2 for trailing newlines
                if needed > remaining {
                    break;
                }
                output.push_str(body);
                output.push_str("\n\n");
                remaining -= needed;
            }
        }

        Ok(output)
    }

    // ── Apply proposals (for GVU integration) ──────────────────

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

    /// Best-effort FTS index sync after a page write.
    /// Silently logs warnings on failure — FTS is an acceleration layer,
    /// not a source of truth.
    fn fts_sync_upsert(&self, path: &str, content: &str) {
        if let Ok(fts) = WikiFts::open(&self.wiki_dir) {
            let title = extract_title(content).unwrap_or_else(|| path.to_string());
            let body = extract_body(content);
            let tags = extract_string_list(content, "tags");
            let layer = extract_field(content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let trust = extract_field(content, "trust")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(default_trust());
            if let Err(e) = fts.upsert(path, &title, &body, &tags, layer, trust) {
                warn!(page = path, "FTS sync upsert failed: {e}");
            }
        }
    }

    /// Best-effort FTS index sync after a page delete.
    fn fts_sync_remove(&self, path: &str) {
        if let Ok(fts) = WikiFts::open(&self.wiki_dir) {
            if let Err(e) = fts.remove(path) {
                warn!(page = path, "FTS sync remove failed: {e}");
            }
        }
    }

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
        self.append_log_with_author(action, page_path, None)
    }

    fn append_log_with_author(&self, action: &str, page_path: &str, author: Option<&str>) -> std::result::Result<(), String> {
        let log_path = self.wiki_dir.join("_log.md");
        let now = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let entry = match author {
            Some(a) => format!("## [{}] {} | {} | by:{}\n", now, action, page_path, a),
            None => format!("## [{}] {} | {}\n", now, action, page_path),
        };

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("open log: {e}"))?;
        f.write_all(entry.as_bytes())
            .map_err(|e| format!("write log: {e}"))
    }

    /// Write a page with author attribution (used by shared wiki).
    pub fn write_page_with_author(&self, path: &str, content: &str, author: &str) -> Result<()> {
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

        // Append log with author
        let action = if is_new { "create" } else { "update" };
        if let Err(e) = self.append_log_with_author(action, path, Some(author)) {
            warn!("Failed to log {action} {path}: {e}");
        }

        // Best-effort FTS sync
        self.fts_sync_upsert(path, content);

        info!(page = path, author, action, "Wiki page written (shared)");
        Ok(())
    }

    /// Extract the author of a page by reading its frontmatter.
    pub fn page_author(&self, path: &str) -> Result<Option<String>> {
        let content = self.read_raw(path)?;
        Ok(extract_field(&content, "author"))
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
    let author = extract_field(content, "author");
    let layer = extract_field(content, "layer")
        .and_then(|v| WikiLayer::from_str(&v).ok())
        .unwrap_or_default();
    let trust = extract_field(content, "trust")
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default_trust());
    let body = extract_body(content);

    Ok(WikiPage {
        path: path.to_string(),
        title,
        created,
        updated,
        tags,
        related,
        sources,
        author,
        layer,
        trust,
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
// FTS5 Full-Text Index
// ---------------------------------------------------------------------------

/// SQLite FTS5 index for accelerated wiki search.
///
/// Stored as `_wiki.db` in the wiki directory. Provides sub-millisecond
/// full-text search as an alternative to linear file scanning.
pub struct WikiFts {
    conn: rusqlite::Connection,
}

impl WikiFts {
    /// Open or create the FTS5 index database at `wiki_dir/_wiki.db`.
    pub fn open(wiki_dir: &Path) -> Result<Self> {
        let db_path = wiki_dir.join("_wiki.db");
        let conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| DuDuClawError::Memory(format!("open FTS DB: {e}")))?;

        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS wiki_fts USING fts5(
                path,
                title,
                body,
                tags,
                layer UNINDEXED,
                trust UNINDEXED,
                tokenize='unicode61'
            );
            CREATE TABLE IF NOT EXISTS wiki_meta (
                path TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                layer TEXT NOT NULL DEFAULT 'deep',
                trust REAL NOT NULL DEFAULT 0.5,
                updated TEXT NOT NULL
            );"
        ).map_err(|e| DuDuClawError::Memory(format!("init FTS schema: {e}")))?;

        Ok(Self { conn })
    }

    /// Insert or update a page in the FTS index.
    pub fn upsert(&self, path: &str, title: &str, body: &str, tags: &[String], layer: WikiLayer, trust: f32) -> Result<()> {
        let tags_str = tags.join(", ");
        let layer_str = layer.to_string();
        let now = Utc::now().to_rfc3339();

        // Delete old entry if exists (FTS5 content table requires delete+insert for updates)
        self.conn.execute(
            "DELETE FROM wiki_fts WHERE path = ?1",
            rusqlite::params![path],
        ).map_err(|e| DuDuClawError::Memory(format!("fts delete: {e}")))?;

        self.conn.execute(
            "INSERT INTO wiki_fts (path, title, body, tags, layer, trust) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![path, title, body, &tags_str, &layer_str, trust],
        ).map_err(|e| DuDuClawError::Memory(format!("fts insert: {e}")))?;

        // Upsert metadata
        self.conn.execute(
            "INSERT OR REPLACE INTO wiki_meta (path, title, layer, trust, updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![path, title, &layer_str, trust, &now],
        ).map_err(|e| DuDuClawError::Memory(format!("meta upsert: {e}")))?;

        Ok(())
    }

    /// Remove a page from the FTS index.
    pub fn remove(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM wiki_fts WHERE path = ?1",
            rusqlite::params![path],
        ).map_err(|e| DuDuClawError::Memory(format!("fts delete: {e}")))?;
        self.conn.execute(
            "DELETE FROM wiki_meta WHERE path = ?1",
            rusqlite::params![path],
        ).map_err(|e| DuDuClawError::Memory(format!("meta delete: {e}")))?;
        Ok(())
    }

    /// Full-text search using FTS5. Returns (path, title, rank, trust, layer) tuples.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // FTS5 query: escape special chars to avoid syntax errors
        let safe_query = fts5_escape_query(query);
        if safe_query.is_empty() {
            return Ok(Vec::new());
        }

        let mut stmt = self.conn.prepare(
            "SELECT f.path, f.title, rank, f.trust, f.layer,
                    snippet(wiki_fts, 2, '>>>', '<<<', '...', 30) as snip
             FROM wiki_fts f
             WHERE wiki_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        ).map_err(|e| DuDuClawError::Memory(format!("fts prepare: {e}")))?;

        let hits: Vec<SearchHit> = stmt.query_map(
            rusqlite::params![&safe_query, limit as i64],
            |row| {
                let path: String = row.get(0)?;
                let title: String = row.get(1)?;
                let fts_rank: f64 = row.get(2)?;
                let trust: f32 = row.get(3)?;
                let layer_str: String = row.get(4)?;
                let snippet: String = row.get(5)?;

                let layer = WikiLayer::from_str(&layer_str).unwrap_or_default();
                // FTS5 rank is negative (lower = better), convert to positive score
                let score = (-fts_rank * 10.0).max(1.0) as usize;
                let weighted_score = score as f64 * (0.5 + trust as f64);

                Ok(SearchHit {
                    path,
                    title,
                    score,
                    weighted_score,
                    trust,
                    layer,
                    context_lines: if snippet.is_empty() { vec![] } else { vec![snippet] },
                })
            },
        ).map_err(|e| DuDuClawError::Memory(format!("fts query: {e}")))?
        .filter_map(|r| r.ok())
        .collect();

        Ok(hits)
    }

    /// Rebuild the FTS index from all pages in the wiki directory.
    pub fn rebuild(&self, wiki_dir: &Path) -> Result<usize> {
        // Clear existing data
        self.conn.execute_batch(
            "DELETE FROM wiki_fts; DELETE FROM wiki_meta;"
        ).map_err(|e| DuDuClawError::Memory(format!("fts clear: {e}")))?;

        let md_files = collect_md_files_recursive(wiki_dir, wiki_dir);
        let mut count = 0;

        for rel in &md_files {
            let full = wiki_dir.join(rel);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let rel_str = rel.to_string_lossy().to_string();
            let title = extract_title(&content).unwrap_or_else(|| rel_str.clone());
            let body = extract_body(&content);
            let tags = extract_string_list(&content, "tags");
            let layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let trust = extract_field(&content, "trust")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(default_trust());

            self.upsert(&rel_str, &title, &body, &tags, layer, trust)?;
            count += 1;
        }

        info!(count, "FTS index rebuilt");
        Ok(count)
    }

    /// Number of indexed pages.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM wiki_meta", [], |row| row.get(0),
        ).map_err(|e| DuDuClawError::Memory(format!("fts count: {e}")))?;
        Ok(n as usize)
    }
}

/// Escape a user query for safe use in FTS5 MATCH.
///
/// FTS5 interprets special chars like `*`, `"`, `-`, `+` as operators.
/// We wrap each term in double quotes to treat them as literals.
fn fts5_escape_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| {
            // Escape internal double quotes
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// WikiStore FTS integration
// ---------------------------------------------------------------------------

impl WikiStore {
    /// Open or create the FTS5 index for this wiki.
    pub fn open_fts(&self) -> Result<WikiFts> {
        WikiFts::open(&self.wiki_dir)
    }

    /// Rebuild the FTS5 index from all pages on disk.
    pub fn rebuild_fts(&self) -> Result<usize> {
        let fts = self.open_fts()?;
        fts.rebuild(&self.wiki_dir)
    }
}

// ---------------------------------------------------------------------------
// Dedup detection (P2.3)
// ---------------------------------------------------------------------------

/// A pair of pages that appear to be duplicates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupCandidate {
    pub page_a: String,
    pub page_b: String,
    /// Similarity reason (e.g. "identical title", "80% tag overlap").
    pub reason: String,
    /// Trust score of page A.
    pub trust_a: f32,
    /// Trust score of page B.
    pub trust_b: f32,
}

impl WikiStore {
    /// Detect potential duplicate pages using title and tag similarity.
    ///
    /// This is a zero-LLM heuristic approach:
    /// - Exact title match (case-insensitive) → definite duplicate
    /// - High tag overlap (Jaccard >= 0.8) + same subdirectory → likely duplicate
    pub fn detect_duplicates(&self) -> Result<Vec<DedupCandidate>> {
        let pages = self.list_pages()?;
        let mut candidates = Vec::new();

        // Index pages by normalized title for O(n) title matching
        let mut by_title: HashMap<String, Vec<&PageMeta>> = HashMap::new();
        for page in &pages {
            let key = page.title.to_lowercase().trim().to_string();
            by_title.entry(key).or_default().push(page);
        }

        // Check exact title duplicates
        for (title, group) in &by_title {
            if group.len() < 2 || title.is_empty() {
                continue;
            }
            for i in 0..group.len() {
                for j in (i + 1)..group.len() {
                    candidates.push(DedupCandidate {
                        page_a: group[i].path.clone(),
                        page_b: group[j].path.clone(),
                        reason: "identical title (case-insensitive)".to_string(),
                        trust_a: group[i].trust,
                        trust_b: group[j].trust,
                    });
                }
            }
        }

        // Check tag overlap for pages in the same subdirectory
        for i in 0..pages.len() {
            let dir_i = Path::new(&pages[i].path).parent().and_then(|p| p.to_str()).unwrap_or("");
            if pages[i].tags.is_empty() {
                continue;
            }
            let set_i: HashSet<&String> = pages[i].tags.iter().collect();

            for j in (i + 1)..pages.len() {
                let dir_j = Path::new(&pages[j].path).parent().and_then(|p| p.to_str()).unwrap_or("");
                if dir_i != dir_j || pages[j].tags.is_empty() {
                    continue;
                }

                // Skip pairs already found by title match
                let already = candidates.iter().any(|c| {
                    (c.page_a == pages[i].path && c.page_b == pages[j].path)
                    || (c.page_a == pages[j].path && c.page_b == pages[i].path)
                });
                if already {
                    continue;
                }

                let set_j: HashSet<&String> = pages[j].tags.iter().collect();
                let intersection = set_i.intersection(&set_j).count();
                let union = set_i.union(&set_j).count();

                if union > 0 {
                    let jaccard = intersection as f64 / union as f64;
                    if jaccard >= 0.8 {
                        candidates.push(DedupCandidate {
                            page_a: pages[i].path.clone(),
                            page_b: pages[j].path.clone(),
                            reason: format!("tag overlap: {:.0}% Jaccard in {}/", jaccard * 100.0, dir_i),
                            trust_a: pages[i].trust,
                            trust_b: pages[j].trust,
                        });
                    }
                }
            }
        }

        Ok(candidates)
    }
}

// ---------------------------------------------------------------------------
// Knowledge Graph Mermaid export (P2.5)
// ---------------------------------------------------------------------------

impl WikiStore {
    /// Generate a Mermaid graph diagram of the wiki knowledge graph.
    ///
    /// Nodes = pages, edges = `related` frontmatter links + body markdown links.
    /// If `center` is provided, only shows pages within `depth` hops.
    pub fn export_mermaid(&self, center: Option<&str>, depth: usize) -> Result<String> {
        let pages = self.list_pages()?;
        let page_set: HashSet<String> = pages.iter().map(|p| p.path.clone()).collect();

        // Build adjacency list
        let mut edges: Vec<(String, String)> = Vec::new();
        for page in &pages {
            let full = self.wiki_dir.join(&page.path);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let related = extract_string_list(&content, "related");
            let body_links = extract_body_links(&content);
            for target in related.into_iter().chain(body_links) {
                if page_set.contains(&target) && target != page.path {
                    edges.push((page.path.clone(), target));
                }
            }
        }

        // If center is specified, BFS to find reachable nodes within depth
        let visible: HashSet<String> = if let Some(center_path) = center {
            let mut visited = HashSet::new();
            let mut frontier = vec![center_path.to_string()];
            visited.insert(center_path.to_string());

            for _ in 0..depth {
                let mut next_frontier = Vec::new();
                for node in &frontier {
                    for (a, b) in &edges {
                        let neighbor = if a == node { b } else if b == node { a } else { continue };
                        if visited.insert(neighbor.clone()) {
                            next_frontier.push(neighbor.clone());
                        }
                    }
                }
                frontier = next_frontier;
            }
            visited
        } else {
            page_set.clone()
        };

        // Build Mermaid output
        let mut mermaid = String::from("graph LR\n");

        // Generate stable short IDs for each page
        let mut node_ids: HashMap<String, String> = HashMap::new();
        for (i, path) in visible.iter().enumerate() {
            node_ids.insert(path.clone(), format!("n{i}"));
        }

        // Add node definitions with labels
        for page in &pages {
            if !visible.contains(&page.path) {
                continue;
            }
            let id = &node_ids[&page.path];
            // Escape special mermaid chars in title
            let safe_title = page.title
                .replace('"', "'")
                .replace('[', "(")
                .replace(']', ")");
            let dir = Path::new(&page.path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or("root");

            // Style nodes by layer
            let full = self.wiki_dir.join(&page.path);
            let layer = if let Ok(content) = std::fs::read_to_string(&full) {
                extract_field(&content, "layer")
                    .and_then(|v| WikiLayer::from_str(&v).ok())
                    .unwrap_or_default()
            } else {
                WikiLayer::Deep
            };

            let shape = match layer {
                WikiLayer::Identity => format!("    {id}[[\"{safe_title}\"]]\n"),
                WikiLayer::Core => format!("    {id}[/\"{safe_title}\"\\]\n"),
                WikiLayer::Context => format!("    {id}(\"{safe_title}\")\n"),
                WikiLayer::Deep => format!("    {id}[\"{safe_title}\"]\n"),
            };
            mermaid.push_str(&shape);

            // Add subgraph for directory grouping (only for full graph)
            let _ = dir; // used below for subgraph
        }

        // Add edges
        for (a, b) in &edges {
            if visible.contains(a) && visible.contains(b) {
                if let (Some(id_a), Some(id_b)) = (node_ids.get(a), node_ids.get(b)) {
                    mermaid.push_str(&format!("    {id_a} --> {id_b}\n"));
                }
            }
        }

        Ok(mermaid)
    }
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
                target: WikiTarget::default(),
            },
            WikiProposal {
                page_path: "concepts/greeting.md".to_string(),
                action: WikiAction::Create,
                content: Some(sample_page("Greeting")),
                rationale: "Common pattern".to_string(),
                related_pages: vec!["entities/bob.md".to_string()],
                target: WikiTarget::default(),
            },
        ];

        let applied = store.apply_proposals(&proposals).unwrap();
        assert_eq!(applied, 2);
        assert!(store.read_page("entities/bob.md").is_ok());
        assert!(store.read_page("concepts/greeting.md").is_ok());
    }

    // ── WikiLayer tests ────────────────────────────────────────

    #[test]
    fn test_wiki_layer_from_str_roundtrip() {
        for (input, expected) in [
            ("identity", WikiLayer::Identity),
            ("core", WikiLayer::Core),
            ("context", WikiLayer::Context),
            ("deep", WikiLayer::Deep),
            ("l0", WikiLayer::Identity),
            ("L1", WikiLayer::Core),
            ("L2", WikiLayer::Context),
            ("l3", WikiLayer::Deep),
        ] {
            let parsed = WikiLayer::from_str(input).unwrap();
            assert_eq!(parsed, expected, "input: {input}");
        }
        assert!(WikiLayer::from_str("unknown").is_err());
    }

    #[test]
    fn test_wiki_layer_display() {
        assert_eq!(WikiLayer::Identity.to_string(), "identity");
        assert_eq!(WikiLayer::Core.to_string(), "core");
        assert_eq!(WikiLayer::Context.to_string(), "context");
        assert_eq!(WikiLayer::Deep.to_string(), "deep");
    }

    #[test]
    fn test_wiki_layer_priority() {
        assert!(WikiLayer::Identity.priority() < WikiLayer::Core.priority());
        assert!(WikiLayer::Core.priority() < WikiLayer::Context.priority());
        assert!(WikiLayer::Context.priority() < WikiLayer::Deep.priority());
    }

    #[test]
    fn test_wiki_layer_auto_inject() {
        assert!(WikiLayer::Identity.auto_inject());
        assert!(WikiLayer::Core.auto_inject());
        assert!(!WikiLayer::Context.auto_inject());
        assert!(!WikiLayer::Deep.auto_inject());
    }

    // ── Layer / Trust parsing tests ────────────────────────────

    fn page_with_layer_trust(title: &str, layer: &str, trust: f32) -> String {
        format!(
            "---\ntitle: {title}\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [test]\nrelated: []\nsources: []\nlayer: {layer}\ntrust: {trust}\n---\n\nBody of {title}.\n"
        )
    }

    #[test]
    fn test_parse_page_with_layer_trust() {
        let content = page_with_layer_trust("Identity Page", "identity", 0.9);
        let page = parse_wiki_page("entities/test.md", &content).unwrap();
        assert_eq!(page.layer, WikiLayer::Identity);
        assert!((page.trust - 0.9).abs() < 0.01);
    }

    #[test]
    fn test_parse_page_missing_layer_trust_defaults() {
        let content = sample_page("Old Page");
        let page = parse_wiki_page("concepts/old.md", &content).unwrap();
        assert_eq!(page.layer, WikiLayer::Deep); // default
        assert!((page.trust - 0.5).abs() < 0.01); // default
    }

    #[test]
    fn test_list_pages_includes_layer_trust() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/core-fact.md", &page_with_layer_trust("Core Fact", "core", 0.8)).unwrap();
        store.write_page("concepts/deep-note.md", &page_with_layer_trust("Deep Note", "deep", 0.3)).unwrap();

        let pages = store.list_pages().unwrap();
        assert_eq!(pages.len(), 2);

        let core_page = pages.iter().find(|p| p.title == "Core Fact").unwrap();
        assert_eq!(core_page.layer, WikiLayer::Core);
        assert!((core_page.trust - 0.8).abs() < 0.01);

        let deep_page = pages.iter().find(|p| p.title == "Deep Note").unwrap();
        assert_eq!(deep_page.layer, WikiLayer::Deep);
        assert!((deep_page.trust - 0.3).abs() < 0.01);
    }

    // ── Trust-weighted search tests ────────────────────────────

    #[test]
    fn test_search_trust_weighted_ranking() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        // Both pages match "rust" but have different trust scores
        store.write_page("concepts/high-trust.md",
            &page_with_layer_trust("Rust High Trust", "core", 0.9)).unwrap();
        store.write_page("concepts/low-trust.md",
            &page_with_layer_trust("Rust Low Trust", "deep", 0.2)).unwrap();

        let hits = store.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 2);
        // High trust page should rank first
        assert_eq!(hits[0].path, "concepts/high-trust.md");
        assert!(hits[0].weighted_score > hits[1].weighted_score);
        assert!((hits[0].trust - 0.9).abs() < 0.01);
        assert_eq!(hits[0].layer, WikiLayer::Core);
    }

    // ── Backlink index tests ───────────────────────────────────

    #[test]
    fn test_build_backlink_index() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let page_a = "---\ntitle: Page A\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: [concepts/page-b.md]\nsources: []\n---\nLinks to B.\n";
        let page_b = "---\ntitle: Page B\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: []\nsources: []\n---\nStandalone page.\n";

        store.write_page("concepts/page-a.md", page_a).unwrap();
        store.write_page("concepts/page-b.md", page_b).unwrap();

        let backlinks = store.build_backlink_index().unwrap();
        // page-b should have a backlink from page-a
        let b_links = backlinks.get("concepts/page-b.md").unwrap();
        assert!(b_links.contains(&"concepts/page-a.md".to_string()));
        // page-a should have no backlinks
        assert!(backlinks.get("concepts/page-a.md").is_none());
    }

    // ── Layer-aware injection tests ────────────────────────────

    #[test]
    fn test_collect_by_layer() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("entities/identity.md", &page_with_layer_trust("My Identity", "identity", 0.95)).unwrap();
        store.write_page("concepts/core-env.md", &page_with_layer_trust("Core Environment", "core", 0.8)).unwrap();
        store.write_page("concepts/deep-arch.md", &page_with_layer_trust("Deep Architecture", "deep", 0.5)).unwrap();

        let identity_pages = store.collect_by_layer(WikiLayer::Identity).unwrap();
        assert_eq!(identity_pages.len(), 1);
        assert!(identity_pages[0].1.contains("My Identity"));

        let core_pages = store.collect_by_layer(WikiLayer::Core).unwrap();
        assert_eq!(core_pages.len(), 1);

        let deep_pages = store.collect_by_layer(WikiLayer::Deep).unwrap();
        assert_eq!(deep_pages.len(), 1);
    }

    #[test]
    fn test_build_injection_context() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("entities/me.md", &page_with_layer_trust("Agent Identity", "identity", 0.95)).unwrap();
        store.write_page("concepts/env.md", &page_with_layer_trust("Environment", "core", 0.8)).unwrap();
        store.write_page("concepts/deep.md", &page_with_layer_trust("Deep Stuff", "deep", 0.5)).unwrap();

        let context = store.build_injection_context(10000).unwrap();
        // Should include identity and core but not deep
        assert!(context.contains("Agent Identity"));
        assert!(context.contains("Environment"));
        assert!(!context.contains("Deep Stuff"));
    }

    #[test]
    fn test_build_injection_context_respects_budget() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("entities/me.md", &page_with_layer_trust("Agent Identity", "identity", 0.95)).unwrap();
        store.write_page("concepts/env.md", &page_with_layer_trust("Environment", "core", 0.8)).unwrap();

        // Very small budget — should truncate
        let context = store.build_injection_context(50).unwrap();
        assert!(context.len() <= 100); // some header overhead, but body should be truncated
    }

    // ── search_filtered tests ──────────────────────────────────

    #[test]
    fn test_search_filtered_min_trust() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/high.md", &page_with_layer_trust("Rust High", "deep", 0.9)).unwrap();
        store.write_page("concepts/low.md", &page_with_layer_trust("Rust Low", "deep", 0.2)).unwrap();

        let hits = store.search_filtered("rust", 10, Some(0.5), None, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "concepts/high.md");
    }

    #[test]
    fn test_search_filtered_layer() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/core.md", &page_with_layer_trust("Rust Core", "core", 0.8)).unwrap();
        store.write_page("concepts/deep.md", &page_with_layer_trust("Rust Deep", "deep", 0.5)).unwrap();

        let hits = store.search_filtered("rust", 10, None, Some(WikiLayer::Core), false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].layer, WikiLayer::Core);
    }

    #[test]
    fn test_search_filtered_expand() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        // page-a matches "quantum" and references page-b
        let page_a = "---\ntitle: Quantum Page\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [physics]\nrelated: [concepts/related-page.md]\nsources: []\nlayer: deep\ntrust: 0.7\n---\n\nQuantum computing is fascinating.\n";
        let page_b = "---\ntitle: Related Page\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: []\nsources: []\nlayer: deep\ntrust: 0.6\n---\n\nThis page has no matching keywords.\n";

        store.write_page("concepts/quantum-page.md", page_a).unwrap();
        store.write_page("concepts/related-page.md", page_b).unwrap();

        // Without expand: only quantum-page matches
        let hits = store.search_filtered("quantum", 10, None, None, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "concepts/quantum-page.md");

        // With expand: related-page also appears via the related link
        let hits = store.search_filtered("quantum", 10, None, None, true).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|h| h.path == "concepts/related-page.md"));
    }

    // ── FTS5 tests ─────────────────────────────────────────────

    #[test]
    fn test_fts_open_and_rebuild() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/alpha.md", &page_with_layer_trust("Alpha Topic", "core", 0.8)).unwrap();
        store.write_page("concepts/beta.md", &page_with_layer_trust("Beta Topic", "deep", 0.5)).unwrap();

        let count = store.rebuild_fts().unwrap();
        assert_eq!(count, 2);

        let fts = store.open_fts().unwrap();
        assert_eq!(fts.count().unwrap(), 2);
    }

    #[test]
    fn test_fts_search() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store.write_page("concepts/rust.md",
            "---\ntitle: Rust Language\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [programming]\nrelated: []\nsources: []\nlayer: core\ntrust: 0.9\n---\n\nRust is a systems programming language.\n"
        ).unwrap();
        store.write_page("concepts/python.md",
            "---\ntitle: Python Language\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [programming]\nrelated: []\nsources: []\nlayer: deep\ntrust: 0.5\n---\n\nPython is great for data science.\n"
        ).unwrap();

        store.rebuild_fts().unwrap();
        let fts = store.open_fts().unwrap();

        let hits = fts.search("rust systems", 10).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, "concepts/rust.md");
        assert!((hits[0].trust - 0.9).abs() < 0.01);
        assert_eq!(hits[0].layer, WikiLayer::Core);
    }

    #[test]
    fn test_fts_upsert_and_remove() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let fts = store.open_fts().unwrap();
        fts.upsert("concepts/test.md", "Test Page", "Some body text", &["tag1".to_string()], WikiLayer::Deep, 0.5).unwrap();
        assert_eq!(fts.count().unwrap(), 1);

        let hits = fts.search("body text", 10).unwrap();
        assert_eq!(hits.len(), 1);

        fts.remove("concepts/test.md").unwrap();
        assert_eq!(fts.count().unwrap(), 0);
    }

    // ── Dedup detection tests ──────────────────────────────────

    #[test]
    fn test_detect_duplicates_title() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        // Two pages with the same title (case-insensitive)
        store.write_page("concepts/rust-a.md", &page_with_layer_trust("Rust Language", "deep", 0.8)).unwrap();
        store.write_page("concepts/rust-b.md", &page_with_layer_trust("rust language", "deep", 0.3)).unwrap();
        // Unrelated page (different directory to avoid tag overlap match)
        store.write_page("entities/python.md", &page_with_layer_trust("Python Language", "deep", 0.5)).unwrap();

        let dups = store.detect_duplicates().unwrap();
        // Title duplicate (rust-a ↔ rust-b) + tag overlap (same tags in concepts/)
        // but rust-a/rust-b already found by title, so tag check skips them.
        // Only 1 title match remains.
        assert_eq!(dups.len(), 1);
        assert!(dups[0].reason.contains("identical title"));
    }

    #[test]
    fn test_detect_duplicates_tag_overlap() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let page_a = "---\ntitle: Page A\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [rust, programming, systems, performance]\nrelated: []\nsources: []\n---\nContent A\n";
        let page_b = "---\ntitle: Page B\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: [rust, programming, systems, performance]\nrelated: []\nsources: []\n---\nContent B\n";

        store.write_page("concepts/page-a.md", page_a).unwrap();
        store.write_page("concepts/page-b.md", page_b).unwrap();

        let dups = store.detect_duplicates().unwrap();
        assert!(!dups.is_empty());
        assert!(dups[0].reason.contains("tag overlap"));
    }

    // ── Mermaid graph tests ────────────────────────────────────

    #[test]
    fn test_export_mermaid_full() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let page_a = "---\ntitle: Node A\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: [concepts/node-b.md]\nsources: []\nlayer: core\n---\nBody A\n";
        let page_b = "---\ntitle: Node B\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: []\nsources: []\nlayer: deep\n---\nBody B\n";

        store.write_page("concepts/node-a.md", page_a).unwrap();
        store.write_page("concepts/node-b.md", page_b).unwrap();

        let mermaid = store.export_mermaid(None, 2).unwrap();
        assert!(mermaid.starts_with("graph LR"));
        assert!(mermaid.contains("Node A"));
        assert!(mermaid.contains("Node B"));
        assert!(mermaid.contains("-->")); // edge exists
    }

    #[test]
    fn test_export_mermaid_centered() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let page_a = "---\ntitle: Center\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: [concepts/linked.md]\nsources: []\n---\n";
        let page_b = "---\ntitle: Linked\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: []\nsources: []\n---\n";
        let page_c = "---\ntitle: Isolated\ncreated: 2026-04-20\nupdated: 2026-04-20\ntags: []\nrelated: []\nsources: []\n---\n";

        store.write_page("concepts/center.md", page_a).unwrap();
        store.write_page("concepts/linked.md", page_b).unwrap();
        store.write_page("concepts/isolated.md", page_c).unwrap();

        let mermaid = store.export_mermaid(Some("concepts/center.md"), 1).unwrap();
        assert!(mermaid.contains("Center"));
        assert!(mermaid.contains("Linked"));
        // Isolated page should NOT appear (not within 1 hop)
        assert!(!mermaid.contains("Isolated"));
    }

    // ── FTS5 escape tests ──────────────────────────────────────

    #[test]
    fn test_fts5_escape_query() {
        assert_eq!(fts5_escape_query("hello world"), "\"hello\" \"world\"");
        assert_eq!(fts5_escape_query("rust*"), "\"rust*\"");
        assert_eq!(fts5_escape_query(""), "");
    }
}
