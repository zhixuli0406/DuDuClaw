//! CheckpointManager — 長任務狀態快照
//!
//! 為長時間執行的任務提供狀態持久化，確保崩潰恢復後能從最後快照點繼續。
//!
//! ## 使用範例
//! ```rust,ignore
//! let mgr = CheckpointManager::new(CheckpointConfig::default());
//!
//! // 儲存快照
//! mgr.save("task-123", "agent-1", "phase-2", json!({"progress": 0.5}), 3600).await?;
//!
//! // 恢復快照
//! if let Some(cp) = mgr.restore("task-123", "agent-1", None).await? {
//!     println!("Resuming from phase: {}", cp.phase);
//! }
//! ```

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CheckpointError {
    #[error("checkpoint not found for task '{task_id}', agent '{agent_id}'")]
    NotFound {
        task_id: String,
        agent_id: String,
    },

    #[error("checkpoint '{checkpoint_id}' has expired")]
    Expired {
        checkpoint_id: String,
    },

    #[error("invalid TTL: must be > 0")]
    InvalidTtl,
}

// ── Config ────────────────────────────────────────────────────────────────────

/// CheckpointManager 配置（所有參數可配置）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointConfig {
    /// 預設 TTL（秒），若未指定時使用。
    #[serde(default = "default_ttl")]
    pub default_ttl_seconds: u64,
    /// 最大儲存快照數量（防止記憶體溢出）。
    #[serde(default = "default_max_checkpoints")]
    pub max_checkpoints: usize,
    /// 過期清理間隔（秒）。
    #[serde(default = "default_cleanup_interval")]
    pub cleanup_interval_seconds: u64,
}

fn default_ttl() -> u64 {
    3600 * 24 // 24h
}
fn default_max_checkpoints() -> usize {
    10_000
}
fn default_cleanup_interval() -> u64 {
    3600
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            default_ttl_seconds: default_ttl(),
            max_checkpoints: default_max_checkpoints(),
            cleanup_interval_seconds: default_cleanup_interval(),
        }
    }
}

// ── Checkpoint ────────────────────────────────────────────────────────────────

/// 單一快照記錄。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// 快照唯一 ID。
    pub checkpoint_id: String,
    /// 關聯的任務 ID。
    pub task_id: String,
    /// 執行此任務的 Agent ID。
    pub agent_id: String,
    /// 任務執行階段（例如 `"phase-2"`, `"step-5"`）。
    pub phase: String,
    /// 任務狀態（任意 JSON）。
    pub state: serde_json::Value,
    /// 快照建立時間（ISO8601）。
    pub created_at: String,
    /// 快照到期時間（ISO8601）。
    pub expires_at: String,
}

// ── Internal storage ──────────────────────────────────────────────────────────

struct CheckpointRecord {
    checkpoint: Checkpoint,
    created_instant: Instant,
    ttl: Duration,
}

impl CheckpointRecord {
    fn is_expired(&self) -> bool {
        self.created_instant.elapsed() >= self.ttl
    }
}

/// Lookup key: (task_id, agent_id, phase)
type CheckpointKey = (String, String, String);

// ── CheckpointManager ─────────────────────────────────────────────────────────

/// 任務快照管理器（in-memory 實作）。
pub struct CheckpointManager {
    config: CheckpointConfig,
    /// Key: (task_id, agent_id, phase) → latest checkpoint
    records: RwLock<HashMap<CheckpointKey, CheckpointRecord>>,
    last_cleanup: RwLock<Instant>,
}

impl CheckpointManager {
    /// 建立 CheckpointManager。
    pub fn new(config: CheckpointConfig) -> Self {
        Self {
            config,
            records: RwLock::new(HashMap::new()),
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    /// 儲存任務狀態快照。
    ///
    /// 若相同 (task_id, agent_id, phase) 已有快照，覆寫舊快照。
    pub async fn save(
        &self,
        task_id: &str,
        agent_id: &str,
        phase: &str,
        state: serde_json::Value,
        ttl_seconds: u64,
    ) -> Result<Checkpoint, CheckpointError> {
        if ttl_seconds == 0 {
            return Err(CheckpointError::InvalidTtl);
        }

        self.maybe_cleanup().await;

        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(ttl_seconds as i64);
        let checkpoint = Checkpoint {
            checkpoint_id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            phase: phase.to_string(),
            state,
            created_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
        };

        let key = (task_id.to_string(), agent_id.to_string(), phase.to_string());
        let record = CheckpointRecord {
            checkpoint: checkpoint.clone(),
            created_instant: Instant::now(),
            ttl: Duration::from_secs(ttl_seconds),
        };

        let mut records = self.records.write().await;
        records.insert(key, record);

        tracing::debug!(
            task_id,
            agent_id,
            phase,
            checkpoint_id = checkpoint.checkpoint_id.as_str(),
            "checkpoint saved"
        );

        Ok(checkpoint)
    }

    /// 恢復任務快照。
    ///
    /// - `phase`: 若提供，恢復指定階段的快照；若 `None`，恢復最近的快照（任意 phase）。
    /// - 若快照已過期，回傳 `CheckpointError::Expired`。
    /// - 若無快照，回傳 `CheckpointError::NotFound`。
    pub async fn restore(
        &self,
        task_id: &str,
        agent_id: &str,
        phase: Option<&str>,
    ) -> Result<Checkpoint, CheckpointError> {
        let records = self.records.read().await;

        if let Some(phase) = phase {
            // Look up specific phase
            let key = (task_id.to_string(), agent_id.to_string(), phase.to_string());
            match records.get(&key) {
                None => Err(CheckpointError::NotFound {
                    task_id: task_id.into(),
                    agent_id: agent_id.into(),
                }),
                Some(record) if record.is_expired() => Err(CheckpointError::Expired {
                    checkpoint_id: record.checkpoint.checkpoint_id.clone(),
                }),
                Some(record) => Ok(record.checkpoint.clone()),
            }
        } else {
            // Find the most recent non-expired checkpoint for this task+agent
            let latest = records
                .iter()
                .filter(|(k, _)| k.0 == task_id && k.1 == agent_id)
                .filter(|(_, v)| !v.is_expired())
                .max_by_key(|(_, v)| v.created_instant);

            match latest {
                None => Err(CheckpointError::NotFound {
                    task_id: task_id.into(),
                    agent_id: agent_id.into(),
                }),
                Some((_, record)) => Ok(record.checkpoint.clone()),
            }
        }
    }

    /// 刪除任務的所有快照（任務完成後清理）。
    pub async fn clear_task(&self, task_id: &str, agent_id: &str) -> usize {
        let mut records = self.records.write().await;
        let before = records.len();
        records.retain(|k, _| !(k.0 == task_id && k.1 == agent_id));
        before - records.len()
    }

    /// 查詢當前快照總數（監控用）。
    pub async fn count(&self) -> usize {
        let records = self.records.read().await;
        records.len()
    }

    async fn maybe_cleanup(&self) {
        let interval = Duration::from_secs(self.config.cleanup_interval_seconds);
        {
            let last = self.last_cleanup.read().await;
            if last.elapsed() < interval {
                return;
            }
        }
        let mut records = self.records.write().await;
        records.retain(|_, v| !v.is_expired());
        *self.last_cleanup.write().await = Instant::now();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_mgr() -> CheckpointManager {
        CheckpointManager::new(CheckpointConfig::default())
    }

    // ── save ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_returns_checkpoint_with_correct_fields() {
        let mgr = make_mgr();
        let state = serde_json::json!({"step": 3, "data": "abc"});
        let cp = mgr
            .save("task-1", "agent-1", "phase-1", state.clone(), 3600)
            .await
            .unwrap();

        assert_eq!(cp.task_id, "task-1");
        assert_eq!(cp.agent_id, "agent-1");
        assert_eq!(cp.phase, "phase-1");
        assert_eq!(cp.state, state);
        assert!(!cp.checkpoint_id.is_empty());
        chrono::DateTime::parse_from_rfc3339(&cp.created_at).unwrap();
        chrono::DateTime::parse_from_rfc3339(&cp.expires_at).unwrap();
    }

    #[tokio::test]
    async fn test_save_zero_ttl_returns_error() {
        let mgr = make_mgr();
        let result = mgr
            .save("task-1", "agent-1", "phase-1", serde_json::json!({}), 0)
            .await;
        assert!(matches!(result, Err(CheckpointError::InvalidTtl)));
    }

    #[tokio::test]
    async fn test_save_multiple_phases_independently() {
        let mgr = make_mgr();
        mgr.save("task-1", "agent-1", "phase-1", serde_json::json!("p1"), 3600)
            .await
            .unwrap();
        mgr.save("task-1", "agent-1", "phase-2", serde_json::json!("p2"), 3600)
            .await
            .unwrap();
        assert_eq!(mgr.count().await, 2);
    }

    // ── restore ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_restore_specific_phase() {
        let mgr = make_mgr();
        mgr.save("task-1", "agent-1", "phase-1", serde_json::json!("state1"), 3600)
            .await
            .unwrap();
        mgr.save("task-1", "agent-1", "phase-2", serde_json::json!("state2"), 3600)
            .await
            .unwrap();

        let cp = mgr.restore("task-1", "agent-1", Some("phase-1")).await.unwrap();
        assert_eq!(cp.phase, "phase-1");
        assert_eq!(cp.state, "state1");
    }

    #[tokio::test]
    async fn test_restore_latest_when_phase_is_none() {
        let mgr = make_mgr();
        mgr.save("task-1", "agent-1", "phase-1", serde_json::json!("state1"), 3600)
            .await
            .unwrap();
        // Small delay to ensure phase-2 has later timestamp
        tokio::time::sleep(Duration::from_millis(5)).await;
        mgr.save("task-1", "agent-1", "phase-2", serde_json::json!("state2"), 3600)
            .await
            .unwrap();

        let cp = mgr.restore("task-1", "agent-1", None).await.unwrap();
        assert_eq!(cp.phase, "phase-2");
    }

    #[tokio::test]
    async fn test_restore_not_found_returns_error() {
        let mgr = make_mgr();
        let result = mgr.restore("nonexistent", "agent-1", None).await;
        assert!(matches!(result, Err(CheckpointError::NotFound { .. })));
    }

    #[tokio::test]
    async fn test_restore_different_agent_not_found() {
        let mgr = make_mgr();
        mgr.save("task-1", "agent-1", "p1", serde_json::json!("ok"), 3600)
            .await
            .unwrap();

        let result = mgr.restore("task-1", "agent-2", None).await;
        assert!(matches!(result, Err(CheckpointError::NotFound { .. })));
    }

    // ── clear_task ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_clear_task_removes_all_phases() {
        let mgr = make_mgr();
        mgr.save("task-1", "agent-1", "p1", serde_json::json!(1), 3600).await.unwrap();
        mgr.save("task-1", "agent-1", "p2", serde_json::json!(2), 3600).await.unwrap();
        mgr.save("task-2", "agent-1", "p1", serde_json::json!(3), 3600).await.unwrap();

        let removed = mgr.clear_task("task-1", "agent-1").await;
        assert_eq!(removed, 2);
        assert_eq!(mgr.count().await, 1); // task-2 remains

        // task-1 should be gone
        assert!(mgr.restore("task-1", "agent-1", None).await.is_err());
    }

    // ── Default config ────────────────────────────────────────────────────────

    #[test]
    fn test_default_config_values() {
        let cfg = CheckpointConfig::default();
        assert_eq!(cfg.default_ttl_seconds, 86400); // 24h
        assert_eq!(cfg.max_checkpoints, 10_000);
        assert_eq!(cfg.cleanup_interval_seconds, 3600);
    }
}
