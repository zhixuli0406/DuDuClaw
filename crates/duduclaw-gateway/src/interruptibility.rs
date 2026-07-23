//! OS-native P2-1: interruptibility (cost-of-interruption) scoring.
//!
//! Estimates how costly it is to interrupt the user *right now*, as a single
//! `0.0..=1.0` score where **0.0 = safe to interrupt** and **1.0 = do not
//! disturb**. The [`ProactiveGate`](crate::proactive_gate) reads this score to
//! raise its proactive-intervention threshold when the user is busy (Horvitz
//! CHI'99: "factor the user's attentional state into the timing of service").
//!
//! ## Signals — pure OS interaction, no wearables
//!
//! CHI'18 *Sensing Interruptibility in the Office* found that plain
//! computer-interaction data (window focus / keyboard-mouse / idle) predicts
//! interruptibility at 74.8% — **better than physiological sensors** — and that
//! **application/window switching frequency is the single strongest signal**.
//! We use exactly the signals the P2-4 sensing layer already emits onto the
//! autopilot broadcast, so this module adds *zero* new sensing:
//!
//! - **frontmost switches** (`os_frontmost` events) — strongest weight.
//! - **file-event density** (`os_file` events) — secondary activity signal.
//! - **idle** (`agent_idle` events) — a *relief* signal.
//!
//! ## P2 implementation rule #1 — single idle judgment source
//!
//! This tracker NEVER computes idle itself. It only *reads* the existing
//! `AgentIdle` signal that the heartbeat scheduler already emits onto the same
//! broadcast — consuming an existing event is not "opening a second idle
//! source". If no `agent_idle` event is seen, idle simply doesn't factor in.
//!
//! ## No signal → neutral
//!
//! An agent with no frontmost/file/idle observation in the window scores a
//! neutral `0.5` (and logs the signal gap at debug), so a fresh gateway with no
//! OS sensing wired never biases the gate either way.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::autopilot_engine::AutopilotEvent;

/// Sliding window over which activity is counted. 15 minutes matches the
/// attention-window granularity used by the CHI'18 field study and is short
/// enough that a burst of activity decays back to "interruptible" quickly.
pub const WINDOW: Duration = Duration::from_secs(15 * 60);

/// Frontmost-switch count (within [`WINDOW`]) that saturates the switch
/// component to 1.0. ~12 app/window switches in 15 min ≈ heavy context
/// switching. Strongest signal per CHI'18, hence the highest weight below.
const SWITCH_SATURATION: f32 = 12.0;

/// File-event count (within [`WINDOW`]) that saturates the file component.
/// Higher than switches: file churn is noisier and a single save can emit
/// several FSEvents, so it takes more of them to imply "busy".
const FILE_SATURATION: f32 = 20.0;

/// Idle minutes at which relief saturates to 1.0 (fully interruptible). Ten
/// minutes of no interaction is a strong "stepped away / paused" signal.
const IDLE_FULL_MINUTES: f32 = 10.0;

/// Weight of the frontmost-switch component — the strongest interruptibility
/// signal in the CHI'18 study, so it dominates the activity estimate.
const W_SWITCH: f32 = 0.6;

/// Weight of the file-event component — secondary activity signal.
const W_FILE: f32 = 0.4;

/// Score returned when the agent has no perception signal at all.
pub const NEUTRAL: f32 = 0.5;

/// Compute the interruptibility score from raw signal counts.
///
/// Pure and deterministic — the unit-testable core of [`InterruptibilityTracker::score`].
///
/// `switches` / `file_events` are counts already restricted to [`WINDOW`].
/// `idle_minutes` is the most recent `agent_idle` observation, if any.
///
/// Returns `0.0..=1.0` (0 = interrupt freely, 1 = do not disturb). Idle acts as
/// a multiplicative *relief*: an idle user has their activity-derived busyness
/// scaled down toward 0 (fully interruptible).
pub fn compute_interruptibility(
    switches: usize,
    file_events: usize,
    idle_minutes: Option<i64>,
) -> f32 {
    let switch_c = (switches as f32 / SWITCH_SATURATION).min(1.0);
    let file_c = (file_events as f32 / FILE_SATURATION).min(1.0);
    let activity = W_SWITCH * switch_c + W_FILE * file_c;
    let score = match idle_minutes {
        Some(m) if m > 0 => {
            let relief = (m as f32 / IDLE_FULL_MINUTES).clamp(0.0, 1.0);
            activity * (1.0 - relief)
        }
        // idle == 0 / negative (just-active or absent) → no relief.
        _ => activity,
    };
    score.clamp(0.0, 1.0)
}

/// Per-agent sliding-window state. Timestamps are pruned lazily on record/read.
#[derive(Default)]
struct AgentWindow {
    switches: VecDeque<Instant>,
    file_events: VecDeque<Instant>,
    /// Most recent idle observation: (idle_minutes, observed_at).
    idle: Option<(i64, Instant)>,
}

impl AgentWindow {
    fn prune(&mut self, now: Instant) {
        while self
            .switches
            .front()
            .is_some_and(|t| now.duration_since(*t) > WINDOW)
        {
            self.switches.pop_front();
        }
        while self
            .file_events
            .front()
            .is_some_and(|t| now.duration_since(*t) > WINDOW)
        {
            self.file_events.pop_front();
        }
        if let Some((_, at)) = self.idle {
            if now.duration_since(at) > WINDOW {
                self.idle = None;
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.switches.is_empty() && self.file_events.is_empty() && self.idle.is_none()
    }
}

/// Tracks per-agent interruptibility from the autopilot sensing broadcast.
///
/// Interior-mutable (a `std::sync::Mutex` over an in-memory map — never held
/// across an `.await`) so it can be shared as `Arc<InterruptibilityTracker>`
/// between the background ingest task and the [`ProactiveGate`](crate::proactive_gate).
pub struct InterruptibilityTracker {
    agents: Mutex<HashMap<String, AgentWindow>>,
}

impl Default for InterruptibilityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl InterruptibilityTracker {
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
        }
    }

    /// Record a frontmost app/window switch for `agent_id` at `now`.
    pub fn record_switch(&self, agent_id: &str, now: Instant) {
        let mut map = self.agents.lock().unwrap();
        let w = map.entry(agent_id.to_string()).or_default();
        w.prune(now);
        w.switches.push_back(now);
    }

    /// Record a filesystem event for `agent_id` at `now`.
    pub fn record_file_event(&self, agent_id: &str, now: Instant) {
        let mut map = self.agents.lock().unwrap();
        let w = map.entry(agent_id.to_string()).or_default();
        w.prune(now);
        w.file_events.push_back(now);
    }

    /// Record an idle observation for `agent_id`. This only *reads* the existing
    /// heartbeat `agent_idle` signal — the tracker never derives idle itself.
    pub fn record_idle(&self, agent_id: &str, idle_minutes: i64, now: Instant) {
        let mut map = self.agents.lock().unwrap();
        let w = map.entry(agent_id.to_string()).or_default();
        w.prune(now);
        w.idle = Some((idle_minutes, now));
    }

    /// Route one autopilot event into the appropriate window. Non-sensing
    /// events are ignored. Idle is consumed from the existing `AgentIdle`
    /// variant (rule #1 — no second idle source).
    pub fn note_event(&self, event: &AutopilotEvent, now: Instant) {
        match event {
            AutopilotEvent::OsFrontmostEvent { agent_id, .. } => self.record_switch(agent_id, now),
            AutopilotEvent::OsFileEvent { agent_id, .. } => self.record_file_event(agent_id, now),
            AutopilotEvent::AgentIdle {
                agent_id,
                idle_minutes,
            } => self.record_idle(agent_id, *idle_minutes, now),
            _ => {}
        }
    }

    /// Current interruptibility score for `agent_id` (0 = interrupt freely,
    /// 1 = do not disturb). Returns [`NEUTRAL`] when there is no signal.
    pub fn score(&self, agent_id: &str) -> f32 {
        let now = Instant::now();
        let mut map = self.agents.lock().unwrap();
        let Some(w) = map.get_mut(agent_id) else {
            debug!(agent = %agent_id, "interruptibility: no window for agent → neutral");
            return NEUTRAL;
        };
        w.prune(now);
        if w.is_empty() {
            debug!(
                agent = %agent_id,
                "interruptibility: no perception signal in window → neutral"
            );
            return NEUTRAL;
        }
        compute_interruptibility(
            w.switches.len(),
            w.file_events.len(),
            w.idle.map(|(m, _)| m),
        )
    }

    /// Spawn a background task that feeds `rx` into this tracker. The tracker is
    /// shared (`Arc`) with the [`ProactiveGate`](crate::proactive_gate) so the
    /// gate reads a live score. Lagged events are tolerated (interruptibility is
    /// a soft estimate, missing a few switches only under-counts activity).
    pub fn spawn(
        self: std::sync::Arc<Self>,
        mut rx: broadcast::Receiver<AutopilotEvent>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => self.note_event(&event, Instant::now()),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        debug!(dropped = n, "interruptibility: broadcast lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        warn!("interruptibility: broadcast closed — ingest task stopping");
                        break;
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_signal_is_neutral() {
        assert_eq!(compute_interruptibility(0, 0, None), 0.0);
        // But the tracker returns NEUTRAL when the window is genuinely empty.
        let t = InterruptibilityTracker::new();
        assert_eq!(t.score("nobody"), NEUTRAL);
    }

    #[test]
    fn switch_component_dominates_file() {
        // Same saturation fraction: switches (weight .6) must outweigh files (.4).
        let by_switch = compute_interruptibility(12, 0, None); // full switch → .6
        let by_file = compute_interruptibility(0, 20, None); // full file → .4
        assert!((by_switch - 0.6).abs() < 1e-4, "got {by_switch}");
        assert!((by_file - 0.4).abs() < 1e-4, "got {by_file}");
        assert!(by_switch > by_file);
    }

    #[test]
    fn fully_busy_saturates_to_one() {
        let s = compute_interruptibility(100, 100, None);
        assert!((s - 1.0).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn idle_relieves_busyness() {
        // Busy (full activity) but idle 10 min → fully interruptible.
        let busy_idle = compute_interruptibility(12, 20, Some(10));
        assert!(
            busy_idle.abs() < 1e-4,
            "10-min idle should relieve to ~0, got {busy_idle}"
        );
        // Half idle → half relief.
        let half = compute_interruptibility(12, 0, Some(5)); // activity .6 * (1-.5)=.3
        assert!((half - 0.3).abs() < 1e-4, "got {half}");
        // idle == 0 → no relief.
        let none = compute_interruptibility(12, 0, Some(0));
        assert!((none - 0.6).abs() < 1e-4, "got {none}");
    }

    #[test]
    fn window_prunes_old_events() {
        let t = InterruptibilityTracker::new();
        let base = Instant::now();
        // An event older than the window must not count.
        let old = base.checked_sub(WINDOW + Duration::from_secs(1)).unwrap();
        t.record_switch("a", old);
        // Only-old → window empty → neutral.
        assert_eq!(t.score("a"), NEUTRAL);
    }

    #[test]
    fn recent_switches_count() {
        let t = InterruptibilityTracker::new();
        let now = Instant::now();
        for _ in 0..6 {
            t.record_switch("a", now);
        }
        // 6/12 switches → switch_c .5 → activity .6*.5 = .3
        let s = t.score("a");
        assert!((s - 0.3).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn idle_event_routes_and_relieves() {
        let t = InterruptibilityTracker::new();
        let now = Instant::now();
        for _ in 0..12 {
            t.record_switch("a", now);
        }
        t.note_event(
            &AutopilotEvent::AgentIdle {
                agent_id: "a".into(),
                idle_minutes: 10,
            },
            now,
        );
        // Full switch busyness relieved by 10-min idle → ~0.
        assert!(t.score("a").abs() < 1e-4);
    }

    #[test]
    fn non_sensing_events_ignored() {
        let t = InterruptibilityTracker::new();
        let now = Instant::now();
        t.note_event(&AutopilotEvent::CronTick { now: "x".into() }, now);
        assert_eq!(t.score("a"), NEUTRAL);
    }
}
