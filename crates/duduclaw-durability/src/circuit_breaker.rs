//! CircuitBreaker — 外部依賴熔斷保護
//!
//! 實作三狀態機：CLOSED → OPEN → HALF_OPEN → CLOSED
//!
//! ## 設計原則
//! - 所有 threshold 參數**必須從配置讀取**，禁止 magic number
//! - 每個 dependency 配置獨立，互不影響
//! - 狀態變更觸發 callback（供 Audit Trail 發射事件）
//!
//! ## 狀態機
//! ```text
//! CLOSED ──(failure_rate > threshold)──> OPEN
//! OPEN   ──(reset_timeout 到期)──────> HALF_OPEN
//! HALF_OPEN ──(probe_success)──────────> CLOSED
//! HALF_OPEN ──(probe_failure)──────────> OPEN
//! ```

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Errors ────────────────────────────────────────────────────────────────────

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CircuitBreakerError {
    #[error("circuit breaker OPEN for dependency '{dependency}': {reason}")]
    CircuitOpen {
        dependency: String,
        reason: String,
    },

    #[error("dependency '{0}' not configured")]
    DependencyNotFound(String),
}

// ── Config ────────────────────────────────────────────────────────────────────

/// 單一依賴的熔斷器配置（所有 threshold 均可配置，禁止 magic number）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CircuitBreakerDependencyConfig {
    /// 依賴名稱（唯一識別碼）。
    pub name: String,
    /// 失敗率觸發熔斷的閾值（0.0 - 1.0）。可配置，禁止硬編碼。
    pub failure_rate_threshold: f64,
    /// 計算失敗率所需的最小請求數。可配置，禁止硬編碼。
    pub min_request_count: u32,
    /// 熔斷後等待探測的時間（秒）。可配置，禁止硬編碼。
    pub reset_timeout_seconds: u64,
    /// HALF_OPEN 狀態需連續成功幾次才恢復 CLOSED。可配置，禁止硬編碼。
    pub probe_success_required: u32,
    /// 滑動視窗大小（用於計算失敗率的最近 N 次請求）。
    #[serde(default = "default_window_size")]
    pub window_size: u32,
}

fn default_window_size() -> u32 {
    20
}

impl CircuitBreakerDependencyConfig {
    /// 驗證配置合法性。
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() {
            return Err("dependency name cannot be empty".into());
        }
        if self.failure_rate_threshold <= 0.0 || self.failure_rate_threshold > 1.0 {
            return Err(format!(
                "failure_rate_threshold must be in (0.0, 1.0], got {}",
                self.failure_rate_threshold
            ));
        }
        if self.min_request_count == 0 {
            return Err("min_request_count must be >= 1".into());
        }
        if self.reset_timeout_seconds == 0 {
            return Err("reset_timeout_seconds must be >= 1".into());
        }
        if self.probe_success_required == 0 {
            return Err("probe_success_required must be >= 1".into());
        }
        if self.window_size == 0 {
            return Err("window_size must be >= 1".into());
        }
        Ok(())
    }

    /// memory_service 預設配置（W19 規格 §2.6）。
    pub fn default_memory_service() -> Self {
        Self {
            name: "memory_service".into(),
            failure_rate_threshold: 0.5,
            min_request_count: 10,
            reset_timeout_seconds: 30,
            probe_success_required: 2,
            window_size: 20,
        }
    }

    /// external_mcp_client 預設配置（W19 規格 §2.6）。
    pub fn default_external_mcp_client() -> Self {
        Self {
            name: "external_mcp_client".into(),
            failure_rate_threshold: 0.3,
            min_request_count: 5,
            reset_timeout_seconds: 60,
            probe_success_required: 1,
            window_size: 20,
        }
    }

    /// wiki_service 預設配置（W19 規格 §2.6）。
    pub fn default_wiki_service() -> Self {
        Self {
            name: "wiki_service".into(),
            failure_rate_threshold: 0.4,
            min_request_count: 8,
            reset_timeout_seconds: 45,
            probe_success_required: 2,
            window_size: 20,
        }
    }
}

// ── BreakerState ──────────────────────────────────────────────────────────────

/// 熔斷器的三種狀態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    /// 正常運作 — 所有請求通過。
    Closed,
    /// 熔斷 — 請求被阻止，等待 reset_timeout。
    Open,
    /// 探測中 — 少量請求通過以測試恢復。
    HalfOpen,
}

impl std::fmt::Display for BreakerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::Open => write!(f, "open"),
            Self::HalfOpen => write!(f, "half_open"),
        }
    }
}

// ── StateTransition ───────────────────────────────────────────────────────────

/// 狀態轉換事件（用於 Audit Trail 發射）。
#[derive(Debug, Clone)]
pub struct StateTransition {
    pub dependency: String,
    pub from: BreakerState,
    pub to: BreakerState,
    pub failure_rate: Option<f64>,
    pub request_count: u32,
    pub reset_timeout_seconds: Option<u64>,
}

// ── Per-dependency runtime state ──────────────────────────────────────────────

#[derive(Debug)]
struct DependencyState {
    state: BreakerState,
    /// 請求結果滑動視窗（true = success, false = failure）。
    window: std::collections::VecDeque<bool>,
    /// 狀態進入 OPEN 的時間（用於計算 reset_timeout）。
    opened_at: Option<Instant>,
    /// HALF_OPEN 狀態下連續成功次數。
    consecutive_successes: u32,
    /// HALF_OPEN 狀態下目前在途（inflight）探測請求數。
    ///
    /// before_call 允許探測通過時遞增，after_call 回報結果後遞減。
    /// 限制並發探測數量不超過 `probe_success_required`，防止多個並發
    /// 探測同時成功導致 `consecutive_successes` 過度遞增而過早恢復到 CLOSED。
    probe_inflight: u32,
}

impl DependencyState {
    fn new() -> Self {
        Self {
            state: BreakerState::Closed,
            window: std::collections::VecDeque::new(),
            opened_at: None,
            consecutive_successes: 0,
            probe_inflight: 0,
        }
    }

    fn failure_rate(&self, config: &CircuitBreakerDependencyConfig) -> (f64, u32) {
        let count = self.window.len() as u32;
        if count < config.min_request_count {
            return (0.0, count);
        }
        let failures = self.window.iter().filter(|&&s| !s).count() as f64;
        (failures / count as f64, count)
    }

    fn record(&mut self, success: bool, window_size: u32) {
        self.window.push_back(success);
        // Keep only the last window_size results
        while self.window.len() > window_size as usize {
            self.window.pop_front();
        }
    }
}

// ── CircuitBreakerRegistry ────────────────────────────────────────────────────

/// 多依賴熔斷器管理中心。
///
/// 每個依賴有獨立的熔斷狀態，可動態新增/更新配置。
pub struct CircuitBreakerRegistry {
    configs: RwLock<HashMap<String, CircuitBreakerDependencyConfig>>,
    states: RwLock<HashMap<String, DependencyState>>,
    /// 狀態轉換 callback（用於 Audit Trail）。
    transition_callbacks: RwLock<Vec<Arc<dyn Fn(StateTransition) + Send + Sync>>>,
}

impl CircuitBreakerRegistry {
    /// 建立包含預設依賴配置的 Registry。
    pub fn new_with_defaults() -> Self {
        let mut configs = HashMap::new();
        for cfg in [
            CircuitBreakerDependencyConfig::default_memory_service(),
            CircuitBreakerDependencyConfig::default_external_mcp_client(),
            CircuitBreakerDependencyConfig::default_wiki_service(),
        ] {
            configs.insert(cfg.name.clone(), cfg);
        }
        Self {
            configs: RwLock::new(configs),
            states: RwLock::new(HashMap::new()),
            transition_callbacks: RwLock::new(vec![]),
        }
    }

    /// 建立空 Registry（供測試使用）。
    pub fn empty() -> Self {
        Self {
            configs: RwLock::new(HashMap::new()),
            states: RwLock::new(HashMap::new()),
            transition_callbacks: RwLock::new(vec![]),
        }
    }

    /// 新增或更新依賴配置（熱重載支援）。
    pub async fn upsert_config(
        &self,
        config: CircuitBreakerDependencyConfig,
    ) -> Result<(), String> {
        config.validate()?;
        let mut configs = self.configs.write().await;
        configs.insert(config.name.clone(), config);
        Ok(())
    }

    /// 註冊狀態轉換 callback。
    pub async fn on_transition(&self, cb: impl Fn(StateTransition) + Send + Sync + 'static) {
        let mut callbacks = self.transition_callbacks.write().await;
        callbacks.push(Arc::new(cb));
    }

    /// 查詢依賴的目前狀態。
    pub async fn state(&self, dependency: &str) -> Option<BreakerState> {
        // First check if config exists
        {
            let configs = self.configs.read().await;
            if !configs.contains_key(dependency) {
                return None;
            }
        }
        let states = self.states.read().await;
        Some(states.get(dependency).map_or(BreakerState::Closed, |s| s.state))
    }

    /// 在呼叫依賴前檢查熔斷狀態。
    ///
    /// 回傳 `Ok(())` 表示允許請求通過，`Err(CircuitOpen)` 表示熔斷中。
    pub async fn before_call(&self, dependency: &str) -> Result<(), CircuitBreakerError> {
        let config = {
            let configs = self.configs.read().await;
            configs
                .get(dependency)
                .cloned()
                .ok_or_else(|| CircuitBreakerError::DependencyNotFound(dependency.into()))?
        };

        let mut states = self.states.write().await;
        let state = states.entry(dependency.to_string()).or_insert_with(DependencyState::new);

        match state.state {
            BreakerState::Closed => Ok(()),
            BreakerState::Open => {
                // Check if reset_timeout has elapsed
                if let Some(opened_at) = state.opened_at {
                    let reset = Duration::from_secs(config.reset_timeout_seconds);
                    if opened_at.elapsed() >= reset {
                        // Transition OPEN → HALF_OPEN
                        let prev = state.state;
                        state.state = BreakerState::HalfOpen;
                        state.consecutive_successes = 0;
                        drop(states);
                        info!(dependency, "circuit breaker: OPEN → HALF_OPEN");
                        self.fire_transition(StateTransition {
                            dependency: dependency.into(),
                            from: prev,
                            to: BreakerState::HalfOpen,
                            failure_rate: None,
                            request_count: 0,
                            reset_timeout_seconds: None,
                        })
                        .await;
                        return Ok(()); // Allow the probe
                    }
                }
                Err(CircuitBreakerError::CircuitOpen {
                    dependency: dependency.into(),
                    reason: format!(
                        "circuit is OPEN, reset_timeout={}s",
                        config.reset_timeout_seconds
                    ),
                })
            }
            BreakerState::HalfOpen => {
                // 限制並發探測數量不超過 probe_success_required，防止多個探測並發
                // 導致 consecutive_successes 過度計數，過早觸發 HALF_OPEN → CLOSED。
                if state.probe_inflight >= config.probe_success_required {
                    return Err(CircuitBreakerError::CircuitOpen {
                        dependency: dependency.into(),
                        reason: format!(
                            "circuit is HALF_OPEN, {} probe(s) already inflight (max {})",
                            state.probe_inflight, config.probe_success_required
                        ),
                    });
                }
                state.probe_inflight = state.probe_inflight.saturating_add(1);
                Ok(())
            }
        }
    }

    /// 在依賴呼叫後回報結果（成功或失敗）。
    pub async fn after_call(&self, dependency: &str, success: bool) {
        let config = {
            let configs = self.configs.read().await;
            match configs.get(dependency).cloned() {
                Some(c) => c,
                None => return, // Unknown dependency, ignore
            }
        };

        let mut states = self.states.write().await;
        let state = states.entry(dependency.to_string()).or_insert_with(DependencyState::new);

        let prev_breaker_state = state.state;

        match state.state {
            BreakerState::Closed => {
                state.record(success, config.window_size);
                let (failure_rate, count) = state.failure_rate(&config);

                if failure_rate > config.failure_rate_threshold {
                    // CLOSED → OPEN
                    state.state = BreakerState::Open;
                    state.opened_at = Some(Instant::now());
                    drop(states);
                    warn!(
                        dependency,
                        failure_rate,
                        threshold = config.failure_rate_threshold,
                        "circuit breaker: CLOSED → OPEN"
                    );
                    self.fire_transition(StateTransition {
                        dependency: dependency.into(),
                        from: prev_breaker_state,
                        to: BreakerState::Open,
                        failure_rate: Some(failure_rate),
                        request_count: count,
                        reset_timeout_seconds: Some(config.reset_timeout_seconds),
                    })
                    .await;
                }
            }
            BreakerState::HalfOpen => {
                // 探測結果回報，釋放 probe_inflight slot（無論成功或失敗）
                state.probe_inflight = state.probe_inflight.saturating_sub(1);

                if success {
                    state.consecutive_successes += 1;
                    if state.consecutive_successes >= config.probe_success_required {
                        // HALF_OPEN → CLOSED (recovered!)
                        state.state = BreakerState::Closed;
                        state.window.clear();
                        state.opened_at = None;
                        state.consecutive_successes = 0;
                        state.probe_inflight = 0;
                        drop(states);
                        info!(dependency, "circuit breaker: HALF_OPEN → CLOSED (recovered)");
                        self.fire_transition(StateTransition {
                            dependency: dependency.into(),
                            from: prev_breaker_state,
                            to: BreakerState::Closed,
                            failure_rate: None,
                            request_count: 0,
                            reset_timeout_seconds: None,
                        })
                        .await;
                    }
                } else {
                    // HALF_OPEN → OPEN (probe failed)
                    state.state = BreakerState::Open;
                    state.opened_at = Some(Instant::now());
                    state.consecutive_successes = 0;
                    state.probe_inflight = 0;
                    drop(states);
                    warn!(dependency, "circuit breaker: HALF_OPEN → OPEN (probe failed)");
                    self.fire_transition(StateTransition {
                        dependency: dependency.into(),
                        from: prev_breaker_state,
                        to: BreakerState::Open,
                        failure_rate: None,
                        request_count: 0,
                        reset_timeout_seconds: Some(config.reset_timeout_seconds),
                    })
                    .await;
                }
            }
            BreakerState::Open => {
                // Shouldn't get here (before_call should block OPEN requests)
                // If it does, just record
                state.record(success, config.window_size);
            }
        }
    }

    async fn fire_transition(&self, transition: StateTransition) {
        let callbacks = self.transition_callbacks.read().await;
        for cb in callbacks.iter() {
            cb(transition.clone());
        }
    }

    /// 強制重置依賴到 CLOSED 狀態（供測試和管理 API 使用）。
    pub async fn force_close(&self, dependency: &str) {
        let mut states = self.states.write().await;
        if let Some(state) = states.get_mut(dependency) {
            state.state = BreakerState::Closed;
            state.window.clear();
            state.opened_at = None;
            state.consecutive_successes = 0;
            state.probe_inflight = 0;
        }
    }

    /// 取得所有依賴的狀態快照。
    pub async fn all_states(&self) -> HashMap<String, BreakerState> {
        let configs = self.configs.read().await;
        let states = self.states.read().await;
        configs
            .keys()
            .map(|name| {
                let state = states.get(name).map_or(BreakerState::Closed, |s| s.state);
                (name.clone(), state)
            })
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(
        name: &str,
        failure_threshold: f64,
        min_requests: u32,
        reset_secs: u64,
        probe_required: u32,
    ) -> CircuitBreakerDependencyConfig {
        CircuitBreakerDependencyConfig {
            name: name.into(),
            failure_rate_threshold: failure_threshold,
            min_request_count: min_requests,
            reset_timeout_seconds: reset_secs,
            probe_success_required: probe_required,
            window_size: 10,
        }
    }

    async fn registry_with(config: CircuitBreakerDependencyConfig) -> CircuitBreakerRegistry {
        let reg = CircuitBreakerRegistry::empty();
        reg.upsert_config(config).await.unwrap();
        reg
    }

    // ── Config validation ─────────────────────────────────────────────────────

    #[test]
    fn test_config_valid() {
        let cfg = CircuitBreakerDependencyConfig::default_memory_service();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_config_empty_name_invalid() {
        let mut cfg = CircuitBreakerDependencyConfig::default_memory_service();
        cfg.name = "".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_threshold_out_of_range_invalid() {
        let mut cfg = CircuitBreakerDependencyConfig::default_memory_service();
        cfg.failure_rate_threshold = 1.5; // > 1.0
        assert!(cfg.validate().is_err());

        cfg.failure_rate_threshold = 0.0; // must be > 0
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_config_zero_min_requests_invalid() {
        let mut cfg = CircuitBreakerDependencyConfig::default_memory_service();
        cfg.min_request_count = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_all_default_configs_valid() {
        for cfg in [
            CircuitBreakerDependencyConfig::default_memory_service(),
            CircuitBreakerDependencyConfig::default_external_mcp_client(),
            CircuitBreakerDependencyConfig::default_wiki_service(),
        ] {
            assert!(cfg.validate().is_ok(), "default config '{}' should be valid", cfg.name);
        }
    }

    // ── State machine: initial state ──────────────────────────────────────────

    #[tokio::test]
    async fn test_initial_state_is_closed() {
        let reg = registry_with(make_config("svc", 0.5, 5, 30, 1)).await;
        assert_eq!(reg.state("svc").await, Some(BreakerState::Closed));
    }

    #[tokio::test]
    async fn test_unknown_dependency_state_is_none() {
        let reg = CircuitBreakerRegistry::empty();
        assert_eq!(reg.state("unknown").await, None);
    }

    // ── State machine: CLOSED → OPEN ──────────────────────────────────────────

    #[tokio::test]
    async fn test_circuit_opens_when_failure_rate_exceeds_threshold() {
        // threshold=50%, min_requests=4
        let reg = registry_with(make_config("svc", 0.5, 4, 30, 1)).await;

        // 2 successes, 3 failures → 60% failure rate
        for _ in 0..2 {
            reg.after_call("svc", true).await;
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::Closed));

        for _ in 0..3 {
            reg.after_call("svc", false).await;
        }

        assert_eq!(
            reg.state("svc").await,
            Some(BreakerState::Open),
            "60% failure rate should open circuit at 50% threshold"
        );
    }

    #[tokio::test]
    async fn test_circuit_stays_closed_below_min_request_count() {
        // min_requests=10, so fewer requests should not trigger opening
        let reg = registry_with(make_config("svc", 0.5, 10, 30, 1)).await;

        // 5 failures — below min_request_count
        for _ in 0..5 {
            reg.after_call("svc", false).await;
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::Closed));
    }

    // ── before_call: OPEN returns error ──────────────────────────────────────

    #[tokio::test]
    async fn test_before_call_open_circuit_returns_error() {
        let reg = registry_with(make_config("svc", 0.5, 4, 3600, 1)).await;

        // Trigger opening
        for _ in 0..4 {
            reg.after_call("svc", false).await;
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::Open));

        // before_call should return error
        let result = reg.before_call("svc").await;
        assert!(result.is_err());
        assert!(matches!(result, Err(CircuitBreakerError::CircuitOpen { .. })));
    }

    #[tokio::test]
    async fn test_before_call_closed_circuit_returns_ok() {
        let reg = registry_with(make_config("svc", 0.5, 4, 30, 1)).await;
        assert!(reg.before_call("svc").await.is_ok());
    }

    #[tokio::test]
    async fn test_before_call_unknown_dependency_returns_error() {
        let reg = CircuitBreakerRegistry::empty();
        let result = reg.before_call("unknown").await;
        assert!(matches!(result, Err(CircuitBreakerError::DependencyNotFound(_))));
    }

    // ── State machine: HALF_OPEN → CLOSED (recovery) ─────────────────────────

    #[tokio::test]
    async fn test_recovery_after_probe_success() {
        // reset_timeout=0 effectively (we use force_close as a shortcut here)
        let reg = registry_with(make_config("svc", 0.5, 4, 3600, 2)).await;

        // Trigger open
        for _ in 0..4 {
            reg.after_call("svc", false).await;
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::Open));

        // Force to HALF_OPEN for testing (bypassing reset_timeout)
        {
            let mut states = reg.states.write().await;
            if let Some(s) = states.get_mut("svc") {
                s.state = BreakerState::HalfOpen;
                s.consecutive_successes = 0;
            }
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::HalfOpen));

        // Two consecutive successes → CLOSED
        reg.after_call("svc", true).await;
        assert_eq!(reg.state("svc").await, Some(BreakerState::HalfOpen));

        reg.after_call("svc", true).await;
        assert_eq!(
            reg.state("svc").await,
            Some(BreakerState::Closed),
            "after 2 probe successes, circuit should close"
        );
    }

    // ── State machine: HALF_OPEN → OPEN (probe failure) ──────────────────────

    #[tokio::test]
    async fn test_probe_failure_reopens_circuit() {
        let reg = registry_with(make_config("svc", 0.5, 4, 3600, 2)).await;

        // Force to HALF_OPEN
        {
            let mut states = reg.states.write().await;
            states.insert("svc".into(), DependencyState {
                state: BreakerState::HalfOpen,
                window: Default::default(),
                opened_at: None,
                consecutive_successes: 0,
                probe_inflight: 0,
            });
        }

        // Probe failure → OPEN again
        reg.after_call("svc", false).await;
        assert_eq!(reg.state("svc").await, Some(BreakerState::Open));
    }

    // ── Configurable thresholds ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_thresholds_are_configurable_at_runtime() {
        let reg = CircuitBreakerRegistry::empty();

        // Start with lenient threshold
        reg.upsert_config(make_config("svc", 0.8, 4, 30, 1)).await.unwrap();

        // 70% failure rate — should NOT open at 80% threshold
        reg.after_call("svc", false).await;
        reg.after_call("svc", false).await;
        reg.after_call("svc", false).await;
        reg.after_call("svc", true).await;
        assert_eq!(reg.state("svc").await, Some(BreakerState::Closed));

        // Update to stricter threshold
        reg.upsert_config(make_config("svc", 0.5, 4, 30, 1)).await.unwrap();

        // 5th call: still failure → now 4 fail out of 5 = 80% > 50% threshold
        reg.after_call("svc", false).await;
        assert_eq!(
            reg.state("svc").await,
            Some(BreakerState::Open),
            "updated threshold should take effect immediately"
        );
    }

    // ── Independent dependencies ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_dependencies_are_independent() {
        let reg = CircuitBreakerRegistry::empty();
        reg.upsert_config(make_config("svc-a", 0.5, 4, 30, 1)).await.unwrap();
        reg.upsert_config(make_config("svc-b", 0.5, 4, 30, 1)).await.unwrap();

        // Open svc-a
        for _ in 0..4 {
            reg.after_call("svc-a", false).await;
        }
        assert_eq!(reg.state("svc-a").await, Some(BreakerState::Open));

        // svc-b should still be closed
        assert_eq!(reg.state("svc-b").await, Some(BreakerState::Closed));
        assert!(reg.before_call("svc-b").await.is_ok());
    }

    // ── force_close ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_force_close_resets_to_closed() {
        let reg = registry_with(make_config("svc", 0.5, 4, 3600, 1)).await;

        // Trigger open
        for _ in 0..4 {
            reg.after_call("svc", false).await;
        }
        assert_eq!(reg.state("svc").await, Some(BreakerState::Open));

        reg.force_close("svc").await;
        assert_eq!(reg.state("svc").await, Some(BreakerState::Closed));
        assert!(reg.before_call("svc").await.is_ok());
    }

    // ── Transition callback ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_transition_callback_fired_on_open() {
        let reg = registry_with(make_config("svc", 0.5, 4, 30, 1)).await;
        let transitions = Arc::new(RwLock::new(vec![]));

        let t = Arc::clone(&transitions);
        reg.on_transition(move |transition| {
            let t = Arc::clone(&t);
            tokio::spawn(async move {
                t.write().await.push(transition.to);
            });
        })
        .await;

        // Trigger CLOSED → OPEN
        for _ in 0..4 {
            reg.after_call("svc", false).await;
        }

        // Give the spawned task time to run
        tokio::time::sleep(Duration::from_millis(10)).await;

        let logged = transitions.read().await;
        assert!(
            logged.contains(&BreakerState::Open),
            "OPEN transition should have been fired"
        );
    }

    // ── all_states ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_all_states_returns_all_dependencies() {
        let reg = CircuitBreakerRegistry::new_with_defaults();
        let states = reg.all_states().await;
        assert!(states.contains_key("memory_service"));
        assert!(states.contains_key("external_mcp_client"));
        assert!(states.contains_key("wiki_service"));
        assert_eq!(states["memory_service"], BreakerState::Closed);
    }
}
