//! Governance 政策類型定義
//!
//! 定義四種政策類型：Rate / Permission / Quota / Lifecycle。
//! 所有類型均支援 YAML 和 JSON 序列化（YAML 為主要格式）。

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Type aliases ─────────────────────────────────────────────────────────────

/// 政策唯一識別碼。
pub type PolicyId = String;

/// Agent 識別碼，`"*"` 代表作用於所有 Agent。
pub type AgentId = String;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("Invalid policy schema: {0}")]
    InvalidSchema(String),

    #[error("Policy not found: {0}")]
    NotFound(String),

    #[error("Policy conflict: {0}")]
    Conflict(String),

    #[error("YAML parse error: {0}")]
    YamlParse(#[from] serde_yaml::Error),
}

// ── Resource enum ─────────────────────────────────────────────────────────────

/// 速率限制的資源類型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    McpCalls,
    MemoryWrites,
    WikiWrites,
    MessageSends,
}

impl std::fmt::Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::McpCalls => write!(f, "mcp_calls"),
            Self::MemoryWrites => write!(f, "memory_writes"),
            Self::WikiWrites => write!(f, "wiki_writes"),
            Self::MessageSends => write!(f, "message_sends"),
        }
    }
}

// ── ActionOnViolation ─────────────────────────────────────────────────────────

/// 違規時的處置方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionOnViolation {
    /// 直接拒絕操作，回傳錯誤。
    Reject,
    /// 允許操作但記錄警告。
    Warn,
    /// 限速，延遲操作。
    Throttle,
}

impl Default for ActionOnViolation {
    fn default() -> Self {
        Self::Reject
    }
}

impl std::fmt::Display for ActionOnViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reject => write!(f, "reject"),
            Self::Warn => write!(f, "warn"),
            Self::Throttle => write!(f, "throttle"),
        }
    }
}

// ── RatePolicy ────────────────────────────────────────────────────────────────

/// 速率政策 — 限制單位時間內的操作次數。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatePolicy {
    pub policy_id: PolicyId,
    /// `"*"` 代表作用於所有 Agent。
    pub agent_id: AgentId,
    pub resource: Resource,
    /// 時間窗口內允許的最大操作次數。
    pub limit: u32,
    /// 時間窗口大小（秒）。
    pub window_seconds: u32,
    #[serde(default)]
    pub action_on_violation: ActionOnViolation,
}

impl RatePolicy {
    /// 驗證政策合法性。
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.policy_id.is_empty() {
            return Err(PolicyError::InvalidSchema("policy_id cannot be empty".into()));
        }
        if self.limit == 0 {
            return Err(PolicyError::InvalidSchema(
                "rate limit must be > 0".into(),
            ));
        }
        if self.window_seconds == 0 {
            return Err(PolicyError::InvalidSchema(
                "window_seconds must be > 0".into(),
            ));
        }
        Ok(())
    }
}

// ── PermissionPolicy ──────────────────────────────────────────────────────────

/// 權限政策 — 定義 Agent 允許/禁止的 scope 及需要核准的操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    pub policy_id: PolicyId,
    pub agent_id: AgentId,
    #[serde(default)]
    pub allowed_scopes: Vec<String>,
    #[serde(default)]
    pub denied_scopes: Vec<String>,
    #[serde(default)]
    pub requires_approval: Vec<String>,
}

impl PermissionPolicy {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.policy_id.is_empty() {
            return Err(PolicyError::InvalidSchema("policy_id cannot be empty".into()));
        }
        // Check for conflicts between allowed and denied
        for scope in &self.allowed_scopes {
            if self.denied_scopes.contains(scope) {
                return Err(PolicyError::Conflict(format!(
                    "scope '{scope}' appears in both allowed_scopes and denied_scopes"
                )));
            }
        }
        Ok(())
    }

    /// 檢查給定 scope 是否被允許。
    pub fn is_scope_allowed(&self, scope: &str) -> bool {
        // Denied takes priority
        if self.denied_scopes.contains(&scope.to_string()) {
            return false;
        }
        // If no allowed list, allow everything not denied
        if self.allowed_scopes.is_empty() {
            return true;
        }
        self.allowed_scopes.contains(&scope.to_string())
    }

    /// 檢查給定 scope 是否需要核准。
    pub fn requires_approval_for(&self, scope: &str) -> bool {
        self.requires_approval.contains(&scope.to_string())
    }
}

// ── QuotaPolicy ───────────────────────────────────────────────────────────────

/// 配額政策 — 管理每日 token 預算、並發任務數與記憶條目數。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaPolicy {
    pub policy_id: PolicyId,
    pub agent_id: AgentId,
    /// 每日最大 token 使用量。
    pub daily_token_budget: u64,
    /// 最大並發任務數。
    pub max_concurrent_tasks: u32,
    /// 最大記憶條目數。
    pub max_memory_entries: u64,
    /// 重置 cron 表達式（預設每日 00:00）。
    #[serde(default = "default_reset_cron")]
    pub reset_cron: String,
}

fn default_reset_cron() -> String {
    "0 0 * * *".to_string()
}

impl QuotaPolicy {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.policy_id.is_empty() {
            return Err(PolicyError::InvalidSchema("policy_id cannot be empty".into()));
        }
        if self.daily_token_budget == 0 {
            return Err(PolicyError::InvalidSchema(
                "daily_token_budget must be > 0".into(),
            ));
        }
        if self.max_concurrent_tasks == 0 {
            return Err(PolicyError::InvalidSchema(
                "max_concurrent_tasks must be > 0".into(),
            ));
        }
        Ok(())
    }
}

// ── LifecyclePolicy ───────────────────────────────────────────────────────────

/// 生命週期政策 — 管理 Agent 閒置時間、健康檢查與自動暫停。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecyclePolicy {
    pub policy_id: PolicyId,
    pub agent_id: AgentId,
    /// 閒置超過此時數自動暫停（小時）。
    pub max_idle_hours: u32,
    /// 健康檢查間隔（秒）。
    pub health_check_interval_seconds: u32,
    /// 違規次數達此值時自動暫停 Agent。
    pub auto_suspend_on_violation_count: u32,
}

impl LifecyclePolicy {
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.policy_id.is_empty() {
            return Err(PolicyError::InvalidSchema("policy_id cannot be empty".into()));
        }
        if self.max_idle_hours == 0 {
            return Err(PolicyError::InvalidSchema(
                "max_idle_hours must be > 0".into(),
            ));
        }
        if self.health_check_interval_seconds == 0 {
            return Err(PolicyError::InvalidSchema(
                "health_check_interval_seconds must be > 0".into(),
            ));
        }
        Ok(())
    }
}

// ── PolicyType enum ───────────────────────────────────────────────────────────

/// 所有政策類型的統一枚舉。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "policy_type", rename_all = "snake_case")]
pub enum PolicyType {
    Rate(RatePolicy),
    Permission(PermissionPolicy),
    Quota(QuotaPolicy),
    Lifecycle(LifecyclePolicy),
}

impl PolicyType {
    /// 取得政策 ID。
    pub fn policy_id(&self) -> &str {
        match self {
            Self::Rate(p) => &p.policy_id,
            Self::Permission(p) => &p.policy_id,
            Self::Quota(p) => &p.policy_id,
            Self::Lifecycle(p) => &p.policy_id,
        }
    }

    /// 取得政策適用的 Agent ID（`"*"` = 全域）。
    pub fn agent_id(&self) -> &str {
        match self {
            Self::Rate(p) => &p.agent_id,
            Self::Permission(p) => &p.agent_id,
            Self::Quota(p) => &p.agent_id,
            Self::Lifecycle(p) => &p.agent_id,
        }
    }

    /// 取得政策類型字串（用於稽核事件）。
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Rate(_) => "rate",
            Self::Permission(_) => "permission",
            Self::Quota(_) => "quota",
            Self::Lifecycle(_) => "lifecycle",
        }
    }

    /// 驗證政策合法性。
    pub fn validate(&self) -> Result<(), PolicyError> {
        match self {
            Self::Rate(p) => p.validate(),
            Self::Permission(p) => p.validate(),
            Self::Quota(p) => p.validate(),
            Self::Lifecycle(p) => p.validate(),
        }
    }
}

// ── Policy (aliased) ──────────────────────────────────────────────────────────

/// `PolicyType` 的別名，供外部使用。
pub type Policy = PolicyType;

// ── YAML file structure ───────────────────────────────────────────────────────

/// YAML 政策檔案的頂層結構。
///
/// ```yaml
/// policies:
///   - policy_type: rate
///     policy_id: default-rate-mcp
///     agent_id: "*"
///     ...
/// ```
#[derive(Debug, Deserialize)]
pub struct PolicyFile {
    pub policies: Vec<PolicyType>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── RatePolicy tests ──────────────────────────────────────────────────────

    #[test]
    fn test_rate_policy_valid() {
        let p = RatePolicy {
            policy_id: "test-rate".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_rate_policy_zero_limit_invalid() {
        let p = RatePolicy {
            policy_id: "test-rate".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 0, // invalid
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_rate_policy_zero_window_invalid() {
        let p = RatePolicy {
            policy_id: "test-rate".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 0, // invalid
            action_on_violation: ActionOnViolation::Reject,
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_rate_policy_empty_id_invalid() {
        let p = RatePolicy {
            policy_id: "".into(), // invalid
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        };
        assert!(p.validate().is_err());
    }

    // ── PermissionPolicy tests ────────────────────────────────────────────────

    #[test]
    fn test_permission_policy_scope_allowed() {
        let p = PermissionPolicy {
            policy_id: "test-perm".into(),
            agent_id: "*".into(),
            allowed_scopes: vec!["memory:read".into(), "wiki:read".into()],
            denied_scopes: vec!["admin".into()],
            requires_approval: vec!["wiki:write".into()],
        };
        assert!(p.is_scope_allowed("memory:read"));
        assert!(p.is_scope_allowed("wiki:read"));
        assert!(!p.is_scope_allowed("admin")); // denied
        assert!(!p.is_scope_allowed("wiki:write")); // not in allowed
    }

    #[test]
    fn test_permission_policy_denied_takes_priority() {
        let p = PermissionPolicy {
            policy_id: "test-perm".into(),
            agent_id: "*".into(),
            allowed_scopes: vec!["memory:read".into()],
            denied_scopes: vec!["memory:read".into()], // conflict — denied wins
            requires_approval: vec![],
        };
        // validate should catch conflict
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_permission_policy_empty_allowed_allows_all_except_denied() {
        let p = PermissionPolicy {
            policy_id: "test-perm".into(),
            agent_id: "*".into(),
            allowed_scopes: vec![],
            denied_scopes: vec!["admin".into()],
            requires_approval: vec![],
        };
        assert!(p.validate().is_ok());
        assert!(p.is_scope_allowed("anything"));
        assert!(!p.is_scope_allowed("admin"));
    }

    #[test]
    fn test_permission_requires_approval() {
        let p = PermissionPolicy {
            policy_id: "test-perm".into(),
            agent_id: "*".into(),
            allowed_scopes: vec![],
            denied_scopes: vec![],
            requires_approval: vec!["agent:create".into(), "agent:modify".into()],
        };
        assert!(p.requires_approval_for("agent:create"));
        assert!(p.requires_approval_for("agent:modify"));
        assert!(!p.requires_approval_for("memory:read"));
    }

    // ── QuotaPolicy tests ─────────────────────────────────────────────────────

    #[test]
    fn test_quota_policy_valid() {
        let p = QuotaPolicy {
            policy_id: "test-quota".into(),
            agent_id: "*".into(),
            daily_token_budget: 500_000,
            max_concurrent_tasks: 5,
            max_memory_entries: 10_000,
            reset_cron: "0 0 * * *".into(),
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_quota_policy_zero_budget_invalid() {
        let p = QuotaPolicy {
            policy_id: "test-quota".into(),
            agent_id: "*".into(),
            daily_token_budget: 0, // invalid
            max_concurrent_tasks: 5,
            max_memory_entries: 10_000,
            reset_cron: "0 0 * * *".into(),
        };
        assert!(p.validate().is_err());
    }

    // ── LifecyclePolicy tests ─────────────────────────────────────────────────

    #[test]
    fn test_lifecycle_policy_valid() {
        let p = LifecyclePolicy {
            policy_id: "test-lifecycle".into(),
            agent_id: "*".into(),
            max_idle_hours: 48,
            health_check_interval_seconds: 300,
            auto_suspend_on_violation_count: 10,
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn test_lifecycle_policy_zero_idle_hours_invalid() {
        let p = LifecyclePolicy {
            policy_id: "test-lifecycle".into(),
            agent_id: "*".into(),
            max_idle_hours: 0, // invalid
            health_check_interval_seconds: 300,
            auto_suspend_on_violation_count: 10,
        };
        assert!(p.validate().is_err());
    }

    // ── YAML deserialization tests ────────────────────────────────────────────

    #[test]
    fn test_yaml_rate_policy_deserialize() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject
"#;
        let file: PolicyFile = serde_yaml::from_str(yaml).expect("YAML parse failed");
        assert_eq!(file.policies.len(), 1);
        match &file.policies[0] {
            PolicyType::Rate(p) => {
                assert_eq!(p.policy_id, "default-rate-mcp");
                assert_eq!(p.limit, 200);
                assert_eq!(p.window_seconds, 60);
                assert_eq!(p.action_on_violation, ActionOnViolation::Reject);
            }
            _ => panic!("Expected rate policy"),
        }
    }

    #[test]
    fn test_yaml_permission_policy_deserialize() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
      - wiki:read
    denied_scopes:
      - admin
    requires_approval:
      - agent:create
      - agent:modify
"#;
        let file: PolicyFile = serde_yaml::from_str(yaml).expect("YAML parse failed");
        assert_eq!(file.policies.len(), 1);
        match &file.policies[0] {
            PolicyType::Permission(p) => {
                assert_eq!(p.allowed_scopes.len(), 3);
                assert_eq!(p.denied_scopes, vec!["admin"]);
                assert_eq!(p.requires_approval.len(), 2);
            }
            _ => panic!("Expected permission policy"),
        }
    }

    #[test]
    fn test_yaml_quota_policy_deserialize() {
        let yaml = r#"
policies:
  - policy_type: quota
    policy_id: default-quota-daily
    agent_id: "*"
    daily_token_budget: 500000
    max_concurrent_tasks: 5
    max_memory_entries: 10000
    reset_cron: "0 0 * * *"
"#;
        let file: PolicyFile = serde_yaml::from_str(yaml).expect("YAML parse failed");
        match &file.policies[0] {
            PolicyType::Quota(p) => {
                assert_eq!(p.daily_token_budget, 500_000);
                assert_eq!(p.max_concurrent_tasks, 5);
            }
            _ => panic!("Expected quota policy"),
        }
    }

    #[test]
    fn test_yaml_multiple_policies_deserialize() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: rate-1
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject
  - policy_type: permission
    policy_id: perm-1
    agent_id: "*"
    allowed_scopes:
      - memory:read
    denied_scopes: []
    requires_approval: []
  - policy_type: quota
    policy_id: quota-1
    agent_id: "*"
    daily_token_budget: 100000
    max_concurrent_tasks: 3
    max_memory_entries: 5000
  - policy_type: lifecycle
    policy_id: lifecycle-1
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
"#;
        let file: PolicyFile = serde_yaml::from_str(yaml).expect("YAML parse failed");
        assert_eq!(file.policies.len(), 4);
    }

    #[test]
    fn test_policy_type_accessors() {
        let p = PolicyType::Rate(RatePolicy {
            policy_id: "test-rate".into(),
            agent_id: "agent-1".into(),
            resource: Resource::MemoryWrites,
            limit: 50,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Warn,
        });
        assert_eq!(p.policy_id(), "test-rate");
        assert_eq!(p.agent_id(), "agent-1");
        assert_eq!(p.type_name(), "rate");
    }
}
