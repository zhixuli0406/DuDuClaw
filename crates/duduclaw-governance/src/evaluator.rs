//! PolicyEvaluator — 政策評估引擎
//!
//! ## 設計
//! - `evaluate(agent_id, operation) -> EvaluationResult`
//! - 本地 in-memory 快取（TTL 60s，快取 permission/lifecycle/quota 結構）
//! - Rate 計數器使用 Sliding Window（精確度誤差 < 1%）
//! - p99 < 5ms（所有評估在記憶體內完成）

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
    policy::{
        ActionOnViolation, LifecyclePolicy, PermissionPolicy, PolicyType, QuotaPolicy, RatePolicy,
        Resource,
    },
    registry::PolicyRegistry,
    Operation, OperationType,
};

// ── ViolationType ─────────────────────────────────────────────────────────────

/// 違規類型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationType {
    RateExceeded,
    PermissionDenied,
    QuotaExceeded,
    ApprovalRequired,
    /// Agent 違反生命週期政策（idle 超時等）。
    LifecycleViolation,
}

impl std::fmt::Display for ViolationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateExceeded => write!(f, "rate_exceeded"),
            Self::PermissionDenied => write!(f, "permission_denied"),
            Self::QuotaExceeded => write!(f, "quota_exceeded"),
            Self::ApprovalRequired => write!(f, "approval_required"),
            Self::LifecycleViolation => write!(f, "lifecycle_violation"),
        }
    }
}

// ── EvaluationResult ──────────────────────────────────────────────────────────

/// 政策評估結果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationResult {
    /// 操作是否被允許。
    pub allowed: bool,
    /// 觸發評估結果的政策 ID（如適用）。
    pub policy_id: Option<String>,
    /// 違規類型（`None` 表示操作被允許）。
    pub violation_type: Option<ViolationType>,
    /// 操作是否需要核准（需要核准不算違規）。
    pub approval_required: bool,
    /// 核准申請 ID（若 `approval_required = true` 時由 ApprovalWorkflow 填充）。
    pub approval_request_id: Option<String>,
    /// 人類可讀的評估說明。
    pub message: String,
}

impl EvaluationResult {
    /// 建立「允許」結果。
    pub fn allow() -> Self {
        Self {
            allowed: true,
            policy_id: None,
            violation_type: None,
            approval_required: false,
            approval_request_id: None,
            message: "allowed".into(),
        }
    }

    /// 建立「拒絕」結果。
    pub fn deny(policy_id: impl Into<String>, violation: ViolationType, msg: impl Into<String>) -> Self {
        Self {
            allowed: false,
            policy_id: Some(policy_id.into()),
            violation_type: Some(violation),
            approval_required: false,
            approval_request_id: None,
            message: msg.into(),
        }
    }

    /// 建立「需要核准」結果（不是拒絕，是待核准）。
    pub fn require_approval(
        policy_id: impl Into<String>,
        msg: impl Into<String>,
    ) -> Self {
        Self {
            allowed: false,
            policy_id: Some(policy_id.into()),
            violation_type: Some(ViolationType::ApprovalRequired),
            approval_required: true,
            approval_request_id: None,
            message: msg.into(),
        }
    }

    /// 建立「警告但允許」結果（rate policy 設為 warn 時）。
    pub fn warn(policy_id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            allowed: true,
            policy_id: Some(policy_id.into()),
            violation_type: Some(ViolationType::RateExceeded),
            approval_required: false,
            approval_request_id: None,
            message: msg.into(),
        }
    }
}

// ── Sliding Window Rate Counter ───────────────────────────────────────────────

/// 滑動視窗速率計數器，精確追蹤單位時間內的操作次數。
#[derive(Debug, Default)]
pub struct SlidingWindow {
    /// 操作時間戳記佇列（Instant）。
    timestamps: VecDeque<Instant>,
}

impl SlidingWindow {
    /// 在視窗 `window` 內記錄一次操作，並檢查是否超過 `limit`。
    ///
    /// 回傳 `true` 表示允許（未超限），`false` 表示超限。
    pub fn record_and_check(&mut self, window: Duration, limit: u32) -> bool {
        let now = Instant::now();
        let cutoff = now - window;

        // Remove expired timestamps
        while let Some(&front) = self.timestamps.front() {
            if front < cutoff {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }

        let current_count = self.timestamps.len() as u32;
        if current_count >= limit {
            // Over limit — don't record this attempt
            false
        } else {
            self.timestamps.push_back(now);
            true
        }
    }

    /// 查詢目前視窗內的操作次數（不記錄新操作）。
    pub fn current_count(&mut self, window: Duration) -> u32 {
        let cutoff = Instant::now() - window;
        while let Some(&front) = self.timestamps.front() {
            if front < cutoff {
                self.timestamps.pop_front();
            } else {
                break;
            }
        }
        self.timestamps.len() as u32
    }
}

// ── Quota State ───────────────────────────────────────────────────────────────

/// 配額狀態（每 Agent 一份）。
#[derive(Debug)]
struct QuotaState {
    token_used: u64,
    /// 原子性預留：每次 evaluate_quota 通過後遞增 1，
    /// record_tokens_used 呼叫後遞減 1。防止 TOCTOU 競爭超過 daily_token_budget。
    token_reserved: u64,
    concurrent_tasks: u32,
    /// 原子性預留：每次 evaluate_quota 通過 concurrent_tasks 檢查後遞增 1，
    /// increment_concurrent_tasks 確認後遞減 1。防止 TOCTOU 競爭超過 max_concurrent_tasks。
    pending_tasks: u32,
    memory_entries: u64,
    reset_at: Instant,
}

impl QuotaState {
    fn new() -> Self {
        Self {
            token_used: 0,
            token_reserved: 0,
            concurrent_tasks: 0,
            pending_tasks: 0,
            memory_entries: 0,
            reset_at: Instant::now() + Duration::from_secs(86400), // 24h default
        }
    }

    fn reset_if_needed(&mut self) {
        if Instant::now() >= self.reset_at {
            self.token_used = 0;
            self.token_reserved = 0;
            self.concurrent_tasks = 0;
            self.pending_tasks = 0;
            self.memory_entries = 0;
            self.reset_at = Instant::now() + Duration::from_secs(86400);
        }
    }
}

// ── PolicyEvaluator ───────────────────────────────────────────────────────────

/// 速率計數器的 Key：(agent_id, resource)。
type RateCounterKey = (String, String);

/// 配額狀態的 Key：agent_id。
type QuotaKey = String;

/// PolicyEvaluator — 評估 Agent 操作是否符合政策。
pub struct PolicyEvaluator {
    registry: Arc<PolicyRegistry>,
    /// Rate counter: (agent_id, resource) → SlidingWindow
    rate_counters: RwLock<HashMap<RateCounterKey, SlidingWindow>>,
    /// Quota state: agent_id → QuotaState
    quota_states: RwLock<HashMap<QuotaKey, QuotaState>>,
    /// Last activity time: agent_id → Instant（用於 LifecyclePolicy idle 計算）
    last_activity_times: RwLock<HashMap<String, Instant>>,
    /// Policy cache TTL（預設 60s）。
    _cache_ttl: Duration,
}

impl PolicyEvaluator {
    /// 建立 PolicyEvaluator，使用預設 TTL 60s。
    pub fn new(registry: Arc<PolicyRegistry>) -> Self {
        Self {
            registry,
            rate_counters: RwLock::new(HashMap::new()),
            quota_states: RwLock::new(HashMap::new()),
            last_activity_times: RwLock::new(HashMap::new()),
            _cache_ttl: Duration::from_secs(60),
        }
    }

    /// 建立 PolicyEvaluator，使用自訂 cache TTL。
    pub fn with_cache_ttl(registry: Arc<PolicyRegistry>, ttl: Duration) -> Self {
        Self {
            registry,
            rate_counters: RwLock::new(HashMap::new()),
            quota_states: RwLock::new(HashMap::new()),
            last_activity_times: RwLock::new(HashMap::new()),
            _cache_ttl: ttl,
        }
    }

    /// 評估 `agent_id` 執行 `operation` 是否符合所有適用政策。
    ///
    /// 評估順序：Permission → Quota → Rate（Rate 最後因需記錄計數）。
    pub async fn evaluate(&self, agent_id: &str, operation: &Operation) -> EvaluationResult {
        let policies = self.registry.get_policies_for_agent(agent_id).await;

        // 1. Permission check
        for policy in &policies {
            if let PolicyType::Permission(p) = policy {
                if Self::applies_to(&p.agent_id, agent_id) {
                    let result = self.evaluate_permission(p, operation);
                    if result.is_some() {
                        return result.unwrap();
                    }
                }
            }
        }

        // 2. Quota check
        for policy in &policies {
            if let PolicyType::Quota(q) = policy {
                if Self::applies_to(&q.agent_id, agent_id) {
                    let result = self.evaluate_quota(agent_id, q, operation).await;
                    if result.is_some() {
                        return result.unwrap();
                    }
                }
            }
        }

        // 3. Lifecycle check
        for policy in &policies {
            if let PolicyType::Lifecycle(lp) = policy {
                if Self::applies_to(&lp.agent_id, agent_id) {
                    let result = self.evaluate_lifecycle(agent_id, lp).await;
                    if result.is_some() {
                        return result.unwrap();
                    }
                }
            }
        }

        // 4. Rate check (last because it records the attempt)
        let resource = Self::operation_to_resource(operation);
        for policy in &policies {
            if let PolicyType::Rate(r) = policy {
                if Self::applies_to(&r.agent_id, agent_id)
                    && resource.as_ref().map_or(false, |res| *res == r.resource)
                {
                    let result = self.evaluate_rate(agent_id, r).await;
                    if result.is_some() {
                        return result.unwrap();
                    }
                }
            }
        }

        EvaluationResult::allow()
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// 檢查政策是否適用於此 Agent（`"*"` 適用於所有）。
    fn applies_to(policy_agent_id: &str, agent_id: &str) -> bool {
        policy_agent_id == "*" || policy_agent_id == agent_id
    }

    /// 將操作類型對應到 Rate 資源類型。
    fn operation_to_resource(operation: &Operation) -> Option<Resource> {
        match &operation.op_type {
            OperationType::McpCall => Some(Resource::McpCalls),
            OperationType::MemoryWrite => Some(Resource::MemoryWrites),
            OperationType::WikiWrite => Some(Resource::WikiWrites),
            OperationType::MessageSend => Some(Resource::MessageSends),
            _ => None,
        }
    }

    /// 評估 PermissionPolicy。
    ///
    /// 回傳 `Some(result)` 表示有評估結果（允許、拒絕或需核准）；
    /// `None` 表示此政策對此操作不產生結論，繼續評估下一個政策。
    fn evaluate_permission(
        &self,
        policy: &PermissionPolicy,
        operation: &Operation,
    ) -> Option<EvaluationResult> {
        let scope = &operation.scope;

        // Check if scope requires approval
        if policy.requires_approval_for(scope) {
            return Some(EvaluationResult::require_approval(
                &policy.policy_id,
                format!("operation '{scope}' requires approval per policy '{}'", policy.policy_id),
            ));
        }

        // Check if scope is denied
        if policy.denied_scopes.contains(scope) {
            return Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::PermissionDenied,
                format!("scope '{scope}' is denied by policy '{}'", policy.policy_id),
            ));
        }

        // Check if scope is allowed (only when allowed_scopes is non-empty)
        if !policy.allowed_scopes.is_empty() && !policy.allowed_scopes.contains(scope) {
            return Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::PermissionDenied,
                format!("scope '{scope}' is not in allowed_scopes of policy '{}'", policy.policy_id),
            ));
        }

        None // no conclusion from this policy
    }

    /// 評估 QuotaPolicy，並在通過檢查後原子性地預留配額資源。
    ///
    /// ## TOCTOU 防護
    /// 所有讀取-檢查-寫入操作在同一寫鎖內完成：
    /// - 通過 `max_concurrent_tasks` 檢查後立即遞增 `pending_tasks`
    /// - 通過 `daily_token_budget` 檢查後立即遞增 `token_reserved`
    ///
    /// 呼叫者在實際執行後必須：
    /// - 呼叫 `increment_concurrent_tasks()` 確認並轉為正式計數（同時釋放 `pending_tasks`）
    /// - 呼叫 `record_tokens_used()` 記錄實際用量（同時釋放 `token_reserved`）
    async fn evaluate_quota(
        &self,
        agent_id: &str,
        policy: &QuotaPolicy,
        _operation: &Operation,
    ) -> Option<EvaluationResult> {
        let mut quota_map = self.quota_states.write().await;
        let state = quota_map
            .entry(agent_id.to_string())
            .or_insert_with(QuotaState::new);

        state.reset_if_needed();

        // Check concurrent tasks（含 pending 預留，防 TOCTOU）
        if state.concurrent_tasks.saturating_add(state.pending_tasks) >= policy.max_concurrent_tasks
        {
            return Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::QuotaExceeded,
                format!(
                    "max_concurrent_tasks ({}) exceeded for agent '{}' per policy '{}'",
                    policy.max_concurrent_tasks, agent_id, policy.policy_id
                ),
            ));
        }

        // Check token budget（含 reserved 預留，防 TOCTOU）
        if state.token_used.saturating_add(state.token_reserved) >= policy.daily_token_budget {
            return Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::QuotaExceeded,
                format!(
                    "daily_token_budget ({}) exhausted for agent '{}' per policy '{}'",
                    policy.daily_token_budget, agent_id, policy.policy_id
                ),
            ));
        }

        // Check memory entries
        if state.memory_entries >= policy.max_memory_entries {
            return Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::QuotaExceeded,
                format!(
                    "max_memory_entries ({}) exceeded for agent '{}' per policy '{}'",
                    policy.max_memory_entries, agent_id, policy.policy_id
                ),
            ));
        }

        // 原子性預留：在同一寫鎖內標記資源已被預定，防止並發請求競爭同一配額
        state.pending_tasks = state.pending_tasks.saturating_add(1);
        state.token_reserved = state.token_reserved.saturating_add(1);

        None
    }

    /// 評估 LifecyclePolicy（idle 時間計算）。
    ///
    /// 若 Agent 最後活躍時間超過 `max_idle_hours`，回傳 `LifecycleViolation`。
    /// 若 Agent 無活動記錄（新 Agent），視為合法（預設允許）。
    async fn evaluate_lifecycle(
        &self,
        agent_id: &str,
        policy: &LifecyclePolicy,
    ) -> Option<EvaluationResult> {
        let times = self.last_activity_times.read().await;
        let last_active = match times.get(agent_id) {
            Some(t) => *t,
            None => return None, // 新 Agent，無活動記錄 → 允許
        };
        drop(times);

        let idle_duration = Instant::now().saturating_duration_since(last_active);
        let max_idle = Duration::from_secs(policy.max_idle_hours as u64 * 3600);

        if idle_duration > max_idle {
            Some(EvaluationResult::deny(
                &policy.policy_id,
                ViolationType::LifecycleViolation,
                format!(
                    "agent '{}' has been idle for {}s, exceeds max_idle_hours={} (policy '{}')",
                    agent_id,
                    idle_duration.as_secs(),
                    policy.max_idle_hours,
                    policy.policy_id,
                ),
            ))
        } else {
            None
        }
    }

    /// 評估 RatePolicy（使用滑動視窗計數器）。
    async fn evaluate_rate(
        &self,
        agent_id: &str,
        policy: &RatePolicy,
    ) -> Option<EvaluationResult> {
        let key = (agent_id.to_string(), policy.resource.to_string());
        let window = Duration::from_secs(policy.window_seconds as u64);
        let limit = policy.limit;

        let mut counters = self.rate_counters.write().await;
        let window_counter = counters.entry(key).or_default();

        let allowed = window_counter.record_and_check(window, limit);

        if !allowed {
            return Some(match policy.action_on_violation {
                ActionOnViolation::Reject => EvaluationResult::deny(
                    &policy.policy_id,
                    ViolationType::RateExceeded,
                    format!(
                        "rate limit exceeded: {} {} per {}s (policy '{}')",
                        limit, policy.resource, policy.window_seconds, policy.policy_id
                    ),
                ),
                ActionOnViolation::Warn => EvaluationResult::warn(
                    &policy.policy_id,
                    format!(
                        "rate limit warning: {} {} per {}s exceeded (policy '{}')",
                        limit, policy.resource, policy.window_seconds, policy.policy_id
                    ),
                ),
                ActionOnViolation::Throttle => EvaluationResult::deny(
                    &policy.policy_id,
                    ViolationType::RateExceeded,
                    format!(
                        "rate limit throttled: {} {} per {}s (policy '{}')",
                        limit, policy.resource, policy.window_seconds, policy.policy_id
                    ),
                ),
            });
        }

        None
    }

    /// 更新配額使用量（成功執行操作後呼叫）。
    ///
    /// 同時釋放 `evaluate_quota()` 預留的 `token_reserved` slot，
    /// 確保後續請求可以使用真實的剩餘配額。
    pub async fn record_tokens_used(&self, agent_id: &str, tokens: u64) {
        let mut quota_map = self.quota_states.write().await;
        let state = quota_map
            .entry(agent_id.to_string())
            .or_insert_with(QuotaState::new);
        state.reset_if_needed();
        state.token_used = state.token_used.saturating_add(tokens);
        // 釋放 evaluate_quota 預留的 1 個 token slot
        state.token_reserved = state.token_reserved.saturating_sub(1);
    }

    /// 確認並發任務計數（evaluate_quota 通過後由呼叫者確認）。
    ///
    /// 同時釋放 `evaluate_quota()` 預留的 `pending_tasks` slot，
    /// 將預留轉換為正式的 `concurrent_tasks` 計數。
    pub async fn increment_concurrent_tasks(&self, agent_id: &str) {
        let mut quota_map = self.quota_states.write().await;
        let state = quota_map
            .entry(agent_id.to_string())
            .or_insert_with(QuotaState::new);
        state.reset_if_needed();
        state.concurrent_tasks = state.concurrent_tasks.saturating_add(1);
        // 釋放 evaluate_quota 預留的 1 個 pending slot
        state.pending_tasks = state.pending_tasks.saturating_sub(1);
    }

    /// 減少並發任務計數。
    pub async fn decrement_concurrent_tasks(&self, agent_id: &str) {
        let mut quota_map = self.quota_states.write().await;
        let state = quota_map
            .entry(agent_id.to_string())
            .or_insert_with(QuotaState::new);
        state.concurrent_tasks = state.concurrent_tasks.saturating_sub(1);
    }

    /// 記錄 Agent 的活動時間（更新 last_activity_time）。
    ///
    /// 每次 Agent 執行操作後應呼叫此方法，確保 LifecyclePolicy 正確評估 idle 時間。
    pub async fn record_activity(&self, agent_id: &str) {
        let mut times = self.last_activity_times.write().await;
        times.insert(agent_id.to_string(), Instant::now());
    }

    /// 強制設定 Agent 的最後活動時間（僅供測試使用）。
    ///
    /// 允許測試模擬 idle 超時情境，不應在生產程式碼中呼叫。
    #[cfg(test)]
    pub async fn set_last_activity_for_test(&self, agent_id: &str, t: Instant) {
        let mut times = self.last_activity_times.write().await;
        times.insert(agent_id.to_string(), t);
    }

    /// 取得目前 Rate 計數（用於測試/監控）。
    pub async fn current_rate_count(&self, agent_id: &str, resource: &Resource) -> u32 {
        let key = (agent_id.to_string(), resource.to_string());
        let mut counters = self.rate_counters.write().await;
        if let Some(window) = counters.get_mut(&key) {
            // We need window_seconds from policy, use a default for this query
            window.current_count(Duration::from_secs(60))
        } else {
            0
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn make_evaluator_with_policies(
        policies_yaml: &str,
    ) -> (Arc<PolicyRegistry>, PolicyEvaluator, TempDir) {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("global.yaml"), policies_yaml).unwrap();

        let registry = Arc::new(PolicyRegistry::new(dir.path().to_path_buf()));
        registry.load().await.unwrap();

        let evaluator = PolicyEvaluator::new(Arc::clone(&registry));
        // Return dir so it stays alive (not dropped) during the test
        (registry, evaluator, dir)
    }

    // ── Permission tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_permission_denied_scope() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: test-perm
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - wiki:read
    denied_scopes:
      - admin
    requires_approval: []
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "admin".into(),
            metadata: serde_json::json!({}),
        };
        let result = evaluator.evaluate("any-agent", &op).await;
        assert!(!result.allowed, "admin scope should be denied");
        assert_eq!(result.violation_type, Some(ViolationType::PermissionDenied));
    }

    #[tokio::test]
    async fn test_permission_allowed_scope() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: test-perm
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - wiki:read
    denied_scopes:
      - admin
    requires_approval: []
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::MemoryRead,
            resource_id: None,
            scope: "memory:read".into(),
            metadata: serde_json::json!({}),
        };
        let result = evaluator.evaluate("any-agent", &op).await;
        assert!(result.allowed, "memory:read should be allowed");
    }

    #[tokio::test]
    async fn test_approval_required() {
        let yaml = r#"
policies:
  - policy_type: permission
    policy_id: test-perm
    agent_id: "*"
    allowed_scopes: []
    denied_scopes: []
    requires_approval:
      - agent:create
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::AgentCreate,
            resource_id: None,
            scope: "agent:create".into(),
            metadata: serde_json::json!({}),
        };
        let result = evaluator.evaluate("any-agent", &op).await;
        assert!(!result.allowed);
        assert!(result.approval_required);
        assert_eq!(
            result.violation_type,
            Some(ViolationType::ApprovalRequired)
        );
    }

    // ── Rate limit tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_rate_limit_reject_on_exceed() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: test-rate
    agent_id: "*"
    resource: mcp_calls
    limit: 3
    window_seconds: 60
    action_on_violation: reject
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        };

        // First 3 should be allowed
        for i in 1..=3 {
            let result = evaluator.evaluate("test-agent", &op).await;
            assert!(result.allowed, "call {i} should be allowed");
        }

        // 4th should be denied
        let result = evaluator.evaluate("test-agent", &op).await;
        assert!(!result.allowed, "4th call should be denied");
        assert_eq!(result.violation_type, Some(ViolationType::RateExceeded));
    }

    #[tokio::test]
    async fn test_rate_limit_warn_on_exceed() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: test-rate-warn
    agent_id: "*"
    resource: mcp_calls
    limit: 2
    window_seconds: 60
    action_on_violation: warn
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        };

        // First 2 allowed
        for _ in 0..2 {
            evaluator.evaluate("test-agent", &op).await;
        }

        // 3rd: warn (allowed=true, violation_type=RateExceeded)
        let result = evaluator.evaluate("test-agent", &op).await;
        assert!(result.allowed, "warn action should still allow");
        assert_eq!(result.violation_type, Some(ViolationType::RateExceeded));
    }

    #[tokio::test]
    async fn test_rate_limit_different_agents_independent() {
        let yaml = r#"
policies:
  - policy_type: rate
    policy_id: test-rate
    agent_id: "*"
    resource: memory_writes
    limit: 2
    window_seconds: 60
    action_on_violation: reject
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        let op = Operation {
            op_type: OperationType::MemoryWrite,
            resource_id: None,
            scope: "memory:write".into(),
            metadata: serde_json::json!({}),
        };

        // Agent A uses 2 calls
        evaluator.evaluate("agent-a", &op).await;
        evaluator.evaluate("agent-a", &op).await;

        // Agent B should still have its full quota
        let result = evaluator.evaluate("agent-b", &op).await;
        assert!(result.allowed, "agent-b should not be affected by agent-a's rate");
    }

    // ── Quota tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_quota_concurrent_tasks_exceeded() {
        let yaml = r#"
policies:
  - policy_type: quota
    policy_id: test-quota
    agent_id: "*"
    daily_token_budget: 500000
    max_concurrent_tasks: 2
    max_memory_entries: 10000
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        // Simulate 2 concurrent tasks
        evaluator.increment_concurrent_tasks("test-agent").await;
        evaluator.increment_concurrent_tasks("test-agent").await;

        let op = Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        };

        let result = evaluator.evaluate("test-agent", &op).await;
        assert!(!result.allowed);
        assert_eq!(result.violation_type, Some(ViolationType::QuotaExceeded));
    }

    #[tokio::test]
    async fn test_quota_token_budget_exceeded() {
        let yaml = r#"
policies:
  - policy_type: quota
    policy_id: test-quota
    agent_id: "*"
    daily_token_budget: 100
    max_concurrent_tasks: 5
    max_memory_entries: 10000
"#;
        let (_reg, evaluator, _dir) = make_evaluator_with_policies(yaml).await;

        // Use all tokens
        evaluator.record_tokens_used("test-agent", 100).await;

        let op = Operation {
            op_type: OperationType::McpCall,
            resource_id: None,
            scope: "mcp:call".into(),
            metadata: serde_json::json!({}),
        };

        let result = evaluator.evaluate("test-agent", &op).await;
        assert!(!result.allowed);
        assert_eq!(result.violation_type, Some(ViolationType::QuotaExceeded));
    }

    // ── No policy = allow ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_no_policies_allow_all() {
        let dir = TempDir::new().unwrap();
        let registry = Arc::new(PolicyRegistry::new(dir.path()));
        registry.load().await.unwrap();
        let evaluator = PolicyEvaluator::new(registry);

        let op = Operation {
            op_type: OperationType::MemoryWrite,
            resource_id: None,
            scope: "memory:write".into(),
            metadata: serde_json::json!({}),
        };

        let result = evaluator.evaluate("any-agent", &op).await;
        assert!(result.allowed, "no policies → should allow all");
    }

    // ── Sliding window accuracy ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_sliding_window_accuracy() {
        let mut window = SlidingWindow::default();
        let w = Duration::from_secs(60);

        // Add 100 requests
        for _ in 0..100 {
            window.record_and_check(w, 200);
        }
        assert_eq!(window.current_count(w), 100);

        // 101st should still be within limit
        let ok = window.record_and_check(w, 200);
        assert!(ok);
        assert_eq!(window.current_count(w), 101);
    }
}
