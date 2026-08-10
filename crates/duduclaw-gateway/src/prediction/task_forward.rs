//! WP-A2/A8 — task-layer forward model: schema + the deterministic diff
//! algorithm.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §2, §3.
//! This module is deliberately **parallel to, not shared with**,
//! `prediction::engine::PredictionEngine` (design §4.3): the conversational
//! `Prediction` predicts user reaction; `TaskPrediction` predicts what a
//! goal-loop dispatch round will *do* (tool classes, call volume, outcome,
//! artifact shape).
//!
//! The statistical buckets + `TaskForwardModel` storage (WP-A7) live in the
//! sibling module [`super::task_forward_store`] — split out purely to keep
//! both files under the project's ~800-line file-size convention; the two
//! together form one cohesive unit (`task_forward_store` re-exports nothing
//! back here, it just depends on these types via `use super::task_forward`).
//!
//! **Not wired into any hot path in this change** (goal_loop.rs /
//! dispatch_engine.rs hooking is WP-A9, out of scope here). This module is
//! self-contained, offline-testable, and has zero production callers today
//! — exactly like `foresight_gate` (WP-B4) is meant to.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::calibration;
use super::engine::ErrorCategory;
use super::metacognition::AdaptiveThresholds;
use super::tool_class::ToolClass;

// ═══════════════════════════════════════════════════════════════════════
// §2.2 — Tool classes: state key
// ═══════════════════════════════════════════════════════════════════════

/// Coarse goal classification. **Deliberately coarse** — a finer split
/// would leave every bucket permanently cold-started (design §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    CodingSimple,
    CodingComplex,
    ResearchOrQa,
    PlanningOrDoc,
    OpsOrExternal,
    Unknown,
}

impl GoalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CodingSimple => "coding_simple",
            Self::CodingComplex => "coding_complex",
            Self::ResearchOrQa => "research_or_qa",
            Self::PlanningOrDoc => "planning_or_doc",
            Self::OpsOrExternal => "ops_or_external",
            Self::Unknown => "unknown",
        }
    }
}

/// Which phase of the dispatch loop this round is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundPhase {
    /// First dispatch.
    First,
    /// Re-dispatched after judge rejection (carries judge feedback).
    Retry,
    /// Re-dispatched because it stalled unclaimed.
    Restall,
}

impl RoundPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Retry => "retry",
            Self::Restall => "restall",
        }
    }
}

/// Statistical-bucket query key. Serializes to a stable string used as the
/// SQL index/primary key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskStateKey {
    pub agent_id: String,
    pub goal_kind: GoalKind,
    pub phase: RoundPhase,
    /// Whether this round carries a structured `outcome:<b64>` production
    /// contract (`outcome_spec.rs`).
    pub has_outcome_spec: bool,
}

impl TaskStateKey {
    /// `"<agent>|<goal_kind>|<phase>|<0|1>"` — stable, usable as a SQL key.
    pub fn canonical(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.agent_id,
            self.goal_kind.as_str(),
            self.phase.as_str(),
            if self.has_outcome_spec { 1 } else { 0 }
        )
    }

    /// One level coarser: drops `phase` and `has_outcome_spec`. Queried
    /// when the canonical bucket is cold (design §2.3, stage 2).
    pub fn marginal(&self) -> String {
        format!("{}|{}", self.agent_id, self.goal_kind.as_str())
    }
}

// ═══════════════════════════════════════════════════════════════════════
// §2.2 — Prediction
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedOutcome {
    Accept,
    Reject,
    Blocked,
    Escalate,
}

impl ExpectedOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::Blocked => "blocked",
            Self::Escalate => "escalate",
        }
    }
}

/// Expected artifact shape. Derived deterministically from `OutcomeSpec` +
/// goal text (derivation is out of this module's scope — WP-A9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactShape {
    TextOnly,
    FileWrite,
    StructuredJson,
    ExternalEffect,
}

impl ArtifactShape {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextOnly => "text_only",
            Self::FileWrite => "file_write",
            Self::StructuredJson => "structured_json",
            Self::ExternalEffect => "external_effect",
        }
    }
}

/// Where a `TaskPrediction` came from — logged for honest self-reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionSource {
    /// The exact `state_key` has enough settled history — pure statistics,
    /// zero LLM.
    Statistical,
    /// Canonical bucket too thin; fell back to the marginal bucket. Zero
    /// LLM.
    Marginal,
    /// Fully cold — built-in prior table by `GoalKind`. Zero LLM.
    Prior,
    /// Cold-start LLM query (T3: default OFF, not wired in this version —
    /// see module docs). Reserved so a future WP-A9 config toggle can
    /// produce this variant without a schema change.
    ColdStartLlm,
}

impl PredictionSource {
    /// Stable snake_case identifier — matches the design §5.1 SQL comment
    /// (`-- statistical|marginal|prior|cold_start_llm`). **Do not** use
    /// `format!("{:?}", source).to_lowercase()` for this: `Debug` on
    /// `ColdStartLlm` lowercases to `"coldstartllm"` (no underscore), which
    /// silently diverges from the documented column contract (WP-A9 found
    /// this via its settle-hook integration test).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Statistical => "statistical",
            Self::Marginal => "marginal",
            Self::Prior => "prior",
            Self::ColdStartLlm => "cold_start_llm",
        }
    }
}

/// Forward prediction for one goal-loop dispatch round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPrediction {
    pub prediction_id: String,
    pub task_id: String,
    pub agent_id: String,
    /// `task.revision_round + 1`.
    pub round: u32,
    pub state_key: TaskStateKey,

    pub expected_tool_classes: BTreeSet<ToolClass>,
    /// `[lo, hi]` — landing inside the band scores zero error.
    pub expected_call_band: (u32, u32),
    pub expected_outcome: ExpectedOutcome,
    pub expected_artifact: ArtifactShape,

    /// `0.0` = cold start, `1.0` = mature. `min(1.0, n_samples / MATURE_N)`.
    pub confidence: f64,
    pub source: PredictionSource,
    pub created_at: DateTime<Utc>,
}

// ═══════════════════════════════════════════════════════════════════════
// §2.2 — Observation
// ═══════════════════════════════════════════════════════════════════════

/// Observation fidelity — the honesty core of A3 (design §2.2, §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationFidelity {
    /// Native tools + MCP tools both visible, with success/failure.
    Full,
    /// Only MCP tools visible (`tool_calls.jsonl`). **The default/primary
    /// branch of the first shippable version** (design §8.2) — not an
    /// edge case.
    McpOnly,
    /// No tool evidence at all (missing home_dir / missing file / window
    /// parse failure).
    None,
}

impl ObservationFidelity {
    /// Stable snake_case identifier — matches the design §5.1 SQL comment
    /// (`-- full|mcp_only|none`). **Do not** use
    /// `format!("{:?}", fidelity).to_lowercase()` for this: `Debug` on
    /// `McpOnly` lowercases to `"mcponly"` (no underscore), which silently
    /// diverges from the documented column contract (WP-A9 found this via
    /// its settle-hook integration test — every settled row was being
    /// stamped `"mcponly"` instead of `"mcp_only"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::McpOnly => "mcp_only",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedOutcome {
    Accepted,
    Rejected,
    Blocked,
    Escalated,
    Unknown,
}

impl ObservedOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Blocked => "blocked",
            Self::Escalated => "escalated",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this observed outcome corresponds to `expected` — used by
    /// the diff algorithm (§3.1 `outcome_error`).
    fn matches(&self, expected: ExpectedOutcome) -> bool {
        matches!(
            (self, expected),
            (Self::Accepted, ExpectedOutcome::Accept)
                | (Self::Rejected, ExpectedOutcome::Reject)
                | (Self::Blocked, ExpectedOutcome::Blocked)
                | (Self::Escalated, ExpectedOutcome::Escalate)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskObservation {
    pub task_id: String,
    pub agent_id: String,
    pub round: u32,
    pub observed_tool_classes: BTreeSet<ToolClass>,
    pub observed_calls: u32,
    pub observed_errors: u32,
    pub observed_outcome: ObservedOutcome,
    pub observed_artifact: ArtifactShape,
    pub fidelity: ObservationFidelity,
    /// Observation window `[claimed_at, review_at]`, RFC3339.
    pub window: (String, String),
    /// Runtime name, logged for post-hoc stratification.
    pub runtime: String,
}

// ═══════════════════════════════════════════════════════════════════════
// §2.2 / §3 — Error
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPredictionError {
    /// `1 - Jaccard(expected_classes, observed_classes)`.
    pub tool_set_error: f64,
    /// Normalized distance of the observed call count outside the band.
    pub volume_error: f64,
    /// `expected_outcome == observed_outcome` ⇒ 0.0, else 1.0 (or omitted —
    /// see `outcome_error_applicable`).
    pub outcome_error: f64,
    /// Whether `outcome_error` was computable (`observed_outcome !=
    /// Unknown`). When `false`, `outcome_error` is `0.0` as a placeholder
    /// and was excluded from the weighted sum (weights renormalized over
    /// the remaining dimensions — design §3.3).
    pub outcome_error_applicable: bool,
    /// `0.0` match, `0.5` adjacent shape, `1.0` opposite.
    pub artifact_error: f64,
    pub composite_error: f64,
    pub category: ErrorCategory,
    pub fidelity: ObservationFidelity,
    /// Design §3.3: an `Unknown`-outcome round observed under `McpOnly`
    /// degrades to "log only, do not feed the statistical bucket / A4
    /// induction". `settle_prediction` consults this flag.
    pub eligible_for_stats: bool,
    pub prediction: TaskPrediction,
    pub observation: TaskObservation,
}

// ═══════════════════════════════════════════════════════════════════════
// WP-P2 — calibration scoring (design DESIGN-lwm-calibration-2026-08-10.md
// §2/§4 platform section)
// ═══════════════════════════════════════════════════════════════════════

/// Domain-agnostic "was this prediction correct" verdict for calibration
/// scoring: `Negligible`/`Moderate` composite-error categories count as
/// correct, `Significant`/`Critical` do not. This is the `(confidence,
/// realized_outcome)` abstraction the design mandates — it reuses the
/// existing dual-process-router category (§4 platform integration point),
/// not a new domain-specific correctness notion.
pub fn task_prediction_correct(error: &TaskPredictionError) -> bool {
    matches!(error.category, ErrorCategory::Negligible | ErrorCategory::Moderate)
}

/// Brier calibration score for one settled prediction: scores
/// `error.prediction.confidence` against [`task_prediction_correct`] via
/// [`calibration::brier_binary`]. Pure, zero I/O — callers (`settle_prediction`,
/// `transition::build_transition_write`) gate this behind `[task_forward_model]
/// calibration_enabled` (default `false`).
pub fn calibration_brier_score(error: &TaskPredictionError) -> f64 {
    calibration::brier_binary(error.prediction.confidence, task_prediction_correct(error))
}

// ═══════════════════════════════════════════════════════════════════════
// §3 — diff algorithm
// ═══════════════════════════════════════════════════════════════════════

/// fidelity → dimension weights `(w_tool, w_volume, w_outcome, w_artifact)`.
/// `None` fidelity has no weights — callers must not reach this for `None`
/// (see [`diff`]).
fn weights_for_fidelity(fidelity: ObservationFidelity) -> (f64, f64, f64, f64) {
    match fidelity {
        ObservationFidelity::Full => (0.30, 0.10, 0.45, 0.15),
        ObservationFidelity::McpOnly => (0.10, 0.00, 0.75, 0.15),
        ObservationFidelity::None => (0.0, 0.0, 0.0, 0.0),
    }
}

/// Jaccard distance between two `ToolClass` sets. Both empty ⇒ `0.0`
/// (perfectly matched — nothing was expected, nothing was seen).
fn tool_set_error(expected: &BTreeSet<ToolClass>, observed: &BTreeSet<ToolClass>) -> f64 {
    if expected.is_empty() && observed.is_empty() {
        return 0.0;
    }
    let intersection = expected.intersection(observed).count();
    let union = expected.union(observed).count();
    if union == 0 {
        0.0
    } else {
        1.0 - (intersection as f64 / union as f64)
    }
}

/// Volume error: `0.0` inside `[lo, hi]`, else normalized distance to the
/// nearest edge, capped at `1.0`.
fn volume_error(band: (u32, u32), observed_calls: u32) -> f64 {
    let (lo, hi) = band;
    if observed_calls >= lo && observed_calls <= hi {
        return 0.0;
    }
    let dist = if observed_calls < lo {
        (lo - observed_calls) as f64
    } else {
        (observed_calls - hi) as f64
    };
    let denom = (hi.max(1)) as f64;
    (dist / denom).min(1.0)
}

/// 4×4 artifact-shape distance table (design §3.1): exact match `0.0`,
/// `TextOnly` ↔ `ExternalEffect` (or the reverse) `1.0`, everything else
/// (adjacent shapes) `0.5`.
fn artifact_error(expected: ArtifactShape, observed: ArtifactShape) -> f64 {
    use ArtifactShape::*;
    if expected == observed {
        return 0.0;
    }
    match (expected, observed) {
        (TextOnly, ExternalEffect) | (ExternalEffect, TextOnly) => 1.0,
        _ => 0.5,
    }
}

/// Result of [`diff`] — `None` when `fidelity == ObservationFidelity::None`
/// (design §3.2: no evidence ⇒ no composite is computed at all, only an
/// `[unobservable: …]` log line — never a guessed value).
pub enum DiffOutcome {
    /// `fidelity == None`: nothing computable. Carries the reason for the
    /// caller to log verbatim (opus-playbook §5: "空結果優於假結果").
    Unobservable { reason: &'static str },
    Computed(TaskPredictionError),
}

/// The A3 diff algorithm (design §3). Pure, deterministic, fully unit
/// testable — zero LLM cost (hard constraint 2).
pub fn diff(
    prediction: TaskPrediction,
    observation: TaskObservation,
    thresholds: &AdaptiveThresholds,
) -> DiffOutcome {
    if observation.fidelity == ObservationFidelity::None {
        return DiffOutcome::Unobservable {
            reason: "no tool evidence in claim→review window (fidelity=None)",
        };
    }

    let tool_err = tool_set_error(&prediction.expected_tool_classes, &observation.observed_tool_classes);
    let vol_err = volume_error(prediction.expected_call_band, observation.observed_calls);
    let art_err = artifact_error(prediction.expected_artifact, observation.observed_artifact);

    let (w_tool, w_volume, w_outcome, w_artifact) = weights_for_fidelity(observation.fidelity);

    let outcome_applicable = observation.observed_outcome != ObservedOutcome::Unknown;
    let outcome_err = if outcome_applicable {
        if observation.observed_outcome.matches(prediction.expected_outcome) {
            0.0
        } else {
            1.0
        }
    } else {
        0.0
    };

    // §3.3: when outcome is inapplicable, renormalize the remaining three
    // weights proportionally instead of just zeroing w_outcome's
    // contribution (which would silently understate composite_error).
    let composite_error = if outcome_applicable {
        (w_tool * tool_err + w_volume * vol_err + w_outcome * outcome_err + w_artifact * art_err)
            .clamp(0.0, 1.0)
    } else {
        let remaining = w_tool + w_volume + w_artifact;
        if remaining <= 0.0 {
            // All non-outcome weight is zero (degenerate config) — nothing
            // left to compute from; treat as neutral rather than divide by
            // zero.
            0.0
        } else {
            ((w_tool * tool_err + w_volume * vol_err + w_artifact * art_err) / remaining)
                .clamp(0.0, 1.0)
        }
    };

    let category = thresholds.category_for(composite_error);

    // §3.3: an Unknown-outcome round under McpOnly has too little signal
    // left after renormalization (w_tool + w_artifact carries all the
    // weight) — record but do not feed the statistical bucket / A4.
    let eligible_for_stats = outcome_applicable || observation.fidelity == ObservationFidelity::Full;

    DiffOutcome::Computed(TaskPredictionError {
        tool_set_error: tool_err,
        volume_error: vol_err,
        outcome_error: outcome_err,
        outcome_error_applicable: outcome_applicable,
        artifact_error: art_err,
        composite_error,
        category,
        fidelity: observation.fidelity,
        eligible_for_stats,
        prediction,
        observation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(agent: &str, kind: GoalKind, phase: RoundPhase, has_spec: bool) -> TaskStateKey {
        TaskStateKey {
            agent_id: agent.to_string(),
            goal_kind: kind,
            phase,
            has_outcome_spec: has_spec,
        }
    }

    // ── TaskStateKey::canonical / marginal ──

    #[test]
    fn canonical_is_stable_and_field_ordered() {
        let k = key("agnes", GoalKind::CodingSimple, RoundPhase::First, true);
        assert_eq!(k.canonical(), "agnes|coding_simple|first|1");
        let k2 = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        assert_eq!(k2.canonical(), "agnes|coding_simple|first|0");
    }

    #[test]
    fn canonical_distinguishes_phase_and_spec() {
        let first = key("a", GoalKind::ResearchOrQa, RoundPhase::First, false);
        let retry = key("a", GoalKind::ResearchOrQa, RoundPhase::Retry, false);
        let restall = key("a", GoalKind::ResearchOrQa, RoundPhase::Restall, false);
        assert_ne!(first.canonical(), retry.canonical());
        assert_ne!(first.canonical(), restall.canonical());
        assert_ne!(retry.canonical(), restall.canonical());
    }

    #[test]
    fn marginal_drops_phase_and_spec() {
        let a = key("agnes", GoalKind::OpsOrExternal, RoundPhase::First, true);
        let b = key("agnes", GoalKind::OpsOrExternal, RoundPhase::Retry, false);
        assert_eq!(a.marginal(), b.marginal());
        assert_eq!(a.marginal(), "agnes|ops_or_external");
    }

    // ── diff algorithm: tool_set_error ──

    #[test]
    fn tool_set_error_both_empty_is_zero() {
        assert_eq!(tool_set_error(&BTreeSet::new(), &BTreeSet::new()), 0.0);
    }

    #[test]
    fn tool_set_error_exact_match_is_zero() {
        let s = BTreeSet::from([ToolClass::Read, ToolClass::Write]);
        assert_eq!(tool_set_error(&s, &s), 0.0);
    }

    #[test]
    fn tool_set_error_disjoint_is_one() {
        let a = BTreeSet::from([ToolClass::Read]);
        let b = BTreeSet::from([ToolClass::Exec]);
        assert_eq!(tool_set_error(&a, &b), 1.0);
    }

    #[test]
    fn tool_set_error_partial_overlap() {
        let a = BTreeSet::from([ToolClass::Read, ToolClass::Write]);
        let b = BTreeSet::from([ToolClass::Read, ToolClass::Exec]);
        // intersection=1 (Read), union=3 (Read,Write,Exec) -> 1 - 1/3
        assert!((tool_set_error(&a, &b) - (1.0 - 1.0 / 3.0)).abs() < 1e-9);
    }

    // ── diff algorithm: volume_error ──

    #[test]
    fn volume_error_inside_band_is_zero() {
        assert_eq!(volume_error((2, 8), 5), 0.0);
        assert_eq!(volume_error((2, 8), 2), 0.0, "lower boundary inclusive");
        assert_eq!(volume_error((2, 8), 8), 0.0, "upper boundary inclusive");
    }

    #[test]
    fn volume_error_below_band() {
        // dist=2, denom=hi=8 -> 0.25
        assert!((volume_error((2, 8), 0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn volume_error_above_band_caps_at_one() {
        assert_eq!(volume_error((2, 8), 1000), 1.0);
    }

    // ── diff algorithm: artifact_error ──

    #[test]
    fn artifact_error_matrix() {
        use ArtifactShape::*;
        assert_eq!(artifact_error(TextOnly, TextOnly), 0.0);
        assert_eq!(artifact_error(TextOnly, ExternalEffect), 1.0);
        assert_eq!(artifact_error(ExternalEffect, TextOnly), 1.0);
        assert_eq!(artifact_error(FileWrite, StructuredJson), 0.5);
        assert_eq!(artifact_error(TextOnly, FileWrite), 0.5);
    }

    // ── diff(): end-to-end + fidelity weighting + boundary cases ──

    fn base_prediction(state_key: TaskStateKey) -> TaskPrediction {
        TaskPrediction {
            prediction_id: "p1".into(),
            task_id: "t1".into(),
            agent_id: "agnes".into(),
            round: 1,
            expected_tool_classes: BTreeSet::from([ToolClass::Read, ToolClass::Write]),
            expected_call_band: (1, 5),
            expected_outcome: ExpectedOutcome::Accept,
            expected_artifact: ArtifactShape::FileWrite,
            confidence: 0.5,
            source: PredictionSource::Statistical,
            created_at: Utc::now(),
            state_key,
        }
    }

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

    #[test]
    fn diff_none_fidelity_is_unobservable_not_computed() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = base_prediction(sk);
        let obs = base_observation(ObservationFidelity::None);
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Unobservable { .. } => {}
            DiffOutcome::Computed(_) => panic!("None fidelity must never produce a composite_error"),
        }
    }

    #[test]
    fn diff_perfect_match_is_zero_error() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = base_prediction(sk);
        let obs = base_observation(ObservationFidelity::Full);
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                assert_eq!(err.composite_error, 0.0);
                assert_eq!(err.category, ErrorCategory::Negligible);
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }

    #[test]
    fn diff_mcp_only_weights_outcome_heavily() {
        // Tool set totally wrong but outcome matches: under McpOnly (w_tool
        // 0.10, w_outcome 0.75) the composite should stay low despite the
        // tool mismatch.
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = base_prediction(sk);
        let mut obs = base_observation(ObservationFidelity::McpOnly);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                // tool_set_error = 1.0 (disjoint), weight 0.10 -> contributes 0.10
                // volume_error weight 0.0 under McpOnly
                // outcome matches -> 0
                // artifact matches -> 0
                assert!((err.composite_error - 0.10).abs() < 1e-9);
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }

    #[test]
    fn diff_full_weights_outcome_less_than_mcp_only() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let mut pred = base_prediction(sk.clone());
        pred.expected_outcome = ExpectedOutcome::Reject; // will mismatch
        let obs_full = base_observation(ObservationFidelity::Full);
        let obs_mcp = base_observation(ObservationFidelity::McpOnly);
        let thresholds = AdaptiveThresholds::default();

        let full_err = match diff(pred.clone(), obs_full, &thresholds) {
            DiffOutcome::Computed(e) => e.composite_error,
            _ => panic!(),
        };
        let mcp_err = match diff(pred, obs_mcp, &thresholds) {
            DiffOutcome::Computed(e) => e.composite_error,
            _ => panic!(),
        };
        // McpOnly weights outcome mismatch MORE heavily (0.75 vs 0.45).
        assert!(mcp_err > full_err);
    }

    #[test]
    fn diff_unknown_outcome_renormalizes_and_marks_ineligible_under_mcp_only() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = base_prediction(sk);
        let mut obs = base_observation(ObservationFidelity::McpOnly);
        obs.observed_outcome = ObservedOutcome::Unknown;
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                assert!(!err.outcome_error_applicable);
                assert_eq!(err.outcome_error, 0.0);
                // §3.3: McpOnly + Unknown outcome -> not eligible for stats.
                assert!(!err.eligible_for_stats);
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }

    // ── WP-P2: calibration scoring ──

    #[test]
    fn task_prediction_correct_maps_negligible_and_moderate_to_true() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let mut pred = base_prediction(sk);
        pred.confidence = 0.8;
        let obs = base_observation(ObservationFidelity::Full);
        let thresholds = AdaptiveThresholds::default();
        // Perfect match ⇒ Negligible category ⇒ correct == true.
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                assert_eq!(err.category, ErrorCategory::Negligible);
                assert!(task_prediction_correct(&err));
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }

    #[test]
    fn task_prediction_correct_maps_significant_and_critical_to_false() {
        // Force a maximal mismatch on every dimension so category lands in
        // Significant/Critical territory.
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let mut pred = base_prediction(sk);
        pred.expected_tool_classes = BTreeSet::from([ToolClass::Read]);
        pred.expected_call_band = (1, 2);
        pred.expected_outcome = ExpectedOutcome::Reject;
        pred.expected_artifact = ArtifactShape::TextOnly;
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs.observed_calls = 1000;
        obs.observed_outcome = ObservedOutcome::Accepted;
        obs.observed_artifact = ArtifactShape::ExternalEffect;
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                assert!(
                    matches!(err.category, ErrorCategory::Significant | ErrorCategory::Critical),
                    "fixture must land in Significant/Critical, got {:?}",
                    err.category
                );
                assert!(!task_prediction_correct(&err));
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }

    #[test]
    fn calibration_brier_score_matches_calibration_brier_binary() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        // Negligible case: confidence + correct=true.
        let mut pred_ok = base_prediction(sk.clone());
        pred_ok.confidence = 0.73;
        let obs_ok = base_observation(ObservationFidelity::Full);
        let thresholds = AdaptiveThresholds::default();
        let err_ok = match diff(pred_ok, obs_ok, &thresholds) {
            DiffOutcome::Computed(e) => e,
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        };
        assert_eq!(err_ok.category, ErrorCategory::Negligible);
        assert_eq!(
            calibration_brier_score(&err_ok),
            calibration::brier_binary(0.73, true)
        );

        // Critical case: confidence + correct=false.
        let mut pred_bad = base_prediction(sk);
        pred_bad.confidence = 0.9;
        pred_bad.expected_tool_classes = BTreeSet::from([ToolClass::Read]);
        pred_bad.expected_call_band = (1, 2);
        pred_bad.expected_outcome = ExpectedOutcome::Reject;
        pred_bad.expected_artifact = ArtifactShape::TextOnly;
        let mut obs_bad = base_observation(ObservationFidelity::Full);
        obs_bad.observed_tool_classes = BTreeSet::from([ToolClass::Exec, ToolClass::Net]);
        obs_bad.observed_calls = 1000;
        obs_bad.observed_outcome = ObservedOutcome::Accepted;
        obs_bad.observed_artifact = ArtifactShape::ExternalEffect;
        let err_bad = match diff(pred_bad, obs_bad, &thresholds) {
            DiffOutcome::Computed(e) => e,
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        };
        assert!(matches!(err_bad.category, ErrorCategory::Significant | ErrorCategory::Critical));
        assert_eq!(
            calibration_brier_score(&err_bad),
            calibration::brier_binary(0.9, false)
        );
    }

    #[test]
    fn diff_unknown_outcome_under_full_fidelity_still_eligible() {
        let sk = key("agnes", GoalKind::CodingSimple, RoundPhase::First, false);
        let pred = base_prediction(sk);
        let mut obs = base_observation(ObservationFidelity::Full);
        obs.observed_outcome = ObservedOutcome::Unknown;
        let thresholds = AdaptiveThresholds::default();
        match diff(pred, obs, &thresholds) {
            DiffOutcome::Computed(err) => {
                assert!(err.eligible_for_stats, "Full fidelity keeps enough signal even without outcome");
            }
            DiffOutcome::Unobservable { .. } => panic!("should compute"),
        }
    }
}
