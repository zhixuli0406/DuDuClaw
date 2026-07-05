//! F2b Reflexion consolidation — bridge MistakeNotebook → semantic memory.
//!
//! When the same `MistakeCategory` accumulates `>= threshold` unresolved entries,
//! distil them into one generalised rule stored in the agent's **semantic**
//! memory layer (via the F1 temporal supersession chain), then mark the source
//! mistakes resolved so they stop re-triggering / re-counting.
//!
//! Rule synthesis is deterministic (zero LLM cost, fully testable): it aggregates
//! the distinct "what went wrong" lessons into a single guard-rail. The semantic
//! rule then becomes a long-lived recall source (F2a) in place of the noisier
//! per-mistake episodic entries.

use std::path::Path;

use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};

use crate::gvu::mistake_notebook::{MistakeCategory, MistakeEntry, MistakeNotebook};

/// Default number of same-category unresolved mistakes that triggers consolidation.
pub const DEFAULT_CONSOLIDATE_THRESHOLD: u32 = 3;

/// Consolidate recurring mistakes of `category` into a semantic memory rule.
///
/// Returns `Ok(Some(semantic_id))` when a consolidation happened, `Ok(None)`
/// when the unresolved count is below `threshold`.
pub async fn maybe_consolidate(
    notebook: &MistakeNotebook,
    memory_db_path: &Path,
    agent_id: &str,
    category: MistakeCategory,
    threshold: u32,
) -> Result<Option<String>, String> {
    let count = notebook.count_unresolved_by_category(agent_id, category);
    if count < threshold {
        return Ok(None);
    }

    let mistakes =
        notebook.query_unresolved_by_category(agent_id, category, 20.max(threshold as usize));
    if (mistakes.len() as u32) < threshold {
        return Ok(None);
    }

    let rule = synthesize_rule(category, &mistakes);
    let source_ids: Vec<String> = mistakes.iter().map(|m| m.id.clone()).collect();

    let engine =
        SqliteMemoryEngine::new(memory_db_path).map_err(|e| format!("open memory engine: {e}"))?;

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: rule,
        timestamp: chrono::Utc::now(),
        tags: vec![
            "reflexion".to_string(),
            "consolidated".to_string(),
            format!("category:{}", category.as_str()),
        ],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 8.0,
        access_count: 0,
        last_accessed: None,
        source_event: "reflexion_consolidation".to_string(),
    };

    // Triple ties successive consolidations of the same category into a
    // supersession chain (newer rule supersedes the older one automatically).
    let meta = TemporalMeta {
        subject: Some(format!("category:{}", category.as_str())),
        predicate: Some("requires_care".to_string()),
        object: None,
        valid_from: None,
        valid_until: None,
        confidence: Some(0.9),
        // `rule_stats` seeds the ACE/ExpeL lifecycle counters (initial
        // importance = 2) settled per-turn by `prediction::rule_lifecycle`.
        metadata: Some(serde_json::json!({
            "source_mistake_ids": source_ids,
            "rule_stats": crate::prediction::rule_lifecycle::RuleStats::initial(),
        })),
    };

    let semantic_id = engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map_err(|e| format!("store semantic rule: {e}"))?;

    // Resolve source mistakes so they stop re-triggering and re-counting.
    let id_refs: Vec<&str> = source_ids.iter().map(|s| s.as_str()).collect();
    notebook
        .mark_resolved(&id_refs)
        .map_err(|e| format!("mark resolved: {e}"))?;

    Ok(Some(semantic_id))
}

/// Build a concise generalised rule from recurring mistakes (deterministic).
fn synthesize_rule(category: MistakeCategory, mistakes: &[MistakeEntry]) -> String {
    let mut lessons: Vec<String> = Vec::new();
    for m in mistakes {
        let lesson = m.what_went_wrong.trim();
        if !lesson.is_empty() && !lessons.iter().any(|l| l == lesson) {
            lessons.push(lesson.to_string());
        }
    }
    let bullets = lessons
        .iter()
        .take(5)
        .map(|l| format!("- {l}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Recurring {} issues consolidated from {} past mistakes. Apply extra care:\n{}",
        category.as_str(),
        mistakes.len(),
        bullets
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gvu::mistake_notebook::build_mistake_entry;
    use duduclaw_core::traits::MemoryEngine; // brings `search` into scope for assertions
    use tempfile::TempDir;

    fn record_n(nb: &MistakeNotebook, agent: &str, cat: MistakeCategory, n: usize) {
        for i in 0..n {
            let e = build_mistake_entry(
                agent,
                &format!("sess-{i}"),
                cat,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
            );
            nb.record(&e).unwrap();
        }
    }

    #[tokio::test]
    async fn below_threshold_does_not_consolidate() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-a", MistakeCategory::Capability, 2);

        let r = maybe_consolidate(&nb, &mem_path, "agent-a", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "2 < 3 must not consolidate");
        assert_eq!(nb.count_unresolved_by_category("agent-a", MistakeCategory::Capability), 2);
    }

    #[tokio::test]
    async fn threshold_reached_consolidates_to_semantic() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-b", MistakeCategory::Capability, 3);

        let r = maybe_consolidate(&nb, &mem_path, "agent-b", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_some(), "3 >= 3 must consolidate");

        // Source mistakes resolved → count drops to zero.
        assert_eq!(
            nb.count_unresolved_by_category("agent-b", MistakeCategory::Capability),
            0,
            "source mistakes must be marked resolved"
        );

        // A semantic memory rule now exists and is searchable.
        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let results = engine.search("agent-b", "Recurring", 10).await.unwrap();
        assert_eq!(results.len(), 1, "one consolidated semantic rule");
        assert_eq!(results[0].layer, MemoryLayer::Semantic);
        assert_eq!(results[0].source_event, "reflexion_consolidation");
    }

    #[tokio::test]
    async fn consolidated_rule_seeds_lifecycle_counters() {
        use crate::prediction::rule_lifecycle::RuleStats;

        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-d", MistakeCategory::Factual, 3);

        let semantic_id =
            maybe_consolidate(&nb, &mem_path, "agent-d", MistakeCategory::Factual, 3)
                .await
                .unwrap()
                .expect("must consolidate");

        let engine = SqliteMemoryEngine::new(&mem_path).unwrap();
        let meta = engine
            .get_metadata("agent-d", &semantic_id)
            .await
            .unwrap()
            .expect("rule metadata present");
        assert_eq!(
            RuleStats::from_metadata(&meta),
            RuleStats::initial(),
            "F2b must seed helpful=2, harmful=0 (ExpeL initial importance)"
        );
        // Source-mistake provenance still stored alongside the counters.
        assert!(meta["source_mistake_ids"].as_array().is_some_and(|a| a.len() == 3));
    }

    #[tokio::test]
    async fn different_categories_counted_separately() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-c", MistakeCategory::Capability, 2);
        record_n(&nb, "agent-c", MistakeCategory::Factual, 1);

        // Neither category reaches 3 → no consolidation.
        let r = maybe_consolidate(&nb, &mem_path, "agent-c", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());
    }
}
