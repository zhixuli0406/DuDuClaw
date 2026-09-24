//! Keyword rule — a literal term list an operator maintains without writing
//! regex. This is the WP2 "自訂關鍵字快速通道": add a customer name like
//! `Amazon` or `台積電` and have it redacted, no pattern syntax required.
//!
//! Matching is whole-word and CJK-safe:
//! - ASCII-alphanumeric-edged keywords (e.g. `Amazon`) require a non-word
//!   character (or string edge) on each side, so `Amazon` does not fire inside
//!   `Amazonian`.
//! - CJK / symbol-edged keywords (e.g. `台積電`) match as substrings, because
//!   Chinese has no inter-word whitespace and a boundary check would never fire.

use crate::error::{RedactionError, Result};
use crate::rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};

/// Compiled keyword rule.
#[derive(Debug)]
pub struct KeywordRule {
    spec: RuleSpec,
    /// Keywords, pre-lowercased when the rule is case-insensitive so the hot
    /// path avoids re-allocating per scan.
    needles: Vec<String>,
    case_sensitive: bool,
}

impl KeywordRule {
    /// Compile `spec` into a runtime [`KeywordRule`]. Errors if the spec kind
    /// is not `Keyword` or the value list is empty (an empty list is a config
    /// mistake, not a match-nothing rule).
    pub fn compile(spec: RuleSpec) -> Result<Self> {
        let (values, case_sensitive) = match &spec.kind {
            RuleKind::Keyword {
                values,
                case_sensitive,
            } => (values.clone(), *case_sensitive),
            other => {
                return Err(RedactionError::rule_compile(
                    &spec.id,
                    format!("expected Keyword kind, got {other:?}"),
                ));
            }
        };
        let cleaned: Vec<String> = values
            .into_iter()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        if cleaned.is_empty() {
            return Err(RedactionError::rule_compile(
                &spec.id,
                "keyword rule has no non-empty values".to_string(),
            ));
        }
        let needles = if case_sensitive {
            cleaned
        } else {
            // Same fold as the haystack, so both sides of the comparison agree
            // on the rare chars whose lowercase changes byte length (a needle
            // folded with `to_lowercase` could otherwise never match).
            cleaned
                .iter()
                .map(|v| lowercase_preserving_byte_offsets(v))
                .collect()
        };
        Ok(KeywordRule {
            spec,
            needles,
            case_sensitive,
        })
    }
}

/// Should this keyword use ASCII word-boundary matching? True only when both
/// its first and last chars are ASCII alphanumeric — the case where a naive
/// substring match would over-fire (`hi` inside `this`). CJK terms return
/// false and match as substrings.
fn use_word_boundary(needle: &str) -> bool {
    let first = needle.chars().next();
    let last = needle.chars().last();
    matches!((first, last), (Some(a), Some(b)) if a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric())
}

/// Is the byte at `idx` (or the edge) a word character for boundary purposes?
fn is_word_byte(bytes: &[u8], idx: usize) -> bool {
    bytes
        .get(idx)
        .map(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .unwrap_or(false)
}

/// Lowercase `text` **without moving any byte offset**.
///
/// `str::to_lowercase` is not length-preserving: `\u{130}` (Turkish dotted
/// capital I, 2 bytes) lowercases to `i` + a combining dot (3 bytes), and
/// `\u{212A}` (Kelvin sign, 3 bytes) lowercases to `k` (1 byte). Every offset
/// after such a char drifts, so a match found in the lowercased haystack and
/// then sliced out of the ORIGINAL `text` can land mid-char and panic — on
/// attacker-influenced tool-result text, inside the redaction path (CLAUDE.md
/// coding convention 1).
///
/// A char is therefore lowered only when its lowercase form is a *single* char
/// of the *same* UTF-8 byte length; otherwise the original char is kept.
/// Requiring a single char (not merely an equal total length) is what
/// guarantees no new char boundary appears inside what used to be one char, so
/// every boundary in the result is a boundary in `text` and vice versa.
///
/// Consequence, deliberate and rare: the handful of chars whose lowercase
/// changes length are matched case-sensitively. No CJK or ASCII char is
/// affected — the coverage this matcher actually needs.
pub(crate) fn lowercase_preserving_byte_offsets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let mut lowered = ch.to_lowercase();
        match (lowered.next(), lowered.next()) {
            (Some(lc), None) if lc.len_utf8() == ch.len_utf8() => out.push(lc),
            _ => out.push(ch),
        }
    }
    out
}

/// Literal-term scan shared by [`KeywordRule`] and
/// [`crate::rules::identity::IdentityRule`] — the two rule kinds whose matcher
/// is "a list of terms an operator (or the identity directory) supplies",
/// with the same ASCII-whole-word / CJK-substring semantics.
///
/// `needles` MUST already be normalised for `case_sensitive`: when it is
/// `false` the caller folds each needle once at compile time with
/// [`lowercase_preserving_byte_offsets`] (the same fold applied to the
/// haystack here), so the hot path only folds the haystack.
pub(crate) fn match_needles(text: &str, needles: &[String], case_sensitive: bool) -> Vec<Match> {
    let mut matches = Vec::new();
    // For case-insensitive scans, lowercase once — with a fold that cannot
    // move a byte offset, so `text[start..end]` below is always sliced on char
    // boundaries. See [`lowercase_preserving_byte_offsets`].
    let hay = if case_sensitive {
        text.to_string()
    } else {
        lowercase_preserving_byte_offsets(text)
    };
    let hay_bytes = hay.as_bytes();

    for needle in needles {
        let boundary = use_word_boundary(needle);
        let nlen = needle.len();
        let mut from = 0usize;
        while let Some(rel) = hay[from..].find(needle.as_str()) {
            let start = from + rel;
            let end = start + nlen;
            let ok = if boundary {
                !is_word_byte(hay_bytes, start.wrapping_sub(1)) && !is_word_byte(hay_bytes, end)
            } else {
                true
            };
            if ok {
                // Return the ORIGINAL-cased slice from `text`, not `hay`.
                matches.push(Match {
                    start,
                    end,
                    original: text[start..end].to_string(),
                });
            }
            from = end.max(start + 1);
        }
    }
    matches
}

impl Rule for KeywordRule {
    fn id(&self) -> &str {
        &self.spec.id
    }
    fn category(&self) -> &str {
        &self.spec.category
    }
    fn restore_scope(&self) -> &RestoreScope {
        &self.spec.restore_scope
    }
    fn priority(&self) -> i32 {
        self.spec.priority
    }
    fn cross_session_stable(&self) -> bool {
        self.spec.cross_session_stable
    }
    fn apply_to_system_prompt(&self) -> bool {
        self.spec.apply_to_system_prompt
    }

    fn match_text(&self, text: &str) -> Vec<Match> {
        match_needles(text, &self.needles, self.case_sensitive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(values: &[&str], case_sensitive: bool) -> RuleSpec {
        RuleSpec {
            id: "kw".into(),
            category: "CUSTOMER".into(),
            restore_scope: RestoreScope::default(),
            priority: 60,
            cross_session_stable: true,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Keyword {
                values: values.iter().map(|s| s.to_string()).collect(),
                case_sensitive,
            },
        }
    }

    #[test]
    fn ascii_keyword_is_whole_word() {
        let rule = KeywordRule::compile(spec(&["Amazon"], false)).unwrap();
        // Fires on the standalone word...
        let m = rule.match_text("訂單來自 Amazon 的客戶");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].original, "Amazon");
        // ...but not embedded in a longer word.
        assert!(rule.match_text("Amazonian tribes").is_empty());
    }

    #[test]
    fn cjk_keyword_matches_substring() {
        let rule = KeywordRule::compile(spec(&["台積電"], false)).unwrap();
        let m = rule.match_text("這是台積電的訂單");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].original, "台積電");
    }

    #[test]
    fn case_insensitive_preserves_original_case() {
        let rule = KeywordRule::compile(spec(&["amazon"], false)).unwrap();
        let m = rule.match_text("From AMAZON today");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].original, "AMAZON");
    }

    #[test]
    fn empty_values_rejected() {
        assert!(KeywordRule::compile(spec(&[], false)).is_err());
        assert!(KeywordRule::compile(spec(&["   "], false)).is_err());
    }

    #[test]
    fn length_changing_uppercase_does_not_desync_offsets() {
        // U+0130 'İ' lowercases to "i" + U+0307 (2 bytes → 3). With a naive
        // `to_lowercase()` haystack every offset after it drifts by one byte,
        // and slicing the original text at those offsets lands mid-char and
        // panics. The needle must still be found, at the RIGHT offsets.
        let rule = KeywordRule::compile(spec(&["amazon"], false)).unwrap();
        let text = "İstanbul 與 Amazon 開會";
        let m = rule.match_text(text);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].original, "Amazon");
        // The span must address the same bytes in the ORIGINAL text.
        assert_eq!(&text[m[0].start..m[0].end], "Amazon");
    }

    #[test]
    fn length_changing_char_immediately_before_a_cjk_needle() {
        // Worst case: no separator between the length-changing char and the
        // needle, so a one-byte drift would slice into the middle of 台.
        let rule = KeywordRule::compile(spec(&["台積電"], false)).unwrap();
        let text = "İ台積電";
        let m = rule.match_text(text);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].original, "台積電");
        assert_eq!(&text[m[0].start..m[0].end], "台積電");
    }

    #[test]
    fn case_fold_preserves_byte_length_for_every_char() {
        // The invariant the whole offset scheme rests on.
        for text in [
            "İstanbul",
            "\u{212A}elvin",          // KELVIN SIGN → 'k' (3 bytes → 1)
            "Ω Σ ß É ç",
            "台積電 Amazon 09123",
            "",
        ] {
            let folded = lowercase_preserving_byte_offsets(text);
            assert_eq!(
                folded.len(),
                text.len(),
                "byte length changed for {text:?} → {folded:?}"
            );
            // Every char boundary in the fold is a char boundary in the source.
            for (idx, _) in folded.char_indices() {
                assert!(
                    text.is_char_boundary(idx),
                    "offset {idx} is not a char boundary in {text:?}"
                );
            }
        }
        // Ordinary chars still fold.
        assert_eq!(lowercase_preserving_byte_offsets("AMAZON"), "amazon");
        // Length-changing ones are left alone (matched case-sensitively).
        assert_eq!(lowercase_preserving_byte_offsets("İ"), "İ");
    }

    #[test]
    fn multiple_occurrences_all_found() {
        let rule = KeywordRule::compile(spec(&["台積電"], false)).unwrap();
        let m = rule.match_text("台積電與台積電");
        assert_eq!(m.len(), 2);
    }
}
