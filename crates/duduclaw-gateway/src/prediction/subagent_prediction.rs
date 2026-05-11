//! Sub-agent prediction hook (BUG-5).
//!
//! Before this module, the [`PredictionEngine`] only ran for *channel-facing*
//! agents — the ones that receive user messages directly through
//! `channel_reply::build_reply_with_session`.  Sub-agents dispatched via the
//! bus queue (in `dispatcher.rs`) bypassed it entirely.  In a 19-agent
//! deployment this meant 18 of the 19 agents had **zero** rows in
//! `prediction.db.user_models` and zero entries in `prediction_log`, so the
//! prediction-driven evolution loop was effectively scoped to the single
//! root agent.
//!
//! This module gives the dispatcher a small, fire-and-forget hook that:
//!
//! 1. Synthesises a 2-message session from the dispatched payload + the
//!    sub-agent's response (we have no real session for sub-agents).
//! 2. Runs the same `predict → calculate_error → update_model` cycle as the
//!    channel path so the user-model statistics accumulate per sub-agent.
//! 3. Logs a `prediction_error` row in `evolution_events` so silence
//!    detection / metacognition see the activity.
//!
//! ## GVU triggering (P1, 2026-05-09)
//!
//! When `record_subagent_prediction` is called with a `gvu_loop` AND the
//! synthetic prediction error lands in Significant / Critical AND the agent
//! has `gvu_enabled = true` in `agent.toml`, the same evolution loop that
//! channel agents use is invoked here through [`crate::gvu::trigger`].
//!
//! Before this hook, 16 of 17 production agents accumulated `prediction_error`
//! rows but never produced a single `gvu_experiment_log` entry. The gating
//! rules below match channel_reply (Significant/Critical only) so cost
//! profile stays the same.
//!
//! ## User-id convention
//!
//! Sub-agent invocations have no human user — the "caller" is another agent.
//! We synthesise `user_id = "agent:<sender_or_origin>"` so the per-user
//! statistics in `user_models` cluster by inviting agent without colliding
//! with real channel `user_id`s (which are numeric for Discord/Telegram).
//! Empty senders fall back to `agent:_bus`.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{debug, warn};

use crate::gvu::loop_::GvuLoop;
use crate::gvu::mistake_notebook::MistakeNotebook;
use crate::gvu::trigger::{maybe_run_gvu, TriggerSource};
use crate::prediction::engine::PredictionEngine;
use crate::prediction::metrics::ConversationMetrics;
use crate::session::SessionMessage;

/// Build the synthetic user_id used for sub-agent prediction.
///
/// Order of preference: `sender` → `origin` → fixed fallback.  Sender is the
/// agent that *directly* triggered this dispatch (e.g. team leader → engineer);
/// origin is the very first agent in the chain.  Sender is preferred so the
/// per-pair statistics reflect the actual collaboration link.
pub fn synthetic_user_id(sender: &str, origin: &str) -> String {
    let pick = if !sender.is_empty() {
        sender
    } else if !origin.is_empty() {
        origin
    } else {
        "_bus"
    };
    format!("agent:{pick}")
}

/// Approximate token count using the same 1.5 chars/token heuristic the
/// dispatcher uses elsewhere.  Pure CPU — no tokenizer instantiation.
fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count();
    ((chars as f64) / 1.5).ceil() as u32
}

/// Synthesise the minimal `SessionMessage` pair the metrics extractor needs.
///
/// We map the dispatched payload onto a single `user` turn and the
/// sub-agent's response onto a single `assistant` turn.  Sub-agents may
/// produce multi-paragraph replies — that's fine, the metrics extractor
/// works at message granularity, not per-paragraph.
pub fn build_synthetic_messages(payload: &str, response: &str) -> Vec<SessionMessage> {
    let now = chrono::Utc::now().to_rfc3339();
    vec![
        SessionMessage {
            role: "user".to_string(),
            content: payload.to_string(),
            tokens: estimate_tokens(payload),
            timestamp: now.clone(),
        },
        SessionMessage {
            role: "assistant".to_string(),
            content: response.to_string(),
            tokens: estimate_tokens(response),
            timestamp: now,
        },
    ]
}

/// Optional GVU machinery — when supplied, sub-agent predictions whose
/// composite error lands in Significant / Critical fire the same evolution
/// loop the channel path uses.
pub struct GvuTriggerCtx {
    pub gvu_loop: Arc<GvuLoop>,
    pub notebook: Option<Arc<MistakeNotebook>>,
    pub home_dir: PathBuf,
}

/// Run a single prediction cycle for a completed sub-agent dispatch.
///
/// Designed for `tokio::spawn` — every error is logged at `warn!` and
/// swallowed so a flaky prediction path can never break the dispatcher's
/// happy path.
///
/// `gvu_ctx`: when `Some`, an eligible Significant/Critical error also
/// triggers the GVU loop. Eligibility is decided by [`crate::gvu::trigger`].
pub async fn record_subagent_prediction(
    prediction_engine: Arc<PredictionEngine>,
    agent_id: String,
    sender_agent: String,
    origin_agent: String,
    payload: String,
    response_text: String,
    gvu_ctx: Option<Arc<GvuTriggerCtx>>,
) {
    if response_text.is_empty() || payload.is_empty() {
        debug!(
            agent = %agent_id,
            "subagent prediction skipped — empty payload or response"
        );
        return;
    }

    let user_id = synthetic_user_id(&sender_agent, &origin_agent);
    let session_id = format!("subagent:{}:{}", agent_id, chrono::Utc::now().timestamp_millis());

    // 1. Predict
    let prediction = prediction_engine
        .predict(&user_id, &agent_id, &payload)
        .await;

    // 2. Build synthetic metrics from a fake 2-turn session
    let messages = build_synthetic_messages(&payload, &response_text);
    let metrics = ConversationMetrics::extract(
        &session_id,
        &agent_id,
        &user_id,
        &messages,
        0,
    );

    // 3. Calculate the error and reuse the embedding (if any) for update.
    let (error, embedding) = prediction_engine
        .calculate_error(&prediction, &metrics)
        .await;

    // 4. Persist the evolution event so silence-breaker / metacognition see it.
    prediction_engine.log_evolution_event(
        "prediction_error",
        &agent_id,
        Some(error.composite_error),
        Some(&format!("{:?}", error.category)),
        None,
        None,
        None,
    );

    // 5. Update the per-user model.  This writes the user_models row that
    //    was previously missing for sub-agents.
    prediction_engine
        .update_model_with_embedding(&metrics, embedding)
        .await;

    debug!(
        target: "subagent_prediction",
        agent = %agent_id,
        user = %user_id,
        composite_error = format!("{:.3}", error.composite_error),
        category = ?error.category,
        "Sub-agent prediction recorded"
    );

    // 6. Optionally fire GVU (P1, 2026-05-09). The trigger helper enforces
    //    its own eligibility rules — we just hand it the context.
    if let Some(ctx) = gvu_ctx {
        let agent_dir = ctx.home_dir.join("agents").join(&agent_id);
        let home_for_llm = ctx.home_dir.clone();
        let payload_preview: String = payload.chars().take(400).collect();
        let extra = format!(
            "Sub-agent dispatch payload preview:\n{payload_preview}"
        );

        // LLM caller mirrors channel_reply's setup so the GVU loop has a
        // working Generator/Judge backend.
        let call_llm = move |prompt: String| {
            let h = home_for_llm.clone();
            async move {
                crate::channel_reply::call_claude_cli_public(
                    &prompt,
                    "claude-haiku-4-5",
                    "",
                    &h,
                )
                .await
            }
        };

        let _decision = maybe_run_gvu(
            Some(ctx.gvu_loop.clone()),
            prediction_engine.clone(),
            ctx.notebook.clone(),
            &agent_id,
            &agent_dir,
            error.composite_error,
            error.category,
            TriggerSource::SubAgentDispatch,
            Some(&extra),
            call_llm,
        )
        .await;
        // Outcome is logged inside maybe_run_gvu — nothing else to do here.
    }
}

/// Convenience wrapper that swallows panics and detaches as a background
/// task.  The dispatcher uses this so it can keep running even if a buggy
/// prediction path panics.
pub fn spawn_record(
    prediction_engine: Option<Arc<PredictionEngine>>,
    agent_id: String,
    sender_agent: Option<String>,
    origin_agent: Option<String>,
    payload: String,
    response_text: String,
    gvu_ctx: Option<Arc<GvuTriggerCtx>>,
) {
    let Some(pe) = prediction_engine else {
        return;
    };
    let sender = sender_agent.unwrap_or_default();
    let origin = origin_agent.unwrap_or_default();
    tokio::spawn(async move {
        if let Err(e) = std::panic::AssertUnwindSafe(record_subagent_prediction(
            pe, agent_id, sender, origin, payload, response_text, gvu_ctx,
        ))
        .catch_unwind()
        .await
        {
            warn!("subagent prediction panicked: {e:?}");
        }
    });
}

// `catch_unwind` for futures lives in `futures-util` — pull it in via the
// extension trait alias the rest of the gateway already imports.
use futures_util::FutureExt as _;

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::Path;
    use tempfile::TempDir;

    fn make_engine(tmp: &TempDir) -> Arc<PredictionEngine> {
        let db = tmp.path().join("prediction.db");
        let meta = tmp.path().join("metacognition.json");
        Arc::new(PredictionEngine::new(db, Some(meta)))
    }

    fn count_user_models(db: &Path, user_id: &str, agent_id: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM user_models WHERE user_id = ?1 AND agent_id = ?2",
            rusqlite::params![user_id, agent_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    fn count_evolution_events(db: &Path, agent_id: &str, etype: &str) -> i64 {
        let conn = Connection::open(db).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM evolution_events WHERE agent_id = ?1 AND event_type = ?2",
            rusqlite::params![agent_id, etype],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    #[test]
    fn test_synthetic_user_id_prefers_sender() {
        assert_eq!(synthetic_user_id("tl", "agnes"), "agent:tl");
        assert_eq!(synthetic_user_id("", "agnes"), "agent:agnes");
        assert_eq!(synthetic_user_id("", ""), "agent:_bus");
    }

    #[test]
    fn test_build_synthetic_messages_pairs_user_assistant() {
        let m = build_synthetic_messages("hello", "world is large");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].role, "user");
        assert_eq!(m[0].content, "hello");
        assert_eq!(m[1].role, "assistant");
        assert_eq!(m[1].content, "world is large");
        assert!(m[0].tokens > 0);
    }

    #[tokio::test]
    async fn test_record_subagent_prediction_writes_user_model_and_event() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let db = tmp.path().join("prediction.db");

        record_subagent_prediction(
            engine.clone(),
            "duduclaw-tl".to_string(),
            "agnes".to_string(),
            "agnes".to_string(),
            "請更新版本".to_string(),
            "已完成 v1.8.36".to_string(),
            None,
        )
        .await;

        // log_evolution_event uses spawn_blocking; wait briefly for the row.
        for _ in 0..50 {
            if count_evolution_events(&db, "duduclaw-tl", "prediction_error") > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            count_evolution_events(&db, "duduclaw-tl", "prediction_error"),
            1,
            "evolution_events.prediction_error must be recorded"
        );

        // user_models is persisted via update_model_with_embedding which
        // batches every N updates by default. Drive multiple cycles so the
        // row is flushed even if save_interval > 1.
        for _ in 0..6 {
            record_subagent_prediction(
                engine.clone(),
                "duduclaw-tl".to_string(),
                "agnes".to_string(),
                "agnes".to_string(),
                "繼續".to_string(),
                "好的".to_string(),
                None,
            )
            .await;
        }
        for _ in 0..50 {
            if count_user_models(&db, "agent:agnes", "duduclaw-tl") > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("user_models row never persisted");
    }

    #[tokio::test]
    async fn test_record_subagent_prediction_skips_empty() {
        let tmp = TempDir::new().unwrap();
        let engine = make_engine(&tmp);
        let db = tmp.path().join("prediction.db");

        record_subagent_prediction(
            engine.clone(),
            "x".to_string(),
            "y".to_string(),
            "z".to_string(),
            "".to_string(),
            "non-empty".to_string(),
            None,
        )
        .await;

        // Allow any spawned task to flush.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(
            count_evolution_events(&db, "x", "prediction_error"),
            0,
            "empty payload must skip the entire pipeline"
        );
    }

    #[test]
    fn test_estimate_tokens_handles_cjk() {
        // 5 ASCII chars / 1.5 = 4 (ceil)
        assert_eq!(estimate_tokens("hello"), 4);
        // 5 CJK chars / 1.5 = 4 (ceil)
        assert_eq!(estimate_tokens("你好世界平"), 4);
    }
}
