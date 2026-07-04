//! ACE/ExpeL-style lifecycle counters for consolidated reflexion rules.
//!
//! Problem (ACE arXiv:2510.04618 "context collapse"; ExpeL arXiv:2308.10144):
//! F2b-synthesized rules only accumulate — nothing ever demotes or retires a
//! rule that stops paying rent, so the injected context degrades over time.
//!
//! This module gives every consolidated rule (semantic memory entry with
//! `source_event = "reflexion_consolidation"`) a helpful/harmful counter pair
//! stored in the entry's metadata JSON (`{"rule_stats":{...}}`, no schema
//! migration). The lifecycle is fully deterministic (zero LLM cost):
//!
//! 1. F2b seeds new rules with `helpful = 2` (ExpeL initial importance).
//! 2. F2a injection selects non-retired rules ranked by net score
//!    (`helpful − harmful`, descending) and records the injected ids.
//! 3. At prediction-outcome settlement the turn's final [`ErrorCategory`]
//!    credits (Negligible/Moderate → `helpful += 1`) or blames
//!    (Significant/Critical → `harmful += 1`) each injected rule.
//! 4. When `helpful.saturating_sub(harmful) == 0` the rule is retired:
//!    importance dropped and tagged [`RETIRED_RULE_TAG`] so F2a skips it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_memory::SqliteMemoryEngine;

use super::engine::ErrorCategory;

/// `source_event` written by `reflexion::maybe_consolidate` (F2b).
pub const RULE_SOURCE_EVENT: &str = "reflexion_consolidation";
/// Tag appended on retirement; F2a selection excludes entries carrying it.
pub const RETIRED_RULE_TAG: &str = "retired-rule";
/// Max rules injected per turn — mirrors the F2a mistake-injection cap.
pub const INJECTION_LIMIT: usize = 3;
/// ExpeL: new rules start with importance 2 → `helpful = 2, harmful = 0`.
const INITIAL_HELPFUL: u32 = 2;
/// Importance assigned on retirement (fresh rules are stored at 8.0).
const RETIRED_IMPORTANCE: f64 = 1.0;
/// Metadata JSON key holding the counters.
const STATS_KEY: &str = "rule_stats";
/// Candidate scan bound before ranking — keeps per-turn work O(1)-ish even
/// for agents that accumulated many rules over months.
const CANDIDATE_SCAN_CAP: usize = 50;

/// Helpful/harmful lifecycle counters persisted in entry metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RuleStats {
    pub helpful: u32,
    pub harmful: u32,
}

impl RuleStats {
    /// Counters for a freshly consolidated rule (ExpeL initial importance = 2).
    pub fn initial() -> Self {
        Self { helpful: INITIAL_HELPFUL, harmful: 0 }
    }

    /// Signed net score used for injection ranking.
    pub fn net(&self) -> i64 {
        i64::from(self.helpful) - i64::from(self.harmful)
    }

    /// Credit or blame this rule based on the turn's settled error category.
    pub fn record(&mut self, category: ErrorCategory) {
        match category {
            ErrorCategory::Negligible | ErrorCategory::Moderate => {
                self.helpful = self.helpful.saturating_add(1);
            }
            ErrorCategory::Significant | ErrorCategory::Critical => {
                self.harmful = self.harmful.saturating_add(1);
            }
        }
    }

    /// Retirement condition: harmful outcomes have consumed all credit.
    pub fn is_spent(&self) -> bool {
        self.helpful.saturating_sub(self.harmful) == 0
    }

    /// Parse from an entry's metadata blob; missing/malformed → zeroed stats.
    pub fn from_metadata(metadata: &serde_json::Value) -> Self {
        metadata
            .get(STATS_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Write these counters back into a metadata object, preserving all
    /// sibling keys (e.g. `source_mistake_ids`).
    pub fn merge_into(&self, metadata: &mut serde_json::Value) {
        if !metadata.is_object() {
            *metadata = serde_json::json!({});
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                STATS_KEY.to_string(),
                serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({})),
            );
        }
    }
}

/// A consolidated rule selected for prompt injection this turn.
#[derive(Debug, Clone)]
pub struct InjectedRule {
    pub id: String,
    pub content: String,
    pub net: i64,
}

/// Select active (non-retired) consolidated rules for F2a injection, ranked
/// by net score descending, capped at `limit`.
///
/// Ties keep newest-first order (the underlying listing is newest-first and
/// the sort is stable). Errors degrade to an empty selection — rule injection
/// is an enhancement, never a reply blocker.
pub async fn select_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    limit: usize,
) -> Vec<InjectedRule> {
    let rows = match engine
        .list_valid_by_source_event(agent_id, RULE_SOURCE_EVENT, CANDIDATE_SCAN_CAP)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(agent = %agent_id, "rule lifecycle: list rules failed: {e}");
            return Vec::new();
        }
    };

    let mut rules: Vec<InjectedRule> = rows
        .into_iter()
        .filter(|(entry, _)| !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG))
        .map(|(entry, metadata)| InjectedRule {
            net: RuleStats::from_metadata(&metadata).net(),
            id: entry.id,
            content: entry.content,
        })
        .collect();
    rules.sort_by(|a, b| b.net.cmp(&a.net));
    rules.truncate(limit);
    rules
}

/// Blocking helper for the channel-reply prompt-assembly path (spawn_blocking).
///
/// Returns the prompt section text plus the injected rule ids, or `None` when
/// the agent has no active rules (or the engine cannot be opened).
pub fn build_rules_section_blocking(
    db_path: &Path,
    agent_id: &str,
    limit: usize,
) -> Option<(String, Vec<String>)> {
    let engine = SqliteMemoryEngine::new(db_path).ok()?;
    let rt = tokio::runtime::Handle::current();
    let rules = rt.block_on(select_rules(&engine, agent_id, limit));
    if rules.is_empty() {
        return None;
    }
    let ids: Vec<String> = rules.iter().map(|r| r.id.clone()).collect();
    let section = rules
        .iter()
        .map(|r| r.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    Some((section, ids))
}

/// Settle lifecycle counters for the rules injected into a finished turn.
///
/// For each rule id: bump helpful/harmful per `category`, persist the merged
/// metadata, and retire the rule (low importance + [`RETIRED_RULE_TAG`]) when
/// its credit is spent. Returns the ids retired by this settlement.
pub async fn settle_injected_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    rule_ids: &[String],
    category: ErrorCategory,
) -> Vec<String> {
    let mut retired = Vec::new();
    for id in rule_ids {
        // Rule may have been superseded/deleted between injection and
        // settlement — skip silently, nothing to account for.
        let mut metadata = match engine.get_metadata(agent_id, id).await {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "rule lifecycle: read metadata failed: {e}");
                continue;
            }
        };

        let mut stats = RuleStats::from_metadata(&metadata);
        stats.record(category);
        stats.merge_into(&mut metadata);

        if let Err(e) = engine.update_metadata(agent_id, id, &metadata).await {
            warn!(agent = %agent_id, rule = %id, "rule lifecycle: write metadata failed: {e}");
            continue;
        }

        if stats.is_spent() {
            match engine
                .set_importance_and_add_tag(agent_id, id, RETIRED_IMPORTANCE, RETIRED_RULE_TAG)
                .await
            {
                Ok(true) => retired.push(id.clone()),
                Ok(false) => {}
                Err(e) => {
                    warn!(agent = %agent_id, rule = %id, "rule lifecycle: retire failed: {e}");
                }
            }
        }
    }
    retired
}

/// Fire-and-forget settlement used by the channel-reply outcome path.
///
/// Detached so it never delays reply delivery; failures only log.
pub fn settle_detached(
    db_path: PathBuf,
    agent_id: String,
    rule_ids: Vec<String>,
    category: ErrorCategory,
) {
    if rule_ids.is_empty() {
        return;
    }
    tokio::spawn(async move {
        match SqliteMemoryEngine::new(&db_path) {
            Ok(engine) => {
                let retired = settle_injected_rules(&engine, &agent_id, &rule_ids, category).await;
                if !retired.is_empty() {
                    info!(
                        agent = %agent_id,
                        retired = retired.len(),
                        "rule lifecycle: retired net-zero rules"
                    );
                }
            }
            Err(e) => warn!(agent = %agent_id, "rule lifecycle: open memory engine failed: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};
    use duduclaw_memory::TemporalMeta;

    /// Store a consolidated rule with explicit counters; returns its id.
    async fn store_rule(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        stats: RuleStats,
        age_secs: i64,
    ) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::seconds(age_secs),
            tags: vec!["reflexion".to_string(), "consolidated".to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: RULE_SOURCE_EVENT.to_string(),
        };
        let meta = TemporalMeta {
            metadata: Some(serde_json::json!({
                "source_mistake_ids": ["m-1"],
                "rule_stats": stats,
            })),
            ..Default::default()
        };
        engine.store_temporal(agent, entry, meta).await.unwrap()
    }

    async fn read_stats(engine: &SqliteMemoryEngine, agent: &str, id: &str) -> RuleStats {
        let meta = engine.get_metadata(agent, id).await.unwrap().unwrap();
        RuleStats::from_metadata(&meta)
    }

    #[tokio::test]
    async fn settlement_increments_correct_counter_per_category() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-rl";
        let id = store_rule(&engine, agent, "rule A", RuleStats::initial(), 0).await;
        let ids = vec![id.clone()];

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Negligible).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 3, harmful: 0 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Moderate).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 4, harmful: 0 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Significant).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 4, harmful: 1 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Critical).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 4, harmful: 2 });

        // Sibling metadata keys survive the counter round-trips.
        let meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        assert_eq!(meta["source_mistake_ids"][0], "m-1");
    }

    #[tokio::test]
    async fn net_zero_rule_is_retired_and_excluded_from_selection() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-retire";
        let id = store_rule(&engine, agent, "rule B", RuleStats::initial(), 0).await;
        let ids = vec![id.clone()];

        // helpful=2 → two harmful settlements drive net credit to zero.
        let retired =
            settle_injected_rules(&engine, agent, &ids, ErrorCategory::Critical).await;
        assert!(retired.is_empty(), "net still positive after one harmful");
        assert_eq!(select_rules(&engine, agent, INJECTION_LIMIT).await.len(), 1);

        let retired =
            settle_injected_rules(&engine, agent, &ids, ErrorCategory::Significant).await;
        assert_eq!(retired, vec![id.clone()], "second harmful must retire");

        // Tag + low importance persisted on the entry.
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == RETIRED_RULE_TAG));
        assert!(entry.importance < 2.0, "importance demoted on retirement");

        // F2a selection now excludes it.
        assert!(select_rules(&engine, agent, INJECTION_LIMIT).await.is_empty());
    }

    #[tokio::test]
    async fn selection_orders_by_net_score_and_caps_at_limit() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-rank";
        let low =
            store_rule(&engine, agent, "low", RuleStats { helpful: 3, harmful: 2 }, 10).await;
        let high =
            store_rule(&engine, agent, "high", RuleStats { helpful: 7, harmful: 2 }, 20).await;
        let mid =
            store_rule(&engine, agent, "mid", RuleStats { helpful: 5, harmful: 2 }, 30).await;

        let all = select_rules(&engine, agent, INJECTION_LIMIT).await;
        let got: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, vec![high.as_str(), mid.as_str(), low.as_str()]);
        assert_eq!(all[0].net, 5);

        let capped = select_rules(&engine, agent, 2).await;
        let got: Vec<&str> = capped.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, vec![high.as_str(), mid.as_str()]);
    }

    #[tokio::test]
    async fn settlement_skips_unknown_and_foreign_ids() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-own";
        let foreign = store_rule(&engine, "other-agent", "theirs", RuleStats::initial(), 0).await;

        let retired = settle_injected_rules(
            &engine,
            agent,
            &["no-such-id".to_string(), foreign.clone()],
            ErrorCategory::Critical,
        )
        .await;
        assert!(retired.is_empty());
        // Foreign rule untouched (ownership enforced by the engine helpers).
        assert_eq!(
            read_stats(&engine, "other-agent", &foreign).await,
            RuleStats::initial()
        );
    }
}
