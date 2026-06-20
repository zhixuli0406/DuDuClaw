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
