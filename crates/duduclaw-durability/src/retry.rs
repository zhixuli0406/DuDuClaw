//! RetryEngine — 指數退避重試引擎
//!
//! 為關鍵操作提供智能重試機制，支援：
//! - 指數退避（Exponential Backoff）
//! - Jitter（避免重試風暴）
//! - Per-operation 重試策略
//! - 可重試/不可重試錯誤碼分類
//!
//! ## 使用範例
//! ```rust,ignore
//! let engine = RetryEngine::new();
//! let result = engine.execute("mcp_call", || async {
//!     do_mcp_call().await
//! }).await;
//! ```

use std::{collections::HashMap, time::Duration};

use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error, Clone)]
pub enum RetryError {
    #[error("retry exhausted after {attempts} attempts: {last_error}")]
    Exhausted {
        attempts: u32,
        last_error: String,
    },

    #[error("non-retryable error: {0}")]
    NonRetryable(String),

    #[error("retry policy not found: {0}")]
    PolicyNotFound(String),
}

// ── RetryPolicy ───────────────────────────────────────────────────────────────

/// 單一 operation type 的重試策略（所有參數可配置，禁止 magic number）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// 政策 ID（用於識別和稽核）。
    pub retry_policy_id: String,
    /// 適用的操作類型（`"*"` 代表通用後備）。
    pub operation_type: String,
    /// 最大重試次數（包含首次嘗試）。
    pub max_attempts: u32,
    /// 首次重試前的等待時間（毫秒）。
    pub initial_delay_ms: u64,
    /// 重試間隔上限（毫秒）。
    pub max_delay_ms: u64,
    /// 退避倍數（delay *= multiplier 每次重試）。
    pub multiplier: f64,
    /// 是否加入隨機 jitter（防重試風暴）。
    #[serde(default = "default_true")]
    pub jitter: bool,
    /// 可重試的錯誤碼（空 = 全部可重試）。
    #[serde(default)]
    pub retryable_errors: Vec<String>,
    /// 不可重試的錯誤碼（優先于 retryable_errors）。
    #[serde(default)]
    pub non_retryable_errors: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl RetryPolicy {
    /// 預設 MCP 呼叫重試策略。
    pub fn default_mcp_call() -> Self {
        Self {
            retry_policy_id: "default-mcp-call".into(),
            operation_type: "mcp_call".into(),
            max_attempts: 3,
            initial_delay_ms: 500,
            max_delay_ms: 10_000,
            multiplier: 2.0,
            jitter: true,
            retryable_errors: vec![
                "NETWORK_TIMEOUT".into(),
                "SERVICE_UNAVAILABLE".into(),
                "RATE_LIMITED".into(),
            ],
            non_retryable_errors: vec![
                "PERMISSION_DENIED".into(),
                "INVALID_SCHEMA".into(),
                "NOT_FOUND".into(),
            ],
        }
    }

    /// 預設 memory_write 重試策略。
    pub fn default_memory_write() -> Self {
        Self {
            retry_policy_id: "default-memory-write".into(),
            operation_type: "memory_write".into(),
            max_attempts: 5,
            initial_delay_ms: 200,
            max_delay_ms: 5_000,
            multiplier: 2.0,
            jitter: true,
            retryable_errors: vec![
                "NETWORK_TIMEOUT".into(),
                "SERVICE_UNAVAILABLE".into(),
            ],
            non_retryable_errors: vec![
                "PERMISSION_DENIED".into(),
                "INVALID_SCHEMA".into(),
            ],
        }
    }

    /// 預設 wiki_write 重試策略。
    pub fn default_wiki_write() -> Self {
        Self {
            retry_policy_id: "default-wiki-write".into(),
            operation_type: "wiki_write".into(),
            max_attempts: 3,
            initial_delay_ms: 1_000,
            max_delay_ms: 30_000,
            multiplier: 2.0,
            jitter: true,
            retryable_errors: vec![
                "NETWORK_TIMEOUT".into(),
                "SERVICE_UNAVAILABLE".into(),
                "RATE_LIMITED".into(),
            ],
            non_retryable_errors: vec![
                "PERMISSION_DENIED".into(),
                "INVALID_SCHEMA".into(),
            ],
        }
    }

    /// 計算第 `attempt`（0-based）次重試前應等待的毫秒數。
    ///
    /// 使用指數退避公式：`delay = initial_delay * multiplier^attempt`
    /// 加上可選的均勻分佈 jitter（0 到 delay 的 30%）。
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.initial_delay_ms as f64
            * self.multiplier.powi(attempt as i32);
        let capped = base.min(self.max_delay_ms as f64);

        let jitter_ms = if self.jitter {
            // INFRA-SEC-03 fix: use true random jitter to prevent synchronized retry storms.
            // When many agents fail simultaneously, deterministic jitter causes all retries
            // to land at the exact same millisecond — defeating the purpose of jitter.
            rand::thread_rng().gen_range(0.0..capped * 0.3) as u64
        } else {
            0
        };

        Duration::from_millis(capped as u64 + jitter_ms)
    }

    /// 判斷給定錯誤碼是否可重試。
    pub fn is_retryable(&self, error_code: &str) -> bool {
        // Non-retryable takes priority
        if self.non_retryable_errors.contains(&error_code.to_string()) {
            return false;
        }
        // If no retryable list, retry everything not in non_retryable
        if self.retryable_errors.is_empty() {
            return true;
        }
        self.retryable_errors.contains(&error_code.to_string())
    }

    /// 驗證策略合法性。
    pub fn validate(&self) -> Result<(), String> {
        if self.retry_policy_id.is_empty() {
            return Err("retry_policy_id cannot be empty".into());
        }
        if self.max_attempts == 0 {
            return Err("max_attempts must be >= 1".into());
        }
        if self.multiplier <= 0.0 {
            return Err("multiplier must be > 0".into());
        }
        if self.initial_delay_ms == 0 {
            return Err("initial_delay_ms must be > 0".into());
        }
        if self.max_delay_ms < self.initial_delay_ms {
            return Err("max_delay_ms must be >= initial_delay_ms".into());
        }
        Ok(())
    }
}

// ── RetryOutcome ──────────────────────────────────────────────────────────────

/// 帶重試執行後的最終結果摘要。
#[derive(Debug, Clone)]
pub struct RetryOutcome<T> {
    /// 操作是否最終成功。
    pub success: bool,
    /// 成功時的結果值。
    pub result: Option<T>,
    /// 總嘗試次數（包含首次）。
    pub attempts: u32,
    /// 最後一次錯誤（若失敗）。
    pub final_error: Option<String>,
    /// 是否已送入 DLQ（重試耗盡後）。
    pub sent_to_dlq: bool,
}

impl<T> RetryOutcome<T> {
    pub fn succeeded(result: T, attempts: u32) -> Self {
        Self {
            success: true,
            result: Some(result),
            attempts,
            final_error: None,
            sent_to_dlq: false,
        }
    }

    pub fn failed(attempts: u32, error: impl Into<String>, sent_to_dlq: bool) -> Self {
        Self {
            success: false,
            result: None,
            attempts,
            final_error: Some(error.into()),
            sent_to_dlq,
        }
    }
}

// ── RetryEngine ───────────────────────────────────────────────────────────────

/// 重試引擎 — 管理多個 operation type 的重試策略。
pub struct RetryEngine {
    /// operation_type → RetryPolicy
    policies: HashMap<String, RetryPolicy>,
}

impl RetryEngine {
    /// 建立 RetryEngine，包含預設策略集。
    pub fn new() -> Self {
        let mut policies = HashMap::new();
        for policy in Self::default_policies() {
            policies.insert(policy.operation_type.clone(), policy);
        }
        Self { policies }
    }

    /// 建立空 RetryEngine（供測試注入自訂策略）。
    pub fn empty() -> Self {
        Self {
            policies: HashMap::new(),
        }
    }

    /// 預設策略集（W19 規格 §2.4 表格）。
    fn default_policies() -> Vec<RetryPolicy> {
        vec![
            RetryPolicy::default_mcp_call(),
            RetryPolicy::default_memory_write(),
            RetryPolicy::default_wiki_write(),
            RetryPolicy {
                retry_policy_id: "default-message-send".into(),
                operation_type: "message_send".into(),
                max_attempts: 2,
                initial_delay_ms: 1_000,
                max_delay_ms: 5_000,
                multiplier: 2.0,
                jitter: true,
                retryable_errors: vec!["NETWORK_TIMEOUT".into()],
                non_retryable_errors: vec!["PERMISSION_DENIED".into()],
            },
            RetryPolicy {
                retry_policy_id: "default-external-api".into(),
                operation_type: "external_api".into(),
                max_attempts: 3,
                initial_delay_ms: 1_000,
                max_delay_ms: 60_000,
                multiplier: 2.0,
                jitter: true,
                retryable_errors: vec![
                    "NETWORK_TIMEOUT".into(),
                    "SERVICE_UNAVAILABLE".into(),
                    "RATE_LIMITED".into(),
                ],
                non_retryable_errors: vec![
                    "PERMISSION_DENIED".into(),
                    "INVALID_SCHEMA".into(),
                ],
            },
        ]
    }

    /// 新增或更新重試策略。
    pub fn upsert_policy(&mut self, policy: RetryPolicy) -> Result<(), String> {
        policy.validate()?;
        self.policies.insert(policy.operation_type.clone(), policy);
        Ok(())
    }

    /// 取得指定 operation type 的策略（若無則嘗試通用 `"*"` 策略）。
    pub fn get_policy(&self, operation_type: &str) -> Option<&RetryPolicy> {
        self.policies
            .get(operation_type)
            .or_else(|| self.policies.get("*"))
    }

    /// 帶重試地執行 async 閉包。
    ///
    /// - `operation_type`：用於選擇重試策略
    /// - `error_code_extractor`：從錯誤中提取錯誤碼（判斷是否可重試）
    /// - `f`：要執行的操作，回傳 `Result<T, (String, E)>`，其中 `String` 是錯誤碼
    ///
    /// 若策略找不到，使用 max_attempts=1（不重試）的後備行為。
    pub async fn execute<T, E, F, Fut>(
        &self,
        operation_type: &str,
        mut f: F,
    ) -> RetryOutcome<T>
    where
        F: FnMut(u32) -> Fut,  // attempt number (0-based)
        Fut: std::future::Future<Output = Result<T, (String, E)>>,
        E: std::fmt::Display,
    {
        let policy = self.get_policy(operation_type).cloned();
        let max_attempts = policy.as_ref().map_or(1, |p| p.max_attempts);

        let mut last_error = None;

        for attempt in 0..max_attempts {
            match f(attempt).await {
                Ok(value) => {
                    return RetryOutcome::succeeded(value, attempt + 1);
                }
                Err((error_code, err)) => {
                    let error_str = err.to_string();

                    // Check if this error is retryable
                    if let Some(ref policy) = policy {
                        if !policy.is_retryable(&error_code) {
                            return RetryOutcome::failed(
                                attempt + 1,
                                format!("non-retryable error [{error_code}]: {error_str}"),
                                false,
                            );
                        }
                    }

                    last_error = Some(format!("[{error_code}]: {error_str}"));

                    // If there are more attempts, wait before retrying
                    if attempt + 1 < max_attempts {
                        let delay = policy
                            .as_ref()
                            .map_or(Duration::from_millis(500), |p| p.delay_for_attempt(attempt));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
        }

        RetryOutcome::failed(
            max_attempts,
            last_error.unwrap_or_else(|| "unknown error".into()),
            true, // exhausted → sent to DLQ
        )
    }
}

impl Default for RetryEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ── RetryPolicy tests ─────────────────────────────────────────────────────

    #[test]
    fn test_policy_validate_valid() {
        let policy = RetryPolicy::default_mcp_call();
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn test_policy_validate_zero_attempts() {
        let mut p = RetryPolicy::default_mcp_call();
        p.max_attempts = 0;
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_policy_validate_empty_id() {
        let mut p = RetryPolicy::default_mcp_call();
        p.retry_policy_id = "".into();
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_policy_validate_max_delay_less_than_initial() {
        let mut p = RetryPolicy::default_mcp_call();
        p.max_delay_ms = 100;
        p.initial_delay_ms = 500;
        assert!(p.validate().is_err());
    }

    #[test]
    fn test_delay_increases_with_attempts() {
        let p = RetryPolicy {
            jitter: false, // disable jitter for deterministic test
            ..RetryPolicy::default_mcp_call()
        };
        let d0 = p.delay_for_attempt(0);
        let d1 = p.delay_for_attempt(1);
        let d2 = p.delay_for_attempt(2);
        assert!(d0 < d1, "d1 should be bigger than d0");
        assert!(d1 < d2, "d2 should be bigger than d1");
    }

    #[test]
    fn test_delay_capped_at_max() {
        let p = RetryPolicy {
            max_delay_ms: 1000,
            initial_delay_ms: 500,
            multiplier: 10.0,
            jitter: false,
            ..RetryPolicy::default_mcp_call()
        };
        // After a few doublings, delay should cap at max_delay_ms
        let d = p.delay_for_attempt(5);
        assert!(
            d.as_millis() <= 1300,
            "delay should not greatly exceed max_delay_ms (got {}ms)",
            d.as_millis()
        );
    }

    #[test]
    fn test_retryable_error_codes() {
        let p = RetryPolicy::default_mcp_call();
        assert!(p.is_retryable("NETWORK_TIMEOUT"));
        assert!(p.is_retryable("SERVICE_UNAVAILABLE"));
        assert!(!p.is_retryable("PERMISSION_DENIED")); // in non_retryable_errors
        assert!(!p.is_retryable("INVALID_SCHEMA"));
    }

    #[test]
    fn test_empty_retryable_list_retries_all_except_non_retryable() {
        let p = RetryPolicy {
            retryable_errors: vec![],
            non_retryable_errors: vec!["PERMISSION_DENIED".into()],
            ..RetryPolicy::default_mcp_call()
        };
        assert!(p.is_retryable("ANYTHING"));
        assert!(!p.is_retryable("PERMISSION_DENIED"));
    }

    // ── RetryEngine: success on first try ────────────────────────────────────

    #[tokio::test]
    async fn test_succeeds_first_attempt() {
        let engine = RetryEngine::new();

        let outcome = engine
            .execute("mcp_call", |attempt| async move {
                Ok::<_, (String, String)>(format!("success on attempt {attempt}"))
            })
            .await;

        assert!(outcome.success);
        assert_eq!(outcome.attempts, 1);
        assert_eq!(outcome.result.as_deref(), Some("success on attempt 0"));
    }

    // ── RetryEngine: retry on failure then succeed ────────────────────────────

    #[tokio::test]
    async fn test_retries_on_retryable_error_then_succeeds() {
        let engine = RetryEngine::new();
        let call_count = Arc::new(Mutex::new(0u32));

        let call_count_clone = Arc::clone(&call_count);
        let outcome = engine
            .execute("mcp_call", move |_attempt| {
                let cc = Arc::clone(&call_count_clone);
                async move {
                    let mut count = cc.lock().unwrap();
                    *count += 1;
                    if *count < 2 {
                        Err::<String, _>(("NETWORK_TIMEOUT".into(), "timeout".to_string()))
                    } else {
                        Ok("ok".to_string())
                    }
                }
            })
            .await;

        assert!(outcome.success);
        assert_eq!(outcome.attempts, 2);
        assert_eq!(*call_count.lock().unwrap(), 2);
    }

    // ── RetryEngine: non-retryable stops immediately ──────────────────────────

    #[tokio::test]
    async fn test_non_retryable_error_stops_immediately() {
        let engine = RetryEngine::new();
        let call_count = Arc::new(Mutex::new(0u32));

        let cc = Arc::clone(&call_count);
        let outcome = engine
            .execute("mcp_call", move |_| {
                let cc = Arc::clone(&cc);
                async move {
                    *cc.lock().unwrap() += 1;
                    Err::<String, _>(("PERMISSION_DENIED".into(), "access denied".to_string()))
                }
            })
            .await;

        assert!(!outcome.success);
        assert_eq!(outcome.attempts, 1, "should stop after 1 attempt (non-retryable)");
        assert_eq!(*call_count.lock().unwrap(), 1);
        assert!(!outcome.sent_to_dlq, "non-retryable should NOT go to DLQ");
    }

    // ── RetryEngine: exhausted → DLQ ─────────────────────────────────────────

    #[tokio::test]
    async fn test_exhausted_retries_sent_to_dlq() {
        let mut engine = RetryEngine::empty();
        engine
            .upsert_policy(RetryPolicy {
                retry_policy_id: "test-fast-retry".into(),
                operation_type: "test_op".into(),
                max_attempts: 3,
                initial_delay_ms: 1,    // Very short for testing
                max_delay_ms: 10,
                multiplier: 1.0,
                jitter: false,
                retryable_errors: vec!["FAIL".into()],
                non_retryable_errors: vec![],
            })
            .unwrap();

        let call_count = Arc::new(Mutex::new(0u32));
        let cc = Arc::clone(&call_count);

        let outcome = engine
            .execute("test_op", move |_| {
                let cc = Arc::clone(&cc);
                async move {
                    *cc.lock().unwrap() += 1;
                    Err::<String, _>(("FAIL".into(), "always fail".to_string()))
                }
            })
            .await;

        assert!(!outcome.success);
        assert_eq!(outcome.attempts, 3);
        assert_eq!(*call_count.lock().unwrap(), 3);
        assert!(outcome.sent_to_dlq, "exhausted retries should be sent to DLQ");
    }

    // ── RetryEngine: unknown operation type ───────────────────────────────────

    #[tokio::test]
    async fn test_unknown_operation_type_no_retry() {
        let engine = RetryEngine::new(); // no "unknown_op" policy

        let outcome = engine
            .execute("unknown_op", |_| async {
                Err::<String, _>(("FAIL".into(), "error".to_string()))
            })
            .await;

        // Falls back to max_attempts=1 (no retry)
        assert!(!outcome.success);
        assert_eq!(outcome.attempts, 1);
    }

    // ── RetryEngine: policy management ───────────────────────────────────────

    #[test]
    fn test_upsert_policy_replaces_existing() {
        let mut engine = RetryEngine::new();
        let original = engine.get_policy("mcp_call").unwrap().max_attempts;

        let new_policy = RetryPolicy {
            max_attempts: 10,
            ..RetryPolicy::default_mcp_call()
        };
        engine.upsert_policy(new_policy).unwrap();
        assert_eq!(engine.get_policy("mcp_call").unwrap().max_attempts, 10);
        assert_ne!(engine.get_policy("mcp_call").unwrap().max_attempts, original);
    }

    #[test]
    fn test_upsert_invalid_policy_returns_error() {
        let mut engine = RetryEngine::new();
        let bad_policy = RetryPolicy {
            max_attempts: 0, // invalid
            ..RetryPolicy::default_mcp_call()
        };
        assert!(engine.upsert_policy(bad_policy).is_err());
    }

    #[test]
    fn test_default_policies_are_all_valid() {
        let engine = RetryEngine::new();
        for op in ["mcp_call", "memory_write", "wiki_write", "message_send", "external_api"] {
            let policy = engine.get_policy(op).unwrap();
            assert!(
                policy.validate().is_ok(),
                "default policy for '{op}' should be valid"
            );
        }
    }
}
