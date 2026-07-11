//! Burn-rate cost anomaly detection.
//!
//! Fixed spend thresholds (the [`budget`](crate::budget) circuit breaker)
//! catch "you hit your cap", but 2026 FinOps guidance is that most real runaway
//! spend is caught earlier by a *relative* signal: today is burning far faster
//! than this agent's own recent baseline. This module computes that with plain
//! statistics (rolling mean + standard deviation over the agent's per-day spend
//! history) — no ML, no new storage. It reads the per-day series from
//! [`CostTelemetry::daily_cost_millicents`](crate::cost_telemetry::CostTelemetry::daily_cost_millicents).
//!
//! Complements the hard breaker: the breaker *blocks*, this *warns* (a soft
//! signal routed to logs / the notify path) so a spend spike is visible before
//! it reaches the cap.

/// Result of an anomaly check for one agent's current-window spend.
#[derive(Debug, Clone, PartialEq)]
pub struct AnomalyVerdict {
    /// True when `current` exceeds `mean + sigma·std` over the baseline history.
    pub is_anomaly: bool,
    /// Current-window spend (cents).
    pub current_cents: u64,
    /// Baseline mean (cents).
    pub mean_cents: f64,
    /// Baseline standard deviation (cents).
    pub std_cents: f64,
    /// Z-score of `current` against the baseline (`(current − mean) / std`);
    /// `0.0` when std is zero.
    pub z: f64,
}

/// Detect a burn-rate anomaly: is `current_cents` a statistical outlier above
/// the `history` baseline (mean + `sigma`·stddev)?
///
/// Returns a non-anomalous verdict when there are fewer than `min_samples`
/// baseline points (too little history to judge) or when the baseline has zero
/// variance AND `current` is within it. `history` should be the agent's prior
/// per-day spend (cents), excluding the current day.
pub fn detect(
    history: &[u64],
    current_cents: u64,
    sigma: f64,
    min_samples: usize,
) -> AnomalyVerdict {
    let n = history.len();
    let base = AnomalyVerdict {
        is_anomaly: false,
        current_cents,
        mean_cents: 0.0,
        std_cents: 0.0,
        z: 0.0,
    };
    if n < min_samples.max(1) {
        return base;
    }
    let mean = history.iter().map(|x| *x as f64).sum::<f64>() / n as f64;
    let variance =
        history.iter().map(|x| (*x as f64 - mean).powi(2)).sum::<f64>() / n as f64;
    let std = variance.sqrt();
    let cur = current_cents as f64;

    // Zero-variance baseline: any spend strictly above the flat baseline is
    // anomalous; equal/below is not (avoids div-by-zero z-score).
    if std == 0.0 {
        return AnomalyVerdict {
            is_anomaly: cur > mean,
            current_cents,
            mean_cents: mean,
            std_cents: 0.0,
            z: 0.0,
        };
    }
    let z = (cur - mean) / std;
    AnomalyVerdict {
        is_anomaly: cur > mean + sigma * std,
        current_cents,
        mean_cents: mean,
        std_cents: std,
        z,
    }
}

/// Default sensitivity: 3σ above baseline (classic outlier threshold).
pub const DEFAULT_SIGMA: f64 = 3.0;
/// Default minimum baseline days before we'll judge an anomaly.
pub const DEFAULT_MIN_SAMPLES: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_spike_above_baseline() {
        // Steady ~100/day for a week, then 1000 today → anomaly.
        let hist = [100, 110, 90, 105, 95, 100, 100];
        let v = detect(&hist, 1000, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES);
        assert!(v.is_anomaly, "10x spike must flag: z={}", v.z);
        assert!(v.z > 3.0);
    }

    #[test]
    fn normal_variation_not_flagged() {
        let hist = [100, 110, 90, 105, 95, 100, 100];
        assert!(!detect(&hist, 115, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn too_little_history_never_flags() {
        // 2 samples < min 5 → cannot judge, even a huge value is not flagged.
        assert!(!detect(&[10, 10], 100_000, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn zero_variance_baseline() {
        // Flat 50/day baseline: 51 is above → anomaly; 50 is not.
        let hist = [50, 50, 50, 50, 50, 50];
        assert!(detect(&hist, 51, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, 50, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, 49, DEFAULT_SIGMA, DEFAULT_MIN_SAMPLES).is_anomaly);
    }

    #[test]
    fn sigma_controls_sensitivity() {
        // mean=100, std≈12.9 → 1σ≈112.9, 3σ≈138.7.
        let hist = [100, 120, 80, 110, 90, 100];
        // A moderate bump between 1σ and 3σ: flagged at 1σ, not at 3σ.
        let cur = 130;
        assert!(detect(&hist, cur, 1.0, DEFAULT_MIN_SAMPLES).is_anomaly);
        assert!(!detect(&hist, cur, 3.0, DEFAULT_MIN_SAMPLES).is_anomaly);
    }
}
