//! Query-aware wiki injection (#14 glue, 2026-05-12).
//!
//! Bridges `crate::relevance_ranker` (#14 pure ranking policy) with
//! `duduclaw_memory::WikiStore` (the actual page store). The original
//! `WikiStore::build_injection_context*` methods dump pages in file
//! order until a byte cap; this helper instead **ranks** L0+L1 pages
//! against the current user query and keeps the most relevant ones up
//! to the same cap.
//!
//! Lives in the gateway (not memory) crate so the relevance scoring
//! policy can iterate independently of the storage layer.
//!
//! ## Session-stable selection (cache-friendliness)
//!
//! Ranking by the *per-turn* user query means the kept-page set — and
//! therefore the system prompt bytes — changed every turn, invalidating
//! the prompt-cache prefix on both the CLI and Direct API paths. With a
//! `cache_key` (agent + session), the page *selection* is computed once
//! per session (15-min TTL) and reused; citations are still recorded
//! every turn so the feedback loop keeps attributing outcomes to pages.
//!
//! ## Knowledge ownership (`.scope.toml`)
//!
//! A `.scope.toml` at the wiki root may declare, per top-level namespace,
//! `knowledge_owner = "memory"` — meaning the memory system (temporal
//! facts with supersession) is the source of truth for that topic and
//! wiki pages under it are *excluded from prompt injection* (they stay
//! searchable via wiki tools). Default / absent → wiki-owned, injected
//! as before. Same file and table shape as the RFC-21 §3 write policy
//! (`[namespaces."x"]`), so operators manage one file.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use duduclaw_memory::feedback::{CitationTracker, WikiCitation};
use duduclaw_memory::{WikiLayer, WikiStore};

/// Optional citation recording context. When `Some`, kept-pages are
/// recorded in the citation log so the feedback bus can later attribute
/// outcomes back to specific pages.
pub struct CitationContext<'a> {
    pub agent_id: &'a str,
    pub conversation_id: &'a str,
    pub session_id: Option<&'a str>,
    pub tracker: &'a CitationTracker,
}

/// How long a session's kept-page selection stays pinned before being
/// re-ranked. Within the window the system prompt's wiki section is
/// byte-stable → the cached prefix survives across turns.
const SESSION_SELECTION_TTL: Duration = Duration::from_secs(900);
/// Upper bound on cached sessions; expired entries are evicted on access.
const SESSION_CACHE_CAP: usize = 256;

/// Build the wiki injection context with relevance-aware page selection.
///
/// `query` should be the current user message (or a digest of it).
/// When `query` is empty or whitespace, behaviour falls back to file
/// order — identical to `WikiStore::build_injection_context` so this is
/// a safe drop-in.
///
/// `cache_key` — when `Some` (e.g. `"{agent_id}:{session_id}"`), the page
/// selection is served from the session cache (see module docs). `None`
/// ranks fresh every call (previous behaviour).
///
/// Returns the rendered injection text. On any store-side error we log
/// and return an empty string — wiki injection is a best-effort signal,
/// never fatal.
/// `viewer_department` (WP7) — the department of the agent this prompt is for.
/// Pages under the built-in `departments/<dept>/` namespace are kept only when
/// `<dept>` matches; a `None` viewer (no department) sees no department page.
/// Company-layer pages are always eligible. Department is a stable property of
/// the agent, so it does not perturb the session-stable selection cache
/// (the cache key already scopes per agent).
pub fn ranked_wiki_injection(
    store: &WikiStore,
    query: &str,
    max_chars: usize,
    citation: Option<CitationContext<'_>>,
    cache_key: Option<&str>,
    viewer_department: Option<&str>,
) -> String {
    if let Some(key) = cache_key {
        if let Some(pages) = cached_selection(key) {
            return render_and_cite(&pages, citation);
        }
        let pages = select_pages(store, query, max_chars, viewer_department);
        store_selection(key, pages.clone());
        return render_and_cite(&pages, citation);
    }
    let pages = select_pages(store, query, max_chars, viewer_department);
    render_and_cite(&pages, citation)
}

/// One page with the metadata needed for ranking + citation recording.
#[derive(Debug, Clone)]
struct RankedPage {
    layer: WikiLayer,
    path: String,
    body: String,
    trust: f32,
}

/// Collect eligible pages, apply knowledge-ownership filtering, rank by
/// relevance, and keep the top pages within `max_chars`.
fn select_pages(
    store: &WikiStore,
    query: &str,
    max_chars: usize,
    viewer_department: Option<&str>,
) -> Vec<RankedPage> {
    // Gather all candidate pages first. Identity and Core layers are
    // both eligible; L2/L3 stay search-only as the wiki contract states.
    let memory_owned = load_memory_owned_namespaces(store.wiki_dir());
    let mut candidates: Vec<RankedPage> = Vec::new();
    for layer in [WikiLayer::Identity, WikiLayer::Core] {
        match store.collect_by_layer_with_meta(layer) {
            Ok(pages) => {
                for (path, body, trust) in pages {
                    if is_memory_owned(&path, &memory_owned) {
                        continue; // memory system is SoT for this namespace
                    }
                    // WP7: never inject another department's page (or any
                    // department page for a no-department viewer). Company
                    // pages always pass.
                    if !duduclaw_core::department_page_visible(&path, viewer_department) {
                        continue;
                    }
                    candidates.push(RankedPage {
                        layer,
                        path,
                        body,
                        trust,
                    });
                }
            }
            Err(e) => {
                tracing::warn!(?layer, error = %e, "wiki collect_by_layer_with_meta failed");
            }
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    // Rank by relevance to the user query (TF-IDF over bigrams; empty
    // query → original order preserved).
    let ranking =
        crate::relevance_ranker::rank_by_relevance(query, &candidates, |c| c.body.as_str());

    // Keep in ranked order within the byte budget. Oversize pages are
    // skipped, not aborting — a smaller lower-rank page might still fit
    // (matches the existing wiki rendering contract).
    let mut kept: Vec<RankedPage> = Vec::new();
    let mut remaining = max_chars;
    for &idx in &ranking {
        let Some(page) = candidates.get(idx) else { continue };
        let needed = page.body.len() + 2; // +2 newline pair
        if needed > remaining {
            continue;
        }
        remaining -= needed;
        kept.push(page.clone());
    }
    kept
}

/// Render kept pages grouped by layer and record citations for each.
///
/// Rendering is deterministic from `pages`, so cache hits re-render the
/// identical bytes while still recording this turn's citations.
fn render_and_cite(pages: &[RankedPage], citation: Option<CitationContext<'_>>) -> String {
    let kept_identity: Vec<&RankedPage> =
        pages.iter().filter(|p| p.layer == WikiLayer::Identity).collect();
    let kept_core: Vec<&RankedPage> =
        pages.iter().filter(|p| p.layer == WikiLayer::Core).collect();

    let mut output = String::new();
    if !kept_identity.is_empty() {
        output.push_str("### Wiki — Identity\n\n");
        for p in &kept_identity {
            output.push_str(&p.body);
            output.push_str("\n\n");
        }
    }
    if !kept_core.is_empty() {
        output.push_str("### Wiki — Core\n\n");
        for p in &kept_core {
            output.push_str(&p.body);
            output.push_str("\n\n");
        }
    }

    // Citation recording — only for kept pages.
    if let Some(ctx) = citation {
        let now = chrono::Utc::now();
        let citations: Vec<WikiCitation> = kept_identity
            .iter()
            .chain(kept_core.iter())
            .map(|p| {
                let st = duduclaw_memory::wiki::derive_source_type(&p.path, p.trust);
                WikiCitation {
                    page_path: p.path.clone(),
                    agent_id: ctx.agent_id.to_string(),
                    conversation_id: ctx.conversation_id.to_string(),
                    retrieved_at: now,
                    trust_at_cite: p.trust,
                    source_type: st,
                    session_id: ctx.session_id.map(|s| s.to_string()),
                }
            })
            .collect();
        if !citations.is_empty() {
            ctx.tracker.record_many(citations);
        }
    }

    output
}

// ── Knowledge ownership (.scope.toml) ────────────────────────────────

/// Read the set of top-level namespaces whose `knowledge_owner` is
/// `"memory"` from `<wiki_root>/.scope.toml`. Absent or malformed file →
/// empty set (fail-safe: wiki-owned, inject as before).
fn load_memory_owned_namespaces(wiki_root: &Path) -> HashSet<String> {
    let path = wiki_root.join(".scope.toml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return HashSet::new();
    };
    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, ".scope.toml malformed — ignoring knowledge_owner");
            return HashSet::new();
        }
    };
    let Some(namespaces) = table.get("namespaces").and_then(|v| v.as_table()) else {
        return HashSet::new();
    };
    namespaces
        .iter()
        .filter(|(_, v)| {
            v.get("knowledge_owner").and_then(|o| o.as_str()) == Some("memory")
        })
        .map(|(ns, _)| ns.clone())
        .collect()
}

/// A page belongs to a memory-owned namespace when its first path
/// segment matches. Top-level files (no `/`) are never filtered.
fn is_memory_owned(page_path: &str, memory_owned: &HashSet<String>) -> bool {
    if memory_owned.is_empty() {
        return false;
    }
    match page_path.split('/').next() {
        Some(ns) if ns != page_path => memory_owned.contains(ns),
        _ => false,
    }
}

// ── Session-stable selection cache ───────────────────────────────────

type SelectionCache = std::collections::HashMap<String, (Instant, Vec<RankedPage>)>;

static SESSION_SELECTIONS: std::sync::OnceLock<std::sync::Mutex<SelectionCache>> =
    std::sync::OnceLock::new();

fn cached_selection(key: &str) -> Option<Vec<RankedPage>> {
    let cache = SESSION_SELECTIONS.get_or_init(Default::default);
    let guard = cache.lock().ok()?;
    guard
        .get(key)
        .filter(|(at, _)| at.elapsed() < SESSION_SELECTION_TTL)
        .map(|(_, pages)| pages.clone())
}

fn store_selection(key: &str, pages: Vec<RankedPage>) {
    let cache = SESSION_SELECTIONS.get_or_init(Default::default);
    let Ok(mut guard) = cache.lock() else { return };
    if guard.len() >= SESSION_CACHE_CAP {
        guard.retain(|_, (at, _)| at.elapsed() < SESSION_SELECTION_TTL);
        if guard.len() >= SESSION_CACHE_CAP {
            // Still full of live sessions — drop the oldest entry.
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
            }
        }
    }
    guard.insert(key.to_string(), (Instant::now(), pages));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_owned_namespace_matching() {
        let owned: HashSet<String> = ["people".to_string()].into_iter().collect();
        assert!(is_memory_owned("people/alice.md", &owned));
        assert!(!is_memory_owned("policies/security.md", &owned));
        // Top-level files are never namespace-filtered.
        assert!(!is_memory_owned("faq.md", &owned));
        assert!(!is_memory_owned("people.md", &owned));
    }

    #[test]
    fn scope_toml_parses_knowledge_owner() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".scope.toml"),
            r#"
[namespaces."people"]
mode = "agent_writable"
knowledge_owner = "memory"

[namespaces."policies"]
mode = "operator_only"
knowledge_owner = "wiki"

[namespaces."sop"]
mode = "agent_writable"
"#,
        )
        .unwrap();
        let owned = load_memory_owned_namespaces(dir.path());
        assert_eq!(owned.len(), 1);
        assert!(owned.contains("people"));
    }

    #[test]
    fn scope_toml_absent_or_malformed_is_failsafe_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_memory_owned_namespaces(dir.path()).is_empty());
        std::fs::write(dir.path().join(".scope.toml"), "not [ valid toml").unwrap();
        assert!(load_memory_owned_namespaces(dir.path()).is_empty());
    }

    #[test]
    fn session_cache_pins_selection_across_queries() {
        let pages = vec![RankedPage {
            layer: WikiLayer::Core,
            path: "sop/deploy.md".to_string(),
            body: "deploy steps".to_string(),
            trust: 0.8,
        }];
        store_selection("agent-x:sess-1", pages.clone());
        let hit = cached_selection("agent-x:sess-1").expect("cache hit");
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].path, "sop/deploy.md");
        assert!(cached_selection("agent-x:sess-2").is_none());
    }
}
