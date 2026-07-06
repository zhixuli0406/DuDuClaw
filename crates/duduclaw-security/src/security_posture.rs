//! SecurityPosture — a deterministic {Green, Yellow, Red} threat state machine
//! (P3-2 of the runtime-agnostic security redesign).
//!
//! Design notes:
//!   - There is intentionally **no** `ThreatLevel` type in the codebase (two
//!     `grep -rnE "enum ThreatLevel"` sweeps came back empty), so this new
//!     `SecurityPosture` has no naming conflict.
//!   - The counting signal is the security audit log: N high-severity events
//!     (blocked injections, contract violations, …) within a sliding window ≈
//!     "N denials in T" (`audit::count_events_since`).
//!   - A second escalation signal is the prediction engine's `ErrorCategory`
//!     (`Significant` / `Critical`). That enum lives in `duduclaw-gateway`, and
//!     `duduclaw-security` must not depend on it (that would be a dependency
//!     cycle), so this module exposes a local [`EscalationFloor`] that the
//!     gateway maps `ErrorCategory` onto.
//!   - Escalate FAST, decay SLOW: the posture jumps straight to a higher state
//!     the instant the window warrants it, but only steps DOWN one level at a
//!     time (gradual de-escalation), so a brief quiet spell can't mask an
//!     ongoing incident.

/// Threat posture, ordered least → most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecurityPosture {
    Green = 0,
    Yellow = 1,
    Red = 2,
}

impl SecurityPosture {
    /// One step toward `Green` (used for gradual de-escalation).
    pub fn decayed(self) -> Self {
        match self {
            SecurityPosture::Red => SecurityPosture::Yellow,
            SecurityPosture::Yellow => SecurityPosture::Green,
            SecurityPosture::Green => SecurityPosture::Green,
        }
    }
}

/// A minimum posture floor derived from an out-of-band escalation signal (e.g.
/// the gateway's `ErrorCategory`). The gateway maps `Significant → Yellow` and
/// `Critical → Red`.
pub type EscalationFloor = SecurityPosture;

/// Thresholds for mapping a window's audit-event counts to a posture (D-7).
#[derive(Debug, Clone, Copy)]
pub struct PostureThresholds {
    /// Warning-severity events in the window at/above which posture is Yellow.
    pub yellow_warnings: usize,
    /// Critical-severity events in the window at/above which posture is Yellow.
    pub yellow_criticals: usize,
    /// Critical-severity events in the window at/above which posture is Red.
    pub red_criticals: usize,
}

impl Default for PostureThresholds {
    fn default() -> Self {
        // D-7 defaults over a ~60s window: a single blocked-injection /
        // contract-violation raises Yellow; three within the window is Red.
        Self { yellow_warnings: 5, yellow_criticals: 1, red_criticals: 3 }
    }
}

/// Default sliding window (seconds) for the audit-count signal.
pub const DEFAULT_WINDOW_SECONDS: i64 = 60;

/// Pure: map window counts to a posture (deterministic, zero I/O).
pub fn posture_from_counts(
    warning: usize,
    critical: usize,
    t: &PostureThresholds,
) -> SecurityPosture {
    if critical >= t.red_criticals {
        SecurityPosture::Red
    } else if critical >= t.yellow_criticals || warning >= t.yellow_warnings {
        SecurityPosture::Yellow
    } else {
        SecurityPosture::Green
    }
}

/// A stateful posture tracker implementing escalate-fast / decay-slow.
#[derive(Debug, Clone)]
pub struct PostureTracker {
    current: SecurityPosture,
    thresholds: PostureThresholds,
}

impl Default for PostureTracker {
    fn default() -> Self {
        Self { current: SecurityPosture::Green, thresholds: PostureThresholds::default() }
    }
}

impl PostureTracker {
    pub fn new(thresholds: PostureThresholds) -> Self {
        Self { current: SecurityPosture::Green, thresholds }
    }

    pub fn current(&self) -> SecurityPosture {
        self.current
    }

    /// Fold in one observation: the posture the current window warrants, plus
    /// an optional escalation floor (e.g. from `ErrorCategory`). Escalates
    /// immediately to the most severe of the two; if both are below the current
    /// posture, decays exactly one level. Returns the new posture.
    pub fn observe(
        &mut self,
        warning: usize,
        critical: usize,
        floor: Option<EscalationFloor>,
    ) -> SecurityPosture {
        let window = posture_from_counts(warning, critical, &self.thresholds);
        let target = window.max(floor.unwrap_or(SecurityPosture::Green));
        self.current = if target > self.current {
            target // escalate fast
        } else if target < self.current {
            self.current.decayed() // decay slow (one step)
        } else {
            self.current
        };
        self.current
    }
}

/// I/O convenience: derive the current window posture straight from the audit
/// log. Pure counting logic stays in [`posture_from_counts`]; this only adds the
/// time-window read via [`crate::audit::count_events_since`].
pub fn posture_from_audit(
    home_dir: &std::path::Path,
    window_seconds: i64,
    thresholds: &PostureThresholds,
    now: chrono::DateTime<chrono::Utc>,
) -> SecurityPosture {
    let since = (now - chrono::Duration::seconds(window_seconds)).to_rfc3339();
    let (_info, warning, critical) = crate::audit::count_events_since(home_dir, &since);
    posture_from_counts(warning, critical, thresholds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_least_to_most_severe() {
        assert!(SecurityPosture::Green < SecurityPosture::Yellow);
        assert!(SecurityPosture::Yellow < SecurityPosture::Red);
    }

    #[test]
    fn counts_map_to_posture() {
        let t = PostureThresholds::default();
        assert_eq!(posture_from_counts(0, 0, &t), SecurityPosture::Green);
        assert_eq!(posture_from_counts(5, 0, &t), SecurityPosture::Yellow); // warnings
        assert_eq!(posture_from_counts(0, 1, &t), SecurityPosture::Yellow); // one critical
        assert_eq!(posture_from_counts(0, 3, &t), SecurityPosture::Red); // N-deny-in-T
        assert_eq!(posture_from_counts(100, 2, &t), SecurityPosture::Yellow);
    }

    #[test]
    fn escalates_fast_to_red() {
        let mut tracker = PostureTracker::default();
        assert_eq!(tracker.observe(0, 3, None), SecurityPosture::Red);
    }

    #[test]
    fn decays_one_level_at_a_time() {
        let mut tracker = PostureTracker::default();
        tracker.observe(0, 3, None); // Red
        assert_eq!(tracker.current(), SecurityPosture::Red);
        // Quiet window: Red → Yellow, not straight to Green.
        assert_eq!(tracker.observe(0, 0, None), SecurityPosture::Yellow);
        assert_eq!(tracker.observe(0, 0, None), SecurityPosture::Green);
        assert_eq!(tracker.observe(0, 0, None), SecurityPosture::Green);
    }

    #[test]
    fn error_floor_escalates_without_audit_events() {
        // An ErrorCategory::Critical (mapped to a Red floor) escalates even with
        // a clean audit window.
        let mut tracker = PostureTracker::default();
        assert_eq!(
            tracker.observe(0, 0, Some(SecurityPosture::Red)),
            SecurityPosture::Red
        );
        // Significant (Yellow floor) after Red decays only one step (Yellow).
        assert_eq!(
            tracker.observe(0, 0, Some(SecurityPosture::Yellow)),
            SecurityPosture::Yellow
        );
    }

    #[test]
    fn posture_from_audit_reads_window() {
        let tmp = std::env::temp_dir().join(format!("ddc-posture-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Three critical events "now" → Red within a 60s window.
        for _ in 0..3 {
            crate::audit::log_injection_detected(&tmp, "agent-x", 99, &["x".into()], true);
        }
        let posture = posture_from_audit(
            &tmp,
            DEFAULT_WINDOW_SECONDS,
            &PostureThresholds::default(),
            chrono::Utc::now(),
        );
        assert_eq!(posture, SecurityPosture::Red);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
