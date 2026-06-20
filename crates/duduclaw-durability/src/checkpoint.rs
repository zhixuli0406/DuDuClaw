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

    #[error("checkpoint id '{checkpoint_id}' not found")]
    IdNotFound {
        checkpoint_id: String,
    },

    #[error("checkpoint persistence error: {0}")]
    Persist(String),
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
    /// RFC-26 §4.2: lineage — the checkpoint this one was forked/rewound from.
    /// `None` for an original snapshot. Enables "explore alternative approach
    /// from checkpoint X" branching of conversation state.
    #[serde(default)]
    pub parent_checkpoint_id: Option<String>,
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
    /// RFC-26 §4.2: checkpoint_id → full checkpoint, retained so `fork`/`rewind`
    /// can address any prior snapshot by id (the `records` map only keeps the
    /// latest per key). Bounded by `max_checkpoints`.
    archive: RwLock<HashMap<String, Checkpoint>>,
    /// RFC-26 §4.2: optional durable backend. When `Some`, every checkpoint is
    /// mirrored to SQLite so it survives a restart; `None` is pure in-memory.
    persist: Option<std::sync::Mutex<rusqlite::Connection>>,
    last_cleanup: RwLock<Instant>,
}

impl CheckpointManager {
    /// 建立 CheckpointManager（純記憶體）。
    pub fn new(config: CheckpointConfig) -> Self {
        Self {
            config,
            records: RwLock::new(HashMap::new()),
            archive: RwLock::new(HashMap::new()),
            persist: None,
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    /// Build a manager backed by a durable SQLite file. Existing checkpoints are
    /// loaded into memory at open so `fork`/`rewind`/`restore` work across restarts
    /// (RFC-26 §4.2).
    pub fn with_persistence(
        config: CheckpointConfig,
        db_path: impl AsRef<std::path::Path>,
    ) -> Result<Self, CheckpointError> {
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| CheckpointError::Persist(e.to_string()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS checkpoints (
                 checkpoint_id TEXT PRIMARY KEY,
                 task_id TEXT NOT NULL,
                 agent_id TEXT NOT NULL,
                 phase TEXT NOT NULL,
                 state TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 expires_at TEXT NOT NULL,
                 parent_checkpoint_id TEXT
             );",
        )
        .map_err(|e| CheckpointError::Persist(e.to_string()))?;

        // Load existing rows into the archive + latest-per-key records.
        let mut archive: HashMap<String, Checkpoint> = HashMap::new();
        let mut records: HashMap<CheckpointKey, CheckpointRecord> = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT checkpoint_id, task_id, agent_id, phase, state, created_at, expires_at, parent_checkpoint_id FROM checkpoints")
                .map_err(|e| CheckpointError::Persist(e.to_string()))?;
            let rows = stmt
                .query_map([], |r| {
                    let state_str: String = r.get(4)?;
                    Ok(Checkpoint {
                        checkpoint_id: r.get(0)?,
                        task_id: r.get(1)?,
                        agent_id: r.get(2)?,
                        phase: r.get(3)?,
                        state: serde_json::from_str(&state_str).unwrap_or(serde_json::Value::Null),
                        created_at: r.get(5)?,
                        expires_at: r.get(6)?,
                        parent_checkpoint_id: r.get(7)?,
                    })
                })
                .map_err(|e| CheckpointError::Persist(e.to_string()))?;
            for cp in rows.flatten() {
                let key = (cp.task_id.clone(), cp.agent_id.clone(), cp.phase.clone());
                records.insert(
                    key,
                    CheckpointRecord {
                        checkpoint: cp.clone(),
                        created_instant: Instant::now(),
                        ttl: Duration::from_secs(config.default_ttl_seconds),
                    },
                );
                archive.insert(cp.checkpoint_id.clone(), cp);
            }
        }

        Ok(Self {
            config,
            records: RwLock::new(records),
            archive: RwLock::new(archive),
            persist: Some(std::sync::Mutex::new(conn)),
            last_cleanup: RwLock::new(Instant::now()),
        })
    }

    /// Mirror one checkpoint to the durable backend (no-op when in-memory).
    fn persist_checkpoint(&self, cp: &Checkpoint) {
        let Some(conn) = self.persist.as_ref() else { return };
        let conn = conn.lock().expect("checkpoint persist conn poisoned");
        let state_str = serde_json::to_string(&cp.state).unwrap_or_else(|_| "null".to_string());
        let res = conn.execute(
            "INSERT OR REPLACE INTO checkpoints
               (checkpoint_id, task_id, agent_id, phase, state, created_at, expires_at, parent_checkpoint_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                cp.checkpoint_id, cp.task_id, cp.agent_id, cp.phase, state_str,
                cp.created_at, cp.expires_at, cp.parent_checkpoint_id,
            ],
        );
        if let Err(e) = res {
            tracing::warn!("checkpoint persist failed: {e}");
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
            parent_checkpoint_id: None,
        };

        let key = (task_id.to_string(), agent_id.to_string(), phase.to_string());
        let record = CheckpointRecord {
            checkpoint: checkpoint.clone(),
            created_instant: Instant::now(),
            ttl: Duration::from_secs(ttl_seconds),
        };

        let mut records = self.records.write().await;
        records.insert(key, record);

        // Retain in the id-addressable archive (bounded below alongside records).
        {
            let mut archive = self.archive.write().await;
            archive.insert(checkpoint.checkpoint_id.clone(), checkpoint.clone());
            if self.config.max_checkpoints > 0 {
                // Bound the archive at 2× the live cap (lineage history needs a
                // little extra headroom than the latest-per-key live set).
                let cap = self.config.max_checkpoints.saturating_mul(2);
                while archive.len() > cap {
                    let oldest = archive
                        .iter()
                        .min_by(|a, b| a.1.created_at.cmp(&b.1.created_at))
                        .map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => {
                            archive.remove(&k);
                        }
                        None => break,
                    }
                }
            }
        }

        // M50 fix: enforce `max_checkpoints` to bound memory. After inserting, if the
        // store exceeds the cap, evict the oldest records (smallest `created_instant`)
        // until back within the limit. Insert-then-evict keeps the just-saved checkpoint.
        if self.config.max_checkpoints > 0 {
            while records.len() > self.config.max_checkpoints {
                let oldest_key = records
                    .iter()
                    .min_by_key(|(_, v)| v.created_instant)
                    .map(|(k, _)| k.clone());
                match oldest_key {
                    Some(k) => {
                        records.remove(&k);
                    }
                    None => break,
                }
            }
        }

        self.persist_checkpoint(&checkpoint);

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

    /// Fetch any checkpoint by its id (from the lineage archive).
    pub async fn get_by_id(&self, checkpoint_id: &str) -> Option<Checkpoint> {
        self.archive.read().await.get(checkpoint_id).cloned()
    }

    /// RFC-26 §4.2: **fork** a checkpoint — copy its state under a new task
    /// lineage so an alternative approach can be explored without disturbing the
    /// original. The new checkpoint records `parent_checkpoint_id` and becomes the
    /// current snapshot for `(new_task_id, agent, phase)`. Returns the new id.
    pub async fn fork(
        &self,
        checkpoint_id: &str,
        new_task_id: &str,
    ) -> Result<Checkpoint, CheckpointError> {
        let source = self
            .get_by_id(checkpoint_id)
            .await
            .ok_or_else(|| CheckpointError::IdNotFound { checkpoint_id: checkpoint_id.into() })?;

        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(self.config.default_ttl_seconds as i64);
        let forked = Checkpoint {
            checkpoint_id: Uuid::new_v4().to_string(),
            task_id: new_task_id.to_string(),
            agent_id: source.agent_id.clone(),
            phase: source.phase.clone(),
            state: source.state.clone(),
            created_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
            parent_checkpoint_id: Some(source.checkpoint_id.clone()),
        };

        let key = (forked.task_id.clone(), forked.agent_id.clone(), forked.phase.clone());
        let record = CheckpointRecord {
            checkpoint: forked.clone(),
            created_instant: Instant::now(),
            ttl: Duration::from_secs(self.config.default_ttl_seconds),
        };
        self.records.write().await.insert(key, record);
        self.archive.write().await.insert(forked.checkpoint_id.clone(), forked.clone());
        self.persist_checkpoint(&forked);
        Ok(forked)
    }

    /// RFC-26 §4.2: **rewind** — restore an earlier snapshot (by id) as the
    /// current checkpoint for its `(task_id, agent, phase)`, discarding any later
    /// state for that key. `task_id` guards against rewinding a checkpoint that
    /// belongs to a different task. The restored checkpoint records the snapshot it
    /// was rewound from as its parent (lineage).
    pub async fn rewind(
        &self,
        task_id: &str,
        checkpoint_id: &str,
    ) -> Result<Checkpoint, CheckpointError> {
        let source = self
            .get_by_id(checkpoint_id)
            .await
            .ok_or_else(|| CheckpointError::IdNotFound { checkpoint_id: checkpoint_id.into() })?;
        if source.task_id != task_id {
            return Err(CheckpointError::IdNotFound { checkpoint_id: checkpoint_id.into() });
        }

        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(self.config.default_ttl_seconds as i64);
        let restored = Checkpoint {
            checkpoint_id: Uuid::new_v4().to_string(),
            task_id: source.task_id.clone(),
            agent_id: source.agent_id.clone(),
            phase: source.phase.clone(),
            state: source.state.clone(),
            created_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
            parent_checkpoint_id: Some(source.checkpoint_id.clone()),
        };

        let key = (restored.task_id.clone(), restored.agent_id.clone(), restored.phase.clone());
        let record = CheckpointRecord {
            checkpoint: restored.clone(),
            created_instant: Instant::now(),
            ttl: Duration::from_secs(self.config.default_ttl_seconds),
        };
        self.records.write().await.insert(key, record);
        self.archive.write().await.insert(restored.checkpoint_id.clone(), restored.clone());
        self.persist_checkpoint(&restored);
        Ok(restored)
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

    // ── max_checkpoints enforcement ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_save_enforces_max_checkpoints_cap() {
        // M50 regression: save() must bound memory by evicting oldest beyond the cap.
        let mgr = CheckpointManager::new(CheckpointConfig {
            max_checkpoints: 3,
            ..Default::default()
        });

        // Save 5 distinct checkpoints (distinct phases → distinct keys).
        for i in 0..5 {
            mgr.save(
                "task-1",
                "agent-1",
                &format!("phase-{i}"),
                serde_json::json!(i),
                3600,
            )
            .await
            .unwrap();
            // Ensure distinct created_instant ordering for deterministic eviction.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        // Count must be capped at max_checkpoints.
        assert_eq!(mgr.count().await, 3, "store must be capped at max_checkpoints");

        // Oldest (phase-0, phase-1) should be evicted; newest (phase-4) retained.
        assert!(
            mgr.restore("task-1", "agent-1", Some("phase-0")).await.is_err(),
            "oldest checkpoint should have been evicted"
        );
        assert!(
            mgr.restore("task-1", "agent-1", Some("phase-4")).await.is_ok(),
            "newest checkpoint should be retained"
        );
    }

    // ── fork / rewind / lineage (RFC-26 §4.2) ──────────────────────────────────

    #[tokio::test]
    async fn test_get_by_id_after_save() {
        let mgr = make_mgr();
        let cp = mgr
            .save("t1", "a1", "p1", serde_json::json!({"v": 1}), 3600)
            .await
            .unwrap();
        let fetched = mgr.get_by_id(&cp.checkpoint_id).await.unwrap();
        assert_eq!(fetched.checkpoint_id, cp.checkpoint_id);
        assert_eq!(fetched.state, serde_json::json!({"v": 1}));
    }

    #[tokio::test]
    async fn test_fork_copies_state_under_new_lineage() {
        let mgr = make_mgr();
        let src = mgr
            .save("t1", "a1", "p1", serde_json::json!({"answer": 42}), 3600)
            .await
            .unwrap();

        let forked = mgr.fork(&src.checkpoint_id, "t1-branch-a").await.unwrap();
        assert_eq!(forked.task_id, "t1-branch-a");
        assert_eq!(forked.agent_id, "a1");
        assert_eq!(forked.phase, "p1");
        assert_eq!(forked.state, serde_json::json!({"answer": 42}));
        assert_eq!(forked.parent_checkpoint_id.as_deref(), Some(src.checkpoint_id.as_str()));
        assert_ne!(forked.checkpoint_id, src.checkpoint_id);

        // The fork is now the current checkpoint for the new task; the original is untouched.
        let restored = mgr.restore("t1-branch-a", "a1", Some("p1")).await.unwrap();
        assert_eq!(restored.checkpoint_id, forked.checkpoint_id);
        let original = mgr.restore("t1", "a1", Some("p1")).await.unwrap();
        assert_eq!(original.checkpoint_id, src.checkpoint_id);
    }

    #[tokio::test]
    async fn test_fork_unknown_id_errors() {
        let mgr = make_mgr();
        let r = mgr.fork("does-not-exist", "t2").await;
        assert!(matches!(r, Err(CheckpointError::IdNotFound { .. })));
    }

    #[tokio::test]
    async fn test_rewind_restores_earlier_snapshot() {
        let mgr = make_mgr();
        let first = mgr
            .save("t1", "a1", "p1", serde_json::json!("v1"), 3600)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        // Overwrite the latest for the same key.
        mgr.save("t1", "a1", "p1", serde_json::json!("v2"), 3600)
            .await
            .unwrap();
        assert_eq!(mgr.restore("t1", "a1", Some("p1")).await.unwrap().state, "v2");

        // Rewind to the first snapshot by id.
        let rewound = mgr.rewind("t1", &first.checkpoint_id).await.unwrap();
        assert_eq!(rewound.state, "v1");
        assert_eq!(rewound.parent_checkpoint_id.as_deref(), Some(first.checkpoint_id.as_str()));
        // Current state for the key is now back to v1.
        assert_eq!(mgr.restore("t1", "a1", Some("p1")).await.unwrap().state, "v1");
    }

    #[tokio::test]
    async fn test_rewind_wrong_task_rejected() {
        let mgr = make_mgr();
        let cp = mgr.save("t1", "a1", "p1", serde_json::json!(1), 3600).await.unwrap();
        // Rewinding under a different task id must fail (lineage guard).
        let r = mgr.rewind("t2", &cp.checkpoint_id).await;
        assert!(matches!(r, Err(CheckpointError::IdNotFound { .. })));
    }

    // ── durable SQLite backend (RFC-26 §4.2) ───────────────────────────────────

    #[tokio::test]
    async fn test_persistence_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoints.db");

        let saved_id;
        {
            let mgr = CheckpointManager::with_persistence(CheckpointConfig::default(), &path).unwrap();
            let cp = mgr
                .save("t1", "a1", "p1", serde_json::json!({"progress": 0.7}), 3600)
                .await
                .unwrap();
            saved_id = cp.checkpoint_id.clone();
        } // manager dropped — simulates a restart

        // Reopen: the checkpoint is loaded back from SQLite.
        let mgr2 = CheckpointManager::with_persistence(CheckpointConfig::default(), &path).unwrap();
        let restored = mgr2.restore("t1", "a1", Some("p1")).await.unwrap();
        assert_eq!(restored.state, serde_json::json!({"progress": 0.7}));
        // And it's addressable by id (fork/rewind work across restart).
        assert!(mgr2.get_by_id(&saved_id).await.is_some());
    }

    #[tokio::test]
    async fn test_persistence_fork_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoints.db");
        let forked_id;
        {
            let mgr = CheckpointManager::with_persistence(CheckpointConfig::default(), &path).unwrap();
            let src = mgr.save("t1", "a1", "p1", serde_json::json!(1), 3600).await.unwrap();
            let forked = mgr.fork(&src.checkpoint_id, "t1-branch").await.unwrap();
            forked_id = forked.checkpoint_id.clone();
        }
        let mgr2 = CheckpointManager::with_persistence(CheckpointConfig::default(), &path).unwrap();
        let cp = mgr2.get_by_id(&forked_id).await.unwrap();
        assert_eq!(cp.task_id, "t1-branch");
        assert!(cp.parent_checkpoint_id.is_some());
    }

    #[tokio::test]
    async fn test_in_memory_manager_has_no_persistence() {
        // new() stays pure in-memory; no file created.
        let mgr = make_mgr();
        let cp = mgr.save("t", "a", "p", serde_json::json!(1), 3600).await.unwrap();
        assert!(mgr.get_by_id(&cp.checkpoint_id).await.is_some());
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
