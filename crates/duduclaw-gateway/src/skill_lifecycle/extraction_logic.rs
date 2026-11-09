//! Shared pure extraction rules; no filesystem, model or publication authority.
use tracing::warn;
use super::{CompressedSkill, ExtractionResult, WikiAction, WikiProposal, WikiTarget};

pub(super) fn extract_heuristic_at(skill: &CompressedSkill, agent_id: &str, now: chrono::DateTime<chrono::Utc>) -> ExtractionResult {
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
            rationale: format!(
                "Extracted from skill '{}' section '{}'",
                skill.name, section.heading
            ),
            related_pages: vec![format!("sources/skill-{}.md", skill_tag)],
            target: WikiTarget::default(),
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
            rationale: format!(
                "Entity '{}' extracted from skill '{}'",
                entity_name, skill.name
            ),
            related_pages: vec![format!("sources/skill-{}.md", skill_tag)],
            target: WikiTarget::default(),
        });
    }

    // Source summary — always created
    let concept_links: Vec<String> = concepts.iter().map(|c| c.page_path.clone()).collect();
    let entity_links: Vec<String> = entities.iter().map(|e| e.page_path.clone()).collect();
    let all_related: Vec<String> = concept_links
        .iter()
        .chain(entity_links.iter())
        .cloned()
        .collect();
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
        target: WikiTarget::default(),
    };

    ExtractionResult {
        skill_name: skill.name.clone(),
        agent_id: agent_id.to_string(),
        concepts,
        entities,
        source_summary,
    }
}

// ---------------------------------------------------------------------------
// Markdown parsing helpers
// ---------------------------------------------------------------------------

/// A section extracted from markdown content.
pub(super) struct Section {
    pub(super) heading: String,
    pub(super) body: String,
}

/// Parse markdown into sections by `##` headings.
///
/// Only `##` headings create concept sections. `#` headings are ignored
/// (they represent the skill title, not extractable concepts).
/// YAML frontmatter is skipped only if the first non-empty line is `---`.
pub(super) fn parse_sections(content: &str) -> Vec<Section> {
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
            current_heading = trimmed.strip_prefix("## ").unwrap_or("").trim().to_string();
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
            current_heading = trimmed.strip_prefix("## ").unwrap_or("").trim().to_string();
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
pub(super) fn extract_entities_from_skill(content: &str) -> Vec<(String, String)> {
    let mut entities = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Pattern: "**Entity Name**" in bold — common in skill files
        let mut rest = trimmed;
        while let Some(start) = rest.find("**") {
            let after = rest.get(start + 2..).unwrap_or("");
            if let Some(end) = after.find("**") {
                let entity = after.get(..end).unwrap_or("").trim();
                // Only accept 2-40 char names, not generic words
                if entity.len() >= 2
                    && entity.len() <= 40
                    && !entity.contains('\n')
                    && entity
                        .chars()
                        .next()
                        .map(|c| c.is_uppercase() || (c as u32) >= 0x4E00)
                        .unwrap_or(false)
                    && seen.insert(entity.to_lowercase())
                {
                    // Get surrounding context
                    let context = trimmed.replace(&format!("**{}**", entity), entity);
                    entities.push((entity.to_string(), context));
                }
                rest = after.get(end + 2..).unwrap_or("");
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
    let mut word_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    let stopwords = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "to", "of",
        "in", "for", "on", "with", "at", "by", "from", "as", "into", "through", "during", "before",
        "after", "and", "but", "or", "not", "this", "that", "these", "those", "it", "its", "use",
        "used", "when", "if", "then", "also", "all", "each", "such", "per", "any", "both", "must",
        "their", "they", "you", "your", "our", "we",
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
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));
    sorted.into_iter().take(5).map(|(w, _)| w).collect()
}

/// Sanitize a string for use in wiki file paths (kebab-case).
/// Only preserves ASCII alphanumeric and CJK Unified Ideographs (Basic + Extension A).
/// Truncates to 80 chars to stay under filesystem filename limits.
pub(super) fn sanitize_for_path(s: &str) -> String {
    let result: String = s
        .chars()
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
pub(super) fn yaml_quote(s: &str) -> String {
    let clean: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    if clean.contains(':')
        || clean.contains('#')
        || clean.contains('[')
        || clean.contains(']')
        || clean.contains('{')
        || clean.contains('}')
        || clean.contains('\'')
        || clean.contains('"')
        || clean.contains('|')
        || clean.contains('>')
        || clean.contains('*')
        || clean.contains('&')
        || clean.contains('!')
        || clean.contains('%')
        || clean.contains('@')
        || clean.contains(',')
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

