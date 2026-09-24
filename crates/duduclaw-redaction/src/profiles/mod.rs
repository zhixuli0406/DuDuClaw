//! Built-in redaction profiles, embedded into the binary at compile time.
//!
//! Profile TOML lives next to this module file (`general.toml`,
//! `taiwan_strict.toml`, ...). Operators can reference them by name from
//! `agent.toml [redaction] profiles = ["taiwan_strict"]`.

use std::collections::HashMap;

use crate::config::Profile;
use crate::error::Result;

/// All built-in profiles bundled in this build. Keyed by profile name.
pub fn builtin_profiles() -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    map.insert("general", include_str!("general.toml"));
    map.insert("taiwan_strict", include_str!("taiwan_strict.toml"));
    map.insert("taiwan_minimal", include_str!("taiwan_minimal.toml"));
    map.insert("financial", include_str!("financial.toml"));
    map.insert("developer", include_str!("developer.toml"));
    // 2026-09 — needs the NER model installed; `Profile::requires_model()`
    // is what the dashboard asks rather than hard-coding this name.
    map.insert("ai_pii", include_str!("ai_pii.toml"));
    map
}

/// Parse a built-in profile by name.
pub fn load_builtin(name: &str) -> Result<Option<Profile>> {
    match builtin_profiles().get(name) {
        Some(body) => Ok(Some(Profile::from_toml_str(body)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses() {
        for (name, body) in builtin_profiles() {
            let prof = Profile::from_toml_str(body)
                .unwrap_or_else(|e| panic!("profile '{name}' failed to parse: {e}"));
            assert!(!prof.meta.name.is_empty(), "profile '{name}' missing meta.name");
            assert!(
                !prof.rules.is_empty(),
                "profile '{name}' should declare at least one rule"
            );
        }
    }

    #[test]
    fn only_ai_pii_requires_the_model() {
        for (name, body) in builtin_profiles() {
            let prof = Profile::from_toml_str(body).unwrap();
            assert_eq!(
                prof.requires_model(),
                name == "ai_pii",
                "profile '{name}' disagrees about needing a model download"
            );
        }
    }

    #[test]
    fn ai_pii_declares_one_ner_rule_below_every_pattern_rule() {
        let prof = load_builtin("ai_pii").unwrap().unwrap();
        assert_eq!(prof.rules.len(), 1);
        let rule = prof.rules.values().next().unwrap();
        assert!(matches!(
            rule.kind,
            crate::rules::RuleKind::Ner { ref labels, .. } if labels.is_empty()
        ));
        assert_eq!(rule.priority, crate::ner::DEFAULT_NER_PRIORITY);
        // Every other built-in must out-rank it, or a precise pattern could
        // lose an overlap to a fuzzy model span.
        for (name, body) in builtin_profiles() {
            if name == "ai_pii" {
                continue;
            }
            let other = Profile::from_toml_str(body).unwrap();
            for (id, spec) in &other.rules {
                assert!(
                    spec.priority > rule.priority,
                    "{name}.{id} has priority {} which does not beat the NER rule's {}",
                    spec.priority,
                    rule.priority
                );
            }
        }
    }

    #[test]
    fn ai_pii_advertises_all_eight_real_categories_not_the_placeholder() {
        // What the dashboard renders as chips. Reading the rule's declared
        // `category` would show a category called "PII" that no token ever
        // carries.
        let prof = load_builtin("ai_pii").unwrap().unwrap();
        let cats = prof.categories();
        assert_eq!(
            cats,
            vec![
                "ACCOUNT_NUMBER", "ADDRESS", "DATE", "EMAIL", "PERSON", "PHONE", "SECRET", "URL",
            ]
        );
        assert!(!cats.iter().any(|c| c == "PII"), "placeholder must not leak: {cats:?}");
    }

    #[test]
    fn a_narrowed_ner_rule_advertises_only_what_it_asks_for() {
        let prof = Profile::from_toml_str(
            r#"
[meta]
name = "names only"
[rules.names]
type = "ner"
category = "PII"
labels = ["private_person"]
"#,
        )
        .unwrap();
        assert_eq!(prof.categories(), vec!["PERSON"]);
    }

    #[test]
    fn a_pattern_profile_still_advertises_its_declared_categories() {
        let prof = load_builtin("general").unwrap().unwrap();
        let cats = prof.categories();
        assert!(cats.contains(&"EMAIL".to_string()), "{cats:?}");
        assert!(cats.windows(2).all(|w| w[0] <= w[1]), "must be sorted: {cats:?}");
    }

    #[test]
    fn unknown_profile_returns_none() {
        assert!(load_builtin("does_not_exist").unwrap().is_none());
    }

    #[test]
    fn taiwan_national_id_has_word_boundaries() {
        use crate::engine::RuleEngine;
        use crate::source::Source;

        let prof = load_builtin("taiwan_strict").unwrap().unwrap();
        let engine = RuleEngine::from_specs(prof.into_specs()).unwrap();
        let src = Source::ToolResult { tool_name: "x".into() };

        // A clean, delimited national ID still matches.
        let hits = engine.apply("id is A123456789 thanks", &src);
        assert!(
            hits.iter().any(|m| m.span.original == "A123456789"),
            "well-formed national ID must match"
        );

        // A longer alnum run must NOT yield a national-ID substring match.
        let over = engine.apply("XA1234567890", &src);
        assert!(
            !over.iter().any(|m| m.rule.category() == "TW_ID"),
            "national ID must not over-match inside a longer token, got {over:?}"
        );
    }
}
