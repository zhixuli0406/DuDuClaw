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
            cleaned.iter().map(|v| v.to_lowercase()).collect()
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
        let mut matches = Vec::new();
        // For case-insensitive scans, lowercase once. Byte offsets in the
        // lowercased haystack line up with the original for ASCII and for CJK
        // (whose casing is identity), which is the coverage we need.
        let hay = if self.case_sensitive {
            text.to_string()
        } else {
            text.to_lowercase()
        };
        let hay_bytes = hay.as_bytes();

        for needle in &self.needles {
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
    fn multiple_occurrences_all_found() {
        let rule = KeywordRule::compile(spec(&["台積電"], false)).unwrap();
        let m = rule.match_text("台積電與台積電");
        assert_eq!(m.len(), 2);
    }
}
