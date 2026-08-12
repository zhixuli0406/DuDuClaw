//! §1.7.2/1.7.3 stale + capacity sweep (G5), reusing Ebbinghaus retrievability.
//!
//! `duduclaw_memory::decay::run_decay`'s candidate SQL explicitly excludes
//! the semantic layer (`layer != 'semantic'`), so playbook entries — which
//! live on the semantic layer by design (§1.1) — are never touched by it.
//! This module is the playbook-specific sweeper that fills that gap.
//!
//! Deviation from the design doc's file-level checklist (reported, not
//! silently made): the design suggested hooking this into
//! `duduclaw-agent::HeartbeatScheduler::run`'s tick body. That crate does
//! NOT depend on `duduclaw-gateway` (it's the other way around —
//! `duduclaw-gateway` depends on `duduclaw-agent`), so calling
//! `crate::playbook::sweep` from `duduclaw-agent/src/heartbeat.rs` would
//! introduce a dependency cycle. Instead this module exposes its own
//! gateway-owned periodic loop ([`spawn_playbook_sweep_loop`]), following the
//! exact same pattern already used for `night_engine::spawn_night_engine`
//! (also gateway-owned, also scans the shared agent registry on an interval)
//! — same throttle intent (24h/agent), smaller diff, no new crate edge.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use duduclaw_memory::engine::{ebbinghaus_retrievability, RetrievalWeights};
use duduclaw_memory::SqliteMemoryEngine;

use super::entry::{
    PlaybookCategory, PlaybookMeta, PlaybookState, LEGACY_RULE_SOURCE_EVENT, PLAYBOOK_MAX_ENTRIES,
    PLAYBOOK_SOURCE_EVENT,
};
use crate::prediction::rule_lifecycle::{RuleStats, RETIRED_RULE_TAG};

/// Retrievability below which an Active entry is parked as Stale. Aligned
/// with `duduclaw_memory::decay::MemoryDecayPolicy::default().min_retrievability`
/// so playbook and memory age on the same curve.
pub const STALE_RETRIEVABILITY: f64 = 0.05;
/// Days a Stale entry may sit before the sweep retires it outright. Measured
/// against the row's raw age (no separate `stale_since` field exists in the
/// schema — see module test doc for the exact interpretation).
pub const STALE_RETIRE_AFTER_DAYS: i64 = 90;
const RETIRED_IMPORTANCE: f64 = 1.0;

/// Throttle: how often one agent's playbook is actually swept.
pub const SWEEP_THROTTLE_HOURS: i64 = 24;

#[derive(Debug, Default, Clone)]
pub struct SweepReport {
    pub staled: Vec<String>,
    pub retired: Vec<String>,
    pub evicted: Vec<String>,
    /// G6: rule ids flagged source-stale this pass (a recorded source fact was
    /// superseded). Distinct from `staled` above (Ebbinghaus decay) — this is
    /// content-freshness, not recency.
    pub source_staled: Vec<String>,
}

/// §1.7.2: pure retrievability check, no I/O.
pub fn is_stale(
    last_accessed: Option<DateTime<Utc>>,
    timestamp: DateTime<Utc>,
    access_count: u32,
    importance: f64,
    now: DateTime<Utc>,
    w: &RetrievalWeights,
) -> bool {
    let anchor = last_accessed.unwrap_or(timestamp);
    let days = (now - anchor).num_seconds().max(0) as f64 / 86_400.0;
    ebbinghaus_retrievability(days, access_count, importance, w) < STALE_RETRIEVABILITY
}

/// §1.7.3 local GDI (intrinsic 0.40 / usage 0.30 / freshness 0.30 — the
/// paper's 35/30/20/15 with the "community" dimension dropped, redistributed
/// proportionally over the remaining three).
pub fn gdi_score(
    net: i64,
    success_streak: u32,
    access_count: u32,
    last_accessed: Option<DateTime<Utc>>,
    timestamp: DateTime<Utc>,
    importance: f64,
    now: DateTime<Utc>,
    w: &RetrievalWeights,
) -> f64 {
    let intrinsic = (net as f64 / 6.0).clamp(0.0, 1.0) * 0.6 + (success_streak as f64 / 5.0).clamp(0.0, 1.0) * 0.4;
    let usage = ((1.0 + access_count as f64).ln() / 21.0f64.ln()).clamp(0.0, 1.0);
    let anchor = last_accessed.unwrap_or(timestamp);
    let days = (now - anchor).num_seconds().max(0) as f64 / 86_400.0;
    let freshness = ebbinghaus_retrievability(days, access_count, importance, w);
    0.40 * intrinsic + 0.30 * usage + 0.30 * freshness
}

/// One sweep pass for `agent_id`: stale transition, stale→retired after
/// [`STALE_RETIRE_AFTER_DAYS`], then capacity eviction down to
/// [`PLAYBOOK_MAX_ENTRIES`]. `now` is a parameter for determinism/testability.
/// `Regulatory` entries are exempt from BOTH the stale transition and
/// capacity eviction (operator/compliance content should never be
/// auto-retired — §1.7.3), though they still count toward the 40-entry
/// denominator.
pub async fn run_playbook_sweep(engine: &SqliteMemoryEngine, agent_id: &str, now: DateTime<Utc>) -> SweepReport {
    let weights = RetrievalWeights::default();
    let mut report = SweepReport::default();

    // G6: propagate fact-layer supersession up to the rule layer. Runs first
    // (before the Ebbinghaus stale/eviction passes below) and independently —
    // rules with no recorded source facts are untouched (fail-open). Marks the
    // `SOURCE_STALE_RULE_TAG` used by the injection downweight/marker.
    report.source_staled =
        crate::prediction::rule_staleness::refresh_rule_source_staleness(engine, agent_id).await;

    // Pass 1: Active→Stale, Stale→Retired.
    let mut still_active_probation: Vec<(String, i64, u32, u32, Option<DateTime<Utc>>, DateTime<Utc>, f64)> = Vec::new();

    for source_event in [PLAYBOOK_SOURCE_EVENT, LEGACY_RULE_SOURCE_EVENT] {
        let rows = match engine.list_valid_by_source_event(agent_id, source_event, 200).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        for (mem_entry, metadata) in rows {
            if mem_entry.tags.iter().any(|t| t == RETIRED_RULE_TAG) {
                continue;
            }
            let Some(mut meta) = PlaybookMeta::from_metadata(&metadata) else {
                continue; // legacy row never upgraded yet — store::list_active owns that
            };
            if meta.category == PlaybookCategory::Regulatory {
                continue;
            }

            match meta.state {
                PlaybookState::Active => {
                    if is_stale(mem_entry.last_accessed, mem_entry.timestamp, mem_entry.access_count, mem_entry.importance, now, &weights) {
                        meta.state = PlaybookState::Stale;
                        let mut new_metadata = metadata.clone();
                        meta.merge_into(&mut new_metadata);
                        if engine.update_metadata(agent_id, &mem_entry.id, &new_metadata).await.unwrap_or(false) {
                            report.staled.push(mem_entry.id.clone());
                        }
                    } else {
                        let stats = RuleStats::from_metadata(&metadata);
                        still_active_probation.push((
                            mem_entry.id.clone(),
                            stats.net(),
                            meta.success_streak,
                            mem_entry.access_count,
                            mem_entry.last_accessed,
                            mem_entry.timestamp,
                            mem_entry.importance,
                        ));
                    }
                }
                PlaybookState::Probation => {
                    let stats = RuleStats::from_metadata(&metadata);
                    still_active_probation.push((
                        mem_entry.id.clone(),
                        stats.net(),
                        meta.success_streak,
                        mem_entry.access_count,
                        mem_entry.last_accessed,
                        mem_entry.timestamp,
                        mem_entry.importance,
                    ));
                }
                PlaybookState::Stale => {
                    let anchor = mem_entry.last_accessed.unwrap_or(mem_entry.timestamp);
                    let days = (now - anchor).num_seconds().max(0) as f64 / 86_400.0;
                    if days >= STALE_RETIRE_AFTER_DAYS as f64 {
                        meta.state = PlaybookState::Retired;
                        let mut new_metadata = metadata.clone();
                        meta.merge_into(&mut new_metadata);
                        let _ = engine.update_metadata(agent_id, &mem_entry.id, &new_metadata).await;
                        let _ = engine
                            .set_importance_and_add_tag(agent_id, &mem_entry.id, RETIRED_IMPORTANCE, RETIRED_RULE_TAG)
                            .await;
                        report.retired.push(mem_entry.id.clone());
                    }
                }
                PlaybookState::Retired => {}
            }
        }
    }

    // Pass 2: capacity eviction — Active+Probation only, non-Regulatory
    // (already excluded above since only those states were pushed).
    if still_active_probation.len() > PLAYBOOK_MAX_ENTRIES {
        let mut scored: Vec<(String, f64)> = still_active_probation
            .into_iter()
            .map(|(id, net, streak, ac, la, ts, imp)| {
                (id, gdi_score(net, streak, ac, la, ts, imp, now, &weights))
            })
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let evict_count = scored.len() - PLAYBOOK_MAX_ENTRIES;
        for (id, _score) in scored.into_iter().take(evict_count) {
            if let Ok(Some(mut metadata)) = engine.get_metadata(agent_id, &id).await {
                if let Some(mut meta) = PlaybookMeta::from_metadata(&metadata) {
                    meta.state = PlaybookState::Stale;
                    meta.merge_into(&mut metadata);
                    if engine.update_metadata(agent_id, &id, &metadata).await.unwrap_or(false) {
                        report.evicted.push(id);
                    }
                }
            }
        }
    }

    report
}

/// Gateway-owned periodic sweep loop (see module doc for why this isn't in
/// `duduclaw-agent::HeartbeatScheduler`). Mirrors
/// `night_engine::spawn_night_engine`'s shape: scan the shared registry on
/// an interval, open `memory.db` once per tick, sweep every agent due for
/// it. Throttled per-agent to [`SWEEP_THROTTLE_HOURS`] via an in-process
/// `HashMap` (resets on gateway restart — harmless, the sweep is idempotent
/// and cheap to run slightly early).
pub fn spawn_playbook_sweep_loop(
    home_dir: PathBuf,
    registry: Arc<RwLock<duduclaw_agent::registry::AgentRegistry>>,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let memory_db = home_dir.join("memory.db");
        let mut last_swept: HashMap<String, DateTime<Utc>> = HashMap::new();
        tokio::time::sleep(Duration::from_secs(60)).await; // let boot settle, matches night_engine's own delay
        loop {
            tokio::time::sleep(Duration::from_secs(interval_secs.max(30))).await;

            let agent_ids: Vec<String> = {
                let reg = registry.read().await;
                reg.list().iter().map(|a| a.config.agent.name.clone()).collect()
            };
            if agent_ids.is_empty() {
                continue;
            }

            let engine = match SqliteMemoryEngine::new(&memory_db) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "playbook sweep: cannot open memory.db, skipping cycle");
                    continue;
                }
            };

            let now = Utc::now();
            for agent_id in agent_ids {
                let due = last_swept
                    .get(&agent_id)
                    .map(|t| (now - *t).num_hours() >= SWEEP_THROTTLE_HOURS)
                    .unwrap_or(true);
                if !due {
                    continue;
                }
                let report = run_playbook_sweep(&engine, &agent_id, now).await;
                last_swept.insert(agent_id.clone(), now);
                if !report.staled.is_empty()
                    || !report.retired.is_empty()
                    || !report.evicted.is_empty()
                    || !report.source_staled.is_empty()
                {
                    tracing::info!(
                        agent = %agent_id,
                        staled = report.staled.len(),
                        retired = report.retired.len(),
                        evicted = report.evicted.len(),
                        source_staled = report.source_staled.len(),
                        "playbook sweep: state transitions"
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::{MemoryEntry, MemoryLayer};
    use duduclaw_memory::TemporalMeta;

    async fn store_entry(
        engine: &SqliteMemoryEngine,
        agent: &str,
        content: &str,
        state: PlaybookState,
        category: PlaybookCategory,
        importance: f64,
        timestamp: DateTime<Utc>,
    ) -> String {
        use crate::playbook::gene::EvalCaseRef;
        let meta = PlaybookMeta {
            schema_version: crate::playbook::entry::PLAYBOOK_SCHEMA_VERSION,
            category,
            signals_match: vec!["*".to_string()],
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            applications: Vec::new(),
            success_streak: 0,
            state,
            revision: 0,
            dedup_key: "0".repeat(16),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
            assertions: Default::default(),
        };
        let entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: content.to_string(),
            timestamp,
            tags: vec!["playbook".to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance,
            access_count: 0,
            last_accessed: None,
            source_event: PLAYBOOK_SOURCE_EVENT.to_string(),
        };
        let mut blob = serde_json::json!({"rule_stats": {"helpful": 1, "harmful": 0}});
        meta.merge_into(&mut blob);
        let temporal = TemporalMeta { metadata: Some(blob), ..Default::default() };
        engine.store_temporal(agent, entry, temporal).await.unwrap()
    }

    #[tokio::test]
    async fn old_untouched_active_entry_goes_stale() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sweep-stale";
        let old = Utc::now() - chrono::Duration::days(400);
        let id = store_entry(&engine, agent, "old rule", PlaybookState::Active, PlaybookCategory::Repair, 5.0, old).await;

        let report = run_playbook_sweep(&engine, agent, Utc::now()).await;
        assert!(report.staled.contains(&id));

        let meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        assert_eq!(PlaybookMeta::from_metadata(&meta).unwrap().state, PlaybookState::Stale);
    }

    #[tokio::test]
    async fn stale_entry_retires_after_90_days() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sweep-retire";
        let old = Utc::now() - chrono::Duration::days(400);
        let id = store_entry(&engine, agent, "very old rule", PlaybookState::Stale, PlaybookCategory::Repair, 5.0, old).await;

        let report = run_playbook_sweep(&engine, agent, Utc::now()).await;
        assert!(report.retired.contains(&id));

        let entry = engine.get_by_id(agent, &id).await.unwrap().unwrap();
        assert!(entry.tags.iter().any(|t| t == RETIRED_RULE_TAG));
    }

    #[tokio::test]
    async fn regulatory_entries_are_exempt_from_stale_transition() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sweep-regulatory";
        let old = Utc::now() - chrono::Duration::days(400);
        let id = store_entry(&engine, agent, "compliance rule", PlaybookState::Active, PlaybookCategory::Regulatory, 5.0, old).await;

        let report = run_playbook_sweep(&engine, agent, Utc::now()).await;
        assert!(!report.staled.contains(&id));
        let meta = engine.get_metadata(agent, &id).await.unwrap().unwrap();
        assert_eq!(PlaybookMeta::from_metadata(&meta).unwrap().state, PlaybookState::Active);
    }

    #[tokio::test]
    async fn capacity_eviction_stales_lowest_gdi_not_delete() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sweep-capacity";
        let now = Utc::now();
        // One more than the cap, all fresh so nothing goes Stale via pass 1.
        let mut ids = Vec::new();
        for i in 0..(PLAYBOOK_MAX_ENTRIES + 1) {
            let id = store_entry(&engine, agent, &format!("rule {i}"), PlaybookState::Active, PlaybookCategory::Repair, 8.0, now).await;
            ids.push(id);
        }
        let report = run_playbook_sweep(&engine, agent, now).await;
        assert_eq!(report.evicted.len(), 1, "exactly one entry evicted to get back to the cap");

        // Evicted entry still exists (Stale, not deleted).
        let evicted_id = &report.evicted[0];
        let meta = engine.get_metadata(agent, evicted_id).await.unwrap().unwrap();
        assert_eq!(PlaybookMeta::from_metadata(&meta).unwrap().state, PlaybookState::Stale);
        let still_readable = engine.get_by_id(agent, evicted_id).await.unwrap();
        assert!(still_readable.is_some(), "eviction never deletes the row");
    }

    #[tokio::test]
    async fn sweep_flags_rule_whose_source_fact_was_superseded() {
        use crate::prediction::rule_staleness::{record_source_facts, SOURCE_STALE_RULE_TAG};

        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let agent = "agent-sweep-source-stale";
        let now = Utc::now();

        // A source fact the rule depends on.
        let fact_entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "price is 100".to_string(),
            timestamp: now,
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "fact".to_string(),
        };
        let fact_id = engine
            .store_temporal(
                agent,
                fact_entry,
                TemporalMeta {
                    subject: Some("product:price".into()),
                    predicate: Some("is".into()),
                    object: Some("100".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // A fresh (non-decayed) playbook rule recording that source fact.
        let mut meta = PlaybookMeta {
            schema_version: crate::playbook::entry::PLAYBOOK_SCHEMA_VERSION,
            category: PlaybookCategory::Repair,
            signals_match: vec!["*".to_string()],
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: vec![crate::playbook::gene::EvalCaseRef("s/c".to_string())],
            applications: Vec::new(),
            success_streak: 0,
            state: PlaybookState::Active,
            revision: 0,
            dedup_key: "0".repeat(16),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
            assertions: Default::default(),
        };
        let rule_entry = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "when asked price answer 100".to_string(),
            timestamp: now,
            tags: vec!["playbook".to_string()],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 8.0,
            access_count: 0,
            last_accessed: None,
            source_event: PLAYBOOK_SOURCE_EVENT.to_string(),
        };
        let mut blob = serde_json::json!({"rule_stats": {"helpful": 1, "harmful": 0}});
        record_source_facts(&mut blob, &[fact_id.clone()]);
        meta.merge_into(&mut blob);
        let rule_id = engine
            .store_temporal(agent, rule_entry, TemporalMeta { metadata: Some(blob), ..Default::default() })
            .await
            .unwrap();

        // Before supersession: sweep does not flag it.
        let report = run_playbook_sweep(&engine, agent, now).await;
        assert!(!report.source_staled.contains(&rule_id));

        // Supersede the source fact with a newer value.
        let newer = MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: "price is 120".to_string(),
            timestamp: now,
            tags: vec![],
            embedding: None,
            layer: MemoryLayer::Semantic,
            importance: 5.0,
            access_count: 0,
            last_accessed: None,
            source_event: "fact".to_string(),
        };
        engine
            .store_temporal(
                agent,
                newer,
                TemporalMeta {
                    subject: Some("product:price".into()),
                    predicate: Some("is".into()),
                    object: Some("120".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Now the sweep propagates the supersession up to the rule.
        let report = run_playbook_sweep(&engine, agent, now).await;
        assert!(report.source_staled.contains(&rule_id), "rule flagged after its source was superseded");
        let stored = engine.get_by_id(agent, &rule_id).await.unwrap().unwrap();
        assert!(stored.tags.iter().any(|t| t == SOURCE_STALE_RULE_TAG));
    }

    #[test]
    fn gdi_score_rewards_net_streak_usage_and_freshness() {
        let w = RetrievalWeights::default();
        let now = Utc::now();
        let strong = gdi_score(6, 5, 20, Some(now), now, 8.0, now, &w);
        let weak = gdi_score(0, 0, 0, None, now - chrono::Duration::days(400), 1.0, now, &w);
        assert!(strong > weak);
    }
}
