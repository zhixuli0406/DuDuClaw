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
use super::rule_gate::{evaluate_rule_gate, GateDecision, HeldOutStats, DEFAULT_BASELINE_HIT_RATE};

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
    /// G6: at least one recorded source fact has been superseded (carries
    /// [`crate::prediction::rule_staleness::SOURCE_STALE_RULE_TAG`]). Sorts
    /// after fresh rules; the render path appends a 「來源已更新」 marker.
    pub source_stale: bool,
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
            source_stale: crate::prediction::rule_staleness::is_source_stale(&entry.tags),
            id: entry.id,
            content: entry.content,
        })
        .collect();
    // G6: source-stale rules sort after fresh ones regardless of net score
    // (still injectable, just downweighted — mirrors `playbook::select`).
    rules.sort_by(|a, b| {
        a.source_stale
            .cmp(&b.source_stale)
            .then(b.net.cmp(&a.net))
            .then(a.probation.cmp(&b.probation))
    });
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
        .map(|r| {
            // G6: annotate a source-stale rule inline (it is already
            // downweighted to last by the selection sort above).
            if r.source_stale {
                format!("{} {}", r.content, crate::playbook::select::SOURCE_STALE_MARKER)
            } else {
                r.content.clone()
            }
        })
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

// ─────────────────────────────────────────────────────────────────────────
// v1.54 debt — shadow → promotion prequential scoring pass
// (DESIGN-lwm-calibration-2026-08-10.md §6). This is the OTHER half of the
// held-out loop: `settle_injected_rules_held_out` above scores rules that were
// *injected* (a hit there = a GOOD outcome, because an injected rule is
// credited when the turn went well). This pass scores rules that are still
// *shadow* candidates — never injected, so they cannot be validated as helpers;
// instead they are validated as **risk predictors**. A shadow rule carries the
// implicit, falsifiable prediction "when my signal matches the situation, this
// is a HIGH-RISK situation", so its out-of-sample hit polarity is the OPPOSITE
// of the injected path's.
// ─────────────────────────────────────────────────────────────────────────

/// Out-of-sample hit polarity for a **shadow** candidate rule.
///
/// A shadow rule predicts "high-risk when my signal matches". So the settled
/// outcome is a **hit** when the round actually turned out high-risk
/// (`Significant`/`Critical`) — the trigger correctly flagged risk — and a
/// **miss** otherwise (`Negligible`/`Moderate`, i.e. a false alarm).
///
/// This is deliberately the mirror image of [`settle_injected_rules_held_out`]'s
/// hit rule (`Negligible`/`Moderate` ⇒ hit there): an injected rule is a helper
/// scored on whether the turn went well; a shadow rule is a risk predictor
/// scored on whether the predicted risk materialized. Pure — deterministic
/// mapping, no LLM, no I/O.
pub fn score_shadow_candidate(category: ErrorCategory) -> bool {
    matches!(category, ErrorCategory::Significant | ErrorCategory::Critical)
}

/// Fold one out-of-sample observation into a shadow candidate's
/// [`HeldOutStats`] and run the numeric gate, returning the updated record and
/// the [`GateDecision`]. Near-pure: no I/O, no LLM — the caller persists the
/// returned stats.
///
/// Prequential time split (design §6 item 2): the observation is only counted
/// when `now_seq > stats.born_seq`, so a candidate's own birth batch never
/// validates it (train != test). The gate decision is still computed on the
/// (possibly unchanged) record so a same-instant birth-batch call cleanly
/// resolves to [`GateDecision::Shadow`] via the `n < MIN` branch rather than a
/// special case here.
pub fn update_held_out_and_gate(
    mut stats: HeldOutStats,
    category: ErrorCategory,
    now_seq: u64,
    k_concurrent_candidates: usize,
    baseline_hit_rate: f64,
) -> (HeldOutStats, GateDecision) {
    if now_seq > stats.born_seq {
        stats.record(score_shadow_candidate(category));
    }
    let decision = evaluate_rule_gate(&stats, k_concurrent_candidates, baseline_hit_rate);
    (stats, decision)
}

/// Result of one [`score_shadow_candidates_for_task`] pass — telemetry for the
/// caller to log; carries no control-flow authority of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShadowScoreOutcome {
    /// How many shadow candidates were signal-matched and scored this pass.
    pub scored: usize,
    /// Ids promoted out of shadow this pass ([`SHADOW_RULE_TAG`] removed).
    pub promoted: Vec<String>,
    /// Ids retired this pass ([`RETIRED_RULE_TAG`] + low importance).
    pub retired: Vec<String>,
}

/// v1.54 shadow → promotion pass. Reached only when
/// `[task_forward_model] held_out_gate_enabled = true` (the caller branches on
/// that flag; gate-off never calls this, so the gate-off settle path is
/// byte-identical).
///
/// For every **active shadow** task-layer rule ([`TASK_RULE_SOURCE_EVENT`],
/// carrying [`SHADOW_RULE_TAG`], not [`RETIRED_RULE_TAG`]) whose signal matches
/// the current task situation — the `match_tag` is the current round's
/// `goal_kind:<kind>` tag, built by
/// [`task_rule_induce::goal_kind_tag`](super::task_rule_induce::goal_kind_tag),
/// the SAME convention the rule was born with (no duplicated format) — record
/// one out-of-sample observation ([`score_shadow_candidate`] polarity), persist
/// the updated [`HeldOutStats`], and act on the gate:
/// - [`GateDecision::Inject`] → **promote**: remove [`SHADOW_RULE_TAG`] so
///   `select_task_rules` starts injecting it (its record beat the frozen
///   baseline out-of-sample).
/// - [`GateDecision::Retire`] → retire (spent risk predictor: even the
///   optimistic upper bound is below baseline).
/// - [`GateDecision::Demote`] / [`GateDecision::Shadow`] → leave it shadow
///   (not promotable / not yet decidable — it is already un-injected, so
///   keep-better "stop injecting" is a no-op here).
///
/// `k` for the Bonferroni correction is the count of active shadow candidates
/// in this family — more candidates trialed at once makes promotion strictly
/// harder. Every failure path logs and continues; scoring a shadow candidate
/// is an enhancement, never a settle blocker.
pub async fn score_shadow_candidates_for_task(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    match_tag: &str,
    category: ErrorCategory,
    baseline_hit_rate: f64,
    now_seq: u64,
) -> ShadowScoreOutcome {
    let rows = match engine
        .list_valid_by_source_event(agent_id, TASK_RULE_SOURCE_EVENT, CANDIDATE_SCAN_CAP)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(agent = %agent_id, "shadow-scoring: list task rules failed: {e}");
            return ShadowScoreOutcome::default();
        }
    };

    // Active shadow candidates: the concurrent trial family for Bonferroni.
    let shadow: Vec<(String, Vec<String>, serde_json::Value)> = rows
        .into_iter()
        .filter(|(entry, _)| {
            entry.tags.iter().any(|t| t == SHADOW_RULE_TAG)
                && !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG)
        })
        .map(|(entry, meta)| (entry.id, entry.tags, meta))
        .collect();
    let k = shadow.len().max(1);

    let mut out = ShadowScoreOutcome::default();
    for (id, tags, metadata) in shadow {
        // Signal match: only score a shadow rule whose trigger matches this
        // round's task situation (goal_kind tag equality — the reused
        // task_rule_induce convention).
        if !tags.iter().any(|t| t == match_tag) {
            continue;
        }
        score_one_shadow_candidate(
            engine, agent_id, &id, metadata, category, k, baseline_hit_rate, now_seq, &mut out,
        )
        .await;
    }
    out
}

/// Shared per-candidate body of the two shadow-scoring passes
/// ([`score_shadow_candidates_for_task`] / [`score_shadow_candidates_by_ids`]):
/// fold this settle's out-of-sample observation into the candidate's
/// [`HeldOutStats`] (shadow polarity, prequential-guarded), persist, and act
/// on the gate decision. The caller has already established that the row is
/// an active shadow candidate. Failures log and leave the row untouched.
#[allow(clippy::too_many_arguments)]
async fn score_one_shadow_candidate(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    id: &str,
    mut metadata: serde_json::Value,
    category: ErrorCategory,
    k_concurrent_candidates: usize,
    baseline_hit_rate: f64,
    now_seq: u64,
    out: &mut ShadowScoreOutcome,
) {
    let stats = HeldOutStats::from_metadata(&metadata);
    let (updated, decision) = update_held_out_and_gate(
        stats,
        category,
        now_seq,
        k_concurrent_candidates,
        baseline_hit_rate,
    );
    updated.merge_into(&mut metadata);
    if let Err(e) = engine.update_metadata(agent_id, id, &metadata).await {
        warn!(agent = %agent_id, rule = %id, "shadow-scoring: write metadata failed: {e}");
        return;
    }
    out.scored += 1;

    match decision {
        GateDecision::Inject => match engine.remove_tag(agent_id, id, SHADOW_RULE_TAG).await {
            Ok(true) => out.promoted.push(id.to_string()),
            Ok(false) => {}
            Err(e) => warn!(agent = %agent_id, rule = %id, "shadow-scoring: promote failed: {e}"),
        },
        GateDecision::Retire => {
            match engine
                .set_importance_and_add_tag(agent_id, id, RETIRED_IMPORTANCE, RETIRED_RULE_TAG)
                .await
            {
                Ok(true) => out.retired.push(id.to_string()),
                Ok(false) => {}
                Err(e) => {
                    warn!(agent = %agent_id, rule = %id, "shadow-scoring: retire failed: {e}")
                }
            }
        }
        // Already un-injected: nothing to stop injecting (Demote), and
        // Shadow means "not decidable yet" — leave the row as-is.
        GateDecision::Demote | GateDecision::Shadow => {}
    }
}

/// Dialogue-layer shadow-scoring pass (closes the v1.54 debt documented in
/// `docs/features/39-calibrated-forward-model.md` "還沒做的部分") — the
/// channel-reply twin of [`score_shadow_candidates_for_task`].
///
/// The task pass signal-matches at settle time by `goal_kind:` tag equality;
/// dialogue rules instead carry playbook `signals_match` tokens, which the
/// prompt-build step already evaluates against the turn's `TurnSignals`
/// (`playbook::collect_armed_shadow`). So this pass takes the **armed id
/// list** captured at build time: each armed candidate implicitly predicted
/// "this turn is high-risk", and the settled [`ErrorCategory`] is its
/// deterministic out-of-sample oracle ([`score_shadow_candidate`] polarity —
/// hit ⇔ Significant/Critical).
///
/// `k_concurrent_candidates` is the agent's active shadow family size at
/// arming time (Bonferroni — more candidates trialed at once makes promotion
/// strictly harder). A row that lost its shadow tag, was retired, or was
/// deleted between arming and settle is skipped silently: the observation
/// belongs to the state the rule was armed in, and that state no longer
/// exists. Every failure path logs and continues — scoring is an
/// enhancement, never a settle blocker.
pub async fn score_shadow_candidates_by_ids(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    armed_ids: &[String],
    k_concurrent_candidates: usize,
    category: ErrorCategory,
    baseline_hit_rate: f64,
    now_seq: u64,
) -> ShadowScoreOutcome {
    let k = k_concurrent_candidates.max(1);
    let mut out = ShadowScoreOutcome::default();
    for id in armed_ids {
        let entry = match engine.get_by_id(agent_id, id).await {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "shadow-scoring: read entry failed: {e}");
                continue;
            }
        };
        // Still an active shadow candidate? (State may have changed between
        // arming and settle — e.g. promoted or retired by a concurrent turn.)
        let is_shadow = entry.tags.iter().any(|t| t == SHADOW_RULE_TAG);
        let is_retired = entry.tags.iter().any(|t| t == RETIRED_RULE_TAG);
        if !is_shadow || is_retired {
            continue;
        }
        let metadata = match engine.get_metadata(agent_id, id).await {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(e) => {
                warn!(agent = %agent_id, rule = %id, "shadow-scoring: read metadata failed: {e}");
                continue;
            }
        };
        score_one_shadow_candidate(
            engine, agent_id, id, metadata, category, k, baseline_hit_rate, now_seq, &mut out,
        )
        .await;
    }
    out
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

/// Fire-and-forget held-out settlement for the channel-reply outcome path —
/// the gate-on twin of [`settle_detached`], mirroring `dispatch_engine`'s A4
/// settle so dialogue rules ride the same numeric-oracle lifecycle as task
/// rules. Reached only when `[task_forward_model] held_out_gate_enabled =
/// true` (the caller branches; gate-off keeps calling [`settle_detached`],
/// byte-identical).
///
/// Two independent populations settle here:
/// - `injected_ids` — rules injected into this turn's prompt, routed through
///   [`settle_injected_rules_held_out`] (helper polarity: hit ⇔ the turn went
///   well) against the domain-agnostic [`DEFAULT_BASELINE_HIT_RATE`], with
///   the injected batch itself as the Bonferroni family — the same two
///   choices `dispatch_engine` makes for its injected settle.
/// - `armed_shadow_ids` — shadow candidates whose signals matched this turn
///   at prompt-build time (`playbook::collect_armed_shadow`), scored via
///   [`score_shadow_candidates_by_ids`] (risk-predictor polarity: hit ⇔ the
///   turn was high-risk) against `shadow_baseline_hit_rate` — the caller
///   passes the agent's dialogue climatology
///   (`PredictionEngine::high_risk_base_rate`) or its coin-flip fallback.
///
/// Detached so it never delays reply delivery; failures only log.
#[allow(clippy::too_many_arguments)]
pub fn settle_detached_held_out(
    db_path: PathBuf,
    agent_id: String,
    injected_ids: Vec<String>,
    armed_shadow_ids: Vec<String>,
    shadow_family_k: usize,
    category: ErrorCategory,
    shadow_baseline_hit_rate: f64,
) {
    if injected_ids.is_empty() && armed_shadow_ids.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(engine) => engine,
            Err(e) => {
                warn!(agent = %agent_id, "held-out settle: open memory engine failed: {e}");
                return;
            }
        };
        let now_seq = chrono::Utc::now().timestamp().max(0) as u64;
        if !injected_ids.is_empty() {
            let retired = settle_injected_rules_held_out(
                &engine,
                &agent_id,
                &injected_ids,
                category,
                injected_ids.len(),
                DEFAULT_BASELINE_HIT_RATE,
                now_seq,
            )
            .await;
            if !retired.is_empty() {
                info!(
                    agent = %agent_id,
                    retired = retired.len(),
                    "rule lifecycle: held-out gate retired spent rules"
                );
            }
        }
        if !armed_shadow_ids.is_empty() {
            let pass = score_shadow_candidates_by_ids(
                &engine,
                &agent_id,
                &armed_shadow_ids,
                shadow_family_k,
                category,
                shadow_baseline_hit_rate,
                now_seq,
            )
            .await;
            if pass.scored > 0 {
                info!(
                    agent = %agent_id,
                    scored = pass.scored,
                    promoted = pass.promoted.len(),
                    retired = pass.retired.len(),
                    baseline = shadow_baseline_hit_rate,
                    "dialogue shadow-scoring pass"
                );
            }
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

    // ── v1.54: shadow → promotion prequential scoring pass ────────────────

    /// Score-polarity of a shadow (risk-predictor) candidate is the mirror of
    /// the injected (helper) path.
    #[test]
    fn score_shadow_candidate_hits_on_high_risk_only() {
        assert!(score_shadow_candidate(ErrorCategory::Significant));
        assert!(score_shadow_candidate(ErrorCategory::Critical));
        assert!(!score_shadow_candidate(ErrorCategory::Negligible));
        assert!(!score_shadow_candidate(ErrorCategory::Moderate));
    }

    #[test]
    fn update_held_out_and_gate_skips_birth_batch_but_still_evaluates() {
        let born = HeldOutStats::born(1000);
        // now_seq == born_seq → not recorded; n = 0 → Shadow.
        let (s, d) = update_held_out_and_gate(born, ErrorCategory::Critical, 1000, 1, 0.3);
        assert_eq!(s.total(), 0, "birth-batch observation not counted");
        assert_eq!(d, GateDecision::Shadow);
    }

    /// Store a shadow task-layer rule carrying a `goal_kind` tag and a
    /// pre-seeded held-out record — the fixture the shadow→promotion pass
    /// consumes.
    async fn store_shadow_task_rule(
        engine: &SqliteMemoryEngine,
        agent: &str,
        goal_kind_tag: &str,
        held: HeldOutStats,
    ) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: format!("shadow risk rule for {goal_kind_tag}"),
            timestamp: chrono::Utc::now(),
            tags: vec![
                TASK_RULE_TAG.to_string(),
                SHADOW_RULE_TAG.to_string(),
                goal_kind_tag.to_string(),
            ],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: TASK_RULE_SOURCE_EVENT.to_string(),
        };
        let mut metadata = serde_json::json!({ "rule_stats": RuleStats::initial() });
        held.merge_into(&mut metadata);
        let meta = TemporalMeta {
            metadata: Some(metadata),
            ..Default::default()
        };
        engine.store_temporal(agent, entry, meta).await.unwrap()
    }

    #[tokio::test]
    async fn shadow_pass_promotes_after_enough_high_risk_hits() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-promote";
        let tag = "goal_kind:coding_simple";
        // Born at 100; every settle below is at now=200 (strictly after) so it
        // counts as out-of-sample.
        let id = store_shadow_task_rule(&engine, agent, tag, HeldOutStats::born(100)).await;

        // Frozen baseline 0.3; 10 high-risk (Critical) settles → the rule's
        // trigger keeps correctly flagging risk → its Wilson lower bound clears
        // the baseline and it is promoted out of shadow.
        for _ in 0..10 {
            score_shadow_candidates_for_task(&engine, agent, tag, ErrorCategory::Critical, 0.3, 200)
                .await;
        }

        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            !entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "a strong out-of-sample risk-prediction record is promoted out of shadow"
        );
        assert!(
            !entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "promotion is not retirement"
        );
        // Now selectable for injection.
        let sel = select_task_rules(&engine, agent, TASK_RULE_INJECTION_LIMIT).await;
        assert!(sel.iter().any(|r| r.id == id), "promoted rule becomes injectable");
    }

    #[tokio::test]
    async fn shadow_pass_undetermined_record_stays_shadow() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-undetermined";
        let tag = "goal_kind:research_or_qa";
        let id = store_shadow_task_rule(&engine, agent, tag, HeldOutStats::born(0)).await;

        // 5 hits + 5 misses at baseline 0.5 → CI straddles the baseline →
        // not decidable → stays a shadow candidate (neither promoted nor
        // retired).
        for _ in 0..5 {
            score_shadow_candidates_for_task(&engine, agent, tag, ErrorCategory::Critical, 0.5, 10)
                .await;
        }
        for _ in 0..5 {
            score_shadow_candidates_for_task(
                &engine, agent, tag, ErrorCategory::Negligible, 0.5, 10,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == SHADOW_RULE_TAG), "still shadow");
        assert!(!entry.tags.iter().any(|t| t == RETIRED_RULE_TAG), "not retired");
    }

    #[tokio::test]
    async fn shadow_pass_retires_persistent_false_alarm() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-retire";
        let tag = "goal_kind:coding_simple";
        let id = store_shadow_task_rule(&engine, agent, tag, HeldOutStats::born(0)).await;

        // Baseline 0.5; 16 low-risk (Negligible) settles = 16 false alarms →
        // even the optimistic upper bound falls below baseline → retire.
        for _ in 0..16 {
            score_shadow_candidates_for_task(
                &engine, agent, tag, ErrorCategory::Negligible, 0.5, 10,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "a spent false-alarm risk predictor is retired"
        );
        assert!(entry.importance < 2.0, "importance demoted on retirement");
    }

    #[tokio::test]
    async fn shadow_pass_skips_birth_batch() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-birth";
        let tag = "goal_kind:coding_simple";
        let id = store_shadow_task_rule(&engine, agent, tag, HeldOutStats::born(500)).await;

        // now_seq == born_seq → birth batch, not counted.
        score_shadow_candidates_for_task(&engine, agent, tag, ErrorCategory::Critical, 0.3, 500)
            .await;
        let held =
            HeldOutStats::from_metadata(&engine.get_metadata(agent, &id).await.unwrap().unwrap());
        assert_eq!(held.total(), 0, "birth-batch settle (now_seq <= born_seq) must not count");

        // A strictly later settle IS counted.
        score_shadow_candidates_for_task(&engine, agent, tag, ErrorCategory::Critical, 0.3, 501)
            .await;
        let held =
            HeldOutStats::from_metadata(&engine.get_metadata(agent, &id).await.unwrap().unwrap());
        assert_eq!(held.oos_hits, 1, "an out-of-sample settle (now_seq > born_seq) counts");
    }

    #[tokio::test]
    async fn shadow_pass_larger_k_makes_promotion_harder() {
        // A borderline 9/1 record promotes at k=1 but not once the concurrent
        // shadow-candidate family is large (Bonferroni). born_seq is set above
        // now_seq so the pass's own observation is a birth-batch skip, leaving
        // the pre-seeded 9/1 record intact — the gate is then evaluated on
        // exactly 9/1 in both cases, isolating the effect of k.
        let strong = HeldOutStats {
            oos_hits: 9,
            oos_misses: 1,
            born_seq: 1000,
            recent: vec![true; 8],
        };
        let tag = "goal_kind:coding_simple";

        // k = 1: only the target shadow rule exists → promoted.
        let e1 = SqliteMemoryEngine::in_memory().unwrap();
        let a1 = "agent-k1";
        let t1 = store_shadow_task_rule(&e1, a1, tag, strong.clone()).await;
        score_shadow_candidates_for_task(&e1, a1, tag, ErrorCategory::Critical, 0.5, 500).await;
        let entry = e1.get_by_id(a1, &t1).await.unwrap().unwrap();
        assert!(
            !entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "at k=1 the 9/1 record promotes"
        );

        // k = 20: 19 extra active shadow candidates widen the Bonferroni z →
        // the SAME 9/1 record no longer clears the baseline → stays shadow.
        let e2 = SqliteMemoryEngine::in_memory().unwrap();
        let a2 = "agent-k20";
        let t2 = store_shadow_task_rule(&e2, a2, tag, strong).await;
        for _ in 0..19 {
            store_shadow_task_rule(&e2, a2, tag, HeldOutStats::born(0)).await;
        }
        score_shadow_candidates_for_task(&e2, a2, tag, ErrorCategory::Critical, 0.5, 500).await;
        let entry = e2.get_by_id(a2, &t2).await.unwrap().unwrap();
        assert!(
            entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "a large concurrent-candidate family must block a promotion that passed at k=1"
        );
    }

    // ── dialogue-layer shadow-scoring by armed ids (v1.54 debt closure) ──

    /// Store a dialogue-layer (reflexion) shadow rule with a pre-seeded
    /// held-out record — the fixture the by-ids pass consumes.
    async fn store_shadow_dialogue_rule(
        engine: &SqliteMemoryEngine,
        agent: &str,
        held: HeldOutStats,
    ) -> String {
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "shadow inductive lesson".to_string(),
            timestamp: chrono::Utc::now(),
            tags: vec!["reflexion".to_string(), SHADOW_RULE_TAG.to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: RULE_SOURCE_EVENT.to_string(),
        };
        let mut metadata = serde_json::json!({ "rule_stats": RuleStats::initial() });
        held.merge_into(&mut metadata);
        let meta = TemporalMeta {
            metadata: Some(metadata),
            ..Default::default()
        };
        engine.store_temporal(agent, entry, meta).await.unwrap()
    }

    #[tokio::test]
    async fn by_ids_pass_promotes_after_enough_high_risk_hits() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-byids-promote";
        let id = store_shadow_dialogue_rule(&engine, agent, HeldOutStats::born(100)).await;
        let armed = vec![id.clone()];

        // Frozen baseline 0.3; 10 high-risk settles strictly after birth →
        // Wilson lower bound clears the baseline → promoted out of shadow.
        for _ in 0..10 {
            score_shadow_candidates_by_ids(
                &engine, agent, &armed, 1, ErrorCategory::Critical, 0.3, 200,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            !entry.tags.iter().any(|t| t == SHADOW_RULE_TAG),
            "a strong out-of-sample risk-prediction record is promoted out of shadow"
        );
        // Promoted dialogue rules become selectable on the F2a path.
        let sel = select_rules(&engine, agent, INJECTION_LIMIT).await;
        assert!(sel.iter().any(|r| r.id == id), "promoted rule becomes injectable");
    }

    #[tokio::test]
    async fn by_ids_pass_retires_persistent_false_alarm() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-byids-retire";
        let id = store_shadow_dialogue_rule(&engine, agent, HeldOutStats::born(0)).await;
        let armed = vec![id.clone()];

        // 16 low-risk settles = 16 false alarms at baseline 0.5 → even the
        // optimistic upper bound falls below baseline → retire.
        for _ in 0..16 {
            score_shadow_candidates_by_ids(
                &engine, agent, &armed, 1, ErrorCategory::Negligible, 0.5, 10,
            )
            .await;
        }
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(
            entry.tags.iter().any(|t| t == RETIRED_RULE_TAG),
            "a spent false-alarm risk predictor is retired"
        );
        assert!(entry.importance < 2.0, "importance demoted on retirement");
    }

    #[tokio::test]
    async fn by_ids_pass_skips_birth_batch_and_counts_later_settles() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-byids-birth";
        let id = store_shadow_dialogue_rule(&engine, agent, HeldOutStats::born(500)).await;
        let armed = vec![id.clone()];

        // now_seq == born_seq → birth batch, not counted (train != test).
        let out = score_shadow_candidates_by_ids(
            &engine, agent, &armed, 1, ErrorCategory::Critical, 0.3, 500,
        )
        .await;
        assert_eq!(out.scored, 1, "the candidate is still visited (gate evaluated)");
        let held =
            HeldOutStats::from_metadata(&engine.get_metadata(agent, &id).await.unwrap().unwrap());
        assert_eq!(held.total(), 0, "birth-batch settle (now_seq <= born_seq) must not count");

        // A strictly later settle IS counted.
        score_shadow_candidates_by_ids(&engine, agent, &armed, 1, ErrorCategory::Critical, 0.3, 501)
            .await;
        let held =
            HeldOutStats::from_metadata(&engine.get_metadata(agent, &id).await.unwrap().unwrap());
        assert_eq!(held.oos_hits, 1, "an out-of-sample settle (now_seq > born_seq) counts");
    }

    #[tokio::test]
    async fn by_ids_pass_skips_rows_no_longer_shadow() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-byids-stale-arm";
        // Armed as shadow, then promoted by a concurrent settle before this
        // one lands → the stale armed id must be skipped untouched.
        let promoted = store_shadow_dialogue_rule(&engine, agent, HeldOutStats::born(0)).await;
        engine.remove_tag(agent, &promoted, SHADOW_RULE_TAG).await.unwrap();
        // Armed, then retired concurrently → also skipped.
        let retired = store_shadow_dialogue_rule(&engine, agent, HeldOutStats::born(0)).await;
        engine
            .set_importance_and_add_tag(agent, &retired, 1.0, RETIRED_RULE_TAG)
            .await
            .unwrap();

        let out = score_shadow_candidates_by_ids(
            &engine,
            agent,
            &[promoted.clone(), retired.clone(), "no-such-id".to_string()],
            3,
            ErrorCategory::Critical,
            0.3,
            10,
        )
        .await;
        assert_eq!(out.scored, 0, "stale armed ids are skipped, not scored");
        for id in [&promoted, &retired] {
            let held = HeldOutStats::from_metadata(
                &engine.get_metadata(agent, id).await.unwrap().unwrap(),
            );
            assert_eq!(held.total(), 0, "no out-of-sample record written for a stale armed id");
        }
    }

    #[tokio::test]
    async fn shadow_pass_ignores_non_matching_signal() {
        // A shadow rule whose goal_kind tag does NOT match the current task
        // situation is left completely untouched — no held-out record is
        // written, so it can never be promoted or retired by an unrelated
        // situation.
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-shadow-nomatch";
        let id =
            store_shadow_task_rule(&engine, agent, "goal_kind:coding_simple", HeldOutStats::born(0))
                .await;

        let out = score_shadow_candidates_for_task(
            &engine,
            agent,
            "goal_kind:ops_or_external", // different situation
            ErrorCategory::Critical,
            0.3,
            10,
        )
        .await;
        assert_eq!(out.scored, 0, "a non-matching shadow rule is not scored");

        let held =
            HeldOutStats::from_metadata(&engine.get_metadata(agent, &id).await.unwrap().unwrap());
        assert_eq!(held.total(), 0, "no out-of-sample record written for a non-match");
        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == SHADOW_RULE_TAG), "still shadow, untouched");
    }
}
