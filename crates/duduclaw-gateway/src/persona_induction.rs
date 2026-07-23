//! P4-2: persona suppression rule induction — "when not to interrupt".
//!
//! ContextAgent (arXiv:2505.14668) shows persona context is the single
//! biggest lever against false alarms (removing it costs 12.3 F1 points).
//! DuDuClaw already has one half of that loop wired: [`crate::proactive_gate`]
//! *reads* persona lines when scoring, and [`crate::proactive_feedback`]
//! backfills every decision's outcome (`false_alarm` when the user dismissed
//! an Allow). What's missing is the other half — turning a pile of
//! `false_alarm` outcomes into a persona fact that says "don't bother me
//! about this again". This module closes that loop, deterministically
//! (zero LLM, PRIME arXiv:2604.07645 training-free spirit): aggregate
//! `false_alarm` outcomes by situation, apply the same GovMem-style
//! (arXiv:2607.02579) independent-evidence bar the reflexion pipeline uses,
//! and write a suppression fact once the bar is cleared.
//!
//! ## Where this sits (deliberately does not touch [`crate::proactive_gate`])
//!
//! The gate already fetches persona lines via
//! [`crate::autopilot_engine`]'s `fetch_persona_lines`, which calls
//! [`duduclaw_memory::SqliteMemoryEngine::search_facts`] — an FTS5 search
//! over the **`key_facts`** table, not the temporal-memory `memories` table
//! `store_temporal` writes to. Those are two different stores. So a rule
//! induced here is written to *both*:
//! 1. **temporal memory** (`store_temporal`) — the governance record: origin
//!    binding (WP1 arXiv:2606.24322), GovMem promotion bar, and the
//!    supersession chain Janus probation revokes ride on. This is the
//!    source of truth this module's own state and tests reason about.
//! 2. **`key_facts`** (`store_fact`) — a best-effort bridge so the rule's
//!    text is actually reachable by the *existing, unmodified*
//!    `fetch_persona_lines` → `search_facts` path the gate already calls
//!    every proactive evaluation. Without this bridge the induced rule would
//!    be correctly governed but invisible to the scorer.
//!
//! **Known limitation**: `key_facts` has no delete-by-id API (only an
//! age+access-count purge). When a rule is revoked (see below) its temporal
//! memory row is properly superseded, but the bridged `key_facts` row is
//! left in place — it ages out via the existing `purge_stale_facts` janitor
//! like any other key fact, never via this module. A revoked rule's
//! suppression *text* may therefore keep surfacing in `search_facts` for a
//! while after governance considers it superseded. Acceptable because (a)
//! the LLM scorer treats persona lines as soft context, never a hard
//! override, and (b) closing this gap requires a `key_facts` delete-by-id
//! API this work package's scope does not extend to `duduclaw-memory` for.
//!
//! ## GovMem-style promotion bar (mirrors [`crate::reflexion::assess_promotion`])
//!
//! A situation group is only worth suppressing once the false-alarm evidence
//! is *independent*, not one noisy incident: `>= `[`FA_MIN_COUNT`]` false
//! alarms spanning `>= `[`FA_MIN_DISTINCT_DAYS`]` distinct UTC calendar days.
//! Reflexion's bar additionally requires distinct sessions/wording; proactive
//! notifications don't carry that shape (one `proactive_gate.jsonl` line per
//! decision, no session/wording field), so the day-spread requirement is this
//! domain's equivalent decorrelation signal — a single bad afternoon cannot
//! silence a whole situation.
//!
//! ## Situation grouping (three dimensions)
//!
//! - **Time bucket** — Asia/Taipei local hour, `工作時間` (07:00–21:59) vs.
//!   `深夜` (22:00–06:59). Fixed to the product's Taiwan locale (see
//!   `CLAUDE.md` design context) rather than a per-agent config knob — no
//!   existing single-user timezone setting exists to read from, and adding
//!   one is out of this work package's scope.
//! - **Event type** — the raw autopilot `event` field (`os_file`,
//!   `os_frontmost`, `agent_idle`, …), verbatim, for grouping precision. The
//!   induced rule's *text* uses [`crate::proactive_feedback::event_keywords`]
//!   for readability and — not incidentally — better FTS overlap with the
//!   perceived-event text a future decision will search against.
//! - **Interruptibility bucket** — the `interruptibility` score already
//!   recorded on each gate decision line, tertiled into low/mid/high.
//!
//! ## Janus probation (WP2, arXiv:2606.31121), suppression-domain variant
//!
//! Every induced rule is tagged [`PROBATION_RULE_TAG`] on write (reusing
//! [`crate::prediction::rule_lifecycle`]'s constant for consistency). Unlike
//! the reflexion domain there is no per-turn helpful/harmful settlement to
//! "graduate" a rule against — a suppression rule's only judge is whether the
//! *same situation* later produces a `correct_detection` (proof the gate
//! *should* have interrupted). One such observation after the rule's
//! `induced_at` supersedes it immediately (one-strike, matching Janus's
//! probation severity) via the existing temporal-memory supersession chain —
//! no separate "revoke" primitive needed.
//!
//! ## Cost: daily tick, not the 60s loop
//!
//! [`spawn_induction_loop`] wakes hourly but the actual `O(file)` aggregation
//! only runs once per UTC calendar day per agent
//! ([`maybe_run_induction_tick`] short-circuits on a same-day marker in the
//! persisted state) — a suppression rule is not latency-sensitive the way
//! [`crate::proactive_feedback`]'s outcome backfill is, so there is no reason
//! to pay `proactive_gate.jsonl`-sized I/O every 60s. This is the "whichever
//! is cheaper" choice the work package left open; a fresh independent loop
//! avoids touching `proactive_feedback.rs`'s already-tested 60s loop body.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::types::{MemoryEntry, MemoryLayer};
use duduclaw_memory::{SqliteMemoryEngine, TemporalMeta};

use crate::prediction::rule_lifecycle::PROBATION_RULE_TAG;

// ── Tunables ─────────────────────────────────────────────────────────────

/// GovMem-style minimum false-alarm count before a situation is eligible.
pub const FA_MIN_COUNT: u32 = 3;
/// Minimum distinct UTC calendar days the false alarms must span — the
/// decorrelation signal (see module doc).
pub const FA_MIN_DISTINCT_DAYS: u32 = 2;
/// Max concurrently-active induced rules per agent. Beyond this, inducing a
/// new rule evicts the oldest active one first.
pub const MAX_ACTIVE_RULES_PER_AGENT: usize = 10;
/// Background loop wake interval. The actual per-agent work is day-gated
/// internally (see module doc "Cost: daily tick") — this only controls how
/// promptly a day rollover is noticed.
pub const INDUCTION_TICK_INTERVAL: Duration = Duration::from_secs(3600);

/// Temporal-memory origin class for induced rules (WP1 v1.41). Deterministic
/// system inference from operational history is exactly what
/// `agent_derived` (ceiling 0.6) already models — no new origin class added.
const ORIGIN_CLASS: &str = "agent_derived";
/// `source_event` stamped on every write this module makes.
const INDUCTION_SOURCE_EVENT: &str = "persona_suppression_induction";
/// `key_facts` bridge channel/chat_id — a stable, non-conversational
/// provenance pair distinguishing induced rows from real channel facts.
const KEY_FACT_CHANNEL: &str = "system";
const KEY_FACT_CHAT_ID: &str = "persona_induction";
/// Persisted induction state filename under `<home>/`.
const STATE_FILE_NAME: &str = "persona_induction_state.json";

/// Revoke reason: a later `correct_detection` in the same situation.
pub const REASON_CORRECT_DETECTION: &str = "correct_detection";
/// Revoke reason: cap eviction made room for a newly-eligible situation.
pub const REASON_CAP_EVICTION: &str = "cap_eviction";

// ── Situation grouping (pure) ───────────────────────────────────────────

/// Coarse time-of-day bucket, Asia/Taipei local time (see module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TimeBucket {
    WorkHours,
    Night,
}

impl TimeBucket {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::WorkHours => "work_hours",
            Self::Night => "night",
        }
    }

    /// Chinese label used verbatim in the induced rule's template text.
    pub fn label_zh(self) -> &'static str {
        match self {
            Self::WorkHours => "工作時間",
            Self::Night => "深夜",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "work_hours" => Some(Self::WorkHours),
            "night" => Some(Self::Night),
            _ => None,
        }
    }
}

/// Classify a UTC timestamp into a Taiwan-local time bucket. Night = 22:00
/// through 06:59 local; everything else is work hours. Pure.
pub fn time_bucket_of(ts: DateTime<Utc>) -> TimeBucket {
    let local_hour = ts.with_timezone(&chrono_tz::Asia::Taipei).hour();
    if local_hour >= 22 || local_hour < 7 {
        TimeBucket::Night
    } else {
        TimeBucket::WorkHours
    }
}

/// Interruptibility tertile at decision time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InterruptBucket {
    Low,
    Mid,
    High,
}

impl InterruptBucket {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        }
    }

    fn from_key(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "mid" => Some(Self::Mid),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// Tertile an interruptibility score (0.0–1.0, values outside are clamped by
/// construction upstream but this stays defensive). Pure.
pub fn interrupt_bucket_of(v: f32) -> InterruptBucket {
    if v < 0.34 {
        InterruptBucket::Low
    } else if v < 0.67 {
        InterruptBucket::Mid
    } else {
        InterruptBucket::High
    }
}

/// A situation — the grouping key false-alarm/correct-detection outcomes are
/// aggregated by, and the identity an induced rule is keyed on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupKey {
    pub time_bucket: TimeBucket,
    /// Raw autopilot `event` field, verbatim (grouping precision).
    pub event: String,
    pub interrupt_bucket: InterruptBucket,
}

impl GroupKey {
    /// Stable per-agent identity string. Doubles as the temporal-memory
    /// `subject` suffix so distinct situations never collide on the
    /// `(agent, subject, predicate)` supersession key `store_temporal` uses
    /// (see [`induce_rule`] doc) — without this, two unrelated situations
    /// sharing `subject="user"` would silently supersede each other.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}:{}:{}",
            self.time_bucket.as_key(),
            self.event,
            self.interrupt_bucket.as_key()
        )
    }
}

/// Human-readable event label for template text — reuses
/// [`crate::proactive_feedback::event_keywords`] so the induced rule's
/// wording overlaps the same tokens a future perceived event's raw text is
/// likely to contain (better FTS recall from the `key_facts` bridge).
fn event_display(event: &str) -> String {
    let kw = crate::proactive_feedback::event_keywords(event);
    if kw.is_empty() {
        event.to_string()
    } else {
        kw.join(" ")
    }
}

fn suppression_content(key: &GroupKey) -> String {
    format!(
        "{}的 {} 類主動通知曾被多次忽略/打槍，預設沉默",
        key.time_bucket.label_zh(),
        event_display(&key.event)
    )
}

fn revoke_content(key: &GroupKey, reason: &str) -> String {
    let reason_zh = if reason == REASON_CORRECT_DETECTION {
        "後續同情境出現正確偵測，證明其實該打擾"
    } else {
        "規則數已達上限，淘汰最舊規則騰出空間"
    };
    format!(
        "{}的 {} 類主動通知抑制規則已撤銷：{}",
        key.time_bucket.label_zh(),
        event_display(&key.event),
        reason_zh
    )
}

// ── proactive_gate.jsonl parsing ────────────────────────────────────────

/// The subset of one `proactive_gate.jsonl` line this module needs, already
/// filtered to one agent.
#[derive(Debug, Clone)]
struct GateRecord {
    ts: DateTime<Utc>,
    event: String,
    decision: String,
    outcome: Option<String>,
    interruptibility: f32,
}

/// Read + parse every line of `<home>/proactive_gate.jsonl` belonging to
/// `agent_id`. Missing file → empty (nothing to induce from yet, not an
/// error). Malformed lines are skipped, not fatal (mirrors
/// [`crate::proactive_feedback::run_backfill_once`]'s tolerance).
fn read_gate_records(path: &Path, agent_id: &str) -> io::Result<Vec<GateRecord>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("agent").and_then(|a| a.as_str()) != Some(agent_id) {
            continue;
        }
        let (Some(ts), Some(event), Some(decision)) = (
            v.get("ts")
                .and_then(|t| t.as_str())
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|d| d.with_timezone(&Utc)),
            v.get("event").and_then(|e| e.as_str()).map(String::from),
            v.get("decision").and_then(|d| d.as_str()).map(String::from),
        ) else {
            continue;
        };
        let outcome = v.get("outcome").and_then(|o| o.as_str()).map(String::from);
        let interruptibility = v
            .get("interruptibility")
            .and_then(|i| i.as_f64())
            .unwrap_or(0.0) as f32;
        out.push(GateRecord {
            ts,
            event,
            decision,
            outcome,
            interruptibility,
        });
    }
    Ok(out)
}

// ── Aggregation (pure) ───────────────────────────────────────────────────

/// Per-group false-alarm evidence.
#[derive(Debug, Clone, Default)]
pub struct FalseAlarmGroupStats {
    pub count: u32,
    pub distinct_days: HashSet<NaiveDate>,
}

/// Group every `allow` decision whose backfilled `outcome == "false_alarm"`
/// by [`GroupKey`]. Deterministic (`BTreeMap`) iteration order downstream.
fn aggregate_false_alarms(records: &[GateRecord]) -> BTreeMap<GroupKey, FalseAlarmGroupStats> {
    let mut map: BTreeMap<GroupKey, FalseAlarmGroupStats> = BTreeMap::new();
    for r in records {
        if r.decision != "allow" || r.outcome.as_deref() != Some("false_alarm") {
            continue;
        }
        let key = GroupKey {
            time_bucket: time_bucket_of(r.ts),
            event: r.event.clone(),
            interrupt_bucket: interrupt_bucket_of(r.interruptibility),
        };
        let stats = map.entry(key).or_default();
        stats.count += 1;
        stats.distinct_days.insert(r.ts.date_naive());
    }
    map
}

/// Latest `correct_detection` timestamp per group — the revoke signal
/// (module doc "Janus probation"). Only the max matters: a single
/// correct_detection strictly after a rule's `induced_at` is enough to
/// revoke it.
fn aggregate_correct_detections(records: &[GateRecord]) -> BTreeMap<GroupKey, DateTime<Utc>> {
    let mut map: BTreeMap<GroupKey, DateTime<Utc>> = BTreeMap::new();
    for r in records {
        if r.decision != "allow" || r.outcome.as_deref() != Some("correct_detection") {
            continue;
        }
        let key = GroupKey {
            time_bucket: time_bucket_of(r.ts),
            event: r.event.clone(),
            interrupt_bucket: interrupt_bucket_of(r.interruptibility),
        };
        let entry = map.entry(key).or_insert(r.ts);
        if r.ts > *entry {
            *entry = r.ts;
        }
    }
    map
}

/// GovMem-style promotion bar (see module doc). Pure.
pub fn govmem_eligible(stats: &FalseAlarmGroupStats) -> bool {
    stats.count >= FA_MIN_COUNT && stats.distinct_days.len() as u32 >= FA_MIN_DISTINCT_DAYS
}

// ── Persisted per-agent state ───────────────────────────────────────────

/// One induced (or since-revoked) rule, tracked for dedup / cap eviction /
/// revoke lookup. Mirrors just enough of [`GroupKey`] to reconstruct it
/// ([`InducedRuleState::group_key`]) without re-deriving from the memory row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InducedRuleState {
    pub fingerprint: String,
    pub time_bucket: String,
    pub event: String,
    pub interrupt_bucket: String,
    /// Temporal-memory id of the currently-active suppression row (the
    /// *first* induction's id — superseding writes get their own ids but
    /// this module never needs to chase the chain, only fingerprint-match).
    pub memory_id: String,
    /// `key_facts` bridge row id, if the bridge write succeeded.
    pub key_fact_id: Option<String>,
    pub induced_at: DateTime<Utc>,
    pub revoked: bool,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoke_reason: Option<String>,
}

impl InducedRuleState {
    fn group_key(&self) -> Option<GroupKey> {
        Some(GroupKey {
            time_bucket: TimeBucket::from_key(&self.time_bucket)?,
            event: self.event.clone(),
            interrupt_bucket: InterruptBucket::from_key(&self.interrupt_bucket)?,
        })
    }
}

/// Per-agent induction state persisted at `<home>/persona_induction_state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInductionState {
    /// UTC calendar date (`YYYY-MM-DD`) induction last ran for this agent —
    /// the day-gate [`maybe_run_induction_tick`] checks.
    #[serde(default)]
    pub last_run_day: String,
    #[serde(default)]
    pub rules: Vec<InducedRuleState>,
}

fn load_state_map(path: &Path) -> HashMap<String, AgentInductionState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read-modify-write only `agent_id`'s entry, re-reading the map fresh
/// inside the lock (mirrors [`crate::proactive_feedback::calibrate_agent`])
/// so a concurrent writer for a *different* agent is never clobbered.
fn commit_agent_state(path: &Path, agent_id: &str, updated: AgentInductionState) -> io::Result<()> {
    duduclaw_core::with_file_lock(path, || {
        let mut map = load_state_map(path);
        map.insert(agent_id.to_string(), updated);
        let json = serde_json::to_string_pretty(&map)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(path, json)
    })
}

// ── Planning (pure — no I/O, no LLM) ─────────────────────────────────────

/// What one induction pass should do, computed purely from aggregated
/// evidence + current state. Separated from execution so the interesting
/// decision logic (dedup / GovMem bar / revoke / cap eviction) is unit
/// testable without a real memory engine or filesystem.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InductionPlan {
    /// Active rules whose situation saw a later `correct_detection`.
    pub revokes: Vec<InducedRuleState>,
    /// Active rules evicted (oldest-first) to make room under the cap.
    pub evictions: Vec<InducedRuleState>,
    /// Newly-eligible situations to induce, in deterministic
    /// (`GroupKey` ascending) order.
    pub inducts: Vec<GroupKey>,
}

impl InductionPlan {
    pub fn is_empty(&self) -> bool {
        self.revokes.is_empty() && self.evictions.is_empty() && self.inducts.is_empty()
    }
}

/// Compute the plan. Pure: same inputs always produce the same plan.
pub fn plan_induction(
    state: &AgentInductionState,
    fa_groups: &BTreeMap<GroupKey, FalseAlarmGroupStats>,
    cd_groups: &BTreeMap<GroupKey, DateTime<Utc>>,
    cap: usize,
) -> InductionPlan {
    // ── Revoke pass: any active rule whose situation saw a correct_detection
    // strictly after it was induced is proof-of-error — revoke unconditionally.
    let mut revokes = Vec::new();
    let mut still_active: Vec<InducedRuleState> = Vec::new();
    for r in &state.rules {
        if r.revoked {
            continue;
        }
        let hit = r
            .group_key()
            .and_then(|k| cd_groups.get(&k).copied())
            .is_some_and(|max_ts| max_ts > r.induced_at);
        if hit {
            revokes.push(r.clone());
        } else {
            still_active.push(r.clone());
        }
    }

    // ── Dedup: a situation with a still-active rule is never re-induced.
    let active_fps: HashSet<String> = still_active.iter().map(|r| r.fingerprint.clone()).collect();
    let inducts: Vec<GroupKey> = fa_groups
        .iter()
        .filter(|(_, stats)| govmem_eligible(stats))
        .filter(|(k, _)| !active_fps.contains(&k.fingerprint()))
        .map(|(k, _)| k.clone())
        .collect();

    // ── Cap eviction: for each new induction that would push the active
    // count over `cap`, evict the current oldest (by induced_at, ties broken
    // by fingerprint for determinism) from the still-active pool first.
    let mut pool = still_active.clone();
    pool.sort_by(|a, b| {
        a.induced_at
            .cmp(&b.induced_at)
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    let mut current = still_active.len();
    let mut evictions = Vec::new();
    let mut pool_idx = 0;
    for _ in &inducts {
        if current >= cap && pool_idx < pool.len() {
            evictions.push(pool[pool_idx].clone());
            pool_idx += 1;
            current -= 1;
        }
        current += 1;
    }

    InductionPlan {
        revokes,
        evictions,
        inducts,
    }
}

// ── Execution (I/O: temporal memory + key_facts bridge) ─────────────────

/// Write a new suppression rule for `key` — the [`induce_rule`] half of a
/// plan. `store_temporal`'s `(agent, subject, predicate)` conflict-resolution
/// key is `("proactive_suppression:{fingerprint}", "proactive_suppression")`
/// — the fingerprint lives in `subject` specifically so distinct situations
/// never collide (see [`GroupKey::fingerprint`] doc); a later
/// [`revoke_rule`] call for the *same* fingerprint automatically supersedes
/// this row via that same key.
async fn induce_rule(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    key: &GroupKey,
    now: DateTime<Utc>,
) -> Result<InducedRuleState, String> {
    let fingerprint = key.fingerprint();
    let content = suppression_content(key);
    let ceiling = duduclaw_memory::trust_ceiling(ORIGIN_CLASS);

    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.clone(),
        timestamp: now,
        tags: vec![
            "persona_induction".to_string(),
            "proactive_suppression".to_string(),
            PROBATION_RULE_TAG.to_string(),
        ],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 6.0,
        access_count: 0,
        last_accessed: None,
        source_event: INDUCTION_SOURCE_EVENT.to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(format!("proactive_suppression:{fingerprint}")),
        predicate: Some("proactive_suppression".to_string()),
        object: Some("suppress".to_string()),
        // v1.41 convention: confidence tracks the origin class's trust
        // ceiling rather than an arbitrary constant — a deterministic
        // system inference is never asserted with more confidence than its
        // provenance class allows.
        confidence: Some(ceiling),
        origin: Some(ORIGIN_CLASS.to_string()),
        origin_trust: Some(ceiling),
        metadata: Some(serde_json::json!({
            "fingerprint": fingerprint,
            "time_bucket": key.time_bucket.as_key(),
            "event": key.event,
            "interrupt_bucket": key.interrupt_bucket.as_key(),
        })),
        ..Default::default()
    };
    let memory_id = engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map_err(|e| e.to_string())?;

    // Bridge into key_facts (module doc "Where this sits") — best-effort,
    // never fails the induction itself.
    let key_fact_id = engine
        .store_fact(
            agent_id,
            &content,
            KEY_FACT_CHANNEL,
            KEY_FACT_CHAT_ID,
            &fingerprint,
        )
        .await
        .ok();

    Ok(InducedRuleState {
        fingerprint,
        time_bucket: key.time_bucket.as_key().to_string(),
        event: key.event.clone(),
        interrupt_bucket: key.interrupt_bucket.as_key().to_string(),
        memory_id,
        key_fact_id,
        induced_at: now,
        revoked: false,
        revoked_at: None,
        revoke_reason: None,
    })
}

/// Supersede an active rule's temporal-memory row (Janus probation revoke,
/// or cap eviction). Writes a closing-out row via the same
/// `(subject, predicate)` key `induce_rule` used, with a different
/// `object`/content — `store_temporal`'s conflict resolution supersedes the
/// prior row automatically (see module doc; verified in
/// [`tests::revoke_rule_supersedes_temporal_memory_row`]).
async fn revoke_rule(
    engine: &SqliteMemoryEngine,
    agent_id: &str,
    rule: &InducedRuleState,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let key = rule
        .group_key()
        .ok_or_else(|| format!("unparseable group key for fingerprint {}", rule.fingerprint))?;
    let content = revoke_content(&key, reason);
    let ceiling = duduclaw_memory::trust_ceiling(ORIGIN_CLASS);
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content,
        timestamp: now,
        tags: vec![
            "persona_induction".to_string(),
            "proactive_suppression_revoked".to_string(),
        ],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 1.0,
        access_count: 0,
        last_accessed: None,
        source_event: INDUCTION_SOURCE_EVENT.to_string(),
    };
    let meta = TemporalMeta {
        subject: Some(format!("proactive_suppression:{}", rule.fingerprint)),
        predicate: Some("proactive_suppression".to_string()),
        object: Some("lifted".to_string()),
        confidence: Some(ceiling),
        origin: Some(ORIGIN_CLASS.to_string()),
        origin_trust: Some(ceiling),
        metadata: Some(serde_json::json!({ "revoke_reason": reason })),
        ..Default::default()
    };
    engine
        .store_temporal(agent_id, entry, meta)
        .await
        .map_err(|e| e.to_string())
}

fn mark_revoked(
    state: &mut AgentInductionState,
    fingerprint: &str,
    reason: &str,
    now: DateTime<Utc>,
) {
    if let Some(r) = state
        .rules
        .iter_mut()
        .find(|r| r.fingerprint == fingerprint && !r.revoked)
    {
        r.revoked = true;
        r.revoked_at = Some(now);
        r.revoke_reason = Some(reason.to_string());
    }
}

// ── Top-level orchestration ─────────────────────────────────────────────

/// Result of one [`run_induction_for_agent`] pass, for logging/tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InductionReport {
    pub induced: Vec<String>,
    pub revoked: Vec<String>,
    pub evicted: Vec<String>,
}

impl InductionReport {
    pub fn is_empty(&self) -> bool {
        self.induced.is_empty() && self.revoked.is_empty() && self.evicted.is_empty()
    }
}

/// Run one full induction pass for `agent_id`: aggregate
/// `proactive_gate.jsonl`, plan, execute (revoke → evict → induce), persist
/// state. Unconditional — callers wanting the daily cost-gate should use
/// [`maybe_run_induction_tick`] instead.
pub async fn run_induction_for_agent(
    home_dir: &Path,
    agent_id: &str,
    now: DateTime<Utc>,
) -> io::Result<InductionReport> {
    let records = read_gate_records(&home_dir.join("proactive_gate.jsonl"), agent_id)?;
    let fa_groups = aggregate_false_alarms(&records);
    let cd_groups = aggregate_correct_detections(&records);

    let state_path = home_dir.join(STATE_FILE_NAME);
    let mut agent_state = load_state_map(&state_path)
        .remove(agent_id)
        .unwrap_or_default();

    let plan = plan_induction(
        &agent_state,
        &fa_groups,
        &cd_groups,
        MAX_ACTIVE_RULES_PER_AGENT,
    );

    let mut report = InductionReport::default();
    if !plan.is_empty() {
        let db_path = home_dir.join("memory.db");
        let engine = SqliteMemoryEngine::new(&db_path)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

        for r in &plan.revokes {
            match revoke_rule(&engine, agent_id, r, REASON_CORRECT_DETECTION, now).await {
                Ok(_) => {
                    mark_revoked(
                        &mut agent_state,
                        &r.fingerprint,
                        REASON_CORRECT_DETECTION,
                        now,
                    );
                    report.revoked.push(r.fingerprint.clone());
                }
                Err(e) => warn!(
                    agent = %agent_id, fingerprint = %r.fingerprint, error = %e,
                    "persona induction: revoke (correct_detection) failed"
                ),
            }
        }
        for r in &plan.evictions {
            match revoke_rule(&engine, agent_id, r, REASON_CAP_EVICTION, now).await {
                Ok(_) => {
                    mark_revoked(&mut agent_state, &r.fingerprint, REASON_CAP_EVICTION, now);
                    report.evicted.push(r.fingerprint.clone());
                }
                Err(e) => warn!(
                    agent = %agent_id, fingerprint = %r.fingerprint, error = %e,
                    "persona induction: cap eviction failed"
                ),
            }
        }
        for key in &plan.inducts {
            match induce_rule(&engine, agent_id, key, now).await {
                Ok(state_entry) => {
                    report.induced.push(state_entry.fingerprint.clone());
                    agent_state.rules.push(state_entry);
                }
                Err(e) => warn!(
                    agent = %agent_id, fingerprint = %key.fingerprint(), error = %e,
                    "persona induction: induce failed"
                ),
            }
        }
    }

    agent_state.last_run_day = now.format("%Y-%m-%d").to_string();
    commit_agent_state(&state_path, agent_id, agent_state)?;
    Ok(report)
}

/// Day-gated wrapper: skip the (potentially `O(file)`) pass entirely when
/// already run today for this agent (see module doc "Cost: daily tick").
pub async fn maybe_run_induction_tick(
    home_dir: &Path,
    agent_id: &str,
    now: DateTime<Utc>,
) -> io::Result<Option<InductionReport>> {
    let state_path = home_dir.join(STATE_FILE_NAME);
    let today = now.format("%Y-%m-%d").to_string();
    let already_ran = load_state_map(&state_path)
        .get(agent_id)
        .is_some_and(|s| s.last_run_day == today);
    if already_ran {
        return Ok(None);
    }
    run_induction_for_agent(home_dir, agent_id, now)
        .await
        .map(Some)
}

/// Spawn the periodic induction loop. Ticks every [`INDUCTION_TICK_INTERVAL`]
/// (hourly); each tick runs [`maybe_run_induction_tick`] for every agent with
/// `[proactive] enabled = true` (same enablement gate
/// [`crate::proactive_feedback::spawn_feedback_loop`] uses) — a day-old
/// no-op for agents whose induction already ran today. Independent
/// process-lifetime task (mirrors the P2-3/P2-4 background-loop precedent);
/// deliberately not folded into `proactive_feedback`'s 60s loop so that
/// already-tested loop body stays untouched.
pub fn spawn_induction_loop(
    home_dir: PathBuf,
    agent_registry: Arc<RwLock<AgentRegistry>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(INDUCTION_TICK_INTERVAL);
        loop {
            interval.tick().await;

            let enabled_agents: Vec<String> = {
                let reg = agent_registry.read().await;
                reg.list()
                    .iter()
                    .filter_map(|a| {
                        let id = a
                            .dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(String::from)?;
                        crate::proactive_gate::read_proactive_config(&a.dir)
                            .enabled
                            .then_some(id)
                    })
                    .collect()
            };
            for agent_id in enabled_agents {
                match maybe_run_induction_tick(&home_dir, &agent_id, Utc::now()).await {
                    Ok(Some(report)) if !report.is_empty() => {
                        info!(
                            agent = %agent_id,
                            induced = report.induced.len(),
                            revoked = report.revoked.len(),
                            evicted = report.evicted.len(),
                            "persona induction: rules updated"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!(agent = %agent_id, error = %e, "persona induction: tick failed")
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pi-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn append_line(path: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn gate_line(
        ts: DateTime<Utc>,
        agent: &str,
        event: &str,
        decision: &str,
        interruptibility: f32,
        outcome: Option<&str>,
    ) -> String {
        serde_json::json!({
            "ts": ts.to_rfc3339(),
            "agent": agent,
            "event": event,
            "score": 5,
            "threshold": 3,
            "interruptibility": interruptibility,
            "decision": decision,
            "reason": "allowed",
            "latency_ms": 10,
            "outcome": outcome,
        })
        .to_string()
    }

    // ── time_bucket_of / interrupt_bucket_of (pure classifiers) ─────────

    #[test]
    fn time_bucket_of_boundaries() {
        // 2026-01-01T01:00:00Z = 09:00 Asia/Taipei → work hours.
        let work = DateTime::parse_from_rfc3339("2026-01-01T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(time_bucket_of(work), TimeBucket::WorkHours);
        // 2026-01-01T14:30:00Z = 22:30 Asia/Taipei → night.
        let night = DateTime::parse_from_rfc3339("2026-01-01T14:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(time_bucket_of(night), TimeBucket::Night);
        // 2026-01-01T22:00:00Z = 06:00 Asia/Taipei next day → still night.
        let early = DateTime::parse_from_rfc3339("2026-01-01T22:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(time_bucket_of(early), TimeBucket::Night);
    }

    #[test]
    fn interrupt_bucket_of_boundaries() {
        assert_eq!(interrupt_bucket_of(0.0), InterruptBucket::Low);
        assert_eq!(interrupt_bucket_of(0.33), InterruptBucket::Low);
        assert_eq!(interrupt_bucket_of(0.34), InterruptBucket::Mid);
        assert_eq!(interrupt_bucket_of(0.66), InterruptBucket::Mid);
        assert_eq!(interrupt_bucket_of(0.67), InterruptBucket::High);
        assert_eq!(interrupt_bucket_of(1.0), InterruptBucket::High);
    }

    // ── aggregate_false_alarms (分組聚合) ─────────────────────────────

    #[test]
    fn aggregate_false_alarms_groups_by_all_three_dimensions() {
        let night = DateTime::parse_from_rfc3339("2026-01-01T14:00:00Z")
            .unwrap()
            .with_timezone(&Utc); // 22:00 TPE
        let work = DateTime::parse_from_rfc3339("2026-01-01T04:00:00Z")
            .unwrap()
            .with_timezone(&Utc); // 12:00 TPE
        let records = vec![
            GateRecord {
                ts: night,
                event: "os_file".into(),
                decision: "allow".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.1,
            },
            GateRecord {
                ts: night,
                event: "os_file".into(),
                decision: "allow".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.1,
            },
            // Different event → different group.
            GateRecord {
                ts: night,
                event: "agent_idle".into(),
                decision: "allow".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.1,
            },
            // Different time bucket → different group.
            GateRecord {
                ts: work,
                event: "os_file".into(),
                decision: "allow".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.1,
            },
            // Different interruptibility bucket → different group.
            GateRecord {
                ts: night,
                event: "os_file".into(),
                decision: "allow".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.9,
            },
            // Not a false_alarm → excluded entirely.
            GateRecord {
                ts: night,
                event: "os_file".into(),
                decision: "allow".into(),
                outcome: Some("correct_detection".into()),
                interruptibility: 0.1,
            },
            // Suppress decision → excluded (false_alarm is only defined for allow).
            GateRecord {
                ts: night,
                event: "os_file".into(),
                decision: "suppress".into(),
                outcome: Some("false_alarm".into()),
                interruptibility: 0.1,
            },
        ];
        let groups = aggregate_false_alarms(&records);
        assert_eq!(groups.len(), 4, "4 distinct (time,event,interrupt) groups");
        let key = GroupKey {
            time_bucket: TimeBucket::Night,
            event: "os_file".into(),
            interrupt_bucket: InterruptBucket::Low,
        };
        assert_eq!(groups[&key].count, 2);
    }

    // ── govmem_eligible (GovMem 門檻：同日3次不歸納、跨2日歸納) ────────

    #[test]
    fn govmem_same_day_three_times_is_not_eligible() {
        let mut stats = FalseAlarmGroupStats::default();
        let day = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        stats.count = 3;
        stats.distinct_days.insert(day);
        assert!(
            !govmem_eligible(&stats),
            "3 false alarms on ONE day must not be eligible"
        );
    }

    #[test]
    fn govmem_two_distinct_days_is_eligible() {
        let mut stats = FalseAlarmGroupStats::default();
        stats.count = 3;
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        assert!(
            govmem_eligible(&stats),
            "3 false alarms spanning 2 distinct days must be eligible"
        );
    }

    #[test]
    fn govmem_below_count_is_not_eligible_even_with_distinct_days() {
        let mut stats = FalseAlarmGroupStats::default();
        stats.count = 2;
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        assert!(
            !govmem_eligible(&stats),
            "2 < FA_MIN_COUNT must not be eligible regardless of day spread"
        );
    }

    // ── plan_induction: dedup ───────────────────────────────────────────

    fn eligible_group_map(key: GroupKey) -> BTreeMap<GroupKey, FalseAlarmGroupStats> {
        let mut stats = FalseAlarmGroupStats::default();
        stats.count = 5;
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        stats
            .distinct_days
            .insert(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        let mut m = BTreeMap::new();
        m.insert(key, stats);
        m
    }

    fn sample_key() -> GroupKey {
        GroupKey {
            time_bucket: TimeBucket::Night,
            event: "os_file".into(),
            interrupt_bucket: InterruptBucket::Low,
        }
    }

    fn sample_rule_state(fingerprint: &str, induced_at: DateTime<Utc>) -> InducedRuleState {
        InducedRuleState {
            fingerprint: fingerprint.to_string(),
            time_bucket: TimeBucket::Night.as_key().to_string(),
            event: "os_file".to_string(),
            interrupt_bucket: InterruptBucket::Low.as_key().to_string(),
            memory_id: "mem-1".to_string(),
            key_fact_id: None,
            induced_at,
            revoked: false,
            revoked_at: None,
            revoke_reason: None,
        }
    }

    #[test]
    fn plan_induction_dedups_already_active_group() {
        let key = sample_key();
        let fa_groups = eligible_group_map(key.clone());
        let cd_groups = BTreeMap::new();
        let now = Utc::now();
        let mut state = AgentInductionState::default();
        state.rules.push(sample_rule_state(
            &key.fingerprint(),
            now - chrono::Duration::hours(1),
        ));

        let plan = plan_induction(&state, &fa_groups, &cd_groups, MAX_ACTIVE_RULES_PER_AGENT);
        assert!(
            plan.inducts.is_empty(),
            "already-active group must not be re-induced"
        );
        assert!(plan.revokes.is_empty());
        assert!(plan.evictions.is_empty());
    }

    #[test]
    fn plan_induction_inducts_new_eligible_group_when_none_active() {
        let key = sample_key();
        let fa_groups = eligible_group_map(key.clone());
        let cd_groups = BTreeMap::new();
        let state = AgentInductionState::default();

        let plan = plan_induction(&state, &fa_groups, &cd_groups, MAX_ACTIVE_RULES_PER_AGENT);
        assert_eq!(plan.inducts, vec![key]);
    }

    #[test]
    fn plan_induction_ineligible_group_never_inducted() {
        let key = sample_key();
        let mut stats = FalseAlarmGroupStats::default();
        stats.count = 1; // below threshold
        let mut fa_groups = BTreeMap::new();
        fa_groups.insert(key, stats);
        let state = AgentInductionState::default();

        let plan = plan_induction(
            &state,
            &fa_groups,
            &BTreeMap::new(),
            MAX_ACTIVE_RULES_PER_AGENT,
        );
        assert!(plan.inducts.is_empty());
    }

    // ── plan_induction: probation supersede (revoke on correct_detection) ─

    #[test]
    fn plan_induction_revokes_active_rule_on_later_correct_detection() {
        let key = sample_key();
        let induced_at = Utc::now() - chrono::Duration::hours(2);
        let mut state = AgentInductionState::default();
        state
            .rules
            .push(sample_rule_state(&key.fingerprint(), induced_at));

        let mut cd_groups = BTreeMap::new();
        cd_groups.insert(key.clone(), induced_at + chrono::Duration::hours(1)); // after induction

        let plan = plan_induction(
            &state,
            &BTreeMap::new(),
            &cd_groups,
            MAX_ACTIVE_RULES_PER_AGENT,
        );
        assert_eq!(plan.revokes.len(), 1);
        assert_eq!(plan.revokes[0].fingerprint, key.fingerprint());
    }

    #[test]
    fn plan_induction_does_not_revoke_on_correct_detection_before_induction() {
        // A correct_detection that predates the rule is not proof the rule
        // is wrong — the rule was induced *because* of what happened after.
        let key = sample_key();
        let induced_at = Utc::now();
        let mut state = AgentInductionState::default();
        state
            .rules
            .push(sample_rule_state(&key.fingerprint(), induced_at));

        let mut cd_groups = BTreeMap::new();
        cd_groups.insert(key, induced_at - chrono::Duration::hours(1)); // before induction

        let plan = plan_induction(
            &state,
            &BTreeMap::new(),
            &cd_groups,
            MAX_ACTIVE_RULES_PER_AGENT,
        );
        assert!(plan.revokes.is_empty());
    }

    #[test]
    fn plan_induction_already_revoked_rule_is_not_revoked_again() {
        let key = sample_key();
        let induced_at = Utc::now() - chrono::Duration::hours(3);
        let mut rule = sample_rule_state(&key.fingerprint(), induced_at);
        rule.revoked = true;
        rule.revoked_at = Some(induced_at + chrono::Duration::hours(1));
        rule.revoke_reason = Some(REASON_CORRECT_DETECTION.to_string());
        let mut state = AgentInductionState::default();
        state.rules.push(rule);

        let mut cd_groups = BTreeMap::new();
        cd_groups.insert(key, induced_at + chrono::Duration::hours(2));

        let plan = plan_induction(
            &state,
            &BTreeMap::new(),
            &cd_groups,
            MAX_ACTIVE_RULES_PER_AGENT,
        );
        assert!(
            plan.revokes.is_empty(),
            "an already-revoked rule must not surface again"
        );
    }

    // ── plan_induction: cap eviction (上限淘汰) ──────────────────────────

    #[test]
    fn plan_induction_evicts_oldest_active_when_cap_reached() {
        let cap = 3usize;
        let base = Utc::now() - chrono::Duration::days(10);
        let mut state = AgentInductionState::default();
        let mut oldest_fp = String::new();
        for i in 0..cap {
            let ev = format!("event_{i}");
            let key = GroupKey {
                time_bucket: TimeBucket::Night,
                event: ev,
                interrupt_bucket: InterruptBucket::Low,
            };
            let induced_at = base + chrono::Duration::hours(i as i64); // ascending: index 0 is oldest
            if i == 0 {
                oldest_fp = key.fingerprint();
            }
            state
                .rules
                .push(sample_rule_state(&key.fingerprint(), induced_at));
        }
        // A brand-new eligible group not already active.
        let new_key = GroupKey {
            time_bucket: TimeBucket::WorkHours,
            event: "os_new".into(),
            interrupt_bucket: InterruptBucket::High,
        };
        let fa_groups = eligible_group_map(new_key.clone());

        let plan = plan_induction(&state, &fa_groups, &BTreeMap::new(), cap);
        assert_eq!(plan.inducts, vec![new_key]);
        assert_eq!(plan.evictions.len(), 1);
        assert_eq!(
            plan.evictions[0].fingerprint, oldest_fp,
            "must evict the OLDEST active rule"
        );
    }

    #[test]
    fn plan_induction_no_eviction_when_under_cap() {
        let cap = 5usize;
        let mut state = AgentInductionState::default();
        state.rules.push(sample_rule_state(
            "existing",
            Utc::now() - chrono::Duration::hours(1),
        ));
        let new_key = GroupKey {
            time_bucket: TimeBucket::WorkHours,
            event: "os_new".into(),
            interrupt_bucket: InterruptBucket::High,
        };
        let fa_groups = eligible_group_map(new_key.clone());

        let plan = plan_induction(&state, &fa_groups, &BTreeMap::new(), cap);
        assert_eq!(plan.inducts, vec![new_key]);
        assert!(plan.evictions.is_empty());
    }

    // ── store_temporal write shape (origin/confidence) ──────────────────

    #[tokio::test]
    async fn induce_rule_writes_temporal_memory_with_correct_origin_and_confidence() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let key = sample_key();
        let now = Utc::now();

        let state = induce_rule(&engine, "agent-x", &key, now).await.unwrap();

        let ceiling = duduclaw_memory::trust_ceiling("agent_derived");
        let origin = engine
            .get_origin("agent-x", &state.memory_id)
            .await
            .unwrap()
            .flatten();
        assert_eq!(
            origin.as_deref(),
            Some("agent_derived"),
            "origin must be the system-induction class"
        );

        let trust = engine
            .get_origin_trust("agent-x", &state.memory_id)
            .await
            .unwrap();
        assert_eq!(
            trust,
            Some(ceiling),
            "origin_trust must equal the agent_derived ceiling (0.6)"
        );

        let history = engine
            .get_history(
                "agent-x",
                &format!("proactive_suppression:{}", key.fingerprint()),
                "proactive_suppression",
            )
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert!(
            (history[0].confidence - ceiling).abs() < f64::EPSILON,
            "confidence must equal the origin ceiling"
        );
        assert!(
            history[0].valid_until.is_none(),
            "freshly induced rule must be currently valid"
        );
    }

    #[tokio::test]
    async fn induce_rule_bridges_into_key_facts_for_gate_retrieval() {
        // Verifies the claim in the module doc: the induced rule is actually
        // reachable via the SAME search_facts path
        // `autopilot_engine::fetch_persona_lines` calls — not just written to
        // temporal memory (a different table search_facts never queries).
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let key = GroupKey {
            time_bucket: TimeBucket::Night,
            event: "os_file".into(),
            interrupt_bucket: InterruptBucket::Low,
        };
        let state = induce_rule(&engine, "agent-y", &key, Utc::now())
            .await
            .unwrap();
        assert!(state.key_fact_id.is_some());

        let found = engine
            .search_facts("agent-y", "深夜 file", 5)
            .await
            .unwrap();
        assert!(
            found.iter().any(|f| f.id == state.key_fact_id.clone().unwrap()),
            "the induced rule's key_facts row must be findable via search_facts (the gate's real retrieval path)"
        );
    }

    // ── revoke_rule supersession ─────────────────────────────────────────

    #[tokio::test]
    async fn revoke_rule_supersedes_temporal_memory_row() {
        let engine = SqliteMemoryEngine::in_memory().unwrap();
        let key = sample_key();
        let induced = induce_rule(&engine, "agent-z", &key, Utc::now())
            .await
            .unwrap();

        let subj = format!("proactive_suppression:{}", key.fingerprint());
        let before = engine
            .get_history("agent-z", &subj, "proactive_suppression")
            .await
            .unwrap();
        assert_eq!(before.len(), 1);
        assert!(before[0].valid_until.is_none());

        let rule_state = InducedRuleState {
            fingerprint: key.fingerprint(),
            time_bucket: key.time_bucket.as_key().to_string(),
            event: key.event.clone(),
            interrupt_bucket: key.interrupt_bucket.as_key().to_string(),
            memory_id: induced.memory_id.clone(),
            key_fact_id: None,
            induced_at: Utc::now() - chrono::Duration::hours(1),
            revoked: false,
            revoked_at: None,
            revoke_reason: None,
        };
        revoke_rule(
            &engine,
            "agent-z",
            &rule_state,
            REASON_CORRECT_DETECTION,
            Utc::now(),
        )
        .await
        .unwrap();

        let after = engine
            .get_history("agent-z", &subj, "proactive_suppression")
            .await
            .unwrap();
        assert_eq!(
            after.len(),
            2,
            "supersession must append a new row, not mutate in place"
        );
        assert_eq!(after[0].id, induced.memory_id);
        assert!(
            after[0].valid_until.is_some(),
            "old suppression row must be closed out"
        );
        assert_eq!(
            after[0].superseded_by.as_deref(),
            Some(after[1].id.as_str())
        );
        assert!(
            after[1].valid_until.is_none(),
            "revocation row is now the current one"
        );
    }

    // ── end-to-end: run_induction_for_agent ─────────────────────────────

    #[tokio::test]
    async fn run_induction_for_agent_end_to_end_induces_then_revokes() {
        let home = temp_home();
        let gate_path = home.join("proactive_gate.jsonl");
        let day1 = Utc::now() - chrono::Duration::days(2);
        let day2 = Utc::now() - chrono::Duration::days(1);
        // 14:00 UTC = 22:00 Taipei → Night bucket, matches
        // `time_bucket_of`'s definition consistently for both timestamps.
        let night1 = day1.date_naive().and_hms_opt(14, 0, 0).unwrap().and_utc();
        let night2 = day2.date_naive().and_hms_opt(14, 0, 0).unwrap().and_utc();

        append_line(
            &gate_path,
            &gate_line(
                night1,
                "agent-e2e",
                "os_file",
                "allow",
                0.1,
                Some("false_alarm"),
            ),
        );
        append_line(
            &gate_path,
            &gate_line(
                night1,
                "agent-e2e",
                "os_file",
                "allow",
                0.1,
                Some("false_alarm"),
            ),
        );
        append_line(
            &gate_path,
            &gate_line(
                night2,
                "agent-e2e",
                "os_file",
                "allow",
                0.1,
                Some("false_alarm"),
            ),
        );

        let report = run_induction_for_agent(&home, "agent-e2e", Utc::now())
            .await
            .unwrap();
        assert_eq!(
            report.induced.len(),
            1,
            "3 false alarms across 2 distinct days must induce one rule"
        );
        assert!(report.revoked.is_empty());

        // Idempotent-ish: a second immediate run does not re-induce the same
        // group (dedup) nor find anything new to do.
        let report2 = run_induction_for_agent(&home, "agent-e2e", Utc::now())
            .await
            .unwrap();
        assert!(report2.is_empty());

        // Now a correct_detection lands in the SAME situation, after the
        // rule was induced — must revoke it on the next pass. The timestamp
        // must satisfy two things at once: (a) strictly after `induced_at`
        // (the moment `report` was produced above), since the revoke check
        // is `max_ts > r.induced_at`; and (b) the same `GroupKey` —
        // notably the same `time_bucket_of` result — as `night1`/`night2`.
        // Pinning to a fixed 14:00 UTC (the same Night-bucket UTC hour used
        // for night1/night2) a few days out satisfies both, deterministically,
        // regardless of the wall-clock time the test happens to run at.
        // Using `Utc::now()` here previously made this test flaky: whenever
        // the suite ran outside the 14:00–22:59 UTC window (Taipei
        // 22:00–06:59), `Utc::now()` fell into the WorkHours bucket instead
        // of Night, landed in a different `GroupKey`, and `report3.revoked`
        // stayed empty.
        let day3 = day2 + chrono::Duration::days(3);
        let cd_ts = day3.date_naive().and_hms_opt(14, 0, 0).unwrap().and_utc();
        append_line(
            &gate_path,
            &gate_line(
                cd_ts,
                "agent-e2e",
                "os_file",
                "allow",
                0.1,
                Some("correct_detection"),
            ),
        );

        let report3 = run_induction_for_agent(&home, "agent-e2e", Utc::now())
            .await
            .unwrap();
        assert_eq!(
            report3.revoked.len(),
            1,
            "a later correct_detection in the same situation must revoke the rule"
        );
    }

    #[tokio::test]
    async fn maybe_run_induction_tick_is_day_gated() {
        let home = temp_home();
        let now = Utc::now();
        let first = maybe_run_induction_tick(&home, "agent-gate", now)
            .await
            .unwrap();
        assert!(first.is_some(), "first call today must run");

        let second = maybe_run_induction_tick(&home, "agent-gate", now)
            .await
            .unwrap();
        assert!(
            second.is_none(),
            "second call the same UTC day must be a no-op"
        );

        let tomorrow = now + chrono::Duration::days(1);
        let third = maybe_run_induction_tick(&home, "agent-gate", tomorrow)
            .await
            .unwrap();
        assert!(third.is_some(), "a new UTC day must run again");
    }
}
