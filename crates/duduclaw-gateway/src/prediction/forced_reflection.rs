//! ForcedReflection — silence-breaker hook into the evolution loop.
//!
//! When [`duduclaw_agent::SilenceBreakerEvent`] fires (the gateway has not
//! seen any prediction-error trigger for the agent within
//! `evolution.max_silence_hours`), the gateway invokes
//! [`fire_forced_reflection`] to:
//!
//! 1. Append a typed `silence_breaker` row to `prediction.db.evolution_events`
//!    so the absence of organic triggers is itself observable.
//! 2. Apply a per-agent **4-hour cooldown** so the heartbeat scheduler can't
//!    cause the same agent to fire repeatedly during e.g. a long downtime.
//!
//! Triggering an actual GVU loop here would require ferrying a great deal of
//! per-agent state (SOUL.md path, contract, mistake notebook, LLM caller…)
//! out of `channel_reply` and into a free-standing helper. The P0 scope is
//! "make silence visible and stop it pretending"; the deeper hook into GVU
//! is left to the natural next-conversation path, where all the dependencies
//! are already wired.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::prediction::engine::PredictionEngine;

/// In-process cooldown table keyed by agent_id.
///
/// Mirrors the scheduler-level reset of `last_evolution_trigger`, but adds an
/// independent guard for the gateway-side handler so a buggy heartbeat
/// (or a manual `trigger`) can't bypass the cool-down.
pub struct SilenceBreakerCooldown {
    last_fire: Mutex<HashMap<String, DateTime<Utc>>>,
    cooldown: Duration,
}

impl SilenceBreakerCooldown {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_fire: Mutex::new(HashMap::new()),
            cooldown,
        }
    }

    /// Default: 4-hour cooldown.
    pub fn default_4h() -> Self {
        Self::new(Duration::from_secs(4 * 3600))
    }

    /// Returns `true` and records `now` if the cooldown is clear; otherwise
    /// returns `false` without changing state.
    pub async fn try_acquire(&self, agent_id: &str, now: DateTime<Utc>) -> bool {
        let mut guard = self.last_fire.lock().await;
        if let Some(prev) = guard.get(agent_id) {
            let elapsed = now.signed_duration_since(*prev).num_seconds();
            if elapsed >= 0 && (elapsed as u64) < self.cooldown.as_secs() {
                return false;
            }
        }
        guard.insert(agent_id.to_owned(), now);
        true
    }
}

impl Default for SilenceBreakerCooldown {
    fn default() -> Self {
        Self::default_4h()
    }
}

/// Drive a single silence-breaker firing to its evolution-side effects.
///
/// Returns `true` if the event was consumed, `false` if the cooldown
/// rejected it (so the caller can update metrics / surface a reason).
pub async fn fire_forced_reflection(
    cooldown: &SilenceBreakerCooldown,
    prediction_engine: &Arc<PredictionEngine>,
    agent_id: &str,
    hours: f64,
    timestamp: DateTime<Utc>,
) -> bool {
    if !cooldown.try_acquire(agent_id, timestamp).await {
        debug!(
            agent = agent_id,
            "Silence breaker suppressed by 4h cooldown (hours_since={hours:.1})"
        );
        return false;
    }

    let context = format!(
        "Silence breaker: no evolution trigger for {hours:.1}h. \
         Agent has been quiet — schedule a self-reflection on the next \
         conversation if any pending mistakes need addressing."
    );
    prediction_engine.log_evolution_event(
        "silence_breaker",
        agent_id,
        None,
        None,
        Some(&context),
        None,
        None,
    );
    info!(
        target: "forced_reflection",
        agent = agent_id,
        hours = format!("{hours:.1}"),
        "Forced reflection event emitted"
    );
    true
}

/// Spawn the receiver loop that consumes
/// [`duduclaw_agent::SilenceBreakerEvent`]s emitted by the heartbeat
/// scheduler and turns each one into a forced reflection event.
///
/// `cooldown` is shared so all fires through the same gateway use the same
/// per-agent cool-down state.
pub fn spawn_silence_event_consumer(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<duduclaw_agent::SilenceBreakerEvent>,
    prediction_engine: Arc<PredictionEngine>,
    cooldown: Arc<SilenceBreakerCooldown>,
) {
    tokio::spawn(async move {
        info!("SilenceBreaker consumer started");
        while let Some(event) = rx.recv().await {
            let fired = fire_forced_reflection(
                cooldown.as_ref(),
                &prediction_engine,
                &event.agent_id,
                event.hours,
                event.timestamp,
            )
            .await;
            if !fired {
                // Cool-down rejected — informational only.
                warn!(
                    agent = %event.agent_id,
                    "Silence breaker event ignored (cool-down active)"
                );
            }
        }
        warn!("SilenceBreaker consumer channel closed — exiting");
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn make_engine(tmp: &TempDir) -> Arc<PredictionEngine> {
        let db = tmp.path().join("prediction.db");
        let meta = tmp.path().join("metacognition.json");
        Arc::new(PredictionEngine::new(db, Some(meta)))
    }

    fn count_silence_events(db: &std::path::Path, agent: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM evolution_events WHERE agent_id = ?1 AND event_type = 'silence_breaker'",
            rusqlite::params![agent],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    #[tokio::test]
    async fn test_fire_forced_reflection_writes_evolution_event() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let cooldown = SilenceBreakerCooldown::default_4h();

        let fired =
            fire_forced_reflection(&cooldown, &engine, "agent-q", 13.0, Utc::now()).await;
        assert!(fired, "first fire should succeed");

        // The write is non-blocking; give the spawned task a moment to flush.
        for _ in 0..50 {
            if count_silence_events(&tmp.path().join("prediction.db"), "agent-q") > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("silence_breaker event never persisted");
    }

    #[tokio::test]
    async fn test_fire_forced_reflection_respects_cooldown() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let cooldown = SilenceBreakerCooldown::new(Duration::from_secs(3600));

        let now = Utc::now();
        assert!(
            fire_forced_reflection(&cooldown, &engine, "agent-c", 13.0, now).await,
            "first fire ok"
        );
        // 30 minutes later → still within 1h cooldown.
        let later = now + ChronoDuration::minutes(30);
        assert!(
            !fire_forced_reflection(&cooldown, &engine, "agent-c", 1.0, later).await,
            "second fire within cooldown must be rejected"
        );
        // 90 minutes later → cooldown clear.
        let after = now + ChronoDuration::minutes(90);
        assert!(
            fire_forced_reflection(&cooldown, &engine, "agent-c", 1.5, after).await,
            "third fire after cooldown should succeed"
        );
    }

    #[tokio::test]
    async fn test_cooldown_is_per_agent() {
        let cooldown = SilenceBreakerCooldown::new(Duration::from_secs(3600));
        let now = Utc::now();
        assert!(cooldown.try_acquire("a", now).await);
        // Same time, different agent — must be allowed.
        assert!(cooldown.try_acquire("b", now).await);
        // Re-fire on `a` — blocked.
        assert!(!cooldown.try_acquire("a", now).await);
    }
}
