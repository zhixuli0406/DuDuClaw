//! Merge resolution — turn a judge verdict into a promote/defer decision per the
//! configured [`MergeMode`] (RFC-26 §3.3, P2).

use std::collections::HashMap;

use crate::branch::BranchId;
use crate::judge::JudgeVerdict;
use crate::MergeMode;

/// Default minimum confidence below which even `Auto`/`Vote` defer to a human.
pub const DEFAULT_CONFIDENCE_THRESHOLD: f64 = 0.5;

/// The resolution of a fork.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeDecision {
    /// The branch to promote, or `None` when the decision is deferred to a human.
    pub winner: Option<BranchId>,
    /// Whether the caller must obtain operator confirmation before promoting.
    pub needs_confirmation: bool,
    /// Human-readable reason, for the Activity Feed / history log.
    pub reason: String,
}

impl MergeDecision {
    fn defer(reason: impl Into<String>) -> Self {
        MergeDecision { winner: None, needs_confirmation: true, reason: reason.into() }
    }
}

/// Resolve a single-judge verdict under `mode`.
///
/// Fail-closed: a winner whose confidence is below `threshold` is deferred to a
/// human regardless of mode (RFC-26 §2).
pub fn resolve(verdict: &JudgeVerdict, mode: MergeMode, threshold: f64) -> MergeDecision {
    if verdict.confidence < threshold {
        return MergeDecision::defer(format!(
            "confidence {:.2} below threshold {:.2}",
            verdict.confidence, threshold
        ));
    }
    match mode {
        MergeMode::Manual => MergeDecision::defer("manual merge mode: operator selects"),
        MergeMode::Auto => MergeDecision {
            winner: Some(verdict.winner.clone()),
            needs_confirmation: false,
            reason: format!("auto: judge picked with confidence {:.2}", verdict.confidence),
        },
        MergeMode::AutoWithFallback => MergeDecision {
            winner: Some(verdict.winner.clone()),
            needs_confirmation: true,
            reason: format!(
                "auto_with_fallback: judge picked with confidence {:.2}, awaiting confirm",
                verdict.confidence
            ),
        },
        // Vote requires multiple verdicts; a single verdict is treated as a degenerate
        // 1-vote consensus but still surfaced for confirmation.
        MergeMode::Vote => MergeDecision {
            winner: Some(verdict.winner.clone()),
            needs_confirmation: true,
            reason: "vote mode with a single verdict; awaiting confirm".into(),
        },
    }
}

/// Resolve `Vote` mode across N independent verdicts by majority. A strict tie for
/// the top spot defers to a human.
pub fn resolve_vote(verdicts: &[JudgeVerdict], threshold: f64) -> MergeDecision {
    if verdicts.is_empty() {
        return MergeDecision::defer("vote: no verdicts");
    }
    let mut tally: HashMap<&BranchId, usize> = HashMap::new();
    for v in verdicts {
        *tally.entry(&v.winner).or_insert(0) += 1;
    }
    let max_votes = tally.values().copied().max().unwrap_or(0);
    let leaders: Vec<&BranchId> = tally
        .iter()
        .filter(|&(_, &n)| n == max_votes)
        .map(|(id, _)| *id)
        .collect();

    if leaders.len() != 1 {
        return MergeDecision::defer(format!("vote tie among {} branches", leaders.len()));
    }
    let winner = leaders[0].clone();

    // Mean confidence of verdicts that chose the winner.
    let winner_confs: Vec<f64> = verdicts
        .iter()
        .filter(|v| v.winner == winner)
        .map(|v| v.confidence)
        .collect();
    let mean_conf = winner_confs.iter().sum::<f64>() / winner_confs.len() as f64;
    if mean_conf < threshold {
        return MergeDecision::defer(format!(
            "vote winner mean confidence {mean_conf:.2} below threshold {threshold:.2}"
        ));
    }

    MergeDecision {
        winner: Some(winner),
        needs_confirmation: false,
        reason: format!("vote: {max_votes}/{} judges agree", verdicts.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(winner: &BranchId, confidence: f64) -> JudgeVerdict {
        JudgeVerdict {
            winner: winner.clone(),
            confidence,
            per_branch_scores: vec![(winner.clone(), confidence)],
            rationale: "r".into(),
        }
    }

    #[test]
    fn auto_promotes_no_confirm() {
        let w = BranchId::new();
        let d = resolve(&verdict(&w, 0.9), MergeMode::Auto, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, Some(w));
        assert!(!d.needs_confirmation);
    }

    #[test]
    fn auto_with_fallback_promotes_with_confirm() {
        let w = BranchId::new();
        let d = resolve(&verdict(&w, 0.9), MergeMode::AutoWithFallback, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, Some(w));
        assert!(d.needs_confirmation);
    }

    #[test]
    fn manual_always_defers() {
        let w = BranchId::new();
        let d = resolve(&verdict(&w, 0.99), MergeMode::Manual, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, None);
        assert!(d.needs_confirmation);
    }

    #[test]
    fn below_threshold_defers_even_on_auto() {
        let w = BranchId::new();
        let d = resolve(&verdict(&w, 0.2), MergeMode::Auto, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, None);
        assert!(d.needs_confirmation);
    }

    #[test]
    fn vote_majority_wins() {
        let a = BranchId::new();
        let b = BranchId::new();
        let verdicts = vec![verdict(&a, 0.8), verdict(&a, 0.7), verdict(&b, 0.9)];
        let d = resolve_vote(&verdicts, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, Some(a));
        assert!(!d.needs_confirmation);
    }

    #[test]
    fn vote_tie_defers() {
        let a = BranchId::new();
        let b = BranchId::new();
        let verdicts = vec![verdict(&a, 0.9), verdict(&b, 0.9)];
        let d = resolve_vote(&verdicts, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, None);
    }

    #[test]
    fn vote_low_confidence_defers() {
        let a = BranchId::new();
        let verdicts = vec![verdict(&a, 0.3), verdict(&a, 0.2)];
        let d = resolve_vote(&verdicts, DEFAULT_CONFIDENCE_THRESHOLD);
        assert_eq!(d.winner, None);
    }

    #[test]
    fn vote_empty_defers() {
        assert_eq!(resolve_vote(&[], DEFAULT_CONFIDENCE_THRESHOLD).winner, None);
    }
}
