//! IdempotencyGuard — 冪等鍵管理
//!
//! 防止重複操作在 dedup 視窗內被執行多次。
//!
//! ## 冪等鍵格式
//! ```text
//! {agent_id}:{operation_type}:{content_hash}
//! ```
//!
//! ## 使用範例
//! ```rust,ignore
//! let guard = IdempotencyGuard::new(IdempotencyConfig::default());
//! let key = IdempotencyKey::new("agent-1", "wiki_write", b"content");
//!
//! match guard.check_and_record(&key, json!({"result": "ok"})).await {
//!     CheckResult::New => { /* 執行操作 */ }
//!     CheckResult::Duplicate { original_result, .. } => { /* 回傳原始結果 */ }
//! }
//! ```

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

// ── Config ────────────────────────────────────────────────────────────────────

/// IdempotencyGuard 配置（從 TOML 讀取，所有參數可配置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyConfig {
    /// 是否啟用冪等性檢查（預設 true）。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 重複操作偵測視窗（秒）。
    #[serde(default = "default_dedup_window")]
    pub dedup_window_seconds: u64,
    /// 最大 key 長度（字元）。
    #[serde(default = "default_max_key_length")]
    pub max_key_length: usize,
    /// 清理過期記錄的間隔（秒）。
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_seconds: u64,
}

fn default_true() -> bool {
    true
}
fn default_dedup_window() -> u64 {
    3600
}
fn default_max_key_length() -> usize {
    256
}
fn default_cleanup_interval() -> u64 {
    7200
}

impl Default for IdempotencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dedup_window_seconds: 3600,
            max_key_length: 256,
            cleanup_interval_seconds: 7200,
        }
    }
}

// ── IdempotencyKey ────────────────────────────────────────────────────────────

/// 冪等鍵，格式：`{agent_id}:{operation_type}:{content_hash}`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// 從 agent_id、operation_type、內容 bytes 建立冪等鍵。
    ///
    /// content_hash 使用 SHA-256 的前 32 hex 字元（128-bit 碰撞空間）。
    /// 依生日悖論，碰撞機率達 50% 需約 2^64（≈ 1.8×10¹⁹）次操作，防碰撞能力顯著優於 16 字元版本。
    pub fn new(agent_id: &str, operation_type: &str, content: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let hash = hex::encode(hasher.finalize());
        let short_hash = &hash[..32];
        Self(format!("{agent_id}:{operation_type}:{short_hash}"))
    }

    /// 從已知字串直接建立（用於測試或反序列化）。
    pub fn from_raw(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// 取得 key 字串。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 取得 key 長度（字元）。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否為空字串。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── CheckResult ───────────────────────────────────────────────────────────────

/// 冪等性檢查結果。
#[derive(Debug, Clone)]
pub enum CheckResult {
    /// 新操作，尚無記錄 — 呼叫者應繼續執行並回報結果。
    New,
    /// 重複操作，dedup 視窗內已存在相同 key。
    Duplicate {
        /// 原始操作的回傳結果。
        original_result: serde_json::Value,
        /// 原始操作執行時間（ISO8601）。
        original_timestamp: String,
        /// dedup 視窗到期時間（ISO8601）。
        dedup_window_expires_at: String,
    },
}

// ── Internal record ───────────────────────────────────────────────────────────

struct IdempotencyRecord {
    result: serde_json::Value,
    recorded_at: Instant,
    timestamp_iso: String,
}

// ── IdempotencyGuard ──────────────────────────────────────────────────────────

/// 冪等鍵管理器（in-memory 實作）。
///
/// 生產環境可替換為 Redis / SQLite backend，介面保持不變。
pub struct IdempotencyGuard {
    config: IdempotencyConfig,
    records: RwLock<HashMap<String, IdempotencyRecord>>,
    last_cleanup: RwLock<Instant>,
}

impl IdempotencyGuard {
    /// 建立 IdempotencyGuard。
    pub fn new(config: IdempotencyConfig) -> Self {
        Self {
            config,
            records: RwLock::new(HashMap::new()),
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    /// 檢查 key 是否在 dedup 視窗內已存在。
    ///
    /// - 若是新操作：回傳 `CheckResult::New`，**不**自動記錄結果（需呼叫 `record`）。
    /// - 若是重複：回傳 `CheckResult::Duplicate` 含原始結果。
    pub async fn check(&self, key: &IdempotencyKey) -> CheckResult {
        if !self.config.enabled {
            return CheckResult::New;
        }

        if key.len() > self.config.max_key_length {
            tracing::warn!(
                key = key.as_str(),
                max_len = self.config.max_key_length,
                "idempotency key exceeds max_key_length, treating as New"
            );
            return CheckResult::New;
        }

        self.maybe_cleanup().await;

        let records = self.records.read().await;
        let window = Duration::from_secs(self.config.dedup_window_seconds);

        if let Some(record) = records.get(key.as_str()) {
            if record.recorded_at.elapsed() < window {
                let expires_at = chrono::Utc::now()
                    + chrono::Duration::from_std(
                        window.saturating_sub(record.recorded_at.elapsed()),
                    )
                    .unwrap_or_default();
                return CheckResult::Duplicate {
                    original_result: record.result.clone(),
                    original_timestamp: record.timestamp_iso.clone(),
                    dedup_window_expires_at: expires_at.to_rfc3339(),
                };
            }
        }

        CheckResult::New
    }

    /// 記錄操作結果，供後續重複請求回傳。
    ///
    /// 若 key 已存在（但未過期），覆寫其結果。
    pub async fn record(&self, key: &IdempotencyKey, result: serde_json::Value) {
        if !self.config.enabled {
            return;
        }
        let timestamp_iso = chrono::Utc::now().to_rfc3339();
        let mut records = self.records.write().await;
        records.insert(
            key.as_str().to_string(),
            IdempotencyRecord {
                result,
                recorded_at: Instant::now(),
                timestamp_iso,
            },
        );
    }

    /// 查詢目前記錄數（用於監控）。
    pub async fn record_count(&self) -> usize {
        let records = self.records.read().await;
        records.len()
    }

    /// 清理過期記錄（lazy cleanup，在 check/record 時自動觸發）。
    async fn maybe_cleanup(&self) {
        let cleanup_interval = Duration::from_secs(self.config.cleanup_interval_seconds);
        {
            let last_cleanup = self.last_cleanup.read().await;
            if last_cleanup.elapsed() < cleanup_interval {
                return;
            }
        }

        let window = Duration::from_secs(self.config.dedup_window_seconds);
        let mut records = self.records.write().await;
        records.retain(|_, v| v.recorded_at.elapsed() < window);

        let mut last_cleanup = self.last_cleanup.write().await;
        *last_cleanup = Instant::now();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_guard(dedup_window_seconds: u64) -> IdempotencyGuard {
        IdempotencyGuard::new(IdempotencyConfig {
            dedup_window_seconds,
            ..Default::default()
        })
    }

    // ── IdempotencyKey ────────────────────────────────────────────────────────

    #[test]
    fn test_key_format_contains_agent_op_hash() {
        let key = IdempotencyKey::new("agent-1", "wiki_write", b"hello");
        assert!(key.as_str().starts_with("agent-1:wiki_write:"));
        assert_eq!(key.as_str().split(':').count(), 3);
    }

    #[test]
    fn test_same_content_produces_same_key() {
        let k1 = IdempotencyKey::new("a", "op", b"content");
        let k2 = IdempotencyKey::new("a", "op", b"content");
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_different_content_produces_different_key() {
        let k1 = IdempotencyKey::new("a", "op", b"content1");
        let k2 = IdempotencyKey::new("a", "op", b"content2");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_different_agent_produces_different_key() {
        let k1 = IdempotencyKey::new("agent-1", "op", b"content");
        let k2 = IdempotencyKey::new("agent-2", "op", b"content");
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_from_raw_roundtrip() {
        let key = IdempotencyKey::from_raw("agent:op:abc123");
        assert_eq!(key.as_str(), "agent:op:abc123");
        assert_eq!(key.to_string(), "agent:op:abc123");
    }

    #[test]
    fn test_key_len() {
        let key = IdempotencyKey::new("a", "op", b"x");
        // "a:op:<32-hex-chars>" = 1+1+2+1+32 = 37
        assert_eq!(key.len(), 37);
    }

    #[test]
    fn test_key_hash_segment_is_32_chars() {
        // SEC-H2 驗收：content_hash 段落必須 ≥ 32 hex 字元（128-bit 碰撞空間）
        let key = IdempotencyKey::new("agent-1", "wiki_write", b"some content");
        let parts: Vec<&str> = key.as_str().splitn(3, ':').collect();
        assert_eq!(parts.len(), 3, "key must have 3 colon-separated segments");
        let hash_segment = parts[2];
        assert!(
            hash_segment.len() >= 32,
            "hash segment must be ≥ 32 hex chars (128-bit), got {}",
            hash_segment.len()
        );
        // 確認 hash segment 僅含合法 hex 字元
        assert!(
            hash_segment.chars().all(|c| c.is_ascii_hexdigit()),
            "hash segment must contain only hex digits, got: {hash_segment}"
        );
    }

    // ── IdempotencyGuard: New ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_new_key_returns_new() {
        let guard = make_guard(3600);
        let key = IdempotencyKey::new("agent-1", "op", b"data");
        let result = guard.check(&key).await;
        assert!(matches!(result, CheckResult::New));
    }

    #[tokio::test]
    async fn test_after_record_returns_duplicate() {
        let guard = make_guard(3600);
        let key = IdempotencyKey::new("agent-1", "op", b"data");

        guard.record(&key, serde_json::json!({"status": "ok"})).await;

        let result = guard.check(&key).await;
        assert!(matches!(result, CheckResult::Duplicate { .. }));
    }

    #[tokio::test]
    async fn test_duplicate_returns_original_result() {
        let guard = make_guard(3600);
        let key = IdempotencyKey::new("agent-1", "op", b"data");
        let expected = serde_json::json!({"result": "original"});

        guard.record(&key, expected.clone()).await;

        if let CheckResult::Duplicate {
            original_result, ..
        } = guard.check(&key).await
        {
            assert_eq!(original_result, expected);
        } else {
            panic!("expected Duplicate");
        }
    }

    #[tokio::test]
    async fn test_different_keys_are_independent() {
        let guard = make_guard(3600);
        let key1 = IdempotencyKey::new("agent-1", "op", b"data1");
        let key2 = IdempotencyKey::new("agent-1", "op", b"data2");

        guard.record(&key1, serde_json::json!("result1")).await;

        // key1 is duplicate, key2 is new
        assert!(matches!(guard.check(&key1).await, CheckResult::Duplicate { .. }));
        assert!(matches!(guard.check(&key2).await, CheckResult::New));
    }

    #[tokio::test]
    async fn test_different_agents_are_independent() {
        let guard = make_guard(3600);
        let key_a = IdempotencyKey::new("agent-a", "wiki_write", b"content");
        let key_b = IdempotencyKey::new("agent-b", "wiki_write", b"content");

        guard.record(&key_a, serde_json::json!("ok")).await;

        assert!(matches!(guard.check(&key_a).await, CheckResult::Duplicate { .. }));
        assert!(matches!(guard.check(&key_b).await, CheckResult::New));
    }

    #[tokio::test]
    async fn test_disabled_guard_always_returns_new() {
        let guard = IdempotencyGuard::new(IdempotencyConfig {
            enabled: false,
            ..Default::default()
        });
        let key = IdempotencyKey::new("agent-1", "op", b"data");
        guard.record(&key, serde_json::json!("ok")).await;

        // Even after recording, disabled guard returns New
        let result = guard.check(&key).await;
        assert!(matches!(result, CheckResult::New));
    }

    #[tokio::test]
    async fn test_key_exceeding_max_length_returns_new() {
        let guard = IdempotencyGuard::new(IdempotencyConfig {
            max_key_length: 10, // Very short limit
            ..Default::default()
        });
        let key = IdempotencyKey::from_raw("this_key_is_definitely_longer_than_ten_chars");
        let result = guard.check(&key).await;
        assert!(matches!(result, CheckResult::New));
    }

    #[tokio::test]
    async fn test_record_count_increases() {
        let guard = make_guard(3600);
        assert_eq!(guard.record_count().await, 0);

        let key1 = IdempotencyKey::new("a", "op1", b"x");
        let key2 = IdempotencyKey::new("a", "op2", b"y");
        guard.record(&key1, serde_json::json!(null)).await;
        guard.record(&key2, serde_json::json!(null)).await;
        assert_eq!(guard.record_count().await, 2);
    }

    #[tokio::test]
    async fn test_overwrite_same_key() {
        let guard = make_guard(3600);
        let key = IdempotencyKey::new("a", "op", b"data");

        guard.record(&key, serde_json::json!("first")).await;
        guard.record(&key, serde_json::json!("second")).await;

        if let CheckResult::Duplicate { original_result, .. } = guard.check(&key).await {
            assert_eq!(original_result, "second");
        } else {
            panic!("expected Duplicate");
        }
    }

    #[tokio::test]
    async fn test_duplicate_result_fields_are_populated() {
        let guard = make_guard(3600);
        let key = IdempotencyKey::new("agent-x", "memory_write", b"payload");
        guard.record(&key, serde_json::json!({"written": true})).await;

        if let CheckResult::Duplicate {
            original_timestamp,
            dedup_window_expires_at,
            ..
        } = guard.check(&key).await
        {
            // Both timestamps should be parseable ISO8601
            chrono::DateTime::parse_from_rfc3339(&original_timestamp)
                .expect("original_timestamp must be valid RFC3339");
            chrono::DateTime::parse_from_rfc3339(&dedup_window_expires_at)
                .expect("dedup_window_expires_at must be valid RFC3339");
        } else {
            panic!("expected Duplicate");
        }
    }

    // ── Config defaults ───────────────────────────────────────────────────────

    #[test]
    fn test_default_config_values() {
        let cfg = IdempotencyConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.dedup_window_seconds, 3600);
        assert_eq!(cfg.max_key_length, 256);
        assert_eq!(cfg.cleanup_interval_seconds, 7200);
    }
}
