//! Relevance-driven content ranking (#14, 2026-05-12).
//!
//! Today's `build_injection_context` dumps **every** L0+L1 wiki page
//! into the system prompt up to a 6 KB cap — regardless of whether the
//! current user message has anything to do with those pages. For agents
//! with knowledge-rich wikis (the `duduclaw-eng-memory` 404 KB case),
//! that means most of the injected context is irrelevant to *this* turn,
//! burning cache prefix space without helping the model.
//!
//! This module gives callers a cheap, pure-Rust way to rank candidate
//! pages / skills / memory items by their relevance to the current
//! query, so the injection budget can be spent on the top-K matches.
//!
//! ## Design notes
//!
//! - **TF-IDF over character n-grams, no jieba**: keeps the dep
//!   surface small and works for both CJK and ASCII. A bigram model
//!   captures enough word-level signal for relevance ranking without
//!   needing a tokenizer. This is *not* state-of-the-art retrieval —
//!   it's a hot-path-friendly first cut. Phase 2 can swap in a real
//!   embedding service behind the same `rank_by_relevance` interface.
//! - **Pure functions**: no external state, no I/O. The caller passes
//!   candidate items in; we return ranked indices. Easy to test, easy
//!   to call from any prompt builder.
//! - **Determinism**: ties broken by original order. Same input → same
//!   output, every time. Required for cache-stable system prompts.
//!
//! ## Future hook: embedding service
//!
//! If we later wire in `sentence-transformers` (local) or Anthropic
//! embeddings (network), keep `rank_by_relevance` as the public API
//! and swap the implementation. The TF-IDF fallback stays as the
//! offline path.

use std::collections::HashMap;

/// Rank `items` by relevance to `query`. Returns indices in descending
/// relevance order. Items with zero overlap with the query fall to the
/// tail in their original order — they're still listed (so the caller
/// can spend remaining budget) but never preferred over a real match.
///
/// `Item` is whatever the caller wants to rank — wiki path + body,
/// skill name + description, memory snippet. The `text_of` closure
/// extracts the searchable text from each item.
pub fn rank_by_relevance<T, F>(query: &str, items: &[T], text_of: F) -> Vec<usize>
where
    F: Fn(&T) -> &str,
{
    if items.is_empty() {
        return Vec::new();
    }
    if query.trim().is_empty() {
        // No query → preserve original ordering (fast path).
        return (0..items.len()).collect();
    }

    // Build document frequency over bigrams across all items.
    let docs: Vec<Vec<String>> = items.iter().map(|it| bigrams(text_of(it))).collect();
    let mut df: HashMap<String, usize> = HashMap::new();
    for doc in &docs {
        let seen: std::collections::HashSet<&String> = doc.iter().collect();
        for term in seen {
            *df.entry(term.clone()).or_insert(0) += 1;
        }
    }

    let q_terms = bigrams(query);
    let n_docs = items.len() as f64;
    let mut scored: Vec<(usize, f64)> = docs
        .iter()
        .enumerate()
        .map(|(i, doc)| {
            // Count TF for query terms in this doc.
            let mut score = 0.0_f64;
            for q in &q_terms {
                let tf = doc.iter().filter(|t| *t == q).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let df_for_q = *df.get(q).unwrap_or(&1) as f64;
                let idf = (n_docs / df_for_q).ln().max(0.0);
                score += tf * idf;
            }
            (i, score)
        })
        .collect();

    // Sort by score desc, tie-break by original index asc for determinism.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    scored.into_iter().map(|(i, _)| i).collect()
}

/// Take items in `ranked_order` and concatenate their text representations
/// until the byte budget is reached. Returns the included items' indices.
pub fn take_until_budget<T, F>(
    items: &[T],
    ranked_order: &[usize],
    text_of: F,
    max_bytes: usize,
) -> Vec<usize>
where
    F: Fn(&T) -> &str,
{
    let mut used = 0usize;
    let mut out = Vec::new();
    for &idx in ranked_order {
        if idx >= items.len() {
            continue;
        }
        let text = text_of(&items[idx]);
        let needed = text.len() + 4; // separator allowance
        if used + needed > max_bytes {
            break;
        }
        used += needed;
        out.push(idx);
    }
    out
}

/// Compute character bigrams over `text`, lowercased ASCII letters and
/// kept-as-is CJK. Whitespace is treated as a separator (no bigram
/// straddles it). We deliberately don't lemma / segment — bigrams over
/// raw chars are enough to give meaningful relevance signal for CJK
/// (which has no spaces) without a tokenizer dep.
fn bigrams(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text.split_whitespace() {
        let normalized: String = word
            .chars()
            .map(|c| c.to_ascii_lowercase())
            .filter(|c| !c.is_ascii_punctuation())
            .collect();
        let chars: Vec<char> = normalized.chars().collect();
        if chars.len() < 2 {
            // Single-char "words" still informative for CJK — fall back
            // to unigram for these so the model isn't lossy.
            if !chars.is_empty() {
                out.push(chars[0].to_string());
            }
            continue;
        }
        for w in chars.windows(2) {
            out.push(w.iter().collect());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_items_returns_empty() {
        let v: Vec<&str> = vec![];
        let r = rank_by_relevance("anything", &v, |s| *s);
        assert!(r.is_empty());
    }

    #[test]
    fn empty_query_preserves_order() {
        let items = vec!["alpha", "beta", "gamma"];
        let r = rank_by_relevance("", &items, |s| *s);
        assert_eq!(r, vec![0, 1, 2]);
    }

    #[test]
    fn ranks_exact_match_first() {
        let items = vec![
            "totally unrelated content here",
            "memory engine semantic indexing",
            "discord channel webhook",
        ];
        let r = rank_by_relevance("memory engine", &items, |s| *s);
        assert_eq!(r[0], 1, "the memory engine doc must rank first; got {r:?}");
    }

    #[test]
    fn cjk_query_matches_cjk_doc() {
        let items = vec![
            "this doc is about apples and oranges",
            "記憶引擎與向量索引設計",
            "discord channel webhook routing",
        ];
        let r = rank_by_relevance("記憶引擎", &items, |s| *s);
        assert_eq!(r[0], 1, "CJK match must rank first; got {r:?}");
    }

    #[test]
    fn ties_broken_by_original_index() {
        // Both items match equally → original order wins for determinism.
        let items = vec!["foo", "foo"];
        let r = rank_by_relevance("foo", &items, |s| *s);
        assert_eq!(r, vec![0, 1]);
    }

    #[test]
    fn zero_match_items_still_returned_at_tail() {
        let items = vec![
            "memory engine",
            "wholly unrelated cucumber salad",
            "memory consolidation",
        ];
        let r = rank_by_relevance("memory", &items, |s| *s);
        // All 3 returned, the unrelated one at the tail.
        assert_eq!(r.len(), 3);
        assert_eq!(*r.last().unwrap(), 1);
    }

    // ── take_until_budget ──

    #[test]
    fn take_until_budget_respects_cap() {
        let items = vec!["a".repeat(100), "b".repeat(100), "c".repeat(100)];
        let order = vec![0, 1, 2];
        let included = take_until_budget(&items, &order, |s| s.as_str(), 220);
        // ~210 bytes (2 items + sep) fits, 3rd would exceed.
        assert_eq!(included, vec![0, 1]);
    }

    #[test]
    fn take_until_budget_skips_out_of_range_indices() {
        let items = vec!["alpha", "beta"];
        let order = vec![0, 99, 1]; // 99 out of range
        let included = take_until_budget(&items, &order, |s| *s, 10_000);
        assert_eq!(included, vec![0, 1]);
    }

    #[test]
    fn take_until_budget_empty_input_returns_empty() {
        let v: Vec<&str> = vec![];
        let included = take_until_budget(&v, &[], |s| *s, 1_000);
        assert!(included.is_empty());
    }

    // ── End-to-end ranker + budget ──

    #[test]
    fn ranker_plus_budget_picks_relevant_under_constraint() {
        // 4 candidate items; budget fits 2. Ranker should rank the
        // memory-related ones first; budget keeps top 2.
        let items: Vec<String> = vec![
            "discord webhook routing internals".repeat(20),
            "cucumber salad recipe".repeat(20),
            "memory engine semantic search".repeat(20),
            "memory consolidation pipeline".repeat(20),
        ];
        let ranked = rank_by_relevance("memory engine", &items, |s| s.as_str());
        let included = take_until_budget(&items, &ranked, |s| s.as_str(), 1500);
        // Top 2 should be the memory-related entries (idx 2 and 3),
        // possibly in either order depending on tf-idf.
        assert_eq!(included.len(), 2);
        let s: std::collections::HashSet<usize> = included.iter().copied().collect();
        let memory_set: std::collections::HashSet<usize> = [2, 3].into_iter().collect();
        assert_eq!(
            s, memory_set,
            "top-budget items must be the memory-related ones"
        );
    }
}
