//! PolicyRegistry + YAML 規則載入 — M1-A 整合測試
//!
//! TDD Phase（RED → GREEN → REFACTOR）
//!
//! 驗收標準：
//! - 從 policies/global.yaml 載入全域 6 項預設政策
//! - Agent 專屬政策覆寫全域政策（優先序）
//! - fail-safe：非法 YAML / 非法政策不影響現有有效政策
//! - 熱重載：修改 YAML 後不重啟服務即生效
//! - 空目錄不報錯（graceful degradation）

#[cfg(test)]
mod policy_registry_integration_tests {
    use crate::{
        policy::{ActionOnViolation, PolicyType, RatePolicy, Resource},
        registry::PolicyRegistry,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// 建立含六項預設政策的 global.yaml 於暫存目錄。
    async fn setup_global_defaults() -> (Arc<PolicyRegistry>, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let yaml = include_str!("../../../../policies/global.yaml");
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        (registry, dir)
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 1 — YAML 載入（PolicyValidator 行為）
    // ═══════════════════════════════════════════════════════════════════════════

    /// 驗證 global.yaml 能完整載入所有六項政策。
    #[tokio::test]
    async fn test_global_yaml_loads_six_default_policies() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let ids: Vec<&str> = policies.iter().map(|p| p.policy_id()).collect();
        assert!(ids.contains(&"default-rate-mcp"), "missing default-rate-mcp");
        assert!(
            ids.contains(&"default-rate-memory-write"),
            "missing default-rate-memory-write"
        );
        assert!(
            ids.contains(&"default-rate-wiki-write"),
            "missing default-rate-wiki-write"
        );
        assert!(ids.contains(&"default-quota-daily"), "missing default-quota-daily");
        assert!(ids.contains(&"default-permission"), "missing default-permission");
        assert!(ids.contains(&"default-lifecycle"), "missing default-lifecycle");

        assert_eq!(
            policies.len(),
            6,
            "should have exactly 6 default policies, got: {ids:?}"
        );
    }

    /// 驗證 default-rate-mcp 的具體數值符合 SDD §1.6（200/min）。
    #[tokio::test]
    async fn test_default_rate_mcp_values() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let rate_mcp = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "default-rate-mcp" => Some(r.clone()),
                _ => None,
            })
            .expect("default-rate-mcp should exist");

        assert_eq!(rate_mcp.limit, 200, "MCP rate limit should be 200/min");
        assert_eq!(rate_mcp.window_seconds, 60, "window should be 60 seconds");
        assert_eq!(rate_mcp.resource, Resource::McpCalls);
        assert_eq!(rate_mcp.action_on_violation, ActionOnViolation::Reject);
    }

    /// 驗證 default-rate-memory-write 的具體數值（50/min）。
    #[tokio::test]
    async fn test_default_rate_memory_write_values() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let rate_mem = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "default-rate-memory-write" => {
                    Some(r.clone())
                }
                _ => None,
            })
            .expect("default-rate-memory-write should exist");

        assert_eq!(rate_mem.limit, 50, "Memory write rate limit should be 50/min");
        assert_eq!(rate_mem.resource, Resource::MemoryWrites);
    }

    /// 驗證 default-rate-wiki-write 的具體數值（20/min）。
    #[tokio::test]
    async fn test_default_rate_wiki_write_values() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let rate_wiki = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "default-rate-wiki-write" => Some(r.clone()),
                _ => None,
            })
            .expect("default-rate-wiki-write should exist");

        assert_eq!(rate_wiki.limit, 20, "Wiki write rate limit should be 20/min");
        assert_eq!(rate_wiki.resource, Resource::WikiWrites);
    }

    /// 驗證 default-quota-daily 的具體數值（500,000 tokens/日）。
    #[tokio::test]
    async fn test_default_quota_daily_values() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let quota = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Quota(q) if q.policy_id == "default-quota-daily" => Some(q.clone()),
                _ => None,
            })
            .expect("default-quota-daily should exist");

        assert_eq!(
            quota.daily_token_budget, 500_000,
            "Daily token budget should be 500,000"
        );
        assert_eq!(quota.max_concurrent_tasks, 5);
        assert_eq!(quota.max_memory_entries, 10_000);
        assert_eq!(quota.reset_cron, "0 0 * * *");
    }

    /// 驗證 default-permission 禁止 admin scope（SDD §1.6）。
    #[tokio::test]
    async fn test_default_permission_denies_admin_scope() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let perm = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Permission(pp) if pp.policy_id == "default-permission" => {
                    Some(pp.clone())
                }
                _ => None,
            })
            .expect("default-permission should exist");

        assert!(
            perm.denied_scopes.contains(&"admin".to_string()),
            "admin scope should be denied by default"
        );
        assert!(
            !perm.is_scope_allowed("admin"),
            "admin should not be allowed"
        );
        assert!(
            perm.is_scope_allowed("memory:read"),
            "memory:read should be allowed"
        );
    }

    /// 驗證 default-lifecycle idle > 48h 自動暫停設定。
    #[tokio::test]
    async fn test_default_lifecycle_idle_48h() {
        let (registry, _dir) = setup_global_defaults().await;
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;

        let lifecycle = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Lifecycle(l) if l.policy_id == "default-lifecycle" => Some(l.clone()),
                _ => None,
            })
            .expect("default-lifecycle should exist");

        assert_eq!(
            lifecycle.max_idle_hours, 48,
            "max_idle_hours should be 48"
        );
        assert_eq!(lifecycle.health_check_interval_seconds, 300);
        assert_eq!(lifecycle.auto_suspend_on_violation_count, 10);
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 2 — 優先序：Agent 專屬 > 全域 > 系統預設
    // ═══════════════════════════════════════════════════════════════════════════

    /// Agent 專屬政策覆寫全域同 ID 政策。
    #[tokio::test]
    async fn test_agent_specific_overrides_global_policy() {
        let dir = TempDir::new().unwrap();

        // 全域：MCP limit = 200
        let global = r#"
policies:
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes: [memory:read]
    denied_scopes: [admin]
"#;
        // agent-x 專屬：MCP limit = 1000（覆寫全域）
        let agent_x = r#"
policies:
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "agent-x"
    resource: mcp_calls
    limit: 1000
    window_seconds: 60
    action_on_violation: warn
"#;
        std::fs::write(dir.path().join("global.yaml"), global).unwrap();
        std::fs::write(dir.path().join("agent-x.yaml"), agent_x).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        // agent-x：應看到 limit = 1000（專屬政策覆寫）
        let agent_x_policies: Vec<PolicyType> = registry.get_policies_for_agent("agent-x").await;
        let rate = agent_x_policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "default-rate-mcp" => Some(r.clone()),
                _ => None,
            })
            .expect("rate policy should exist for agent-x");
        assert_eq!(rate.limit, 1000, "agent-x should use its own limit of 1000");
        assert_eq!(rate.action_on_violation, ActionOnViolation::Warn);

        // agent-x：全域 permission policy 應繼承
        let has_perm = agent_x_policies
            .iter()
            .any(|p| matches!(p, PolicyType::Permission(pp) if pp.policy_id == "default-permission"));
        assert!(has_perm, "agent-x should inherit global permission policy");

        // agent-y（無專屬）：應看到 limit = 200（全域政策）
        let agent_y_policies: Vec<PolicyType> = registry.get_policies_for_agent("agent-y").await;
        let rate_y = agent_y_policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "default-rate-mcp" => Some(r.clone()),
                _ => None,
            })
            .expect("rate policy should exist for agent-y");
        assert_eq!(rate_y.limit, 200, "agent-y should use global limit of 200");
    }

    /// 全域通配符政策適用於所有 Agent。
    #[tokio::test]
    async fn test_global_wildcard_applies_to_all_agents() {
        let (registry, _dir) = setup_global_defaults().await;

        for agent_id in ["alpha", "beta", "gamma", "delta"] {
            let policies: Vec<PolicyType> = registry.get_policies_for_agent(agent_id).await;
            assert!(
                policies.len() >= 6,
                "Agent {agent_id} should have at least 6 policies from global"
            );
            // 所有 agent 都不允許 admin
            let perm = policies.iter().find_map(|p| match p {
                PolicyType::Permission(pp) => Some(pp.clone()),
                _ => None,
            });
            if let Some(pp) = perm {
                assert!(!pp.is_scope_allowed("admin"), "{agent_id}: admin should be denied");
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 3 — Fail-safe 載入
    // ═══════════════════════════════════════════════════════════════════════════

    /// 完全非法 YAML 不影響 Registry（fail-safe）。
    #[tokio::test]
    async fn test_invalid_yaml_does_not_affect_registry() {
        let dir = TempDir::new().unwrap();

        // 先載入有效的全域政策
        let valid_yaml = r#"
policies:
  - policy_type: rate
    policy_id: good-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 100
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), valid_yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        // 確認有效政策存在
        let before: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(before.len(), 1, "should have 1 valid policy");

        // 嘗試 upsert 非法政策（limit = 0）
        let bad_policy = PolicyType::Rate(RatePolicy {
            policy_id: "bad-rate".into(),
            agent_id: "*".into(),
            resource: Resource::WikiWrites,
            limit: 0, // invalid
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });
        let result = registry.upsert_policy(bad_policy).await;
        assert!(result.is_err(), "invalid policy should be rejected");

        // 有效政策應不受影響
        let after: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(after.len(), 1, "valid policy should be unaffected");
        assert_eq!(after[0].policy_id(), "good-rate");
    }

    /// 混合有效/非法政策的 YAML：只有效政策載入（fail-safe partial load）。
    #[tokio::test]
    async fn test_invalid_policy_in_batch_skipped_failsafe() {
        let dir = TempDir::new().unwrap();

        // 一個非法（limit=0）+ 兩個有效
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: bad-zero-limit
    agent_id: "*"
    resource: mcp_calls
    limit: 0
    window_seconds: 60
    action_on_violation: reject
  - policy_type: rate
    policy_id: good-policy-1
    agent_id: "*"
    resource: mcp_calls
    limit: 100
    window_seconds: 60
    action_on_violation: reject
  - policy_type: rate
    policy_id: good-policy-2
    agent_id: "*"
    resource: memory_writes
    limit: 50
    window_seconds: 60
    action_on_violation: warn
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        let ids: Vec<&str> = policies.iter().map(|p| p.policy_id()).collect();

        assert!(
            !ids.contains(&"bad-zero-limit"),
            "invalid policy should be skipped"
        );
        assert!(
            ids.contains(&"good-policy-1"),
            "good-policy-1 should be loaded"
        );
        assert!(
            ids.contains(&"good-policy-2"),
            "good-policy-2 should be loaded"
        );
        assert_eq!(policies.len(), 2, "only 2 valid policies should be loaded");
    }

    /// policies 目錄不存在時 graceful degradation（空政策集，不報錯）。
    #[tokio::test]
    async fn test_missing_policies_dir_returns_empty() {
        let registry = PolicyRegistry::new("/nonexistent/policies/dir");
        let result = registry.load().await;
        assert!(result.is_ok(), "missing dir should not error");
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;
        assert!(policies.is_empty(), "should have no policies");
    }

    /// 空 policies 目錄返回空政策集。
    #[tokio::test]
    async fn test_empty_policies_dir_returns_empty() {
        let dir = TempDir::new().unwrap(); // no files
        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any-agent").await;
        assert!(policies.is_empty(), "empty dir → no policies");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 4 — 動態 upsert / remove
    // ═══════════════════════════════════════════════════════════════════════════

    /// 動態 upsert 政策後立即生效。
    #[tokio::test]
    async fn test_dynamic_upsert_takes_effect_immediately() {
        let (registry, _dir) = setup_global_defaults().await;

        // upsert 自訂 agent 政策
        let custom = PolicyType::Rate(RatePolicy {
            policy_id: "custom-rate".into(),
            agent_id: "my-agent".into(),
            resource: Resource::WikiWrites,
            limit: 999,
            window_seconds: 120,
            action_on_violation: ActionOnViolation::Warn,
        });
        registry.upsert_policy(custom).await.unwrap();

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("my-agent").await;
        let custom_rate = policies
            .iter()
            .find_map(|p| match p {
                PolicyType::Rate(r) if r.policy_id == "custom-rate" => Some(r.clone()),
                _ => None,
            })
            .expect("custom-rate should exist after upsert");
        assert_eq!(custom_rate.limit, 999);
        assert_eq!(custom_rate.window_seconds, 120);
    }

    /// upsert 同 policy_id 的政策應覆寫（不重複）。
    #[tokio::test]
    async fn test_upsert_replaces_same_policy_id() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let p1 = PolicyType::Rate(RatePolicy {
            policy_id: "dupe-test".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });
        let p2 = PolicyType::Rate(RatePolicy {
            policy_id: "dupe-test".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 500, // updated
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        registry.upsert_policy(p1).await.unwrap();
        registry.upsert_policy(p2).await.unwrap();

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(policies.len(), 1, "should not have duplicates");
        match &policies[0] {
            PolicyType::Rate(r) => assert_eq!(r.limit, 500, "should have updated limit"),
            _ => panic!("expected rate policy"),
        }
    }

    /// remove_policy 正確刪除。
    #[tokio::test]
    async fn test_remove_policy_works() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        registry
            .upsert_policy(PolicyType::Rate(RatePolicy {
                policy_id: "temp-policy".into(),
                agent_id: "*".into(),
                resource: Resource::MessageSends,
                limit: 10,
                window_seconds: 60,
                action_on_violation: ActionOnViolation::Reject,
            }))
            .await
            .unwrap();

        let before: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(before.len(), 1);

        let removed = registry.remove_policy("*", "temp-policy").await;
        assert!(removed, "should return true on successful remove");

        let after: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert!(after.is_empty(), "policy should be removed");
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 5 — 熱重載（Hot Reload）
    // ═══════════════════════════════════════════════════════════════════════════

    /// 模擬熱重載：修改 global.yaml 後 reload() 令新政策生效。
    ///
    /// 注意：此測試模擬「手動 reload」而非 inotify 事件，
    /// 因為 inotify 在 CI/tempdir 環境下不可靠。
    /// 真正的 inotify 熱重載由 `watch()` 方法觸發。
    #[tokio::test]
    async fn test_hot_reload_manual_trigger() {
        let dir = TempDir::new().unwrap();

        // 初始政策：limit = 100
        let yaml_v1 = r#"
policies:
  - policy_type: rate
    policy_id: hot-reload-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 100
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml_v1).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let v1: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        let limit_v1 = v1.iter().find_map(|p| match p {
            PolicyType::Rate(r) if r.policy_id == "hot-reload-rate" => Some(r.limit),
            _ => None,
        });
        assert_eq!(limit_v1, Some(100), "initial limit should be 100");

        // 修改 YAML，更新 limit = 999
        let yaml_v2 = r#"
policies:
  - policy_type: rate
    policy_id: hot-reload-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 999
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml_v2).unwrap();

        // 觸發重載（模擬 file watcher 事件後的 reload）
        registry.load().await.unwrap();

        let v2: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        let limit_v2 = v2.iter().find_map(|p| match p {
            PolicyType::Rate(r) if r.policy_id == "hot-reload-rate" => Some(r.limit),
            _ => None,
        });
        assert_eq!(limit_v2, Some(999), "after reload, limit should be updated to 999");
    }

    /// 熱重載時非法 YAML 不 panic（fail-safe hot reload）。
    #[tokio::test]
    async fn test_hot_reload_invalid_yaml_does_not_panic() {
        let dir = TempDir::new().unwrap();

        // 初始有效政策
        let valid_yaml = r#"
policies:
  - policy_type: rate
    policy_id: stable-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), valid_yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        // 覆寫為非法 YAML
        std::fs::write(dir.path().join("global.yaml"), "INVALID YAML {{{{").unwrap();

        // 重載：非法 YAML → load() 應不 panic，回傳 Ok
        let reload_result = registry.load().await;
        assert!(
            reload_result.is_ok(),
            "reload with invalid YAML should not panic: {reload_result:?}"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 6 — YAML 格式多樣性驗證（PolicyValidator 角色）
    // ═══════════════════════════════════════════════════════════════════════════

    /// 所有四種政策類型均能從 YAML 正確反序列化。
    #[tokio::test]
    async fn test_all_four_policy_types_yaml_roundtrip() {
        let dir = TempDir::new().unwrap();
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: yaml-rate
    agent_id: "*"
    resource: wiki_writes
    limit: 20
    window_seconds: 60
    action_on_violation: throttle
  - policy_type: permission
    policy_id: yaml-perm
    agent_id: "*"
    allowed_scopes:
      - memory:read
    denied_scopes:
      - admin
    requires_approval:
      - agent:create
  - policy_type: quota
    policy_id: yaml-quota
    agent_id: "*"
    daily_token_budget: 250000
    max_concurrent_tasks: 3
    max_memory_entries: 5000
    reset_cron: "0 0 * * *"
  - policy_type: lifecycle
    policy_id: yaml-lifecycle
    agent_id: "*"
    max_idle_hours: 24
    health_check_interval_seconds: 180
    auto_suspend_on_violation_count: 5
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(policies.len(), 4, "should have 4 policies");

        let type_names: Vec<&str> = policies.iter().map(|p| p.type_name()).collect();
        assert!(type_names.contains(&"rate"));
        assert!(type_names.contains(&"permission"));
        assert!(type_names.contains(&"quota"));
        assert!(type_names.contains(&"lifecycle"));

        // 驗證 throttle action 正確解析
        let rate = policies.iter().find_map(|p| match p {
            PolicyType::Rate(r) => Some(r.clone()),
            _ => None,
        }).unwrap();
        assert_eq!(rate.action_on_violation, ActionOnViolation::Throttle);
    }

    /// 缺少必填欄位的 YAML 不被載入（policy_id 空字串）。
    #[tokio::test]
    async fn test_missing_required_fields_policy_rejected() {
        let dir = TempDir::new().unwrap();
        // policy_id 為空字串 — validate() 應拒絕
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: ""
    agent_id: "*"
    resource: mcp_calls
    limit: 100
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert!(
            policies.is_empty(),
            "policy with empty policy_id should be rejected"
        );
    }

    /// 不支援的 policy_type 欄位導致整個檔案跳過（fail-safe）。
    #[tokio::test]
    async fn test_unknown_policy_type_causes_file_parse_error() {
        let dir = TempDir::new().unwrap();
        // unknown policy_type 會導致 serde_yaml 解析失敗
        let yaml = r#"
policies:
  - policy_type: quantum_teleport
    policy_id: bad-type
    agent_id: "*"
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap(); // should not panic

        // 解析失敗 → 整個 global.yaml 跳過 → 空政策集
        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert!(
            policies.is_empty(),
            "unknown policy type → entire file skipped"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // Phase 7 — 並發安全
    // ═══════════════════════════════════════════════════════════════════════════

    /// 並發 upsert 不發生 data race。
    #[tokio::test]
    async fn test_concurrent_upsert_is_safe() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();

        let mut handles = vec![];
        for i in 0_u32..30 {
            let reg = Arc::clone(&registry);
            handles.push(tokio::spawn(async move {
                let p = PolicyType::Rate(RatePolicy {
                    policy_id: format!("concurrent-rate-{i}"),
                    agent_id: "*".into(),
                    resource: Resource::McpCalls,
                    limit: 10 + i,
                    window_seconds: 60,
                    action_on_violation: ActionOnViolation::Reject,
                });
                reg.upsert_policy(p).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let policies: Vec<PolicyType> = registry.get_policies_for_agent("any").await;
        assert_eq!(policies.len(), 30, "all 30 concurrent upserts should succeed");
    }
}
