//! §1.6 two-level deduplication.
//!
//! Level 1 (literal, always on) is what [`crate::playbook::delta::merge`]
//! actually uses — it must stay pure/deterministic for the merge property
//! test. Level 2 (semantic, embedder-gated) is async I/O (it needs the
//! engine's attached embedder) and lives here as a separate pre-step that
//! `crate::playbook::store::apply_deltas` runs BEFORE calling `merge`,
//! rewriting a near-duplicate `Add` into a `Record{outcome:"dup_add"}`
//! against the matched id — `merge` itself never touches an embedder.

use unicode_normalization::UnicodeNormalization;

use super::entry::PlaybookCategory;

/// Near-duplicate cosine threshold. Deliberately high — a false merge
/// silently loses a distinct rule, which is worse than carrying one
/// redundant entry that the capacity sweep will eventually evict.
pub const NEAR_DUP_COSINE: f32 = 0.92;

/// NFKC → lowercase → strip ASCII + CJK fullwidth punctuation → collapse
/// whitespace runs to a single space → trim. CJK-safe throughout (`char`s,
/// never byte slicing).
pub fn normalize_for_key(s: &str) -> String {
    let lowered: String = s.nfkc().flat_map(|c| c.to_lowercase()).collect();
    let mut out = String::with_capacity(lowered.len());
    let mut last_was_space = false;
    for c in lowered.chars() {
        if c.is_whitespace() {
            if !last_was_space && !out.is_empty() {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        if is_strippable_punct(c) {
            continue;
        }
        out.push(c);
        last_was_space = false;
    }
    out.trim_end().to_string()
}

/// ASCII punctuation, plus the CJK Symbols & Punctuation block (U+3000–303F)
/// and the Halfwidth/Fullwidth Forms block (U+FF00–FFEF, covers fullwidth
/// ASCII punctuation like ！／＂／：).
fn is_strippable_punct(c: char) -> bool {
    if c.is_ascii_punctuation() {
        return true;
    }
    matches!(c as u32, 0x3000..=0x303F | 0xFF00..=0xFFEF) && !c.is_alphanumeric()
}

/// `category` deliberately participates in the key: the same sentence
/// authored as `Repair` (backed by a concrete failure) vs. `Innovate` (no
/// failure behind it) is two different claims and must not shadow each
/// other. First 16 hex chars of `sha256(normalize_for_key(content) ‖ 0x1f ‖
/// category)`.
pub fn dedup_key(content: &str, category: PlaybookCategory) -> String {
    use sha2::{Digest, Sha256};
    let normalized = normalize_for_key(content);
    let mut buf = normalized.into_bytes();
    buf.push(0x1f);
    buf.extend_from_slice(category.as_str().as_bytes());
    let digest = Sha256::digest(&buf);
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Level 2: find the highest-cosine existing candidate above
/// [`NEAR_DUP_COSINE`], restricted to candidates embedded with the SAME
/// `embed_model` (a model swap invalidates cross-model cosine comparisons —
/// §1.6's "embed_model 不符時跳過語意比對" rule). `None` when nothing clears
/// the threshold (including when `candidates` is empty).
pub fn near_duplicate_id(
    content_embedding: &[f32],
    embed_model: &str,
    candidates: &[(String, Option<Vec<f32>>, Option<String>)],
) -> Option<String> {
    candidates
        .iter()
        .filter_map(|(id, emb, model)| {
            let emb = emb.as_ref()?;
            if model.as_deref() != Some(embed_model) {
                return None;
            }
            let sim = duduclaw_memory::embedding::cosine_similarity(content_embedding, emb);
            (sim >= NEAR_DUP_COSINE).then_some((id.clone(), sim))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_punct_and_collapses_whitespace() {
        assert_eq!(
            normalize_for_key("  Hello,   World!!  "),
            "hello world"
        );
    }

    #[test]
    fn normalize_is_cjk_safe_and_strips_fullwidth_punct() {
        // Fullwidth comma／period should be stripped; CJK text kept.
        let out = normalize_for_key("請注意，這是重點。");
        assert!(!out.contains('，'));
        assert!(!out.contains('。'));
        assert!(out.contains('注'));
    }

    #[test]
    fn dedup_key_is_16_hex_chars_and_deterministic() {
        let k1 = dedup_key("hello world", PlaybookCategory::Repair);
        let k2 = dedup_key("Hello,   World!", PlaybookCategory::Repair);
        assert_eq!(k1.len(), 16);
        assert!(k1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(k1, k2, "normalization makes these the same key");
    }

    #[test]
    fn dedup_key_differs_by_category() {
        let k1 = dedup_key("same text", PlaybookCategory::Repair);
        let k2 = dedup_key("same text", PlaybookCategory::Innovate);
        assert_ne!(k1, k2, "category participates in the key");
    }

    #[test]
    fn near_duplicate_id_respects_threshold_and_model() {
        let query = vec![1.0f32, 0.0];
        let candidates = vec![
            ("a".to_string(), Some(vec![1.0, 0.0]), Some("m1".to_string())), // cos=1.0
            ("b".to_string(), Some(vec![0.0, 1.0]), Some("m1".to_string())), // cos=0.0
            ("c".to_string(), Some(vec![1.0, 0.0]), Some("m2".to_string())), // wrong model
            ("d".to_string(), None, Some("m1".to_string())),                // no embedding
        ];
        assert_eq!(near_duplicate_id(&query, "m1", &candidates), Some("a".to_string()));
        assert_eq!(near_duplicate_id(&query, "m3", &candidates), None);
        assert_eq!(near_duplicate_id(&query, "m1", &[]), None);
    }
}
