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

use duduclaw_memory::wiki::{WikiAction, WikiProposal, WikiStore};

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
pub fn extract_heuristic(
    skill: &CompressedSkill,
    agent_id: &str,
) -> ExtractionResult {
    let now = Utc::now();
    let date_str = now.to_rfc3339();
    let skill_tag = sanitize_for_path(&skill.name);

    let mut concepts = Vec::new();
    let mut entities = Vec::new();

    // Parse skill content into sections (cap at 30 to prevent DoS)
    let sections = parse_sections(&skill.full_content);
    let max_sections = 30;

    for section in sections.iter().take(max_sections) {
        if section.heading.is_empty() || section.body.trim().is_empty() {
            continue;
        }

        let page_name = sanitize_for_path(&section.heading);
        if page_name.is_empty() {
            continue;
        }
        let page_path = format!("concepts/{}.md", page_name);

        // Build page content with internalization metadata
        let tags = extract_tags_from_content(&section.body);
        let tags_str = if tags.is_empty() {
            format!("{}, internalized", skill_tag)
        } else {
            format!("{}, {}, internalized", skill_tag, tags.join(", "))
        };

        let content = format!(
            "---\n\
             title: {}\n\
             created: {}\n\
             updated: {}\n\
             tags: [{}]\n\
             related: [sources/skill-{}.md]\n\
             sources: [skill:{}]\n\
             internalized_from: {}\n\
             maturity: draft\n\
             ---\n\n\
             {}\n",
            yaml_quote(&section.heading),
            date_str,
            date_str,
            tags_str,
            skill_tag,
            yaml_quote(&skill.name),
            yaml_quote(&skill.name),
            section.body.trim(),
        );

        concepts.push(WikiProposal {
            page_path,
            action: WikiAction::Create,
            content: Some(content),
            rationale: format!("Extracted from skill '{}' section '{}'", skill.name, section.heading),
            related_pages: vec![format!("sources/skill-{}.md", skill_tag)],
        });
    }

    // Extract entities from the full skill content
    let extracted_entities = extract_entities_from_skill(&skill.full_content);
    for (entity_name, entity_context) in &extracted_entities {
        let entity_path = format!("entities/{}.md", sanitize_for_path(entity_name));
        // Truncate entity context to prevent oversized wiki pages
        let safe_context: String = entity_context.chars().take(200).collect();
        let content = format!(
            "---\n\
             title: {}\n\
             created: {}\n\
             updated: {}\n\
             tags: [entity, {}, internalized]\n\
             related: [sources/skill-{}.md]\n\
             sources: [skill:{}]\n\
             internalized_from: {}\n\
             maturity: draft\n\
             ---\n\n\
             {}\n",
            yaml_quote(entity_name),
            date_str,
            date_str,
            skill_tag,
            skill_tag,
            yaml_quote(&skill.name),
            yaml_quote(&skill.name),
            safe_context,
        );

        entities.push(WikiProposal {
            page_path: entity_path,
            action: WikiAction::Create,
            content: Some(content),
            rationale: format!("Entity '{}' extracted from skill '{}'", entity_name, skill.name),
            related_pages: vec![format!("sources/skill-{}.md", skill_tag)],
        });
    }

    // Source summary — always created
    let concept_links: Vec<String> = concepts.iter().map(|c| c.page_path.clone()).collect();
    let entity_links: Vec<String> = entities.iter().map(|e| e.page_path.clone()).collect();
    let all_related: Vec<String> = concept_links.iter().chain(entity_links.iter()).cloned().collect();
    let related_str = if all_related.is_empty() {
        "[]".to_string()
    } else {
        let quoted: Vec<String> = all_related.iter().map(|p| yaml_quote(p)).collect();
        format!("[{}]", quoted.join(", "))
    };

    let source_content = format!(
        "---\n\
         title: {}\n\
         created: {}\n\
         updated: {}\n\
         tags: [skill-source, {}]\n\
         related: {}\n\
         sources: [skill:{}]\n\
         internalized_from: {}\n\
         maturity: draft\n\
         ---\n\n\
         ## Summary\n\n\
         {}\n\n\
         ## Extracted Knowledge\n\n\
         - {} concept pages extracted\n\
         - {} entity pages extracted\n\
         - Skill token count: ~{} tokens\n",
        yaml_quote(&format!("Skill: {}", skill.name)),
        date_str,
        date_str,
        skill_tag,
        related_str,
        yaml_quote(&skill.name),
        yaml_quote(&skill.name),
        skill.summary,
        concepts.len(),
        entities.len(),
        skill.tokens_layer2,
    );

    let source_summary = WikiProposal {
        page_path: format!("sources/skill-{}.md", skill_tag),
        action: WikiAction::Create,
        content: Some(source_content),
        rationale: format!("Source summary for skill '{}'", skill.name),
        related_pages: all_related,
    };

    ExtractionResult {
        skill_name: skill.name.clone(),
        agent_id: agent_id.to_string(),
        concepts,
        entities,
        source_summary,
    }
}

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
pub async fn extract_and_apply(
    skill: &CompressedSkill,
    agent_id: &str,
    home_dir: &Path,
) {
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
        let sentinel = wiki_dir_owned.join("sources").join(format!(".extracting-{}", skill_tag));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&sentinel) {
            Ok(_) => {} // We won the race — proceed
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err("already_extracted".to_string());
            }
            Err(_) => {
                // Check if the actual source page exists (fallback for filesystem issues)
                let source_page = wiki_dir_owned.join("sources").join(format!("skill-{}.md", skill_tag));
                if source_page.exists() {
                    return Err("already_extracted".to_string());
                }
            }
        }

        let result = store.apply_proposals(&proposals_owned).map_err(|e| e.to_string());
        // Clean up sentinel regardless of outcome
        let _ = std::fs::remove_file(&sentinel);
        result
    }).await;

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
        Ok(Err(e)) => warn!(agent = %agent_owned, skill = %skill_name, "Extraction apply failed: {e}"),
        Err(e) => warn!(agent = %agent_owned, skill = %skill_name, "Extraction panicked: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Markdown parsing helpers
// ---------------------------------------------------------------------------

/// A section extracted from markdown content.
struct Section {
    heading: String,
    body: String,
}

/// Parse markdown into sections by `##` headings.
///
/// Only `##` headings create concept sections. `#` headings are ignored
/// (they represent the skill title, not extractable concepts).
/// YAML frontmatter is skipped only if the first non-empty line is `---`.
fn parse_sections(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_heading = String::new();
    let mut current_body = String::new();
    let mut in_frontmatter = false;
    let mut first_line_seen = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Frontmatter: only start if the very first non-empty line is `---`
        if !first_line_seen && !trimmed.is_empty() {
            first_line_seen = true;
            if trimmed == "---" {
                in_frontmatter = true;
                continue;
            }
        }

        // Close frontmatter on second `---`
        if in_frontmatter {
            if trimmed == "---" {
                in_frontmatter = false;
            }
            continue;
        }

        if trimmed.starts_with("## ") {
            // Save previous section
            if !current_heading.is_empty() && !current_body.trim().is_empty() {
                sections.push(Section {
                    heading: current_heading,
                    body: current_body.trim().to_string(),
                });
            }
            current_heading = trimmed[3..].trim().to_string();
            current_body = String::new();
        } else if !trimmed.starts_with("# ") {
            // Accumulate body (skip `# Title` lines — they're not concept sections)
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    // If frontmatter was never closed, warn and parse from scratch without skipping
    if in_frontmatter {
        warn!("Skill has unclosed frontmatter (missing closing ---), parsing all content");
        return parse_sections_no_frontmatter(content);
    }

    // Save last section
    if !current_heading.is_empty() && !current_body.trim().is_empty() {
        sections.push(Section {
            heading: current_heading,
            body: current_body.trim().to_string(),
        });
    }

    sections
}

/// Fallback parser that ignores frontmatter entirely.
fn parse_sections_no_frontmatter(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_heading = String::new();
    let mut current_body = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            if !current_heading.is_empty() && !current_body.trim().is_empty() {
                sections.push(Section {
                    heading: current_heading,
                    body: current_body.trim().to_string(),
                });
            }
            current_heading = trimmed[3..].trim().to_string();
            current_body = String::new();
        } else if !trimmed.starts_with("# ") {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }
    if !current_heading.is_empty() && !current_body.trim().is_empty() {
        sections.push(Section {
            heading: current_heading,
            body: current_body.trim().to_string(),
        });
    }
    sections
}

/// Extract entity-like proper nouns from skill content.
///
/// Uses simple heuristics: CJK name patterns, capitalized multi-word sequences, etc.
fn extract_entities_from_skill(content: &str) -> Vec<(String, String)> {
    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Pattern: "**Entity Name**" in bold — common in skill files
        let mut rest = trimmed;
        while let Some(start) = rest.find("**") {
            let after = &rest[start + 2..];
            if let Some(end) = after.find("**") {
                let entity = after[..end].trim();
                // Only accept 2-40 char names, not generic words
                if entity.len() >= 2
                    && entity.len() <= 40
                    && !entity.contains('\n')
                    && entity.chars().next().map(|c| c.is_uppercase() || (c as u32) >= 0x4E00).unwrap_or(false)
                    && seen.insert(entity.to_lowercase())
                {
                    // Get surrounding context
                    let context = trimmed.replace(&format!("**{}**", entity), entity);
                    entities.push((entity.to_string(), context));
                }
                rest = &after[end + 2..];
            } else {
                break;
            }
        }
    }

    // Limit to 10 entities per skill
    entities.truncate(10);
    entities
}

/// Extract topic tags from content by identifying repeated keywords.
fn extract_tags_from_content(content: &str) -> Vec<String> {
    let lower = content.to_lowercase();
    let mut word_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    let stopwords = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "to", "of", "in", "for", "on", "with",
        "at", "by", "from", "as", "into", "through", "during", "before", "after",
        "and", "but", "or", "not", "this", "that", "these", "those", "it", "its",
        "use", "used", "when", "if", "then", "also", "all", "each", "such",
        "per", "any", "both", "must", "their", "they", "you", "your", "our", "we",
    ];

    for word in lower.split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        // Use char count (not byte length) for CJK-safe minimum length check
        let char_count = clean.chars().count();
        if char_count >= 2 && !stopwords.contains(&clean.as_str()) {
            *word_counts.entry(clean).or_insert(0) += 1;
        }
    }

    let mut sorted: Vec<_> = word_counts.into_iter().filter(|(_, c)| *c >= 2).collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.into_iter().take(5).map(|(w, _)| w).collect()
}

/// Sanitize a string for use in wiki file paths (kebab-case).
/// Only preserves ASCII alphanumeric and CJK Unified Ideographs (Basic + Extension A).
/// Truncates to 80 chars to stay under filesystem filename limits.
fn sanitize_for_path(s: &str) -> String {
    let result: String = s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                let cp = c as u32;
                if (0x4E00..=0x9FFF).contains(&cp) || (0x3400..=0x4DBF).contains(&cp) {
                    c
                } else {
                    '-'
                }
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    // Truncate to 80 chars (safely on char boundary) to prevent OS filename limit issues
    if result.chars().count() > 80 {
        result.chars().take(80).collect()
    } else {
        result
    }
}

/// Quote a string for safe embedding in YAML frontmatter values.
/// Strips newlines/carriage returns and wraps in double quotes if special chars present.
fn yaml_quote(s: &str) -> String {
    let clean: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if clean.contains(':') || clean.contains('#') || clean.contains('[')
        || clean.contains(']') || clean.contains('{') || clean.contains('}')
        || clean.contains('\'') || clean.contains('"')
        || clean.contains('|') || clean.contains('>')
        || clean.contains('*') || clean.contains('&') || clean.contains('!')
        || clean.contains('%') || clean.contains('@') || clean.contains(',')
    {
        let escaped = clean.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{}\"", escaped)
    } else {
        clean
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
"#.to_string(),
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
        assert!(result.concepts.len() >= 2, "Should extract at least Greeting Protocol and Return Policy");

        let concept_paths: Vec<_> = result.concepts.iter().map(|c| &c.page_path).collect();
        assert!(concept_paths.iter().any(|p| p.contains("greeting")));
        assert!(concept_paths.iter().any(|p| p.contains("return")));
    }

    #[test]
    fn test_extract_heuristic_entities() {
        let skill = sample_skill();
        let result = extract_heuristic(&skill, "agnes");

        let entity_names: Vec<_> = result.entities.iter()
            .filter_map(|e| e.content.as_ref())
            .filter(|c| c.contains("Electronics") || c.contains("Manager"))
            .collect();
        assert!(!entity_names.is_empty(), "Should extract bold entities");
    }

    #[test]
    fn test_extract_heuristic_source_summary() {
        let skill = sample_skill();
        let result = extract_heuristic(&skill, "agnes");

        assert!(result.source_summary.page_path.starts_with("sources/skill-"));
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
        ).unwrap();

        assert!(is_already_extracted("test-skill", &wiki_dir));
        assert!(!is_already_extracted("other-skill", &wiki_dir));
    }
}
