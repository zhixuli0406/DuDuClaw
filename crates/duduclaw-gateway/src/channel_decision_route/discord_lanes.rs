//! Discord handler permits, split by lane (review M3).
//!
//! The Discord gateway loop spawns one task per MESSAGE_CREATE and
//! INTERACTION_CREATE and each task waits for a permit before it runs. Until
//! F4 every task shared one pool of 10: ten long replies (a computer-use turn
//! waiting up to 60 s for its own confirmation, LLM replies running for tens
//! of seconds) left the confirmation that would release one of them queued
//! behind them until it expired. Telegram and Slack solved the same problem
//! by handling decisions on the receiver, LINE with a separate decision lane.
//!
//! Decisions now take permits from their own small pool. Since F5-C (review
//! F4-M1) a message only gets a decision permit **after** the sender passed
//! the channel access check, and a decision message never downloads its
//! attachments, so a stranger cannot fill the pool and a member's
//! decision-shaped message holds a permit only for a short, bounded step
//! (SQL reads, one broker write, one send). A flood of decision-shaped
//! messages occupies at most this pool and never the ordinary one, and
//! ordinary work can never take a decision permit.

use std::sync::{Arc, LazyLock};

use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Permits for ordinary message/interaction work (unchanged from before F4).
pub(crate) const DISCORD_GENERAL_PERMITS: usize = 10;
/// Permits reserved for decision messages and decision buttons.
pub(crate) const DISCORD_DECISION_PERMITS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiscordLane {
    Decision,
    General,
}

impl DiscordLane {
    /// A MESSAGE_CREATE is a decision when its text, with this bot's mention
    /// removed, is a strict decision command (verb + complete request UUID).
    /// `content_without_mention` is what `discord::strip_bot_mention` returns.
    pub(crate) fn for_message(content_without_mention: &str) -> Self {
        if super::is_strict_decision(content_without_mention) {
            Self::Decision
        } else {
            Self::General
        }
    }

    /// An INTERACTION_CREATE is a decision when it is a message component
    /// (type 3) whose `custom_id` decodes as a decision action — the same test
    /// `handle_component_interaction` uses to route it.
    pub(crate) fn for_interaction(d: &Value) -> Self {
        let is_component = d["type"].as_u64() == Some(3);
        let is_decision = d["data"]["custom_id"]
            .as_str()
            .and_then(crate::decision_action::parse)
            .is_some();
        if is_component && is_decision {
            Self::Decision
        } else {
            Self::General
        }
    }
}

/// The two permit pools. Production uses [`current_discord_permits`]; tests
/// install their own with [`with_test_discord_permits`] so saturating one
/// cannot stall unrelated tests in the process.
pub(crate) struct DiscordPermits {
    general: Arc<Semaphore>,
    decision: Arc<Semaphore>,
}

impl DiscordPermits {
    pub(crate) fn new(general: usize, decision: usize) -> Self {
        Self {
            general: Arc::new(Semaphore::new(general)),
            decision: Arc::new(Semaphore::new(decision)),
        }
    }

    /// Wait for a permit from the lane's own pool. `None` only if the pool was
    /// closed, which never happens for these pools; callers proceed either way,
    /// exactly as the previous `let _permit = SEMAPHORE.acquire().await` did.
    pub(crate) async fn acquire(&self, lane: DiscordLane) -> Option<OwnedSemaphorePermit> {
        match lane {
            DiscordLane::Decision => self.decision.clone().acquire_owned().await.ok(),
            DiscordLane::General => self.general.clone().acquire_owned().await.ok(),
        }
    }

    /// Permits currently free in a lane (tests and diagnostics).
    #[cfg(test)]
    pub(crate) fn available(&self, lane: DiscordLane) -> usize {
        match lane {
            DiscordLane::Decision => self.decision.available_permits(),
            DiscordLane::General => self.general.available_permits(),
        }
    }
}

/// Process-wide pools used by the Discord gateway.
static DISCORD_PERMITS: LazyLock<Arc<DiscordPermits>> = LazyLock::new(|| {
    Arc::new(DiscordPermits::new(
        DISCORD_GENERAL_PERMITS,
        DISCORD_DECISION_PERMITS,
    ))
});

#[cfg(test)]
tokio::task_local! {
    static TEST_DISCORD_PERMITS: Arc<DiscordPermits>;
}

/// The pools the current task should use: the process-wide ones, or a test's
/// own (installed with [`with_test_discord_permits`]).
pub(crate) fn current_discord_permits() -> Arc<DiscordPermits> {
    #[cfg(test)]
    if let Ok(permits) = TEST_DISCORD_PERMITS.try_with(Arc::clone) {
        return permits;
    }
    DISCORD_PERMITS.clone()
}

/// Run `future` with its own permit pools (tests only).
#[cfg(test)]
pub(crate) async fn with_test_discord_permits<F: std::future::Future>(
    permits: Arc<DiscordPermits>,
    future: F,
) -> F::Output {
    TEST_DISCORD_PERMITS.scope(permits, future).await
}
