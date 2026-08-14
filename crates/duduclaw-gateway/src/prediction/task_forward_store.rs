//! WP-A7 — statistical buckets + `TaskForwardModel` storage.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §2.4, §5.
//! Split out of `task_forward.rs` (WP-A2/A8: schema + diff algorithm) purely
//! to keep both files under the project's ~800-line convention — this
//! module depends on `task_forward`'s types, `task_forward` has zero
//! dependency back on this one.
//!
//! WP-A9 wires the two hot-path hooks (`goal_loop.rs` predict,
//! `dispatch_engine.rs` settle) and adds [`TaskForwardModelConfig`] (design
//! §7.3) plus [`TaskForwardModel::get_prediction`] (the settle-side lookup
//! the design's §4.2 "converge into one settle call" plan needs).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::warn;

use super::task_forward::{
    ArtifactShape, ExpectedOutcome, GoalKind, ObservedOutcome, PredictionSource, TaskPrediction,
    TaskPredictionError, TaskStateKey,
};
use super::tool_class::ToolClass;

// ═══════════════════════════════════════════════════════════════════════
// §2.4 / §5 — statistical buckets
// ═══════════════════════════════════════════════════════════════════════

/// Minimum settled samples in the canonical bucket before it's trusted
/// (design §2.3 stage 1). Same value used for the marginal bucket (stage
/// 2).
pub const MIN_SAMPLES: u32 = 3;
/// Sample count at which `confidence` saturates to `1.0`.
pub const MATURE_N: u32 = 10;
/// Bounded ring buffer size for `call_counts` — keeps the persisted model
/// small and biases percentiles toward recent behavior instead of growing
/// unboundedly (mirrors `consecutive_errors`'s 10-entry ring buffer in
/// `engine.rs`, sized up since call-volume needs more samples for a stable
/// percentile).
const CALL_COUNT_HISTORY_CAP: usize = 50;

// ═══════════════════════════════════════════════════════════════════════
// WP-A9 §7.3 — `[task_forward_model]` config
// ═══════════════════════════════════════════════════════════════════════

/// `[task_forward_model]` config (design §7.3). **`enabled` defaults to
/// `false`** — with it unset/false, the two hot-path hooks
/// (`goal_loop.rs`'s predict-before-dispatch, `dispatch_engine.rs`'s
/// settle-on-review) never construct a `TaskForwardModel` and are complete
/// no-ops, so `enabled = false` behavior is byte-identical to before this
/// change (design §7.3 "回退路徑").
///
/// Parsed in isolation from a generic `toml::Table` — same convention as
/// `GoalLoopConfig::from_home` (`goal_loop.rs`) — so a missing/malformed
/// `[task_forward_model]` section, or a missing/malformed `config.toml`
/// altogether, always resolves to [`TaskForwardModelConfig::default`]
/// rather than failing or panicking.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TaskForwardModelConfig {
    /// Master switch. `false` ⇒ both hot-path hooks are no-ops.
    pub enabled: bool,
    /// T3 (design §11, §2.3 stage 4): reserved for the cold-start LLM
    /// prediction-source stage. **Not consumed** by
    /// `TaskForwardModel::predict` in this version — a fully-cold bucket
    /// always falls to `PredictionSource::Prior` regardless of this flag
    /// (see that variant's doc comment in `task_forward.rs`). Parsed now so
    /// a future wiring of stage 4 doesn't need a config schema change.
    pub cold_start_llm: bool,
    /// Design §2.3 stage 1/2 sample-count gate. **Not yet threaded into**
    /// `TaskForwardModel::predict`, which uses the fixed [`MIN_SAMPLES`]
    /// constant — parsed for forward-compatibility with a future change
    /// that makes the cascade's thresholds runtime-configurable, not
    /// currently load-bearing.
    pub min_samples: u32,
    /// Design §2.3 confidence-saturation sample count. Same
    /// not-yet-threaded status as `min_samples` — see that field's doc
    /// comment. `TaskForwardModel::predict` uses the fixed [`MATURE_N`]
    /// constant today.
    pub mature_n: u32,
    /// WP-B4 `foresight_gate` τ (design §6.4, T7). Consumed by
    /// `foresight_gate::ForesightGateConfig`, not by this module.
    pub foresight_tau: f64,
    /// WP-B4 `foresight_gate` recent-k window (design §6.4, T7). Consumed by
    /// `foresight_gate::ForesightGateConfig`, not by this module.
    pub foresight_recent_k: usize,
    /// WP-A4 sub-switch (design §6.5): the induce/inject/settle rule
    /// pipeline (`prediction::task_rule_induce`). Only consulted when
    /// `enabled = true` — A4 is a strict downstream of A3, so `enabled =
    /// false` already turns the whole thing off regardless of this flag.
    /// Defaults `true` (opt-out, not opt-in) since it only ever *adds*
    /// zero-LLM template rules gated by the same B1 novelty gate every other
    /// consolidated-rule write path already uses; set `false` to keep A3's
    /// prediction/observation/diff machinery running while suppressing rule
    /// writes and injection entirely.
    pub rule_induction: bool,
    /// WP-P2 (`commercial/docs/DESIGN-lwm-calibration-2026-08-10.md` §4
    /// platform section): gates the calibrated-forward-model scoring path
    /// — proper-scoring (`prediction::calibration::brier_binary`) of every
    /// settled `TaskPrediction.confidence` against whether the round turned
    /// out well. **Defaults `true` (v1.54).** This is a strictly additive,
    /// domain-agnostic `(confidence, realized_outcome)` scorer — it never
    /// gates dispatch/accept behavior, only whether `brier_score` gets
    /// computed and persisted (a nullable column / nullable transition
    /// fields). Nothing here runs unless the outer `enabled` master switch
    /// (still default `false`) is on, so turning this default on cannot
    /// change any agent's behavior until the forward model itself is enabled.
    /// Set `false` to keep the forward model running while skipping the
    /// calibration calculation entirely (leaving those fields `NULL`/`None`,
    /// byte-identical to the pre-calibration behavior).
    pub calibration_enabled: bool,
    /// WP-P3 + v1.54 shadow→promotion pass
    /// (`commercial/docs/DESIGN-lwm-calibration-2026-08-10.md` §1.4, §6):
    /// gates the held-out rule gate — the anti-overfitting adoption authority
    /// that turns self-learning into "reflection PROPOSES a candidate, an
    /// external numeric oracle DECIDES adoption". **Defaults `true`
    /// (v1.54).** When on, inductive/task-layer lessons are born as shadow
    /// candidates (`reflexion::consolidate_group`, `task_rule_induce`),
    /// excluded from injection (`rule_lifecycle::select_*`), scored
    /// out-of-sample every settle (`dispatch_engine`'s injected-rule held-out
    /// path AND the shadow→promotion prequential pass), and promoted only
    /// when their record beats the frozen climatology baseline. Nothing here
    /// runs unless the outer `enabled` master switch (still default `false`)
    /// is on. Set `false` to keep the forward model running while settlement
    /// uses the unchanged `ErrorCategory`-credit lifecycle and no shadow tags
    /// are ever minted (byte-identical to the pre-WP3 behavior). See
    /// `prediction::rule_gate`.
    pub held_out_gate_enabled: bool,
}

impl Default for TaskForwardModelConfig {
    fn default() -> Self {
        Self {
            // v1.54: the calibrated forward model is a default-ON platform
            // capability. The master switch is on so predict-act-verify,
            // proper-scoring, and the held-out rule gate actually run for every
            // agent out of the box. `cold_start_llm` stays off, so cold starts
            // remain zero-LLM (statistical/prior degradation only). Operators
            // opt out per-deployment via `config.toml [task_forward_model]` or
            // the dashboard toggle.
            enabled: true,
            cold_start_llm: false,
            min_samples: MIN_SAMPLES,
            mature_n: MATURE_N,
            foresight_tau: super::foresight_gate::DEFAULT_FORESIGHT_TAU,
            foresight_recent_k: super::foresight_gate::DEFAULT_FORESIGHT_RECENT_K,
            rule_induction: true,
            // calibrated forward model + held-out rule gate: on with the master.
            calibration_enabled: true,
            held_out_gate_enabled: true,
        }
    }
}

impl TaskForwardModelConfig {
    /// Load `[task_forward_model]` from `<home>/config.toml`. Absent /
    /// malformed section, or absent / malformed `config.toml` ⇒
    /// [`Self::default`] (`enabled = true` as of v1.54 — the capability is
    /// on out of the box; operators opt out explicitly).
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("task_forward_model") {
            Some(section) => section
                .clone()
                .try_into::<TaskForwardModelConfig>()
                .unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// Per-`ToolClass` frequency counters. A fixed-field struct (not a
/// `HashMap<ToolClass, u32>`) so the model serializes as plain JSON object
/// fields — no enum-as-map-key serde subtleties to reason about, and every
/// field is directly inspectable in a DB dump.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolClassCounts {
    pub read: u32,
    pub write: u32,
    pub search: u32,
    pub exec: u32,
    pub net: u32,
    pub delegate: u32,
    pub task_board: u32,
    pub memory: u32,
    pub approval: u32,
    pub other: u32,
}

impl ToolClassCounts {
    fn bump(&mut self, class: ToolClass) {
        let field = match class {
            ToolClass::Read => &mut self.read,
            ToolClass::Write => &mut self.write,
            ToolClass::Search => &mut self.search,
            ToolClass::Exec => &mut self.exec,
            ToolClass::Net => &mut self.net,
            ToolClass::Delegate => &mut self.delegate,
            ToolClass::TaskBoard => &mut self.task_board,
            ToolClass::Memory => &mut self.memory,
            ToolClass::Approval => &mut self.approval,
            ToolClass::Other => &mut self.other,
        };
        *field += 1;
    }

    fn get(&self, class: ToolClass) -> u32 {
        match class {
            ToolClass::Read => self.read,
            ToolClass::Write => self.write,
            ToolClass::Search => self.search,
            ToolClass::Exec => self.exec,
            ToolClass::Net => self.net,
            ToolClass::Delegate => self.delegate,
            ToolClass::TaskBoard => self.task_board,
            ToolClass::Memory => self.memory,
            ToolClass::Approval => self.approval,
            ToolClass::Other => self.other,
        }
    }

    /// Classes whose `freq / n_samples >= 0.5` (design §2.4).
    fn frequent(&self, n_samples: u32) -> BTreeSet<ToolClass> {
        if n_samples == 0 {
            return BTreeSet::new();
        }
        const ALL: [ToolClass; 10] = [
            ToolClass::Read,
            ToolClass::Write,
            ToolClass::Search,
            ToolClass::Exec,
            ToolClass::Net,
            ToolClass::Delegate,
            ToolClass::TaskBoard,
            ToolClass::Memory,
            ToolClass::Approval,
            ToolClass::Other,
        ];
        ALL.into_iter()
            .filter(|c| self.get(*c) as f64 / n_samples as f64 >= 0.5)
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub accept: u32,
    pub reject: u32,
    pub blocked: u32,
    pub escalate: u32,
}

impl OutcomeCounts {
    fn bump(&mut self, outcome: ExpectedOutcome) {
        match outcome {
            ExpectedOutcome::Accept => self.accept += 1,
            ExpectedOutcome::Reject => self.reject += 1,
            ExpectedOutcome::Blocked => self.blocked += 1,
            ExpectedOutcome::Escalate => self.escalate += 1,
        }
    }

    /// Mode, ties broken by enum declaration order (Accept first) for
    /// determinism. NOTE: deliberately NOT `Iterator::max_by_key` — on a
    /// tie, `max_by_key` returns the LAST maximal element, which is the
    /// opposite of the declaration-order tie-break this needs. A manual
    /// first-wins fold is used instead.
    fn mode(&self) -> ExpectedOutcome {
        let candidates = [
            (ExpectedOutcome::Accept, self.accept),
            (ExpectedOutcome::Reject, self.reject),
            (ExpectedOutcome::Blocked, self.blocked),
            (ExpectedOutcome::Escalate, self.escalate),
        ];
        let mut best = candidates[0];
        for candidate in candidates.into_iter().skip(1) {
            if candidate.1 > best.1 {
                best = candidate;
            }
        }
        best.0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ArtifactCounts {
    pub text_only: u32,
    pub file_write: u32,
    pub structured_json: u32,
    pub external_effect: u32,
}

impl ArtifactCounts {
    fn bump(&mut self, artifact: ArtifactShape) {
        match artifact {
            ArtifactShape::TextOnly => self.text_only += 1,
            ArtifactShape::FileWrite => self.file_write += 1,
            ArtifactShape::StructuredJson => self.structured_json += 1,
            ArtifactShape::ExternalEffect => self.external_effect += 1,
        }
    }

    /// Same first-wins tie-break rationale as `OutcomeCounts::mode` above.
    fn mode(&self) -> ArtifactShape {
        let candidates = [
            (ArtifactShape::TextOnly, self.text_only),
            (ArtifactShape::FileWrite, self.file_write),
            (ArtifactShape::StructuredJson, self.structured_json),
            (ArtifactShape::ExternalEffect, self.external_effect),
        ];
        let mut best = candidates[0];
        for candidate in candidates.into_iter().skip(1) {
            if candidate.1 > best.1 {
                best = candidate;
            }
        }
        best.0
    }
}

/// Nearest-rank percentile over a sorted slice. `p` in `[0.0, 1.0]`. Empty
/// slice ⇒ `0`.
fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx]
}

/// Persisted statistical bucket for one `TaskStateKey`. Entirely
/// recomputable from `task_prediction_log` (design §5.1) — this is a cache,
/// not a source of truth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskStateModel {
    pub tool_class_counts: ToolClassCounts,
    pub call_counts: VecDeque<u32>,
    pub outcome_counts: OutcomeCounts,
    pub artifact_counts: ArtifactCounts,
    pub n_samples: u32,
}

impl TaskStateModel {
    /// Fold one settled, stats-eligible observation into the bucket.
    fn record(&mut self, error: &TaskPredictionError) {
        for class in &error.observation.observed_tool_classes {
            self.tool_class_counts.bump(*class);
        }
        self.call_counts.push_back(error.observation.observed_calls);
        while self.call_counts.len() > CALL_COUNT_HISTORY_CAP {
            self.call_counts.pop_front();
        }
        // Fold outcome/artifact counters from what was actually observed
        // (not what was predicted) — the bucket tracks reality.
        if let Some(outcome) = observed_outcome_as_expected(error.observation.observed_outcome) {
            self.outcome_counts.bump(outcome);
        }
        self.artifact_counts.bump(error.observation.observed_artifact);
        self.n_samples += 1;
    }

    fn call_band(&self) -> (u32, u32) {
        let mut sorted: Vec<u32> = self.call_counts.iter().copied().collect();
        sorted.sort_unstable();
        let lo = percentile(&sorted, 0.25);
        let hi = percentile(&sorted, 0.75);
        (lo, hi.max(lo))
    }
}

/// Maps an `ObservedOutcome` onto the `ExpectedOutcome` vocabulary for
/// bucket bookkeeping. `Unknown` observations don't move the outcome
/// counters (there's nothing to attribute).
fn observed_outcome_as_expected(observed: ObservedOutcome) -> Option<ExpectedOutcome> {
    match observed {
        ObservedOutcome::Accepted => Some(ExpectedOutcome::Accept),
        ObservedOutcome::Rejected => Some(ExpectedOutcome::Reject),
        ObservedOutcome::Blocked => Some(ExpectedOutcome::Blocked),
        ObservedOutcome::Escalated => Some(ExpectedOutcome::Escalate),
        ObservedOutcome::Unknown => None,
    }
}

/// Built-in prior table by `GoalKind`, consulted when both the canonical
/// and marginal buckets are cold (design §2.3 stage 3).
fn prior_for(goal_kind: GoalKind) -> (BTreeSet<ToolClass>, (u32, u32), ExpectedOutcome, ArtifactShape) {
    use ToolClass::*;
    match goal_kind {
        GoalKind::CodingSimple => (
            BTreeSet::from([Read, Write]),
            (1, 5),
            ExpectedOutcome::Accept,
            ArtifactShape::FileWrite,
        ),
        GoalKind::CodingComplex => (
            BTreeSet::from([Read, Write, Exec, Search]),
            (5, 20),
            ExpectedOutcome::Accept,
            ArtifactShape::FileWrite,
        ),
        GoalKind::ResearchOrQa => (
            BTreeSet::from([Read, Search]),
            (1, 10),
            ExpectedOutcome::Accept,
            ArtifactShape::TextOnly,
        ),
        GoalKind::PlanningOrDoc => (
            BTreeSet::from([Read, Write]),
            (1, 8),
            ExpectedOutcome::Accept,
            ArtifactShape::TextOnly,
        ),
        GoalKind::OpsOrExternal => (
            BTreeSet::from([Net, Exec]),
            (1, 10),
            ExpectedOutcome::Accept,
            ArtifactShape::ExternalEffect,
        ),
        GoalKind::Unknown => (BTreeSet::new(), (0, 5), ExpectedOutcome::Accept, ArtifactShape::TextOnly),
    }
}

/// Build a `TaskPrediction` from a resolved bucket + source, at `confidence
/// = min(1.0, n_samples / MATURE_N)`.
fn prediction_from_bucket(
    task_id: &str,
    agent_id: &str,
    round: u32,
    state_key: TaskStateKey,
    model: &TaskStateModel,
    source: PredictionSource,
) -> TaskPrediction {
    TaskPrediction {
        prediction_id: uuid::Uuid::new_v4().to_string(),
        task_id: task_id.to_string(),
        agent_id: agent_id.to_string(),
        round,
        expected_tool_classes: model.tool_class_counts.frequent(model.n_samples),
        expected_call_band: model.call_band(),
        expected_outcome: model.outcome_counts.mode(),
        expected_artifact: model.artifact_counts.mode(),
        confidence: (model.n_samples as f64 / MATURE_N as f64).min(1.0),
        source,
        created_at: Utc::now(),
        state_key,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// §5 — storage: `TaskForwardModel`
// ═══════════════════════════════════════════════════════════════════════

/// Idempotent table creation. Called from `PredictionEngine::init_tables`
/// (same `prediction.db` connection/file — design §5.1: "same file, two new
/// tables, `prediction_log` untouched") and directly by this module's own
/// tests (which open a throwaway temp DB and never construct a
/// `PredictionEngine`).
pub(crate) fn init_task_tables(conn: &Connection) -> Result<(), String> {
    // WP-P2 (DESIGN-lwm-calibration-2026-08-10.md §4): best-effort column
    // migration for pre-calibration DBs. Run BEFORE the `CREATE TABLE IF
    // NOT EXISTS` below (same ordering as `trust_store.rs::ensure_schema`)
    // — a fresh DB has no `task_prediction_log` table yet so this is a
    // harmless no-op error, then the CREATE TABLE below brings the column
    // in directly; an existing pre-calibration DB gets the column added
    // here, and the CREATE TABLE below is then itself a no-op. Either way
    // idempotent — "duplicate column" errors are deliberately swallowed.
    let _ = conn.execute("ALTER TABLE task_prediction_log ADD COLUMN brier_score REAL", []);
    // 2026-08-14: per-dimension error breakdown — previously the four
    // sub-errors were computed at settle and immediately discarded, so
    // "which dimension did the prediction miss" was unanswerable.
    let _ = conn.execute("ALTER TABLE task_prediction_log ADD COLUMN error_json TEXT", []);

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_prediction_log (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            prediction_id   TEXT NOT NULL UNIQUE,
            task_id         TEXT NOT NULL,
            agent_id        TEXT NOT NULL,
            round           INTEGER NOT NULL,
            state_key       TEXT NOT NULL,
            runtime         TEXT NOT NULL DEFAULT '',
            prediction_json TEXT NOT NULL,
            prediction_source TEXT NOT NULL,
            observation_json TEXT,
            fidelity        TEXT,
            composite_error REAL,
            category        TEXT,
            created_at      TEXT NOT NULL,
            settled_at      TEXT,
            brier_score     REAL,
            error_json      TEXT
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_tpl_task_round
            ON task_prediction_log(task_id, round);
        CREATE INDEX IF NOT EXISTS idx_tpl_state
            ON task_prediction_log(agent_id, state_key, settled_at);

        CREATE TABLE IF NOT EXISTS task_state_models (
            state_key    TEXT PRIMARY KEY,
            agent_id     TEXT NOT NULL,
            model_json   TEXT NOT NULL,
            n_samples    INTEGER NOT NULL DEFAULT 0,
            last_updated TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_tsm_agent ON task_state_models(agent_id);",
    )
    .map_err(|e| e.to_string())
}

/// Task-layer forward model: in-memory statistical buckets backed by
/// `task_state_models`, plus the `task_prediction_log` append/settle audit
/// trail. Structurally mirrors `PredictionEngine`'s `user_models` cache and
/// `load_all_models` / `save_model` / `flush_all` three-part persistence
/// pattern (design §5.1) — including the "shutdown must await pending
/// writes" discipline documented on `flush_all` below.
pub struct TaskForwardModel {
    models: Arc<Mutex<HashMap<String, TaskStateModel>>>,
    db_path: PathBuf,
    /// WP-A4: which `TASK_RULE_SOURCE_EVENT` rule ids were injected into a
    /// given `(task_id, round)`'s dispatch prompt (`goal_loop.rs`'s
    /// injection step) — consumed exactly once by `dispatch_engine.rs`'s
    /// settle step so `rule_lifecycle::settle_injected_rules` can credit or
    /// blame them. In-memory only, by design (task instruction: "隨
    /// goal_state_json 或記憶體 map,選成本低者" — this Arc is already
    /// shared between the `GoalLoopDriver` predict/inject hook and the
    /// `DispatchEngine` settle hook, same as `models` above, so no new
    /// cross-struct wiring is needed). Safe to lose on restart: worst case
    /// is a few in-flight rounds' rule credit is skipped, never
    /// mis-attributed to the wrong round.
    injected_task_rules: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

/// Defensive cap on `injected_task_rules` — a settle should always consume
/// its entry, but a task that never reaches a terminal review (crashed
/// runtime, orphaned row) must not let this map grow unboundedly across a
/// long-lived gateway process.
const INJECTED_TASK_RULES_CAP: usize = 1000;

impl TaskForwardModel {
    /// Open (or create) `db_path`, ensure the two tables exist, and preload
    /// all persisted buckets into memory.
    pub fn new(db_path: PathBuf) -> Self {
        if let Ok(conn) = Connection::open(&db_path) {
            if let Err(e) = init_task_tables(&conn) {
                warn!("Failed to init task_forward tables: {e}");
            }
        }
        let engine = Self {
            models: Arc::new(Mutex::new(HashMap::new())),
            db_path,
            injected_task_rules: Arc::new(Mutex::new(HashMap::new())),
        };
        if let Err(e) = engine.load_all_models() {
            warn!("Failed to load task state models from disk: {e}");
        }
        engine
    }

    /// WP-A4 injection-side bookkeeping: record which task-rule ids
    /// `goal_loop.rs` injected into this round's dispatch prompt. No-op for
    /// an empty list (nothing to settle later).
    pub async fn record_injected_task_rules(&self, task_id: &str, round: u32, rule_ids: Vec<String>) {
        if rule_ids.is_empty() {
            return;
        }
        let key = format!("{task_id}#{round}");
        let mut map = self.injected_task_rules.lock().await;
        if map.len() >= INJECTED_TASK_RULES_CAP && !map.contains_key(&key) {
            if let Some(oldest) = map.keys().next().cloned() {
                map.remove(&oldest);
            }
        }
        map.insert(key, rule_ids);
    }

    /// WP-A4 settle-side bookkeeping: consume (remove) the rule ids recorded
    /// for `(task_id, round)`. Empty when nothing was recorded — rule
    /// injection was disabled, no active rules existed to inject, or this
    /// round predates the feature. Degrades to "nothing to settle", never a
    /// guess.
    pub async fn take_injected_task_rules(&self, task_id: &str, round: u32) -> Vec<String> {
        let key = format!("{task_id}#{round}");
        self.injected_task_rules.lock().await.remove(&key).unwrap_or_default()
    }

    fn load_all_models(&self) -> Result<(), String> {
        let conn = Connection::open(&self.db_path).map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT state_key, model_json FROM task_state_models")
            .map_err(|e| e.to_string())?;
        let mut models = HashMap::new();
        let rows = stmt
            .query_map([], |row| {
                let key: String = row.get(0)?;
                let json: String = row.get(1)?;
                Ok((key, json))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            if let Ok((key, json)) = row {
                if let Ok(model) = serde_json::from_str::<TaskStateModel>(&json) {
                    models.insert(key, model);
                }
            }
        }
        let count = models.len();
        if let Ok(mut guard) = self.models.try_lock() {
            *guard = models;
        } else {
            warn!("Could not acquire task state models lock at startup — not preloaded");
        }
        if count > 0 {
            tracing::info!(count, "Loaded task state models from disk");
        }
        Ok(())
    }

    /// The A3 four-stage prediction cascade (design §2.3). Zero LLM cost —
    /// stage 4 (`ColdStartLlm`) is reserved (T3 default OFF) and not
    /// produced by this version; a fully cold bucket falls to `Prior`.
    pub async fn predict(
        &self,
        task_id: &str,
        agent_id: &str,
        round: u32,
        state_key: TaskStateKey,
    ) -> TaskPrediction {
        let canonical = state_key.canonical();
        let marginal = state_key.marginal();
        let models = self.models.lock().await;

        if let Some(model) = models.get(&canonical) {
            if model.n_samples >= MIN_SAMPLES {
                return prediction_from_bucket(
                    task_id,
                    agent_id,
                    round,
                    state_key,
                    model,
                    PredictionSource::Statistical,
                );
            }
        }
        if let Some(model) = models.get(&marginal) {
            if model.n_samples >= MIN_SAMPLES {
                return prediction_from_bucket(
                    task_id,
                    agent_id,
                    round,
                    state_key,
                    model,
                    PredictionSource::Marginal,
                );
            }
        }
        drop(models);

        let (classes, band, outcome, artifact) = prior_for(state_key.goal_kind);
        TaskPrediction {
            prediction_id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            round,
            expected_tool_classes: classes,
            expected_call_band: band,
            expected_outcome: outcome,
            expected_artifact: artifact,
            confidence: 0.0,
            source: PredictionSource::Prior,
            created_at: Utc::now(),
            state_key,
        }
    }

    /// WP-A9 settle-side lookup: read back the prediction logged for
    /// `(task_id, round)` (design §4.2's "在迴圈開頭取出 `Option<TaskPrediction>`").
    /// `None` when nothing was ever logged for this round — the predict
    /// hook was disabled at dispatch time, the round predates this feature,
    /// or `home_dir`/DB access failed. Never panics; a read/parse failure
    /// degrades to `None` exactly like "no prediction was made" (opus
    /// playbook §5: no evidence ⇒ no guess).
    pub async fn get_prediction(&self, task_id: &str, round: u32) -> Option<TaskPrediction> {
        let db_path = self.db_path.clone();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).ok()?;
            let json: String = conn
                .query_row(
                    "SELECT prediction_json FROM task_prediction_log \
                     WHERE task_id = ?1 AND round = ?2",
                    params![task_id, round],
                    |row| row.get(0),
                )
                .ok()?;
            serde_json::from_str(&json).ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Frozen / slow-moving **climatology** estimate of this agent's
    /// high-risk base rate: the fraction of its settled predictions whose
    /// `category` is Significant/Critical, over ALL of the agent's settled
    /// history in `task_prediction_log`.
    ///
    /// Used as the frozen baseline the held-out shadow→promotion gate must
    /// beat (DESIGN-lwm-calibration-2026-08-10.md §1.3: the RPSS / gate
    /// baseline must be frozen or slow-moving, else the denominator drifts and
    /// the skill claim is unfalsifiable). This is a **climatology
    /// approximation, not an exact frozen prior** — an all-history proportion
    /// moves negligibly per new sample once N is large, which is the honest
    /// "persistent count" fallback the design permits ("若無現成凍結基準,用
    /// 簡單持久化累積比例,並在 doc 註明其為 climatology 近似").
    ///
    /// Returns `None` when fewer than `min_samples` settled rows exist (too
    /// little history to trust a climatology; the caller falls back to the
    /// domain-agnostic 0.5 coin-flip default). Never panics: a DB/read failure
    /// degrades to `None`, same posture as [`Self::get_prediction`].
    pub async fn high_risk_base_rate(&self, agent_id: &str, min_samples: u64) -> Option<f64> {
        let db_path = self.db_path.clone();
        let agent = agent_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).ok()?;
            let (hi, total): (i64, i64) = conn
                .query_row(
                    "SELECT
                       SUM(CASE WHEN category IN ('significant','critical') THEN 1 ELSE 0 END),
                       COUNT(*)
                     FROM task_prediction_log
                     WHERE agent_id = ?1 AND settled_at IS NOT NULL AND category IS NOT NULL",
                    params![agent],
                    |row| {
                        // SUM(...) is NULL when zero rows match — coalesce to 0.
                        let hi = row.get::<_, Option<i64>>(0)?.unwrap_or(0);
                        let total: i64 = row.get(1)?;
                        Ok((hi, total))
                    },
                )
                .ok()?;
            if total <= 0 || (total as u64) < min_samples {
                return None;
            }
            Some(hi as f64 / total as f64)
        })
        .await
        .ok()
        .flatten()
    }

    /// Append an unsettled prediction row (`observation_json = NULL`).
    /// Idempotent on `(task_id, round)` via `INSERT OR REPLACE` semantics
    /// through the unique index — a re-dispatch of the same round
    /// overwrites, never duplicates.
    pub async fn log_prediction(&self, prediction: &TaskPrediction) -> Result<(), String> {
        let db_path = self.db_path.clone();
        let prediction_json = serde_json::to_string(prediction).map_err(|e| e.to_string())?;
        let prediction_id = prediction.prediction_id.clone();
        let task_id = prediction.task_id.clone();
        let agent_id = prediction.agent_id.clone();
        let round = prediction.round;
        let state_key = prediction.state_key.canonical();
        let source = prediction.source.as_str().to_string();
        let created_at = prediction.created_at.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO task_prediction_log
                 (prediction_id, task_id, agent_id, round, state_key, prediction_json,
                  prediction_source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    prediction_id, task_id, agent_id, round, state_key, prediction_json,
                    source, created_at
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Settle a prediction with its observed error: writes the
    /// `observation_json`/`fidelity`/`composite_error`/`category` columns
    /// and, when `error.eligible_for_stats`, folds the observation into the
    /// in-memory bucket and persists it. `fidelity == None` never reaches
    /// here — `task_forward::diff` never produces a `TaskPredictionError`
    /// for it.
    ///
    /// `calibration_enabled` (WP-P2, design §4/§7 platform section) gates
    /// the `brier_score` column: `true` scores
    /// `error.prediction.confidence` via `calibration::brier_binary` against
    /// `task_forward::task_prediction_correct(error)` (`Negligible` /
    /// `Moderate` category ⇒ correct) and persists it; `false` leaves the
    /// column untouched (`NULL`), matching pre-calibration behavior exactly.
    pub async fn settle_prediction(
        &self,
        error: &TaskPredictionError,
        calibration_enabled: bool,
    ) -> Result<(), String> {
        let db_path = self.db_path.clone();
        let prediction_id = error.prediction.prediction_id.clone();
        let observation_json = serde_json::to_string(&error.observation).map_err(|e| e.to_string())?;
        let fidelity = error.fidelity.as_str().to_string();
        let composite_error = error.composite_error;
        let category = format!("{:?}", error.category).to_lowercase();
        let settled_at = Utc::now().to_rfc3339();
        let brier_score: Option<f64> = if calibration_enabled {
            Some(super::task_forward::calibration_brier_score(error))
        } else {
            None
        };
        // Per-dimension breakdown, previously computed-then-discarded.
        let error_json = serde_json::json!({
            "tool_set_error": error.tool_set_error,
            "volume_error": error.volume_error,
            "outcome_error": error.outcome_error,
            "outcome_error_applicable": error.outcome_error_applicable,
            "artifact_error": error.artifact_error,
            "eligible_for_stats": error.eligible_for_stats,
        })
        .to_string();

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open(&db_path).map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE task_prediction_log
                 SET observation_json = ?1, fidelity = ?2, composite_error = ?3,
                     category = ?4, settled_at = ?5, brier_score = ?6, error_json = ?8
                 WHERE prediction_id = ?7",
                params![
                    observation_json, fidelity, composite_error, category, settled_at,
                    brier_score, prediction_id, error_json
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
        .await
        .map_err(|e| e.to_string())??;

        if error.eligible_for_stats {
            // Fold into BOTH the canonical bucket AND the marginal bucket
            // (design §2.3 stage 2). The marginal key aggregates across
            // every phase/outcome_spec combination for this
            // (agent, goal_kind) — it must be a real, independently
            // maintained bucket, not merely a fallback *lookup* of the
            // canonical map under a different key (which would never have
            // an entry, since nothing is ever settled under the literal
            // marginal string otherwise).
            let canonical_key = error.prediction.state_key.canonical();
            let marginal_key = error.prediction.state_key.marginal();
            let agent_id = error.prediction.agent_id.clone();

            let mut models = self.models.lock().await;
            let canonical_model = models.entry(canonical_key.clone()).or_default();
            canonical_model.record(error);
            let canonical_clone = canonical_model.clone();

            let marginal_model = models.entry(marginal_key.clone()).or_default();
            marginal_model.record(error);
            let marginal_clone = marginal_model.clone();
            drop(models);

            if let Some(handle) = self.save_model(&canonical_key, &agent_id, &canonical_clone).await {
                let _ = handle.await;
            }
            if let Some(handle) = self.save_model(&marginal_key, &agent_id, &marginal_clone).await {
                let _ = handle.await;
            }
        }
        Ok(())
    }

    #[must_use = "shutdown paths must await the write; hot paths should `let _ =` it explicitly"]
    async fn save_model(
        &self,
        state_key: &str,
        agent_id: &str,
        model: &TaskStateModel,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let db_path = self.db_path.clone();
        let key = state_key.to_string();
        let agent = agent_id.to_string();
        let json = serde_json::to_string(model).ok()?;
        let n_samples = model.n_samples;
        Some(tokio::task::spawn_blocking(move || {
            if let Ok(conn) = Connection::open(&db_path) {
                let _ = conn.execute(
                    "INSERT OR REPLACE INTO task_state_models
                     (state_key, agent_id, model_json, n_samples, last_updated)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, agent, json, n_samples, Utc::now().to_rfc3339()],
                );
            }
        }))
    }

    /// Flush every in-memory bucket to disk (call on shutdown). Awaits
    /// every spawned write — see `PredictionEngine::flush_all`'s doc
    /// comment (`engine.rs`) for why a detached write must never be allowed
    /// to vanish across a self-restart `exec()`.
    pub async fn flush_all(&self) {
        let models = self.models.lock().await;
        let snapshot: Vec<(String, TaskStateModel)> =
            models.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(models);

        let mut handles = Vec::with_capacity(snapshot.len());
        for (state_key, model) in snapshot {
            // agent_id isn't tracked per-bucket in the in-memory map key;
            // it's embedded in state_key's canonical form
            // (`"<agent>|..."`), so extract it back out for the column.
            let agent_id = state_key.split('|').next().unwrap_or("").to_string();
            if let Some(handle) = self.save_model(&state_key, &agent_id, &model).await {
                handles.push(handle);
            }
        }
        for handle in handles {
            if tokio::time::timeout(std::time::Duration::from_secs(3), handle)
                .await
                .is_err()
            {
                warn!("task state model write did not finish within 3s — continuing shutdown");
            }
        }
        tracing::info!("Flushed all task state models to disk");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::task_forward::{
        DiffOutcome, ObservationFidelity, RoundPhase, TaskObservation, diff,
    };
    use crate::prediction::engine::ErrorCategory;
    use crate::prediction::metacognition::AdaptiveThresholds;

    fn key(agent: &str, kind: GoalKind, phase: RoundPhase, has_spec: bool) -> TaskStateKey {
        TaskStateKey {
            agent_id: agent.to_string(),
            goal_kind: kind,
            phase,
            has_outcome_spec: has_spec,
        }
    }

    /// Duplicated (small, intentional) from `task_forward::tests` — keeping
    /// this test module independent of the sibling file's private test
    /// helpers rather than reaching across `#[cfg(test)]` boundaries.
    fn base_observation(fidelity: ObservationFidelity) -> TaskObservation {
        TaskObservation {
            task_id: "t1".into(),
            agent_id: "agnes".into(),
            round: 1,
            observed_tool_classes: BTreeSet::from([ToolClass::Read, ToolClass::Write]),
            observed_calls: 3,
            observed_errors: 0,
            observed_outcome: ObservedOutcome::Accepted,
            observed_artifact: ArtifactShape::FileWrite,
            fidelity,
            window: ("2026-08-06T00:00:00Z".into(), "2026-08-06T00:10:00Z".into()),
            runtime: "claude".into(),
        }
    }

    // ── ToolClassCounts / OutcomeCounts / ArtifactCounts ──

    #[test]
    fn tool_class_counts_frequent_threshold() {
        let mut c = ToolClassCounts::default();
        c.bump(ToolClass::Read);
        c.bump(ToolClass::Read);
        c.bump(ToolClass::Write);
        // n_samples = 4 rounds recorded (not bump count) — simulate 4
        // rounds where Read appeared in 2 (0.5) and Write in 1 (0.25).
        let freq = c.frequent(4);
        assert!(freq.contains(&ToolClass::Read)); // 2/4 = 0.5 >= 0.5
        assert!(!freq.contains(&ToolClass::Write)); // 1/4 = 0.25 < 0.5
    }

    #[test]
    fn outcome_counts_mode_ties_favor_declaration_order() {
        let mut c = OutcomeCounts::default();
        c.bump(ExpectedOutcome::Reject);
        c.bump(ExpectedOutcome::Accept);
        // tie 1-1 -> Accept (declared first) wins deterministically
        assert_eq!(c.mode(), ExpectedOutcome::Accept);
    }

    #[test]
    fn artifact_counts_mode_picks_majority() {
        let mut c = ArtifactCounts::default();
        c.bump(ArtifactShape::FileWrite);
        c.bump(ArtifactShape::FileWrite);
        c.bump(ArtifactShape::TextOnly);
        assert_eq!(c.mode(), ArtifactShape::FileWrite);
    }

    // ── percentile ──

    #[test]
    fn percentile_empty_is_zero() {
        assert_eq!(percentile(&[], 0.25), 0);
    }

    #[test]
    fn percentile_single_value() {
        assert_eq!(percentile(&[7], 0.25), 7);
        assert_eq!(percentile(&[7], 0.75), 7);
    }

    #[test]
    fn percentile_known_distribution() {
        let sorted = vec![1, 2, 3, 4, 5, 6, 7, 8];
        // p25 nearest-rank: ceil(0.25*8)=2 -> idx1 -> value 2
        assert_eq!(percentile(&sorted, 0.25), 2);
        // p75: ceil(0.75*8)=6 -> idx5 -> value 6
        assert_eq!(percentile(&sorted, 0.75), 6);
    }

    // ── TaskForwardModel: predict() cascade + persistence ──

    async fn temp_model() -> (TaskForwardModel, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let model = TaskForwardModel::new(db_path);
        (model, dir)
    }

    #[tokio::test]
    async fn predict_cold_start_is_prior_with_zero_confidence() {
        let (model, _dir) = temp_model().await;
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = model.predict("t1", "agnes", 1, sk).await;
        assert_eq!(pred.source, PredictionSource::Prior);
        assert_eq!(pred.confidence, 0.0);
        assert!(pred.expected_tool_classes.contains(&ToolClass::Read));
    }

    #[tokio::test]
    async fn predict_promotes_to_statistical_after_min_samples() {
        let (model, _dir) = temp_model().await;
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let thresholds = AdaptiveThresholds::default();

        for round in 1..=MIN_SAMPLES {
            let pred = model.predict("t1", "agnes", round, sk.clone()).await;
            model.log_prediction(&pred).await.unwrap();
            let obs = base_observation(ObservationFidelity::Full);
            if let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) {
                model.settle_prediction(&err, false).await.unwrap();
            } else {
                panic!("should compute");
            }
        }

        let pred = model.predict("t2", "agnes", MIN_SAMPLES + 1, sk).await;
        assert_eq!(pred.source, PredictionSource::Statistical);
        assert!(pred.confidence > 0.0);
        assert!(pred.expected_tool_classes.contains(&ToolClass::Read));
        assert!(pred.expected_tool_classes.contains(&ToolClass::Write));
    }

    #[tokio::test]
    async fn predict_marginal_fallback_when_canonical_cold() {
        let (model, _dir) = temp_model().await;
        let thresholds = AdaptiveThresholds::default();
        // Feed the FIRST-phase bucket to maturity.
        let first_key = key("agnes", GoalKind::OpsOrExternal, RoundPhase::First, false);
        for round in 1..=MIN_SAMPLES {
            let pred = model.predict("t1", "agnes", round, first_key.clone()).await;
            model.log_prediction(&pred).await.unwrap();
            let obs = base_observation(ObservationFidelity::Full);
            if let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) {
                model.settle_prediction(&err, false).await.unwrap();
            }
        }
        // Query a RETRY-phase key for the same agent/goal_kind — canonical
        // bucket is cold (0 samples), but marginal (agent, goal_kind)
        // matches the FIRST bucket's marginal key.
        let retry_key = key("agnes", GoalKind::OpsOrExternal, RoundPhase::Retry, false);
        let pred = model.predict("t9", "agnes", 99, retry_key).await;
        assert_eq!(pred.source, PredictionSource::Marginal);
    }

    #[tokio::test]
    async fn settle_prediction_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let thresholds = AdaptiveThresholds::default();
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        {
            let model = TaskForwardModel::new(db_path.clone());
            for round in 1..=MIN_SAMPLES {
                let pred = model.predict("t1", "agnes", round, sk.clone()).await;
                model.log_prediction(&pred).await.unwrap();
                let obs = base_observation(ObservationFidelity::Full);
                if let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) {
                    model.settle_prediction(&err, false).await.unwrap();
                }
            }
            model.flush_all().await;
        }
        // Reopen — bucket must have survived.
        let reopened = TaskForwardModel::new(db_path);
        let pred = reopened.predict("t2", "agnes", 99, sk).await;
        assert_eq!(pred.source, PredictionSource::Statistical);
    }

    #[tokio::test]
    async fn ineligible_settle_does_not_pollute_bucket() {
        let (model, _dir) = temp_model().await;
        let thresholds = AdaptiveThresholds::default();
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        for round in 1..=MIN_SAMPLES {
            let pred = model.predict("t1", "agnes", round, sk.clone()).await;
            model.log_prediction(&pred).await.unwrap();
            let mut obs = base_observation(ObservationFidelity::McpOnly);
            obs.observed_outcome = ObservedOutcome::Unknown; // ineligible
            if let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) {
                assert!(!err.eligible_for_stats);
                model.settle_prediction(&err, false).await.unwrap();
            }
        }
        // Bucket should still be cold — ineligible settles never fed it.
        let pred = model.predict("t2", "agnes", 99, sk).await;
        assert_eq!(pred.source, PredictionSource::Prior);
    }

    #[tokio::test]
    async fn log_prediction_is_idempotent_on_task_round() {
        let (model, _dir) = temp_model().await;
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred1 = model.predict("t1", "agnes", 1, sk.clone()).await;
        model.log_prediction(&pred1).await.unwrap();
        // A second dispatch of the SAME (task_id, round) must not violate
        // the unique index — this exercises the re-dispatch/idempotency
        // guarantee design §5.1 calls out. Give it a distinct
        // prediction_id (a real re-predict would produce one) but the same
        // (task_id, round); the second insert should be rejected by the
        // unique index rather than silently duplicating the row, matching
        // "re-dispatch of a stalled round" semantics — assert on that
        // explicitly rather than assuming.
        let mut pred2 = model.predict("t1", "agnes", 1, sk).await;
        pred2.prediction_id = uuid::Uuid::new_v4().to_string();
        let result = model.log_prediction(&pred2).await;
        assert!(result.is_err(), "duplicate (task_id, round) must be rejected by the unique index");
    }

    // ── WP-A9: get_prediction (settle-side lookup) ──

    #[tokio::test]
    async fn get_prediction_returns_logged_prediction() {
        let (model, _dir) = temp_model().await;
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = model.predict("t1", "agnes", 1, sk).await;
        model.log_prediction(&pred).await.unwrap();

        let got = model.get_prediction("t1", 1).await;
        assert!(got.is_some());
        assert_eq!(got.unwrap().prediction_id, pred.prediction_id);
    }

    #[tokio::test]
    async fn get_prediction_none_when_never_logged() {
        let (model, _dir) = temp_model().await;
        assert!(model.get_prediction("no-such-task", 1).await.is_none());
    }

    // ── WP-A9: TaskForwardModelConfig ──

    #[test]
    fn task_forward_model_config_defaults_to_enabled() {
        // v1.54: the forward model is a default-ON platform capability.
        let dir = tempfile::tempdir().unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.enabled, "no config.toml ⇒ enabled defaults to true (v1.54)");
        assert_eq!(cfg, TaskForwardModelConfig::default());
    }

    #[test]
    fn task_forward_model_config_malformed_section_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nenabled = \"not-a-bool\"\n",
        )
        .unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert_eq!(cfg, TaskForwardModelConfig::default());
    }

    #[test]
    fn task_forward_model_config_parses_enabled_and_leaves_rest_default() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nenabled = true\nmin_samples = 7\n",
        )
        .unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.enabled);
        assert_eq!(cfg.min_samples, 7);
        assert_eq!(cfg.mature_n, MATURE_N, "unset field keeps the struct default");
        assert!(cfg.rule_induction, "WP-A4 sub-switch defaults true (opt-out, not opt-in)");
    }

    #[test]
    fn task_forward_model_config_missing_section_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[other_section]\nx = 1\n").unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert_eq!(cfg, TaskForwardModelConfig::default());
    }

    // ── WP-A4: rule_induction sub-switch ──

    #[test]
    fn task_forward_model_config_rule_induction_can_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nenabled = true\nrule_induction = false\n",
        )
        .unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.enabled);
        assert!(!cfg.rule_induction);
    }

    // ── WP-P2 / v1.54: calibration_enabled + held_out_gate_enabled default ON ──

    #[test]
    fn task_forward_model_config_calibration_and_held_out_default_on() {
        // v1.54: the whole capability is default ON — master switch plus both
        // learning gates — so predict-act-verify, proper-scoring, and the
        // held-out rule gate run for every agent out of the box.
        let d = TaskForwardModelConfig::default();
        assert!(d.calibration_enabled, "v1.54: calibration_enabled defaults ON");
        assert!(d.held_out_gate_enabled, "v1.54: held_out_gate_enabled defaults ON");
        assert!(d.enabled, "v1.54: the master switch defaults ON too");

        // A config.toml with the section present but these keys unset inherits
        // the ON default (serde default) — absent == ON, per the v1.54 design.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nenabled = true\n",
        )
        .unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.calibration_enabled, "unset calibration key inherits the ON default");
        assert!(cfg.held_out_gate_enabled, "unset held-out key inherits the ON default");
    }

    #[test]
    fn task_forward_model_config_calibration_and_held_out_can_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[task_forward_model]\nenabled = true\ncalibration_enabled = false\n\
             held_out_gate_enabled = false\n",
        )
        .unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.enabled);
        assert!(!cfg.calibration_enabled, "explicit off is honored");
        assert!(!cfg.held_out_gate_enabled, "explicit off is honored");
    }

    #[test]
    fn task_forward_model_absent_config_file_defaults_on() {
        // No config.toml at all ⇒ Self::default() ⇒ whole capability ON (v1.54):
        // master switch and both learning gates. "absent == ON".
        let dir = tempfile::tempdir().unwrap();
        let cfg = TaskForwardModelConfig::from_home(dir.path());
        assert!(cfg.calibration_enabled);
        assert!(cfg.held_out_gate_enabled);
        assert!(cfg.enabled);
    }

    // ── WP-P2: settle_prediction brier_score persistence ──

    /// Read back the `brier_score` column directly — there's no public
    /// getter (the column is an internal scoring artifact, not part of
    /// `TaskForwardModel`'s public surface), so tests reach into the DB the
    /// same way `settle_prediction_persists_across_reload` above reaches in
    /// via `TaskForwardModel::new` reopening — a raw read is the simplest
    /// honest check here.
    fn read_brier_score(db_path: &std::path::Path, prediction_id: &str) -> Option<f64> {
        let conn = Connection::open(db_path).unwrap();
        conn.query_row(
            "SELECT brier_score FROM task_prediction_log WHERE prediction_id = ?1",
            params![prediction_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn settle_prediction_calibration_disabled_leaves_brier_score_null() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let model = TaskForwardModel::new(db_path.clone());
        let thresholds = AdaptiveThresholds::default();
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = model.predict("t1", "agnes", 1, sk).await;
        model.log_prediction(&pred).await.unwrap();
        let prediction_id = pred.prediction_id.clone();
        let obs = base_observation(ObservationFidelity::Full);
        let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) else {
            panic!("should compute");
        };
        model.settle_prediction(&err, false).await.unwrap();
        assert_eq!(read_brier_score(&db_path, &prediction_id), None);
    }

    #[tokio::test]
    async fn settle_prediction_calibration_enabled_writes_brier_score() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("prediction.db");
        let model = TaskForwardModel::new(db_path.clone());
        let thresholds = AdaptiveThresholds::default();
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = model.predict("t1", "agnes", 1, sk).await;
        // Cold-start prior prediction always has confidence 0.0; force a
        // non-trivial confidence so the Brier score isn't the degenerate
        // conf=0.0 case.
        let mut pred = pred;
        pred.confidence = 0.65;
        model.log_prediction(&pred).await.unwrap();
        let prediction_id = pred.prediction_id.clone();
        let obs = base_observation(ObservationFidelity::Full);
        let DiffOutcome::Computed(err) = diff(pred, obs, &thresholds) else {
            panic!("should compute");
        };
        assert_eq!(err.category, ErrorCategory::Negligible, "perfect-match fixture stays Negligible");
        model.settle_prediction(&err, true).await.unwrap();
        let expected = super::super::calibration::brier_binary(0.65, true);
        assert_eq!(read_brier_score(&db_path, &prediction_id), Some(expected));
    }

    // ── WP-A4: injected_task_rules bookkeeping round-trip ──

    #[tokio::test]
    async fn injected_task_rules_round_trips_and_is_consumed_once() {
        let (model, _dir) = temp_model().await;
        model
            .record_injected_task_rules("t1", 1, vec!["r1".to_string(), "r2".to_string()])
            .await;

        let got = model.take_injected_task_rules("t1", 1).await;
        assert_eq!(got, vec!["r1".to_string(), "r2".to_string()]);

        // Consumed — a second take returns empty, not the stale value.
        let second = model.take_injected_task_rules("t1", 1).await;
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn injected_task_rules_empty_write_is_noop() {
        let (model, _dir) = temp_model().await;
        model.record_injected_task_rules("t1", 1, Vec::new()).await;
        assert!(model.take_injected_task_rules("t1", 1).await.is_empty());
    }

    #[tokio::test]
    async fn injected_task_rules_distinguishes_task_and_round() {
        let (model, _dir) = temp_model().await;
        model.record_injected_task_rules("t1", 1, vec!["r1".to_string()]).await;
        model.record_injected_task_rules("t1", 2, vec!["r2".to_string()]).await;
        model.record_injected_task_rules("t2", 1, vec!["r3".to_string()]).await;

        assert_eq!(model.take_injected_task_rules("t1", 1).await, vec!["r1".to_string()]);
        assert_eq!(model.take_injected_task_rules("t1", 2).await, vec!["r2".to_string()]);
        assert_eq!(model.take_injected_task_rules("t2", 1).await, vec!["r3".to_string()]);
    }

    #[tokio::test]
    async fn injected_task_rules_never_recorded_is_empty() {
        let (model, _dir) = temp_model().await;
        assert!(model.take_injected_task_rules("no-such-task", 1).await.is_empty());
    }
}
