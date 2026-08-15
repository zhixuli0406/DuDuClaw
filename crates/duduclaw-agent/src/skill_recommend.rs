//! WP2.6 §4 — static per-template skill recommendations.
//!
//! Each industry template (`templates/<industry>/agent.toml`, and any agent's
//! own `agent.toml`) may carry a curated `[skills] recommended = [...]` list:
//! the hand-picked skills that industry's agents almost always want. This is
//! the zero-cost, zero-LLM tier of the recommendation system — a human-curated
//! shortlist that needs no federated search to produce.
//!
//! Format (added to `agent.toml`):
//!
//! ```toml
//! [skills]
//! # WP2.6: skills this template's agents typically want. Each entry may be a
//! # bare slug ("code-review") or "hub:slug" ("clawhub:pro-code-reviewer").
//! recommended = ["clawhub:pro-code-reviewer", "pdf", "xlsx"]
//! ```
//!
//! The reader is intentionally lenient: a missing section, a missing file, or a
//! malformed table yields an empty list (never an error) — recommendations are
//! advisory. Entries are validated to be safe slugs so a poisoned template
//! cannot inject shell/path metacharacters downstream.

use std::path::Path;

/// One recommended skill parsed from a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendedSkill {
    /// Optional hub id qualifier (`clawhub`, `anthropic-skills`, …). `None`
    /// means "any configured hub / discovery".
    pub hub: Option<String>,
    /// Skill slug / identifier.
    pub slug: String,
}

impl RecommendedSkill {
    /// Render back to the `hub:slug` / `slug` wire form.
    pub fn as_ref_str(&self) -> String {
        match &self.hub {
            Some(h) => format!("{h}:{}", self.slug),
            None => self.slug.clone(),
        }
    }
}

/// True for a slug/hub token safe to place in a URL path or file name.
fn is_safe_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parse a single `recommended` entry (`"hub:slug"` or `"slug"`) into a
/// validated [`RecommendedSkill`]. `None` when either segment is unsafe.
pub fn parse_entry(raw: &str) -> Option<RecommendedSkill> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (hub, slug) = match raw.split_once(':') {
        Some((h, s)) => (Some(h.trim()), s.trim()),
        None => (None, raw),
    };
    if !is_safe_token(slug) {
        return None;
    }
    if let Some(h) = hub {
        if !is_safe_token(h) {
            return None;
        }
    }
    Some(RecommendedSkill {
        hub: hub.map(|h| h.to_string()),
        slug: slug.to_string(),
    })
}

/// Parse the `[skills] recommended` list out of raw `agent.toml` content.
/// Lenient: any parse failure / absent section ⇒ empty list. Unsafe entries
/// are dropped (fail-safe), not errored.
pub fn parse_recommended_from_toml(content: &str) -> Vec<RecommendedSkill> {
    // Shared typed parse point (R2 unification). Malformed TOML, an absent
    // `[skills]` table, an absent / non-array `recommended`, and non-string
    // elements inside it all degrade exactly as the raw walk did — to an empty
    // list, or to the list minus the bad elements.
    duduclaw_core::agent_toml::parse(content)
        .skills
        .recommended
        .iter()
        .filter_map(|s| parse_entry(s))
        .collect()
}

/// Read `[skills] recommended` from an agent/template directory's `agent.toml`.
/// Missing file ⇒ empty list.
pub fn read_recommended(dir: &Path) -> Vec<RecommendedSkill> {
    match std::fs::read_to_string(dir.join("agent.toml")) {
        Ok(c) => parse_recommended_from_toml(&c),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── R5: `[skills] recommended` direction, pinned ─────────────────────
    //
    // malformed TOML / absent `[skills]` / absent or non-array `recommended`
    // ⇒ EMPTY. Non-string elements, and entries that fail `parse_entry`'s
    // safety check, are DROPPED rather than erroring — a bad recommendation
    // must cost that one entry, never the whole list (and never the agent).

    #[test]
    fn default_direction_recommended_is_empty_for_anything_unusable() {
        for body in [
            "",                                    // empty file
            "[agent]\nname = \"x\"\n",             // no [skills]
            "[skills]\n",                          // section, no key
            "[skills]\nrecommended = \"one\"\n",   // non-array
            "[skills]\nrecommended = []\n",        // explicit empty
            "skills = 1\n",                        // wrong-typed section
            "not toml [[[",                        // malformed file
        ] {
            assert!(
                parse_recommended_from_toml(body).is_empty(),
                "for {body:?}"
            );
        }
    }

    #[test]
    fn default_direction_bad_entries_are_dropped_not_fatal() {
        // The raw walk did `filter_map(as_str)` then `filter_map(parse_entry)`
        // — a non-string element and an unsafe slug each cost one entry.
        let got = parse_recommended_from_toml(
            "[skills]\nrecommended = [\"good-skill\", 42, \"../escape\", \"also-good\"]\n",
        );
        let slugs: Vec<&str> = got.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(slugs, vec!["good-skill", "also-good"]);
    }

    #[test]
    fn default_direction_read_recommended_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!(
            "duduclaw-skillrec-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join("agent.toml"));
        assert!(read_recommended(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_bare_and_qualified_entries() {
        let toml = r#"
[agent]
name = "x"

[skills]
recommended = ["clawhub:pro-code-reviewer", "pdf", " xlsx ", "anthropic-skills:docx"]
"#;
        let recs = parse_recommended_from_toml(toml);
        assert_eq!(recs.len(), 4);
        assert_eq!(recs[0].hub.as_deref(), Some("clawhub"));
        assert_eq!(recs[0].slug, "pro-code-reviewer");
        assert_eq!(
            recs[1],
            RecommendedSkill {
                hub: None,
                slug: "pdf".into()
            }
        );
        assert_eq!(recs[2].slug, "xlsx", "trimmed");
        assert_eq!(recs[0].as_ref_str(), "clawhub:pro-code-reviewer");
        assert_eq!(recs[1].as_ref_str(), "pdf");
    }

    #[test]
    fn absent_section_and_garbage_are_empty_not_error() {
        assert!(parse_recommended_from_toml("[agent]\nname='x'").is_empty());
        assert!(parse_recommended_from_toml("not valid toml {{{").is_empty());
        assert!(parse_recommended_from_toml("[skills]\nrecommended = 5").is_empty());
    }

    #[test]
    fn unsafe_entries_are_dropped() {
        let toml = r#"
[skills]
recommended = ["ok-slug", "../evil", "bad slug", "rm -rf", "hub id:slug"]
"#;
        let recs = parse_recommended_from_toml(toml);
        // Only "ok-slug" survives; ".." and spaces are rejected.
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].slug, "ok-slug");
    }
}
