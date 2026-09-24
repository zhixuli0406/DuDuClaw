//! `type = "ner"` — model-backed detection of shapeless PII.
//!
//! One `[rules.<id>]` block with `type = "ner"` compiles into **one rule per
//! requested label**, each carrying its own DuDuClaw category. That is what
//! lets the rest of the pipeline stay unchanged: `Rule::category()` is a
//! single string, source filters are per-category, and an operator who only
//! wants names can write `labels = ["private_person"]`.
//!
//! The eight rules share one [`NerEngine`], and the engine caches by text, so
//! the eight `match_text` calls a single turn makes cost **one** inference.
//!
//! The spec's own `category` field is not used: each match is categorised by
//! the label the model assigned it (design §5). It stays in the schema
//! because every other rule kind needs it.

use std::sync::Arc;

use crate::error::{RedactionError, Result};
use crate::ner::install::InstallDirs;
use crate::ner::labels::{self, NerLabel};
use crate::ner::runtime::NerEngine;
use crate::ner::NerConfig;
use crate::rules::{Match, RestoreScope, Rule, RuleKind, RuleSpec};

/// One label's worth of a compiled `ner` rule.
#[derive(Debug)]
pub struct NerRule {
    spec: RuleSpec,
    label: &'static str,
    engine: NerEngine,
    min_chars: usize,
    max_chars: usize,
}

impl NerRule {
    /// Compile a `ner` spec into one rule per requested label.
    ///
    /// **Fail-closed.** An uninstalled model, an unknown label, or an
    /// unsupported platform is an `Err`, which the engine turns into a
    /// load failure and the gateway into the poison state. It never returns
    /// an empty vector — a rule that matches nothing looks exactly like a
    /// working one at runtime, which is the failure mode this whole pipeline
    /// exists to avoid.
    pub fn compile(
        spec: RuleSpec,
        dirs: &InstallDirs,
        config: &NerConfig,
    ) -> Result<Vec<Arc<dyn Rule>>> {
        let (requested, min_chars, max_chars) = match &spec.kind {
            RuleKind::Ner {
                labels,
                min_chars,
                max_chars,
            } => (
                labels.clone(),
                min_chars.unwrap_or(config.min_chars),
                max_chars.unwrap_or(config.max_chars),
            ),
            other => {
                return Err(RedactionError::rule_compile(
                    &spec.id,
                    format!("expected Ner kind, got {other:?}"),
                ));
            }
        };

        let resolved: Vec<&'static NerLabel> = labels::resolve_labels(&requested)
            .map_err(|e| RedactionError::rule_compile(&spec.id, e))?;

        let engine = NerEngine::open(dirs, config)
            .map_err(|e| RedactionError::rule_compile(&spec.id, e.to_string()))?;

        // A max_chars under the min is a config mistake that would silently
        // disable the rule; clamp so the rule still works and the operator's
        // intent (a long minimum) is honoured.
        let max_chars = max_chars.max(min_chars.max(1));

        let mut out: Vec<Arc<dyn Rule>> = Vec::with_capacity(resolved.len());
        for label in resolved {
            let mut per_label = spec.clone();
            per_label.category = label.category.to_string();
            out.push(Arc::new(NerRule {
                spec: per_label,
                label: label.model_label,
                engine: engine.clone(),
                min_chars,
                max_chars,
            }));
        }
        Ok(out)
    }

    /// The model label this rule surfaces.
    pub fn label(&self) -> &str {
        self.label
    }
}

impl Rule for NerRule {
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
        if text.chars().count() < self.min_chars {
            return Vec::new();
        }
        let spans = match self.engine.spans(text, self.max_chars) {
            Ok(s) => s,
            Err(e) => {
                // A runtime inference failure must not silently pass text
                // through as clean. `match_text` cannot return an error, so
                // the loud part is here; the structural fail-closed guarantee
                // is at compile time (an uninstalled model never yields a
                // rule at all).
                tracing::error!(
                    rule = %self.spec.id,
                    label = %self.label,
                    chars = text.chars().count(),
                    error = %e,
                    "NER inference failed — this text was not scanned by the model"
                );
                return Vec::new();
            }
        };
        spans
            .iter()
            .filter(|s| s.label == self.label)
            .filter_map(|s| {
                let (start, end) = crate::ner::safe_bounds(text, s.start, s.end);
                if end <= start {
                    return None;
                }
                Some(Match {
                    start,
                    end,
                    original: text[start..end].to_string(),
                })
            })
            .collect()
    }

    fn engine_kind(&self) -> &'static str {
        "ner"
    }

    fn model_revision(&self) -> Option<&str> {
        Some(self.engine.model_revision())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ner::{DEFAULT_MAX_CHARS, DEFAULT_MIN_CHARS, DEFAULT_NER_PRIORITY};

    fn spec(labels: Vec<String>) -> RuleSpec {
        RuleSpec {
            id: "ai_pii".into(),
            category: "PII".into(),
            restore_scope: RestoreScope::Owner,
            priority: DEFAULT_NER_PRIORITY,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Ner {
                labels,
                min_chars: None,
                max_chars: None,
            },
        }
    }

    #[test]
    fn compile_fails_closed_when_the_model_is_not_installed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        let err = NerRule::compile(spec(vec![]), &dirs, &NerConfig::default()).unwrap_err();
        match err {
            RedactionError::RuleCompile { rule_id, reason } => {
                assert_eq!(rule_id, "ai_pii");
                assert!(!reason.is_empty());
            }
            other => panic!("expected a rule-compile error, got {other:?}"),
        }
    }

    #[test]
    fn compile_rejects_an_unknown_label_before_touching_the_model() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        let err = NerRule::compile(spec(vec!["private_persons".into()]), &dirs, &NerConfig::default())
            .unwrap_err();
        assert!(format!("{err}").contains("private_persons"), "{err}");
    }

    #[test]
    fn compile_rejects_a_non_ner_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        let mut s = spec(vec![]);
        s.kind = RuleKind::Regex { pattern: "a".into() };
        let err = NerRule::compile(s, &dirs, &NerConfig::default()).unwrap_err();
        assert!(format!("{err}").contains("expected Ner kind"), "{err}");
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        assert_eq!(DEFAULT_MIN_CHARS, 24);
        assert_eq!(DEFAULT_MAX_CHARS, 32_000);
        assert_eq!(DEFAULT_NER_PRIORITY, 30);
    }
}
