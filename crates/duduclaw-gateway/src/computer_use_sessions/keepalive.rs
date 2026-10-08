//! Keep-alive for idle tool-driven sessions (P8): instead of ending a
//! session after [`super::IDLE_TIMEOUT`], an employee with
//! `[capabilities.computer_use_config] keep_alive_minutes > 0` gets its
//! container paused (`docker pause`) and resumed by the next tool call or
//! dashboard viewer. Everything else that ends a session still does, paused
//! or not: the stop flag, threat level RED, the hard deadline
//! (`max_session_minutes`, which keeps counting while paused) and every
//! check [`super::ComputerUseSessions::check_alive`] runs besides these.
//!
//! The decisions are pure so the whole matrix is unit-tested without
//! Docker; the session manager applies them.

use std::time::{Duration, Instant};

use super::EndReason;
use crate::computer_use_orchestrator::ThreatLevel;

/// The inputs both decisions read.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub now: Instant,
    pub deadline: Instant,
    /// Last activity (tool call, approval wait, viewer, takeover).
    pub activity: Instant,
    pub idle_timeout: Duration,
    /// Zero = no keep-alive (the pre-P8 behaviour).
    pub keep_alive: Duration,
    /// When the container was paused, if it is.
    pub frozen_since: Option<Instant>,
    pub stopped: bool,
    pub threat: ThreatLevel,
}

/// What an arriving operation (tool call or viewer request) finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnOp {
    /// Running and alive: go ahead.
    Alive,
    /// Paused and still within its keep-alive window: resume, then go ahead.
    Resume,
    /// End the session.
    End(EndReason),
}

/// What one reaper pass does with a session it could lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnReap {
    Keep,
    /// Idle past the timeout with keep-alive on: pause the container.
    Freeze,
    End(EndReason),
}

/// The reasons that end a session whatever its pause state. Order: stop
/// flag, threat RED, hard deadline (as [`super::end_reason`]).
fn hard_end(c: &Clock) -> Option<EndReason> {
    if c.stopped {
        return Some(EndReason::Stopped);
    }
    if c.threat == ThreatLevel::Red {
        return Some(EndReason::ThreatRed);
    }
    if c.now >= c.deadline {
        return Some(EndReason::Deadline);
    }
    None
}

/// Whether a paused session has used up its keep-alive window.
fn frozen_too_long(c: &Clock, since: Instant) -> bool {
    c.now.saturating_duration_since(since) >= c.keep_alive
}

/// Whether a running session is idle beyond any window it may still get.
fn idle_beyond(c: &Clock) -> (bool, bool) {
    let idle = c.now.saturating_duration_since(c.activity);
    let past_timeout = idle >= c.idle_timeout;
    let past_keep_alive = idle >= c.idle_timeout.saturating_add(c.keep_alive);
    (past_timeout, past_keep_alive)
}

/// [`OnOp`] for an operation arriving now.
pub fn on_op(c: &Clock) -> OnOp {
    if let Some(reason) = hard_end(c) {
        return OnOp::End(reason);
    }
    if let Some(since) = c.frozen_since {
        // A paused session with keep-alive switched off since it was paused
        // (zero window) ends instead of resuming.
        return if frozen_too_long(c, since) {
            OnOp::End(EndReason::Idle)
        } else {
            OnOp::Resume
        };
    }
    let (past_timeout, past_keep_alive) = idle_beyond(c);
    if past_timeout && (c.keep_alive.is_zero() || past_keep_alive) {
        return OnOp::End(EndReason::Idle);
    }
    OnOp::Alive
}

/// [`OnReap`] for one reaper pass.
pub fn on_reap(c: &Clock) -> OnReap {
    if let Some(reason) = hard_end(c) {
        return OnReap::End(reason);
    }
    if let Some(since) = c.frozen_since {
        return if frozen_too_long(c, since) {
            OnReap::End(EndReason::Idle)
        } else {
            OnReap::Keep
        };
    }
    let (past_timeout, past_keep_alive) = idle_beyond(c);
    if !past_timeout {
        return OnReap::Keep;
    }
    if c.keep_alive.is_zero() || past_keep_alive {
        return OnReap::End(EndReason::Idle);
    }
    OnReap::Freeze
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock(now: Instant) -> Clock {
        Clock {
            now,
            deadline: now + Duration::from_secs(3600),
            activity: now,
            idle_timeout: Duration::from_secs(120),
            keep_alive: Duration::from_secs(600),
            frozen_since: None,
            stopped: false,
            threat: ThreatLevel::Green,
        }
    }

    #[test]
    fn without_keep_alive_idle_still_ends_the_session() {
        let base = Instant::now();
        let mut c = clock(base + Duration::from_secs(121));
        c.activity = base;
        c.keep_alive = Duration::ZERO;
        assert_eq!(on_reap(&c), OnReap::End(EndReason::Idle));
        assert_eq!(on_op(&c), OnOp::End(EndReason::Idle));
    }

    #[test]
    fn idle_with_keep_alive_freezes_and_a_call_resumes() {
        let base = Instant::now();
        let mut c = clock(base + Duration::from_secs(121));
        c.activity = base;
        assert_eq!(on_reap(&c), OnReap::Freeze);
        // An op arriving before the reaper ran is just alive.
        assert_eq!(on_op(&c), OnOp::Alive);
        c.frozen_since = Some(base + Duration::from_secs(121));
        c.now = base + Duration::from_secs(300);
        assert_eq!(on_reap(&c), OnReap::Keep);
        assert_eq!(on_op(&c), OnOp::Resume);
    }

    #[test]
    fn a_paused_session_ends_when_its_keep_alive_window_runs_out() {
        let base = Instant::now();
        let mut c = clock(base + Duration::from_secs(721));
        c.activity = base;
        c.frozen_since = Some(base + Duration::from_secs(120));
        assert_eq!(on_reap(&c), OnReap::End(EndReason::Idle));
        assert_eq!(on_op(&c), OnOp::End(EndReason::Idle));
        // Keep-alive switched off while paused: no resume either.
        c.now = base + Duration::from_secs(130);
        c.keep_alive = Duration::ZERO;
        assert_eq!(on_op(&c), OnOp::End(EndReason::Idle));
    }

    #[test]
    fn a_running_session_never_reaped_ends_past_both_windows() {
        let base = Instant::now();
        let mut c = clock(base + Duration::from_secs(721));
        c.activity = base;
        assert_eq!(on_reap(&c), OnReap::End(EndReason::Idle));
        assert_eq!(on_op(&c), OnOp::End(EndReason::Idle));
    }

    #[test]
    fn deadline_threat_and_stop_win_over_pause_state() {
        let base = Instant::now();
        for frozen in [None, Some(base)] {
            let mut c = clock(base + Duration::from_secs(10));
            c.frozen_since = frozen;
            c.deadline = base;
            assert_eq!(on_reap(&c), OnReap::End(EndReason::Deadline));
            assert_eq!(on_op(&c), OnOp::End(EndReason::Deadline));
            c.threat = ThreatLevel::Red;
            assert_eq!(on_op(&c), OnOp::End(EndReason::ThreatRed));
            c.stopped = true;
            assert_eq!(on_reap(&c), OnReap::End(EndReason::Stopped));
        }
    }

    #[test]
    fn yellow_does_not_end_or_freeze_an_active_session() {
        let base = Instant::now();
        let mut c = clock(base + Duration::from_secs(5));
        c.activity = base;
        c.threat = ThreatLevel::Yellow;
        assert_eq!(on_reap(&c), OnReap::Keep);
        assert_eq!(on_op(&c), OnOp::Alive);
    }
}
