//! Session supervision — periodic health checks + bookkeeping. Phase 1 ships
//! the minimal contract; Phase 2 wires it into the pool's eviction logic.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tracing::{info, warn};

use crate::session::PtySession;

#[derive(Debug, Clone, Copy)]
pub enum RestartPolicy {
    /// Replace unhealthy sessions on next acquire (Phase 1 default).
    OnNextAcquire,
    /// Phase 2: eagerly restart in the background.
    Eager,
}

pub struct Supervisor {
    sessions: DashMap<String, Arc<PtySession>>,
    pub policy: RestartPolicy,
    pub probe_interval: Duration,
}

impl Supervisor {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            sessions: DashMap::new(),
            policy,
            probe_interval: Duration::from_secs(30),
        }
    }

    pub fn track(&self, key: String, session: Arc<PtySession>) {
        self.sessions.insert(key, session);
    }

    pub fn untrack(&self, key: &str) -> Option<Arc<PtySession>> {
        self.sessions.remove(key).map(|(_, v)| v)
    }

    /// Return ids of currently-unhealthy sessions. Caller decides what to do
    /// with them (Phase 2 pool re-spawns).
    pub fn unhealthy(&self) -> Vec<String> {
        self.sessions
            .iter()
            .filter_map(|entry| {
                if entry.value().is_healthy() {
                    None
                } else {
                    Some(entry.key().clone())
                }
            })
            .collect()
    }

    /// Drop sessions matching a predicate. Returns the dropped count.
    pub async fn evict_where<F>(&self, mut pred: F) -> usize
    where
        F: FnMut(&str, &PtySession) -> bool,
    {
        let mut to_drop = Vec::new();
        for entry in self.sessions.iter() {
            if pred(entry.key(), entry.value()) {
                to_drop.push(entry.key().clone());
            }
        }
        let mut dropped = 0;
        for key in to_drop {
            if let Some((_, session)) = self.sessions.remove(&key) {
                info!(key = %key, "supervisor: evicting session");
                session.shutdown().await;
                dropped += 1;
            }
        }
        if dropped > 0 {
            warn!(count = dropped, "supervisor evicted sessions");
        }
        dropped
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new(RestartPolicy::OnNextAcquire)
    }
}
