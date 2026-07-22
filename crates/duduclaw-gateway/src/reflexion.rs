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

use std::collections::BTreeMap;
use std::path::Path;

use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};

use crate::gvu::mistake_notebook::{
    MistakeCategory, MistakeEntry, MistakeNotebook, MAX_UNRESOLVED_PER_AGENT,
};
use crate::prediction::rule_lifecycle::PROBATION_RULE_TAG;

/// Default number of same-category unresolved mistakes that triggers consolidation.
pub const DEFAULT_CONSOLIDATE_THRESHOLD: u32 = 3;

/// GovMem-style (arXiv:2607.02579) promotion verdict for a candidate group of
/// same-category, same-`source_kind` mistakes.
///
/// Deterministic, zero LLM cost — see [`assess_promotion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promotion {
    /// Independent evidence is sufficient — consolidate this group.
    Promote,
    /// All observations trace back to the same session and/or the same
    /// wording — correlated, not independent, evidence. Wait for more
    /// observations rather than consolidating on a single incident.
    NeedsMoreEvidence,
}

/// Decide whether a candidate group of mistakes (already filtered to one
/// `category` + one `source_kind`, already at/above the count threshold)
/// carries enough *independent* evidence to promote into a consolidated rule.
///
/// GovMem's failure mode this guards against: a single incident that just
/// happens to re-trigger the same mistake 3+ times within one session (or
/// gets logged with byte-identical wording) is one correlated observation,
/// not three independent ones. Promotion requires:
/// - distinct `session_id` count >= 2, AND
/// - distinct normalized `what_went_wrong` (trimmed, lowercased, whitespace
///   collapsed) count >= 2.
///
/// Pure function — no I/O, no LLM call.
pub fn assess_promotion(mistakes: &[MistakeEntry]) -> Promotion {
    let mut sessions: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut lessons: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in mistakes {
        sessions.insert(m.session_id.as_str());
        let normalized = normalize_lesson(&m.what_went_wrong);
        if !normalized.is_empty() {
            lessons.insert(normalized);
        }
    }
    if sessions.len() >= 2 && lessons.len() >= 2 {
        Promotion::Promote
    } else {
        Promotion::NeedsMoreEvidence
    }
}

/// Normalize `what_went_wrong` for de-duplication: trim, lowercase, collapse
/// internal whitespace runs to a single space.
fn normalize_lesson(s: &str) -> String {
    s.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Consolidate recurring mistakes of `category` into a semantic memory rule.
///
/// WP2 (GovMem 2607.02579): mistakes are first grouped by `source_kind` — an
/// orthogonal axis to `category` recording *how* the failure was detected
/// (e.g. RFC-24 `"decision_gap"` vs. general `"task_failure"`, both of which
/// may land in `MistakeCategory::Capability`). Each group is counted and
/// evaluated independently so unrelated failure modes never pool into one
/// consolidation, and a group below `threshold` never blocks a different
/// group that has reached it. Groups are visited in deterministic
/// (lexicographic `source_kind`) order; the first eligible group — reaching
/// `threshold` AND assessed [`Promotion::Promote`] by [`assess_promotion`] —
/// is consolidated and returned. Remaining eligible groups (if any) will be
/// picked up by a subsequent call (this function already runs after every
/// qualifying mistake record, so no evidence is lost — it's just spread
/// across turns).
///
/// Returns `Ok(Some(semantic_id))` when a consolidation happened, `Ok(None)`
/// when no group reached `threshold`, or every group that did was assessed
/// [`Promotion::NeedsMoreEvidence`] (correlated, not independent, evidence —
/// left unresolved; the notebook's existing FIFO cap bounds accumulation).
pub async fn maybe_consolidate(
    notebook: &MistakeNotebook,
    memory_db_path: &Path,
    agent_id: &str,
    category: MistakeCategory,
    threshold: u32,
) -> Result<Option<String>, String> {
    let total = notebook.count_unresolved_by_category(agent_id, category);
    if total < threshold {
        // No sub-group can reach `threshold` if the total doesn't either.
        return Ok(None);
    }

    let mistakes = notebook.query_unresolved_by_category(
        agent_id,
        category,
        MAX_UNRESOLVED_PER_AGENT as usize,
    );

    // Group by source_kind (WP2). Empty string ("" — unattributed / legacy
    // rows) is its own group rather than joining a named one, so it can
    // neither pad out `"decision_gap"`/`"task_failure"` counts nor be
    // silently dropped. BTreeMap gives deterministic iteration order.
    let mut groups: BTreeMap<String, Vec<MistakeEntry>> = BTreeMap::new();
    for m in mistakes {
        groups.entry(m.source_kind.clone()).or_default().push(m);
    }

    for group in groups.into_values() {
        if (group.len() as u32) < threshold {
            continue;
        }
        if assess_promotion(&group) != Promotion::Promote {
            continue;
        }
        return consolidate_group(notebook, memory_db_path, agent_id, category, group).await;
    }

    Ok(None)
}

/// Synthesize + store a semantic rule from one already-eligible group of
/// mistakes, then mark that group's source mistakes resolved. Split out of
/// `maybe_consolidate` so the grouping/eligibility logic above stays
/// readable.
async fn consolidate_group(
    notebook: &MistakeNotebook,
    memory_db_path: &Path,
    agent_id: &str,
    category: MistakeCategory,
    mistakes: Vec<MistakeEntry>,
) -> Result<Option<String>, String> {
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
            // WP2 Janus (arXiv:2606.31121): every freshly consolidated rule
            // starts on a trial period — see `prediction::rule_lifecycle`.
            PROBATION_RULE_TAG.to_string(),
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
        // WP1: a consolidated reflexion rule is agent self-derived content.
        origin: Some("agent_derived".to_string()),
        // `rule_stats` seeds the Janus lifecycle counters (WP2: initial
        // helpful = 1, on probation) settled per-turn by
        // `prediction::rule_lifecycle`.
        metadata: Some(serde_json::json!({
            "source_mistake_ids": source_ids,
            "rule_stats": crate::prediction::rule_lifecycle::RuleStats::initial(),
        })),
        ..Default::default()
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

    /// Record `n` mistakes with distinct session ids and distinct wording —
    /// independent evidence that should promote once `n >= threshold`.
    fn record_n(nb: &MistakeNotebook, agent: &str, cat: MistakeCategory, n: usize, source_kind: &str) {
        for i in 0..n {
            let e = build_mistake_entry(
                agent,
                &format!("sess-{i}"),
                cat,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
                source_kind,
            );
            nb.record(&e).unwrap();
        }
    }

    /// Record `n` mistakes that all share the same session id (correlated
    /// observations from one incident, not independent evidence).
    fn record_same_session_n(
        nb: &MistakeNotebook,
        agent: &str,
        cat: MistakeCategory,
        n: usize,
        source_kind: &str,
    ) {
        for i in 0..n {
            let e = build_mistake_entry(
                agent,
                "sess-fixed",
                cat,
                &format!("user asked thing {i}"),
                "agent answered wrong",
                &format!("missed validation step {i}"),
                None,
                source_kind,
            );
            nb.record(&e).unwrap();
        }
    }

    #[tokio::test]
    async fn below_threshold_does_not_consolidate() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-a", MistakeCategory::Capability, 2, "");

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
        record_n(&nb, "agent-b", MistakeCategory::Capability, 3, "");

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
        record_n(&nb, "agent-d", MistakeCategory::Factual, 3, "");

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
            "F2b must seed helpful=1, harmful=0 (WP2 Janus trial-period seed)"
        );
        // Source-mistake provenance still stored alongside the counters.
        assert!(meta["source_mistake_ids"].as_array().is_some_and(|a| a.len() == 3));
        // WP2 Janus: every freshly consolidated rule starts on probation.
        let entry = engine.get_by_id("agent-d", &semantic_id).await.unwrap().unwrap();
        assert!(entry
            .tags
            .iter()
            .any(|t| t == crate::prediction::rule_lifecycle::PROBATION_RULE_TAG));
    }

    #[tokio::test]
    async fn different_categories_counted_separately() {
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-c", MistakeCategory::Capability, 2, "");
        record_n(&nb, "agent-c", MistakeCategory::Factual, 1, "");

        // Neither category reaches 3 → no consolidation.
        let r = maybe_consolidate(&nb, &mem_path, "agent-c", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn correlated_same_session_mistakes_do_not_consolidate() {
        // GovMem: 3 mistakes that all trace back to the same session are one
        // correlated incident, not three independent observations.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_same_session_n(&nb, "agent-corr", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, "agent-corr", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_none(), "same-session mistakes are correlated, not independent, evidence");

        // Left unresolved (NeedsMoreEvidence), not silently dropped — still
        // counted as unresolved so a genuinely independent 4th observation
        // can still tip it over.
        assert_eq!(
            nb.count_unresolved_by_category("agent-corr", MistakeCategory::Capability),
            3,
            "NeedsMoreEvidence must not mark_resolved the source mistakes"
        );
    }

    #[tokio::test]
    async fn distinct_sessions_promote() {
        // Mirror of `correlated_same_session_mistakes_do_not_consolidate`:
        // same count and category, but distinct sessions + distinct wording
        // — GovMem's independence bar is met, so this must promote.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-indep", MistakeCategory::Capability, 3, "");

        let r = maybe_consolidate(&nb, &mem_path, "agent-indep", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(r.is_some(), "distinct sessions + distinct wording must promote");
        assert_eq!(
            nb.count_unresolved_by_category("agent-indep", MistakeCategory::Capability),
            0,
            "promoted group's source mistakes must be resolved"
        );
    }

    #[tokio::test]
    async fn decision_gap_and_task_failure_counted_separately() {
        // WP2: source_kind groups are counted independently — 2 decision_gap
        // + 2 task_failure mistakes total 4 (>= threshold 3 in aggregate),
        // but neither group alone reaches the threshold, so neither promotes.
        let dir = TempDir::new().unwrap();
        let nb = MistakeNotebook::new(&dir.path().join("mistakes.db"));
        let mem_path = dir.path().join("memory.db");
        record_n(&nb, "agent-split", MistakeCategory::Capability, 2, "decision_gap");
        record_n(&nb, "agent-split", MistakeCategory::Capability, 2, "task_failure");

        assert_eq!(
            nb.count_unresolved_by_category("agent-split", MistakeCategory::Capability),
            4,
            "total unresolved count spans both source_kind groups"
        );

        let r = maybe_consolidate(&nb, &mem_path, "agent-split", MistakeCategory::Capability, 3)
            .await
            .unwrap();
        assert!(
            r.is_none(),
            "neither source_kind group individually reaches the threshold — must not pool"
        );
        assert_eq!(
            nb.count_unresolved_by_category("agent-split", MistakeCategory::Capability),
            4,
            "nothing resolved — both groups still below threshold"
        );
    }
}
