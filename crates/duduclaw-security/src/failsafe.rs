//! Failsafe graceful degradation state machine.
//!
//! Instead of binary on/off, provides a 5-level degradation spectrum
//! inspired by the [FAILSAFE.md](https://failsafe.md/) standard (2026-03).
//!
//! Levels: L0 Normal → L1 Degraded → L2 Restricted → L3 Muted → L4 Halted

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::killswitch::FailsafeConfig;

/// Failsafe degradation level (ordered from least to most severe).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FailsafeLevel {
    /// Full functionality — no restrictions.
    L0Normal = 0,
    /// Degraded — rate-limited, prefer local model if available.
    L1Degraded = 1,
    /// Restricted — only canned/safe replies, admin notified.
    L2Restricted = 2,
    /// Muted — silently drop messages, only audit log.
    L3Muted = 3,
    /// Halted — reply with "service paused" message, fully stopped.
    L4Halted = 4,
}

impl FailsafeLevel {
    /// Return the next more severe level, or self if already at max.
    pub fn escalate(self) -> Self {
        match self {
            Self::L0Normal => Self::L1Degraded,
            Self::L1Degraded => Self::L2Restricted,
            Self::L2Restricted => Self::L3Muted,
            Self::L3Muted => Self::L4Halted,
            Self::L4Halted => Self::L4Halted,
        }
    }

    /// Return the next less severe level, or self if already at min.
    pub fn deescalate(self) -> Self {
        match self {
            Self::L0Normal => Self::L0Normal,
            Self::L1Degraded => Self::L0Normal,
            Self::L2Restricted => Self::L1Degraded,
            Self::L3Muted => Self::L2Restricted,
            Self::L4Halted => Self::L3Muted,
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::L0Normal => "Normal",
            Self::L1Degraded => "Degraded",
            Self::L2Restricted => "Restricted",
            Self::L3Muted => "Muted",
            Self::L4Halted => "Halted",
        }
    }

    /// Whether this level blocks all message processing (L3+).
    pub fn blocks_processing(&self) -> bool {
        matches!(self, Self::L3Muted | Self::L4Halted)
    }

    /// Whether this level should reply with a canned message.
    pub fn uses_canned_reply(&self) -> bool {
        matches!(self, Self::L2Restricted | Self::L4Halted)
    }
}

impl std::fmt::Display for FailsafeLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "L{} ({})", *self as u8, self.label())
    }
}

/// Per-scope failsafe state.
#[derive(Debug, Clone)]
pub struct FailsafeState {
    /// Current degradation level.
    pub level: FailsafeLevel,
    /// When the current level was entered.
    pub since: Instant,
    /// Reason for the current level.
    pub reason: String,
    /// When to auto-recover to L0 (None = manual only).
    pub auto_recover_at: Option<Instant>,
}

impl FailsafeState {
    fn normal() -> Self {
        Self {
            level: FailsafeLevel::L0Normal,
            since: Instant::now(),
            reason: String::new(),
            auto_recover_at: None,
        }
    }
}

/// Manages failsafe state for all scopes.
pub struct FailsafeManager {
    states: Arc<RwLock<HashMap<String, FailsafeState>>>,
    config: FailsafeConfig,
}

impl FailsafeManager {
    /// Create a new failsafe manager with the given config.
    pub fn new(config: FailsafeConfig) -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Get the current failsafe level for a scope.
    ///
    /// Also checks and applies auto-recovery if the timer has elapsed.
    pub async fn get_level(&self, scope: &str) -> FailsafeLevel {
        let mut map = self.states.write().await;
        if let Some(state) = map.get(scope) {
            // Check auto-recovery
            if let Some(recover_at) = state.auto_recover_at {
                if Instant::now() >= recover_at {
                    info!(scope, from = %state.level, "Failsafe auto-recovering to L0");
                    map.remove(scope);
                    return FailsafeLevel::L0Normal;
                }
            }
            state.level
        } else {
            FailsafeLevel::L0Normal
        }
    }

    /// Get full state info for a scope (for status display).
    ///
    /// Also checks and applies auto-recovery, so the returned state
    /// is always current.
    pub async fn get_state(&self, scope: &str) -> Option<FailsafeState> {
        let mut map = self.states.write().await;
        if let Some(state) = map.get_mut(scope) {
            // Check auto-recovery before returning
            if let Some(recover_at) = state.auto_recover_at {
                if Instant::now() >= recover_at {
                    info!(scope, from = %state.level, "Failsafe auto-recovering to L0 (via get_state)");
                    map.remove(scope);
                    return None;
                }
            }
            // Only return non-normal states
            if state.level == FailsafeLevel::L0Normal {
                None
            } else {
                Some(state.clone())
            }
        } else {
            None
        }
    }

    /// Escalate the failsafe level by one step.
    ///
    /// Returns the new level after escalation.
    pub async fn escalate(&self, scope: &str, reason: &str) -> FailsafeLevel {
        let mut map = self.states.write().await;
        let current = map
            .get(scope)
            .map(|s| s.level)
            .unwrap_or(FailsafeLevel::L0Normal);

        let new_level = current.escalate();
        let auto_recover = self.auto_recover_duration(new_level);

        warn!(
            scope,
            from = %current,
            to = %new_level,
            reason,
            "Failsafe escalated"
        );

        map.insert(
            scope.to_string(),
            FailsafeState {
                level: new_level,
                since: Instant::now(),
                reason: reason.to_string(),
                auto_recover_at: auto_recover.map(|d| Instant::now() + d),
            },
        );

        new_level
    }

    /// De-escalate the failsafe level by one step.
    pub async fn deescalate(&self, scope: &str) -> FailsafeLevel {
        let mut map = self.states.write().await;
        let current = map
            .get(scope)
            .map(|s| s.level)
            .unwrap_or(FailsafeLevel::L0Normal);

        let new_level = current.deescalate();
        if new_level == FailsafeLevel::L0Normal {
            map.remove(scope);
        } else {
            if let Some(state) = map.get_mut(scope) {
                state.level = new_level;
                state.since = Instant::now();
                state.auto_recover_at = self
                    .auto_recover_duration(new_level)
                    .map(|d| Instant::now() + d);
            }
        }

        info!(scope, from = %current, to = %new_level, "Failsafe de-escalated");
        new_level
    }

    /// Set a specific failsafe level (for explicit control).
    pub async fn set_level(&self, scope: &str, level: FailsafeLevel, reason: &str) {
        let mut map = self.states.write().await;

        if level == FailsafeLevel::L0Normal {
            map.remove(scope);
            info!(scope, "Failsafe reset to L0 Normal");
            return;
        }

        let auto_recover = self.auto_recover_duration(level);
        map.insert(
            scope.to_string(),
            FailsafeState {
                level,
                since: Instant::now(),
                reason: reason.to_string(),
                auto_recover_at: auto_recover.map(|d| Instant::now() + d),
            },
        );
        warn!(scope, level = %level, reason, "Failsafe level set");
    }

    /// Force halt — immediately set to L4.
    pub async fn force_halt(&self, scope: &str, reason: &str) {
        self.set_level(scope, FailsafeLevel::L4Halted, reason).await;
    }

    /// Resume — immediately return to L0 Normal.
    pub async fn resume(&self, scope: &str) {
        self.set_level(scope, FailsafeLevel::L0Normal, "manual resume")
            .await;
    }

    /// Get the appropriate canned reply for the current level.
    pub fn canned_reply(&self, level: FailsafeLevel) -> Option<&str> {
        match level {
            FailsafeLevel::L2Restricted => Some(&self.config.default_restricted_reply),
            FailsafeLevel::L4Halted => Some(&self.config.default_halted_reply),
            _ => None,
        }
    }

    /// Get auto-recovery duration for a given level.
    fn auto_recover_duration(&self, level: FailsafeLevel) -> Option<Duration> {
        let secs = match level {
            FailsafeLevel::L1Degraded => self.config.l1_auto_recover_secs,
            FailsafeLevel::L2Restricted => self.config.l2_auto_recover_secs,
            FailsafeLevel::L3Muted => self.config.l3_auto_recover_secs,
            _ => 0,
        };
        if secs > 0 {
            Some(Duration::from_secs(secs))
        } else {
            None
        }
    }

    /// List all scopes currently in a non-normal state.
    pub async fn active_states(&self) -> Vec<(String, FailsafeState)> {
        let map = self.states.read().await;
        map.iter()
            .filter(|(_, s)| s.level != FailsafeLevel::L0Normal)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// Format a human-readable status message for a scope.
pub fn format_status(scope: &str, state: Option<&FailsafeState>) -> String {
    match state {
        Some(s) => {
            let elapsed = Instant::now().duration_since(s.since);
            let auto_recover = s.auto_recover_at.map(|at| {
                let remaining = at.saturating_duration_since(Instant::now());
                format!(", auto-recover in {}s", remaining.as_secs())
            }).unwrap_or_default();
            format!(
                "Scope: {scope}\nLevel: {}\nSince: {}s ago\nReason: {}{auto_recover}",
                s.level,
                elapsed.as_secs(),
                s.reason,
            )
        }
        None => format!("Scope: {scope}\nLevel: L0 (Normal)\nAll systems operational."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FailsafeConfig {
        FailsafeConfig {
            l1_auto_recover_secs: 1, // 1s for testing
            l2_auto_recover_secs: 2,
            l3_auto_recover_secs: 0, // manual only
            default_restricted_reply: "Limited.".to_string(),
            default_halted_reply: "Paused.".to_string(),
        }
    }

    #[test]
    fn level_ordering() {
        assert!(FailsafeLevel::L0Normal < FailsafeLevel::L1Degraded);
        assert!(FailsafeLevel::L1Degraded < FailsafeLevel::L4Halted);
    }

    #[test]
    fn escalate_progression() {
        assert_eq!(FailsafeLevel::L0Normal.escalate(), FailsafeLevel::L1Degraded);
        assert_eq!(FailsafeLevel::L1Degraded.escalate(), FailsafeLevel::L2Restricted);
        assert_eq!(FailsafeLevel::L2Restricted.escalate(), FailsafeLevel::L3Muted);
        assert_eq!(FailsafeLevel::L3Muted.escalate(), FailsafeLevel::L4Halted);
        assert_eq!(FailsafeLevel::L4Halted.escalate(), FailsafeLevel::L4Halted);
    }

    #[test]
    fn deescalate_progression() {
        assert_eq!(FailsafeLevel::L4Halted.deescalate(), FailsafeLevel::L3Muted);
        assert_eq!(FailsafeLevel::L3Muted.deescalate(), FailsafeLevel::L2Restricted);
        assert_eq!(FailsafeLevel::L0Normal.deescalate(), FailsafeLevel::L0Normal);
    }

    #[test]
    fn blocks_processing() {
        assert!(!FailsafeLevel::L0Normal.blocks_processing());
        assert!(!FailsafeLevel::L1Degraded.blocks_processing());
        assert!(!FailsafeLevel::L2Restricted.blocks_processing());
        assert!(FailsafeLevel::L3Muted.blocks_processing());
        assert!(FailsafeLevel::L4Halted.blocks_processing());
    }

    #[tokio::test]
    async fn starts_normal() {
        let mgr = FailsafeManager::new(test_config());
        assert_eq!(mgr.get_level("scope1").await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn escalate_and_deescalate() {
        let mgr = FailsafeManager::new(test_config());
        let l = mgr.escalate("s1", "test").await;
        assert_eq!(l, FailsafeLevel::L1Degraded);

        let l = mgr.escalate("s1", "still bad").await;
        assert_eq!(l, FailsafeLevel::L2Restricted);

        let l = mgr.deescalate("s1").await;
        assert_eq!(l, FailsafeLevel::L1Degraded);
    }

    #[tokio::test]
    async fn force_halt() {
        let mgr = FailsafeManager::new(test_config());
        mgr.force_halt("s1", "emergency").await;
        assert_eq!(mgr.get_level("s1").await, FailsafeLevel::L4Halted);
    }

    #[tokio::test]
    async fn resume_resets() {
        let mgr = FailsafeManager::new(test_config());
        mgr.force_halt("s1", "emergency").await;
        mgr.resume("s1").await;
        assert_eq!(mgr.get_level("s1").await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn auto_recovery() {
        let mgr = FailsafeManager::new(test_config());
        mgr.escalate("s1", "test").await; // L1, auto-recover in 1s
        assert_eq!(mgr.get_level("s1").await, FailsafeLevel::L1Degraded);

        // Wait for auto-recovery
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(mgr.get_level("s1").await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn canned_replies() {
        let mgr = FailsafeManager::new(test_config());
        assert_eq!(mgr.canned_reply(FailsafeLevel::L2Restricted), Some("Limited."));
        assert_eq!(mgr.canned_reply(FailsafeLevel::L4Halted), Some("Paused."));
        assert_eq!(mgr.canned_reply(FailsafeLevel::L0Normal), None);
        assert_eq!(mgr.canned_reply(FailsafeLevel::L3Muted), None);
    }

    #[tokio::test]
    async fn independent_scopes() {
        let mgr = FailsafeManager::new(test_config());
        mgr.force_halt("s1", "test").await;
        assert_eq!(mgr.get_level("s1").await, FailsafeLevel::L4Halted);
        assert_eq!(mgr.get_level("s2").await, FailsafeLevel::L0Normal);
    }

    #[tokio::test]
    async fn global_halt_dominates_scope() {
        let mgr = FailsafeManager::new(test_config());
        // Set global to halted, scope to normal
        mgr.force_halt("__global__", "emergency").await;
        // Scope is normal
        assert_eq!(mgr.get_level("scope1").await, FailsafeLevel::L0Normal);
        // But effective level should be max(global, scope)
        let global = mgr.get_level("__global__").await;
        let scope = mgr.get_level("scope1").await;
        let effective = std::cmp::max(global, scope);
        assert_eq!(effective, FailsafeLevel::L4Halted);
    }

    #[tokio::test]
    async fn resume_scope_does_not_affect_global() {
        let mgr = FailsafeManager::new(test_config());
        mgr.force_halt("__global__", "emergency").await;
        mgr.force_halt("scope1", "also halted").await;

        // Resume only scope1
        mgr.resume("scope1").await;
        assert_eq!(mgr.get_level("scope1").await, FailsafeLevel::L0Normal);
        // Global should still be halted
        assert_eq!(mgr.get_level("__global__").await, FailsafeLevel::L4Halted);
    }

    #[tokio::test]
    async fn get_state_returns_none_after_auto_recovery() {
        let mgr = FailsafeManager::new(test_config());
        mgr.escalate("s1", "test").await; // L1, auto-recover in 1s
        assert!(mgr.get_state("s1").await.is_some());

        tokio::time::sleep(Duration::from_millis(1100)).await;
        // After auto-recovery, get_state should return None
        assert!(mgr.get_state("s1").await.is_none());
    }

    #[test]
    fn format_status_normal() {
        let msg = format_status("test-scope", None);
        assert!(msg.contains("Normal"));
        assert!(msg.contains("operational"));
    }

    #[test]
    fn format_status_halted() {
        let state = FailsafeState {
            level: FailsafeLevel::L4Halted,
            since: Instant::now(),
            reason: "circuit breaker tripped".to_string(),
            auto_recover_at: None,
        };
        let msg = format_status("test-scope", Some(&state));
        assert!(msg.contains("L4"));
        assert!(msg.contains("circuit breaker"));
    }
}
