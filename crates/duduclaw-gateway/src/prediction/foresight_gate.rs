//! WP-B4 — selective foresight gate.
//!
//! See `commercial/docs/design-task-forward-model-2026-08-06.md` §6.4. Any
//! path that would surface a prediction-derived narrative to a prompt or to
//! a human (D1 approval copy, D2 three-step trajectory preview) must pass
//! [`foresight_gate`] first. All four conditions must hold; failing any one
//! means **no injection at all** — never a downgraded "we're not sure but…"
//! placeholder (design §6.4: "缺一即完全不注入（不降級成模糊敘述）").
//!
//! Named `foresight_gate` rather than folded into the existing
//! `crate::foresight` module: that module is the unrelated R2
//! `AgentForesight` early-failure-warning scorer (trajectory-prefix based,
//! nothing to do with task-forward-model predictions). Reusing the name
//! would have made two structurally unrelated "foresight" concepts share
//! one file. See the implementation report for this deviation from the
//! design doc's "新檔或併入 foresight.rs" wording.
//!
//! **No consumer wired in this change** (the D-series HITL surfaces this
//! would gate are future work) — same "land the primitive first" posture
//! design §9 calls for on this work package.

use super::task_forward::{ObservationFidelity, PredictionSource, TaskPrediction};

/// Confidence floor (design §6.4 condition 1). `n_samples >= 5` given
/// `MATURE_N = 10` (`confidence = n_samples / MATURE_N`).
pub const DEFAULT_FORESIGHT_TAU: f64 = 0.5;
/// Recent-error window size (design §6.4 condition 3, `k`).
pub const DEFAULT_FORESIGHT_RECENT_K: usize = 5;
/// Recent-error mean ceiling (design §6.4 condition 3).
pub const DEFAULT_FORESIGHT_ERROR_CEILING: f64 = 0.5;

/// Tunables, mirroring the `[task_forward_model] foresight_tau` /
/// `foresight_recent_k` config keys design §7.3 reserves (not parsed by
/// this change — no config wiring exists yet since there's no consumer;
/// callers construct this directly or via [`ForesightGateConfig::default`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ForesightGateConfig {
    pub tau: f64,
    pub recent_k: usize,
    pub error_ceiling: f64,
}

impl Default for ForesightGateConfig {
    fn default() -> Self {
        Self {
            tau: DEFAULT_FORESIGHT_TAU,
            recent_k: DEFAULT_FORESIGHT_RECENT_K,
            error_ceiling: DEFAULT_FORESIGHT_ERROR_CEILING,
        }
    }
}

/// The four design §6.4 conditions, evaluated independently so a caller
/// that wants to log *why* the gate closed can inspect which one failed
/// (rather than getting back a single opaque bool). [`foresight_gate`] is
/// the all-four-must-hold convenience wrapper most callers want.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForesightGateReasons {
    /// Condition 1: `prediction.confidence >= tau`.
    pub confidence_ok: bool,
    /// Condition 2: `prediction.source ∈ {Statistical, Marginal}`.
    pub source_ok: bool,
    /// Condition 3: mean of the last `k` settled errors for this
    /// `state_key` `< error_ceiling`.
    pub recent_accuracy_ok: bool,
    /// Condition 4: the transition material's own fidelity is `Full`.
    pub fidelity_ok: bool,
}

impl ForesightGateReasons {
    pub fn all_pass(&self) -> bool {
        self.confidence_ok && self.source_ok && self.recent_accuracy_ok && self.fidelity_ok
    }

    /// Human-readable reasons for whichever conditions failed — for a
    /// `tracing::debug!` at the call site, never shown to the end user
    /// (the gate closing means nothing is shown at all).
    pub fn failed_conditions(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.confidence_ok {
            out.push("confidence below tau");
        }
        if !self.source_ok {
            out.push("prediction source is Prior or ColdStartLlm");
        }
        if !self.recent_accuracy_ok {
            out.push("recent settled-error mean at/above ceiling");
        }
        if !self.fidelity_ok {
            out.push("transition fidelity is not Full");
        }
        out
    }
}

/// Evaluate all four design §6.4 conditions.
///
/// `recent_errors` is the caller-supplied list of the most recent settled
/// `composite_error` values for this exact `state_key` (oldest-first or
/// any order — only the last `config.recent_k` entries, or all of them if
/// fewer, are averaged). An empty slice fails condition 3 closed (no
/// evidence of accuracy ⇒ not gated open) rather than vacuously passing.
pub fn evaluate_foresight_gate(
    prediction: &TaskPrediction,
    recent_errors: &[f64],
    fidelity: ObservationFidelity,
    config: ForesightGateConfig,
) -> ForesightGateReasons {
    let confidence_ok = prediction.confidence >= config.tau;
    let source_ok = matches!(
        prediction.source,
        PredictionSource::Statistical | PredictionSource::Marginal
    );
    let recent_accuracy_ok = if recent_errors.is_empty() {
        false
    } else {
        let window = &recent_errors[recent_errors.len().saturating_sub(config.recent_k)..];
        let mean = window.iter().sum::<f64>() / window.len() as f64;
        mean < config.error_ceiling
    };
    let fidelity_ok = fidelity == ObservationFidelity::Full;

    ForesightGateReasons {
        confidence_ok,
        source_ok,
        recent_accuracy_ok,
        fidelity_ok,
    }
}

/// Convenience: `true` only when all four conditions hold. This is the gate
/// callers should actually branch on — `evaluate_foresight_gate` exists for
/// diagnostics.
pub fn foresight_gate(
    prediction: &TaskPrediction,
    recent_errors: &[f64],
    fidelity: ObservationFidelity,
) -> bool {
    evaluate_foresight_gate(prediction, recent_errors, fidelity, ForesightGateConfig::default())
        .all_pass()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction::task_forward::{
        ArtifactShape, ExpectedOutcome, GoalKind, RoundPhase, TaskStateKey,
    };
    use std::collections::BTreeSet;

    fn prediction(confidence: f64, source: PredictionSource) -> TaskPrediction {
        TaskPrediction {
            prediction_id: "p1".into(),
            task_id: "t1".into(),
            agent_id: "agnes".into(),
            round: 1,
            state_key: TaskStateKey {
                agent_id: "agnes".into(),
                goal_kind: GoalKind::CodingSimple,
                phase: RoundPhase::First,
                has_outcome_spec: false,
            },
            expected_tool_classes: BTreeSet::new(),
            expected_call_band: (1, 5),
            expected_outcome: ExpectedOutcome::Accept,
            expected_artifact: ArtifactShape::FileWrite,
            confidence,
            source,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn all_four_conditions_pass_opens_gate() {
        let pred = prediction(0.6, PredictionSource::Statistical);
        assert!(foresight_gate(&pred, &[0.1, 0.2, 0.1], ObservationFidelity::Full));
    }

    #[test]
    fn low_confidence_closes_gate() {
        let pred = prediction(0.4, PredictionSource::Statistical);
        assert!(!foresight_gate(&pred, &[0.1], ObservationFidelity::Full));
    }

    #[test]
    fn confidence_exactly_at_tau_passes() {
        let pred = prediction(DEFAULT_FORESIGHT_TAU, PredictionSource::Statistical);
        assert!(foresight_gate(&pred, &[0.1], ObservationFidelity::Full));
    }

    #[test]
    fn prior_source_closes_gate_even_with_high_confidence() {
        // Confidence field could in principle be nonzero even for Prior in
        // some future variant — the gate must key off `source`, not just
        // confidence.
        let pred = prediction(0.9, PredictionSource::Prior);
        assert!(!foresight_gate(&pred, &[0.1], ObservationFidelity::Full));
    }

    #[test]
    fn cold_start_llm_source_closes_gate() {
        let pred = prediction(0.9, PredictionSource::ColdStartLlm);
        assert!(!foresight_gate(&pred, &[0.1], ObservationFidelity::Full));
    }

    #[test]
    fn marginal_source_is_acceptable() {
        let pred = prediction(0.6, PredictionSource::Marginal);
        assert!(foresight_gate(&pred, &[0.1], ObservationFidelity::Full));
    }

    #[test]
    fn empty_recent_errors_closes_gate_not_vacuous_pass() {
        let pred = prediction(0.9, PredictionSource::Statistical);
        assert!(!foresight_gate(&pred, &[], ObservationFidelity::Full));
    }

    #[test]
    fn high_recent_error_mean_closes_gate() {
        let pred = prediction(0.9, PredictionSource::Statistical);
        assert!(!foresight_gate(&pred, &[0.8, 0.9, 0.7], ObservationFidelity::Full));
    }

    #[test]
    fn recent_error_window_only_considers_last_k() {
        let pred = prediction(0.9, PredictionSource::Statistical);
        // Old high errors outside the k=5 window should not sink an
        // otherwise-good recent run.
        let mut errors = vec![0.99, 0.99, 0.99, 0.99, 0.99]; // stale, will be pushed out
        errors.extend([0.05, 0.05, 0.05, 0.05, 0.05]); // last 5 — good
        assert!(foresight_gate(&pred, &errors, ObservationFidelity::Full));
    }

    #[test]
    fn mcp_only_fidelity_closes_gate() {
        // This is the deliberate "first version blocks D-series" outcome
        // design §6.4/§8.2/T7 calls out: McpOnly transition material must
        // not reach the user-facing narrative.
        let pred = prediction(0.9, PredictionSource::Statistical);
        assert!(!foresight_gate(&pred, &[0.1], ObservationFidelity::McpOnly));
    }

    #[test]
    fn none_fidelity_closes_gate() {
        let pred = prediction(0.9, PredictionSource::Statistical);
        assert!(!foresight_gate(&pred, &[0.1], ObservationFidelity::None));
    }

    #[test]
    fn evaluate_foresight_gate_reports_all_failed_conditions() {
        let pred = prediction(0.1, PredictionSource::Prior);
        let reasons = evaluate_foresight_gate(&pred, &[], ObservationFidelity::McpOnly, ForesightGateConfig::default());
        assert!(!reasons.all_pass());
        let failed = reasons.failed_conditions();
        assert_eq!(failed.len(), 4, "all four conditions should fail for this fixture: {failed:?}");
    }

    #[test]
    fn custom_config_widens_or_narrows_thresholds() {
        let pred = prediction(0.3, PredictionSource::Statistical);
        let lenient = ForesightGateConfig { tau: 0.2, recent_k: 5, error_ceiling: 0.9 };
        assert!(foresight_gate_with_config(&pred, &[0.5], ObservationFidelity::Full, lenient));
    }

    fn foresight_gate_with_config(
        prediction: &TaskPrediction,
        recent_errors: &[f64],
        fidelity: ObservationFidelity,
        config: ForesightGateConfig,
    ) -> bool {
        evaluate_foresight_gate(prediction, recent_errors, fidelity, config).all_pass()
    }
}
