//! Channel-level user access control.
//!
//! Checks whether a user is allowed to interact with an agent, based on
//! `allowed_users`, `blocked_users`, and optional pairing code verification.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use tracing::{info, warn};

/// Result of an access check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessDecision {
    /// User is allowed to interact.
    Allowed,
    /// User is explicitly blocked — silently ignore.
    Blocked,
    /// User must present a pairing code first.
    RequirePairing,
}

/// A pending pairing request.
#[derive(Debug, Clone)]
struct PairingCode {
    code: String,
    expires_at: chrono::DateTime<chrono::Utc>,
    failed_attempts: u32,
}

/// Constant-time string comparison to prevent timing attacks.
fn constant_time_eq_str(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Per-tool authorization for an agent.
#[derive(Debug, Clone)]
pub struct ToolApproval {
    pub tool_name: String,
    pub agent_id: String,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub session_scoped: bool,
    /// Session ID for session-scoped approvals (None for non-session-scoped).
    pub session_id: Option<String>,
}

/// Emergency stop state for all agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmergencyState {
    Normal,
    Stopped,
}

/// Manages user access control and pairing for a single agent.
pub struct AccessController {
    /// Active pairing codes: user_id → PairingCode
    pending_pairings: Arc<RwLock<HashMap<String, PairingCode>>>,
    /// Dynamically approved users — persisted to disk across restarts.
    /// These supplement the static `allowed_users` from agent.toml.
    runtime_allowed: Arc<RwLock<Vec<String>>>,
    /// Per-tool-per-agent approvals
    tool_approvals: Vec<ToolApproval>,
    /// Emergency stop state
    emergency_state: EmergencyState,
    /// Session ID for session-scoped approvals
    current_session_id: Option<String>,
    /// Path to persist runtime_allowed list (None = no persistence)
    persist_path: Option<std::path::PathBuf>,
}

impl AccessController {
    pub fn new() -> Self {
        Self {
            pending_pairings: Arc::new(RwLock::new(HashMap::new())),
            runtime_allowed: Arc::new(RwLock::new(Vec::new())),
            tool_approvals: Vec::new(),
            emergency_state: EmergencyState::Normal,
            current_session_id: None,
            persist_path: None,
        }
    }

    /// Create an AccessController with file-based persistence for runtime approvals.
    ///
    /// On creation, loads any previously persisted approvals from `persist_path`.
    /// All future approvals/revocations are written to that file.
    pub fn with_persistence(persist_path: std::path::PathBuf) -> Self {
        let mut ctrl = Self::new();
        ctrl.persist_path = Some(persist_path.clone());
        // Load previously persisted approvals (best-effort, ignore errors)
        if let Ok(data) = std::fs::read_to_string(&persist_path) {
            if let Ok(users) = serde_json::from_str::<Vec<String>>(&data) {
                if let Ok(mut list) = ctrl.runtime_allowed.try_write() {
                    *list = users;
                }
            }
        }
        ctrl
    }

    /// Persist the current runtime_allowed list to disk (best-effort).
    fn persist_runtime_allowed(&self, list: &[String]) {
        if let Some(path) = &self.persist_path {
            if let Ok(json) = serde_json::to_string(list) {
                // Write to temp file then rename for atomicity
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, &json).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
        }
    }

    /// Check whether `user_id` on `channel` is allowed to interact with the agent.
    pub async fn check_access(
        &self,
        user_id: &str,
        _channel: &str,
        allowed_users: Option<&[String]>,
        blocked_users: &[String],
        require_pairing: bool,
    ) -> AccessDecision {
        // 1. Blocked users are always denied
        if blocked_users.iter().any(|b| b == user_id) {
            return AccessDecision::Blocked;
        }

        // 2. If allowlist exists, check it (plus runtime-approved users)
        if let Some(allowed) = allowed_users {
            if allowed.iter().any(|a| a == user_id) {
                return AccessDecision::Allowed;
            }
            // Check runtime-approved users
            let runtime = self.runtime_allowed.read().await;
            if runtime.iter().any(|a| a == user_id) {
                return AccessDecision::Allowed;
            }
            // Not in allowlist → require pairing or block
            if require_pairing {
                return AccessDecision::RequirePairing;
            }
            return AccessDecision::Blocked;
        }

        // 3. No allowlist — check require_pairing
        if require_pairing {
            let runtime = self.runtime_allowed.read().await;
            if runtime.iter().any(|a| a == user_id) {
                return AccessDecision::Allowed;
            }
            return AccessDecision::RequirePairing;
        }

        // 4. Open access
        AccessDecision::Allowed
    }

    /// Generate a 6-digit pairing code for a user (valid for 5 minutes).
    ///
    /// Returns `None` if the cumulative failed attempt count across all generated
    /// codes has reached the hard limit of 15, preventing brute-force via
    /// repeated regeneration (R4-H1).
    pub async fn generate_pairing_code(&self, user_id: &str) -> Option<String> {
        let mut pairings = self.pending_pairings.write().await;

        // Preserve cumulative attempt count from previous code
        let prev_total = pairings.get(user_id)
            .map(|p| p.failed_attempts)
            .unwrap_or(0);

        // Hard limit: 15 total attempts across all generated codes
        if prev_total >= 15 {
            warn!(user_id, "Pairing code generation blocked — too many cumulative attempts");
            return None;
        }

        // Use UUID-based entropy to avoid adding the `rand` crate
        let uuid = uuid::Uuid::new_v4();
        let num = uuid.as_u128() % 1_000_000;
        let code: String = format!("{num:06}");
        let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);

        pairings.insert(
            user_id.to_string(),
            PairingCode {
                code: code.clone(),
                expires_at,
                failed_attempts: prev_total, // carry over, don't reset to 0
            },
        );

        info!(user_id, "Pairing code generated (expires in 5 min)");
        Some(code)
    }

    /// Verify a pairing code. On success, adds the user to runtime-approved list.
    pub async fn verify_pairing_code(&self, user_id: &str, code: &str) -> bool {
        let mut pairings = self.pending_pairings.write().await;
        if let Some(pairing) = pairings.get_mut(user_id) {
            if pairing.failed_attempts >= 5 {
                warn!(user_id, "Pairing code locked — too many failed attempts");
                return false;
            }
            if constant_time_eq_str(&pairing.code, code) && chrono::Utc::now() < pairing.expires_at {
                pairings.remove(user_id);
                drop(pairings);

                // Add to runtime-approved and persist
                let mut list = self.runtime_allowed.write().await;
                list.push(user_id.to_string());
                self.persist_runtime_allowed(&list);
                drop(list);
                info!(user_id, "Pairing verified — user approved");
                return true;
            }
            pairing.failed_attempts += 1;
            warn!(user_id, attempts = pairing.failed_attempts, "Pairing code verification failed");
        }
        false
    }

    /// Get the list of runtime-approved users.
    pub async fn runtime_approved_users(&self) -> Vec<String> {
        self.runtime_allowed.read().await.clone()
    }

    /// Manually approve a user (from Dashboard).
    pub async fn approve_user(&self, user_id: &str) {
        let mut list = self.runtime_allowed.write().await;
        if !list.iter().any(|u| u == user_id) {
            list.push(user_id.to_string());
            self.persist_runtime_allowed(&list);
            info!(user_id, "User manually approved");
        }
    }

    /// Remove a user from runtime-approved list.
    pub async fn revoke_user(&self, user_id: &str) {
        let mut list = self.runtime_allowed.write().await;
        list.retain(|u| u != user_id);
        self.persist_runtime_allowed(&list);
        info!(user_id, "User revoked");
    }

    // --- Tool-level authorization ---

    /// Check if a specific tool is approved for use by an agent.
    ///
    /// `session_id` is `Some(id)` for a session-scoped check; `None` skips the
    /// session check and matches any approval (including session-scoped ones).
    pub fn is_tool_approved(&self, tool_name: &str, agent_id: &str, session_id: Option<&str>) -> bool {
        let now = Utc::now();
        self.tool_approvals.iter().any(|a| {
            a.tool_name == tool_name
                && a.agent_id == agent_id
                && a.expires_at.map_or(true, |exp| exp > now)
                && (!a.session_scoped || session_id.map_or(false, |sid| a.session_id.as_deref() == Some(sid)))
        })
    }

    /// Approve a tool for a specific agent.
    pub fn approve_tool(
        &mut self,
        tool_name: String,
        agent_id: String,
        approved_by: String,
        duration_minutes: Option<u32>,
        session_scoped: bool,
    ) {
        let now = Utc::now();
        let expires_at = duration_minutes.map(|m| now + chrono::Duration::minutes(i64::from(m)));

        // Remove existing approval for same tool+agent if any
        self.tool_approvals
            .retain(|a| !(a.tool_name == tool_name && a.agent_id == agent_id));

        let session_id = if session_scoped { self.current_session_id.clone() } else { None };
        self.tool_approvals.push(ToolApproval {
            tool_name: tool_name.clone(),
            agent_id: agent_id.clone(),
            approved_by,
            approved_at: now,
            expires_at,
            session_scoped,
            session_id,
        });

        info!(tool = %tool_name, agent = %agent_id, "Tool approved");
    }

    /// Revoke a tool approval.
    pub fn revoke_tool(&mut self, tool_name: &str, agent_id: &str) {
        self.tool_approvals
            .retain(|a| !(a.tool_name == tool_name && a.agent_id == agent_id));
        info!(tool = %tool_name, agent = %agent_id, "Tool approval revoked");
    }

    /// List all tool approvals (optionally filtered by agent).
    pub fn list_tool_approvals(&self, agent_id: Option<&str>) -> Vec<&ToolApproval> {
        let now = Utc::now();
        self.tool_approvals
            .iter()
            .filter(|a| {
                agent_id.map_or(true, |id| a.agent_id == id)
                    && a.expires_at.map_or(true, |exp| exp > now)
            })
            .collect()
    }

    // --- Session-scoped authorization ---

    /// Start a new session, generating a session ID.
    pub fn start_session(&mut self) -> String {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.current_session_id = Some(session_id.clone());
        info!(session_id = %session_id, "Session started");
        session_id
    }

    /// End the current session, revoking all session-scoped approvals.
    pub fn end_session(&mut self) {
        let revoked = self.tool_approvals.iter().filter(|a| a.session_scoped).count();
        self.tool_approvals.retain(|a| !a.session_scoped);
        self.current_session_id = None;
        info!(revoked_count = revoked, "Session ended, session-scoped approvals revoked");
    }

    // --- Emergency stop ---

    /// Activate emergency stop. All agent operations should be halted.
    pub fn emergency_stop(&mut self) {
        self.emergency_state = EmergencyState::Stopped;
        warn!("EMERGENCY STOP activated — all agent operations halted");
    }

    /// Resume from emergency stop.
    pub fn emergency_resume(&mut self) {
        self.emergency_state = EmergencyState::Normal;
        info!("Emergency stop lifted — operations resumed");
    }

    /// Check if emergency stop is active.
    pub fn is_emergency_stopped(&self) -> bool {
        self.emergency_state == EmergencyState::Stopped
    }
}

impl Default for AccessController {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_access() {
        let ctrl = AccessController::new();
        assert_eq!(
            ctrl.check_access("user1", "telegram", None, &[], false).await,
            AccessDecision::Allowed
        );
    }

    #[tokio::test]
    async fn test_blocked_user() {
        let ctrl = AccessController::new();
        let blocked = vec!["bad_user".to_string()];
        assert_eq!(
            ctrl.check_access("bad_user", "telegram", None, &blocked, false).await,
            AccessDecision::Blocked
        );
    }

    #[tokio::test]
    async fn test_blocked_overrides_allowed() {
        let ctrl = AccessController::new();
        let allowed = vec!["user1".to_string()];
        let blocked = vec!["user1".to_string()];
        assert_eq!(
            ctrl.check_access("user1", "telegram", Some(&allowed), &blocked, false).await,
            AccessDecision::Blocked
        );
    }

    #[tokio::test]
    async fn test_allowlist_denies_unknown() {
        let ctrl = AccessController::new();
        let allowed = vec!["user1".to_string()];
        assert_eq!(
            ctrl.check_access("user2", "telegram", Some(&allowed), &[], false).await,
            AccessDecision::Blocked
        );
    }

    #[tokio::test]
    async fn test_require_pairing() {
        let ctrl = AccessController::new();
        assert_eq!(
            ctrl.check_access("user1", "telegram", None, &[], true).await,
            AccessDecision::RequirePairing
        );
    }

    #[test]
    fn tool_approval_basic() {
        let mut ctrl = AccessController::new();
        assert!(!ctrl.is_tool_approved("browser", "agent1", None));
        ctrl.approve_tool("browser".into(), "agent1".into(), "admin".into(), None, false);
        assert!(ctrl.is_tool_approved("browser", "agent1", None));
        ctrl.revoke_tool("browser", "agent1");
        assert!(!ctrl.is_tool_approved("browser", "agent1", None));
    }

    #[test]
    fn session_scoped_revoke() {
        let mut ctrl = AccessController::new();
        let sid = ctrl.start_session();
        ctrl.approve_tool("browser".into(), "agent1".into(), "admin".into(), None, true);
        assert!(ctrl.is_tool_approved("browser", "agent1", Some(&sid)));
        ctrl.end_session();
        assert!(!ctrl.is_tool_approved("browser", "agent1", Some(&sid)));
    }

    #[test]
    fn emergency_stop_state() {
        let mut ctrl = AccessController::new();
        assert!(!ctrl.is_emergency_stopped());
        ctrl.emergency_stop();
        assert!(ctrl.is_emergency_stopped());
        ctrl.emergency_resume();
        assert!(!ctrl.is_emergency_stopped());
    }

    #[tokio::test]
    async fn test_pairing_flow() {
        let ctrl = AccessController::new();

        // Generate code
        let code = ctrl.generate_pairing_code("user1").await.expect("code should be generated");
        assert_eq!(code.len(), 6);

        // Verify with wrong code fails
        assert!(!ctrl.verify_pairing_code("user1", "000000").await);

        // Re-generate (the old code was consumed by failed attempt? No, only on success)
        let code = ctrl.generate_pairing_code("user1").await.expect("code should be generated");

        // Verify with correct code succeeds
        assert!(ctrl.verify_pairing_code("user1", &code).await);

        // After pairing, user is approved
        assert_eq!(
            ctrl.check_access("user1", "telegram", None, &[], true).await,
            AccessDecision::Allowed
        );
    }

    #[tokio::test]
    async fn test_pairing_lockout_across_regenerations() {
        let ctrl = AccessController::new();

        // Exhaust 15 cumulative attempts across multiple code generations
        let mut total_attempts = 0u32;
        loop {
            let code_opt = ctrl.generate_pairing_code("attacker").await;
            if code_opt.is_none() {
                break;
            }
            // Fail this code (adds to cumulative count)
            ctrl.verify_pairing_code("attacker", "000000").await;
            total_attempts += 1;
            if total_attempts > 20 {
                panic!("Should have been blocked before 20 attempts");
            }
        }
        // Generation should have been blocked before exceeding 15 total attempts
        assert!(total_attempts <= 15);
    }
}
