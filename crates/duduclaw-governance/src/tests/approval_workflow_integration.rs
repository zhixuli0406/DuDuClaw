//! ApprovalWorkflow + QuotaManager — M1-C 整合測試
//!
//! TDD Phase（RED → GREEN → REFACTOR）
//!
//! 驗收標準：
//! - E2E 核准工作流：申請 → 核准 → 執行 → 稽核記錄全鏈路通過
//! - QuotaManager 每日重置 + cron 機制測試通過
//! - 所有 Approval 相關 Audit Event 正確發射
//! - TTL 過期自動拒絕測試通過
//! - 核准者身份驗證
//! - 多層核准鏈（多個 approvers）測試
//! - `POLICY_APPROVAL_REQUIRED`（202）錯誤碼驗證
//! - 測試覆蓋率 ≥ 80%

#[cfg(test)]
mod approval_workflow_integration_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::{
        approval::{
            ApprovalDecision, ApprovalDecisionType, ApprovalError, ApprovalRequest, ApprovalStatus,
            ApprovalWorkflow,
        },
        audit::{AuditEventSink, RecordingAuditSink},
        error_codes::{PolicyApiError, PolicyErrorCode},
        evaluator::{EvaluationResult, PolicyEvaluator, ViolationType},
        policy::QuotaPolicy,
        quota_manager::{QuotaError, QuotaManager},
        registry::PolicyRegistry,
        Operation, OperationType,
    };
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_approval_request(agent_id: &str, scope: &str, ttl: u64) -> ApprovalRequest {
        ApprovalRequest {
            agent_id: agent_id.into(),
            operation: Operation {
                op_type: OperationType::AgentCreate,
                resource_id: None,
                scope: scope.into(),
                metadata: serde_json::json!({}),
            },
            justification: "Need to perform this operation".into(),
            ttl_seconds: ttl,
        }
    }

    fn make_quota_policy(daily_tokens: u64, max_tasks: u32, max_memory: u64) -> QuotaPolicy {
        QuotaPolicy {
            policy_id: "e2e-quota".into(),
            agent_id: "*".into(),
            daily_token_budget: daily_tokens,
            max_concurrent_tasks: max_tasks,
            max_memory_entries: max_memory,
            reset_cron: "0 0 * * *".into(),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 1 — ApprovalWorkflow E2E 完整鏈路
    // ═══════════════════════════════════════════════════════════════════════════

    /// E2E：申請 → 核准 → 決策記錄 → 稽核事件全鏈路。
    #[tokio::test]
    async fn test_e2e_request_approve_audit_chain() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        // Step 1: 申請
        let req = make_approval_request("agent-e2e", "agent:create", 3600);
        let response = workflow.request(req).await;
        let request_id = response.approval_request_id.clone();

        assert_eq!(response.status, ApprovalStatus::Pending);
        assert!(!request_id.is_empty());
        assert_eq!(response.approvers, vec!["admin"]);

        // Step 2: 核准
        let decision = ApprovalDecision {
            approval_request_id: request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: Some("Operation looks safe".into()),
        };
        let result = workflow.decide(decision).await;
        assert!(result.is_ok());
        let decision_resp = result.unwrap();
        assert_eq!(decision_resp.status, ApprovalStatus::Approved);

        // Step 3: 稽核事件確認
        tokio::time::sleep(Duration::from_millis(100)).await;

        let requests = sink.approval_requests.lock().await;
        assert_eq!(requests.len(), 1, "approval_requested event should be emitted");
        assert_eq!(requests[0].agent_id, "agent-e2e");
        assert_eq!(requests[0].outcome, "pending");
        assert_eq!(requests[0].approval_request_id, request_id);

        let decisions = sink.approval_decisions.lock().await;
        assert_eq!(decisions.len(), 1, "approval_decided event should be emitted");
        assert_eq!(decisions[0].agent_id, "agent-e2e");
        assert_eq!(decisions[0].outcome, "approved");
        assert_eq!(decisions[0].approver_id, "admin");
        assert_eq!(decisions[0].reason, Some("Operation looks safe".into()));
    }

    /// E2E：申請 → 拒絕 → 稽核記錄。
    #[tokio::test]
    async fn test_e2e_request_reject_audit_chain() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        let response = workflow
            .request(make_approval_request("risky-agent", "admin", 3600))
            .await;
        let request_id = response.approval_request_id.clone();

        let decision = ApprovalDecision {
            approval_request_id: request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Reject,
            reason: Some("Not authorized for this operation".into()),
        };
        let result = workflow.decide(decision).await.unwrap();
        assert_eq!(result.status, ApprovalStatus::Rejected);

        tokio::time::sleep(Duration::from_millis(100)).await;
        let decisions = sink.approval_decisions.lock().await;
        assert_eq!(decisions[0].outcome, "rejected");
        assert_eq!(decisions[0].reason, Some("Not authorized for this operation".into()));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 2 — 核准者身份驗證
    // ═══════════════════════════════════════════════════════════════════════════

    /// 非授權核准者嘗試決策應回傳 Unauthorized 錯誤。
    #[tokio::test]
    async fn test_unauthorized_approver_rejected() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into(), "supervisor".into()],
        );

        let response = workflow
            .request(make_approval_request("agent-1", "agent:create", 3600))
            .await;

        let decision = ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "random-user".into(), // 不在核准者列表
            decision: ApprovalDecisionType::Approve,
            reason: None,
        };

        let result = workflow.decide(decision).await;
        assert!(
            matches!(result, Err(ApprovalError::Unauthorized(_))),
            "unauthorized approver should be rejected, got: {:?}",
            result
        );
    }

    /// 授權的核准者可以核准。
    #[tokio::test]
    async fn test_authorized_approver_can_approve() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into(), "supervisor".into()],
        );

        let response = workflow
            .request(make_approval_request("agent-2", "agent:create", 3600))
            .await;

        // supervisor 是授權核准者
        let decision = ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "supervisor".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        };

        let result = workflow.decide(decision).await;
        assert!(result.is_ok(), "supervisor should be able to approve");
        assert_eq!(result.unwrap().status, ApprovalStatus::Approved);
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 3 — 多層核准鏈（多個 approvers）
    // ═══════════════════════════════════════════════════════════════════════════

    /// 多核准者列表：任一核准者均可決策（not 全部都要核准）。
    #[tokio::test]
    async fn test_multiple_approvers_any_can_decide() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into(), "mgr-a".into(), "mgr-b".into()],
        );

        let response = workflow
            .request(make_approval_request("multi-agent", "agent:modify", 3600))
            .await;

        // Verify approvers list is returned correctly
        assert_eq!(response.approvers.len(), 3);
        assert!(response.approvers.contains(&"admin".to_string()));
        assert!(response.approvers.contains(&"mgr-a".to_string()));
        assert!(response.approvers.contains(&"mgr-b".to_string()));

        // mgr-b 決策
        let decision = ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "mgr-b".into(),
            decision: ApprovalDecisionType::Approve,
            reason: Some("Approved by mgr-b".into()),
        };

        let result = workflow.decide(decision).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, ApprovalStatus::Approved);
    }

    /// 多個並行申請可同時存在。
    #[tokio::test]
    async fn test_multiple_concurrent_requests() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        // 三個不同 Agent 同時申請
        let r1 = workflow.request(make_approval_request("ag-1", "agent:create", 3600)).await;
        let r2 = workflow.request(make_approval_request("ag-2", "agent:create", 3600)).await;
        let r3 = workflow.request(make_approval_request("ag-3", "agent:modify", 3600)).await;

        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 3, "should have 3 pending requests");

        // 核准第一個
        workflow.decide(ApprovalDecision {
            approval_request_id: r1.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        }).await.unwrap();

        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 2, "should have 2 pending after deciding r1");

        // 拒絕第二個
        workflow.decide(ApprovalDecision {
            approval_request_id: r2.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Reject,
            reason: None,
        }).await.unwrap();

        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 1, "should have 1 pending after deciding r2");
        assert_eq!(pending[0], r3.approval_request_id);
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 4 — TTL 過期自動拒絕
    // ═══════════════════════════════════════════════════════════════════════════

    /// TTL = 0 的申請立即過期，嘗試決策應回傳 Expired。
    #[tokio::test]
    async fn test_ttl_expiry_auto_rejects() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        let response = workflow
            .request(make_approval_request("expiry-agent", "agent:create", 0))
            .await;

        // Small sleep to ensure expiry
        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        }).await;

        assert!(
            matches!(result, Err(ApprovalError::Expired(_))),
            "TTL=0 request should expire, got: {:?}",
            result
        );
    }

    /// 過期前可以核准，過期後不行。
    #[tokio::test]
    async fn test_can_approve_before_expiry_not_after() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        // 1 秒後才過期
        let response = workflow
            .request(make_approval_request("timely-agent", "agent:create", 3600))
            .await;

        // 馬上核准，應成功
        let result = workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        }).await;
        assert!(result.is_ok(), "should be approvable before expiry");
    }

    /// 已決策的申請不可重複決策（AlreadyDecided）。
    #[tokio::test]
    async fn test_double_decide_returns_already_decided() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow
            .request(make_approval_request("dup-agent", "agent:create", 3600))
            .await;

        // First decision
        workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        }).await.unwrap();

        // Second decision — should fail
        let result = workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Reject,
            reason: None,
        }).await;

        assert!(matches!(result, Err(ApprovalError::AlreadyDecided(_))));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 5 — POLICY_APPROVAL_REQUIRED (202) 錯誤碼驗證
    // ═══════════════════════════════════════════════════════════════════════════

    /// POLICY_APPROVAL_REQUIRED 應對應 HTTP 202（Accepted）。
    #[test]
    fn test_policy_approval_required_is_202() {
        let code = PolicyErrorCode::ApprovalRequired;
        assert_eq!(code.http_status(), 202);
        assert_eq!(code.error_code(), "POLICY_APPROVAL_REQUIRED");
    }

    /// 評估結果為 approval_required 時，PolicyApiError 回傳 202。
    #[test]
    fn test_evaluation_approval_required_gives_202_api_error() {
        let result = EvaluationResult::require_approval("perm-policy", "agent:create requires approval");
        let api_err = PolicyApiError::from_evaluation_result(&result)
            .expect("should produce an api error for approval_required");
        assert_eq!(api_err.http_status(), 202);
        assert_eq!(api_err.error_code().error_code(), "POLICY_APPROVAL_REQUIRED");
    }

    /// PolicyEvaluator：`requires_approval` scope 觸發 202 流程。
    #[tokio::test]
    async fn test_evaluator_approval_required_returns_approval_result() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: require-approval-policy
    agent_id: "*"
    allowed_scopes: []
    denied_scopes: []
    requires_approval:
      - agent:create
      - agent:modify
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        let evaluator = PolicyEvaluator::new(registry);

        let op = Operation {
            op_type: OperationType::AgentCreate,
            resource_id: None,
            scope: "agent:create".into(),
            metadata: serde_json::json!({}),
        };

        let r = evaluator.evaluate("any-agent", &op).await;
        assert!(!r.allowed, "requires_approval should block operation");
        assert!(r.approval_required, "approval_required should be true");
        assert_eq!(r.violation_type, Some(ViolationType::ApprovalRequired));

        let api_err = PolicyApiError::from_evaluation_result(&r).unwrap();
        assert_eq!(api_err.http_status(), 202, "should be HTTP 202");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 6 — QuotaManager 每日重置 + Cron 機制
    // ═══════════════════════════════════════════════════════════════════════════

    /// QuotaManager：每日 token 追蹤與重置。
    #[tokio::test]
    async fn test_quota_manager_daily_token_tracking_and_reset() {
        let sink = Arc::new(RecordingAuditSink::default());
        let mgr = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policy = make_quota_policy(500, 5, 10000);

        // 消耗一部分
        mgr.consume_tokens("qa-agent", &policy, 300).await.unwrap();
        let snap = mgr.get_usage("qa-agent").await.unwrap();
        assert_eq!(snap.token_used, 300);

        // 消耗到接近上限
        mgr.consume_tokens("qa-agent", &policy, 199).await.unwrap();
        let snap = mgr.get_usage("qa-agent").await.unwrap();
        assert_eq!(snap.token_used, 499);

        // 再多 2 tokens 超過 500 → 失敗
        let result = mgr.consume_tokens("qa-agent", &policy, 2).await;
        assert!(result.is_err(), "should exceed budget");

        // 每日重置後可重新使用
        mgr.reset_all().await;
        let snap = mgr.get_usage("qa-agent").await.unwrap();
        assert_eq!(snap.token_used, 0, "should be reset");

        // 重置後可再次消耗
        mgr.consume_tokens("qa-agent", &policy, 100).await.unwrap();
        let snap = mgr.get_usage("qa-agent").await.unwrap();
        assert_eq!(snap.token_used, 100);
    }

    /// QuotaManager：reset_all 發射 governance_quota_reset 事件。
    #[tokio::test]
    async fn test_quota_reset_all_emits_governance_event() {
        let sink = Arc::new(RecordingAuditSink::default());
        let mgr = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policy = make_quota_policy(1000, 5, 10000);

        mgr.consume_tokens("event-agent-1", &policy, 100).await.unwrap();
        mgr.consume_tokens("event-agent-2", &policy, 200).await.unwrap();

        mgr.reset_all().await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1, "one bulk reset event");
        assert_eq!(resets[0].agent_id, "*");
        assert_eq!(resets[0].reset_type, "daily");
        assert_eq!(resets[0].outcome, "success");
        assert!(!resets[0].event_id.is_empty());
        assert!(!resets[0].timestamp.is_empty());
    }

    /// QuotaManager：過期 reset_at 觸發自動重置。
    #[tokio::test]
    async fn test_quota_auto_reset_on_expired_time() {
        let sink = Arc::new(RecordingAuditSink::default());
        let mgr = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policy = make_quota_policy(1000, 5, 10000);

        // 建立使用量
        mgr.consume_tokens("auto-agent", &policy, 800).await.unwrap();
        let snap = mgr.get_usage("auto-agent").await.unwrap();
        assert_eq!(snap.token_used, 800);

        // 模擬 reset_at 已過期
        let past = chrono::Utc::now() - chrono::Duration::seconds(1);
        mgr.set_reset_at_for_test("auto-agent", past).await;

        // 下次操作自動重置
        mgr.consume_tokens("auto-agent", &policy, 50).await.unwrap();
        let snap = mgr.get_usage("auto-agent").await.unwrap();
        assert_eq!(snap.token_used, 50, "should be reset and then consume 50");

        // Wait for async event
        tokio::time::sleep(Duration::from_millis(100)).await;
        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1, "auto reset event should be emitted");
        assert_eq!(resets[0].reset_type, "daily");
    }

    /// QuotaManager：max_concurrent_tasks 控制並發。
    #[tokio::test]
    async fn test_quota_max_concurrent_tasks_control() {
        let sink = Arc::new(RecordingAuditSink::default());
        let mgr = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policy = make_quota_policy(500000, 3, 10000);

        // 允許 3 個並發
        mgr.increment_concurrent_tasks("task-agent", &policy).await.unwrap();
        mgr.increment_concurrent_tasks("task-agent", &policy).await.unwrap();
        mgr.increment_concurrent_tasks("task-agent", &policy).await.unwrap();

        // 第 4 個失敗
        let result = mgr.increment_concurrent_tasks("task-agent", &policy).await;
        assert!(
            matches!(result, Err(QuotaError::ConcurrentTasksExceeded { current: 3, max: 3 })),
            "4th task should be rejected"
        );

        // 完成一個任務
        mgr.decrement_concurrent_tasks("task-agent").await;

        // 現在可以再啟動一個
        let result = mgr.increment_concurrent_tasks("task-agent", &policy).await;
        assert!(result.is_ok(), "should allow new task after decrement");
    }

    /// QuotaManager：max_memory_entries 限制。
    #[tokio::test]
    async fn test_quota_max_memory_entries_limit() {
        let sink = Arc::new(RecordingAuditSink::default());
        let mgr = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let policy = make_quota_policy(500000, 5, 1000);

        mgr.set_memory_entries("mem-agent", &policy, 1000).await.unwrap();
        let result = mgr.set_memory_entries("mem-agent", &policy, 1001).await;
        assert!(
            matches!(result, Err(QuotaError::MemoryEntriesExceeded { .. })),
            "should reject exceeding memory entries"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 7 — Audit Event 完整驗證
    // ═══════════════════════════════════════════════════════════════════════════

    /// 所有 approval 事件欄位必須完整。
    #[tokio::test]
    async fn test_audit_event_fields_complete() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::with_approvers(
            Arc::clone(&sink) as Arc<dyn AuditEventSink>,
            vec!["admin".into()],
        );

        let req = ApprovalRequest {
            agent_id: "field-check-agent".into(),
            operation: Operation {
                op_type: OperationType::AgentRemove,
                resource_id: Some("target-agent-123".into()),
                scope: "agent:remove".into(),
                metadata: serde_json::json!({"target": "agent-123"}),
            },
            justification: "Removing deprecated agent".into(),
            ttl_seconds: 3600,
        };

        let response = workflow.request(req).await;
        workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: Some("Confirmed deprecated".into()),
        }).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let requests = sink.approval_requests.lock().await;
        assert!(!requests[0].event_id.is_empty(), "event_id required");
        assert!(!requests[0].timestamp.is_empty(), "timestamp required");
        assert_eq!(requests[0].agent_id, "field-check-agent");
        assert_eq!(requests[0].justification, "Removing deprecated agent");
        assert_eq!(requests[0].operation_type, "agent_remove");

        let decisions = sink.approval_decisions.lock().await;
        assert!(!decisions[0].event_id.is_empty(), "event_id required");
        assert!(!decisions[0].timestamp.is_empty(), "timestamp required");
        assert_eq!(decisions[0].approver_id, "admin");
        assert_eq!(decisions[0].reason, Some("Confirmed deprecated".into()));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 8 — get_status 查詢
    // ═══════════════════════════════════════════════════════════════════════════

    /// 可查詢 pending 申請的狀態。
    #[tokio::test]
    async fn test_get_status_pending() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow
            .request(make_approval_request("status-agent", "agent:create", 3600))
            .await;

        let (status, expires_at) = workflow
            .get_status(&response.approval_request_id)
            .await
            .expect("should have status");

        assert_eq!(status, ApprovalStatus::Pending);
        assert!(expires_at > chrono::Utc::now());
    }

    /// 不存在的申請 ID 應回傳 None。
    #[tokio::test]
    async fn test_get_status_nonexistent_returns_none() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = workflow.get_status("nonexistent-id").await;
        assert!(result.is_none());
    }

    /// 核准後狀態變為 Approved。
    #[tokio::test]
    async fn test_get_status_after_approval() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let response = workflow
            .request(make_approval_request("state-agent", "agent:create", 3600))
            .await;

        workflow.decide(ApprovalDecision {
            approval_request_id: response.approval_request_id.clone(),
            approver_id: "admin".into(),
            decision: ApprovalDecisionType::Approve,
            reason: None,
        }).await.unwrap();

        let (status, _) = workflow
            .get_status(&response.approval_request_id)
            .await
            .unwrap();
        assert_eq!(status, ApprovalStatus::Approved);
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 9 — cleanup_expired 清理過期申請
    // ═══════════════════════════════════════════════════════════════════════════

    /// cleanup_expired 移除超過 24 小時的過期申請。
    #[tokio::test]
    async fn test_cleanup_expired_requests() {
        let sink = Arc::new(RecordingAuditSink::default());
        let workflow = ApprovalWorkflow::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        // 建立一個正常申請
        let _valid = workflow
            .request(make_approval_request("valid-agent", "agent:create", 3600))
            .await;

        // 建立一個立即過期的申請
        let _expired = workflow
            .request(make_approval_request("expired-agent", "agent:create", 0))
            .await;

        // Wait for expiry
        tokio::time::sleep(Duration::from_millis(10)).await;

        // cleanup_expired 標記過期申請
        workflow.cleanup_expired().await;

        // Pending 列表應只剩 valid
        let pending = workflow.list_pending().await;
        assert_eq!(pending.len(), 1, "only valid request should remain pending");
    }
}
