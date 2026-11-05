//! The same pure rules as legacy extraction, with an fd-only bounded data model.
use duduclaw_isolation::{bounded::{List, Text}, worker::{ParseError, ParsedItem, SkillPage}};
#[path = "../../../duduclaw-gateway/src/skill_lifecycle/compression.rs"]
mod compression;
use compression::CompressedSkill;
#[path = "../../../duduclaw-gateway/src/skill_lifecycle/extraction_logic.rs"]
mod logic;

#[allow(dead_code)]
#[derive(Clone)]
enum WikiAction { Create }
#[derive(Clone, Default)]
struct WikiTarget;
#[allow(dead_code)]
#[derive(Clone)]
struct WikiProposal {
    page_path: String, action: WikiAction, content: Option<String>, rationale: String,
    related_pages: Vec<String>, target: WikiTarget,
}
struct ExtractionResult {
    skill_name: String, agent_id: String, concepts: Vec<WikiProposal>, entities: Vec<WikiProposal>, source_summary: WikiProposal,
}
pub(super) fn extract(bytes: &[u8], slot: u16, name: &str, agent: &str, at: &str) -> Result<ParsedItem, ParseError> {
    let content = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
    if content.trim().is_empty() { return Err(ParseError::InvalidContent); }
    let at = chrono::DateTime::parse_from_rfc3339(at).map_err(|_| ParseError::InvalidContent)?.with_timezone(&chrono::Utc);
    let result = logic::extract_heuristic_at(&CompressedSkill::compress(name, content, None), agent, at);
    if result.skill_name != name || result.agent_id != agent { return Err(ParseError::InvalidContent); }
    let concepts = result.concepts.len() as u16;
    let entities = result.entities.len() as u16;
    let pages = result.concepts.into_iter().chain(result.entities).chain([result.source_summary]).map(|p| {
        Ok(SkillPage { path: Text::new(p.page_path).map_err(|_| ParseError::LimitExceeded)?,
            content: Text::new(p.content.ok_or(ParseError::InvalidContent)?).map_err(|_| ParseError::LimitExceeded)?,
            rationale: Text::new(p.rationale).map_err(|_| ParseError::LimitExceeded)?,
            related: List::new(p.related_pages.into_iter().map(Text::new).collect::<Result<Vec<_>, _>>().map_err(|_| ParseError::LimitExceeded)?
                ).map_err(|_| ParseError::LimitExceeded)? })
    }).collect::<Result<Vec<_>, ParseError>>()?;
    Ok(ParsedItem::SkillKnowledge { input_slot: slot, concepts, entities,
        pages: List::new(pages).map_err(|_| ParseError::LimitExceeded)? })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn i1c_skill_extraction_preserves_concepts_entities_and_source_summary() {
        let result = extract("# 技能\n## 退款流程\nAsk **Customer Service** before handling refunds.\n".as_bytes(), 0,
            "refund", "alice", "2026-11-05T00:00:00Z").unwrap();
        let ParsedItem::SkillKnowledge { concepts, entities, pages, .. } = result else { panic!() };
        assert_eq!(concepts, 1); assert_eq!(entities, 1); assert_eq!(pages.as_slice().len(), 3);
        assert_eq!(pages.as_slice()[2].path.as_str(), "sources/skill-refund.md");
        assert!(extract(b"", 0, "x", "alice", "2026-11-05T00:00:00Z").is_err());
        assert!(extract(b"hello", 0, "x", "alice", "invalid").is_err());
    }
}
