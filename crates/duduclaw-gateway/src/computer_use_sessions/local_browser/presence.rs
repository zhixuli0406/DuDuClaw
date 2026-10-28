//! Host-authenticated presence, never a JSON field supplied by an employee or JWT.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub(crate) const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(3);
pub(crate) const LOST_COMPANION_STOP: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PresenceReport {
    pub session_available: bool,
    pub on_console: bool,
    pub login_done: bool,
    pub locked: bool,
    pub idle_seconds: f64,
    pub browser_frontmost: bool,
    /// Monotonic host input observation. Native CDP input does not update it.
    pub input_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresenceProblem {
    Offline,
    Locked,
    Idle,
    HumanInput,
}

pub(crate) struct PresenceState {
    report: Option<(PresenceReport, Instant)>,
    accepted_input: u64,
    hold: Option<PresenceProblem>,
}

impl PresenceState {
    pub(crate) fn new() -> Self {
        Self {
            report: None,
            accepted_input: 0,
            hold: Some(PresenceProblem::Offline),
        }
    }

    pub(crate) fn update(&mut self, report: PresenceReport, now: Instant, idle_limit: u64) {
        let problem = if !report.session_available || !report.on_console || !report.login_done {
            Some(PresenceProblem::Locked)
        } else if report.locked {
            Some(PresenceProblem::Locked)
        } else if !report.idle_seconds.is_finite()
            || report.idle_seconds < 0.0
            || report.idle_seconds >= idle_limit as f64
        {
            Some(PresenceProblem::Idle)
        } else if report.browser_frontmost && report.input_sequence != self.accepted_input {
            Some(PresenceProblem::HumanInput)
        } else {
            None
        };
        // A good heartbeat never silently clears a latched freeze. Only the
        // native companion's explicit Continue gesture can do so.
        if problem.is_some() {
            self.hold = problem;
        }
        if !report.browser_frontmost {
            self.accepted_input = report.input_sequence;
        }
        self.report = Some((report, now));
    }

    pub(crate) fn problem(&self, now: Instant) -> Option<PresenceProblem> {
        if self
            .report
            .as_ref()
            .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= HEARTBEAT_DEADLINE)
        {
            Some(PresenceProblem::Offline)
        } else {
            self.hold
        }
    }

    pub(crate) fn must_stop(&self, now: Instant) -> bool {
        self.report
            .as_ref()
            .is_none_or(|(_, at)| now.saturating_duration_since(*at) >= LOST_COMPANION_STOP)
    }

    pub(crate) fn resume(&mut self, now: Instant, idle_limit: u64) -> Result<(), PresenceProblem> {
        let Some((report, at)) = &self.report else {
            return Err(PresenceProblem::Offline);
        };
        if now.saturating_duration_since(*at) >= HEARTBEAT_DEADLINE {
            return Err(PresenceProblem::Offline);
        }
        if !report.session_available || !report.on_console || !report.login_done || report.locked {
            return Err(PresenceProblem::Locked);
        }
        if !report.idle_seconds.is_finite()
            || report.idle_seconds < 0.0
            || report.idle_seconds >= idle_limit as f64
        {
            return Err(PresenceProblem::Idle);
        }
        self.accepted_input = report.input_sequence;
        self.hold = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn present() -> PresenceReport {
        PresenceReport {
            session_available: true,
            on_console: true,
            login_done: true,
            locked: false,
            idle_seconds: 0.5,
            browser_frontmost: false,
            input_sequence: 1,
        }
    }
    #[test]
    fn offline_lock_idle_and_human_input_latch_until_native_continue() {
        let now = Instant::now();
        let mut state = PresenceState::new();
        assert_eq!(state.problem(now), Some(PresenceProblem::Offline));
        state.update(present(), now, 600);
        state.resume(now, 600).unwrap();
        assert_eq!(state.problem(now), None);
        let mut r = present();
        r.locked = true;
        state.update(r, now, 600);
        assert_eq!(state.problem(now), Some(PresenceProblem::Locked));
        state.update(present(), now, 600);
        assert_eq!(state.problem(now), Some(PresenceProblem::Locked));
        state.resume(now, 600).unwrap();
        let mut r = present();
        r.browser_frontmost = true;
        r.input_sequence = 2;
        state.update(r, now, 600);
        assert_eq!(state.problem(now), Some(PresenceProblem::HumanInput));
        state.resume(now, 600).unwrap();
        let mut r = present();
        r.idle_seconds = f64::NAN;
        state.update(r, now, 600);
        assert_eq!(state.resume(now, 600), Err(PresenceProblem::Idle));
        assert_eq!(
            state.problem(now + Duration::from_secs(8)),
            Some(PresenceProblem::Offline)
        );
        assert!(state.must_stop(now + LOST_COMPANION_STOP));
    }
    #[test]
    fn a_wrapped_or_reset_hardware_counter_also_requires_native_continue() {
        let now = Instant::now();
        let mut state = PresenceState::new();
        let mut report = present();
        report.input_sequence = u32::MAX as u64;
        state.update(report.clone(), now, 600);
        state.resume(now, 600).unwrap();
        report.browser_frontmost = true;
        report.input_sequence = 0;
        state.update(report, now, 600);
        assert_eq!(state.problem(now), Some(PresenceProblem::HumanInput));
    }
}
