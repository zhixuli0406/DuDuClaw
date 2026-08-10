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
//! 1. F2b seeds new rules with `helpful = 1` and tags them
//!    [`PROBATION_RULE_TAG`] (WP2 Janus arXiv:2606.31121 trial period — a
//!    freshly consolidated rule hasn't earned trust yet; the old
//!    `helpful = 2` seed let one bad rule survive two harmful outcomes
//!    while polluting the injected prompt the whole time).
//! 2. F2a injection selects non-retired rules ranked by net score
//!    (`helpful − harmful`, descending; ties put probation rules after
//!    graduated ones) and records the injected ids.
//! 3. At prediction-outcome settlement the turn's final [`ErrorCategory`]
//!    credits (Negligible/Moderate → `helpful += 1`) or blames
//!    (Significant/Critical → `harmful += 1`) each injected rule.
//! 4. Probation rules retire on their **first** harmful outcome, full stop
//!    (Janus one-strike trial), regardless of any helpful credit banked so
//!    far. They **graduate** — [`PROBATION_RULE_TAG`] is removed — once
//!    `helpful >= `[`PROBATION_GRADUATE_HELPFUL`] with zero harmful strikes.
//!    Graduated (non-probation) rules keep the original generic rule:
//!    `helpful.saturating_sub(harmful) == 0` retires (importance dropped,
//!    tagged [`RETIRED_RULE_TAG`], so F2a skips it).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_memory::SqliteMemoryEngine;

use super::engine::ErrorCategory;
use super::rule_gate::{evaluate_rule_gate, GateDecision, HeldOutStats};

/// `source_event` written by `reflexion::maybe_consolidate` (F2b).
pub const RULE_SOURCE_EVENT: &str = "reflexion_consolidation";
/// `source_event` written by `task_rule_induce::maybe_induce_task_rule`
/// (WP-A4, design §6.5 T8/T9: task-layer rules go through this same
/// ACE/ExpeL lifecycle machinery directly, never through `playbook`).
/// Deliberately distinct from [`RULE_SOURCE_EVENT`] so
/// `list_valid_by_source_event` scans never mix dialogue-layer
/// (`reflexion.rs`) and task-layer (`task_rule_induce.rs`) rules — the
/// channel-reply F2a injection (`select_rules`) must never surface a
/// task-layer rule, and the goal-loop dispatch injection
/// (`select_task_rules`) must never surface a dialogue-layer one.
pub const TASK_RULE_SOURCE_EVENT: &str = "task_forward_induction";
/// Tag distinguishing a task-layer rule (WP-A4) from a dialogue-layer
/// (F2b `reflexion.rs`) one — both live on `MemoryLayer::Semantic` and
/// share this module's `RuleStats`/probation/retirement machinery, so the
/// tag is the human-legible marker of provenance (the `source_event`
/// difference above is the machine-enforced one).
pub const TASK_RULE_TAG: &str = "task-rule";
/// Tag appended on retirement; F2a selection excludes entries carrying it.
pub const RETIRED_RULE_TAG: &str = "retired-rule";
/// WP-P3 (held-out rule gate): marks a rule as a **shadow candidate** — it is
/// scored out-of-sample but never injected into a prompt. Selection excludes
/// entries carrying it exactly like [`RETIRED_RULE_TAG`]. The tag is only ever
/// minted when `[task_forward_model] held_out_gate_enabled = true` (a newly
/// born inductive lesson, or a keep-better demotion), so excluding it
/// unconditionally in selection is **byte-identical** when the gate is off —
/// no rule can carry the tag then. See `prediction::rule_gate`.
pub const SHADOW_RULE_TAG: &str = "shadow-rule";
/// Tag appended to a freshly consolidated rule (WP2 Janus trial period).
/// Removed on graduation (`helpful >= PROBATION_GRADUATE_HELPFUL` with no
/// harmful strikes yet); any harmful strike while present retires the rule
/// immediately regardless of accumulated helpful credit.
pub const PROBATION_RULE_TAG: &str = "probation-rule";
/// Cumulative helpful settlements (with zero harmful strikes) required for a
/// probation rule to graduate — i.e. lose [`PROBATION_RULE_TAG`].
pub const PROBATION_GRADUATE_HELPFUL: u32 = 3;
/// Max rules injected per turn — mirrors the F2a mistake-injection cap.
pub const INJECTION_LIMIT: usize = 3;
/// Max WP-A4 task rules injected per goal-loop dispatch round — smaller than
/// [`INJECTION_LIMIT`] since the dispatch prompt already carries the `<state>`
/// block, judge feedback, and acceptance criteria; task rules are a small
/// addendum, not the primary steering signal (design §6.5 item 2: "取 cap
/// 2").
pub const TASK_RULE_INJECTION_LIMIT: usize = 2;
/// Janus trial period (WP2): new rules start on probation with the minimum
/// possible credit → `helpful = 1, harmful = 0`. A single harmful strike
/// during probation retires the rule outright (see [`settle_injected_rules`]);
/// this replaces the old ExpeL `helpful = 2` seed which let one bad rule
/// survive two harmful outcomes before demotion.
const INITIAL_HELPFUL: u32 = 1;
/// Importance assigned on retirement (fresh rules are stored at 8.0).
const RETIRED_IMPORTANCE: f64 = 1.0;
/// Metadata JSON key holding the counters.
const STATS_KEY: &str = "rule_stats";
/// Candidate scan bound before ranking — keeps per-turn work O(1)-ish even
/// for agents that accumulated many rules over months.
///
/// `pub(crate)` so `crate::playbook` can scan the same window (WP1.2/1.3):
/// `PLAYBOOK_MAX_ENTRIES` (40) is deliberately kept <= this cap so ranking
/// never sees a truncated candidate set.
pub(crate) const CANDIDATE_SCAN_CAP: usize = 50;

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
    /// Still on the Janus trial period (carries [`PROBATION_RULE_TAG`]).
    pub probation: bool,
}

/// Select active (non-retired) consolidated rules for F2a injection, ranked
/// by net score descending, capped at `limit`.
///
/// At equal net score, graduated rules are preferred over probation rules
/// (WP2 Janus — a trial-period rule hasn't earned the same trust as a
/// graduated one even if its score currently ties). Beyond that, ties keep
/// newest-first order (the underlying listing is newest-first and the sort
/// is stable). Errors degrade to an empty selection — rule injection is an
/// enhancement, never a reply blocker.
pub async fn select_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    limit: usize,
) -> Vec<InjectedRule> {
    select_rules_by_source_event(engine, agent_id, RULE_SOURCE_EVENT, limit).await
}

/// Same selection/ranking contract as [`select_rules`], scoped to WP-A4
/// task-layer rules ([`TASK_RULE_SOURCE_EVENT`]) instead of F2b dialogue
/// rules. Used by `goal_loop.rs`'s dispatch-prompt injection step — kept as
/// a distinct public entry point (rather than a `source_event` parameter on
/// `select_rules` itself) so every existing `select_rules` call site keeps
/// its exact prior signature.
pub async fn select_task_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    limit: usize,
) -> Vec<InjectedRule> {
    select_rules_by_source_event(engine, agent_id, TASK_RULE_SOURCE_EVENT, limit).await
}

/// Shared selection/ranking body for [`select_rules`] and
/// [`select_task_rules`] — identical logic, only the `source_event` scan
/// filter differs (see those functions' docs for why the two must never
/// share a scan).
async fn select_rules_by_source_event(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    source_event: &str,
    limit: usize,
) -> Vec<InjectedRule> {
    let rows = match engine
        .list_valid_by_source_event(agent_id, source_event, CANDIDATE_SCAN_CAP)
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
        .filter(|(entry, _)| {
            // WP-P3: shadow candidates are scored out-of-sample but never
            // injected. No-op when the held-out gate is off (no rule carries
            // the tag then) — byte-identical to the pre-WP3 retired-only filter.
            !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG || t == SHADOW_RULE_TAG)
        })
        .map(|(entry, metadata)| InjectedRule {
            net: RuleStats::from_metadata(&metadata).net(),
            probation: entry.tags.iter().any(|t| t == PROBATION_RULE_TAG),
            id: entry.id,
            content: entry.content,
        })
        .collect();
    rules.sort_by(|a, b| b.net.cmp(&a.net).then(a.probation.cmp(&b.probation)));
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
/// metadata, then apply the Janus trial-period rule (WP2):
/// - **Probation** rule (carries [`PROBATION_RULE_TAG`]) hit by a harmful
///   outcome this turn → retire immediately (low importance +
///   [`RETIRED_RULE_TAG`]), regardless of any helpful credit banked so far.
/// - Probation rule with a helpful outcome this turn and
///   `helpful >= `[`PROBATION_GRADUATE_HELPFUL`] → graduate: remove
///   [`PROBATION_RULE_TAG`], stays active.
/// - Graduated (non-probation) rule → original generic rule: retire when
///   `helpful.saturating_sub(harmful) == 0`.
///
/// Returns the ids retired by this settlement.
pub async fn settle_injected_rules(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    rule_ids: &[String],
    category: ErrorCategory,
) -> Vec<String> {
    let is_harmful_event =
        matches!(category, ErrorCategory::Significant | ErrorCategory::Critical);
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
        // Tags live on the entry row, not in the metadata blob — a separate
        // read is needed to know whether this rule is still on probation.
        let entry = match engine.get_by_id(agent_id, id).await {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "rule lifecycle: read entry failed: {e}");
                continue;
            }
        };
        let is_probation = entry.tags.iter().any(|t| t == PROBATION_RULE_TAG);

        let mut stats = RuleStats::from_metadata(&metadata);
        stats.record(category);
        stats.merge_into(&mut metadata);

        if let Err(e) = engine.update_metadata(agent_id, id, &metadata).await {
            warn!(agent = %agent_id, rule = %id, "rule lifecycle: write metadata failed: {e}");
            continue;
        }

        if is_probation && is_harmful_event {
            // Janus one-strike trial: any harmful outcome during probation
            // retires the rule outright, no net-score grace period.
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
        } else if is_probation {
            // Helpful outcome while on probation — graduate once enough
            // credit is banked (zero harmful strikes by construction, since
            // any harmful strike above already retired the rule this turn).
            if stats.helpful >= PROBATION_GRADUATE_HELPFUL {
                if let Err(e) = engine.remove_tag(agent_id, id, PROBATION_RULE_TAG).await {
                    warn!(agent = %agent_id, rule = %id, "rule lifecycle: graduate failed: {e}");
                }
            }
        } else if stats.is_spent() {
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

/// WP-P3 held-out-gated settlement — reached only when
/// `[task_forward_model] held_out_gate_enabled = true` (the caller branches on
/// that flag; when off it calls the unchanged [`settle_injected_rules`], so the
/// gate-off path is byte-identical).
///
/// For each injected rule this records the turn's out-of-sample outcome — a
/// deterministic hit (Negligible/Moderate) or miss (Significant/Critical),
/// **never an LLM self-assessment** — into the rule's [`HeldOutStats`], then
/// hands the fate decision to [`evaluate_rule_gate`], which reads only that
/// numeric record. This replaces the pure `ErrorCategory`-credit retirement of
/// [`settle_injected_rules`] for a gate-on deployment:
/// - [`GateDecision::Retire`] → [`RETIRED_RULE_TAG`] + low importance (returned
///   in the retired list).
/// - [`GateDecision::Demote`] → [`SHADOW_RULE_TAG`] (keep-better: stop
///   injecting, keep the row + its history; a regime shift can be reverted, not
///   deleted).
/// - [`GateDecision::Inject`] → remove [`SHADOW_RULE_TAG`] if present (promote /
///   stay active).
/// - [`GateDecision::Shadow`] → leave tags untouched (not yet decidable).
///
/// `now_seq` is the current prequential cursor (e.g. unix seconds). An outcome
/// is counted only when `now_seq > born_seq` so a candidate's own birth batch
/// never validates it (train != test; design §6 item 2). `RuleStats` counters
/// are still updated for telemetry/continuity but no longer drive retirement on
/// this path.
#[allow(clippy::too_many_arguments)]
pub async fn settle_injected_rules_held_out(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    rule_ids: &[String],
    category: ErrorCategory,
    k_concurrent_candidates: usize,
    baseline_hit_rate: f64,
    now_seq: u64,
) -> Vec<String> {
    let is_hit = matches!(category, ErrorCategory::Negligible | ErrorCategory::Moderate);
    let mut retired = Vec::new();
    for id in rule_ids {
        let mut metadata = match engine.get_metadata(agent_id, id).await {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "held-out gate: read metadata failed: {e}");
                continue;
            }
        };
        let entry = match engine.get_by_id(agent_id, id).await {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "held-out gate: read entry failed: {e}");
                continue;
            }
        };

        // Telemetry-only rule_stats update (kept for continuity; no longer the
        // retirement authority on this path).
        let mut stats = RuleStats::from_metadata(&metadata);
        stats.record(category);
        stats.merge_into(&mut metadata);

        // Held-out numeric record. Prequential guard: never count the birth
        // batch against the candidate that produced it.
        let mut held = HeldOutStats::from_metadata(&metadata);
        if now_seq > held.born_seq {
            held.record(is_hit);
        }
        held.merge_into(&mut metadata);

        if let Err(e) = engine.update_metadata(agent_id, id, &metadata).await {
            warn!(agent = %agent_id, rule = %id, "held-out gate: write metadata failed: {e}");
            continue;
        }

        let is_shadow = entry.tags.iter().any(|t| t == SHADOW_RULE_TAG);
        match evaluate_rule_gate(&held, k_concurrent_candidates, baseline_hit_rate) {
            GateDecision::Retire => {
                match engine
                    .set_importance_and_add_tag(agent_id, id, RETIRED_IMPORTANCE, RETIRED_RULE_TAG)
                    .await
                {
                    Ok(true) => retired.push(id.clone()),
                    Ok(false) => {}
                    Err(e) => {
                        warn!(agent = %agent_id, rule = %id, "held-out gate: retire failed: {e}");
                    }
                }
            }
            GateDecision::Demote => {
                // Keep-better: add the shadow tag (stop injecting) while
                // preserving the row's current importance and full history.
                if !is_shadow {
                    if let Err(e) = engine
                        .set_importance_and_add_tag(agent_id, id, entry.importance, SHADOW_RULE_TAG)
                        .await
                    {
                        warn!(agent = %agent_id, rule = %id, "held-out gate: demote failed: {e}");
                    }
                }
            }
            GateDecision::Inject => {
                // Promotion / continued eligibility: clear any shadow tag.
                if is_shadow {
                    if let Err(e) = engine.remove_tag(agent_id, id, SHADOW_RULE_TAG).await {
                        warn!(agent = %agent_id, rule = %id, "held-out gate: promote failed: {e}");
                    }
                }
            }
            GateDecision::Shadow => {
                // Not yet decidable — leave the rule exactly as it is.
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
    /// `probation` tags the entry [`PROBATION_RULE_TAG`] to simulate a
    /// freshly F2b-consolidated rule under the Janus trial period.
    async fn store_rule(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        stats: RuleStats,
        age_secs: i64,
        probation: bool,
    ) -> String {
        let mut tags = vec!["reflexion".to_string(), "consolidated".to_string()];
        if probation {
            tags.push(PROBATION_RULE_TAG.to_string());
        }
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now() - chrono::Duration::seconds(age_secs),
            tags,
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

    /// Same as `store_rule` but with an explicit `source_event` + `tags` —
    /// used by the WP-A4 isolation test below to simulate a
    /// `task_rule_induce`-written row without depending on that module.
    async fn store_rule_with_source(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        stats: RuleStats,
        source_event: &str,
        tags: Vec<String>,
    ) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            tags,
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: source_event.to_string(),
        };
        let meta = TemporalMeta {
            metadata: Some(serde_json::json!({ "rule_stats": stats })),
            ..Default::default()
        };
        engine.store_temporal(agent, entry, meta).await.unwrap()
    }

    #[tokio::test]
    async fn settlement_increments_correct_counter_per_category() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-rl";
        // Non-probation (graduated-style) rule: exercises the generic
        // net-score accounting path in isolation from Janus probation rules.
        let id = store_rule(&engine, agent, "rule A", RuleStats::initial(), 0, false).await;
        let ids = vec![id.clone()];

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Negligible).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 2, harmful: 0 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Moderate).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 3, harmful: 0 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Significant).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 3, harmful: 1 });

        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Critical).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 3, harmful: 2 });

        // Sibling metadata keys survive the counter round-trips.
        let meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        assert_eq!(meta["source_mistake_ids"][0], "m-1");
    }

    #[tokio::test]
    async fn net_zero_rule_is_retired_and_excluded_from_selection() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-retire";
        // Non-probation rule seeded at the new Janus `helpful = 1` floor —
        // a single harmful settlement alone already zeroes net credit.
        let id = store_rule(&engine, agent, "rule B", RuleStats::initial(), 0, false).await;
        let ids = vec![id.clone()];

        let retired =
            settle_injected_rules(&engine, agent, &ids, ErrorCategory::Critical).await;
        assert_eq!(
            retired,
            vec![id.clone()],
            "helpful=1 seed → a single harmful settlement retires (net = 1 - 1 = 0)"
        );

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
            store_rule(&engine, agent, "low", RuleStats { helpful: 3, harmful: 2 }, 10, false)
                .await;
        let high =
            store_rule(&engine, agent, "high", RuleStats { helpful: 7, harmful: 2 }, 20, false)
                .await;
        let mid =
            store_rule(&engine, agent, "mid", RuleStats { helpful: 5, harmful: 2 }, 30, false)
                .await;

        let all = select_rules(&engine, agent, INJECTION_LIMIT).await;
        let got: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, vec![high.as_str(), mid.as_str(), low.as_str()]);
        assert_eq!(all[0].net, 5);

        let capped = select_rules(&engine, agent, 2).await;
        let got: Vec<&str> = capped.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, vec![high.as_str(), mid.as_str()]);

        // WP2: at an equal net score, a probation rule sorts after a
        // graduated (non-probation) one.
        let grad_tie = store_rule(
            &engine, agent, "graduated-tie", RuleStats { helpful: 5, harmful: 0 }, 5, false,
        )
        .await;
        let probation_tie = store_rule(
            &engine, agent, "probation-tie", RuleStats { helpful: 5, harmful: 0 }, 1, true,
        )
        .await;
        let all2 = select_rules(&engine, agent, 10).await;
        let idx_grad = all2.iter().position(|r| r.id == grad_tie).unwrap();
        let idx_probation = all2.iter().position(|r| r.id == probation_tie).unwrap();
        assert!(
            idx_grad < idx_probation,
            "graduated rule must rank before a probation rule at equal net score"
        );
        assert!(!all2[idx_grad].probation);
        assert!(all2[idx_probation].probation);
    }

    #[tokio::test]
    async fn settlement_skips_unknown_and_foreign_ids() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-own";
        let foreign =
            store_rule(&engine, "other-agent", "theirs", RuleStats::initial(), 0, false).await;

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

    #[tokio::test]
    async fn probation_rule_retires_on_first_harmful() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-probation-retire";
        let id = store_rule(&engine, agent, "rule P", RuleStats::initial(), 0, true).await;
        let ids = vec![id.clone()];

        // Bank one helpful credit first (helpful=2, harmful=0). A generic
        // net-score check would NOT retire on the next harmful outcome
        // (net = 2 - 1 = 1), but Janus one-strike trial retires *any*
        // probation rule on its first harmful outcome regardless of banked
        // credit.
        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Negligible).await;
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 2, harmful: 0 });
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == PROBATION_RULE_TAG), "still on probation");

        let retired = settle_injected_rules(&engine, agent, &ids, ErrorCategory::Significant).await;
        assert_eq!(
            retired,
            vec![id.clone()],
            "first harmful outcome during probation retires immediately"
        );

        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == RETIRED_RULE_TAG));
        assert!(entry.importance < 2.0, "importance demoted on retirement");
        assert!(select_rules(&engine, agent, INJECTION_LIMIT).await.is_empty());
    }

    #[tokio::test]
    async fn probation_rule_graduates_after_three_helpful() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-probation-graduate";
        let id = store_rule(&engine, agent, "rule Q", RuleStats::initial(), 0, true).await;
        let ids = vec![id.clone()];

        // helpful=1 seed → two more helpful settlements (zero harmful
        // strikes) reach PROBATION_GRADUATE_HELPFUL = 3.
        settle_injected_rules(&engine, agent, &ids, ErrorCategory::Negligible).await;
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == PROBATION_RULE_TAG), "not yet graduated");

        let retired = settle_injected_rules(&engine, agent, &ids, ErrorCategory::Moderate).await;
        assert!(retired.is_empty(), "graduation must not be reported as retirement");
        assert_eq!(read_stats(&engine, agent, &id).await, RuleStats { helpful: 3, harmful: 0 });

        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            !entry.tags.iter().any(|t| t == PROBATION_RULE_TAG),
            "probation tag removed on graduation"
        );
        assert!(
            !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "graduation must not retire the rule"
        );

        // Still selectable, now ranked as a normal (non-probation) rule.
        let selected = select_rules(&engine, agent, INJECTION_LIMIT).await;
        assert_eq!(selected.len(), 1);
        assert!(!selected[0].probation);
    }

    // ── WP-A4: select_task_rules / select_rules source_event isolation ──

    #[tokio::test]
    async fn select_task_rules_never_surfaces_dialogue_rules_and_vice_versa() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-a4-isolation";

        let dialogue_id = store_rule(&engine, agent, "dialogue rule", RuleStats::initial(), 0, true).await;
        let task_id = store_rule_with_source(
            &engine,
            agent,
            "task rule",
            RuleStats::initial(),
            TASK_RULE_SOURCE_EVENT,
            vec![TASK_RULE_TAG.to_string(), PROBATION_RULE_TAG.to_string()],
        )
        .await;

        let dialogue_selected = select_rules(&engine, agent, INJECTION_LIMIT).await;
        assert_eq!(dialogue_selected.len(), 1);
        assert_eq!(dialogue_selected[0].id, dialogue_id);

        let task_selected = select_task_rules(&engine, agent, INJECTION_LIMIT).await;
        assert_eq!(task_selected.len(), 1);
        assert_eq!(task_selected[0].id, task_id);
    }

    #[tokio::test]
    async fn select_task_rules_ranks_and_excludes_retired_same_as_select_rules() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-a4-rank";
        let low = store_rule_with_source(
            &engine, agent, "low", RuleStats { helpful: 3, harmful: 2 },
            TASK_RULE_SOURCE_EVENT, vec![TASK_RULE_TAG.to_string()],
        )
        .await;
        let high = store_rule_with_source(
            &engine, agent, "high", RuleStats { helpful: 7, harmful: 2 },
            TASK_RULE_SOURCE_EVENT, vec![TASK_RULE_TAG.to_string()],
        )
        .await;

        let selected = select_task_rules(&engine, agent, INJECTION_LIMIT).await;
        let got: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(got, vec![high.as_str(), low.as_str()]);

        // Retire `high` via the shared settle function, then confirm
        // selection excludes it — same lifecycle machinery as dialogue rules.
        settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        let retired = settle_injected_rules(&engine, agent, &[high.clone()], ErrorCategory::Critical).await;
        assert_eq!(retired, vec![high.clone()]);
        let after = select_task_rules(&engine, agent, INJECTION_LIMIT).await;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, low);
    }

    // ── WP-P3: held-out rule gate wiring ──────────────────────────────────

    #[tokio::test]
    async fn select_excludes_shadow_tagged_rules() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-select";
        let normal =
            store_rule(&engine, agent, "normal", RuleStats { helpful: 5, harmful: 0 }, 0, false)
                .await;
        // A shadow-tagged rule with an even higher net score must still be
        // excluded — shadow candidates are scored, never injected.
        let shadow = store_rule_with_source(
            &engine,
            agent,
            "shadow",
            RuleStats { helpful: 9, harmful: 0 },
            RULE_SOURCE_EVENT,
            vec![SHADOW_RULE_TAG.to_string()],
        )
        .await;

        let selected = select_rules(&engine, agent, INJECTION_LIMIT).await;
        let ids: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&normal.as_str()), "non-shadow rule is selectable");
        assert!(
            !ids.contains(&shadow.as_str()),
            "shadow-tagged rule must be excluded even with a higher net score"
        );
    }

    #[tokio::test]
    async fn held_out_gate_injects_strong_record_then_keep_better_demotes() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-heldout";
        // born_seq 0 (default) so every settle counts as out-of-sample.
        let id = store_rule(&engine, agent, "rule H", RuleStats::initial(), 0, false).await;
        let ids = vec![id.clone()];
        let now = 1_000_000u64;

        // 20 out-of-sample hits → strong cumulative record; the gate keeps it
        // injectable (no shadow tag).
        for _ in 0..20 {
            settle_injected_rules_held_out(
                &engine, agent, &ids, ErrorCategory::Negligible, 1, 0.5, now,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            !entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "a strong out-of-sample record stays injectable (Inject)"
        );
        // The held-out counters are populated from the numeric outcomes.
        let held = HeldOutStats::from_metadata(
            &engine.get_metadata(agent, &id).await.unwrap().unwrap(),
        );
        assert_eq!(held.oos_hits, 20);
        assert_eq!(held.oos_misses, 0);

        // 8 misses collapse the recent rolling window → keep-better demotion:
        // shadow-tagged (stop injecting) but NOT retired (row + history kept).
        for _ in 0..8 {
            settle_injected_rules_held_out(
                &engine, agent, &ids, ErrorCategory::Critical, 1, 0.5, now,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "recent-window regression demotes to shadow (keep-better)"
        );
        assert!(
            !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "keep-better never deletes/retires on a recoverable regression"
        );

        // Once demoted the rule is excluded from injection selection.
        let ok = store_rule(
            &engine, agent, "rule OK", RuleStats { helpful: 5, harmful: 0 }, 0, false,
        )
        .await;
        let selected = select_rules(&engine, agent, INJECTION_LIMIT).await;
        let sel: Vec<&str> = selected.iter().map(|r| r.id.as_str()).collect();
        assert!(sel.contains(&ok.as_str()));
        assert!(!sel.contains(&id.as_str()), "demoted (shadow) rule is not injected");
    }

    #[tokio::test]
    async fn held_out_gate_retires_spent_candidate() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-heldout-retire";
        let id = store_rule(&engine, agent, "rule bad", RuleStats::initial(), 0, false).await;
        let ids = vec![id.clone()];
        let now = 2_000_000u64;

        // 1 hit + 15 misses: even the optimistic Wilson upper bound falls
        // below baseline → the candidate is spent → retire.
        settle_injected_rules_held_out(&engine, agent, &ids, ErrorCategory::Negligible, 1, 0.5, now)
            .await;
        for _ in 0..15 {
            settle_injected_rules_held_out(
                &engine, agent, &ids, ErrorCategory::Critical, 1, 0.5, now,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "spent candidate (upper bound below baseline) is retired"
        );
        assert!(entry.importance < 2.0, "importance demoted on retirement");
    }

    #[tokio::test]
    async fn held_out_gate_prequential_split_skips_birth_batch_observation() {
        // An outcome whose sequence is at/below born_seq is the birth batch and
        // must not be counted (train != test).
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-prequential";
        let id = store_rule_with_source(
            &engine,
            agent,
            "rule seq",
            RuleStats::initial(),
            RULE_SOURCE_EVENT,
            vec![],
        )
        .await;
        // Seed a birth cursor of 500.
        let mut meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        HeldOutStats::born(500).merge_into(&mut meta);
        engine.update_metadata(agent, &id, &meta).await.unwrap();

        // Settle with now_seq == born_seq → not counted.
        settle_injected_rules_held_out(
            &engine, agent, &[id.clone()], ErrorCategory::Negligible, 1, 0.5, 500,
        )
        .await;
        let held = HeldOutStats::from_metadata(
            &engine.get_metadata(agent, &id).await.unwrap().unwrap(),
        );
        assert_eq!(held.total(), 0, "birth-batch observation (now_seq <= born_seq) must not count");

        // A later observation IS counted.
        settle_injected_rules_held_out(
            &engine, agent, &[id.clone()], ErrorCategory::Negligible, 1, 0.5, 501,
        )
        .await;
        let held = HeldOutStats::from_metadata(
            &engine.get_metadata(agent, &id).await.unwrap().unwrap(),
        );
        assert_eq!(held.oos_hits, 1, "an out-of-sample observation (now_seq > born_seq) counts");
    }
}
