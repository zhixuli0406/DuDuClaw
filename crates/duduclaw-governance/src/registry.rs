//! PolicyRegistry — YAML 政策定義儲存庫
//!
//! ## 功能
//! - 從 `policies/global.yaml` 載入全域政策
//! - 從 `policies/{agent_id}.yaml` 載入 Agent 專屬政策（覆寫全域）
//! - JSON Schema 驗證（fail-safe 載入：非法政策不影響現有有效政策）
//! - inotify 熱重載（政策更新不需重啟服務）
//!
//! ## 優先序
//! ```text
//! Agent 專屬政策（policies/{agent_id}.yaml）
//!   > 全域政策（policies/global.yaml）
//!   > 系統內建預設
//! ```

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::policy::{PolicyError, PolicyFile, PolicyType};

// ── Registry ──────────────────────────────────────────────────────────────────

/// YAML 政策載入器，持有所有已載入的政策。
///
/// 使用 `Arc<PolicyRegistry>` 在多處共享同一實例。
pub struct PolicyRegistry {
    /// 政策檔案目錄（包含 global.yaml 和 {agent_id}.yaml）。
    policies_dir: PathBuf,
    /// agent_id → 該 Agent 適用的政策列表（已合併全域政策）。
    /// `"*"` key 存放全域政策。
    policies: RwLock<HashMap<String, Vec<PolicyType>>>,
}

impl std::fmt::Debug for PolicyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyRegistry")
            .field("policies_dir", &self.policies_dir)
            .finish()
    }
}

impl PolicyRegistry {
    /// 建立新的 PolicyRegistry，指向 `policies_dir` 目錄。
    ///
    /// 此時尚未載入任何政策；呼叫 [`load`](Self::load) 後才開始讀取。
    pub fn new(policies_dir: impl Into<PathBuf>) -> Self {
        Self {
            policies_dir: policies_dir.into(),
            policies: RwLock::new(HashMap::new()),
        }
    }

    /// 同步載入所有政策檔案（global.yaml + 所有 {agent_id}.yaml）。
    ///
    /// - 非法 YAML 格式的檔案會記錄警告並跳過（fail-safe）。
    /// - 非法政策（validate() 失敗）同樣跳過，不影響已有效政策。
    pub async fn load(&self) -> Result<(), PolicyError> {
        let dir = &self.policies_dir;

        if !dir.exists() {
            // 目錄不存在：建立空政策集並 return（不算錯誤）
            info!(
                "Policy directory {:?} does not exist, using empty policies",
                dir
            );
            *self.policies.write().await = HashMap::new();
            return Ok(());
        }

        let mut new_policies: HashMap<String, Vec<PolicyType>> = HashMap::new();

        // 載入 global.yaml
        let global_path = dir.join("global.yaml");
        if global_path.exists() {
            match Self::load_file(&global_path) {
                Ok(file_policies) => {
                    let valid: Vec<PolicyType> = file_policies
                        .into_iter()
                        .filter(|p| match p.validate() {
                            Ok(()) => true,
                            Err(e) => {
                                warn!(
                                    "Skipping invalid policy '{}' in global.yaml: {}",
                                    p.policy_id(),
                                    e
                                );
                                false
                            }
                        })
                        .collect();
                    new_policies.insert("*".to_string(), valid);
                }
                Err(e) => {
                    warn!("Failed to load global.yaml (skipping): {e}");
                }
            }
        }

        // 載入 {agent_id}.yaml（除了 global.yaml）
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if stem == "global" {
                        continue;
                    }
                    // 安全性驗證：agent_id 只允許 [a-zA-Z0-9\-_] 字元集
                    // 防止惡意命名的 YAML 透過 stem 引入邏輯錯誤或注入攻擊
                    if !Self::is_valid_agent_id(&stem) {
                        warn!(
                            "Skipping policy file {:?}: agent_id '{stem}' contains invalid characters (only [a-zA-Z0-9\\-_] allowed)",
                            path
                        );
                        continue;
                    }
                    // stem = agent_id
                    match Self::load_file(&path) {
                        Ok(file_policies) => {
                            let valid: Vec<PolicyType> = file_policies
                                .into_iter()
                                .filter(|p| match p.validate() {
                                    Ok(()) => true,
                                    Err(e) => {
                                        warn!(
                                            "Skipping invalid policy '{}' in {stem}.yaml: {}",
                                            p.policy_id(),
                                            e
                                        );
                                        false
                                    }
                                })
                                .collect();
                            new_policies.insert(stem, valid);
                        }
                        Err(e) => {
                            warn!("Failed to load {stem}.yaml (skipping): {e}");
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Cannot read policies directory {:?}: {e}", dir);
            }
        }

        *self.policies.write().await = new_policies;
        info!("PolicyRegistry loaded {} agent entries", {
            let lock = self.policies.read().await;
            lock.len()
        });
        Ok(())
    }

    /// 取得適用於特定 Agent 的所有政策（全域 + 專屬，專屬覆寫全域）。
    ///
    /// 回傳 Agent 專屬政策 + 未被專屬政策同 ID 覆寫的全域政策。
    pub async fn get_policies_for_agent(&self, agent_id: &str) -> Vec<PolicyType> {
        let lock = self.policies.read().await;

        let global: Vec<&PolicyType> = lock
            .get("*")
            .map(|v| v.iter().collect())
            .unwrap_or_default();

        let agent_specific: Vec<&PolicyType> = lock
            .get(agent_id)
            .map(|v| v.iter().collect())
            .unwrap_or_default();

        // Collect agent-specific policy IDs for dedup
        let agent_ids: std::collections::HashSet<&str> =
            agent_specific.iter().map(|p| p.policy_id()).collect();

        // Merge: agent-specific first, then global (if not overridden)
        let mut merged: Vec<PolicyType> = agent_specific
            .into_iter()
            .cloned()
            .collect();

        for global_policy in global {
            if !agent_ids.contains(global_policy.policy_id()) {
                merged.push(global_policy.clone());
            }
        }

        merged
    }

    /// 啟動 inotify 熱重載監聽器。
    ///
    /// 當 policies 目錄下的 YAML 檔案有變動時，自動重新載入。
    ///
    /// 回傳 watcher 句柄，丟棄後監聽器停止。
    pub fn watch(self: Arc<Self>) -> Result<RecommendedWatcher, notify::Error> {
        let registry = Arc::clone(&self);
        let dir = self.policies_dir.clone();

        let mut watcher = notify::recommended_watcher(
            move |result: Result<Event, notify::Error>| {
                match result {
                    Ok(event) => {
                        // 只在 Create/Modify/Remove 時重新載入
                        let should_reload = matches!(
                            event.kind,
                            EventKind::Create(_)
                                | EventKind::Modify(_)
                                | EventKind::Remove(_)
                        );
                        if should_reload {
                            // 篩選 .yaml 檔案
                            let yaml_changed = event.paths.iter().any(|p| {
                                p.extension().and_then(|e| e.to_str()) == Some("yaml")
                            });
                            if yaml_changed {
                                let reg = Arc::clone(&registry);
                                tokio::spawn(async move {
                                    info!("Policy file changed, reloading...");
                                    if let Err(e) = reg.load().await {
                                        error!("Failed to reload policies: {e}");
                                    }
                                });
                            }
                        }
                    }
                    Err(e) => {
                        error!("File watcher error: {e}");
                    }
                }
            },
        )?;

        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        info!("PolicyRegistry watching {:?} for changes", dir);
        Ok(watcher)
    }

    /// 直接插入或更新政策（用於測試或動態管理）。
    pub async fn upsert_policy(&self, policy: PolicyType) -> Result<(), PolicyError> {
        policy.validate()?;
        let agent_id = policy.agent_id().to_string();
        let mut lock = self.policies.write().await;
        let entry = lock.entry(agent_id).or_default();
        // 若已存在同 policy_id 則替換
        if let Some(pos) = entry.iter().position(|p| p.policy_id() == policy.policy_id()) {
            entry[pos] = policy;
        } else {
            entry.push(policy);
        }
        Ok(())
    }

    /// 帶有 scope 權限驗證的 upsert。
    ///
    /// 需要 `admin` 或 `governance:write` scope，否則回傳 `PolicyError::PermissionDenied`。
    ///
    /// 同時進行衝突偵測：若已存在同 policy_id 但**不同 policy_type**，回傳 `PolicyError::Conflict`。
    pub async fn upsert_policy_with_scope(
        &self,
        policy: PolicyType,
        caller_scopes: &[String],
    ) -> Result<(), PolicyError> {
        // 1. 權限檢查：需要 admin 或 governance:write
        let has_permission = caller_scopes.iter().any(|s| {
            s == "admin" || s == "governance:write"
        });
        if !has_permission {
            return Err(PolicyError::PermissionDenied(format!(
                "upsert_policy requires 'admin' or 'governance:write' scope; caller has: {:?}",
                caller_scopes
            )));
        }

        // 2. 政策 schema 驗證
        policy.validate()?;

        // 3. 衝突偵測：同 policy_id 但不同 policy_type → Conflict
        let agent_id = policy.agent_id().to_string();
        {
            let lock = self.policies.read().await;
            if let Some(existing_policies) = lock.get(&agent_id) {
                if let Some(existing) = existing_policies
                    .iter()
                    .find(|p| p.policy_id() == policy.policy_id())
                {
                    // 只有在 type_name 不同時才是衝突（type 相同則為 upsert 更新）
                    if existing.type_name() != policy.type_name() {
                        return Err(PolicyError::Conflict(format!(
                            "policy '{}' already exists with type '{}', cannot change to '{}'",
                            policy.policy_id(),
                            existing.type_name(),
                            policy.type_name(),
                        )));
                    }
                }
            }
        }

        // 4. 執行 upsert
        let mut lock = self.policies.write().await;
        let entry = lock.entry(agent_id).or_default();
        if let Some(pos) = entry.iter().position(|p| p.policy_id() == policy.policy_id()) {
            entry[pos] = policy;
        } else {
            entry.push(policy);
        }

        info!("Policy '{}' upserted with scope authorization", {
            // Note: policy moved above, read from entry
            entry.last().map(|p| p.policy_id()).unwrap_or("?")
        });

        Ok(())
    }

    /// 刪除政策（用於測試或動態管理）。
    pub async fn remove_policy(&self, agent_id: &str, policy_id: &str) -> bool {
        let mut lock = self.policies.write().await;
        if let Some(policies) = lock.get_mut(agent_id) {
            let before = policies.len();
            policies.retain(|p| p.policy_id() != policy_id);
            return policies.len() < before;
        }
        false
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn load_file(path: &Path) -> Result<Vec<PolicyType>, PolicyError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            PolicyError::InvalidSchema(format!("Cannot read {:?}: {e}", path))
        })?;
        let file: PolicyFile = serde_yaml::from_str(&content)?;
        Ok(file.policies)
    }

    /// 驗證 agent_id 字元格式。
    ///
    /// 只允許 `[a-zA-Z0-9\-_]` 字元集，防止惡意命名的政策 YAML 透過
    /// `file_stem` 引入含有路徑分隔符、空白或其他特殊字元的 agent_id，
    /// 避免後續查詢邏輯錯誤或潛在注入問題。
    fn is_valid_agent_id(id: &str) -> bool {
        !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ActionOnViolation, RatePolicy, Resource};
    use tempfile::TempDir;

    fn make_global_yaml() -> &'static str {
        r#"
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
    allowed_scopes:
      - memory:read
      - memory:write
    denied_scopes:
      - admin
    requires_approval:
      - agent:create
"#
    }

    fn make_agent_yaml(agent_id: &str) -> String {
        format!(
            r#"
policies:
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "{agent_id}"
    resource: mcp_calls
    limit: 500
    window_seconds: 60
    action_on_violation: warn
"#
        )
    }

    #[tokio::test]
    async fn test_load_global_yaml() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("global.yaml"), make_global_yaml()).unwrap();

        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        let policies = registry.get_policies_for_agent("any-agent").await;
        assert_eq!(policies.len(), 2);
    }

    #[tokio::test]
    async fn test_agent_specific_overrides_global() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("global.yaml"), make_global_yaml()).unwrap();
        std::fs::write(
            dir.path().join("duduclaw-eng-infra.yaml"),
            make_agent_yaml("duduclaw-eng-infra"),
        )
        .unwrap();

        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        // For the specific agent, default-rate-mcp should come from agent file (limit=500)
        let policies = registry
            .get_policies_for_agent("duduclaw-eng-infra")
            .await;

        let rate_policy = policies.iter().find_map(|p| match p {
            PolicyType::Rate(r) if r.policy_id == "default-rate-mcp" => Some(r),
            _ => None,
        });
        assert!(rate_policy.is_some(), "rate policy should exist");
        assert_eq!(
            rate_policy.unwrap().limit,
            500,
            "agent-specific limit should override global"
        );

        // The permission policy should still be inherited from global
        let has_permission = policies
            .iter()
            .any(|p| matches!(p, PolicyType::Permission(pp) if pp.policy_id == "default-permission"));
        assert!(has_permission, "global permission policy should be inherited");
    }

    #[tokio::test]
    async fn test_nonexistent_directory_returns_empty() {
        let registry = PolicyRegistry::new("/nonexistent/path/that/does/not/exist");
        registry.load().await.unwrap(); // should not error
        let policies = registry.get_policies_for_agent("any-agent").await;
        assert!(policies.is_empty());
    }

    #[tokio::test]
    async fn test_invalid_yaml_is_skipped_failsafe() {
        let dir = TempDir::new().unwrap();
        // Write invalid YAML
        std::fs::write(dir.path().join("global.yaml"), "NOT VALID YAML: {{{").unwrap();

        let registry = PolicyRegistry::new(dir.path());
        // Should not panic or error — fail-safe
        registry.load().await.unwrap();
        let policies = registry.get_policies_for_agent("any-agent").await;
        assert!(policies.is_empty()); // invalid file → no policies loaded
    }

    #[tokio::test]
    async fn test_invalid_policy_skipped_failsafe() {
        let dir = TempDir::new().unwrap();
        // policy with limit=0 (invalid) mixed with valid policy
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: bad-policy
    agent_id: "*"
    resource: mcp_calls
    limit: 0
    window_seconds: 60
    action_on_violation: reject
  - policy_type: rate
    policy_id: good-policy
    agent_id: "*"
    resource: mcp_calls
    limit: 100
    window_seconds: 60
    action_on_violation: reject
"#;
        std::fs::write(dir.path().join("global.yaml"), yaml).unwrap();

        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        let policies = registry.get_policies_for_agent("any-agent").await;
        assert_eq!(policies.len(), 1, "invalid policy should be skipped");
        assert_eq!(policies[0].policy_id(), "good-policy");
    }

    #[tokio::test]
    async fn test_upsert_policy() {
        let dir = TempDir::new().unwrap();
        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        let policy = PolicyType::Rate(RatePolicy {
            policy_id: "dynamic-rate".into(),
            agent_id: "*".into(),
            resource: Resource::MemoryWrites,
            limit: 50,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        registry.upsert_policy(policy).await.unwrap();
        let policies = registry.get_policies_for_agent("any-agent").await;
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].policy_id(), "dynamic-rate");
    }

    #[tokio::test]
    async fn test_upsert_replaces_existing() {
        let dir = TempDir::new().unwrap();
        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        let policy1 = PolicyType::Rate(RatePolicy {
            policy_id: "same-id".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });
        let policy2 = PolicyType::Rate(RatePolicy {
            policy_id: "same-id".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 200, // updated limit
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        registry.upsert_policy(policy1).await.unwrap();
        registry.upsert_policy(policy2).await.unwrap();

        let policies = registry.get_policies_for_agent("any-agent").await;
        assert_eq!(policies.len(), 1, "should not have duplicates");
        match &policies[0] {
            PolicyType::Rate(r) => assert_eq!(r.limit, 200, "limit should be updated"),
            _ => panic!("expected rate policy"),
        }
    }

    #[tokio::test]
    async fn test_remove_policy() {
        let dir = TempDir::new().unwrap();
        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        let policy = PolicyType::Rate(RatePolicy {
            policy_id: "to-remove".into(),
            agent_id: "*".into(),
            resource: Resource::McpCalls,
            limit: 100,
            window_seconds: 60,
            action_on_violation: ActionOnViolation::Reject,
        });

        registry.upsert_policy(policy).await.unwrap();
        let removed = registry.remove_policy("*", "to-remove").await;
        assert!(removed, "policy should have been removed");

        let policies = registry.get_policies_for_agent("any-agent").await;
        assert!(policies.is_empty());
    }

    // ── M3 Security: agent_id 字元集驗證 ──────────────────────────────────────

    #[test]
    fn test_is_valid_agent_id_accepts_valid_chars() {
        assert!(PolicyRegistry::is_valid_agent_id("my-agent"));
        assert!(PolicyRegistry::is_valid_agent_id("agent_123"));
        assert!(PolicyRegistry::is_valid_agent_id("AgentABC"));
        assert!(PolicyRegistry::is_valid_agent_id("a"));
        assert!(PolicyRegistry::is_valid_agent_id("abc-def_GHI-123"));
    }

    #[test]
    fn test_is_valid_agent_id_rejects_invalid_chars() {
        assert!(!PolicyRegistry::is_valid_agent_id(""));          // 空字串
        assert!(!PolicyRegistry::is_valid_agent_id("../evil"));   // 路徑穿越
        assert!(!PolicyRegistry::is_valid_agent_id("agent id"));  // 空白
        assert!(!PolicyRegistry::is_valid_agent_id("agent.name")); // 點號
        assert!(!PolicyRegistry::is_valid_agent_id("agent/sub")); // 斜線
        assert!(!PolicyRegistry::is_valid_agent_id("agent\0null")); // null byte
        assert!(!PolicyRegistry::is_valid_agent_id("$(cmd)")); // Shell 注入
    }

    #[tokio::test]
    async fn test_malicious_yaml_filename_is_skipped() {
        let dir = TempDir::new().unwrap();

        // 建立含有非法字元的 YAML 檔名（路徑穿越嘗試）
        // 在檔案系統上允許但 agent_id 驗證應拒絕
        let malicious_stem = "evil..agent"; // 包含點號，應被拒絕
        let malicious_path = dir.path().join(format!("{malicious_stem}.yaml"));
        let valid_yaml = make_agent_yaml("evil..agent");
        std::fs::write(&malicious_path, valid_yaml).unwrap();

        // 建立合法的 agent YAML
        let valid_path = dir.path().join("valid-agent.yaml");
        std::fs::write(&valid_path, make_agent_yaml("valid-agent")).unwrap();

        let registry = PolicyRegistry::new(dir.path());
        registry.load().await.unwrap();

        // 合法 agent 應載入
        let valid_policies = registry.get_policies_for_agent("valid-agent").await;
        assert!(!valid_policies.is_empty(), "valid-agent should have policies");

        // 惡意命名的 YAML 應被跳過（policies 中不應有該 stem 的 entry）
        // 注意：get_policies_for_agent 對不存在的 agent 回傳全域政策
        // 所以用內部 policies 長度確認：只有 valid-agent（無 global.yaml）
        // 透過 upsert 驗證：evil..agent 的 policies 不存在
        let evil_policies = registry.get_policies_for_agent("evil..agent").await;
        // 沒有 global.yaml，evil..agent 的專屬政策應為空
        assert!(
            evil_policies.is_empty(),
            "maliciously named yaml should not be loaded as agent policies"
        );
    }
}
