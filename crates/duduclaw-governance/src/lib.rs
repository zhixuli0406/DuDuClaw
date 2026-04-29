//! DuDuClaw Governance PolicyEngine — W19-P1 柱一
//!
//! 為每個 Agent 定義行為邊界，提供違規偵測、核准工作流與資源配額管理。
//!
//! ## 模組架構
//! ```text
//! PolicyEngine
//! ├── PolicyRegistry    — YAML 政策定義儲存庫（per-agent + global）
//! ├── PolicyEvaluator   — 請求/操作前的政策評估（in-memory cache, p99 < 5ms）
//! ├── ViolationDetector — 違規即時偵測 + Audit Trail 發射
//! └── ApprovalWorkflow  — 高權限操作的核准鏈
//! ```
//!
//! ## 快速開始
//! ```rust,ignore
//! use duduclaw_governance::{PolicyRegistry, PolicyEvaluator, Operation, OperationType};
//! use std::sync::Arc;
//! use std::path::PathBuf;
//!
//! let registry = Arc::new(PolicyRegistry::new(PathBuf::from("policies"))?);
//! registry.load().await?;
//!
//! let evaluator = PolicyEvaluator::new(registry);
//! let result = evaluator.evaluate("my-agent", &Operation {
//!     op_type: OperationType::MemoryWrite,
//!     resource_id: None,
//!     scope: "memory:write".into(),
//!     metadata: serde_json::json!({}),
//! }).await;
//! ```

pub mod approval;
pub mod audit;
pub mod evaluator;
pub mod policy;
pub mod registry;
pub mod violation;

#[cfg(test)]
pub mod tests;

// Re-export commonly used types
pub use approval::{ApprovalDecision, ApprovalRequest, ApprovalResponse, ApprovalWorkflow};
pub use audit::AuditEventSink;
pub use evaluator::{EvaluationResult, PolicyEvaluator, ViolationType};
pub use policy::{
    ActionOnViolation, LifecyclePolicy, PermissionPolicy, Policy, PolicyId, PolicyType,
    QuotaPolicy, RatePolicy, Resource,
};
pub use registry::PolicyRegistry;
pub use violation::ViolationDetector;

use serde::{Deserialize, Serialize};

// ── Operation types ───────────────────────────────────────────────────────────

/// 可被政策評估的操作類型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    McpCall,
    MemoryWrite,
    MemoryRead,
    WikiWrite,
    WikiRead,
    MessageSend,
    AgentCreate,
    AgentModify,
    AgentRemove,
    SkillActivate,
    SkillDeactivate,
    PolicyChange,
    Other(String),
}

impl std::fmt::Display for OperationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::McpCall => write!(f, "mcp_call"),
            Self::MemoryWrite => write!(f, "memory_write"),
            Self::MemoryRead => write!(f, "memory_read"),
            Self::WikiWrite => write!(f, "wiki_write"),
            Self::WikiRead => write!(f, "wiki_read"),
            Self::MessageSend => write!(f, "message_send"),
            Self::AgentCreate => write!(f, "agent_create"),
            Self::AgentModify => write!(f, "agent_modify"),
            Self::AgentRemove => write!(f, "agent_remove"),
            Self::SkillActivate => write!(f, "skill_activate"),
            Self::SkillDeactivate => write!(f, "skill_deactivate"),
            Self::PolicyChange => write!(f, "policy_change"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// 單筆操作的描述，供 PolicyEvaluator 評估用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// 操作類型
    pub op_type: OperationType,
    /// 被操作的資源 ID（可選）
    pub resource_id: Option<String>,
    /// 操作所需的 scope（例如 `"wiki:write"`）
    pub scope: String,
    /// 額外 metadata（用於 Quota/Lifecycle 評估）
    pub metadata: serde_json::Value,
}
