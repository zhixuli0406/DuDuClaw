//! ViolationDetector — 政策違規偵測與事件發射
//!
//! ## 功能
//! - 從 PolicyEvaluator 結果中偵測違規
//! - 自動發射 `governance_violation` 事件至 Audit Trail
//! - 追蹤 Agent 違規次數（用於 LifecyclePolicy auto_suspend）

use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;
use tracing::warn;

use crate::{
    audit::{AuditEventSink, GovernanceViolationEvent},
    evaluator::{EvaluationResult, ViolationType},
    policy::PolicyType,
    Operation,
};

// ── Violation record ──────────────────────────────────────────────────────────

/// 單次違規的記錄。
#[derive(Debug, Clone)]
pub struct ViolationRecord {
    pub agent_id: String,
    pub policy_id: String,
    pub policy_type: String,
    pub violation_type: ViolationType,
    pub operation_type: String,
    pub message: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

// ── ViolationDetector ─────────────────────────────────────────────────────────

/// 違規偵測器。
///
/// 接收 PolicyEvaluator 的評估結果，若發現違規則：
/// 1. 記錄到內部計數器（用於 LifecyclePolicy auto_suspend）
/// 2. 非同步發射 `governance_violation` 事件至 Audit Trail
pub struct ViolationDetector {
    audit_sink: Arc<dyn AuditEventSink>,
    /// agent_id → 累計違規次數
    violation_counts: RwLock<HashMap<String, u32>>,
    /// 最近 N 筆違規記錄
    recent_violations: RwLock<Vec<ViolationRecord>>,
    /// 最多保留的最近違規筆數
    max_recent: usize,
}

impl ViolationDetector {
    /// 建立 ViolationDetector。
    pub fn new(audit_sink: Arc<dyn AuditEventSink>) -> Self {
        Self {
            audit_sink,
            violation_counts: RwLock::new(HashMap::new()),
            recent_violations: RwLock::new(Vec::new()),
            max_recent: 1000,
        }
    }

    /// 處理評估結果：若違規則記錄並發射稽核事件。
    ///
    /// 回傳 `true` 表示已記錄一次違規，`false` 表示無違規。
    pub async fn process_result(
        &self,
        agent_id: &str,
        operation: &Operation,
        result: &EvaluationResult,
        policies: &[PolicyType],
    ) -> bool {
        // approval_required 不算違規（是正常流程）
        if result.approval_required {
            return false;
        }

        // allowed 且無 violation_type 表示完全通過
        let violation_type = match &result.violation_type {
            Some(vt) => vt,
            None => return false,
        };

        // warn action: allowed=true but violation_type set → 記錄但不阻斷
        let is_actual_violation = !result.allowed
            || matches!(violation_type, ViolationType::RateExceeded)
                && result.allowed; // warn case

        if !is_actual_violation {
            return false;
        }

        let policy_id = result
            .policy_id
            .as_deref()
            .unwrap_or("unknown");

        // Determine policy_type string
        let policy_type_str = policies
            .iter()
            .find(|p| p.policy_id() == policy_id)
            .map(|p| p.type_name())
            .unwrap_or("unknown");

        let outcome = if result.allowed {
            "warned"
        } else {
            match violation_type {
                ViolationType::RateExceeded => "blocked",
                ViolationType::PermissionDenied => "blocked",
                ViolationType::QuotaExceeded => "blocked",
                ViolationType::ApprovalRequired => "pending",
                ViolationType::LifecycleViolation => "blocked",
            }
        };

        // Increment violation counter
        {
            let mut counts = self.violation_counts.write().await;
            *counts.entry(agent_id.to_string()).or_insert(0) += 1;
        }

        // Add to recent violations
        {
            let record = ViolationRecord {
                agent_id: agent_id.to_string(),
                policy_id: policy_id.to_string(),
                policy_type: policy_type_str.to_string(),
                violation_type: violation_type.clone(),
                operation_type: operation.op_type.to_string(),
                message: result.message.clone(),
                timestamp: chrono::Utc::now(),
            };
            let mut recent = self.recent_violations.write().await;
            recent.push(record);
            if recent.len() > self.max_recent {
                recent.remove(0);
            }
        }

        // Emit audit event (fire-and-forget)
        let event = GovernanceViolationEvent::new(
            agent_id,
            policy_id,
            policy_type_str,
            &result.message,
            operation.op_type.to_string(),
            outcome,
        );

        let sink = Arc::clone(&self.audit_sink);
        tokio::spawn(async move {
            sink.emit_governance_violation(event).await;
        });

        warn!(
            agent_id = agent_id,
            policy_id = policy_id,
            violation_type = %violation_type,
            outcome = outcome,
            "Governance violation detected"
        );

        true
    }

    /// 取得 Agent 的累計違規次數。
    pub async fn violation_count(&self, agent_id: &str) -> u32 {
        let counts = self.violation_counts.read().await;
        counts.get(agent_id).copied().unwrap_or(0)
    }

    /// 取得最近的違規記錄（按時間順序）。
    pub async fn recent_violations(&self) -> Vec<ViolationRecord> {
        self.recent_violations.read().await.clone()
    }

    /// 重設 Agent 的違規計數器（例如 Agent 重啟後）。
    pub async fn reset_violation_count(&self, agent_id: &str) {
        let mut counts = self.violation_counts.write().await;
        counts.remove(agent_id);
    }

    /// 檢查 Agent 是否達到自動暫停閾值。
    pub async fn should_auto_suspend(
        &self,
        agent_id: &str,
        policies: &[PolicyType],
    ) -> bool {
        let count = self.violation_count(agent_id).await;
        for policy in policies {
            if let PolicyType::Lifecycle(lp) = policy {
                if lp.auto_suspend_on_violation_count > 0
                    && count >= lp.auto_suspend_on_violation_count
                {
                    return true;
                }
            }
        }
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audit::RecordingAuditSink,
        evaluator::ViolationType,
        policy::{ActionOnViolation, LifecyclePolicy, RatePolicy, Resource},
        OperationType,
    };
    use std::sync::Arc;

    fn make_deny_result(policy_id: &str) -> EvaluationResult {
        EvaluationResult::deny(
            policy_id,
            ViolationType::RateExceeded,
            "rate limit exceeded",
        )
    }

    fn make_warn_result(policy_id: &str) -> EvaluationResult {
        EvaluationResult::warn(policy_id, "rate limit warning")
    }

    fn make_op() -> Operation {
        Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        }
    }

    fn make_policies(policy_id: &str) -> Vec<PolicyType> {
        vec![PolicyType::Rate(RatePolicy {
            policy_id: policy_id.to_string(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        })]
    }

    #[tokio::test]
    async fn test_violation_recorded_on_deny() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = make_deny_result("test-policy");
        let is_violation = detector
            .process_result("test-agent", &make_op(), &result, &make_policies("test-policy"))
            .await;

        assert!(is_violation);
        assert_eq!(detector.violation_count("test-agent").await, 1);
    }

    #[tokio::test]
    async fn test_violation_not_recorded_on_allow() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::allow();
        let is_violation = detector
            .process_result("test-agent", &make_op(), &result, &make_policies("test-policy"))
            .await;

        assert!(!is_violation);
        assert_eq!(detector.violation_count("test-agent").await, 0);
    }

    #[tokio::test]
    async fn test_warn_violation_counted() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = make_warn_result("test-policy");
        let is_violation = detector
            .process_result("test-agent", &make_op(), &result, &make_policies("test-policy"))
            .await;

        assert!(is_violation, "warn should still count as violation");
        assert_eq!(detector.violation_count("test-agent").await, 1);
    }

    #[tokio::test]
    async fn test_audit_event_emitted_on_violation() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = make_deny_result("test-policy");
        detector
            .process_result("my-agent", &make_op(), &result, &make_policies("test-policy"))
            .await;

        // Give tokio::spawn time to run
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let violations = sink.violations.lock().await;
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].agent_id, "my-agent");
        assert_eq!(violations[0].policy_id, "test-policy");
        assert_eq!(violations[0].outcome, "blocked");
    }

    #[tokio::test]
    async fn test_multiple_violations_accumulate() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policies = make_policies("p1");
        let op = make_op();

        for _ in 0..5 {
            let result = make_deny_result("p1");
            detector.process_result("agent-x", &op, &result, &policies).await;
        }

        assert_eq!(detector.violation_count("agent-x").await, 5);
    }

    #[tokio::test]
    async fn test_approval_required_not_violation() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::require_approval("perm-policy", "approval needed");
        let is_violation = detector
            .process_result("test-agent", &make_op(), &result, &[])
            .await;

        assert!(!is_violation, "approval_required should not be a violation");
        assert_eq!(detector.violation_count("test-agent").await, 0);
    }

    #[tokio::test]
    async fn test_should_auto_suspend() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let op = make_op();
        let policies = make_policies("p1");

        let lifecycle_policies = vec![PolicyType::Lifecycle(LifecyclePolicy {
            policy_id: "lc-1".into(),
            agent_id: "*".into(),
            max_idle_hours: 48,
            health_check_interval_seconds: 300,
            auto_suspend_on_violation_count: 3,
        })];

        // 2 violations — not yet
        for _ in 0..2 {
            let r = make_deny_result("p1");
            detector.process_result("at-risk-agent", &op, &r, &policies).await;
        }
        assert!(!detector.should_auto_suspend("at-risk-agent", &lifecycle_policies).await);

        // 3rd violation — triggers auto_suspend
        let r = make_deny_result("p1");
        detector.process_result("at-risk-agent", &op, &r, &policies).await;
        assert!(detector.should_auto_suspend("at-risk-agent", &lifecycle_policies).await);
    }

    #[tokio::test]
    async fn test_reset_violation_count() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policies = make_policies("p1");
        let op = make_op();

        for _ in 0..3 {
            let r = make_deny_result("p1");
            detector.process_result("reset-agent", &op, &r, &policies).await;
        }
        assert_eq!(detector.violation_count("reset-agent").await, 3);

        detector.reset_violation_count("reset-agent").await;
        assert_eq!(detector.violation_count("reset-agent").await, 0);
    }
}
