//! QuotaManager — 資源配額管理器
//!
//! ## 功能
//! - 每日 token 消耗追蹤（`daily_token_budget`）
//! - 最大並發任務數控制（`max_concurrent_tasks`）
//! - 最大記憶體條目數限制（`max_memory_entries`）
//! - Cron 重置機制（`reset_cron: "0 0 * * *"`）
//! - 重置時自動發射 `governance_quota_reset` 事件
//!
//! ## 設計
//! `QuotaManager` 是 `PolicyEvaluator` 之外獨立存在的配額追蹤器，
//! 持有每 Agent 的使用量狀態，並透過 `AuditEventSink` 發射重置事件。

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::info;

use crate::{
    audit::{AuditEventSink, GovernanceQuotaResetEvent},
    policy::QuotaPolicy,
};

// ── Errors ────────────────────────────────────────────────────────────────────

/// 配額違規錯誤。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum QuotaError {
    #[error("Daily token budget exhausted: used {used}, budget {budget}")]
    TokenBudgetExhausted { used: u64, budget: u64 },

    #[error("Max concurrent tasks exceeded: current {current}, max {max}")]
    ConcurrentTasksExceeded { current: u32, max: u32 },

    #[error("Max memory entries exceeded: current {current}, max {max}")]
    MemoryEntriesExceeded { current: u64, max: u64 },
}

// ── QuotaUsageSnapshot ────────────────────────────────────────────────────────

/// 配額使用量快照（供外部查詢）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaUsageSnapshot {
    pub agent_id: String,
    pub token_used: u64,
    pub concurrent_tasks: u32,
    pub memory_entries: u64,
    pub reset_at: DateTime<Utc>,
}

// ── QuotaUsage (internal) ─────────────────────────────────────────────────────

/// 配額使用量（內部狀態）。
#[derive(Debug)]
struct QuotaUsage {
    token_used: u64,
    concurrent_tasks: u32,
    memory_entries: u64,
    reset_at: DateTime<Utc>,
}

impl QuotaUsage {
    fn new(reset_at: DateTime<Utc>) -> Self {
        Self {
            token_used: 0,
            concurrent_tasks: 0,
            memory_entries: 0,
            reset_at,
        }
    }

    fn needs_reset(&self) -> bool {
        Utc::now() >= self.reset_at
    }

    fn reset(&mut self, next_reset_at: DateTime<Utc>) {
        self.token_used = 0;
        self.concurrent_tasks = 0;
        self.memory_entries = 0;
        self.reset_at = next_reset_at;
    }
}

// ── QuotaManager ──────────────────────────────────────────────────────────────

/// 資源配額管理器。
///
/// 獨立追蹤每 Agent 的配額使用量，供 PolicyEvaluator 在評估時查詢。
/// 每日重置時間為 UTC 00:00（cron: `0 0 * * *`）。
pub struct QuotaManager {
    /// agent_id → QuotaUsage
    usage: RwLock<HashMap<String, QuotaUsage>>,
    audit_sink: Arc<dyn AuditEventSink>,
}

impl QuotaManager {
    /// 建立 QuotaManager。
    pub fn new(audit_sink: Arc<dyn AuditEventSink>) -> Self {
        Self {
            usage: RwLock::new(HashMap::new()),
            audit_sink,
        }
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// 消耗 token，若超過每日預算則回傳 `QuotaError::TokenBudgetExhausted`。
    pub async fn consume_tokens(
        &self,
        agent_id: &str,
        policy: &QuotaPolicy,
        tokens: u64,
    ) -> Result<(), QuotaError> {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));

        let was_reset = if entry.needs_reset() {
            entry.reset(next_reset);
            true
        } else {
            false
        };

        let new_total = entry.token_used.saturating_add(tokens);
        if new_total > policy.daily_token_budget {
            let err = QuotaError::TokenBudgetExhausted {
                used: entry.token_used,
                budget: policy.daily_token_budget,
            };
            drop(map);
            if was_reset {
                self.fire_quota_reset_event(agent_id, "daily");
            }
            return Err(err);
        }
        entry.token_used = new_total;
        drop(map);

        if was_reset {
            self.fire_quota_reset_event(agent_id, "daily");
        }
        Ok(())
    }

    /// 檢查每日 token 預算（不消耗，僅查詢）。
    pub async fn check_token_budget(
        &self,
        agent_id: &str,
        policy: &QuotaPolicy,
    ) -> Result<(), QuotaError> {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));

        let was_reset = if entry.needs_reset() {
            entry.reset(next_reset);
            true
        } else {
            false
        };

        let exhausted = entry.token_used >= policy.daily_token_budget;
        let used = entry.token_used;
        drop(map);

        if was_reset {
            self.fire_quota_reset_event(agent_id, "daily");
        }

        if exhausted {
            return Err(QuotaError::TokenBudgetExhausted {
                used,
                budget: policy.daily_token_budget,
            });
        }
        Ok(())
    }

    /// 增加並發任務計數；若超過最大值則回傳 `QuotaError::ConcurrentTasksExceeded`。
    pub async fn increment_concurrent_tasks(
        &self,
        agent_id: &str,
        policy: &QuotaPolicy,
    ) -> Result<(), QuotaError> {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));

        let was_reset = if entry.needs_reset() {
            entry.reset(next_reset);
            true
        } else {
            false
        };

        if entry.concurrent_tasks >= policy.max_concurrent_tasks {
            let err = QuotaError::ConcurrentTasksExceeded {
                current: entry.concurrent_tasks,
                max: policy.max_concurrent_tasks,
            };
            drop(map);
            if was_reset {
                self.fire_quota_reset_event(agent_id, "daily");
            }
            return Err(err);
        }

        entry.concurrent_tasks = entry.concurrent_tasks.saturating_add(1);
        drop(map);

        if was_reset {
            self.fire_quota_reset_event(agent_id, "daily");
        }
        Ok(())
    }

    /// 減少並發任務計數（完成任務時呼叫）。
    pub async fn decrement_concurrent_tasks(&self, agent_id: &str) {
        let mut map = self.usage.write().await;
        if let Some(entry) = map.get_mut(agent_id) {
            entry.concurrent_tasks = entry.concurrent_tasks.saturating_sub(1);
        }
    }

    /// 設定記憶體條目數；若超過最大值則回傳 `QuotaError::MemoryEntriesExceeded`。
    pub async fn set_memory_entries(
        &self,
        agent_id: &str,
        policy: &QuotaPolicy,
        count: u64,
    ) -> Result<(), QuotaError> {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));

        let was_reset = if entry.needs_reset() {
            entry.reset(next_reset);
            true
        } else {
            false
        };

        if count > policy.max_memory_entries {
            let err = QuotaError::MemoryEntriesExceeded {
                current: count,
                max: policy.max_memory_entries,
            };
            drop(map);
            if was_reset {
                self.fire_quota_reset_event(agent_id, "daily");
            }
            return Err(err);
        }
        entry.memory_entries = count;
        drop(map);

        if was_reset {
            self.fire_quota_reset_event(agent_id, "daily");
        }
        Ok(())
    }

    /// 手動重置特定 Agent 的所有配額使用量，並發射 `governance_quota_reset` 事件。
    pub async fn reset_agent(&self, agent_id: &str) {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        if let Some(entry) = map.get_mut(agent_id) {
            entry.reset(next_reset);
        }
        drop(map);
        self.fire_quota_reset_event(agent_id, "manual");
        info!(agent_id = agent_id, "QuotaManager: manual reset for agent");
    }

    /// 重置所有 Agent 的配額使用量（每日 cron 重置時呼叫）。
    ///
    /// 對應 `reset_cron: "0 0 * * *"`。
    pub async fn reset_all(&self) {
        let next_reset = Self::next_daily_reset();
        let count = {
            let mut map = self.usage.write().await;
            let count = map.len();
            for entry in map.values_mut() {
                entry.reset(next_reset);
            }
            count
        };
        self.fire_quota_reset_event("*", "daily");
        info!(agents = count, "QuotaManager: daily reset for all agents");
    }

    /// 取得 Agent 的配額使用量快照（`None` 表示尚無使用記錄）。
    pub async fn get_usage(&self, agent_id: &str) -> Option<QuotaUsageSnapshot> {
        let map = self.usage.read().await;
        map.get(agent_id).map(|entry| QuotaUsageSnapshot {
            agent_id: agent_id.to_string(),
            token_used: entry.token_used,
            concurrent_tasks: entry.concurrent_tasks,
            memory_entries: entry.memory_entries,
            reset_at: entry.reset_at,
        })
    }

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// 強制設定重置時間（僅供測試使用）。
    ///
    /// 可用於模擬「已過重置時間」的情境。
    #[cfg(test)]
    pub async fn set_reset_at_for_test(&self, agent_id: &str, reset_at: DateTime<Utc>) {
        let mut map = self.usage.write().await;
        if let Some(entry) = map.get_mut(agent_id) {
            entry.reset_at = reset_at;
        }
    }

    /// 強制設定 token 使用量（僅供測試使用）。
    #[cfg(test)]
    pub async fn set_token_used_for_test(&self, agent_id: &str, tokens: u64) {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));
        entry.token_used = tokens;
    }

    /// 強制設定並發任務數（僅供測試使用）。
    #[cfg(test)]
    pub async fn set_concurrent_tasks_for_test(&self, agent_id: &str, tasks: u32) {
        let next_reset = Self::next_daily_reset();
        let mut map = self.usage.write().await;
        let entry = map
            .entry(agent_id.to_string())
            .or_insert_with(|| QuotaUsage::new(next_reset));
        entry.concurrent_tasks = tasks;
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// 計算下一次每日重置時間（明天 UTC 00:00）。
    fn next_daily_reset() -> DateTime<Utc> {
        let now = Utc::now();
        let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
        let reset_naive = tomorrow
            .and_hms_opt(0, 0, 0)
            .expect("valid midnight time");
        DateTime::<Utc>::from_naive_utc_and_offset(reset_naive, Utc)
    }

    /// fire-and-forget 發射 governance_quota_reset 事件。
    fn fire_quota_reset_event(&self, agent_id: &str, reset_type: &str) {
        let event = GovernanceQuotaResetEvent::new(agent_id, reset_type);
        let sink = Arc::clone(&self.audit_sink);
        tokio::spawn(async move {
            sink.emit_quota_reset(event).await;
        });
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audit::RecordingAuditSink, policy::QuotaPolicy};
    use std::sync::Arc;

    fn make_policy(daily_tokens: u64, max_tasks: u32, max_memory: u64) -> QuotaPolicy {
        QuotaPolicy {
            policy_id: "test-quota".into(),
            agent_id: "*".into(),
            daily_token_budget: daily_tokens,
            max_concurrent_tasks: max_tasks,
            max_memory_entries: max_memory,
            reset_cron: "0 0 * * *".into(),
        }
    }

    fn make_manager() -> (QuotaManager, Arc<RecordingAuditSink>) {
        let sink = Arc::new(RecordingAuditSink::default());
        let manager = QuotaManager::new(Arc::clone(&sink) as Arc<dyn AuditEventSink>);
        (manager, sink)
    }

    // ── Token budget tests ────────────────────────────────────────────────────

    /// 消耗 token 正常路徑：未超過預算則成功。
    #[tokio::test]
    async fn test_consume_tokens_within_budget() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        let result = mgr.consume_tokens("agent-1", &policy, 500).await;
        assert!(result.is_ok());

        let snap = mgr.get_usage("agent-1").await.unwrap();
        assert_eq!(snap.token_used, 500);
    }

    /// 消耗 token 超過預算回傳 TokenBudgetExhausted。
    #[tokio::test]
    async fn test_consume_tokens_exceeds_budget() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(100, 5, 10000);

        // 先消耗 80
        mgr.consume_tokens("agent-budget", &policy, 80)
            .await
            .unwrap();

        // 再消耗 30（總計 110，超過 100）
        let result = mgr.consume_tokens("agent-budget", &policy, 30).await;
        assert!(matches!(
            result,
            Err(QuotaError::TokenBudgetExhausted { used: 80, budget: 100 })
        ));
    }

    /// 消耗全額預算後，下次消耗應失敗。
    #[tokio::test]
    async fn test_consume_tokens_exactly_at_budget() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(100, 5, 10000);

        mgr.consume_tokens("exact-agent", &policy, 100)
            .await
            .unwrap();

        // 0 tokens at budget boundary — check returns error
        let check = mgr.check_token_budget("exact-agent", &policy).await;
        assert!(check.is_err(), "should fail when budget is exhausted");
    }

    /// 累計消耗：多次小批次消耗應正確累計。
    #[tokio::test]
    async fn test_consume_tokens_accumulates_correctly() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        for _ in 0..5 {
            mgr.consume_tokens("accum-agent", &policy, 100)
                .await
                .unwrap();
        }
        let snap = mgr.get_usage("accum-agent").await.unwrap();
        assert_eq!(snap.token_used, 500);
    }

    /// 不同 Agent 的配額互相獨立。
    #[tokio::test]
    async fn test_token_budget_isolated_per_agent() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(100, 5, 10000);

        // agent-a 消耗所有預算
        mgr.consume_tokens("agent-a", &policy, 100).await.unwrap();

        // agent-b 不受影響
        let result = mgr.consume_tokens("agent-b", &policy, 50).await;
        assert!(result.is_ok(), "agent-b should have independent quota");
    }

    // ── Concurrent tasks tests ────────────────────────────────────────────────

    /// 並發任務計數：正常增減。
    #[tokio::test]
    async fn test_concurrent_tasks_increment_decrement() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 3, 10000);

        mgr.increment_concurrent_tasks("task-agent", &policy)
            .await
            .unwrap();
        mgr.increment_concurrent_tasks("task-agent", &policy)
            .await
            .unwrap();

        let snap = mgr.get_usage("task-agent").await.unwrap();
        assert_eq!(snap.concurrent_tasks, 2);

        mgr.decrement_concurrent_tasks("task-agent").await;
        let snap = mgr.get_usage("task-agent").await.unwrap();
        assert_eq!(snap.concurrent_tasks, 1);
    }

    /// 超過最大並發任務數時回傳錯誤。
    #[tokio::test]
    async fn test_concurrent_tasks_exceeded() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 2, 10000);

        mgr.increment_concurrent_tasks("conc-agent", &policy)
            .await
            .unwrap();
        mgr.increment_concurrent_tasks("conc-agent", &policy)
            .await
            .unwrap();

        // 3rd task should fail
        let result = mgr
            .increment_concurrent_tasks("conc-agent", &policy)
            .await;
        assert!(matches!(
            result,
            Err(QuotaError::ConcurrentTasksExceeded { current: 2, max: 2 })
        ));
    }

    /// decrement 不會讓計數低於 0（saturating_sub）。
    #[tokio::test]
    async fn test_decrement_does_not_go_below_zero() {
        let (mgr, _sink) = make_manager();

        // 不曾 increment，直接 decrement
        mgr.decrement_concurrent_tasks("zero-agent").await;
        // get_usage returns None since never created
        assert!(mgr.get_usage("zero-agent").await.is_none());
    }

    // ── Memory entries tests ──────────────────────────────────────────────────

    /// 設定記憶體條目數正常路徑。
    #[tokio::test]
    async fn test_set_memory_entries_within_limit() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        mgr.set_memory_entries("mem-agent", &policy, 5000)
            .await
            .unwrap();

        let snap = mgr.get_usage("mem-agent").await.unwrap();
        assert_eq!(snap.memory_entries, 5000);
    }

    /// 超過最大記憶體條目數時回傳錯誤。
    #[tokio::test]
    async fn test_set_memory_entries_exceeded() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 5, 1000);

        let result = mgr.set_memory_entries("mem-agent", &policy, 1001).await;
        assert!(matches!(
            result,
            Err(QuotaError::MemoryEntriesExceeded {
                current: 1001,
                max: 1000
            })
        ));
    }

    // ── Reset tests ───────────────────────────────────────────────────────────

    /// 手動重置特定 Agent 後，使用量歸零，並發射 governance_quota_reset 事件。
    #[tokio::test]
    async fn test_manual_reset_agent_clears_usage_and_emits_event() {
        let (mgr, sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        mgr.consume_tokens("reset-agent", &policy, 500)
            .await
            .unwrap();
        mgr.increment_concurrent_tasks("reset-agent", &policy)
            .await
            .unwrap();

        // Reset
        mgr.reset_agent("reset-agent").await;

        let snap = mgr.get_usage("reset-agent").await.unwrap();
        assert_eq!(snap.token_used, 0, "tokens should be reset");
        assert_eq!(snap.concurrent_tasks, 0, "tasks should be reset");

        // Wait for async event
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1, "one quota reset event should be emitted");
        assert_eq!(resets[0].agent_id, "reset-agent");
        assert_eq!(resets[0].reset_type, "manual");
        assert_eq!(resets[0].outcome, "success");
    }

    /// reset_all 重置所有 Agent 並發射 governance_quota_reset 事件（agent_id="*"）。
    #[tokio::test]
    async fn test_reset_all_clears_all_agents_and_emits_event() {
        let (mgr, sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        // 兩個 Agent 各有使用量
        mgr.consume_tokens("agent-x", &policy, 300).await.unwrap();
        mgr.consume_tokens("agent-y", &policy, 400).await.unwrap();

        // Daily reset
        mgr.reset_all().await;

        let snap_x = mgr.get_usage("agent-x").await.unwrap();
        let snap_y = mgr.get_usage("agent-y").await.unwrap();
        assert_eq!(snap_x.token_used, 0);
        assert_eq!(snap_y.token_used, 0);

        // Wait for async event
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1, "one bulk reset event");
        assert_eq!(resets[0].agent_id, "*", "bulk reset uses '*' as agent_id");
        assert_eq!(resets[0].reset_type, "daily");
    }

    /// 每日重置：當 reset_at 到期時，下次操作自動重置並發射事件。
    #[tokio::test]
    async fn test_daily_auto_reset_on_expired_reset_at() {
        let (mgr, sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        // 先建立使用量
        mgr.consume_tokens("auto-reset-agent", &policy, 500)
            .await
            .unwrap();

        // 強制將 reset_at 設為過去（模擬已到期）
        let past = Utc::now() - chrono::Duration::seconds(1);
        mgr.set_reset_at_for_test("auto-reset-agent", past).await;

        // 下次操作時應自動重置
        mgr.consume_tokens("auto-reset-agent", &policy, 100)
            .await
            .unwrap();

        let snap = mgr.get_usage("auto-reset-agent").await.unwrap();
        assert_eq!(snap.token_used, 100, "token should be 100 after auto-reset");

        // Wait for async event
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1, "auto-reset event should be emitted");
        assert_eq!(resets[0].agent_id, "auto-reset-agent");
        assert_eq!(resets[0].reset_type, "daily");
    }

    /// cron 重置後，Agent 可重新使用配額。
    #[tokio::test]
    async fn test_after_reset_quota_is_available_again() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(100, 2, 1000);

        // 耗盡預算
        mgr.set_token_used_for_test("refill-agent", 100).await;
        let check = mgr.check_token_budget("refill-agent", &policy).await;
        assert!(check.is_err(), "should be exhausted before reset");

        // 手動重置
        mgr.reset_agent("refill-agent").await;

        // 重置後可再次使用
        let check = mgr.check_token_budget("refill-agent", &policy).await;
        assert!(check.is_ok(), "should be available after reset");
    }

    // ── Cron reset mechanism tests ────────────────────────────────────────────

    /// next_daily_reset 應回傳明天 UTC 00:00 之後的時間。
    #[test]
    fn test_next_daily_reset_is_tomorrow_midnight() {
        let reset = QuotaManager::next_daily_reset();
        let now = Utc::now();
        // reset 應在未來
        assert!(reset > now, "next_daily_reset should be in the future");
        // reset 應在 48 小時內
        let diff = reset.signed_duration_since(now);
        assert!(
            diff.num_hours() <= 48,
            "next reset should be within 48 hours, got {}h",
            diff.num_hours()
        );
    }

    /// reset_cron 預設值驗證。
    #[test]
    fn test_quota_policy_default_reset_cron() {
        let policy = QuotaPolicy {
            policy_id: "test".into(),
            agent_id: "*".into(),
            daily_token_budget: 1000,
            max_concurrent_tasks: 5,
            max_memory_entries: 1000,
            reset_cron: "0 0 * * *".into(),
        };
        assert_eq!(policy.reset_cron, "0 0 * * *");
    }

    // ── Audit event field validation ──────────────────────────────────────────

    /// governance_quota_reset 事件欄位驗證。
    #[tokio::test]
    async fn test_quota_reset_event_has_required_fields() {
        let (mgr, sink) = make_manager();
        mgr.reset_all().await;

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let resets = sink.quota_resets.lock().await;
        assert_eq!(resets.len(), 1);
        assert!(!resets[0].event_id.is_empty(), "event_id should be set");
        assert!(!resets[0].timestamp.is_empty(), "timestamp should be set");
        assert_eq!(resets[0].outcome, "success");
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    /// 取得不存在 Agent 的使用量應回傳 None。
    #[tokio::test]
    async fn test_get_usage_nonexistent_agent_returns_none() {
        let (mgr, _sink) = make_manager();
        let snap = mgr.get_usage("does-not-exist").await;
        assert!(snap.is_none());
    }

    /// 多次 set_memory_entries 應更新最新值。
    #[tokio::test]
    async fn test_set_memory_entries_updates_value() {
        let (mgr, _sink) = make_manager();
        let policy = make_policy(1000, 5, 10000);

        mgr.set_memory_entries("mem2-agent", &policy, 100)
            .await
            .unwrap();
        mgr.set_memory_entries("mem2-agent", &policy, 200)
            .await
            .unwrap();

        let snap = mgr.get_usage("mem2-agent").await.unwrap();
        assert_eq!(snap.memory_entries, 200, "latest value should be kept");
    }
}
