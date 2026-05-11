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

/// Build the wiki injection context with relevance-aware page selection.
///
/// `query` should be the current user message (or a digest of it).
/// When `query` is empty or whitespace, behaviour falls back to file
/// order — identical to `WikiStore::build_injection_context` so this is
/// a safe drop-in.
///
/// Returns the rendered injection text. On any store-side error we log
/// and return an empty string — wiki injection is a best-effort signal,
/// never fatal.
pub fn ranked_wiki_injection(
    store: &WikiStore,
    query: &str,
    max_chars: usize,
    citation: Option<CitationContext<'_>>,
) -> String {
    // Gather all candidate pages first. Identity and Core layers are
    // both eligible; L2/L3 stay search-only as the wiki contract states.
    let mut candidates: Vec<RankedPage> = Vec::new();
    for layer in [WikiLayer::Identity, WikiLayer::Core] {
        match store.collect_by_layer_with_meta(layer) {
            Ok(pages) => {
                for (path, body, trust) in pages {
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
        return String::new();
    }

    // Rank by relevance to the user query (TF-IDF over bigrams; empty
    // query → original order preserved).
    let ranking =
        crate::relevance_ranker::rank_by_relevance(query, &candidates, |c| c.body.as_str());

    render_within_budget(&candidates, &ranking, max_chars, citation)
}

/// One page with the metadata needed for ranking + citation recording.
#[derive(Debug, Clone)]
struct RankedPage {
    layer: WikiLayer,
    path: String,
    body: String,
    trust: f32,
}

/// Render the ranked pages into a single string, respecting `max_chars`.
///
/// Groups output by layer with a header (so the existing prompt shape
/// stays familiar). Within a layer, the kept pages appear in their
/// ranked order. Citations are recorded for every kept page when a
/// `CitationContext` is supplied.
fn render_within_budget(
    candidates: &[RankedPage],
    ranking: &[usize],
    max_chars: usize,
    citation: Option<CitationContext<'_>>,
) -> String {
    let mut output = String::new();
    let mut remaining = max_chars;

    // First pass: pick keepers in ranked order; remember insertion order
    // by layer so we can render layer-grouped output.
    let mut kept_identity: Vec<&RankedPage> = Vec::new();
    let mut kept_core: Vec<&RankedPage> = Vec::new();
    for &idx in ranking {
        let Some(page) = candidates.get(idx) else { continue; };
        let needed = page.body.len() + 2; // +2 newline pair
        if needed > remaining {
            // Skip oversize pages but don't abort — a smaller lower-rank
            // page might still fit (matches the existing wiki rendering
            // contract).
            continue;
        }
        remaining -= needed;
        match page.layer {
            WikiLayer::Identity => kept_identity.push(page),
            WikiLayer::Core => kept_core.push(page),
            _ => {}
        }
    }

    // Second pass: render. Layer headers cost characters; subtract from
    // each layer that has at least one kept page. (We've already
    // checked total body bytes fit; the small header overhead is fine
    // to over-spend by a few bytes since callers treat max_chars as
    // a soft target.)
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
