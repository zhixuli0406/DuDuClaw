//! G6 — rule-layer source-fact staleness (Hindsight #6/#7 parity).
//!
//! DuDuClaw already does F1 temporal supersession at the **fact** layer:
//! writing a newer `(subject, predicate, object)` fact expires the prior one
//! (`valid_until` set, `superseded_by` chained — see
//! `SqliteMemoryEngine::store_temporal`). What it did NOT do is propagate that
//! signal **up** to the consolidated rules that were built on top of those
//! facts. Hindsight's Mental Models record which raw facts generated them and
//! flag the model as "behind" when a source fact is later replaced, rather
//! than silently keep returning stale guidance. This module is the equivalent
//! layer for DuDuClaw's reflexion-consolidated rules / playbook entries / task
//! rules (all three live on `MemoryLayer::Semantic` and share the
//! `prediction::rule_lifecycle` machinery — this reuses that exact metadata
//! row, no new store).
//!
//! Contract:
//! - **Record** ([`record_source_facts`]): a rule may record, in its metadata
//!   `source_facts` key, the memory ids of the F1 facts it was derived from.
//!   Writers that have no fact source (e.g. `reflexion.rs`, which consolidates
//!   from `MistakeNotebook` entries, not F1 facts) simply record nothing.
//! - **Detect + mark** ([`refresh_rule_source_staleness`]): for every rule
//!   carrying a non-empty `source_facts`, ask the memory engine which of those
//!   source ids are now superseded ([`SqliteMemoryEngine::superseded_fact_ids`]).
//!   If ≥1 is, the rule is stamped [`SOURCE_STALE_RULE_TAG`] + a
//!   `source_staleness` metadata detail (which source ids, when).
//! - **Query** ([`list_source_stale_rules`]): enumerate the agent's currently
//!   source-stale rules for the dashboard "refresh this outdated rule" flow
//!   (Hindsight #6) — the rule itself is still valid (it is the *source* fact
//!   that was superseded, not the rule), so it stays in the valid-row scan.
//! - **Fail-open** (the critical invariant): a rule with NO recorded
//!   `source_facts` is never flagged stale — the overwhelming majority of
//!   existing rules (mistake-derived, no F1 fact source) must keep behaving
//!   exactly as before. A recorded source id that cannot be located is also
//!   not treated as stale (the memory-side query omits it).
//!
//! Injection downweighting + the user-facing 「來源已更新」 marker live in
//! `crate::playbook::select` (the `## Learned Rules` render path) and
//! `rule_lifecycle::select_rules`, which read [`SOURCE_STALE_RULE_TAG`].

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use duduclaw_core::types::MemoryEntry;
use duduclaw_memory::SqliteMemoryEngine;

use crate::playbook::entry::PLAYBOOK_SOURCE_EVENT;
use crate::prediction::rule_lifecycle::{
    CANDIDATE_SCAN_CAP, RETIRED_RULE_TAG, RULE_SOURCE_EVENT, TASK_RULE_SOURCE_EVENT,
};

/// Tag stamped on a rule once at least one of its recorded source facts has
/// been superseded. Read by the injection selectors to downweight + annotate.
pub const SOURCE_STALE_RULE_TAG: &str = "source-stale";

/// Metadata key holding the memory ids of the F1 facts a rule was derived
/// from. Absent / empty ⇒ fail-open (never flagged stale).
pub const SOURCE_FACTS_KEY: &str = "source_facts";

/// Metadata key holding the [`SourceStaleness`] detection detail.
pub const SOURCE_STALENESS_KEY: &str = "source_staleness";

/// All three `source_event` families that carry rule-lifecycle rows. Scanned
/// together so a source-fact supersession reaches reflexion (F2b), playbook,
/// and task-layer rules alike. `LEGACY_RULE_SOURCE_EVENT` == [`RULE_SOURCE_EVENT`]
/// (both `"reflexion_consolidation"`), so only these three distinct strings
/// are needed.
const RULE_SOURCE_EVENTS: [&str; 3] =
    [RULE_SOURCE_EVENT, PLAYBOOK_SOURCE_EVENT, TASK_RULE_SOURCE_EVENT];

/// Persisted detail of why a rule is source-stale.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceStaleness {
    /// Source-fact memory ids that were found superseded at detection time.
    pub superseded: Vec<String>,
    /// RFC3339 detection timestamp.
    pub detected_at: String,
}

impl SourceStaleness {
    pub fn from_metadata(metadata: &Value) -> Self {
        metadata
            .get(SOURCE_STALENESS_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Write `self` into a metadata object, preserving sibling keys.
    pub fn merge_into(&self, metadata: &mut Value) {
        if !metadata.is_object() {
            *metadata = json!({});
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                SOURCE_STALENESS_KEY.to_string(),
                serde_json::to_value(self).unwrap_or_else(|_| json!({})),
            );
        }
    }
}

/// A source-stale rule, for the dashboard "refresh outdated rule" query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleRuleInfo {
    pub id: String,
    pub content: String,
    pub source_event: String,
    pub superseded_source_ids: Vec<String>,
    pub detected_at: String,
}

/// Record the memory ids of the F1 facts a rule was derived from into its
/// metadata (deduped, order-preserving). Empty input is a no-op — the key is
/// NOT written, so `source_facts_from_metadata` reads back empty and the rule
/// stays fail-open. Blank ids are dropped.
pub fn record_source_facts(metadata: &mut Value, fact_ids: &[String]) {
    let mut seen = HashSet::new();
    let deduped: Vec<String> = fact_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    if deduped.is_empty() {
        return;
    }
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert(SOURCE_FACTS_KEY.to_string(), json!(deduped));
    }
}

/// Read a rule's recorded source-fact ids. Missing/malformed ⇒ empty (fail-open).
pub fn source_facts_from_metadata(metadata: &Value) -> Vec<String> {
    metadata
        .get(SOURCE_FACTS_KEY)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Whether an entry's tags mark it source-stale.
pub fn is_source_stale(tags: &[String]) -> bool {
    tags.iter().any(|t| t == SOURCE_STALE_RULE_TAG)
}

/// Check one already-fetched rule row and, if any recorded source fact is now
/// superseded, persist the stale marker (tag + `source_staleness` detail).
///
/// Fail-open: returns `None` (not stale, nothing written) when the rule has no
/// recorded `source_facts`, when none of them are superseded, or when the
/// engine query errors. Idempotent: re-detecting the identical superseded set
/// on an already-tagged rule writes nothing.
async fn mark_if_stale(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    entry: &MemoryEntry,
    metadata: &Value,
) -> Option<Vec<String>> {
    let facts = source_facts_from_metadata(metadata);
    if facts.is_empty() {
        return None; // fail-open: no recorded source → never stale
    }
    let superseded = match engine.superseded_fact_ids(agent_id, &facts).await {
        Ok(s) => s,
        Err(e) => {
            warn!(agent = %agent_id, rule = %entry.id, "rule staleness: superseded query failed: {e}");
            return None; // fail-open on error
        }
    };
    if superseded.is_empty() {
        return None;
    }

    let already_tagged = is_source_stale(&entry.tags);
    let stored = SourceStaleness::from_metadata(metadata);
    if already_tagged && stored.superseded == superseded {
        return Some(superseded); // nothing changed — no write
    }

    let detail = SourceStaleness {
        superseded: superseded.clone(),
        detected_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut new_metadata = metadata.clone();
    detail.merge_into(&mut new_metadata);
    if let Err(e) = engine.update_metadata(agent_id, &entry.id, &new_metadata).await {
        warn!(agent = %agent_id, rule = %entry.id, "rule staleness: write metadata failed: {e}");
        return None;
    }
    // Preserve the row's current importance while adding the tag.
    if let Err(e) = engine
        .set_importance_and_add_tag(agent_id, &entry.id, entry.importance, SOURCE_STALE_RULE_TAG)
        .await
    {
        warn!(agent = %agent_id, rule = %entry.id, "rule staleness: add tag failed: {e}");
    }
    Some(superseded)
}

/// Scan an agent's consolidated rules (reflexion / playbook / task) and mark
/// every one whose recorded source facts have been superseded. Returns the ids
/// found (newly-or-already) source-stale this pass. Errors on any single
/// source-event scan degrade to skipping that scan (never a hard failure) —
/// staleness detection is an enhancement, never a blocker.
pub async fn refresh_rule_source_staleness(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut stale = Vec::new();
    for source_event in RULE_SOURCE_EVENTS {
        let rows = match engine
            .list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(agent = %agent_id, source_event, "rule staleness: list failed: {e}");
                continue;
            }
        };
        for (entry, metadata) in rows {
            if !seen.insert(entry.id.clone()) {
                continue;
            }
            // A retired rule is out of selection anyway — don't spend a query.
            if entry.tags.iter().any(|t| t == RETIRED_RULE_TAG) {
                continue;
            }
            if mark_if_stale(engine, agent_id, &entry, &metadata).await.is_some() {
                stale.push(entry.id);
            }
        }
    }
    stale
}

/// Enumerate the agent's currently source-stale rules (carrying
/// [`SOURCE_STALE_RULE_TAG`]) with their supersession detail. Read-only —
/// does NOT run detection (call [`refresh_rule_source_staleness`] for that).
/// Retired rules are excluded. Errors degrade to a partial/empty list.
pub async fn list_source_stale_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
) -> Vec<StaleRuleInfo> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for source_event in RULE_SOURCE_EVENTS {
        let rows = match engine
            .list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(agent = %agent_id, source_event, "rule staleness: list (query) failed: {e}");
                continue;
            }
        };
        for (entry, metadata) in rows {
            if !seen.insert(entry.id.clone()) {
                continue;
            }
            if entry.tags.iter().any(|t| t == RETIRED_RULE_TAG) {
                continue;
            }
            if !is_source_stale(&entry.tags) {
                continue;
            }
            let detail = SourceStaleness::from_metadata(&metadata);
            out.push(StaleRuleInfo {
                id: entry.id,
                content: entry.content,
                source_event: source_event.to_string(),
                superseded_source_ids: detail.superseded,
                detected_at: detail.detected_at,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};
    use duduclaw_memory::TemporalMeta;

    fn fact_meta(subject: &str, predicate: &str, object: &str) -> TemporalMeta {
        TemporalMeta {
            subject: Some(subject.to_string()),
            predicate: Some(predicate.to_string()),
            object: Some(object.to_string()),
            ..Default::default()
        }
    }

    /// Store an F1 fact, return its memory id.
    async fn store_fact(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "test_fact".to_string(),
        };
        engine.store_temporal(agent, entry, fact_meta(subject, predicate, object)).await.unwrap()
    }

    /// Store a consolidated rule with an optional recorded `source_facts` list.
    async fn store_rule(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        source_facts: &[String],
    ) -> String {
        let mut metadata = json!({ "rule_stats": { "helpful": 1, "harmful": 0 } });
        record_source_facts(&mut metadata, source_facts);
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec!["reflexion".to_string(), "consolidated".to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: RULE_SOURCE_EVENT.to_string(),
        };
        // NOTE: no (subject, predicate) triple — a consolidated rule must not
        // itself enter the F1 supersession chain via this test path.
        let meta = TemporalMeta { metadata: Some(metadata), ..Default::default() };
        engine.store_temporal(agent, entry, meta).await.unwrap()
    }

    #[test]
    fn record_source_facts_empty_is_noop_and_dedups() {
        let mut m = json!({ "rule_stats": { "helpful": 1 } });
        record_source_facts(&mut m, &[]);
        assert!(m.get(SOURCE_FACTS_KEY).is_none(), "empty input must not write the key (fail-open)");
        assert!(source_facts_from_metadata(&m).is_empty());

        record_source_facts(&mut m, &["a".into(), "a".into(), " ".into(), "b".into()]);
        assert_eq!(source_facts_from_metadata(&m), vec!["a".to_string(), "b".to_string()]);
        // Sibling key preserved.
        assert_eq!(m["rule_stats"]["helpful"], 1);
    }

    #[tokio::test]
    async fn superseded_source_marks_rule_stale_and_lists_it() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "stale-agent";

        // Source fact the rule depends on.
        let fact = store_fact(&engine, agent, "price is 100", "product:price", "is", "100").await;
        let rule = store_rule(&engine, agent, "when asked price, answer 100", &[fact.clone()]).await;

        // Not stale yet — source is still valid.
        assert!(refresh_rule_source_staleness(&engine, agent).await.is_empty());
        assert!(list_source_stale_rules(&engine, agent).await.is_empty());

        // Supersede the source fact.
        store_fact(&engine, agent, "price is 120", "product:price", "is", "120").await;

        let stale = refresh_rule_source_staleness(&engine, agent).await;
        assert_eq!(stale, vec![rule.clone()], "rule whose source was superseded is flagged");

        // Tag + detail persisted; query returns it.
        let entry = engine.get_by_id(agent, &rule).await.unwrap().unwrap();
        assert!(is_source_stale(&entry.tags));
        let listed = list_source_stale_rules(&engine, agent).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, rule);
        assert_eq!(listed[0].superseded_source_ids, vec![fact]);
        assert!(!listed[0].detected_at.is_empty());
    }

    #[tokio::test]
    async fn fail_open_when_no_source_facts_recorded() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "failopen-agent";

        // Rule with NO recorded source facts (the overwhelming majority case).
        let rule = store_rule(&engine, agent, "generic rule", &[]).await;
        // A totally unrelated fact gets superseded in the same agent.
        let f = store_fact(&engine, agent, "v1", "k", "is", "1").await;
        store_fact(&engine, agent, "v2", "k", "is", "2").await;
        assert_eq!(engine.superseded_fact_ids(agent, &[f]).await.unwrap().len(), 1);

        let stale = refresh_rule_source_staleness(&engine, agent).await;
        assert!(stale.is_empty(), "a rule with no recorded source facts must never be flagged stale");
        let entry = engine.get_by_id(agent, &rule).await.unwrap().unwrap();
        assert!(!is_source_stale(&entry.tags));
    }

    #[tokio::test]
    async fn not_stale_while_source_remains_valid() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "valid-agent";
        let fact = store_fact(&engine, agent, "still true", "topic", "state", "ok").await;
        let rule = store_rule(&engine, agent, "rely on topic=ok", &[fact]).await;

        let stale = refresh_rule_source_staleness(&engine, agent).await;
        assert!(stale.is_empty(), "source still valid → rule not stale");
        let entry = engine.get_by_id(agent, &rule).await.unwrap().unwrap();
        assert!(!is_source_stale(&entry.tags));
    }

    #[tokio::test]
    async fn detection_is_idempotent_no_duplicate_tag() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "idem-agent";
        let fact = store_fact(&engine, agent, "a", "s", "is", "a").await;
        let rule = store_rule(&engine, agent, "rule on s", &[fact]).await;
        store_fact(&engine, agent, "b", "s", "is", "b").await; // supersede

        refresh_rule_source_staleness(&engine, agent).await;
        refresh_rule_source_staleness(&engine, agent).await; // second pass
        let entry = engine.get_by_id(agent, &rule).await.unwrap().unwrap();
        let count = entry.tags.iter().filter(|t| *t == SOURCE_STALE_RULE_TAG).count();
        assert_eq!(count, 1, "the stale tag must not be duplicated across passes");
    }
}
