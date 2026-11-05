//! Skill Knowledge Extraction — extracts structured knowledge from skills into wiki pages.
//!
//! When a skill is activated or reaches distillation readiness, this module
//! extracts domain concepts, entities, and processes into the agent's wiki.
//!
//! Two modes:
//! - **Heuristic** (zero LLM cost): Parses markdown structure to identify knowledge
//! - **LLM**: Piggybacks on GVU distillation calls for deeper extraction

use std::path::Path;

use chrono::Utc;
use tracing::{debug, info, warn};

use duduclaw_memory::wiki::{WikiAction, WikiProposal, WikiStore, WikiTarget};

use super::compression::CompressedSkill;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Result of extracting knowledge from a skill.
#[derive(Debug, Clone)]
pub struct ExtractionResult {
    pub skill_name: String,
    pub agent_id: String,
    /// Concept pages (→ wiki/concepts/)
    pub concepts: Vec<WikiProposal>,
    /// Entity pages (→ wiki/entities/)
    pub entities: Vec<WikiProposal>,
    /// Source summary page (→ wiki/sources/skill-{name}.md)
    pub source_summary: WikiProposal,
}

impl ExtractionResult {
    /// All proposals in one flat list.
    pub fn all_proposals(&self) -> Vec<WikiProposal> {
        let mut all = self.concepts.clone();
        all.extend(self.entities.clone());
        all.push(self.source_summary.clone());
        all
    }
}

// ---------------------------------------------------------------------------
// Heuristic Extraction (zero LLM cost)
// ---------------------------------------------------------------------------

/// Extract structured knowledge from a skill using markdown parsing heuristics.
///
/// Parses headings, lists, and content structure to identify:
/// - `## Section` headings → concept pages
/// - Proper nouns and entity patterns → entity pages
/// - Full skill → source summary with `internalized_from` metadata
pub fn extract_heuristic(skill: &CompressedSkill, agent_id: &str) -> ExtractionResult {
    logic::extract_heuristic_at(skill, agent_id, Utc::now())
}

#[path = "extraction_logic.rs"]
mod logic;
use logic::sanitize_for_path;
#[cfg(test)]
use logic::parse_sections;

/// Check if a skill has already been extracted to wiki.
pub fn is_already_extracted(skill_name: &str, wiki_dir: &Path) -> bool {
    let source_path = wiki_dir
        .join("sources")
        .join(format!("skill-{}.md", sanitize_for_path(skill_name)));
    source_path.exists()
}

/// Run extraction and apply proposals to the wiki.
///
/// Wraps in `spawn_blocking` to avoid blocking async runtime with flock.
pub async fn extract_and_apply(skill: &CompressedSkill, agent_id: &str, home_dir: &Path) {
    let wiki_dir = home_dir.join("agents").join(agent_id).join("wiki");

    let result = extract_heuristic(skill, agent_id);
    let proposals = result.all_proposals();

    if proposals.is_empty() {
        debug!(agent = agent_id, skill = %skill.name, "No knowledge to extract from skill");
        return;
    }

    // Validate proposals
    if let Err(gradient) = crate::gvu::verifier::verify_wiki_proposals(&proposals) {
        warn!(
            agent = agent_id,
            skill = %skill.name,
            critique = %gradient.critique,
            "Skill extraction proposals rejected by verifier"
        );
        return;
    }

    let proposals_owned = proposals;
    let agent_owned = agent_id.to_string();
    let skill_name = skill.name.clone();
    let skill_tag = sanitize_for_path(&skill.name);
    let wiki_dir_owned = wiki_dir;

    // Use spawn_blocking + create_new sentinel to prevent TOCTOU race
    let result = tokio::task::spawn_blocking(move || {
        let store = WikiStore::new(wiki_dir_owned.clone());
        if let Err(e) = store.ensure_scaffold() {
            return Err(format!("scaffold: {e}"));
        }

        // Atomic sentinel: O_EXCL guarantees only one caller succeeds
        let sentinel = wiki_dir_owned
            .join("sources")
            .join(format!(".extracting-{}", skill_tag));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&sentinel)
        {
            Ok(_) => {} // We won the race — proceed
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err("already_extracted".to_string());
            }
            Err(_) => {
                // Check if the actual source page exists (fallback for filesystem issues)
                let source_page = wiki_dir_owned
                    .join("sources")
                    .join(format!("skill-{}.md", skill_tag));
                if source_page.exists() {
                    return Err("already_extracted".to_string());
                }
            }
        }

        let result = store
            .apply_proposals(&proposals_owned)
            .map_err(|e| e.to_string());
        // Clean up sentinel regardless of outcome
        let _ = std::fs::remove_file(&sentinel);
        result
    })
    .await;

    match result {
        Ok(Ok(count)) => {
            info!(
                agent = %agent_owned,
                skill = %skill_name,
                pages = count,
                "Skill knowledge extracted to wiki"
            );
        }
        Ok(Err(ref e)) if e == "already_extracted" => {
            debug!(agent = %agent_owned, skill = %skill_name, "Skill already extracted to wiki, skipping");
        }
        Ok(Err(e)) => {
            warn!(agent = %agent_owned, skill = %skill_name, "Extraction apply failed: {e}")
        }
        Err(e) => warn!(agent = %agent_owned, skill = %skill_name, "Extraction panicked: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_skill() -> CompressedSkill {
        CompressedSkill {
            name: "customer-service".to_string(),
            tag: "customer-service".to_string(),
            summary: "Guidelines for handling customer inquiries".to_string(),
            full_content: r#"# Customer Service Guidelines

## Greeting Protocol
Always greet customers warmly. Use their name when available.
- Say "Welcome!" or the equivalent in the customer's language
- Ask how you can help today

## Return Policy
Items can be returned within 30 days of purchase.
- Must have original receipt
- **Electronics** have a 15-day return window
- **Fresh Food** items are non-returnable

## Escalation Rules
When the customer is upset:
1. Acknowledge their feelings
2. Apologize sincerely
3. Offer a solution or escalate to **Manager On Duty**
"#
            .to_string(),
            tokens_layer0: 5,
            tokens_layer1: 30,
            tokens_layer2: 200,
        }
    }

    #[test]
    fn test_extract_heuristic_concepts() {
        let skill = sample_skill();
        let result = extract_heuristic(&skill, "agnes");

        assert_eq!(result.skill_name, "customer-service");
        assert!(
            result.concepts.len() >= 2,
            "Should extract at least Greeting Protocol and Return Policy"
        );

        let concept_paths: Vec<_> = result.concepts.iter().map(|c| &c.page_path).collect();
        assert!(concept_paths.iter().any(|p| p.contains("greeting")));
        assert!(concept_paths.iter().any(|p| p.contains("return")));
    }

    #[test]
    fn test_extract_heuristic_entities() {
        let skill = sample_skill();
        let result = extract_heuristic(&skill, "agnes");

        let entity_names: Vec<_> = result
            .entities
            .iter()
            .filter_map(|e| e.content.as_ref())
            .filter(|c| c.contains("Electronics") || c.contains("Manager"))
            .collect();
        assert!(!entity_names.is_empty(), "Should extract bold entities");
    }

    #[test]
    fn test_extract_heuristic_source_summary() {
        let skill = sample_skill();
        let result = extract_heuristic(&skill, "agnes");

        assert!(
            result
                .source_summary
                .page_path
                .starts_with("sources/skill-")
        );
        let content = result.source_summary.content.as_ref().unwrap();
        assert!(content.contains("internalized_from: customer-service"));
        assert!(content.contains("maturity: draft"));
    }

    #[test]
    fn test_extract_empty_skill() {
        let skill = CompressedSkill {
            name: "empty".to_string(),
            tag: "empty".to_string(),
            summary: String::new(),
            full_content: String::new(),
            tokens_layer0: 0,
            tokens_layer1: 0,
            tokens_layer2: 0,
        };
        let result = extract_heuristic(&skill, "test");
        assert!(result.concepts.is_empty());
        assert!(result.entities.is_empty());
        // Source summary always exists
        assert!(result.source_summary.content.is_some());
    }

    #[test]
    fn test_parse_sections() {
        let content = "## Section A\nContent A\n\n## Section B\nContent B\n";
        let sections = parse_sections(content);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].heading, "Section A");
        assert_eq!(sections[1].heading, "Section B");
    }

    #[test]
    fn test_parse_sections_with_title() {
        let content = "# Title\n\n## Section A\nContent A\n\n## Section B\nContent B\n";
        let sections = parse_sections(content);
        // # Title is ignored — only ## headings create sections
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].heading, "Section A");
        assert_eq!(sections[1].heading, "Section B");
    }

    #[test]
    fn test_parse_sections_unclosed_frontmatter() {
        let content = "---\ntitle: Test\n## Section\nBody text.\n";
        let sections = parse_sections(content);
        // Unclosed frontmatter triggers fallback parser
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, "Section");
    }

    #[test]
    fn test_parse_sections_with_frontmatter() {
        let content = "---\ntitle: Test\ntags: [a]\n---\n\n## Concept\nBody here.\n";
        let sections = parse_sections(content);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, "Concept");
        assert!(sections[0].body.contains("Body here"));
    }

    #[test]
    fn test_sanitize_for_path() {
        assert_eq!(sanitize_for_path("Hello World!"), "hello-world");
        assert_eq!(sanitize_for_path("Return Policy"), "return-policy");
        assert_eq!(sanitize_for_path("customer-service"), "customer-service");
        // Emoji should be replaced with dash (not kept)
        assert_eq!(sanitize_for_path("🔧 Setup"), "setup");
        // Path traversal chars stripped
        assert_eq!(sanitize_for_path("../evil"), "evil");
    }

    #[test]
    fn test_is_already_extracted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wiki_dir = tmp.path().join("wiki");
        std::fs::create_dir_all(wiki_dir.join("sources")).unwrap();
        std::fs::write(
            wiki_dir.join("sources/skill-test-skill.md"),
            "---\ntitle: test\n---\n",
        )
        .unwrap();

        assert!(is_already_extracted("test-skill", &wiki_dir));
        assert!(!is_already_extracted("other-skill", &wiki_dir));
    }
}
