//! DuDuClaw Durability Layer — W19-P1 柱二
//!
//! 為所有關鍵寫入操作提供冪等性保證、智能重試機制與狀態檢查點，
//! 確保系統在網路抖動、服務暫停、程序崩潰後能可靠恢復。
//!
//! ## 核心元件
//!
//! ```text
//! DurabilityLayer
//! ├── IdempotencyGuard     — 冪等鍵管理（dedup window）
//! ├── RetryEngine          — 指數退避重試（per-operation 策略）
//! ├── CircuitBreakerRegistry — 外部依賴熔斷保護（可配置 threshold）
//! ├── CheckpointManager    — 長任務狀態快照
//! └── DeadLetterQueue      — 失敗任務隔離與回放
//! ```
//!
//! ## 快速開始
//!
//! ```rust,ignore
//! use duduclaw_durability::prelude::*;
//!
//! // IdempotencyGuard
//! let guard = IdempotencyGuard::new(IdempotencyConfig::default());
//! let key = IdempotencyKey::new("agent-1", "wiki_write", b"content");
//!
//! // RetryEngine
//! let engine = RetryEngine::new();
//!
//! // CircuitBreaker
//! let cb = CircuitBreakerRegistry::new_with_defaults();
//!
//! // CheckpointManager
//! let mgr = CheckpointManager::new(CheckpointConfig::default());
//! ```

pub mod checkpoint;
pub mod circuit_breaker;
pub mod dlq;
pub mod idempotency;
pub mod retry;

/// 常用 re-exports。
pub mod prelude {
    pub use crate::checkpoint::{
        Checkpoint, CheckpointConfig, CheckpointError, CheckpointManager,
    };
    pub use crate::circuit_breaker::{
        BreakerState, CircuitBreakerDependencyConfig, CircuitBreakerError,
        CircuitBreakerRegistry, StateTransition,
    };
    pub use crate::dlq::{DeadLetterQueue, DlqRecord, DlqStatus};
    pub use crate::idempotency::{CheckResult, IdempotencyConfig, IdempotencyGuard, IdempotencyKey};
    pub use crate::retry::{RetryEngine, RetryError, RetryOutcome, RetryPolicy};
}
