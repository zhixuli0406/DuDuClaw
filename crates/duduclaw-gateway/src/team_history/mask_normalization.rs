//! Preserve rules written for the original text across NFKC. A whole
//! grapheme is withheld when a match touches any part of it. We verify the
//! per-grapheme normalization against the whole-string result; uncertain
//! boundaries produce one fully masked segment, never an estimated offset.
use super::{SensitiveSpan, ViewMasker, category, cover_union, identity_key};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn preserve_original_hits(
    masker: &ViewMasker,
    original: &str,
    canonical: &str,
    mut spans: Vec<SensitiveSpan>,
    salt: &[u8; 32],
) -> Vec<SensitiveSpan> {
    let hits = masker.matches(original);
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|h| h.span.start < h.span.end)
        .collect();
    if hits.is_empty() {
        return spans;
    }
    let fallback = || {
        vec![SensitiveSpan {
            start: 0,
            end: canonical.len(),
            cat: "REDACTED".into(),
            key: identity_key("REDACTED", canonical, salt),
            priority: i32::MAX,
        }]
    };
    let mut points = Vec::with_capacity(hits.len() * 2);
    for (i, hit) in hits.iter().enumerate() {
        if !original.is_char_boundary(hit.span.start) || !original.is_char_boundary(hit.span.end) {
            return fallback();
        }
        points.push((hit.span.start, true, i));
        points.push((hit.span.end, false, i));
    }
    points.sort_unstable_by_key(|p| p.0);
    let mut starts = vec![0; hits.len()];
    let mut ends = vec![0; hits.len()];
    let mut point = 0;
    let mut normalized_at = 0;
    for (original_at, grapheme) in original.grapheme_indices(true) {
        let canonical_piece = if grapheme.is_ascii() {
            std::borrow::Cow::Borrowed(grapheme)
        } else {
            std::borrow::Cow::Owned(grapheme.nfkc().collect::<String>())
        };
        let normalized_end = normalized_at + canonical_piece.len();
        if canonical.get(normalized_at..normalized_end) != Some(canonical_piece.as_ref()) {
            return fallback();
        }
        let original_end = original_at + grapheme.len();
        while point < points.len() && points[point].0 < original_end {
            let (byte, start, i) = points[point];
            if byte < original_at {
                return fallback();
            }
            if start {
                starts[i] = normalized_at;
            } else {
                ends[i] = if byte == original_at {
                    normalized_at
                } else {
                    normalized_end
                };
            }
            point += 1;
        }
        normalized_at = normalized_end;
    }
    if normalized_at != canonical.len() {
        return fallback();
    }
    while point < points.len() {
        let (byte, start, i) = points[point];
        if byte != original.len() {
            return fallback();
        }
        if start {
            starts[i] = canonical.len()
        } else {
            ends[i] = canonical.len()
        }
        point += 1;
    }
    for (i, hit) in hits.into_iter().enumerate() {
        if starts[i] < ends[i] {
            let cat = category(hit.rule.category()).to_owned();
            spans.push(SensitiveSpan {
                start: starts[i],
                end: ends[i],
                key: identity_key(&cat, &original[hit.span.start..hit.span.end], salt),
                cat,
                priority: hit.rule.priority(),
            });
        }
    }
    cover_union(spans)
}
