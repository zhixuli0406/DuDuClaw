//! DeadLetterQueue (DLQ) — 失敗任務隔離與回放
//!
//! 重試耗盡後的操作自動進入 DLQ，支援手動回放與狀態追蹤。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

// ── DLQ Status ────────────────────────────────────────────────────────────────

/// DLQ 記錄的狀態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlqStatus {
    /// 等待回放。
    Pending,
    /// 已成功回放。
    Replayed,
    /// 已放棄（不再回放）。
    Abandoned,
}

impl std::fmt::Display for DlqStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Replayed => write!(f, "replayed"),
            Self::Abandoned => write!(f, "abandoned"),
        }
    }
}

// ── DlqRecord ─────────────────────────────────────────────────────────────────

/// DLQ 中的一筆記錄。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqRecord {
    /// DLQ 記錄唯一 ID。
    pub dlq_id: String,
    /// 原始操作的詳情（JSON）。
    pub original_operation: serde_json::Value,
    /// 執行此操作的 Agent ID。
    pub agent_id: String,
    /// 操作類型。
    pub operation_type: String,
    /// 重試次數。
    pub retry_count: u32,
    /// 最後一次錯誤訊息。
    pub last_error: String,
    /// 失敗時間（ISO8601）。
    pub failed_at: String,
    /// 到期時間（ISO8601）。
    pub expires_at: String,
    /// 目前狀態。
    pub status: DlqStatus,
}

// ── Internal storage ──────────────────────────────────────────────────────────

struct DlqEntry {
    record: DlqRecord,
    created_instant: Instant,
    ttl: Duration,
}

impl DlqEntry {
    fn is_expired(&self) -> bool {
        self.created_instant.elapsed() >= self.ttl
    }
}

// ── DeadLetterQueue ───────────────────────────────────────────────────────────

/// Dead Letter Queue — 管理重試耗盡的操作記錄。
pub struct DeadLetterQueue {
    /// dlq_id → DlqEntry
    entries: RwLock<HashMap<String, DlqEntry>>,
    /// 預設 TTL（秒）。
    default_ttl_seconds: u64,
}

impl DeadLetterQueue {
    /// 建立 DLQ，指定預設 TTL。
    pub fn new(default_ttl_seconds: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            default_ttl_seconds,
        }
    }

    /// 新增失敗操作到 DLQ。
    pub async fn enqueue(
        &self,
        agent_id: &str,
        operation_type: &str,
        original_operation: serde_json::Value,
        retry_count: u32,
        last_error: impl Into<String>,
        ttl_seconds: Option<u64>,
    ) -> String {
        let dlq_id = Uuid::new_v4().to_string();
        let ttl = ttl_seconds.unwrap_or(self.default_ttl_seconds);
        let now = chrono::Utc::now();
        let expires = now + chrono::Duration::seconds(ttl as i64);

        let record = DlqRecord {
            dlq_id: dlq_id.clone(),
            original_operation,
            agent_id: agent_id.to_string(),
            operation_type: operation_type.to_string(),
            retry_count,
            last_error: last_error.into(),
            failed_at: now.to_rfc3339(),
            expires_at: expires.to_rfc3339(),
            status: DlqStatus::Pending,
        };

        let entry = DlqEntry {
            record,
            created_instant: Instant::now(),
            ttl: Duration::from_secs(ttl),
        };

        self.entries.write().await.insert(dlq_id.clone(), entry);
        dlq_id
    }

    /// 取得 DLQ 中的所有 pending 記錄（不含過期）。
    pub async fn list_pending(&self) -> Vec<DlqRecord> {
        let entries = self.entries.read().await;
        entries
            .values()
            .filter(|e| !e.is_expired() && e.record.status == DlqStatus::Pending)
            .map(|e| e.record.clone())
            .collect()
    }

    /// 取得所有記錄（含不同狀態），用於監控。
    pub async fn list_all(&self) -> (Vec<DlqRecord>, usize) {
        let entries = self.entries.read().await;
        let all: Vec<DlqRecord> = entries
            .values()
            .filter(|e| !e.is_expired())
            .map(|e| e.record.clone())
            .collect();
        let pending_count = all.iter().filter(|r| r.status == DlqStatus::Pending).count();
        (all, pending_count)
    }

    /// 標記 DLQ 記錄為已回放（成功）。
    pub async fn mark_replayed(&self, dlq_id: &str) -> bool {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(dlq_id) {
            entry.record.status = DlqStatus::Replayed;
            return true;
        }
        false
    }

    /// 標記 DLQ 記錄為已放棄。
    pub async fn abandon(&self, dlq_id: &str) -> bool {
        let mut entries = self.entries.write().await;
        if let Some(entry) = entries.get_mut(dlq_id) {
            entry.record.status = DlqStatus::Abandoned;
            return true;
        }
        false
    }

    /// 取得指定 dlq_id 的記錄。
    pub async fn get(&self, dlq_id: &str) -> Option<DlqRecord> {
        let entries = self.entries.read().await;
        entries
            .get(dlq_id)
            .filter(|e| !e.is_expired())
            .map(|e| e.record.clone())
    }

    /// 清理過期記錄。
    pub async fn cleanup_expired(&self) -> usize {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|_, v| !v.is_expired());
        before - entries.len()
    }

    /// 目前 pending 記錄數（監控用）。
    pub async fn pending_count(&self) -> usize {
        let entries = self.entries.read().await;
        entries
            .values()
            .filter(|e| !e.is_expired() && e.record.status == DlqStatus::Pending)
            .count()
    }
}

impl Default for DeadLetterQueue {
    fn default() -> Self {
        Self::new(86400) // 24h default TTL
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dlq() -> DeadLetterQueue {
        DeadLetterQueue::new(3600)
    }

    // ── enqueue ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_enqueue_returns_non_empty_id() {
        let dlq = make_dlq();
        let id = dlq
            .enqueue(
                "agent-1",
                "mcp_call",
                serde_json::json!({"op": "test"}),
                3,
                "timeout",
                None,
            )
            .await;
        assert!(!id.is_empty());
    }

    #[tokio::test]
    async fn test_enqueued_record_is_pending() {
        let dlq = make_dlq();
        let id = dlq
            .enqueue("agent-1", "wiki_write", serde_json::json!({}), 3, "error", None)
            .await;

        let record = dlq.get(&id).await.unwrap();
        assert_eq!(record.status, DlqStatus::Pending);
        assert_eq!(record.agent_id, "agent-1");
        assert_eq!(record.operation_type, "wiki_write");
        assert_eq!(record.retry_count, 3);
        assert_eq!(record.last_error, "error");
    }

    #[tokio::test]
    async fn test_enqueue_increments_pending_count() {
        let dlq = make_dlq();
        assert_eq!(dlq.pending_count().await, 0);

        dlq.enqueue("a", "op1", serde_json::json!({}), 3, "err", None).await;
        dlq.enqueue("a", "op2", serde_json::json!({}), 3, "err", None).await;
        assert_eq!(dlq.pending_count().await, 2);
    }

    // ── list_pending ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_pending_returns_only_pending_records() {
        let dlq = make_dlq();
        let id1 = dlq.enqueue("a", "op1", serde_json::json!({}), 1, "e1", None).await;
        let id2 = dlq.enqueue("a", "op2", serde_json::json!({}), 1, "e2", None).await;
        let id3 = dlq.enqueue("a", "op3", serde_json::json!({}), 1, "e3", None).await;

        dlq.mark_replayed(&id1).await;
        dlq.abandon(&id3).await;

        let pending = dlq.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].dlq_id, id2);
    }

    // ── mark_replayed ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_mark_replayed_changes_status() {
        let dlq = make_dlq();
        let id = dlq.enqueue("a", "op", serde_json::json!({}), 3, "err", None).await;

        let success = dlq.mark_replayed(&id).await;
        assert!(success);

        let record = dlq.get(&id).await.unwrap();
        assert_eq!(record.status, DlqStatus::Replayed);
    }

    #[tokio::test]
    async fn test_mark_replayed_unknown_id_returns_false() {
        let dlq = make_dlq();
        assert!(!dlq.mark_replayed("nonexistent").await);
    }

    // ── abandon ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_abandon_changes_status() {
        let dlq = make_dlq();
        let id = dlq.enqueue("a", "op", serde_json::json!({}), 3, "err", None).await;

        dlq.abandon(&id).await;
        let record = dlq.get(&id).await.unwrap();
        assert_eq!(record.status, DlqStatus::Abandoned);
    }

    // ── list_all ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_list_all_returns_total_and_pending_count() {
        let dlq = make_dlq();
        let id1 = dlq.enqueue("a", "op1", serde_json::json!({}), 1, "e", None).await;
        dlq.enqueue("a", "op2", serde_json::json!({}), 1, "e", None).await;
        dlq.mark_replayed(&id1).await;

        let (all, pending_count) = dlq.list_all().await;
        assert_eq!(all.len(), 2);
        assert_eq!(pending_count, 1);
    }

    // ── get ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_unknown_id_returns_none() {
        let dlq = make_dlq();
        assert!(dlq.get("nonexistent").await.is_none());
    }

    // ── DlqStatus display ────────────────────────────────────────────────────

    #[test]
    fn test_status_display() {
        assert_eq!(DlqStatus::Pending.to_string(), "pending");
        assert_eq!(DlqStatus::Replayed.to_string(), "replayed");
        assert_eq!(DlqStatus::Abandoned.to_string(), "abandoned");
    }
}
