//! Regex-based rule — the simplest matcher, covers structured PII
//! (emails, phone numbers, national IDs, credit cards, API keys).
//!
//! Patterns are length-capped (4096 chars) to reduce ReDoS surface;
//! operator-supplied free-form patterns should still be vetted by review.

use regex::Regex;

use crate::error::{RedactionError, Result};
use crate::rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};

/// Maximum allowed pattern length. Beyond this we refuse to compile.
pub const MAX_PATTERN_LEN: usize = 4096;

/// Compiled regex rule.
#[derive(Debug)]
pub struct RegexRule {
    spec: RuleSpec,
    compiled: Regex,
}

impl RegexRule {
    /// Compile `spec` into a runtime [`RegexRule`].
    ///
    /// Returns [`RedactionError::RuleCompile`] if the spec's kind is not
    /// `Regex`, the pattern is too long, or the regex crate rejects it.
    pub fn compile(spec: RuleSpec) -> Result<Self> {
        let pattern = match &spec.kind {
            RuleKind::Regex { pattern } => pattern.clone(),
            other => {
                return Err(RedactionError::rule_compile(
                    &spec.id,
                    format!("expected Regex kind, got {other:?}"),
                ));
            }
        };

        if pattern.len() > MAX_PATTERN_LEN {
            return Err(RedactionError::rule_compile(
                &spec.id,
                format!("pattern too long ({} > {MAX_PATTERN_LEN})", pattern.len()),
            ));
        }

        let compiled = Regex::new(&pattern)
            .map_err(|e| RedactionError::rule_compile(&spec.id, e.to_string()))?;

        Ok(RegexRule { spec, compiled })
    }
}

impl Rule for RegexRule {
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
        self.compiled
            .find_iter(text)
            .map(|m| Match {
                start: m.start(),
                end: m.end(),
                original: m.as_str().to_string(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email_spec() -> RuleSpec {
        RuleSpec {
            id: "email".into(),
            category: "EMAIL".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex {
                pattern: r"[\w.+-]+@[\w-]+\.[\w.-]+".into(),
            },
        }
    }

    #[test]
    fn compile_then_match_email() {
        let rule = RegexRule::compile(email_spec()).unwrap();
        let hits = rule.match_text("ping alice@acme.com and bob@example.org plz");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].original, "alice@acme.com");
        assert_eq!(hits[1].original, "bob@example.org");
    }

    #[test]
    fn empty_text_yields_no_matches() {
        let rule = RegexRule::compile(email_spec()).unwrap();
        assert!(rule.match_text("").is_empty());
    }

    #[test]
    fn invalid_regex_rejected() {
        let bad = RuleSpec {
            id: "bad".into(),
            category: "X".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex { pattern: "(unclosed".into() },
        };
        let err = RegexRule::compile(bad).unwrap_err();
        assert!(matches!(err, RedactionError::RuleCompile { .. }));
    }

    #[test]
    fn oversized_pattern_rejected() {
        let huge = "a".repeat(MAX_PATTERN_LEN + 1);
        let bad = RuleSpec {
            id: "huge".into(),
            category: "X".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex { pattern: huge },
        };
        assert!(RegexRule::compile(bad).is_err());
    }

    #[test]
    fn rule_metadata_passthrough() {
        let rule = RegexRule::compile(email_spec()).unwrap();
        assert_eq!(rule.id(), "email");
        assert_eq!(rule.category(), "EMAIL");
        assert_eq!(rule.priority(), 50);
        assert!(!rule.cross_session_stable());
    }

    #[test]
    fn taiwan_id_pattern() {
        let spec = RuleSpec {
            id: "tw_id".into(),
            category: "TW_ID".into(),
            restore_scope: RestoreScope::Owner,
            priority: 100,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex { pattern: r"[A-Z][12]\d{8}".into() },
        };
        let rule = RegexRule::compile(spec).unwrap();
        assert_eq!(rule.match_text("A123456789").len(), 1);
        // Z987654321 doesn't match (second char must be 1 or 2). B299999999 does.
        assert_eq!(rule.match_text("Z987654321 plus B299999999").len(), 1);
        assert_eq!(rule.match_text("A123456789 and B299999999").len(), 2);
        assert!(rule.match_text("no id here").is_empty());
    }
}
