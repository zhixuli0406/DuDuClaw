//! Rule set + overlap resolution.
//!
//! The engine owns a flat `Vec<Arc<dyn Rule>>`. When `apply()` is called
//! it asks every rule for matches, then resolves overlaps with a stable
//! deterministic algorithm:
//!
//! 1. Sort all matches by `(priority desc, start asc, rule_id asc)`.
//! 2. Walk the sorted list keeping a "covered" interval set.
//! 3. Drop any match whose span overlaps an already-kept span.
//!
//! This is intentionally simple — no nesting, no partial-overlap merging.
//! Profile authors are expected to give overlapping rules sensible
//! priorities (e.g. `tw_national_id` priority 100 wins over a more general
//! `digits_8_to_12` priority 50).

use std::sync::Arc;

use crate::error::Result;
use crate::rules::{Match, Rule, RuleKind, RuleSpec};
use crate::rules::regex::RegexRule;
use crate::source::Source;

/// One winning match plus the rule that produced it.
#[derive(Debug, Clone)]
pub struct MatchedSpan {
    pub rule: Arc<dyn Rule>,
    pub span: Match,
}

/// A collection of compiled rules with `apply()` that returns resolved
/// matches in left-to-right order.
pub struct RuleEngine {
    rules: Vec<Arc<dyn Rule>>,
}

impl RuleEngine {
    /// Compile a vector of [`RuleSpec`] into a runtime engine.
    ///
    /// Rule types not yet implemented (Identity / Keyword / JsonPath) are
    /// silently skipped with a `tracing::warn` — this is intentional so a
    /// profile that ships with future rule types degrades gracefully.
    pub fn from_specs(specs: Vec<RuleSpec>) -> Result<Self> {
        let mut rules: Vec<Arc<dyn Rule>> = Vec::new();
        for spec in specs {
            match &spec.kind {
                RuleKind::Regex { .. } => {
                    let rule = RegexRule::compile(spec)?;
                    rules.push(Arc::new(rule));
                }
                other => {
                    tracing::warn!(
                        target: "duduclaw_redaction::engine",
                        rule_id = %spec.id,
                        kind = ?other,
                        "rule kind not yet implemented in this build — skipping"
                    );
                }
            }
        }
        Ok(RuleEngine { rules })
    }

    /// Number of compiled rules.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Apply all rules to `text` and return the resolved matches in
    /// left-to-right order.
    ///
    /// Pass a `source` so rules can opt out of certain sources (e.g.
    /// only system-prompt-aware rules fire on a `SystemPrompt` source
    /// with `selective` policy).
    pub fn apply(&self, text: &str, source: &Source) -> Vec<MatchedSpan> {
        let only_system_prompt_rules = matches!(source, Source::SystemPrompt { .. });

        let mut all: Vec<MatchedSpan> = Vec::new();
        for rule in &self.rules {
            if only_system_prompt_rules && !rule.apply_to_system_prompt() {
                continue;
            }
            for span in rule.match_text(text) {
                all.push(MatchedSpan {
                    rule: rule.clone(),
                    span,
                });
            }
        }

        resolve_overlaps(all)
    }
}

/// Pure helper exposed for unit testing. Given an arbitrary list of
/// matched spans (possibly overlapping), return the kept subset sorted
/// by `start` ascending.
pub(crate) fn resolve_overlaps(mut spans: Vec<MatchedSpan>) -> Vec<MatchedSpan> {
    // Sort by (priority desc, span.start asc, rule_id asc).
    spans.sort_by(|a, b| {
        b.rule
            .priority()
            .cmp(&a.rule.priority())
            .then_with(|| a.span.start.cmp(&b.span.start))
            .then_with(|| a.rule.id().cmp(b.rule.id()))
    });

    let mut kept: Vec<MatchedSpan> = Vec::with_capacity(spans.len());
    'outer: for candidate in spans {
        for existing in &kept {
            if overlaps(&candidate.span, &existing.span) {
                continue 'outer;
            }
        }
        kept.push(candidate);
    }

    // Final return order: left-to-right by start.
    kept.sort_by_key(|m| m.span.start);
    kept
}

fn overlaps(a: &Match, b: &Match) -> bool {
    a.start < b.end && b.start < a.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RestoreScope;

    fn rspec(id: &str, pattern: &str, priority: i32) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            category: id.to_uppercase(),
            restore_scope: RestoreScope::Owner,
            priority,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex { pattern: pattern.into() },
        }
    }

    #[test]
    fn engine_applies_multiple_rules() {
        let engine = RuleEngine::from_specs(vec![
            rspec("email", r"[\w.+-]+@[\w-]+\.[\w.-]+", 50),
            rspec("phone", r"09\d{8}", 50),
        ])
        .unwrap();
        let hits = engine.apply(
            "contact alice@acme.com or 0912345678",
            &Source::ToolResult { tool_name: "x".into() },
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].span.original, "alice@acme.com");
        assert_eq!(hits[1].span.original, "0912345678");
    }

    #[test]
    fn overlap_higher_priority_wins() {
        // Two rules match overlapping spans. priority 100 must win.
        let engine = RuleEngine::from_specs(vec![
            rspec("digits", r"\d{8,12}", 50),
            rspec("tw_id", r"[A-Z][12]\d{8}", 100),
        ])
        .unwrap();
        let hits = engine.apply("A123456789", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rule.id(), "tw_id");
    }

    #[test]
    fn non_overlapping_matches_all_kept() {
        let engine = RuleEngine::from_specs(vec![
            rspec("digits", r"\d{4}", 50),
        ])
        .unwrap();
        let hits = engine.apply("1234 abcd 5678", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn left_to_right_order_after_resolve() {
        let engine = RuleEngine::from_specs(vec![
            rspec("a", r"foo", 50),
            rspec("b", r"bar", 50),
        ])
        .unwrap();
        let hits = engine.apply("bar foo bar", &Source::ToolResult { tool_name: "x".into() });
        let starts: Vec<usize> = hits.iter().map(|m| m.span.start).collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted);
    }

    #[test]
    fn system_prompt_source_only_runs_opted_in_rules() {
        let mut opted_in = rspec("opt_in", r"X", 50);
        opted_in.apply_to_system_prompt = true;
        let opted_out = rspec("opt_out", r"X", 50);

        let engine = RuleEngine::from_specs(vec![opted_in, opted_out]).unwrap();

        let prompt_hits = engine.apply("X", &Source::SystemPrompt { component: "soul".into() });
        assert_eq!(prompt_hits.len(), 1);
        assert_eq!(prompt_hits[0].rule.id(), "opt_in");

        let tool_hits = engine.apply("X", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(tool_hits.len(), 1);
        // tool-source: which one wins is deterministic by id ordering
    }

    #[test]
    fn future_rule_kinds_are_skipped_gracefully() {
        let spec = RuleSpec {
            id: "future".into(),
            category: "X".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Keyword {
                values: vec!["foo".into()],
                case_sensitive: true,
            },
        };
        let engine = RuleEngine::from_specs(vec![spec]).unwrap();
        assert_eq!(engine.rule_count(), 0);
    }
}
