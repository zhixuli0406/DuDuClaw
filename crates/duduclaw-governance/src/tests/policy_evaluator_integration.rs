//! PolicyEvaluator + ViolationDetector — M1-B 整合測試
//!
//! TDD Phase（RED → GREEN → REFACTOR）
//!
//! 驗收標準：
//! - PolicyEvaluator.evaluate() p99 < 5ms（本地快取命中）
//! - Sliding window 速率限制精確度誤差 < 1%
//! - LifecyclePolicy idle 時間計算正確
//! - 違規事件自動發射至 Audit Trail
//! - HTTP 錯誤碼對應（403 / 404 / 409 / 422）
//! - Policy upsert 需要 `admin` 或 `governance:write` scope 驗證
//! - 測試覆蓋率 ≥ 80%

#[cfg(test)]
mod policy_evaluator_integration_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::{
        audit::{AuditEventSink, RecordingAuditSink},
        error_codes::{PolicyApiError, PolicyErrorCode},
        evaluator::{EvaluationResult, PolicyEvaluator, ViolationType},
        policy::{ActionOnViolation, LifecyclePolicy, PolicyError, PolicyType, RatePolicy, Resource},
        registry::PolicyRegistry,
        violation::ViolationDetector,
        Operation, OperationType,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    async fn setup_registry(yaml: &str) -> (Arc<PolicyRegistry>, TempDir) {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        (registry, dir)
    }

    fn mcp_op() -> Operation {
        Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        }
    }

    fn memory_write_op() -> Operation {
        Operation {
            op_type: OperationType::MemoryWrite,
            resource_id: None,
            scope: "memory:write".into(),
            metadata: serde_json::json!({}),
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 1 — PolicyEvaluator 評估邏輯（Sliding Window + Permission + Quota）
    // ═══════════════════════════════════════════════════════════════════════════

    /// Rate limit：前 N 次允許，第 N+1 次拒絕（reject action）。
    #[tokio::test]
    async fn test_rate_policy_allows_up_to_limit_then_rejects() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: rate-3
    agent_id: "*"
    resource: mcp_calls
    limit: 3
    window_seconds: 60
    action_on_violation: reject
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));
        let op = mcp_op();

        for i in 1..=3 {
            let r = evaluator.evaluate("agent-1", &op).await;
            assert!(r.allowed, "call {i}: should be allowed");
            assert!(r.violation_type.is_none());
        }
        let r = evaluator.evaluate("agent-1", &op).await;
        assert!(!r.allowed, "4th call should be rejected");
        assert_eq!(r.violation_type, Some(ViolationType::RateExceeded));
        assert_eq!(r.policy_id.as_deref(), Some("rate-3"));
    }

    /// Rate limit：warn action 允許通過但標記違規類型。
    #[tokio::test]
    async fn test_rate_policy_warn_action_allows_but_marks_violation() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: rate-warn
    agent_id: "*"
    resource: mcp_calls
    limit: 2
    window_seconds: 60
    action_on_violation: warn
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));
        let op = mcp_op();

        evaluator.evaluate("warn-agent", &op).await;
        evaluator.evaluate("warn-agent", &op).await;

        let r = evaluator.evaluate("warn-agent", &op).await;
        assert!(r.allowed, "warn action must still allow");
        assert_eq!(r.violation_type, Some(ViolationType::RateExceeded));
    }

    /// Rate limit：不同 agent 計數器獨立（agent-a 超限不影響 agent-b）。
    #[tokio::test]
    async fn test_rate_counters_isolated_per_agent() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: rate-iso
    agent_id: "*"
    resource: memory_writes
    limit: 2
    window_seconds: 60
    action_on_violation: reject
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));
        let op = memory_write_op();

        // agent-a 耗盡 2 次額度
        evaluator.evaluate("agent-a", &op).await;
        evaluator.evaluate("agent-a", &op).await;
        let blocked = evaluator.evaluate("agent-a", &op).await;
        assert!(!blocked.allowed, "agent-a should be blocked");

        // agent-b 應仍然允許
        let ok = evaluator.evaluate("agent-b", &op).await;
        assert!(ok.allowed, "agent-b should not be affected");
    }

    /// Sliding window 精確度：100 次請求後視窗計數應為 100。
    #[tokio::test]
    async fn test_sliding_window_accuracy_within_1_percent() {
        use crate::evaluator::SlidingWindow;

        let mut window = SlidingWindow::default();
        let w = Duration::from_secs(60);

        for _ in 0..100 {
            window.record_and_check(w, 200);
        }
        let count = window.current_count(w);
        // 誤差 < 1%：100 ± 1
        assert!(count >= 99, "count should be at least 99, got {count}");
        assert!(count <= 101, "count should be at most 101, got {count}");
    }

    /// Permission：允許 scope 通過。
    #[tokio::test]
    async fn test_permission_allows_allowed_scope() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: perm-1
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
    denied_scopes:
      - admin
    requires_approval: []
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        let op = Operation {
            op_type: OperationType::MemoryRead,
            resource_id: None,
            scope: "memory:read".into(),
            metadata: serde_json::json!({}),
        };
        let r = evaluator.evaluate("any-agent", &op).await;
        assert!(r.allowed, "memory:read should be allowed");
    }

    /// Permission：denied_scopes 中的 scope 被拒絕（POLICY_PERMISSION_DENIED）。
    #[tokio::test]
    async fn test_permission_denies_admin_scope() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: perm-deny
    agent_id: "*"
    allowed_scopes: []
    denied_scopes:
      - admin
    requires_approval: []
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        let op = Operation {
            op_type: OperationType::AgentCreate,
            resource_id: None,
            scope: "admin".into(),
            metadata: serde_json::json!({}),
        };
        let r = evaluator.evaluate("any-agent", &op).await;
        assert!(!r.allowed);
        assert_eq!(r.violation_type, Some(ViolationType::PermissionDenied));
    }

    /// Permission：requires_approval scope 觸發 ApprovalRequired（非違規）。
    #[tokio::test]
    async fn test_permission_scope_requires_approval() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: perm-approval
    agent_id: "*"
    allowed_scopes: []
    denied_scopes: []
    requires_approval:
      - agent:create
      - agent:modify
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        let op = Operation {
            op_type: OperationType::AgentCreate,
            resource_id: None,
            scope: "agent:create".into(),
            metadata: serde_json::json!({}),
        };
        let r = evaluator.evaluate("any-agent", &op).await;
        assert!(!r.allowed);
        assert!(r.approval_required);
        assert_eq!(r.violation_type, Some(ViolationType::ApprovalRequired));
    }

    /// Quota：每日 token 預算耗盡觸發 QuotaExceeded。
    #[tokio::test]
    async fn test_quota_token_budget_exhausted() {
        let yaml = r#"
policies:
  - policy_type: quota
    policy_id: quota-tight
    agent_id: "*"
    daily_token_budget: 100
    max_concurrent_tasks: 10
    max_memory_entries: 50000
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        evaluator.record_tokens_used("budget-agent", 100).await;
        let r = evaluator.evaluate("budget-agent", &mcp_op()).await;
        assert!(!r.allowed);
        assert_eq!(r.violation_type, Some(ViolationType::QuotaExceeded));
    }

    /// Quota：並發任務超限觸發 QuotaExceeded。
    #[tokio::test]
    async fn test_quota_concurrent_tasks_exceeded() {
        let yaml = r#"
policies:
  - policy_type: quota
    policy_id: quota-conc
    agent_id: "*"
    daily_token_budget: 999999
    max_concurrent_tasks: 2
    max_memory_entries: 50000
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        evaluator.increment_concurrent_tasks("conc-agent").await;
        evaluator.increment_concurrent_tasks("conc-agent").await;

        let r = evaluator.evaluate("conc-agent", &mcp_op()).await;
        assert!(!r.allowed);
        assert_eq!(r.violation_type, Some(ViolationType::QuotaExceeded));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 2 — LifecyclePolicy 評估（idle 時間計算）
    // ═══════════════════════════════════════════════════════════════════════════

    /// 無 LifecyclePolicy 時，操作應通過。
    #[tokio::test]
    async fn test_lifecycle_no_policy_allows_all() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        let evaluator = PolicyEvaluator::new(registry);

        let r = evaluator.evaluate("fresh-agent", &mcp_op()).await;
        assert!(r.allowed, "no lifecycle policy → should allow");
    }

    /// LifecyclePolicy：活躍 Agent（idle < max_idle_hours）應通過。
    #[tokio::test]
    async fn test_lifecycle_active_agent_allowed() {
        let yaml = r#"
policies:
  - policy_type: lifecycle
    policy_id: lifecycle-48h
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        // 記錄近期活動（剛剛活躍）
        evaluator.record_activity("active-agent").await;

        let r = evaluator.evaluate("active-agent", &mcp_op()).await;
        assert!(r.allowed, "recently active agent should be allowed");
    }

    /// LifecyclePolicy：idle 超過 max_idle_hours 的 Agent 應被拒絕。
    #[tokio::test]
    async fn test_lifecycle_idle_agent_rejected() {
        let yaml = r#"
policies:
  - policy_type: lifecycle
    policy_id: lifecycle-short
    agent_id: "*"
    max_idle_hours: 1
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        // 直接設定一個超過 1 小時的舊活動時間
        evaluator
            .set_last_activity_for_test(
                "idle-agent",
                std::time::Instant::now() - Duration::from_secs(3601),
            )
            .await;

        let r = evaluator.evaluate("idle-agent", &mcp_op()).await;
        assert!(!r.allowed, "idle agent should be rejected");
        assert_eq!(r.violation_type, Some(ViolationType::LifecycleViolation));
        assert_eq!(r.policy_id.as_deref(), Some("lifecycle-short"));
    }

    /// LifecyclePolicy：新 Agent（無活動記錄）應通過（預設允許）。
    #[tokio::test]
    async fn test_lifecycle_new_agent_allowed_by_default() {
        let yaml = r#"
policies:
  - policy_type: lifecycle
    policy_id: lifecycle-48h
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        // 不呼叫 record_activity → 視為新 agent
        let r = evaluator.evaluate("brand-new-agent", &mcp_op()).await;
        assert!(r.allowed, "new agent with no activity record should be allowed");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 3 — HTTP 錯誤碼對應測試
    // ═══════════════════════════════════════════════════════════════════════════

    /// POLICY_RATE_EXCEEDED → HTTP 403。
    #[test]
    fn test_error_code_rate_exceeded_is_403() {
        let code = PolicyErrorCode::RateExceeded;
        assert_eq!(code.http_status(), 403);
        assert_eq!(code.error_code(), "POLICY_RATE_EXCEEDED");
    }

    /// POLICY_PERMISSION_DENIED → HTTP 403。
    #[test]
    fn test_error_code_permission_denied_is_403() {
        let code = PolicyErrorCode::PermissionDenied;
        assert_eq!(code.http_status(), 403);
        assert_eq!(code.error_code(), "POLICY_PERMISSION_DENIED");
    }

    /// POLICY_QUOTA_EXCEEDED → HTTP 403。
    #[test]
    fn test_error_code_quota_exceeded_is_403() {
        let code = PolicyErrorCode::QuotaExceeded;
        assert_eq!(code.http_status(), 403);
        assert_eq!(code.error_code(), "POLICY_QUOTA_EXCEEDED");
    }

    /// POLICY_NOT_FOUND → HTTP 404。
    #[test]
    fn test_error_code_not_found_is_404() {
        let code = PolicyErrorCode::NotFound;
        assert_eq!(code.http_status(), 404);
        assert_eq!(code.error_code(), "POLICY_NOT_FOUND");
    }

    /// POLICY_CONFLICT → HTTP 409。
    #[test]
    fn test_error_code_conflict_is_409() {
        let code = PolicyErrorCode::Conflict;
        assert_eq!(code.http_status(), 409);
        assert_eq!(code.error_code(), "POLICY_CONFLICT");
    }

    /// POLICY_INVALID_SCHEMA → HTTP 422。
    #[test]
    fn test_error_code_invalid_schema_is_422() {
        let code = PolicyErrorCode::InvalidSchema;
        assert_eq!(code.http_status(), 422);
        assert_eq!(code.error_code(), "POLICY_INVALID_SCHEMA");
    }

    /// POLICY_APPROVAL_REQUIRED → HTTP 202（Accepted，不是 4xx）。
    #[test]
    fn test_error_code_approval_required_is_202() {
        let code = PolicyErrorCode::ApprovalRequired;
        assert_eq!(code.http_status(), 202);
        assert_eq!(code.error_code(), "POLICY_APPROVAL_REQUIRED");
    }

    /// EvaluationResult → PolicyErrorCode 對應測試。
    #[test]
    fn test_evaluation_result_to_error_code_mapping() {
        // Rate exceeded → RateExceeded
        let r = EvaluationResult::deny("p1", ViolationType::RateExceeded, "rate exceeded");
        let api_err = PolicyApiError::from_evaluation_result(&r).expect("should have error");
        assert_eq!(api_err.error_code().http_status(), 403);
        assert_eq!(api_err.error_code().error_code(), "POLICY_RATE_EXCEEDED");

        // Permission denied → PermissionDenied
        let r = EvaluationResult::deny("p1", ViolationType::PermissionDenied, "permission denied");
        let api_err = PolicyApiError::from_evaluation_result(&r).expect("should have error");
        assert_eq!(api_err.error_code().error_code(), "POLICY_PERMISSION_DENIED");

        // Quota exceeded → QuotaExceeded
        let r = EvaluationResult::deny("p1", ViolationType::QuotaExceeded, "quota exceeded");
        let api_err = PolicyApiError::from_evaluation_result(&r).expect("should have error");
        assert_eq!(api_err.error_code().error_code(), "POLICY_QUOTA_EXCEEDED");

        // Allow → None
        let r = EvaluationResult::allow();
        let code = PolicyApiError::from_evaluation_result(&r);
        assert!(code.is_none(), "allowed result should have no error code");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 4 — ViolationDetector 違規事件發射
    // ═══════════════════════════════════════════════════════════════════════════

    /// 違規後 governance_violation 事件自動發射至 Audit Trail。
    #[tokio::test]
    async fn test_violation_detector_emits_audit_event() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::deny("test-p", ViolationType::RateExceeded, "exceeded");
        let op = mcp_op();
        let policies = vec![PolicyType::Rate(RatePolicy {
            policy_id: "test-p".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        })];

        detector.process_result("audit-agent", &op, &result, &policies).await;

        // 等待 tokio::spawn 完成
        tokio::time::sleep(Duration::from_millis(50)).await;

        let violations = sink.violations.lock().await;
        assert_eq!(violations.len(), 1, "one violation event should be emitted");
        assert_eq!(violations[0].agent_id, "audit-agent");
        assert_eq!(violations[0].policy_id, "test-p");
        assert_eq!(violations[0].policy_type, "rate");
        assert_eq!(violations[0].operation_type, "mcp_call");
        assert_eq!(violations[0].outcome, "blocked");
    }

    /// 違規事件 metadata 包含必要欄位。
    #[tokio::test]
    async fn test_violation_event_metadata_contains_required_fields() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::deny(
            "meta-policy",
            ViolationType::PermissionDenied,
            "scope denied",
        );
        let op = Operation {
            op_type: OperationType::WikiWrite,
            resource_id: None,
            scope: "admin".into(),
            metadata: serde_json::json!({}),
        };

        let policies = vec![PolicyType::Rate(RatePolicy {
            policy_id: "meta-policy".into(),
            agent_id: "*".into(),
            resource: Resource::WikiWrites,
            limit: 20,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        })];

        detector.process_result("meta-agent", &op, &result, &policies).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let violations = sink.violations.lock().await;
        assert!(!violations[0].event_id.is_empty(), "event_id should be set");
        assert!(!violations[0].timestamp.is_empty(), "timestamp should be set");
        assert_eq!(violations[0].violation_detail, "scope denied");
    }

    /// Warn action 違規計入計數器但不阻斷。
    #[tokio::test]
    async fn test_violation_warn_counted_not_blocked() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::warn("warn-p", "rate warning");
        let op = mcp_op();
        let policies = vec![PolicyType::Rate(RatePolicy {
            policy_id: "warn-p".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Warn,
        })];

        let is_violation = detector
            .process_result("warn-agent", &op, &result, &policies)
            .await;
        assert!(is_violation, "warn should be counted as violation");
        assert_eq!(detector.violation_count("warn-agent").await, 1);
    }

    /// approval_required 不觸發違規計數。
    #[tokio::test]
    async fn test_approval_required_not_counted_as_violation() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);

        let result = EvaluationResult::require_approval("perm-p", "needs approval");
        let op = mcp_op();

        let is_violation = detector.process_result("app-agent", &op, &result, &[]).await;
        assert!(!is_violation, "approval_required is not a violation");
        assert_eq!(detector.violation_count("app-agent").await, 0);
    }

    /// 自動暫停閾值檢查：達到 auto_suspend_on_violation_count 時觸發。
    #[tokio::test]
    async fn test_auto_suspend_threshold() {
        let sink = Arc::new(RecordingAuditSink::default());
        let detector = ViolationDetector::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        let op = mcp_op();
        let policies = vec![PolicyType::Rate(RatePolicy {
            policy_id: "p1".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        })];
        let lifecycle_policies = vec![PolicyType::Lifecycle(LifecyclePolicy {
            policy_id: "lc-1".into(),
            agent_id: "*".into(),
            max_idle_hours: 48,
            health_check_interval_seconds: 300,
            auto_suspend_on_violation_count: 3,
        })];

        for _ in 0..2 {
            let r = EvaluationResult::deny("p1", ViolationType::RateExceeded, "exceeded");
            detector.process_result("risk-agent", &op, &r, &policies).await;
        }
        assert!(
            !detector.should_auto_suspend("risk-agent", &lifecycle_policies).await,
            "2 violations should not trigger auto suspend"
        );

        let r = EvaluationResult::deny("p1", ViolationType::RateExceeded, "exceeded");
        detector.process_result("risk-agent", &op, &r, &policies).await;
        assert!(
            detector.should_auto_suspend("risk-agent", &lifecycle_policies).await,
            "3 violations should trigger auto suspend"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 5 — Policy Upsert 端點權限驗證
    // ═══════════════════════════════════════════════════════════════════════════

    /// upsert 需要 admin scope：無權限時回傳 PermissionDenied 錯誤。
    #[tokio::test]
    async fn test_policy_upsert_requires_admin_scope() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policy = PolicyType::Rate(RatePolicy {
            policy_id: "new-rate".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        // 無任何 scope：應拒絕
        let result = registry.upsert_policy_with_scope(policy.clone(), &[]).await;
        assert!(result.is_err(), "upsert without scope should fail");
        let err = result.unwrap_err();
        assert!(matches!(err, PolicyError::PermissionDenied(_)),
            "expected PermissionDenied, got {:?}", err);
    }

    /// upsert 有 admin scope 時成功。
    #[tokio::test]
    async fn test_policy_upsert_succeeds_with_admin_scope() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policy = PolicyType::Rate(RatePolicy {
            policy_id: "admin-rate".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        let result = registry
            .upsert_policy_with_scope(policy, &["admin".to_string()])
            .await;
        assert!(result.is_ok(), "upsert with admin scope should succeed");

        let policies = registry.get_policies_for_agent("any").await;
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].policy_id(), "admin-rate");
    }

    /// upsert 有 governance:write scope 時成功。
    #[tokio::test]
    async fn test_policy_upsert_succeeds_with_governance_write_scope() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policy = PolicyType::Rate(RatePolicy {
            policy_id: "gov-rate".into(),
            agent_id: "*".into(),
            resource: Resource::WikiWrites,
            limit: 20,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        let result = registry
            .upsert_policy_with_scope(policy, &["governance:write".to_string()])
            .await;
        assert!(result.is_ok(), "upsert with governance:write should succeed");
    }

    /// upsert 衝突偵測：同 policy_id 不同類型應回傳 Conflict。
    #[tokio::test]
    async fn test_policy_upsert_conflict_detection() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        // 先插入 Rate policy
        let p1 = PolicyType::Rate(RatePolicy {
            policy_id: "conflict-id".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });
        registry
            .upsert_policy_with_scope(p1, &["admin".to_string()])
            .await
            .unwrap();

        // 嘗試以相同 policy_id 插入不同類型（Permission policy）→ 應 Conflict
        let p2 = PolicyType::Permission(crate::policy::PermissionPolicy {
            policy_id: "conflict-id".into(), // 同 ID
            agent_id: "*".into(),
            allowed_scopes: vec!["memory:read".into()],
            denied_scopes: vec![],
            requires_approval: vec![],
        });
        let result = registry
            .upsert_policy_with_scope(p2, &["admin".to_string()])
            .await;
        assert!(result.is_err(), "type conflict should be detected");
        assert!(matches!(result.unwrap_err(), PolicyError::Conflict(_)));
    }

    /// upsert 後立即生效（PolicyEvaluator 能看到新政策）。
    #[tokio::test]
    async fn test_policy_upsert_takes_immediate_effect() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));

        // 未有政策：admin scope 應通過
        let admin_op = Operation {
            op_type: OperationType::AgentCreate,
            resource_id: None,
            scope: "admin".into(),
            metadata: serde_json::json!({}),
        };
        let r = evaluator.evaluate("any-agent", &admin_op).await;
        assert!(r.allowed, "without policy, admin should be allowed");

        // 動態新增禁止 admin 的 Permission policy
        let deny_admin = PolicyType::Permission(crate::policy::PermissionPolicy {
            policy_id: "live-deny-admin".into(),
            agent_id: "*".into(),
            allowed_scopes: vec![],
            denied_scopes: vec!["admin".into()],
            requires_approval: vec![],
        });
        registry
            .upsert_policy_with_scope(deny_admin, &["admin".to_string()])
            .await
            .unwrap();

        // 立即生效：admin scope 應被拒絕
        let r = evaluator.evaluate("any-agent", &admin_op).await;
        assert!(!r.allowed, "policy change should take immediate effect");
        assert_eq!(r.violation_type, Some(ViolationType::PermissionDenied));
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 6 — p99 < 5ms 效能驗證
    // ═══════════════════════════════════════════════════════════════════════════

    /// PolicyEvaluator.evaluate() 在本地快取命中時 p99 < 5ms。
    #[tokio::test]
    async fn test_evaluate_p99_under_5ms() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: perf-perm
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
      - wiki:read
      - wiki:write
      - mcp:call
    denied_scopes:
      - admin
    requires_approval: []
  - policy_type: rate
    policy_id: perf-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 99999
    window_seconds: 60
    action_on_violation: reject
"#;
        let (registry, _dir) = setup_registry(yaml).await;
        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));
        let op = mcp_op();

        // 先做一次 warm-up
        evaluator.evaluate("perf-agent", &op).await;

        // 取樣 100 次，計算 p99
        let mut durations = Vec::with_capacity(100);
        for _ in 0..100 {
            let start = std::time::Instant::now();
            evaluator.evaluate("perf-agent", &op).await;
            durations.push(start.elapsed());
        }

        durations.sort();
        let p99 = durations[98]; // 99th percentile (index 98 of 100)
        assert!(
            p99 < Duration::from_millis(5),
            "p99 ({p99:?}) should be < 5ms"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Section 7 — 無政策時預設允許
    // ═══════════════════════════════════════════════════════════════════════════

    /// 無任何政策時，所有操作應預設允許（fail-open）。
    #[tokio::test]
    async fn test_no_policies_fail_open() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        let evaluator = PolicyEvaluator::new(registry);

        for op_type in [
            OperationType::McpCall,
            OperationType::MemoryWrite,
            OperationType::WikiWrite,
            OperationType::AgentCreate,
        ] {
            let op = Operation {
                op_type: op_type.clone(),
                resource_id: None,
                scope: "any:scope".into(),
                metadata: serde_json::json!({}),
            };
            let r = evaluator.evaluate("any-agent", &op).await;
            assert!(r.allowed, "no policies → {op_type} should be allowed");
        }
    }
}
