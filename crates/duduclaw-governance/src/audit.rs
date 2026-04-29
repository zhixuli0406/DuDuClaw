//! AuditEventSink trait — Governance 事件發射抽象介面
//!
//! 定義 trait 讓 governance 模組可在不直接依賴 `duduclaw-gateway` 的情況下
//! 發射稽核事件。呼叫端（duduclaw-cli / duduclaw-gateway）提供具體實作。
//!
//! ## Null 實作
//! 測試環境或不需要稽核追蹤時，可使用 [`NoopAuditSink`]。

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Governance audit event types ─────────────────────────────────────────────

/// governance_violation 事件的詳細資料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceViolationEvent {
    pub event_id: String,
    pub timestamp: String,
    pub agent_id: String,
    pub policy_id: String,
    pub policy_type: String,
    pub violation_detail: String,
    pub operation_type: String,
    /// "blocked" | "warned" | "throttled"
    pub outcome: String,
}

impl GovernanceViolationEvent {
    pub fn new(
        agent_id: impl Into<String>,
        policy_id: impl Into<String>,
        policy_type: impl Into<String>,
        violation_detail: impl Into<String>,
        operation_type: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            agent_id: agent_id.into(),
            policy_id: policy_id.into(),
            policy_type: policy_type.into(),
            violation_detail: violation_detail.into(),
            operation_type: operation_type.into(),
            outcome: outcome.into(),
        }
    }
}

/// governance_approval_requested 事件的詳細資料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceApprovalRequestedEvent {
    pub event_id: String,
    pub timestamp: String,
    pub agent_id: String,
    pub approval_request_id: String,
    pub operation_type: String,
    pub justification: String,
    /// always "pending"
    pub outcome: String,
}

impl GovernanceApprovalRequestedEvent {
    pub fn new(
        agent_id: impl Into<String>,
        approval_request_id: impl Into<String>,
        operation_type: impl Into<String>,
        justification: impl Into<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            agent_id: agent_id.into(),
            approval_request_id: approval_request_id.into(),
            operation_type: operation_type.into(),
            justification: justification.into(),
            outcome: "pending".into(),
        }
    }
}

/// governance_approval_decided 事件的詳細資料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceApprovalDecidedEvent {
    pub event_id: String,
    pub timestamp: String,
    pub agent_id: String,
    pub approval_request_id: String,
    pub approver_id: String,
    /// "approved" | "rejected"
    pub outcome: String,
    pub reason: Option<String>,
}

impl GovernanceApprovalDecidedEvent {
    pub fn new(
        agent_id: impl Into<String>,
        approval_request_id: impl Into<String>,
        approver_id: impl Into<String>,
        outcome: impl Into<String>,
        reason: Option<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            agent_id: agent_id.into(),
            approval_request_id: approval_request_id.into(),
            approver_id: approver_id.into(),
            outcome: outcome.into(),
            reason,
        }
    }
}

/// governance_policy_changed 事件的詳細資料。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernancePolicyChangedEvent {
    pub event_id: String,
    pub timestamp: String,
    pub agent_id: String,
    pub policy_id: String,
    pub change_type: String, // "created" | "updated" | "deleted"
    /// "success" | "failure"
    pub outcome: String,
}

impl GovernancePolicyChangedEvent {
    pub fn new(
        agent_id: impl Into<String>,
        policy_id: impl Into<String>,
        change_type: impl Into<String>,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            agent_id: agent_id.into(),
            policy_id: policy_id.into(),
            change_type: change_type.into(),
            outcome: outcome.into(),
        }
    }
}

// ── AuditEventSink trait ──────────────────────────────────────────────────────

/// Governance 稽核事件發射介面。
///
/// 所有方法均為 fire-and-forget（非同步、不回傳錯誤），
/// 稽核失敗不得影響主流程。
#[async_trait::async_trait]
pub trait AuditEventSink: Send + Sync {
    /// 發射 governance_violation 事件。
    async fn emit_governance_violation(&self, event: GovernanceViolationEvent);

    /// 發射 governance_approval_requested 事件。
    async fn emit_approval_requested(&self, event: GovernanceApprovalRequestedEvent);

    /// 發射 governance_approval_decided 事件。
    async fn emit_approval_decided(&self, event: GovernanceApprovalDecidedEvent);

    /// 發射 governance_policy_changed 事件。
    async fn emit_policy_changed(&self, event: GovernancePolicyChangedEvent);
}

// ── Noop implementation ───────────────────────────────────────────────────────

/// 不發射任何事件的空實作，適合測試環境。
#[derive(Debug, Default, Clone)]
pub struct NoopAuditSink;

#[async_trait::async_trait]
impl AuditEventSink for NoopAuditSink {
    async fn emit_governance_violation(&self, _event: GovernanceViolationEvent) {}
    async fn emit_approval_requested(&self, _event: GovernanceApprovalRequestedEvent) {}
    async fn emit_approval_decided(&self, _event: GovernanceApprovalDecidedEvent) {}
    async fn emit_policy_changed(&self, _event: GovernancePolicyChangedEvent) {}
}

/// Noop sink 的 Arc 包裝，方便測試使用。
pub fn noop_sink() -> Arc<dyn AuditEventSink> {
    Arc::new(NoopAuditSink)
}

// ── Recording implementation (for tests) ─────────────────────────────────────

/// 記錄所有收到事件的測試 sink。
#[derive(Debug, Default)]
pub struct RecordingAuditSink {
    pub violations: tokio::sync::Mutex<Vec<GovernanceViolationEvent>>,
    pub approval_requests: tokio::sync::Mutex<Vec<GovernanceApprovalRequestedEvent>>,
    pub approval_decisions: tokio::sync::Mutex<Vec<GovernanceApprovalDecidedEvent>>,
    pub policy_changes: tokio::sync::Mutex<Vec<GovernancePolicyChangedEvent>>,
}

#[async_trait::async_trait]
impl AuditEventSink for RecordingAuditSink {
    async fn emit_governance_violation(&self, event: GovernanceViolationEvent) {
        self.violations.lock().await.push(event);
    }

    async fn emit_approval_requested(&self, event: GovernanceApprovalRequestedEvent) {
        self.approval_requests.lock().await.push(event);
    }

    async fn emit_approval_decided(&self, event: GovernanceApprovalDecidedEvent) {
        self.approval_decisions.lock().await.push(event);
    }

    async fn emit_policy_changed(&self, event: GovernancePolicyChangedEvent) {
        self.policy_changes.lock().await.push(event);
    }
}
