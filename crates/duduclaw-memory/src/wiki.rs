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

/// Provenance of a wiki page — distinguishes raw dialogue captures
/// from human/GVU-verified knowledge so RAG retrieval can weight them differently.
///
/// Auto-derived in `parse_wiki_page` when not explicitly set in frontmatter:
/// - `sources/*` → `RawDialogue`
/// - `concepts/*` or `entities/*` with trust ≥ 0.7 → `VerifiedFact`
/// - everything else → `Unknown`
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    /// Default — pre-existing pages, no provenance signal yet.
    #[default]
    Unknown,
    /// Auto-ingested dialogue (`sources/*.md` from Discord/Telegram/etc).
    /// Treated cautiously — RAG penalises this in ranking.
    RawDialogue,
    /// Tool / system output snapshot (logs, command output).
    ToolOutput,
    /// User-stated fact (explicit assertion in conversation).
    UserStatement,
    /// Promoted concept that has passed audit / GVU verification.
    /// RAG boosts these in ranking.
    VerifiedFact,
}

impl SourceType {
    /// Multiplier applied to search score during ranking.
    /// Together with `(0.5 + trust)`, this discourages raw dialogue from
    /// shadowing verified facts even when keyword match counts are equal.
    pub fn ranking_factor(self) -> f64 {
        match self {
            Self::VerifiedFact => 1.2,
            Self::UserStatement => 1.0,
            Self::ToolOutput => 0.9,
            Self::Unknown => 0.8,
            Self::RawDialogue => 0.6,
        }
    }
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::RawDialogue => write!(f, "raw_dialogue"),
            Self::ToolOutput => write!(f, "tool_output"),
            Self::UserStatement => write!(f, "user_statement"),
            Self::VerifiedFact => write!(f, "verified_fact"),
        }
    }
}

impl FromStr for SourceType {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "unknown" | "" => Ok(Self::Unknown),
            "raw_dialogue" | "dialogue" | "raw" => Ok(Self::RawDialogue),
            "tool_output" | "tool" => Ok(Self::ToolOutput),
            "user_statement" | "user" | "statement" => Ok(Self::UserStatement),
            "verified_fact" | "verified" | "fact" => Ok(Self::VerifiedFact),
            _ => Err(format!("unknown source_type: '{s}'")),
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
    /// Provenance — distinguishes raw dialogue from verified facts.
    /// Auto-derived from path + trust when frontmatter omits it.
    #[serde(default)]
    pub source_type: SourceType,
    /// Last time this page's facts were re-verified by audit / human / GVU.
    /// `None` for pre-existing pages — treated as never-verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified: Option<DateTime<Utc>>,
    /// Cumulative citation count (incremented when RAG returns this page).
    /// Authoritative source is `WikiTrustStore`; this field mirrors a snapshot.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub citation_count: u32,
    /// Cumulative high-error feedback count (negative TrustSignals).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub error_signal_count: u32,
    /// Cumulative low-error feedback count (positive TrustSignals).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub success_signal_count: u32,
    /// If true, RAG will exclude this page from search results.
    /// Set automatically when trust < 0.1, or manually via `wiki_trust_override`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub do_not_inject: bool,
    /// Markdown body (without frontmatter).
    pub body: String,
}

#[inline]
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[inline]
fn is_false(v: &bool) -> bool {
    !v
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
    /// Provenance — see `SourceType`.
    #[serde(default)]
    pub source_type: SourceType,
    /// Whether RAG should skip this page.
    #[serde(default, skip_serializing_if = "is_false")]
    pub do_not_inject: bool,
}

/// A search hit with relevance score.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub score: usize,
    /// Final ranking score: `score * (0.5 + trust) * source_type.ranking_factor()`.
    /// Higher = ranked first.
    pub weighted_score: f64,
    /// Trust score of the matched page.
    pub trust: f32,
    /// Knowledge layer of the matched page.
    pub layer: WikiLayer,
    /// Provenance of the matched page (used for ranking + downstream
    /// trust-feedback decisions).
    pub source_type: SourceType,
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

    /// Derive `agent_id` from the wiki directory path.
    ///
    /// Layout: `<home>/agents/<agent_id>/wiki/` → `Some("<agent_id>")`.
    /// Shared wiki and unrecognised layouts return `None`; callers using
    /// trust_store should treat that as "live trust unavailable" and fall
    /// back to frontmatter trust.
    pub fn derived_agent_id(&self) -> Option<String> {
        if self.shared {
            return None;
        }
        let parent = self.wiki_dir.parent()?;
        let last = parent.file_name()?.to_string_lossy().to_string();
        if last.is_empty() { None } else { Some(last) }
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
            let source_type = extract_field(&content, "source_type")
                .and_then(|v| SourceType::from_str(&v).ok())
                .unwrap_or_else(|| derive_source_type(&rel_str, trust));
            let do_not_inject = extract_field(&content, "do_not_inject")
                .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false);

            pages.push(PageMeta {
                path: rel_str,
                title,
                updated,
                tags,
                author,
                layer,
                trust,
                source_type,
                do_not_inject,
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

    /// Move a quarantined page into `_archive/<original_path>`.
    ///
    /// Used by the Phase 3 janitor when a page has been `do_not_inject`
    /// long enough to merit physical removal from the live tree. The file
    /// remains restorable via `restore_archived`. Returns `true` if a move
    /// happened, `false` if the source path didn't exist (e.g. already archived).
    ///
    /// Refuses to operate when `_archive/` is a symlink — otherwise an
    /// attacker who can write inside `wiki_dir` could redirect archived
    /// pages outside the wiki tree (review H4).
    pub fn archive_page(&self, path: &str) -> Result<bool> {
        self.validate_page_path(path)?;
        let src = self.wiki_dir.join(path);
        if !src.exists() {
            return Ok(false);
        }
        let archive_root = self.wiki_dir.join("_archive");
        if let Ok(meta) = std::fs::symlink_metadata(&archive_root) {
            if meta.file_type().is_symlink() {
                return Err(DuDuClawError::Memory(
                    "_archive/ is a symlink — refusing to archive (would escape wiki dir)".into(),
                ));
            }
        }
        let dest = archive_root.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DuDuClawError::Memory(format!("create archive dir: {e}")))?;
        }
        // Confirm the resolved destination still lives under wiki_dir/_archive
        // (defence in depth — `path` is validate_page_path'd, but a future
        // attacker who plants symlinks deeper in the tree shouldn't escape).
        if let (Ok(canon_root), Ok(canon_dest)) = (
            archive_root.canonicalize(),
            dest.parent()
                .map(|p| p.canonicalize())
                .unwrap_or_else(|| Err(std::io::Error::other("no parent"))),
        ) {
            if !canon_dest.starts_with(&canon_root) {
                return Err(DuDuClawError::Memory(
                    "archive destination escapes _archive/ (symlink detected)".into(),
                ));
            }
        }
        std::fs::rename(&src, &dest)
            .map_err(|e| DuDuClawError::Memory(format!("archive {path}: {e}")))?;

        if let Err(e) = self.remove_from_index(path) {
            warn!("Failed to remove archived {path} from index: {e}");
        }
        if let Err(e) = self.append_log("archive", path) {
            warn!("Failed to log archive {path}: {e}");
        }
        self.fts_sync_remove(path);

        info!(page = path, "Wiki page archived");
        Ok(true)
    }

    /// Move an archived page back to its original location.
    pub fn restore_archived(&self, path: &str) -> Result<bool> {
        self.validate_page_path(path)?;
        let archived = self.wiki_dir.join("_archive").join(path);
        if !archived.exists() {
            return Ok(false);
        }
        let dest = self.wiki_dir.join(path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DuDuClawError::Memory(format!("create restore dir: {e}")))?;
        }
        std::fs::rename(&archived, &dest)
            .map_err(|e| DuDuClawError::Memory(format!("restore {path}: {e}")))?;

        if let Ok(content) = std::fs::read_to_string(&dest) {
            let title = extract_title(&content).unwrap_or_else(|| path.to_string());
            if let Err(e) = self.update_index(path, &title) {
                warn!("Failed to update index for restored {path}: {e}");
            }
            self.fts_sync_upsert(path, &content);
        }
        if let Err(e) = self.append_log("restore", path) {
            warn!("Failed to log restore {path}: {e}");
        }
        info!(page = path, "Wiki page restored from archive");
        Ok(true)
    }

    /// Re-write the page's frontmatter so on-disk `trust` and `do_not_inject`
    /// reflect the current `WikiTrustStore` snapshot. Used by the Phase 3
    /// janitor's snapshot-sync pass.
    pub fn update_frontmatter_trust(
        &self,
        path: &str,
        new_trust: f32,
        new_do_not_inject: bool,
    ) -> Result<()> {
        let mut page = self.read_page(path)?;
        page.trust = new_trust;
        page.do_not_inject = new_do_not_inject;
        page.updated = Utc::now();
        let content = serialize_page(&page);
        self.write_page(path, &content)
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
    /// Ranking: `score * (0.5 + trust) * source_type.ranking_factor()`.
    /// `trust` is sourced from `WikiTrustStore` when available (live RL state),
    /// otherwise falls back to the frontmatter snapshot.
    /// Pages whose live state has `do_not_inject = true` are silently dropped.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let md_files = collect_md_files_recursive(&self.wiki_dir, &self.wiki_dir);
        let mut hits: Vec<SearchHit> = Vec::new();

        // Live trust state from WikiTrustStore (when initialised). Bulk-loaded
        // up-front so each hit only does an in-memory HashMap lookup.
        let live_state: Option<std::collections::HashMap<String, crate::trust_store::WikiTrustSnapshot>> = {
            let agent_id = self.derived_agent_id();
            let store = crate::trust_store::global_trust_store();
            match (agent_id.as_ref(), store) {
                (Some(aid), Some(s)) => {
                    let all_paths: Vec<String> = md_files
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    s.get_many(aid, &all_paths).ok()
                }
                _ => None,
            }
        };

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
            let rel_str = rel.to_string_lossy().to_string();
            let frontmatter_trust = extract_field(&content, "trust")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(default_trust());
            let layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let mut do_not_inject = extract_field(&content, "do_not_inject")
                .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false);

            // Live state overrides frontmatter snapshot when available.
            let trust = if let Some(snap) = live_state.as_ref().and_then(|m| m.get(&rel_str)) {
                if snap.do_not_inject {
                    do_not_inject = true;
                }
                snap.trust
            } else {
                frontmatter_trust
            };

            if do_not_inject {
                continue;
            }
            let source_type = extract_field(&content, "source_type")
                .and_then(|v| SourceType::from_str(&v).ok())
                .unwrap_or_else(|| derive_source_type(&rel_str, trust));
            let weighted_score =
                score as f64 * (0.5 + trust as f64) * source_type.ranking_factor();

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
                path: rel_str,
                title,
                score,
                weighted_score,
                trust,
                layer,
                source_type,
                context_lines,
            });
        }

        // Sort by weighted_score descending (trust + source_type aware ranking)
        hits.sort_by(|a, b| b.weighted_score.partial_cmp(&a.weighted_score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }

    /// `search` variant that records each hit into a `CitationTracker`.
    ///
    /// Behaviour identical to `search` apart from the side-effect: every
    /// returned `SearchHit` is recorded as a `WikiCitation` keyed by the
    /// supplied `conversation_id`. Use this whenever the caller is part of
    /// the live RAG pipeline — the tracker entries are later drained by the
    /// prediction-error feedback bus to update trust scores.
    pub fn search_with_citation(
        &self,
        query: &str,
        limit: usize,
        agent_id: &str,
        conversation_id: &str,
        session_id: Option<&str>,
        tracker: &crate::feedback::CitationTracker,
    ) -> Result<Vec<SearchHit>> {
        let hits = self.search(query, limit)?;
        let now = Utc::now();
        let citations: Vec<crate::feedback::WikiCitation> = hits
            .iter()
            .map(|h| crate::feedback::WikiCitation {
                page_path: h.path.clone(),
                agent_id: agent_id.to_string(),
                conversation_id: conversation_id.to_string(),
                retrieved_at: now,
                trust_at_cite: h.trust,
                source_type: h.source_type,
                session_id: session_id.map(|s| s.to_string()),
            })
            .collect();
        if !citations.is_empty() {
            tracker.record_many(citations);
        }
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
                let do_not_inject = extract_field(&content, "do_not_inject")
                    .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
                    .unwrap_or(false);
                if do_not_inject {
                    continue;
                }
                let source_type = extract_field(&content, "source_type")
                    .and_then(|v| SourceType::from_str(&v).ok())
                    .unwrap_or_else(|| derive_source_type(&exp_path, trust));

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
                    source_type,
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
    ///
    /// Pages with `do_not_inject: true` (frontmatter or live trust state)
    /// are excluded so trust feedback can quarantine misleading pages
    /// without deleting them. Delegates to the meta-aware helper to ensure
    /// both code paths share the same filtering rules (review HIGH-code:
    /// previously this version skipped live trust overrides).
    pub fn collect_by_layer(&self, layer: WikiLayer) -> Result<Vec<(String, String)>> {
        let with_meta = self.collect_by_layer_with_meta(layer)?;
        Ok(with_meta
            .into_iter()
            .map(|(path, body, _trust)| (path, body))
            .collect())
    }

    /// Build injection context from L0 (Identity) + L1 (Core) pages,
    /// respecting a character budget.
    ///
    /// Returns combined text suitable for system prompt injection.
    /// L0 pages are injected first (highest priority), then L1.
    /// L2/L3 are excluded — they are retrieved on-demand via search.
    pub fn build_injection_context(&self, max_chars: usize) -> Result<String> {
        let (text, _injected) = self.build_injection_context_inner(max_chars)?;
        Ok(text)
    }

    /// Variant of `build_injection_context` that also records which pages
    /// landed in the prompt as wiki citations.
    ///
    /// Use this from the live runner so the feedback bus can later attribute
    /// prediction error to the exact pages we surfaced.
    pub fn build_injection_context_with_citations(
        &self,
        max_chars: usize,
        agent_id: &str,
        conversation_id: &str,
        session_id: Option<&str>,
        tracker: &crate::feedback::CitationTracker,
    ) -> Result<String> {
        let (text, injected) = self.build_injection_context_inner(max_chars)?;
        if !injected.is_empty() {
            let now = Utc::now();
            let citations: Vec<crate::feedback::WikiCitation> = injected
                .into_iter()
                .map(|(path, trust)| {
                    let st = derive_source_type(&path, trust);
                    crate::feedback::WikiCitation {
                        page_path: path,
                        agent_id: agent_id.to_string(),
                        conversation_id: conversation_id.to_string(),
                        retrieved_at: now,
                        trust_at_cite: trust,
                        source_type: st,
                        session_id: session_id.map(|s| s.to_string()),
                    }
                })
                .collect();
            tracker.record_many(citations);
        }
        Ok(text)
    }

    /// Shared implementation — returns the combined prompt text *and* the list
    /// of `(page_path, trust)` pairs actually included (after the byte budget).
    fn build_injection_context_inner(
        &self,
        max_chars: usize,
    ) -> Result<(String, Vec<(String, f32)>)> {
        let mut output = String::new();
        let mut remaining = max_chars;
        let mut injected: Vec<(String, f32)> = Vec::new();

        for layer in [WikiLayer::Identity, WikiLayer::Core] {
            let pages = self.collect_by_layer_with_meta(layer)?;
            if pages.is_empty() {
                continue;
            }

            let header = format!("### Wiki — {layer}\n\n");
            if header.len() >= remaining {
                break;
            }
            output.push_str(&header);
            remaining -= header.len();

            for (path, body, trust) in &pages {
                let needed = body.len() + 2; // +2 for trailing newlines
                if needed > remaining {
                    break;
                }
                output.push_str(body);
                output.push_str("\n\n");
                remaining -= needed;
                injected.push((path.clone(), *trust));
            }
        }

        Ok((output, injected))
    }

    /// Internal — like `collect_by_layer` but also yields the trust score
    /// so we can record it in the citation log. Honours `WikiTrustStore`
    /// live `do_not_inject` overrides when initialised.
    ///
    /// Made `pub` for #14 (2026-05-12) so the gateway can apply
    /// query-driven relevance ranking on top before injection. The
    /// existing `build_injection_context_*` paths still call this
    /// internally and apply no ranking — only the new gateway-side
    /// `ranked_wiki_injection` helper consumes it for ranking.
    pub fn collect_by_layer_with_meta(
        &self,
        layer: WikiLayer,
    ) -> Result<Vec<(String, String, f32)>> {
        let md_files = collect_md_files_recursive(&self.wiki_dir, &self.wiki_dir);
        let mut results = Vec::new();

        let live_state: Option<std::collections::HashMap<String, crate::trust_store::WikiTrustSnapshot>> = {
            let agent_id = self.derived_agent_id();
            let store = crate::trust_store::global_trust_store();
            match (agent_id.as_ref(), store) {
                (Some(aid), Some(s)) => {
                    let all_paths: Vec<String> = md_files
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    s.get_many(aid, &all_paths).ok()
                }
                _ => None,
            }
        };

        for rel in &md_files {
            let full = self.wiki_dir.join(rel);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let page_layer = extract_field(&content, "layer")
                .and_then(|v| WikiLayer::from_str(&v).ok())
                .unwrap_or_default();
            let frontmatter_no_inject = extract_field(&content, "do_not_inject")
                .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
                .unwrap_or(false);
            let rel_str = rel.to_string_lossy().to_string();
            let live_no_inject = live_state
                .as_ref()
                .and_then(|m| m.get(&rel_str))
                .map(|s| s.do_not_inject)
                .unwrap_or(false);
            if page_layer == layer && !frontmatter_no_inject && !live_no_inject {
                let body = extract_body(&content);
                let title = extract_title(&content)
                    .unwrap_or_else(|| rel_str.clone());
                let frontmatter_trust = extract_field(&content, "trust")
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(default_trust());
                let trust = live_state
                    .as_ref()
                    .and_then(|m| m.get(&rel_str))
                    .map(|s| s.trust)
                    .unwrap_or(frontmatter_trust);
                results.push((
                    rel_str,
                    format!("## {title}\n\n{body}"),
                    trust,
                ));
            }
        }
        results.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(results)
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
    let explicit_source_type = extract_field(content, "source_type")
        .and_then(|v| SourceType::from_str(&v).ok());
    let source_type = explicit_source_type.unwrap_or_else(|| derive_source_type(path, trust));
    let last_verified = extract_datetime_field(content, "last_verified");
    let citation_count = extract_field(content, "citation_count")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let error_signal_count = extract_field(content, "error_signal_count")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let success_signal_count = extract_field(content, "success_signal_count")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let do_not_inject = extract_field(content, "do_not_inject")
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(false);
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
        source_type,
        last_verified,
        citation_count,
        error_signal_count,
        success_signal_count,
        do_not_inject,
        body,
    })
}

/// Re-serialise a `WikiPage` back to markdown — preserves all frontmatter
/// fields including the Phase 0/1/2 additions.
pub fn serialize_page(page: &WikiPage) -> String {
    let mut fm = String::new();
    fm.push_str("---\n");
    // Strings that may contain user-controlled newlines / quotes are wrapped
    // in JSON-style quoted scalars so they cannot break out of frontmatter
    // and inject extra YAML keys (CRITICAL — security review C1/H3).
    fm.push_str(&format!("title: {}\n", yaml_quote(&page.title)));
    fm.push_str(&format!("created: {}\n", page.created.to_rfc3339()));
    fm.push_str(&format!("updated: {}\n", page.updated.to_rfc3339()));
    fm.push_str(&format!("tags: [{}]\n", yaml_quoted_list(&page.tags)));
    if !page.related.is_empty() {
        fm.push_str(&format!("related: [{}]\n", yaml_quoted_list(&page.related)));
    }
    if !page.sources.is_empty() {
        fm.push_str(&format!("sources: [{}]\n", yaml_quoted_list(&page.sources)));
    }
    if let Some(author) = &page.author {
        fm.push_str(&format!("author: {}\n", yaml_quote(author)));
    }
    fm.push_str(&format!("layer: {}\n", page.layer));
    fm.push_str(&format!("trust: {:.3}\n", page.trust));
    if page.source_type != SourceType::Unknown {
        fm.push_str(&format!("source_type: {}\n", page.source_type));
    }
    if let Some(lv) = page.last_verified {
        fm.push_str(&format!("last_verified: {}\n", lv.to_rfc3339()));
    }
    // Phase 0/2: preserve trust-feedback counters across round-trips.
    // Skipping zero values keeps legacy pages clean (CRITICAL — code review C1).
    if page.citation_count != 0 {
        fm.push_str(&format!("citation_count: {}\n", page.citation_count));
    }
    if page.error_signal_count != 0 {
        fm.push_str(&format!("error_signal_count: {}\n", page.error_signal_count));
    }
    if page.success_signal_count != 0 {
        fm.push_str(&format!(
            "success_signal_count: {}\n",
            page.success_signal_count
        ));
    }
    if page.do_not_inject {
        fm.push_str("do_not_inject: true\n");
    }
    fm.push_str("---\n\n");
    fm.push_str(&page.body);
    fm
}

/// JSON-style quoted YAML scalar — escapes `"`, `\`, line/paragraph
/// separators, and any control char so user-controlled strings cannot
/// break out of frontmatter and inject extra YAML keys.
///
/// Escape format is intentionally a strict subset of YAML 1.2 double-quoted
/// scalars (and JSON), so `extract_field` + `yaml_unescape` below can
/// faithfully round-trip any value.
fn yaml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\u0000"),
            // YAML 1.2 treats U+2028 / U+2029 as line breaks inside
            // double-quoted scalars (review security 1) — escape them.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c.is_control() => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Reverse of `yaml_quote` for round-trip parsing of frontmatter scalars.
/// Operates on a string that has had its outer `"..."` already stripped.
/// Unknown escape sequences pass through verbatim — never panics.
fn yaml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('u') => {
                // Read up to 4 hex digits — `\uXXXX`. Anything malformed
                // falls through to literal.
                let mut hex = String::with_capacity(4);
                for _ in 0..4 {
                    match chars.clone().next() {
                        Some(h) if h.is_ascii_hexdigit() => {
                            hex.push(h);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                if let Ok(code) = u32::from_str_radix(&hex, 16) {
                    if let Some(decoded) = char::from_u32(code) {
                        out.push(decoded);
                    }
                } else {
                    out.push_str("\\u");
                    out.push_str(&hex);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn yaml_quoted_list(items: &[String]) -> String {
    items
        .iter()
        .map(|t| yaml_quote(t))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Auto-derive `SourceType` from path heuristics when frontmatter omits it.
///
/// - `sources/*` → `RawDialogue` (Discord/Telegram auto-ingest)
/// - `concepts/*` or `entities/*` with trust ≥ 0.7 → `VerifiedFact`
/// - everything else → `Unknown`
pub fn derive_source_type(path: &str, trust: f32) -> SourceType {
    let normalised = path.trim_start_matches('/');
    if normalised.starts_with("sources/") {
        SourceType::RawDialogue
    } else if (normalised.starts_with("concepts/") || normalised.starts_with("entities/"))
        && trust >= 0.7
    {
        SourceType::VerifiedFact
    } else {
        SourceType::Unknown
    }
}

/// Extract a string field from YAML frontmatter (best-effort, no YAML parser).
///
/// Round-trips with `yaml_quote`: a value written as `"hello\nworld"`
/// returns as `hello\nworld` with a real newline (review HIGH R2-1).
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
            let raw = after.trim();
            let val = unquote_scalar(raw);
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Read the raw contents inside `[...]` for an inline list field.
/// Returns the substring between the outermost brackets, or None if the
/// field is missing / not in inline form. Does not interpret escapes.
fn extract_inline_list_raw(content: &str, field: &str) -> Option<String> {
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
            let after = after.trim();
            if let Some(stripped) = after.strip_prefix('[') {
                if let Some(inner) = stripped.strip_suffix(']') {
                    return Some(inner.to_string());
                }
            }
        }
    }
    None
}

/// Split an inline YAML list body into individual scalars.
///
/// Respects double-quoted runs so commas inside `"a, b"` stay together.
/// (review HIGH R2: previous naive `.split(',')` mangled tags with commas.)
fn split_yaml_inline_list(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                current.push('\\');
                escape = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push('"');
            }
            ',' if !in_quotes => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    items.push(unquote_scalar(&trimmed));
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        items.push(unquote_scalar(&trimmed));
    }
    items
}

/// Strip outer YAML quotes (single or double) from a scalar and apply
/// `yaml_unescape` when double-quoted. Single-quoted YAML doesn't process
/// `\` escapes — the only escape is `''` for a literal apostrophe — so we
/// preserve the inner string verbatim.
fn unquote_scalar(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        yaml_unescape(&raw[1..raw.len() - 1])
    } else if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        raw[1..raw.len() - 1].replace("''", "'")
    } else {
        raw.to_string()
    }
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
    // Try inline format first. Cannot use `extract_field` because that path
    // already unescapes the WHOLE quoted value — for inline lists like
    // `tags: ["a, b", "c"]` we need to split BEFORE unescaping per-item.
    if let Some(raw_inline) = extract_inline_list_raw(content, field) {
        let parsed = split_yaml_inline_list(&raw_inline);
        if !parsed.is_empty() {
            return parsed;
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
                let val = unquote_scalar(item.trim());
                if !val.is_empty() {
                    items.push(val);
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
            // Phase 3 archive directory: skip — pages here are quarantined,
            // RAG and listings should not surface them (review M6).
            // We compare by name only (depth-1 from wiki root) so that any
            // legitimate user content in `concepts/_archive_notes/` etc.
            // stays visible.
            if depth == 0 && path.file_name().and_then(|n| n.to_str()) == Some("_archive") {
                continue;
            }
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
                // FTS table doesn't store source_type yet — derive from path + trust.
                let source_type = derive_source_type(&path, trust);
                // FTS5 rank is negative (lower = better), convert to positive score
                let score = (-fts_rank * 10.0).max(1.0) as usize;
                let weighted_score =
                    score as f64 * (0.5 + trust as f64) * source_type.ranking_factor();

                Ok(SearchHit {
                    path,
                    title,
                    score,
                    weighted_score,
                    trust,
                    layer,
                    source_type,
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

    // ── Phase 0/1: SourceType + citation tracking ──────────────

    #[test]
    fn source_type_inferred_from_path() {
        assert_eq!(derive_source_type("sources/2026-05-03.md", 0.4), SourceType::RawDialogue);
        assert_eq!(derive_source_type("concepts/cron.md", 0.9), SourceType::VerifiedFact);
        assert_eq!(derive_source_type("concepts/cron.md", 0.4), SourceType::Unknown);
        assert_eq!(derive_source_type("entities/me.md", 0.8), SourceType::VerifiedFact);
        assert_eq!(derive_source_type("research/paper.md", 0.9), SourceType::Unknown);
    }

    #[test]
    fn legacy_page_loads_with_default_provenance() {
        let content = page_with_layer_trust("Old Page", "deep", 0.4);
        let page = parse_wiki_page("research/old.md", &content).unwrap();
        assert_eq!(page.source_type, SourceType::Unknown);
        assert_eq!(page.citation_count, 0);
        assert_eq!(page.error_signal_count, 0);
        assert!(!page.do_not_inject);
        assert!(page.last_verified.is_none());
    }

    #[test]
    fn explicit_source_type_overrides_path_inference() {
        let content = "---\ntitle: Tagged\ncreated: 2026-04-20\nupdated: 2026-04-20\nlayer: deep\ntrust: 0.4\nsource_type: verified_fact\n---\nBody.";
        let page = parse_wiki_page("sources/x.md", content).unwrap();
        assert_eq!(page.source_type, SourceType::VerifiedFact);
    }

    #[test]
    fn search_records_citations_in_tracker() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store
            .write_page("concepts/a.md", &page_with_layer_trust("Alpha rust", "core", 0.8))
            .unwrap();
        store
            .write_page("concepts/b.md", &page_with_layer_trust("Beta rust", "core", 0.6))
            .unwrap();

        let tracker = crate::feedback::CitationTracker::new();
        let hits = store
            .search_with_citation("rust", 10, "agnes", "conv-1", None, &tracker)
            .unwrap();
        assert_eq!(hits.len(), 2);

        let drained = tracker.drain("conv-1");
        assert_eq!(drained.len(), 2);
        assert!(drained.iter().all(|c| c.agent_id == "agnes"));
        assert!(drained.iter().any(|c| c.page_path == "concepts/a.md"));
        assert!(drained.iter().any(|c| c.page_path == "concepts/b.md"));
    }

    #[test]
    fn injection_records_citations() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store
            .write_page("entities/me.md", &page_with_layer_trust("Identity", "identity", 0.9))
            .unwrap();
        store
            .write_page("concepts/env.md", &page_with_layer_trust("Env", "core", 0.8))
            .unwrap();
        // Deep page should not appear in injection or citations
        store
            .write_page("concepts/deep.md", &page_with_layer_trust("Deep", "deep", 0.5))
            .unwrap();

        let tracker = crate::feedback::CitationTracker::new();
        let ctx = store
            .build_injection_context_with_citations(10000, "agnes", "conv-2", None, &tracker)
            .unwrap();
        assert!(ctx.contains("Identity"));
        assert!(ctx.contains("Env"));

        let drained = tracker.drain("conv-2");
        let paths: Vec<_> = drained.iter().map(|c| c.page_path.as_str()).collect();
        assert!(paths.contains(&"entities/me.md"));
        assert!(paths.contains(&"concepts/env.md"));
        assert!(!paths.contains(&"concepts/deep.md")); // L3 excluded
    }

    #[test]
    fn do_not_inject_excludes_page_from_search_and_injection() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        let banished = "---\ntitle: Banished\ncreated: 2026-04-20\nupdated: 2026-04-20\nlayer: core\ntrust: 0.05\ndo_not_inject: true\n---\nrust details.\n";
        let kept = page_with_layer_trust("Kept rust page", "core", 0.8);
        store.write_page("concepts/banished.md", banished).unwrap();
        store.write_page("concepts/kept.md", &kept).unwrap();

        let hits = store.search("rust", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "concepts/kept.md");

        let ctx = store.build_injection_context(10000).unwrap();
        assert!(ctx.contains("Kept rust page"));
        assert!(!ctx.contains("Banished"));
    }

    #[test]
    fn yaml_quote_round_trip_preserves_special_chars() {
        // Regression for review HIGH R2-1: yaml_quote was lossy on read.
        let cases = [
            "simple",
            "with \"double\" quotes",
            "with \\ backslash",
            "with\nnewline",
            "with\ttab",
            "with\rCR",
            "with\u{2028}line-sep\u{2029}para-sep",
            "C:\\path\\to\\file",
            "control \u{0001}\u{007f}",
        ];
        for original in cases {
            let quoted = yaml_quote(original);
            // Quoted form must start and end with `"`.
            assert!(quoted.starts_with('"') && quoted.ends_with('"'));
            // Round-trip via unquote_scalar.
            let back = unquote_scalar(&quoted);
            assert_eq!(back, original, "round-trip lost data for: {original:?}");
            // Quoted form must contain no raw newline / U+2028 / U+2029,
            // otherwise frontmatter scanners that split on lines could be
            // tricked into reading injected keys.
            assert!(!quoted.contains('\n'));
            assert!(!quoted.contains('\u{2028}'));
            assert!(!quoted.contains('\u{2029}'));
        }
    }

    #[test]
    fn yaml_inline_list_split_handles_quoted_commas() {
        // Regression for review HIGH R2-2: tags with commas got mangled.
        let raw = "\"alpha, beta\", \"gamma\", \"delta\\\\back\"";
        let items = split_yaml_inline_list(raw);
        assert_eq!(items, vec![
            "alpha, beta".to_string(),
            "gamma".to_string(),
            "delta\\back".to_string(),
        ]);
    }

    #[test]
    fn serialize_page_round_trip_preserves_counters_and_special_chars() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        // Seed via plain frontmatter, then read into WikiPage and rewrite.
        store
            .write_page(
                "concepts/round-trip.md",
                &page_with_layer_trust("Hello\nworld", "core", 0.85),
            )
            .unwrap();

        let mut page = store.read_page("concepts/round-trip.md").unwrap();
        // Mutate counters that previously got dropped on rewrite.
        page.citation_count = 7;
        page.error_signal_count = 2;
        page.success_signal_count = 5;
        page.do_not_inject = false;
        let new_content = serialize_page(&page);
        store
            .write_page("concepts/round-trip.md", &new_content)
            .unwrap();

        let parsed = store.read_page("concepts/round-trip.md").unwrap();
        assert_eq!(parsed.citation_count, 7);
        assert_eq!(parsed.error_signal_count, 2);
        assert_eq!(parsed.success_signal_count, 5);
    }

    #[test]
    fn search_uses_live_trust_when_store_initialised() {
        // Build wiki under a fake "agnes" path so derived_agent_id() returns Some("agnes").
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents").join("agnes");
        let wiki_dir = agents_dir.join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        store
            .write_page(
                "concepts/page.md",
                &page_with_layer_trust("Page rust", "core", 0.9),
            )
            .unwrap();

        // No trust store yet → frontmatter trust used.
        let hits = store.search("rust", 10).unwrap();
        assert!((hits[0].trust - 0.9).abs() < 1e-3);

        // Initialise per-test trust store with a low live trust + do_not_inject.
        let trust_store = std::sync::Arc::new(crate::trust_store::WikiTrustStore::in_memory().unwrap());
        trust_store
            .manual_set("concepts/page.md", "agnes", 0.05, false, Some(true), None)
            .unwrap();
        crate::trust_store::_set_global_trust_store_for_test(trust_store);

        // Now search should drop the page entirely (live do_not_inject).
        let hits = store.search("rust", 10).unwrap();
        assert!(hits.is_empty(), "live do_not_inject should hide page from search");
    }

    #[test]
    fn search_ranks_verified_fact_over_raw_dialogue() {
        let tmp = TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let store = WikiStore::new(wiki_dir);
        store.ensure_scaffold().unwrap();

        // Both pages match "cron"; the dialogue has trust 0.4, the verified
        // concept has trust 0.9. With type-factor multiplier the gap widens.
        store.write_page(
            "sources/discord-cron.md",
            "---\ntitle: Old discord cron talk\ncreated: 2026-04-20\nupdated: 2026-04-20\nlayer: context\ntrust: 0.4\n---\ncron session-only.\n",
        ).unwrap();
        store.write_page(
            "concepts/cron-facts.md",
            "---\ntitle: Cron facts\ncreated: 2026-04-20\nupdated: 2026-04-20\nlayer: core\ntrust: 0.9\n---\ncron persistent.\n",
        ).unwrap();

        let hits = store.search("cron", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "concepts/cron-facts.md");
        assert_eq!(hits[0].source_type, SourceType::VerifiedFact);
        assert_eq!(hits[1].source_type, SourceType::RawDialogue);
        // Concept score should be at least 2× dialogue (verified 1.2 × trust 1.4
        // vs raw 0.6 × trust 0.9 → 1.68 vs 0.54 — > 3× separation).
        assert!(hits[0].weighted_score > hits[1].weighted_score * 2.0);
    }
}
