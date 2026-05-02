//! ApprovalWorkflow — 高權限操作核准工作流
//!
//! ## 功能
//! - `governance/approval/request`：Agent 申請高權限操作
//! - `governance/approval/decide`：核准者做出決策（approve/reject）
//! - 申請有 TTL（超時自動過期）
//! - 決策後自動發射 `governance_approval_*` 稽核事件

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

use crate::{
    audit::{
        AuditEventSink, GovernanceApprovalDecidedEvent, GovernanceApprovalRequestedEvent,
    },
    Operation,
};

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("Approval request not found: {0}")]
    NotFound(String),

    #[error("Approval request expired: {0}")]
    Expired(String),

    #[error("Approval request already decided: {0}")]
    AlreadyDecided(String),

    #[error("Unauthorized approver: {0}")]
    Unauthorized(String),
}

// ── ApprovalStatus ────────────────────────────────────────────────────────────

/// 核准申請的狀態。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl std::fmt::Display for ApprovalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Approved => write!(f, "approved"),
            Self::Rejected => write!(f, "rejected"),
            Self::Expired => write!(f, "expired"),
        }
    }
}

// ── ApprovalRequest ───────────────────────────────────────────────────────────

/// 核准申請請求（輸入）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// 申請的 Agent ID。
    pub agent_id: String,
    /// 申請執行的操作。
    pub operation: Operation,
    /// 申請理由。
    pub justification: String,
    /// 申請的有效期（秒）。
    #[serde(default = "default_ttl_seconds")]
    pub ttl_seconds: u64,
}

fn default_ttl_seconds() -> u64 {
    3600 // 1 hour
}

/// 核准申請回應（輸出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub approval_request_id: String,
    pub status: ApprovalStatus,
    pub expires_at: DateTime<Utc>,
    /// 可核准的人員列表（目前預設為系統管理員 "admin"）。
    pub approvers: Vec<String>,
}

// ── ApprovalDecision ──────────────────────────────────────────────────────────

/// 核准決策（輸入）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub approval_request_id: String,
    pub approver_id: String,
    pub decision: ApprovalDecisionType,
    pub reason: Option<String>,
}

/// 決策類型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionType {
    Approve,
    Reject,
}

impl std::fmt::Display for ApprovalDecisionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Approve => write!(f, "approved"),
            Self::Reject => write!(f, "rejected"),
        }
    }
}

/// 決策回應（輸出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecisionResponse {
    pub approval_request_id: String,
    pub status: ApprovalStatus,
    pub decided_at: DateTime<Utc>,
}

// ── Internal pending record ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct PendingApproval {
    pub agent_id: String,
    /// 原始申請（保留供稽核歷史記錄使用）。
    #[allow(dead_code)]
    pub request: ApprovalRequest,
    pub status: ApprovalStatus,
    /// 申請建立時間（保留供稽核歷史記錄使用）。
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
}

impl PendingApproval {
    fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

// ── ApprovalWorkflow ──────────────────────────────────────────────────────────

/// 核准工作流管理器。
///
/// 持有所有待審批的申請（包含已決策的歷史記錄）。
pub struct ApprovalWorkflow {
    /// approval_request_id → PendingApproval
    pending: RwLock<HashMap<String, PendingApproval>>,
    audit_sink: Arc<dyn AuditEventSink>,
    /// 預設核准者列表。
    default_approvers: Vec<String>,
}

impl ApprovalWorkflow {
    /// 建立 ApprovalWorkflow。
    pub fn new(audit_sink: Arc<dyn AuditEventSink>) -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
            audit_sink,
            default_approvers: vec!["admin".to_string()],
        }
    }

    /// 建立 ApprovalWorkflow，指定預設核准者。
    pub fn with_approvers(
        audit_sink: Arc<dyn AuditEventSink>,
        approvers: Vec<String>,
    ) -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
            audit_sink,
            default_approvers: approvers,
        }
    }

    /// 申請高權限操作核准。
    ///
    /// 回傳申請 ID 和過期時間。
    pub async fn request(&self, req: ApprovalRequest) -> ApprovalResponse {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let expires_at = now
            + chrono::Duration::seconds(req.ttl_seconds as i64);

        let pending = PendingApproval {
            agent_id: req.agent_id.clone(),
            request: req.clone(),
            status: ApprovalStatus::Pending,
            created_at: now,
            expires_at,
            decided_at: None,
            decided_by: None,
        };

        self.pending.write().await.insert(id.clone(), pending);

        // Emit audit event (fire-and-forget)
        let event = GovernanceApprovalRequestedEvent::new(
            &req.agent_id,
            &id,
            req.operation.op_type.to_string(),
            &req.justification,
        );
        let sink = Arc::clone(&self.audit_sink);
        tokio::spawn(async move {
            sink.emit_approval_requested(event).await;
        });

        info!(
            agent_id = %req.agent_id,
            request_id = %id,
            expires_at = %expires_at,
            "Approval request created"
        );

        ApprovalResponse {
            approval_request_id: id,
            status: ApprovalStatus::Pending,
            expires_at,
            approvers: self.default_approvers.clone(),
        }
    }

    /// 核准者對申請做出決策。
    pub async fn decide(
        &self,
        decision: ApprovalDecision,
    ) -> Result<ApprovalDecisionResponse, ApprovalError> {
        let request_id = &decision.approval_request_id;

        let mut lock = self.pending.write().await;
        let entry = lock
            .get_mut(request_id)
            .ok_or_else(|| ApprovalError::NotFound(request_id.clone()))?;

        // Check expiry
        if entry.is_expired() && entry.status == ApprovalStatus::Pending {
            entry.status = ApprovalStatus::Expired;
            return Err(ApprovalError::Expired(request_id.clone()));
        }

        // Check already decided
        if entry.status != ApprovalStatus::Pending {
            return Err(ApprovalError::AlreadyDecided(format!(
                "{} (status: {})",
                request_id, entry.status
            )));
        }

        // Check approver authorization: if approvers list is non-empty, the approver must be in it
        if !self.default_approvers.is_empty()
            && !self.default_approvers.contains(&decision.approver_id)
        {
            return Err(ApprovalError::Unauthorized(format!(
                "'{}' is not in the authorized approvers list",
                decision.approver_id
            )));
        }

        let now = Utc::now();
        let new_status = match decision.decision {
            ApprovalDecisionType::Approve => ApprovalStatus::Approved,
            ApprovalDecisionType::Reject => ApprovalStatus::Rejected,
        };

        entry.status = new_status.clone();
        entry.decided_at = Some(now);
        entry.decided_by = Some(decision.approver_id.clone());

        let agent_id = entry.agent_id.clone();
        let outcome = decision.decision.to_string();

        // Emit audit event
        let event = GovernanceApprovalDecidedEvent::new(
            &agent_id,
            request_id,
            &decision.approver_id,
            &outcome,
            decision.reason.clone(),
        );
        let sink = Arc::clone(&self.audit_sink);
        tokio::spawn(async move {
            sink.emit_approval_decided(event).await;
        });

        info!(
            agent_id = %agent_id,
            request_id = %request_id,
            approver_id = %decision.approver_id,
            outcome = %outcome,
            "Approval decision made"
        );

        Ok(ApprovalDecisionResponse {
            approval_request_id: request_id.clone(),
            status: new_status,
            decided_at: now,
        })
    }

    /// 取得申請的目前狀態。
    pub async fn get_status(
        &self,
        request_id: &str,
    ) -> Option<(ApprovalStatus, DateTime<Utc>)> {
        let lock = self.pending.read().await;
        lock.get(request_id).map(|p| {
            let status = if p.status == ApprovalStatus::Pending && p.is_expired() {
                ApprovalStatus::Expired
            } else {
                p.status.clone()
            };
            (status, p.expires_at)
        })
    }

    /// 清理過期申請（可定期呼叫）。
    pub async fn cleanup_expired(&self) {
        let mut lock = self.pending.write().await;
        lock.retain(|_, v| {
            if v.status == ApprovalStatus::Pending && v.is_expired() {
                v.status = ApprovalStatus::Expired;
            }
            // 保留已決策的記錄（歷史）；只移除過期超過 24h 的空殼
            let keep = v.status != ApprovalStatus::Expired
                || Utc::now() - v.expires_at < chrono::Duration::hours(24);
            keep
        });
    }

    /// 取得待審批的申請清單。
    pub async fn list_pending(&self) -> Vec<String> {
        let lock = self.pending.read().await;
        lock.iter()
            .filter(|(_, v)| v.status == ApprovalStatus::Pending && !v.is_expired())
            .map(|(k, _)| k.clone())
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audit::RecordingAuditSink, OperationType};
    use std::sync::Arc;

    fn make_request(ttl: u64) -> ApprovalRequest {
        ApprovalRequest {
            agent_id: "test-agent".into(),
            operation: Operation {
                op_type: OperationType::AgentCreate,
                resource_id: None,
                scope: "agent:create".into(),
                metadata: serde_json::json!({}),
            },
            justification: "Need to create an agent for testing".into(),
            ttl_seconds: ttl,
        }
    }

    #[tokio::test]
    async fn test_request_creates_pending_approval() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow.request(make_request(3600)).await;
        assert_eq!(response.status, ApprovalStatus::Pending);
        assert!(!response.approval_request_id.is_empty());
        assert!(!response.approvers.is_empty());
    }

    #[tokio::test]
    async fn test_approve_decision() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow.request(make_request(3600)).await;
        let request_id = response.approval_request_id.clone();

        let decision = ApprovalDecision {
            approval_request_id: request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: Some("Looks good".into()),
        };

        let result = workflow.decide(decision).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status, ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn test_reject_decision() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow.request(make_request(3600)).await;
        let request_id = response.approval_request_id.clone();

        let decision = ApprovalDecision {
            approval_request_id: request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Reject,
            reason: Some("Not authorized".into()),
        };

        let result = workflow.decide(decision).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, ApprovalStatus::Rejected);
    }

    #[tokio::test]
    async fn test_already_decided_error() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow.request(make_request(3600)).await;
        let request_id = response.approval_request_id.clone();

        // First decision
        workflow
            .decide(ApprovalDecision {
                approval_request_id: request_id.clone(),
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Approve,
                reason: None,
            })
            .await
            .unwrap();

        // Second decision — should fail
        let result = workflow
            .decide(ApprovalDecision {
                approval_request_id: request_id.clone(),
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Reject,
                reason: None,
            })
            .await;

        assert!(matches!(result, Err(ApprovalError::AlreadyDecided(_))));
    }

    #[tokio::test]
    async fn test_not_found_error() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = workflow
            .decide(ApprovalDecision {
                approval_request_id: "non-existent-id".into(),
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Approve,
                reason: None,
            })
            .await;

        assert!(matches!(result, Err(ApprovalError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_expired_request() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        // TTL = 0 → immediately expired
        // We use a very small TTL and then check
        // For testing, we use 0 seconds (already expired by the time we decide)
        // Note: chrono Duration of 0 means it will expire immediately
        let response = workflow.request(make_request(0)).await;
        let request_id = response.approval_request_id.clone();

        // Small sleep to ensure expiry
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let result = workflow
            .decide(ApprovalDecision {
                approval_request_id: request_id,
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Approve,
                reason: None,
            })
            .await;

        assert!(matches!(result, Err(ApprovalError::Expired(_))));
    }

    #[tokio::test]
    async fn test_audit_events_emitted() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        let response = workflow.request(make_request(3600)).await;
        let request_id = response.approval_request_id.clone();

        workflow
            .decide(ApprovalDecision {
                approval_request_id: request_id,
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Approve,
                reason: Some("LGTM".into()),
            })
            .await
            .unwrap();

        // Wait for spawned tasks
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let requests = sink.approval_requests.lock().await;
        assert_eq!(requests.len(), 1, "approval request event should be emitted");
        assert_eq!(requests[0].agent_id, "test-agent");
        assert_eq!(requests[0].outcome, "pending");

        let decisions = sink.approval_decisions.lock().await;
        assert_eq!(decisions.len(), 1, "approval decision event should be emitted");
        assert_eq!(decisions[0].outcome, "approved");
        assert_eq!(decisions[0].approver_id, "admin");
    }

    #[tokio::test]
    async fn test_list_pending() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let r1 = workflow.request(make_request(3600)).await;
        let r2 = workflow.request(make_request(3600)).await;

        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 2);

        // Decide on one
        workflow
            .decide(ApprovalDecision {
                approval_request_id: r1.approval_request_id.clone(),
                approver_id: "admin".into(),
                decision: ApprovalDecisionType::Approve,
                reason: None,
            })
            .await
            .unwrap();

        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0], r2.approval_request_id);
    }
}
