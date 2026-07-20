//! D5 — semi-automatic topology evolution (edge optimization, **human-gated**).
//!
//! ## What this is (GPTSwarm 2402.16823 / AFlow 2410.10762 / ADAS 2408.08435)
//!
//! GVU / SOUL.md evolution only ever optimizes *nodes* (an agent's prompt). The
//! *edges* of the multi-agent graph — who a `reports_to` hierarchy routes a
//! task class to — have been static. This module makes that edge a
//! **proposal-only, human-approved** optimization target:
//!
//! 1. **Evidence analyzer** (pure, unit-tested): aggregates per-`(agent,
//!    task_class)` quality signals from the task store over the last N days —
//!    MAV/review reject rate, needs_human escalation rate, and goal-loop
//!    no-progress oscillation count.
//! 2. **Proposal**: when an agent's reject rate for a task class crosses a
//!    threshold with enough samples, and a `reports_to` **sibling** handles that
//!    class better, the driver files a `reroute` proposal — never a direct
//!    change.
//! 3. **Human gate (never bypassable)**: every proposal goes through the
//!    [`crate::approval::ApprovalBroker`] as an **always-human** action
//!    (`topology_reroute`). There is no LLM-judge path and no `autonomy_level`
//!    relaxation — the driver only ever calls `request` + `poll`, never an auto
//!    approve. TTL expiry counts as denial (broker fail-closed).
//! 4. **Observation window + auto-rollback**: an approved override is watched
//!    for `observe_hours` (default 24h). If the new agent does not actually beat
//!    the old agent's baseline reject rate for that class, the override is rolled
//!    back automatically (routing reverts). Insufficient samples extend the
//!    window once, then roll back (conservative convergence).
//! 5. **Anti-storm**: at most one proposal per `(task_class, from_agent)` within
//!    `proposal_cooldown_days` (default 7), tracked durably in the override file.
//!
//! ## Failure posture / conventions
//!
//! - **Default OFF.** `[topology_evolution] enabled = false` by default: the
//!   evidence analyzer never runs, no proposals are filed, and the dispatch
//!   lookup path is byte-identical to plain `FixedHierarchy`.
//! - **Fail-safe routing.** A missing / corrupt `routing_overrides.json` is
//!   treated as "no override" — dispatch reverts to the current
//!   `assigned_to` (never strands a task).
//! - **Cross-process safe writes.** Every read-modify-write of the override file
//!   goes through [`duduclaw_core::with_file_lock`] + atomic temp/rename
//!   (project convention #3).
//! - **Exact matching.** `task_class` / agent ids are compared by exact equality
//!   (project convention #2), never substring.
//!
//! This module keeps its **own** durable state (the override file); it does not
//! share the goal-loop or SOUL.md observation-window state — it only mirrors the
//! 24h-window + rollback *pattern*.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalStatus};
use crate::dispatch_policy::{task_class, DispatchPolicy, DispatchPolicyKind, FixedHierarchy};
use crate::events_store::EventBusStore;
use crate::task_store::{ActivityRow, TaskRow, TaskStore};

/// Action kind used for the ApprovalBroker request. This is an **always-human**
/// action class (no LLM judge, no autonomy relaxation).
pub const ACTION_KIND_REROUTE: &str = "topology_reroute";

/// Statuses a routing override can hold (durable text in the file).
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_ROLLED_BACK: &str = "rolled_back";
pub const STATUS_CONFIRMED: &str = "confirmed";

/// Statuses a proposal record can hold.
pub const PROPOSAL_PENDING: &str = "pending";
pub const PROPOSAL_APPROVED: &str = "approved";
pub const PROPOSAL_DENIED: &str = "denied";

/// The activity event-type feed prefix (dashboard Activity tab).
const ACT_PROPOSED: &str = "topology.proposed";
const ACT_APPROVED: &str = "topology.approved";
const ACT_REJECTED: &str = "topology.rejected";
const ACT_ROLLED_BACK: &str = "topology.rolled_back";
const ACT_CONFIRMED: &str = "topology.confirmed";
const ACT_EXTENDED: &str = "topology.extended";

// ── Config ──────────────────────────────────────────────────

/// Tuning for D5, from `config.toml [topology_evolution]`. **Default OFF.**
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct TopologyEvolutionConfig {
    /// Master switch — the whole feature is off unless this is `true`.
    pub enabled: bool,
    /// Evidence lookback window (days).
    pub lookback_days: i64,
    /// Minimum decided samples for a `(agent, task_class)` cell to count.
    pub min_samples: u32,
    /// Reject-rate at/above which a reroute is proposed.
    pub reject_rate_threshold: f64,
    /// Observation window after an override is approved (hours).
    pub observe_hours: i64,
    /// Anti-storm: min days between proposals for the same `(class, from_agent)`.
    pub proposal_cooldown_days: i64,
    /// Driver tick cadence (seconds).
    pub tick_secs: u64,
    /// TTL for a reroute approval request (seconds). Expiry = denial.
    pub approval_ttl_secs: i64,
}

impl Default for TopologyEvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lookback_days: 14,
            min_samples: 5,
            reject_rate_threshold: 0.6,
            observe_hours: 24,
            proposal_cooldown_days: 7,
            tick_secs: 3600,
            approval_ttl_secs: 86_400,
        }
    }
}

impl TopologyEvolutionConfig {
    /// Load `[topology_evolution]` from `<home>/config.toml`. Parsed in isolation
    /// from a generic `toml::Table` so unrelated / malformed config elsewhere can
    /// never make this fail — absent / malformed ⇒ defaults (feature stays OFF).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("topology_evolution") {
            Some(section) => section
                .clone()
                .try_into::<TopologyEvolutionConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// Whether D5 is enabled for this home dir (cheap config read).
pub fn enabled(home_dir: &Path) -> bool {
    TopologyEvolutionConfig::from_home(home_dir).enabled
}

// ── Evidence analyzer (pure) ────────────────────────────────

/// Aggregated quality signals for one `(agent, task_class)` cell.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvidenceMetrics {
    /// Decided goal-mode samples in the window (`done`/`needs_human`/`failed`).
    pub samples: u32,
    /// Samples that carry a rejection signal (see [`aggregate_metrics`]).
    pub rejects: u32,
    /// `rejects / samples` (0.0 when `samples == 0`).
    pub reject_rate: f64,
    /// Samples that ended in `needs_human`.
    pub needs_human: u32,
    /// Goal-loop no-progress oscillation events over these samples.
    pub oscillation: u32,
    /// Up to 10 sample task ids (evidence payload).
    pub sample_task_ids: Vec<String>,
}

/// True when `ts` (RFC3339) is a past instant within `days` of `now`.
/// Unparseable / future timestamps ⇒ `false` (excluded — never count ambiguous
/// data as evidence).
fn within_days(ts: &str, now: DateTime<Utc>, days: i64) -> bool {
    match DateTime::parse_from_rfc3339(ts) {
        Ok(t) => {
            let age = now - t.with_timezone(&Utc);
            age.num_seconds() >= 0 && age <= chrono::Duration::days(days)
        }
        Err(_) => false,
    }
}

/// Statuses that count as a *decided* goal-mode outcome (a usable sample). An
/// in-flight task (todo/pending/in_progress/review) is not yet decided;
/// `cancelled` is a human/system abort, not a quality signal — both excluded.
fn is_decided(status: &str) -> bool {
    matches!(status, "done" | "needs_human" | "failed")
}

/// Aggregate per-`(agent, task_class)` evidence over the lookback window.
///
/// Sample base = **goal-mode** tasks assigned to a concrete agent, `updated_at`
/// within `lookback_days`, in a decided status. A sample counts as *rejected*
/// when it ended in `needs_human`/`failed` OR needed at least one retry
/// (`retry_count > 0`, i.e. the MAV/review rejected it and it was requeued).
/// `oscillation_by_task` maps a task id → count of goal-loop no-progress
/// oscillation events for it (the caller derives this from the activity feed).
pub fn aggregate_metrics(
    tasks: &[TaskRow],
    oscillation_by_task: &HashMap<String, u32>,
    now: DateTime<Utc>,
    lookback_days: i64,
) -> BTreeMap<(String, String), EvidenceMetrics> {
    let mut out: BTreeMap<(String, String), EvidenceMetrics> = BTreeMap::new();
    for t in tasks {
        if !t.goal_mode {
            continue;
        }
        let agent = t.assigned_to.trim();
        if agent.is_empty() {
            continue;
        }
        if !is_decided(&t.status) {
            continue;
        }
        if !within_days(&t.updated_at, now, lookback_days) {
            continue;
        }
        let class = task_class(t);
        let cell = out.entry((agent.to_string(), class)).or_default();
        cell.samples += 1;
        let rejected = t.status == "needs_human" || t.status == "failed" || t.retry_count > 0;
        if rejected {
            cell.rejects += 1;
        }
        if t.status == "needs_human" {
            cell.needs_human += 1;
        }
        cell.oscillation += oscillation_by_task.get(&t.id).copied().unwrap_or(0);
        if cell.sample_task_ids.len() < 10 {
            cell.sample_task_ids.push(t.id.clone());
        }
    }
    for cell in out.values_mut() {
        cell.reject_rate = if cell.samples == 0 {
            0.0
        } else {
            cell.rejects as f64 / cell.samples as f64
        };
    }
    out
}

/// A reroute proposal derived from the evidence (pre-approval).
#[derive(Debug, Clone, PartialEq)]
pub struct RerouteProposal {
    pub task_class: String,
    pub from_agent: String,
    pub to_agent: String,
    /// `from_agent`'s reject rate — the baseline the observation window must beat.
    pub baseline_reject_rate: f64,
    /// `to_agent`'s current reject rate for the class (the reason to prefer it).
    pub to_reject_rate: f64,
    /// `from_agent`'s sample count (evidence).
    pub samples: u32,
    /// Up to 10 offending sample task ids (evidence payload).
    pub sample_task_ids: Vec<String>,
}

/// Select reroute proposals from the evidence.
///
/// For every `(from_agent, class)` cell whose `samples >= min_samples` and
/// `reject_rate >= reject_rate_threshold`, the candidate target is the
/// `reports_to` **sibling** (same parent — both `None`, i.e. roots, count as
/// siblings of each other) with the **lowest** reject rate for that class and
/// `samples >= min_samples`. A proposal is emitted only when that sibling
/// actually beats the offender's baseline (`to_reject_rate < baseline`).
/// No qualifying sibling ⇒ no proposal (empty result beats a fabricated one).
/// The result is sorted worst-offender first.
pub fn select_reroute(
    metrics: &BTreeMap<(String, String), EvidenceMetrics>,
    parent_of: &HashMap<String, Option<String>>,
    cfg: &TopologyEvolutionConfig,
) -> Vec<RerouteProposal> {
    let mut proposals = Vec::new();
    for ((from_agent, class), m) in metrics {
        if m.samples < cfg.min_samples || m.reject_rate < cfg.reject_rate_threshold {
            continue;
        }
        // The offender must be a known agent so its parent is resolvable.
        let Some(from_parent) = parent_of.get(from_agent) else {
            continue;
        };
        // Best sibling = same parent, has metrics for this class with enough
        // samples, lowest reject rate.
        let mut best: Option<(&String, f64)> = None;
        for (candidate, cand_parent) in parent_of {
            if candidate == from_agent || cand_parent != from_parent {
                continue;
            }
            let Some(cm) = metrics.get(&(candidate.clone(), class.clone())) else {
                continue;
            };
            if cm.samples < cfg.min_samples {
                continue;
            }
            match best {
                Some((_, best_rate)) if cm.reject_rate >= best_rate => {}
                _ => best = Some((candidate, cm.reject_rate)),
            }
        }
        if let Some((to_agent, to_rate)) = best {
            // Only propose a genuine improvement.
            if to_rate < m.reject_rate {
                proposals.push(RerouteProposal {
                    task_class: class.clone(),
                    from_agent: from_agent.clone(),
                    to_agent: to_agent.clone(),
                    baseline_reject_rate: m.reject_rate,
                    to_reject_rate: to_rate,
                    samples: m.samples,
                    sample_task_ids: m.sample_task_ids.clone(),
                });
            }
        }
    }
    // Worst offender first (highest baseline reject rate).
    proposals.sort_by(|a, b| {
        b.baseline_reject_rate
            .partial_cmp(&a.baseline_reject_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    proposals
}

// ── Durable override file ───────────────────────────────────

/// One approved routing override (the D5 spec record shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutingOverride {
    pub task_class: String,
    pub from_agent: String,
    pub to_agent: String,
    pub approved_at: String,
    /// End of the observation window (RFC3339).
    pub observe_until: String,
    /// `active` | `rolled_back` | `confirmed`.
    pub status: String,
    /// `from_agent`'s reject rate captured at approval — the rollback baseline.
    #[serde(default)]
    pub baseline_reject_rate: f64,
    /// Whether the observation window has already been extended once.
    #[serde(default)]
    pub extended: bool,
}

/// One proposal record (durable — powers the anti-storm cooldown + approval poll).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposalRecord {
    pub id: String,
    pub task_class: String,
    pub from_agent: String,
    pub to_agent: String,
    pub created_at: String,
    /// ApprovalBroker id this proposal is gated on.
    pub approval_id: String,
    /// `pending` | `approved` | `denied`.
    pub status: String,
    #[serde(default)]
    pub baseline_reject_rate: f64,
    #[serde(default)]
    pub samples: u32,
    #[serde(default)]
    pub reject_rate: f64,
}

/// The `routing_overrides.json` document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverridesFile {
    #[serde(default)]
    pub overrides: Vec<RoutingOverride>,
    #[serde(default)]
    pub proposals: Vec<ProposalRecord>,
}

fn overrides_path(home_dir: &Path) -> PathBuf {
    home_dir.join("routing_overrides.json")
}

/// Read the override file (lock-free). Missing / corrupt ⇒ empty document
/// (fail-safe: routing reverts to the current `assigned_to`).
pub fn load_file(home_dir: &Path) -> OverridesFile {
    match std::fs::read(overrides_path(home_dir)) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => OverridesFile::default(),
    }
}

/// Cross-process safe read-modify-write of the override file. The closure sees
/// the current document and mutates it in place; the result is atomically
/// persisted (temp + rename) under an advisory lock.
fn mutate_file<F>(home_dir: &Path, f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut OverridesFile),
{
    let path = overrides_path(home_dir);
    duduclaw_core::with_file_lock(&path, || {
        let mut doc = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => OverridesFile::default(),
        };
        f(&mut doc);
        let bytes = serde_json::to_vec_pretty(&doc)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)
    })
}

/// The active reroute target for `(task_class, from_agent)`, if any. Only an
/// `active` override matches — a rolled_back / confirmed one reverts routing.
pub fn lookup_active_reroute(home_dir: &Path, task_class: &str, from_agent: &str) -> Option<String> {
    load_file(home_dir).overrides.into_iter().find_map(|o| {
        if o.status == STATUS_ACTIVE && o.task_class == task_class && o.from_agent == from_agent {
            Some(o.to_agent)
        } else {
            None
        }
    })
}

/// True when a proposal for `(task_class, from_agent)` was filed within
/// `cooldown_days` (anti-storm). An unparseable `created_at` counts as blocking
/// (conservative: never storm on a corrupt record).
pub fn is_storm_blocked(
    proposals: &[ProposalRecord],
    task_class: &str,
    from_agent: &str,
    now: DateTime<Utc>,
    cooldown_days: i64,
) -> bool {
    proposals.iter().any(|p| {
        if p.task_class != task_class || p.from_agent != from_agent {
            return false;
        }
        match DateTime::parse_from_rfc3339(&p.created_at) {
            Ok(t) => now - t.with_timezone(&Utc) <= chrono::Duration::days(cooldown_days),
            Err(_) => true,
        }
    })
}

/// True when there is already an `active` override OR a `pending` proposal for
/// `(task_class, from_agent)` — so the driver never double-files.
pub fn has_open_change(doc: &OverridesFile, task_class: &str, from_agent: &str) -> bool {
    let active = doc.overrides.iter().any(|o| {
        o.status == STATUS_ACTIVE && o.task_class == task_class && o.from_agent == from_agent
    });
    let pending = doc.proposals.iter().any(|p| {
        p.status == PROPOSAL_PENDING && p.task_class == task_class && p.from_agent == from_agent
    });
    active || pending
}

// ── Observation-window settle decision (pure) ───────────────

/// What to do with an active override at settle time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleDecision {
    /// Still observing — do nothing.
    Keep,
    /// Window elapsed and the new agent beat the baseline — confirm.
    Confirm,
    /// Insufficient samples at window end — extend once.
    Extend,
    /// New agent no better than baseline (or samples never arrived) — revert.
    RollBack,
}

/// Decide the fate of one active override given the target agent's current
/// metrics for the class. `to_metrics = None` ⇒ no samples for the target yet.
///
/// Rules (spec D5.3):
/// - Enough target samples and `reject_rate >= baseline` ⇒ **RollBack**
///   (even mid-window: the new agent is already demonstrably no better).
/// - Enough target samples and better than baseline, still in window ⇒ Keep;
///   window elapsed ⇒ Confirm.
/// - Not enough samples, still in window ⇒ Keep; window elapsed ⇒ Extend once,
///   then RollBack (conservative convergence).
pub fn settle_decision(
    ov: &RoutingOverride,
    to_metrics: Option<&EvidenceMetrics>,
    now: DateTime<Utc>,
    cfg: &TopologyEvolutionConfig,
) -> SettleDecision {
    // Unparseable observe_until ⇒ treat the window as elapsed (resolve, never
    // linger forever on a corrupt record).
    let within = match DateTime::parse_from_rfc3339(&ov.observe_until) {
        Ok(t) => now < t.with_timezone(&Utc),
        Err(_) => false,
    };
    let sufficient = to_metrics.filter(|m| m.samples >= cfg.min_samples);
    match sufficient {
        Some(m) => {
            if m.reject_rate >= ov.baseline_reject_rate {
                SettleDecision::RollBack
            } else if within {
                SettleDecision::Keep
            } else {
                SettleDecision::Confirm
            }
        }
        None => {
            if within {
                SettleDecision::Keep
            } else if !ov.extended {
                SettleDecision::Extend
            } else {
                SettleDecision::RollBack
            }
        }
    }
}

// ── Dispatch policy: hierarchy + active overrides ───────────

/// A [`DispatchPolicy`] that layers active D5 overrides on top of the default
/// `FixedHierarchy` routing. Installed by [`crate::dispatch_policy::build_policy`]
/// only when `[topology_evolution] enabled = true`; otherwise the default path is
/// byte-identical `FixedHierarchy` (returns `None` → dispatch to `assigned_to`).
///
/// A missing / corrupt override file ⇒ falls back to `assigned_to` (fail-safe).
pub struct HierarchyWithOverride {
    home_dir: PathBuf,
}

impl HierarchyWithOverride {
    pub fn new(home_dir: PathBuf) -> Self {
        Self { home_dir }
    }
}

#[async_trait]
impl DispatchPolicy for HierarchyWithOverride {
    fn kind(&self) -> DispatchPolicyKind {
        // It *is* the hierarchy path (override-aware); report as such.
        DispatchPolicyKind::FixedHierarchy
    }
    async fn select(&self, task: &TaskRow, roster: &[String]) -> Option<String> {
        let base = task.assigned_to.trim();
        if base.is_empty() {
            // No opinion — keep the (empty) assignment, exactly like FixedHierarchy.
            return FixedHierarchy.select(task, roster).await;
        }
        let class = task_class(task);
        match lookup_active_reroute(&self.home_dir, &class, base) {
            Some(to) if !to.trim().is_empty() && to != base => Some(to),
            _ => Some(base.to_string()),
        }
    }
}

// ── Driver ──────────────────────────────────────────────────

/// The D5 background driver: settle windows, poll approvals, and (at most one
/// per tick) file a new reroute proposal. Runs only when `[topology_evolution]
/// enabled = true`.
pub struct TopologyEvolutionDriver {
    store: Arc<TaskStore>,
    home_dir: PathBuf,
    broker: Arc<ApprovalBroker>,
    events: Option<Arc<EventBusStore>>,
    config: TopologyEvolutionConfig,
    running: Arc<AtomicBool>,
}

impl TopologyEvolutionDriver {
    pub fn new(
        store: Arc<TaskStore>,
        home_dir: PathBuf,
        broker: Arc<ApprovalBroker>,
        config: TopologyEvolutionConfig,
    ) -> Self {
        // Best-effort events bus (visibility only — never gates control flow).
        let events = EventBusStore::open(&home_dir).ok().map(Arc::new);
        Self {
            store,
            home_dir,
            broker,
            events,
            config,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Stop the loop after the current tick.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the driver loop.
    pub async fn run(self: Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        info!(
            lookback_days = self.config.lookback_days,
            min_samples = self.config.min_samples,
            reject_rate_threshold = self.config.reject_rate_threshold,
            observe_hours = self.config.observe_hours,
            tick_secs = self.config.tick_secs,
            "Topology evolution driver started (human-gated edge optimization)"
        );
        while self.running.load(Ordering::SeqCst) {
            time_sleep(self.config.tick_secs.max(1)).await;
            if let Err(e) = self.tick_once().await {
                warn!(error = %e, "topology evolution tick failed (will retry next tick)");
            }
        }
        warn!("Topology evolution driver stopped");
    }

    /// One driver pass. Public for tests / one-shot recovery.
    pub async fn tick_once(&self) -> Result<(), String> {
        let now = Utc::now();
        let metrics = self.collect_metrics(now).await?;

        // 1) Settle active overrides (rollback / confirm / extend).
        self.settle_overrides(&metrics, now).await?;
        // 2) Poll pending proposals → materialize approved overrides.
        self.poll_proposals(now).await?;
        // 3) File at most one new proposal.
        self.maybe_propose(&metrics, now).await?;
        Ok(())
    }

    /// Aggregate the current evidence map from the task store + activity feed.
    async fn collect_metrics(
        &self,
        now: DateTime<Utc>,
    ) -> Result<BTreeMap<(String, String), EvidenceMetrics>, String> {
        let tasks = self.store.list_tasks(None, None, None).await?;
        // Oscillation counts: goal-loop no-progress events, keyed by task id.
        let (acts, _) = self
            .store
            .list_activity(None, Some("goal_loop.oscillation"), 10_000, 0)
            .await?;
        let mut oscillation_by_task: HashMap<String, u32> = HashMap::new();
        for a in &acts {
            if !within_days(&a.timestamp, now, self.config.lookback_days) {
                continue;
            }
            if let Some(tid) = &a.task_id {
                *oscillation_by_task.entry(tid.clone()).or_insert(0) += 1;
            }
        }
        Ok(aggregate_metrics(
            &tasks,
            &oscillation_by_task,
            now,
            self.config.lookback_days,
        ))
    }

    /// Settle every `active` override against the target agent's fresh metrics.
    async fn settle_overrides(
        &self,
        metrics: &BTreeMap<(String, String), EvidenceMetrics>,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let doc = load_file(&self.home_dir);
        let active: Vec<RoutingOverride> = doc
            .overrides
            .iter()
            .filter(|o| o.status == STATUS_ACTIVE)
            .cloned()
            .collect();
        for ov in active {
            let to_metrics = metrics.get(&(ov.to_agent.clone(), ov.task_class.clone()));
            let decision = settle_decision(&ov, to_metrics, now, &self.config);
            match decision {
                SettleDecision::Keep => {}
                SettleDecision::Confirm => {
                    self.set_override_status(&ov, STATUS_CONFIRMED, false).await;
                    self.record(
                        ACT_CONFIRMED,
                        &ov.from_agent,
                        &format!(
                            "路由改派已確認生效:{} 類任務 {} → {}",
                            ov.task_class, ov.from_agent, ov.to_agent
                        ),
                        json!({
                            "task_class": ov.task_class,
                            "from_agent": ov.from_agent,
                            "to_agent": ov.to_agent,
                            "baseline_reject_rate": ov.baseline_reject_rate,
                        }),
                    )
                    .await;
                }
                SettleDecision::Extend => {
                    // Extend the window once (new observe_until, extended=true).
                    self.extend_override(&ov, now).await;
                    self.record(
                        ACT_EXTENDED,
                        &ov.from_agent,
                        &format!(
                            "觀察期樣本不足,延長一次:{} 類任務 {} → {}",
                            ov.task_class, ov.from_agent, ov.to_agent
                        ),
                        json!({
                            "task_class": ov.task_class,
                            "from_agent": ov.from_agent,
                            "to_agent": ov.to_agent,
                            "extended": true,
                        }),
                    )
                    .await;
                }
                SettleDecision::RollBack => {
                    self.set_override_status(&ov, STATUS_ROLLED_BACK, false).await;
                    self.record(
                        ACT_ROLLED_BACK,
                        &ov.from_agent,
                        &format!(
                            "路由改派自動還原:{} 類任務 {} → {}(未優於原本)",
                            ov.task_class, ov.from_agent, ov.to_agent
                        ),
                        json!({
                            "task_class": ov.task_class,
                            "from_agent": ov.from_agent,
                            "to_agent": ov.to_agent,
                            "baseline_reject_rate": ov.baseline_reject_rate,
                            "to_reject_rate": to_metrics.map(|m| m.reject_rate),
                        }),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    /// Poll every `pending` proposal's approval; on approve materialize an active
    /// override, on deny/expire mark the proposal denied.
    async fn poll_proposals(&self, now: DateTime<Utc>) -> Result<(), String> {
        let doc = load_file(&self.home_dir);
        let pending: Vec<ProposalRecord> = doc
            .proposals
            .iter()
            .filter(|p| p.status == PROPOSAL_PENDING)
            .cloned()
            .collect();
        for p in pending {
            let aid = ApprovalId::from(p.approval_id.clone());
            let status = self.broker.poll(&aid).await.unwrap_or(ApprovalStatus::Denied);
            match status {
                ApprovalStatus::Pending => {}
                ApprovalStatus::Approved => {
                    let observe_until =
                        (now + chrono::Duration::hours(self.config.observe_hours)).to_rfc3339();
                    let ov = RoutingOverride {
                        task_class: p.task_class.clone(),
                        from_agent: p.from_agent.clone(),
                        to_agent: p.to_agent.clone(),
                        approved_at: now.to_rfc3339(),
                        observe_until,
                        status: STATUS_ACTIVE.to_string(),
                        baseline_reject_rate: p.baseline_reject_rate,
                        extended: false,
                    };
                    let pid = p.id.clone();
                    let _ = mutate_file(&self.home_dir, |doc| {
                        for rec in doc.proposals.iter_mut() {
                            if rec.id == pid {
                                rec.status = PROPOSAL_APPROVED.to_string();
                            }
                        }
                        doc.overrides.push(ov);
                    });
                    self.record(
                        ACT_APPROVED,
                        &p.from_agent,
                        &format!(
                            "人工核准路由改派:{} 類任務 {} → {}(觀察 {}h)",
                            p.task_class, p.from_agent, p.to_agent, self.config.observe_hours
                        ),
                        json!({
                            "task_class": p.task_class,
                            "from_agent": p.from_agent,
                            "to_agent": p.to_agent,
                        }),
                    )
                    .await;
                }
                // Denied / Expired (TTL = deny) ⇒ close the proposal.
                _ => {
                    let pid = p.id.clone();
                    let _ = mutate_file(&self.home_dir, |doc| {
                        for rec in doc.proposals.iter_mut() {
                            if rec.id == pid {
                                rec.status = PROPOSAL_DENIED.to_string();
                            }
                        }
                    });
                    self.record(
                        ACT_REJECTED,
                        &p.from_agent,
                        &format!(
                            "路由改派提案未通過（{}）:{} 類任務 {} → {}",
                            status.as_str(),
                            p.task_class,
                            p.from_agent,
                            p.to_agent
                        ),
                        json!({
                            "task_class": p.task_class,
                            "from_agent": p.from_agent,
                            "to_agent": p.to_agent,
                            "decision": status.as_str(),
                        }),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    /// File at most one new reroute proposal (worst offender first), respecting
    /// the anti-storm cooldown and the "no open change" dedup.
    async fn maybe_propose(
        &self,
        metrics: &BTreeMap<(String, String), EvidenceMetrics>,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let parent_of = parent_map(&self.home_dir);
        let candidates = select_reroute(metrics, &parent_of, &self.config);
        if candidates.is_empty() {
            return Ok(());
        }
        let doc = load_file(&self.home_dir);
        for prop in candidates {
            if has_open_change(&doc, &prop.task_class, &prop.from_agent) {
                continue;
            }
            if is_storm_blocked(
                &doc.proposals,
                &prop.task_class,
                &prop.from_agent,
                now,
                self.config.proposal_cooldown_days,
            ) {
                debug!(
                    class = %prop.task_class,
                    from = %prop.from_agent,
                    "topology: proposal suppressed by anti-storm cooldown"
                );
                continue;
            }

            // Human gate — ALWAYS via the broker, never an auto path.
            let summary = format!(
                "建議把「{}」類任務由 {} 改派給 {}(近{}天拒絕率 {:.0}% → {:.0}%,樣本 {})",
                prop.task_class,
                prop.from_agent,
                prop.to_agent,
                self.config.lookback_days,
                prop.baseline_reject_rate * 100.0,
                prop.to_reject_rate * 100.0,
                prop.samples,
            );
            let payload = json!({
                "kind": "reroute",
                "task_class": prop.task_class,
                "from_agent": prop.from_agent,
                "to_agent": prop.to_agent,
                "evidence": {
                    "samples": prop.samples,
                    "reject_rate": prop.baseline_reject_rate,
                    "to_reject_rate": prop.to_reject_rate,
                    "sample_task_ids": prop.sample_task_ids,
                },
            });
            let approval_id = self
                .broker
                .request(
                    &prop.from_agent,
                    ACTION_KIND_REROUTE,
                    &summary,
                    payload.clone(),
                    self.config.approval_ttl_secs,
                )
                .await?;

            let record = ProposalRecord {
                id: uuid::Uuid::new_v4().to_string(),
                task_class: prop.task_class.clone(),
                from_agent: prop.from_agent.clone(),
                to_agent: prop.to_agent.clone(),
                created_at: now.to_rfc3339(),
                approval_id: approval_id.to_string(),
                status: PROPOSAL_PENDING.to_string(),
                baseline_reject_rate: prop.baseline_reject_rate,
                samples: prop.samples,
                reject_rate: prop.baseline_reject_rate,
            };
            let _ = mutate_file(&self.home_dir, |doc| doc.proposals.push(record));
            self.record(
                ACT_PROPOSED,
                &prop.from_agent,
                &summary,
                payload,
            )
            .await;
            info!(
                class = %prop.task_class,
                from = %prop.from_agent,
                to = %prop.to_agent,
                "topology: filed reroute proposal (awaiting human approval)"
            );
            // At most one new proposal per tick.
            break;
        }
        Ok(())
    }

    /// Persist a status change on the matching active override.
    async fn set_override_status(&self, ov: &RoutingOverride, status: &str, extended: bool) {
        let (class, from, to) = (
            ov.task_class.clone(),
            ov.from_agent.clone(),
            ov.to_agent.clone(),
        );
        let status = status.to_string();
        let _ = mutate_file(&self.home_dir, |doc| {
            for rec in doc.overrides.iter_mut() {
                if rec.status == STATUS_ACTIVE
                    && rec.task_class == class
                    && rec.from_agent == from
                    && rec.to_agent == to
                {
                    rec.status = status.clone();
                    if extended {
                        rec.extended = true;
                    }
                }
            }
        });
    }

    /// Extend the observation window once (new observe_until, extended=true).
    async fn extend_override(&self, ov: &RoutingOverride, now: DateTime<Utc>) {
        let new_until = (now + chrono::Duration::hours(self.config.observe_hours)).to_rfc3339();
        let (class, from, to) = (
            ov.task_class.clone(),
            ov.from_agent.clone(),
            ov.to_agent.clone(),
        );
        let _ = mutate_file(&self.home_dir, |doc| {
            for rec in doc.overrides.iter_mut() {
                if rec.status == STATUS_ACTIVE
                    && rec.task_class == class
                    && rec.from_agent == from
                    && rec.to_agent == to
                    && !rec.extended
                {
                    rec.observe_until = new_until.clone();
                    rec.extended = true;
                }
            }
        });
    }

    /// Emit both an events.db row (autopilot bus / dashboard) and an Activity
    /// Feed row. Best-effort — a failure here never breaks the tick.
    async fn record(&self, event_type: &str, agent_id: &str, summary: &str, payload: serde_json::Value) {
        if let Some(ev) = &self.events {
            let _ = ev.append(event_type, &payload.to_string()).await;
        }
        let row = ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: summary.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            metadata: Some(payload.to_string()),
        };
        if let Err(e) = self.store.append_activity(&row).await {
            debug!(error = %e, "topology: activity append failed (non-fatal)");
        }
    }
}

/// Sleep helper (kept small so `run` stays readable / mockable).
async fn time_sleep(secs: u64) {
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

/// Read every agent's `reports_to` parent into a map (`agent_id → Option<parent>`).
/// An empty / missing `reports_to` normalizes to `None` (chain root). Agents
/// whose `agent.toml` is unreadable still appear with `None` so they can be
/// siblings of other roots.
pub fn parent_map(home_dir: &Path) -> HashMap<String, Option<String>> {
    let mut map = HashMap::new();
    let Ok(rd) = std::fs::read_dir(home_dir.join("agents")) else {
        return map;
    };
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let parent = read_reports_to(home_dir, &id);
        map.insert(id, parent);
    }
    map
}

/// Read one agent's `reports_to` (empty ⇒ `None`). Local copy of the private
/// helper in `config_crypto` to keep this module self-contained.
fn read_reports_to(home_dir: &Path, agent_id: &str) -> Option<String> {
    let path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    let table: toml::Value = content.parse().ok()?;
    table
        .get("agent")
        .and_then(|a| a.as_table())
        .and_then(|t| t.get("reports_to"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Test-only helper (used by the `handlers.rs` RPC test): seed one active
/// override + one pending proposal for `(task_class, from_agent → to_agent)`.
#[cfg(test)]
pub fn seed_for_test(home_dir: &Path, task_class: &str, from_agent: &str, to_agent: &str) {
    let now = Utc::now();
    let ov = RoutingOverride {
        task_class: task_class.to_string(),
        from_agent: from_agent.to_string(),
        to_agent: to_agent.to_string(),
        approved_at: now.to_rfc3339(),
        observe_until: (now + chrono::Duration::hours(24)).to_rfc3339(),
        status: STATUS_ACTIVE.to_string(),
        baseline_reject_rate: 0.8,
        extended: false,
    };
    let prop = ProposalRecord {
        id: "seed-proposal".to_string(),
        task_class: task_class.to_string(),
        from_agent: from_agent.to_string(),
        to_agent: to_agent.to_string(),
        created_at: now.to_rfc3339(),
        approval_id: "seed-approval".to_string(),
        status: PROPOSAL_PENDING.to_string(),
        baseline_reject_rate: 0.8,
        samples: 10,
        reject_rate: 0.8,
    };
    mutate_file(home_dir, |doc| {
        doc.overrides.push(ov);
        doc.proposals.push(prop);
    })
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TopologyEvolutionConfig {
        TopologyEvolutionConfig {
            enabled: true,
            lookback_days: 14,
            min_samples: 5,
            reject_rate_threshold: 0.6,
            observe_hours: 24,
            proposal_cooldown_days: 7,
            tick_secs: 3600,
            approval_ttl_secs: 86_400,
        }
    }

    /// A decided goal-mode task assigned to `agent`, updated `days_ago`.
    fn sample(
        id: &str,
        agent: &str,
        class_tag: &str,
        status: &str,
        retry: i64,
        days_ago: i64,
    ) -> TaskRow {
        let mut t = TaskRow::new(
            id.into(),
            format!("goal {id}"),
            "work".into(),
            "medium".into(),
            agent.into(),
            "system".into(),
        );
        t.goal_mode = true;
        t.status = status.into();
        t.retry_count = retry;
        t.tags = class_tag.into();
        t.updated_at = (Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339();
        t
    }

    // ── Evidence aggregation ────────────────────────────────

    #[test]
    fn aggregate_counts_rejects_and_rate() {
        let now = Utc::now();
        let tasks = vec![
            // agent "alice", class "billing": 3 rejected of 5 → 0.6.
            sample("t1", "alice", "billing", "needs_human", 0, 1),
            sample("t2", "alice", "billing", "failed", 0, 2),
            sample("t3", "alice", "billing", "done", 1, 3), // retry>0 ⇒ rejected
            sample("t4", "alice", "billing", "done", 0, 4), // clean
            sample("t5", "alice", "billing", "done", 0, 5), // clean
            // Not goal_mode ⇒ ignored.
            {
                let mut t = sample("t6", "alice", "billing", "done", 0, 1);
                t.goal_mode = false;
                t
            },
            // Out of window ⇒ ignored.
            sample("t7", "alice", "billing", "failed", 0, 90),
            // In-flight (review) ⇒ not a decided sample.
            sample("t8", "alice", "billing", "review", 0, 1),
        ];
        let osc = HashMap::new();
        let m = aggregate_metrics(&tasks, &osc, now, 14);
        let cell = m.get(&("alice".into(), "billing".into())).unwrap();
        assert_eq!(cell.samples, 5);
        assert_eq!(cell.rejects, 3);
        assert!((cell.reject_rate - 0.6).abs() < 1e-9);
        assert_eq!(cell.needs_human, 1);
        assert_eq!(cell.sample_task_ids.len(), 5);
    }

    #[test]
    fn aggregate_uses_priority_when_no_tag_and_counts_oscillation() {
        let now = Utc::now();
        // No tag ⇒ class == priority ("high").
        let mut t = sample("x1", "bob", "", "done", 1, 1);
        t.priority = "high".into();
        t.tags = String::new();
        let mut osc = HashMap::new();
        osc.insert("x1".to_string(), 2u32);
        let m = aggregate_metrics(&[t], &osc, now, 14);
        let cell = m.get(&("bob".into(), "high".into())).unwrap();
        assert_eq!(cell.samples, 1);
        assert_eq!(cell.oscillation, 2);
    }

    // ── Proposal selection ──────────────────────────────────

    fn metrics_cell(samples: u32, rejects: u32) -> EvidenceMetrics {
        EvidenceMetrics {
            samples,
            rejects,
            reject_rate: rejects as f64 / samples as f64,
            needs_human: 0,
            oscillation: 0,
            sample_task_ids: vec!["s1".into()],
        }
    }

    #[test]
    fn select_reroute_picks_best_sibling() {
        let mut metrics = BTreeMap::new();
        // alice: bad (0.8), bob: good (0.2), carol: middling (0.4) — same parent.
        metrics.insert(("alice".to_string(), "billing".to_string()), metrics_cell(10, 8));
        metrics.insert(("bob".to_string(), "billing".to_string()), metrics_cell(10, 2));
        metrics.insert(("carol".to_string(), "billing".to_string()), metrics_cell(10, 4));
        let mut parent = HashMap::new();
        parent.insert("alice".to_string(), Some("boss".to_string()));
        parent.insert("bob".to_string(), Some("boss".to_string()));
        parent.insert("carol".to_string(), Some("boss".to_string()));

        let props = select_reroute(&metrics, &parent, &cfg());
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].from_agent, "alice");
        assert_eq!(props[0].to_agent, "bob", "lowest-reject sibling wins");
        assert!((props[0].baseline_reject_rate - 0.8).abs() < 1e-9);
    }

    #[test]
    fn select_reroute_no_qualified_sibling_yields_nothing() {
        // alice is bad, but her only sibling has too few samples.
        let mut metrics = BTreeMap::new();
        metrics.insert(("alice".to_string(), "billing".to_string()), metrics_cell(10, 8));
        metrics.insert(("bob".to_string(), "billing".to_string()), metrics_cell(2, 0)); // < min_samples
        let mut parent = HashMap::new();
        parent.insert("alice".to_string(), Some("boss".to_string()));
        parent.insert("bob".to_string(), Some("boss".to_string()));
        assert!(select_reroute(&metrics, &parent, &cfg()).is_empty());

        // And a sibling in a DIFFERENT parent does not qualify either.
        let mut metrics2 = BTreeMap::new();
        metrics2.insert(("alice".to_string(), "billing".to_string()), metrics_cell(10, 8));
        metrics2.insert(("dan".to_string(), "billing".to_string()), metrics_cell(10, 1));
        let mut parent2 = HashMap::new();
        parent2.insert("alice".to_string(), Some("boss".to_string()));
        parent2.insert("dan".to_string(), Some("other".to_string()));
        assert!(select_reroute(&metrics2, &parent2, &cfg()).is_empty());
    }

    #[test]
    fn select_reroute_respects_min_samples_and_threshold() {
        // Below threshold ⇒ no proposal even with a great sibling.
        let mut metrics = BTreeMap::new();
        metrics.insert(("alice".to_string(), "billing".to_string()), metrics_cell(10, 5)); // 0.5 < 0.6
        metrics.insert(("bob".to_string(), "billing".to_string()), metrics_cell(10, 0));
        let mut parent = HashMap::new();
        parent.insert("alice".to_string(), Some("boss".to_string()));
        parent.insert("bob".to_string(), Some("boss".to_string()));
        assert!(select_reroute(&metrics, &parent, &cfg()).is_empty());

        // Too few offender samples ⇒ no proposal.
        let mut metrics2 = BTreeMap::new();
        metrics2.insert(("alice".to_string(), "billing".to_string()), metrics_cell(3, 3)); // < min
        metrics2.insert(("bob".to_string(), "billing".to_string()), metrics_cell(10, 0));
        let parent2 = parent.clone();
        assert!(select_reroute(&metrics2, &parent2, &cfg()).is_empty());
    }

    // ── Override file / dispatch lookup ─────────────────────

    #[test]
    fn lookup_active_reroute_hits_and_reverts() {
        let dir = tempfile::tempdir().unwrap();
        let ov = RoutingOverride {
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            approved_at: Utc::now().to_rfc3339(),
            observe_until: (Utc::now() + chrono::Duration::hours(24)).to_rfc3339(),
            status: STATUS_ACTIVE.into(),
            baseline_reject_rate: 0.8,
            extended: false,
        };
        mutate_file(dir.path(), |d| d.overrides.push(ov)).unwrap();
        assert_eq!(
            lookup_active_reroute(dir.path(), "billing", "alice").as_deref(),
            Some("bob")
        );
        // Wrong class / wrong from_agent ⇒ no hit.
        assert_eq!(lookup_active_reroute(dir.path(), "sales", "alice"), None);
        assert_eq!(lookup_active_reroute(dir.path(), "billing", "carol"), None);

        // Rolled-back override reverts (no longer active).
        mutate_file(dir.path(), |d| {
            d.overrides[0].status = STATUS_ROLLED_BACK.into();
        })
        .unwrap();
        assert_eq!(lookup_active_reroute(dir.path(), "billing", "alice"), None);
    }

    #[test]
    fn corrupt_override_file_is_fail_safe() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("routing_overrides.json"), b"{not json").unwrap();
        // Corrupt file ⇒ no override (routing reverts to assigned_to).
        assert_eq!(lookup_active_reroute(dir.path(), "billing", "alice"), None);
        assert!(load_file(dir.path()).overrides.is_empty());
    }

    #[tokio::test]
    async fn hierarchy_with_override_reroutes_on_hit() {
        let dir = tempfile::tempdir().unwrap();
        let ov = RoutingOverride {
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            approved_at: Utc::now().to_rfc3339(),
            observe_until: (Utc::now() + chrono::Duration::hours(24)).to_rfc3339(),
            status: STATUS_ACTIVE.into(),
            baseline_reject_rate: 0.8,
            extended: false,
        };
        mutate_file(dir.path(), |d| d.overrides.push(ov)).unwrap();
        let policy = HierarchyWithOverride::new(dir.path().to_path_buf());

        // Task assigned to alice, class billing ⇒ rerouted to bob.
        let mut hit = TaskRow::new(
            "t1".into(),
            "goal".into(),
            "d".into(),
            "medium".into(),
            "alice".into(),
            "system".into(),
        );
        hit.tags = "billing".into();
        assert_eq!(policy.select(&hit, &[]).await.as_deref(), Some("bob"));

        // Different class ⇒ unchanged (still alice).
        let mut miss = hit.clone();
        miss.tags = "sales".into();
        assert_eq!(policy.select(&miss, &[]).await.as_deref(), Some("alice"));
    }

    // ── Anti-storm ──────────────────────────────────────────

    #[test]
    fn storm_guard_blocks_within_cooldown() {
        let now = Utc::now();
        let recent = ProposalRecord {
            id: "p1".into(),
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            created_at: (now - chrono::Duration::days(2)).to_rfc3339(),
            approval_id: "a1".into(),
            status: PROPOSAL_DENIED.into(), // even a denied one counts
            baseline_reject_rate: 0.8,
            samples: 10,
            reject_rate: 0.8,
        };
        let props = vec![recent];
        // Within 7-day cooldown ⇒ blocked (even though it was denied).
        assert!(is_storm_blocked(&props, "billing", "alice", now, 7));
        // Different edge ⇒ not blocked.
        assert!(!is_storm_blocked(&props, "billing", "carol", now, 7));
        // Old proposal (10 days) ⇒ not blocked.
        let mut old = props.clone();
        old[0].created_at = (now - chrono::Duration::days(10)).to_rfc3339();
        assert!(!is_storm_blocked(&old, "billing", "alice", now, 7));
    }

    #[test]
    fn has_open_change_detects_active_and_pending() {
        let mut doc = OverridesFile::default();
        assert!(!has_open_change(&doc, "billing", "alice"));
        doc.proposals.push(ProposalRecord {
            id: "p1".into(),
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            created_at: Utc::now().to_rfc3339(),
            approval_id: "a1".into(),
            status: PROPOSAL_PENDING.into(),
            baseline_reject_rate: 0.8,
            samples: 10,
            reject_rate: 0.8,
        });
        assert!(has_open_change(&doc, "billing", "alice"));
        // A denied proposal does NOT count as open.
        doc.proposals[0].status = PROPOSAL_DENIED.into();
        assert!(!has_open_change(&doc, "billing", "alice"));
    }

    // ── Settle / rollback decision ──────────────────────────

    fn active_override(baseline: f64, hours_left: i64, extended: bool) -> RoutingOverride {
        RoutingOverride {
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            approved_at: Utc::now().to_rfc3339(),
            observe_until: (Utc::now() + chrono::Duration::hours(hours_left)).to_rfc3339(),
            status: STATUS_ACTIVE.into(),
            baseline_reject_rate: baseline,
            extended,
        }
    }

    #[test]
    fn settle_rolls_back_when_target_not_better() {
        let now = Utc::now();
        let ov = active_override(0.6, 12, false); // still in window
        // Target has enough samples but reject_rate >= baseline ⇒ rollback (even
        // mid-window).
        let to = metrics_cell(10, 7); // 0.7 >= 0.6
        assert_eq!(
            settle_decision(&ov, Some(&to), now, &cfg()),
            SettleDecision::RollBack
        );
    }

    #[test]
    fn settle_keeps_then_confirms_when_target_better() {
        let now = Utc::now();
        let to = metrics_cell(10, 2); // 0.2 < 0.6 baseline
        // In window + better ⇒ Keep.
        let in_window = active_override(0.6, 12, false);
        assert_eq!(
            settle_decision(&in_window, Some(&to), now, &cfg()),
            SettleDecision::Keep
        );
        // Window elapsed + better ⇒ Confirm.
        let elapsed = active_override(0.6, -1, false);
        assert_eq!(
            settle_decision(&elapsed, Some(&to), now, &cfg()),
            SettleDecision::Confirm
        );
    }

    #[test]
    fn settle_extends_once_then_rolls_back_on_insufficient_samples() {
        let now = Utc::now();
        // Window elapsed, no target samples, not yet extended ⇒ Extend.
        let first = active_override(0.6, -1, false);
        assert_eq!(settle_decision(&first, None, now, &cfg()), SettleDecision::Extend);
        // Too-few samples also count as insufficient.
        let few = metrics_cell(2, 0);
        assert_eq!(
            settle_decision(&first, Some(&few), now, &cfg()),
            SettleDecision::Extend
        );
        // Already extended + still insufficient ⇒ RollBack (conservative).
        let second = active_override(0.6, -1, true);
        assert_eq!(settle_decision(&second, None, now, &cfg()), SettleDecision::RollBack);
        // Still in window with insufficient samples ⇒ Keep.
        let waiting = active_override(0.6, 12, false);
        assert_eq!(settle_decision(&waiting, None, now, &cfg()), SettleDecision::Keep);
    }

    // ── Driver end-to-end (propose → human approve → active override) ────────

    fn write_agent(home: &Path, id: &str, reports_to: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{id}\"\nreports_to = \"{reports_to}\"\n"),
        )
        .unwrap();
    }

    async fn insert_sample(store: &TaskStore, id: &str, agent: &str, status: &str) {
        // A fresh decided goal-mode row (billing class, updated 1 day ago).
        store
            .insert_task(&sample(id, agent, "billing", status, 0, 1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn driver_proposes_then_materializes_override_on_human_approval() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_agent(home, "alice", "boss");
        write_agent(home, "bob", "boss");

        let store = Arc::new(TaskStore::open(home).unwrap());
        // alice: 5 decided goal tasks all rejected (reject_rate 1.0).
        for i in 0..5 {
            insert_sample(&store, &format!("a{i}"), "alice", "needs_human").await;
        }
        // bob: 5 decided goal tasks all clean (reject_rate 0.0).
        for i in 0..5 {
            insert_sample(&store, &format!("b{i}"), "bob", "done").await;
        }

        let broker = Arc::new(ApprovalBroker::open(home).unwrap());
        let driver = TopologyEvolutionDriver::new(
            store.clone(),
            home.to_path_buf(),
            broker.clone(),
            cfg(),
        );

        // Tick 1: files exactly one reroute proposal through the human gate.
        driver.tick_once().await.unwrap();
        let pending = broker.list_pending(None).await.unwrap();
        assert_eq!(pending.len(), 1, "one reroute approval filed");
        assert_eq!(pending[0].action_kind, ACTION_KIND_REROUTE);
        let doc = load_file(home);
        assert_eq!(doc.proposals.len(), 1);
        assert_eq!(doc.proposals[0].status, PROPOSAL_PENDING);
        assert!(doc.overrides.is_empty(), "no override before approval");

        // A second tick must NOT file a duplicate (open-change dedup).
        driver.tick_once().await.unwrap();
        assert_eq!(broker.list_pending(None).await.unwrap().len(), 1);

        // Human approves through the broker (dashboard/channel path).
        let approval_id =
            ApprovalId::from(load_file(home).proposals[0].approval_id.clone());
        broker.decide(&approval_id, true, "dashboard:tester").await.unwrap();

        // Tick 3: poll picks up the approval → materializes an active override.
        driver.tick_once().await.unwrap();
        let doc = load_file(home);
        assert_eq!(doc.overrides.len(), 1);
        assert_eq!(doc.overrides[0].status, STATUS_ACTIVE);
        assert_eq!(doc.proposals[0].status, PROPOSAL_APPROVED);
        // The dispatch lookup now reroutes alice's billing tasks to bob.
        assert_eq!(
            lookup_active_reroute(home, "billing", "alice").as_deref(),
            Some("bob")
        );
    }

    #[tokio::test]
    async fn driver_rolls_back_when_target_regresses() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_agent(home, "alice", "boss");
        write_agent(home, "bob", "boss");
        let store = Arc::new(TaskStore::open(home).unwrap());
        let broker = Arc::new(ApprovalBroker::open(home).unwrap());

        // Pre-seed an ALREADY-ELAPSED active override with baseline 0.6.
        let ov = RoutingOverride {
            task_class: "billing".into(),
            from_agent: "alice".into(),
            to_agent: "bob".into(),
            approved_at: (Utc::now() - chrono::Duration::hours(48)).to_rfc3339(),
            observe_until: (Utc::now() - chrono::Duration::hours(1)).to_rfc3339(),
            status: STATUS_ACTIVE.into(),
            baseline_reject_rate: 0.6,
            extended: false,
        };
        mutate_file(home, |d| d.overrides.push(ov)).unwrap();

        // bob now performs WORSE than baseline for billing (5/5 rejected = 1.0).
        for i in 0..5 {
            insert_sample(&store, &format!("b{i}"), "bob", "failed").await;
        }

        let driver =
            TopologyEvolutionDriver::new(store.clone(), home.to_path_buf(), broker, cfg());
        driver.tick_once().await.unwrap();

        let doc = load_file(home);
        assert_eq!(doc.overrides[0].status, STATUS_ROLLED_BACK, "regressed target rolls back");
        // Routing reverts — no active override remains.
        assert_eq!(lookup_active_reroute(home, "billing", "alice"), None);
    }

    #[test]
    fn config_from_home_defaults_off() {
        let dir = tempfile::tempdir().unwrap();
        // Absent config ⇒ disabled by default.
        assert!(!enabled(dir.path()));
        let c = TopologyEvolutionConfig::from_home(dir.path());
        assert!(!c.enabled);
        assert_eq!(c.lookback_days, 14);
        assert_eq!(c.min_samples, 5);

        // Partial section ⇒ only given fields override.
        std::fs::write(
            dir.path().join("config.toml"),
            "[topology_evolution]\nenabled = true\nmin_samples = 8\n",
        )
        .unwrap();
        assert!(enabled(dir.path()));
        let c2 = TopologyEvolutionConfig::from_home(dir.path());
        assert_eq!(c2.min_samples, 8);
        assert_eq!(c2.reject_rate_threshold, 0.6, "unspecified keeps default");
    }
}
