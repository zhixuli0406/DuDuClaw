//! CRUD orchestration for playbook entries.
//!
//! Every persistence call in this file goes through `SqliteMemoryEngine`'s
//! existing public API (`list_valid_by_source_event`, `get_metadata`,
//! `update_metadata`, `update_content`, `store_temporal`,
//! `set_importance_and_add_tag`) — no new SQL is written here (§1.10
//! checklist). [`delta::merge`] itself stays pure; this module is where its
//! output actually gets written to disk.

use std::path::Path;

use chrono::{DateTime, Utc};

use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};

use super::delta::{self, AppliedOp, ExistingEntry, MergeOutcome, PlaybookDelta, ValidationCtx};
use super::entry::{
    PlaybookMeta, PlaybookState, LEGACY_RULE_SOURCE_EVENT, PLAYBOOK_SOURCE_EVENT,
};
use crate::prediction::rule_lifecycle::{RuleStats, PROBATION_RULE_TAG, RETIRED_RULE_TAG};

/// Candidate scan bound — mirrors `rule_lifecycle::CANDIDATE_SCAN_CAP` so
/// both selection paths see the same window.
pub(crate) use crate::prediction::rule_lifecycle::CANDIDATE_SCAN_CAP;

const RETIRED_IMPORTANCE: f64 = 1.0;
const NEW_ENTRY_IMPORTANCE: f64 = 8.0;

/// Fetch every currently-valid entry for `agent_id` across both source
/// events (new `playbook_entry` rows and legacy `reflexion_consolidation`
/// rows), deduped by id.
///
/// A row that has never carried the `playbook` metadata blob key is a
/// legacy F2b rule pre-dating WP1.2 — this function opportunistically
/// upgrades it in place (M-1/M-2 lazy migration: "第一次讀到 schema_version
/// 缺失的列時就地升級") by synthesizing a fail-open default from its tags and
/// persisting it, so subsequent reads see `schema_version = 1` without a
/// one-shot batch migration.
pub async fn list_active(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
) -> Vec<(MemoryEntry, PlaybookMeta, RuleStats)> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for source_event in [PLAYBOOK_SOURCE_EVENT, LEGACY_RULE_SOURCE_EVENT] {
        let rows = match engine
            .list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(agent = %agent_id, source_event, "playbook: list_active failed: {e}");
                continue;
            }
        };
        for (mem_entry, mut metadata) in rows {
            if !seen.insert(mem_entry.id.clone()) {
                continue;
            }
            let meta = match PlaybookMeta::from_metadata(&metadata) {
                Some(m) => m,
                None => {
                    let upgraded = PlaybookMeta::legacy_default(&mem_entry.tags, &mem_entry.content);
                    upgraded.merge_into(&mut metadata);
                    let _ = engine.update_metadata(agent_id, &mem_entry.id, &metadata).await;
                    upgraded
                }
            };
            let stats = RuleStats::from_metadata(&metadata);
            out.push((mem_entry, meta, stats));
        }
    }
    out
}

/// Validate + deterministically merge `deltas` against `agent_id`'s current
/// playbook, then persist every applied op through the engine's existing
/// API. `must_not` is the agent's `CONTRACT.toml` boundaries; `eval_cases_root`
/// is where `<suite>/*.toml` eval case files are discovered from (caller
/// decides — no hardcoded tree, e.g. `agent_dir.join("evals")`).
pub async fn apply_deltas(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    deltas: Vec<PlaybookDelta>,
    must_not: &[String],
    eval_cases_root: &Path,
    now: DateTime<Utc>,
) -> MergeOutcome {
    let current = list_active(engine, agent_id).await;
    let existing_wildcard_count = current
        .iter()
        .filter(|(_, meta, _)| matches!(meta.state, PlaybookState::Active | PlaybookState::Probation))
        .filter(|(_, meta, _)| meta.signals_match.len() == 1 && meta.signals_match[0] == "*")
        .count();

    let ctx = ValidationCtx { must_not, eval_cases_root, existing_wildcard_count };
    let (ok_deltas, mut rejected) = delta::validate_all(deltas, &ctx);

    let existing_view: Vec<ExistingEntry> = current
        .iter()
        .map(|(e, meta, stats)| ExistingEntry {
            id: e.id.clone(),
            content: e.content.clone(),
            meta: meta.clone(),
            stats: *stats,
        })
        .collect();

    let mut outcome = delta::merge(&existing_view, ok_deltas, now);
    outcome.rejected.append(&mut rejected);

    for op in &outcome.applied {
        match op {
            AppliedOp::Added { content, meta, stats, .. } => {
                let entry = MemoryEntry {
                    id: uuid::Uuid::new_v4().to_string(),
                    agent_id: agent_id.to_string(),
                    content: content.clone(),
                    timestamp: now,
                    tags: vec!["playbook".to_string(), PROBATION_RULE_TAG.to_string()],
                    embedding: None,
                    layer: MemoryLayer::Semantic,
                    importance: NEW_ENTRY_IMPORTANCE,
                    access_count: 0,
                    last_accessed: None,
                    source_event: PLAYBOOK_SOURCE_EVENT.to_string(),
                };
                let mut metadata = serde_json::json!({});
                meta.merge_into(&mut metadata);
                stats.merge_into(&mut metadata);
                let temporal = TemporalMeta {
                    confidence: Some(1.0),
                    origin: Some(meta.origin.clone()),
                    metadata: Some(metadata),
                    source_event: Some(PLAYBOOK_SOURCE_EVENT.to_string()),
                    ..Default::default()
                };
                if let Err(e) = engine.store_temporal(agent_id, entry, temporal).await {
                    tracing::warn!(agent = %agent_id, "playbook: store Add failed: {e}");
                }
            }
            AppliedOp::Revised { id, content, meta } => {
                let _ = engine.update_content(agent_id, id, content).await;
                if let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, id).await {
                    meta.merge_into(&mut metadata);
                    let _ = engine.update_metadata(agent_id, id, &metadata).await;
                }
            }
            AppliedOp::Linked { id, meta } | AppliedOp::Recorded { id, meta } => {
                if let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, id).await {
                    meta.merge_into(&mut metadata);
                    let _ = engine.update_metadata(agent_id, id, &metadata).await;
                }
            }
            AppliedOp::Retired { id, meta, .. } => {
                if let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, id).await {
                    meta.merge_into(&mut metadata);
                    let _ = engine.update_metadata(agent_id, id, &metadata).await;
                }
                let _ = engine
                    .set_importance_and_add_tag(agent_id, id, RETIRED_IMPORTANCE, RETIRED_RULE_TAG)
                    .await;
            }
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::entry::PlaybookCategory;
    use crate::playbook::gene::EvalCaseRef;

    fn temp_eval_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let suite = dir.path().join("s");
        std::fs::create_dir(&suite).unwrap();
        std::fs::write(
            suite.join("c.toml"),
            "[case]\nname = \"c\"\nagent = \"a\"\nprompt = \"hi\"\n[judge]\nrubric = \"r\"\n",
        )
        .unwrap();
        dir
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[tokio::test]
    async fn add_persists_and_is_independently_retirable() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let evals = temp_eval_root();
        let agent = "agent-store";

        let add_a = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "entry A content".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:capability".to_string()],
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            strategy: Vec::new(),
            rationale: "x".to_string(),
        };
        let add_b = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "entry B content".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:factual".to_string()],
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            strategy: Vec::new(),
            rationale: "x".to_string(),
        };
        let outcome = apply_deltas(&engine, agent, vec![add_a, add_b], &[], evals.path(), now()).await;
        assert_eq!(outcome.applied.len(), 2);
        assert!(outcome.rejected.is_empty());

        let active = list_active(&engine, agent).await;
        assert_eq!(active.len(), 2, "collapse immunity: two independent entries stored, not one blob");

        // Retiring one MUST NOT affect the other — independent retire.
        let target_id = active
            .iter()
            .find(|(e, _, _)| e.content == "entry A content")
            .unwrap()
            .0
            .id
            .clone();
        let retire = PlaybookDelta::Retire { id: target_id.clone(), reason: "done".to_string() };
        let outcome2 = apply_deltas(&engine, agent, vec![retire], &[], evals.path(), now()).await;
        assert_eq!(outcome2.applied.len(), 1);

        let remaining = list_active(&engine, agent).await;
        let entry_a = remaining.iter().find(|(e, _, _)| e.id == target_id);
        // Retired entries carry RETIRED_RULE_TAG; list_active doesn't filter
        // by tag (that's select.rs's job), so we assert state directly.
        let (_, meta_a, _) = entry_a.expect("row still readable, now retired");
        assert_eq!(meta_a.state, PlaybookState::Retired);
        let entry_b = remaining.iter().find(|(e, _, _)| e.content == "entry B content").unwrap();
        assert_eq!(entry_b.1.state, PlaybookState::Probation, "unrelated entry untouched");
    }

    #[tokio::test]
    async fn add_without_eval_case_never_reaches_storage() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let evals = temp_eval_root();
        let agent = "agent-g6";

        let bad_add = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "no case linked".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:capability".to_string()],
            eval_cases: Vec::new(),
            strategy: Vec::new(),
            rationale: "x".to_string(),
        };
        let outcome = apply_deltas(&engine, agent, vec![bad_add], &[], evals.path(), now()).await;
        assert!(outcome.applied.is_empty());
        assert_eq!(outcome.rejected.len(), 1);
        assert!(outcome.rejected[0].1.contains("eval case"));

        let active = list_active(&engine, agent).await;
        assert!(active.is_empty(), "rejected delta must never be persisted");
    }

    #[tokio::test]
    async fn revising_one_entry_never_touches_another_no_whole_playbook_rewrite() {
        // Structural / behavioral guard (§1.11 "no_code_path_rewrites_whole_playbook"):
        // `apply_deltas` takes a `Vec<PlaybookDelta>` of independent,
        // individually-targeted ops — there is no "replace the whole
        // playbook" function anywhere in this module. Proven behaviorally: a
        // `Revise` on one entry must leave a second, unrelated entry's
        // content and revision counter completely untouched.
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let evals = temp_eval_root();
        let agent = "agent-no-collapse";

        let add_a = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "entry A original".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:capability".to_string()],
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            strategy: Vec::new(),
            rationale: "x".to_string(),
        };
        let add_b = PlaybookDelta::Add {
            assertions: crate::playbook::entry::EntryAssertions { output_contains: vec!["ok".to_string()], ..Default::default() },
            content: "entry B original".to_string(),
            category: PlaybookCategory::Repair,
            signals_match: vec!["mistake:factual".to_string()],
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            strategy: Vec::new(),
            rationale: "x".to_string(),
        };
        apply_deltas(&engine, agent, vec![add_a, add_b], &[], evals.path(), now()).await;
        let before = list_active(&engine, agent).await;
        let id_a = before.iter().find(|(e, _, _)| e.content == "entry A original").unwrap().0.id.clone();
        let id_b = before.iter().find(|(e, _, _)| e.content == "entry B original").unwrap().0.id.clone();

        let revise_a = PlaybookDelta::Revise {
            id: id_a.clone(),
            content: "entry A revised content".to_string(),
            rationale: "fix".to_string(),
        };
        apply_deltas(&engine, agent, vec![revise_a], &[], evals.path(), now()).await;

        let after = list_active(&engine, agent).await;
        let entry_a = after.iter().find(|(e, _, _)| e.id == id_a).unwrap();
        assert_eq!(entry_a.0.content, "entry A revised content");
        assert_eq!(entry_a.1.revision, 1);

        let entry_b = after.iter().find(|(e, _, _)| e.id == id_b).unwrap();
        assert_eq!(entry_b.0.content, "entry B original", "unrelated entry's content untouched");
        assert_eq!(entry_b.1.revision, 0, "unrelated entry's revision counter untouched");
    }

    #[tokio::test]
    async fn legacy_reflexion_row_is_upgraded_in_place_on_read() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-legacy";

        // Simulate a pre-WP1.2 F2b row: no `playbook` metadata key.
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "legacy consolidated rule".to_string(),
            timestamp: Utc::now(),
            tags: vec!["reflexion".to_string(), "consolidated".to_string(), PROBATION_RULE_TAG.to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: LEGACY_RULE_SOURCE_EVENT.to_string(),
        };
        let meta = TemporalMeta {
            metadata: Some(serde_json::json!({"rule_stats": {"helpful": 1, "harmful": 0}})),
            ..Default::default()
        };
        let id = engine.store_temporal(agent, entry, meta).await.unwrap();

        let active = list_active(&engine, agent).await;
        let (_, meta, stats) = active.iter().find(|(e, _, _)| e.id == id).unwrap();
        assert_eq!(meta.state, PlaybookState::Probation, "derived from PROBATION_RULE_TAG");
        assert_eq!(meta.signals_match, vec!["*".to_string()]);
        assert_eq!(*stats, RuleStats { helpful: 1, harmful: 0 }, "rule_stats sibling key preserved");

        // Second read must see the now-persisted schema_version = 1 blob
        // directly via `from_metadata` (no re-upgrade needed).
        let raw = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        assert_eq!(PlaybookMeta::raw_schema_version(&raw), Some(super::super::entry::PLAYBOOK_SCHEMA_VERSION));
    }
}
