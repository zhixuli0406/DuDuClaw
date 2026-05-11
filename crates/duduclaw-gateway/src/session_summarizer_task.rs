//! Background task that drives the async summarization policy (#13).
//!
//! The pure policy + prompt format live in
//! [`crate::session_summarizer`]; this module wires it to the real
//! session store and an LLM caller. The split keeps decision logic
//! unit-testable without a runtime, while this module handles the I/O
//! coordination (cron tick, DB read/write, Haiku call).
//!
//! ## Cadence
//!
//! The task ticks every 10 minutes. Inside each tick:
//! 1. Pull all session candidates via
//!    [`SessionManager::list_summary_candidates`].
//! 2. Run [`session_summarizer::decide_summarization`] to filter +
//!    quota-bound them. This is the policy gate — sessions below
//!    `min_new_turns_to_trigger` or in cooldown drop out.
//! 3. For each `SummarizeUpTo { turn }`:
//!    a. Fetch the first N=turn messages via
//!       [`SessionManager::read_first_n_turns_text`].
//!    b. Build the Haiku prompt via
//!       [`session_summarizer::format_summarization_prompt`].
//!    c. Call Haiku via [`crate::channel_reply::call_claude_cli_public`]
//!       (cheapest reachable path — no rotation gymnastics needed for
//!       a maintenance task).
//!    d. Persist the resulting bullet summary via
//!       [`SessionManager::set_summary`].
//!
//! ## Error handling
//!
//! Each session is wrapped in its own `try` so one Haiku hiccup can't
//! cascade. Failures emit `tracing::warn!` and the session falls back
//! to its previous summary state (which is fine — verbatim history
//! still works). Pipeline-wide errors (e.g. session store unreachable)
//! kill the tick but the next tick retries.
//!
//! ## Why not direct API
//!
//! `call_claude_cli_public` already handles AccountRotator + retries.
//! Reaching for `direct_api` would mean reimplementing all of that for
//! marginal cost gains. The 10-min cadence keeps total invocations low
//! even with many sessions (capped by `max_per_tick`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::session::SessionManager;
use crate::session_summarizer::{
    decide_summarization, format_summarization_prompt, SummarizeDecision, SummarizeParams,
};

/// How often the task wakes up.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(600); // 10 min

/// Background task handle. Spawning is fire-and-forget — the runtime
/// drops the handle.
pub fn spawn_summarizer(
    session_manager: Arc<SessionManager>,
    home_dir: PathBuf,
    params: SummarizeParams,
) -> tokio::task::JoinHandle<()> {
    spawn_summarizer_with_interval(session_manager, home_dir, params, DEFAULT_TICK_INTERVAL)
}

/// Like `spawn_summarizer` but takes a custom interval — used by tests
/// to avoid sleeping for 10 minutes.
pub fn spawn_summarizer_with_interval(
    session_manager: Arc<SessionManager>,
    home_dir: PathBuf,
    params: SummarizeParams,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(
            interval_secs = interval.as_secs(),
            min_new_turns = params.min_new_turns_to_trigger,
            cooldown_secs = params.cooldown_seconds,
            max_per_tick = params.max_per_tick,
            "session summarizer task started"
        );

        let mut ticker = tokio::time::interval(interval);
        // First tick fires immediately — skip it so we wait `interval`
        // before the first run (avoids surprise on cold start).
        ticker.tick().await;

        loop {
            ticker.tick().await;
            tick_once(&session_manager, &home_dir, &params).await;
        }
    })
}

/// One iteration of the task — pulled out as `pub(crate)` so tests
/// can invoke it directly without scheduling.
pub(crate) async fn tick_once(
    session_manager: &SessionManager,
    home_dir: &std::path::Path,
    params: &SummarizeParams,
) {
    let candidates = match session_manager.list_summary_candidates().await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "summarizer: list_summary_candidates failed");
            return;
        }
    };
    if candidates.is_empty() {
        debug!("summarizer: no sessions in store, nothing to do");
        return;
    }

    let decisions = decide_summarization(&candidates, params);
    let to_run: Vec<(String, u32)> = decisions
        .into_iter()
        .filter_map(|(session_id, d)| match d {
            SummarizeDecision::SummarizeUpTo { turn } => Some((session_id, turn)),
            SummarizeDecision::Skip { .. } => None,
        })
        .collect();

    if to_run.is_empty() {
        debug!(
            candidates = candidates.len(),
            "summarizer: no sessions met summarization threshold this tick"
        );
        return;
    }

    info!(
        scheduled = to_run.len(),
        candidates = candidates.len(),
        "summarizer: dispatching this tick"
    );

    for (session_id, through_turn) in to_run {
        match summarize_one(session_manager, home_dir, &session_id, through_turn).await {
            Ok(bytes) => info!(
                session_id = %session_id,
                through_turn,
                summary_bytes = bytes,
                "summarizer: persisted summary"
            ),
            Err(e) => warn!(
                session_id = %session_id,
                through_turn,
                error = %e,
                "summarizer: session-level failure (continuing with next)"
            ),
        }
    }
}

/// Summarize one session's first `through_turn` turns and persist the
/// result. Returns the byte length of the persisted summary on success.
async fn summarize_one(
    session_manager: &SessionManager,
    home_dir: &std::path::Path,
    session_id: &str,
    through_turn: u32,
) -> Result<usize, String> {
    let transcript = session_manager
        .read_first_n_turns_text(session_id, through_turn)
        .await
        .map_err(|e| format!("read_first_n_turns_text: {e}"))?;
    if transcript.trim().is_empty() {
        return Err("transcript is empty — nothing to summarize".to_string());
    }

    let prompt = format_summarization_prompt(&transcript);

    // Haiku via the public CLI helper. Empty system prompt is fine —
    // the summarization prompt is fully self-contained.
    let summary = crate::channel_reply::call_claude_cli_public(
        &prompt,
        "claude-haiku-4-5",
        "",
        home_dir,
    )
    .await
    .map_err(|e| format!("call_claude_cli_public: {e}"))?;

    let trimmed = summary.trim();
    if trimmed.is_empty() {
        return Err("summarizer returned empty response".to_string());
    }

    let bytes = trimmed.len();
    session_manager
        .set_summary(session_id, trimmed, through_turn)
        .await
        .map_err(|e| format!("set_summary: {e}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Build a SessionManager backed by a temp DB so tests don't
    /// pollute the real `~/.duduclaw/sessions.db`.
    fn make_session_manager() -> (Arc<SessionManager>, tempfile::TempDir) {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("sessions.db");
        let sm = Arc::new(SessionManager::new(&db_path).unwrap());
        (sm, tmp)
    }

    /// tick_once on a fresh store with no sessions is a no-op — no
    /// panic, no DB writes, returns quickly.
    #[tokio::test]
    async fn tick_once_handles_empty_store() {
        let (sm, _tmp) = make_session_manager();
        let params = SummarizeParams::default();
        tick_once(&sm, std::path::Path::new("/nonexistent"), &params).await;
    }

    /// tick_once is a no-op when no session has enough new turns to
    /// cross the threshold. Verifies the policy gate, not the LLM
    /// call (which is intentionally skipped here — we don't want to
    /// shell out from a unit test).
    #[tokio::test]
    async fn tick_once_skips_short_sessions() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("short-session", "test-agent")
            .await
            .unwrap();
        // Append 3 turns — below the default 10 threshold.
        for i in 0..3 {
            sm.append_message("short-session", "user", &format!("turn {i}"), 5)
                .await
                .unwrap();
        }

        let params = SummarizeParams::default();
        tick_once(&sm, std::path::Path::new("/nonexistent"), &params).await;

        // Summary must still be empty — no Haiku call was triggered.
        let (summary, through) = sm.get_summary("short-session").await.unwrap();
        assert!(summary.is_empty());
        assert_eq!(through, 0);
    }

    /// Confirm that `list_summary_candidates` reports the expected shape:
    /// sessions with their turn count, prior summarized turn, and
    /// seconds-since-last-summary (None when never summarized).
    #[tokio::test]
    async fn list_summary_candidates_reflects_store() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s1", "agent-a").await.unwrap();
        for _ in 0..15 {
            sm.append_message("s1", "user", "hi", 2).await.unwrap();
        }
        let c = sm.list_summary_candidates().await.unwrap();
        let row = c.iter().find(|c| c.session_id == "s1").unwrap();
        assert_eq!(row.turn_count, 15);
        assert_eq!(row.summarized_through_turn, 0);
        assert!(row.seconds_since_last_summary.is_none());
    }

    /// After `set_summary`, the candidate row should reflect the
    /// summarized turn count and a recent `seconds_since_last_summary`.
    #[tokio::test]
    async fn set_summary_updates_candidate_row() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s2", "agent-a").await.unwrap();
        for _ in 0..20 {
            sm.append_message("s2", "user", "hi", 2).await.unwrap();
        }
        sm.set_summary("s2", "- bullet one\n- bullet two", 15)
            .await
            .unwrap();

        let (summary, through) = sm.get_summary("s2").await.unwrap();
        assert!(summary.contains("bullet one"));
        assert_eq!(through, 15);

        let c = sm.list_summary_candidates().await.unwrap();
        let row = c.iter().find(|c| c.session_id == "s2").unwrap();
        assert_eq!(row.summarized_through_turn, 15);
        // A non-zero, small "seconds since" — we just wrote it.
        let secs = row
            .seconds_since_last_summary
            .expect("must have last_summarized_at after set_summary");
        assert!(secs < 60, "expected recent summary, got {secs}s");
    }

    /// `read_first_n_turns_text` returns turns in insertion order with
    /// "role: content" lines. Used by the summarizer to build the
    /// transcript fed to Haiku.
    #[tokio::test]
    async fn read_first_n_returns_role_prefixed_lines() {
        let (sm, _tmp) = make_session_manager();
        sm.get_or_create("s3", "agent-a").await.unwrap();
        sm.append_message("s3", "user", "hello", 1).await.unwrap();
        sm.append_message("s3", "assistant", "hi there", 2)
            .await
            .unwrap();
        sm.append_message("s3", "user", "another", 1).await.unwrap();

        let text = sm.read_first_n_turns_text("s3", 2).await.unwrap();
        assert!(text.contains("user: hello"));
        assert!(text.contains("assistant: hi there"));
        // Third turn must NOT be included (we asked for first 2).
        assert!(!text.contains("another"));
    }
}
