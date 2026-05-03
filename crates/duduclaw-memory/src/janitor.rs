//! Wiki Janitor — Phase 3 maintenance loop for trust feedback.
//!
//! Three responsibilities, all idempotent and safe to run on a periodic
//! schedule (typically once per day):
//!
//! 1. **Auto-correct tagging** — Pages that accumulate `auto_correct_threshold`
//!    negative signals within the rolling `auto_correct_window_days` window
//!    get a `corrected` tag added to their frontmatter and a 📝 note appended
//!    to the body, prompting human / GVU review.
//!
//! 2. **Auto-archive** — Pages that have been quarantined (`do_not_inject = true`)
//!    AND idle for at least `archive_age_days` are physically moved from their
//!    original path into `wiki/_archive/...`. Restorable via `restore_archived`.
//!
//! 3. **Trust snapshot back-write** (best-effort) — periodically writes the
//!    live `WikiTrustStore.trust` value back into the page's frontmatter so
//!    offline tooling / git diffs can see the current value.
//!
//! Recovery acceleration (low-trust positive signals scaled × 1.5) lives in
//! `WikiTrustStore.upsert_signal` — already part of Phase 2.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tracing::{info, warn};

use duduclaw_core::error::Result;

use crate::trust_store::WikiTrustStore;
use crate::wiki::WikiStore;

// ---------------------------------------------------------------------------
// Config + report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct JanitorConfig {
    /// How many negative signals (within the rolling window) trigger auto-correct.
    pub auto_correct_threshold: u32,
    pub auto_correct_window_days: i64,
    /// Quarantined (`do_not_inject`) pages older than this get archived.
    pub archive_age_days: i64,
    /// Refrain from re-tagging the same page within this interval.
    pub re_correct_cooldown_hours: i64,
    /// Audit history rows older than this are pruned per pass.
    pub keep_history_days: i64,
    /// Cap on rows deleted per pass — protects writers from a long-blocking
    /// DELETE on first run after enabling retention.
    pub max_prune_rows_per_pass: i64,
}

impl Default for JanitorConfig {
    fn default() -> Self {
        Self {
            auto_correct_threshold: 3,
            auto_correct_window_days: 30,
            archive_age_days: 30,
            re_correct_cooldown_hours: 24,
            keep_history_days: 90,
            max_prune_rows_per_pass: 50_000,
        }
    }
}

impl JanitorConfig {
    /// Read overrides from `[wiki.trust_feedback.janitor]`. Missing keys fall
    /// back to defaults; out-of-range values are clamped so a typo can't
    /// break the gateway's daily maintenance loop.
    pub fn from_toml(root: &toml::Table) -> Self {
        let mut cfg = Self::default();
        let section = root
            .get("wiki")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("trust_feedback"))
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("janitor"))
            .and_then(|v| v.as_table());
        if let Some(s) = section {
            if let Some(v) = s.get("auto_correct_threshold").and_then(|v| v.as_integer()) {
                cfg.auto_correct_threshold = v.clamp(1, 10_000) as u32;
            }
            if let Some(v) = s.get("auto_correct_window_days").and_then(|v| v.as_integer()) {
                cfg.auto_correct_window_days = v.clamp(1, 3650);
            }
            if let Some(v) = s.get("archive_age_days").and_then(|v| v.as_integer()) {
                cfg.archive_age_days = v.clamp(1, 3650);
            }
            if let Some(v) = s
                .get("re_correct_cooldown_hours")
                .and_then(|v| v.as_integer())
            {
                cfg.re_correct_cooldown_hours = v.clamp(1, 24 * 365);
            }
            if let Some(v) = s.get("keep_history_days").and_then(|v| v.as_integer()) {
                cfg.keep_history_days = v.clamp(1, 3650);
            }
            if let Some(v) = s.get("max_prune_rows_per_pass").and_then(|v| v.as_integer()) {
                cfg.max_prune_rows_per_pass = v.clamp(100, 1_000_000);
            }
        }
        cfg
    }
}

#[derive(Debug, Default, Clone)]
pub struct JanitorReport {
    pub corrected_pages: Vec<String>,
    pub archived_pages: Vec<String>,
    pub snapshot_synced: u64,
    pub errors: u64,
}

// ---------------------------------------------------------------------------
// Janitor
// ---------------------------------------------------------------------------

pub struct WikiJanitor {
    store: Arc<WikiTrustStore>,
    config: JanitorConfig,
}

impl WikiJanitor {
    pub fn new(store: Arc<WikiTrustStore>) -> Self {
        Self::with_config(store, JanitorConfig::default())
    }

    pub fn with_config(store: Arc<WikiTrustStore>, config: JanitorConfig) -> Self {
        Self { store, config }
    }

    /// Run all three passes for a given agent's wiki.
    ///
    /// Caller is responsible for invoking once per agent — typically iterating
    /// over `~/.duduclaw/agents/<id>/wiki/` directories from the gateway's
    /// daily heartbeat.
    pub fn run_once(&self, wiki_dir: impl AsRef<Path>, agent_id: &str) -> JanitorReport {
        let wiki_dir = wiki_dir.as_ref().to_path_buf();
        let mut report = JanitorReport::default();
        let wiki = WikiStore::new(wiki_dir.clone());

        match self.auto_correct_pass(&wiki, agent_id) {
            Ok(corrected) => report.corrected_pages = corrected,
            Err(e) => {
                warn!(agent = agent_id, "auto-correct pass failed: {e}");
                report.errors += 1;
            }
        }

        match self.archive_pass(&wiki, agent_id) {
            Ok(archived) => report.archived_pages = archived,
            Err(e) => {
                warn!(agent = agent_id, "archive pass failed: {e}");
                report.errors += 1;
            }
        }

        match self.snapshot_sync_pass(&wiki, agent_id) {
            Ok(n) => report.snapshot_synced = n,
            Err(e) => {
                warn!(agent = agent_id, "snapshot sync pass failed: {e}");
                report.errors += 1;
            }
        }

        // (review HIGH-DB N3) Retention pruning is GLOBAL — pulled out of
        // the per-agent loop. Caller must invoke `run_global_retention()`
        // once per janitor cycle.

        info!(
            agent = agent_id,
            corrected = report.corrected_pages.len(),
            archived = report.archived_pages.len(),
            snapshots = report.snapshot_synced,
            "wiki janitor pass complete"
        );
        report
    }

    /// Run retention pruning ONCE per janitor cycle (not per agent).
    /// Returns `(history_deleted, rate_deleted, conv_cap_deleted)`.
    pub fn run_global_retention(&self) -> Result<(u64, u64, u64)> {
        self.store.prune_retention(
            self.config.keep_history_days,
            self.config.max_prune_rows_per_pass,
        )
    }

    // ── Pass 1: auto-correct tagging ──────────────────────────────

    fn auto_correct_pass(&self, wiki: &WikiStore, agent_id: &str) -> Result<Vec<String>> {
        let candidates = self.find_correction_candidates(agent_id)?;
        let mut tagged = Vec::new();
        for (page_path, recent_negative_count) in candidates {
            match self.tag_page_corrected(wiki, &page_path, recent_negative_count) {
                Ok(true) => tagged.push(page_path),
                Ok(false) => {} // already tagged within cooldown
                Err(e) => warn!(page = %page_path, "auto-correct tagging failed: {e}"),
            }
        }
        Ok(tagged)
    }

    /// Pages with `>= auto_correct_threshold` negative signals within the
    /// rolling window. Excludes pages corrected within `re_correct_cooldown_hours`.
    fn find_correction_candidates(&self, agent_id: &str) -> Result<Vec<(String, u32)>> {
        self.store.query_history_aggregate(
            agent_id,
            self.config.auto_correct_window_days,
            self.config.auto_correct_threshold,
            self.config.re_correct_cooldown_hours,
        )
    }

    fn tag_page_corrected(
        &self,
        wiki: &WikiStore,
        page_path: &str,
        recent_negative_count: u32,
    ) -> Result<bool> {
        let page = wiki.read_page(page_path)?;

        // If `corrected` already in tags, only re-tag after cooldown — handled
        // by SQL `last_correction_at` filter; here we just ensure idempotency.
        let already_tagged = page.tags.iter().any(|t| t == "corrected");
        if already_tagged {
            // Still record the audit row — but skip rewrite.
            self.store.record_correction_audit(page_path, &page.author.clone().unwrap_or_default(), recent_negative_count)?;
            return Ok(false);
        }

        // Build a corrected tag list (copy + push) and a body suffix.
        let mut new_tags = page.tags.clone();
        new_tags.push("corrected".to_string());

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let suffix = format!(
            "\n\n---\n\n> 📝 **[{today} 自動標記]** 此頁面在 {window} 天內伴隨 {neg} 次高 prediction error，已自動標記為待審核。請人工或 GVU 核對事實。\n",
            window = self.config.auto_correct_window_days,
            neg = recent_negative_count,
        );

        let new_content = rewrite_page_with_tags_and_suffix(&page, &new_tags, &suffix);
        wiki.write_page(page_path, &new_content)?;

        // Record audit so we know when this happened (cooldown).
        let agent_id = wiki
            .derived_agent_id()
            .unwrap_or_default();
        self.store
            .record_correction_audit(page_path, &agent_id, recent_negative_count)?;
        Ok(true)
    }

    // ── Pass 2: auto-archive ──────────────────────────────────────

    fn archive_pass(&self, wiki: &WikiStore, agent_id: &str) -> Result<Vec<String>> {
        let candidates = self
            .store
            .list_archive_candidates(agent_id, self.config.archive_age_days)?;
        let mut archived = Vec::new();
        for page_path in candidates {
            match wiki.archive_page(&page_path) {
                Ok(true) => archived.push(page_path),
                Ok(false) => {} // already archived (file moved)
                Err(e) => warn!(page = %page_path, "archive failed: {e}"),
            }
        }
        Ok(archived)
    }

    // ── Pass 3: snapshot sync ─────────────────────────────────────

    fn snapshot_sync_pass(&self, wiki: &WikiStore, agent_id: &str) -> Result<u64> {
        let snapshots = self.store.list_low_trust(agent_id, 1.0, 10_000)?;
        let mut synced = 0u64;
        for snap in snapshots {
            // Skip pages whose live state is identical to the snapshot — avoid
            // pointless writes / git churn.
            let page = match wiki.read_page(&snap.page_path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if (page.trust - snap.trust).abs() < 0.005
                && page.do_not_inject == snap.do_not_inject
            {
                continue;
            }
            if let Err(e) = wiki.update_frontmatter_trust(&snap.page_path, snap.trust, snap.do_not_inject) {
                warn!(page = %snap.page_path, "snapshot frontmatter sync failed: {e}");
                continue;
            }
            synced += 1;
        }
        Ok(synced)
    }
}

// ---------------------------------------------------------------------------
// Helpers — frontmatter rewriting
// ---------------------------------------------------------------------------

/// Append a corrected-tag note to a wiki page without losing any frontmatter
/// fields. Delegates to `serialize_page` so escaping + counter preservation
/// stays consistent across all rewrite paths (CRITICAL — code review C1).
fn rewrite_page_with_tags_and_suffix(
    page: &crate::wiki::WikiPage,
    new_tags: &[String],
    suffix: &str,
) -> String {
    let mut updated = page.clone();
    updated.tags = new_tags.to_vec();
    updated.updated = Utc::now();
    let mut new_body = page.body.clone();
    new_body.push_str(suffix);
    updated.body = new_body;
    crate::wiki::serialize_page(&updated)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_page(title: &str, body: &str, trust: f32) -> String {
        format!(
            "---\ntitle: {title}\ncreated: 2026-04-20T00:00:00+00:00\nupdated: 2026-04-20T00:00:00+00:00\ntags: [test]\nrelated: []\nsources: []\nlayer: core\ntrust: {trust}\n---\n\n{body}\n"
        )
    }

    fn setup_wiki() -> (TempDir, WikiStore) {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join("agents").join("agnes");
        let wiki_dir = agents_dir.join("wiki");
        std::fs::create_dir_all(&wiki_dir).unwrap();
        let wiki = WikiStore::new(wiki_dir);
        wiki.ensure_scaffold().unwrap();
        (tmp, wiki)
    }

    #[test]
    fn auto_correct_tags_page_after_three_negatives() {
        let (_tmp, wiki) = setup_wiki();
        wiki.write_page("concepts/x.md", &make_page("X", "Body of X.", 0.5))
            .unwrap();

        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        // Inject 3 negative signals via the public API.
        for i in 0..3 {
            let _ = store.upsert_signal(
                "concepts/x.md",
                "agnes",
                crate::feedback::TrustSignal::Negative { magnitude: 0.05 },
                Some(&format!("c{i}")),
                Some(0.8),
            );
        }

        let janitor = WikiJanitor::with_config(
            store,
            JanitorConfig {
                auto_correct_threshold: 3,
                auto_correct_window_days: 30,
                archive_age_days: 30,
                re_correct_cooldown_hours: 24,
                ..JanitorConfig::default()
            },
        );
        let report = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert_eq!(report.corrected_pages, vec!["concepts/x.md".to_string()]);

        // Page was rewritten with corrected tag + suffix.
        let page = wiki.read_page("concepts/x.md").unwrap();
        assert!(page.tags.iter().any(|t| t == "corrected"));
        assert!(page.body.contains("自動標記"));
    }

    #[test]
    fn auto_correct_skips_when_under_threshold() {
        let (_tmp, wiki) = setup_wiki();
        wiki.write_page("concepts/y.md", &make_page("Y", "Body.", 0.5))
            .unwrap();

        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        for i in 0..2 {
            let _ = store.upsert_signal(
                "concepts/y.md",
                "agnes",
                crate::feedback::TrustSignal::Negative { magnitude: 0.05 },
                Some(&format!("c{i}")),
                Some(0.8),
            );
        }

        let janitor = WikiJanitor::new(store);
        let report = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert!(report.corrected_pages.is_empty());

        let page = wiki.read_page("concepts/y.md").unwrap();
        assert!(!page.tags.iter().any(|t| t == "corrected"));
    }

    #[test]
    fn auto_correct_idempotent_within_cooldown() {
        let (_tmp, wiki) = setup_wiki();
        wiki.write_page("concepts/z.md", &make_page("Z", "Body.", 0.5))
            .unwrap();

        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        for i in 0..3 {
            let _ = store.upsert_signal(
                "concepts/z.md",
                "agnes",
                crate::feedback::TrustSignal::Negative { magnitude: 0.05 },
                Some(&format!("c{i}")),
                Some(0.8),
            );
        }

        let janitor = WikiJanitor::new(store);
        let r1 = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert_eq!(r1.corrected_pages.len(), 1);

        let r2 = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert!(r2.corrected_pages.is_empty(), "cooldown should suppress retag");
    }

    #[test]
    fn archive_moves_quarantined_pages() {
        let (_tmp, wiki) = setup_wiki();
        wiki.write_page("concepts/banished.md", &make_page("Banished", "Body.", 0.05))
            .unwrap();

        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        store
            .manual_set("concepts/banished.md", "agnes", 0.05, false, Some(true), None)
            .unwrap();
        // Force last_signal_at into the past so the age threshold passes.
        store
            .force_archive_age_for_test("concepts/banished.md", "agnes", 60)
            .unwrap();

        let janitor = WikiJanitor::new(store);
        let report = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert_eq!(report.archived_pages, vec!["concepts/banished.md".to_string()]);

        // Original path no longer exists; archive path does.
        assert!(wiki.read_page("concepts/banished.md").is_err());
        assert!(wiki
            .wiki_dir()
            .join("_archive")
            .join("concepts/banished.md")
            .exists());
    }

    #[test]
    fn snapshot_sync_writes_back_live_trust() {
        let (_tmp, wiki) = setup_wiki();
        wiki.write_page("concepts/sync.md", &make_page("Sync", "Body.", 0.5))
            .unwrap();

        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        store
            .manual_set("concepts/sync.md", "agnes", 0.85, false, None, None)
            .unwrap();

        let janitor = WikiJanitor::new(store);
        let report = janitor.run_once(wiki.wiki_dir(), "agnes");
        assert_eq!(report.snapshot_synced, 1);

        let page = wiki.read_page("concepts/sync.md").unwrap();
        assert!((page.trust - 0.85).abs() < 0.01);
    }
}

