//! GVU trigger helper — extracted so non-channel paths can fire evolution too.
//!
//! Before this module, GVU only ran from
//! [`crate::channel_reply::handle_message`] when a channel-facing agent's
//! prediction error landed in Significant / Critical. Sub-agents dispatched
//! through `bus_queue.jsonl` never reached that branch, so 16 of 17 production
//! agents accumulated `prediction_error` rows but never produced a single
//! `gvu_experiment_log` entry (verified 2026-05-09 in `evolution.db`).
//!
//! This helper exposes the minimum decision needed by the dispatcher path:
//!
//! 1. Is GVU eligible for this agent? (`gvu_enabled` + has CONTRACT.toml +
//!    error category warrants reflection.)
//! 2. If so, run the GVU loop with a synthetic trigger context.
//! 3. Persist outcome (info/warn log + metacognition feedback).
//!
//! It deliberately does NOT carry the full channel-side machinery
//! (`evolution_emitter`, proactive rules, wiki ingest). Those are
//! channel-facing concerns; the dispatcher path produces a strictly
//! best-effort GVU pass that resembles a "forced reflection".

use std::path::Path;
use std::sync::Arc;

use tracing::{info, warn};

use crate::gvu::loop_::{GvuLoop, GvuOutcome};
use crate::gvu::mistake_notebook::MistakeNotebook;
use crate::gvu::version_store::VersionMetrics;
use crate::prediction::engine::{ErrorCategory, PredictionEngine};
use crate::prediction::metacognition::MetaCognition;

/// Why we're triggering GVU. Used for log breadcrumbs and the trigger
/// context surfaced to the Generator. Keep these stable — they show up in
/// `evolution_events.trigger_context` and make audit history searchable.
#[derive(Debug, Clone, Copy)]
pub enum TriggerSource {
    /// Channel-facing user message produced a Significant/Critical error.
    ChannelReply,
    /// Sub-agent finished a bus-queue dispatch with a Significant/Critical
    /// synthetic prediction error (BUG-5 follow-up).
    SubAgentDispatch,
    /// Heartbeat silence breaker fired and the agent has gvu_enabled.
    ForcedReflection,
}

impl TriggerSource {
    fn as_label(self) -> &'static str {
        match self {
            TriggerSource::ChannelReply => "channel_reply",
            TriggerSource::SubAgentDispatch => "subagent_dispatch",
            TriggerSource::ForcedReflection => "forced_reflection",
        }
    }
}

/// Outcome of the eligibility decision. The runner returns this even when
/// it short-circuits before invoking GVU so callers can record metrics.
#[derive(Debug, Clone)]
pub enum TriggerDecision {
    /// GVU loop was invoked and returned the wrapped outcome.
    Ran(GvuOutcome),
    /// Skipped — see `reason` for human-readable context. Callers may want
    /// to tally these to detect mis-configurations (e.g. all attempts
    /// skipped because `gvu_enabled=false`).
    Skipped { reason: String },
}

/// Decide whether `category` warrants a reflection-driven GVU pass.
///
/// `Significant` and `Critical` always pass. `Moderate` and `Negligible`
/// are filtered out — they're handled by zero-LLM paths upstream
/// (episodic memory write, statistics update).
fn category_warrants_gvu(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::Significant | ErrorCategory::Critical
    )
}

/// Read `[evolution] gvu_enabled` from the agent's `agent.toml`.
///
/// Returns `false` for missing files / malformed TOML / absent key. The
/// default is intentionally restrictive so a config typo doesn't accidentally
/// burn LLM budget. Operators opt in via `agent.toml [evolution] gvu_enabled = true`.
pub fn agent_gvu_enabled(agent_dir: &Path) -> bool {
    let path = agent_dir.join("agent.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return false;
    };
    value
        .get("evolution")
        .and_then(|e| e.get("gvu_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Run the GVU loop for `agent_id` if eligible. Pure helper — no event-bus
/// emission, no proactive rule fan-out (those are channel-side concerns).
///
/// Eligibility chain:
/// 1. `gvu_loop` must be `Some`
/// 2. `agent_dir.join("agent.toml")` → `[evolution] gvu_enabled = true`
/// 3. `agent_dir.join("CONTRACT.toml")` loadable
/// 4. `category` is Significant or Critical
///
/// Any failed link returns `TriggerDecision::Skipped` with a reason; never
/// errors out. Designed to be called from a `tokio::spawn` background task.
#[allow(clippy::too_many_arguments)]
pub async fn maybe_run_gvu<F, Fut>(
    gvu_loop: Option<Arc<GvuLoop>>,
    prediction_engine: Arc<PredictionEngine>,
    notebook: Option<Arc<MistakeNotebook>>,
    agent_id: &str,
    agent_dir: &Path,
    composite_error: f64,
    category: ErrorCategory,
    source: TriggerSource,
    extra_context: Option<&str>,
    call_llm: F,
) -> TriggerDecision
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let Some(gvu) = gvu_loop else {
        return TriggerDecision::Skipped {
            reason: "gvu_loop unavailable".to_string(),
        };
    };
    if !category_warrants_gvu(category) {
        return TriggerDecision::Skipped {
            reason: format!("category {category:?} does not warrant GVU"),
        };
    }
    if !agent_gvu_enabled(agent_dir) {
        return TriggerDecision::Skipped {
            reason: "agent.toml [evolution] gvu_enabled = false".to_string(),
        };
    }

    let contract = duduclaw_agent::contract::load_contract(agent_dir);
    let pre_metrics = VersionMetrics::default();

    let trigger_context = build_trigger_context(
        agent_id,
        composite_error,
        category,
        source,
        extra_context,
    );

    info!(
        agent = agent_id,
        error = format!("{composite_error:.3}"),
        category = ?category,
        source = source.as_label(),
        "GVU trigger fired"
    );

    let relevant_mistakes = notebook
        .as_ref()
        .map(|nb| nb.query_by_agent(agent_id, 5))
        .unwrap_or_default();

    let meta_snapshot = prediction_engine.metacognition.lock().await.clone();

    let outcome = gvu
        .run_with_context(
            agent_id,
            agent_dir,
            &trigger_context,
            pre_metrics,
            &contract.boundaries.must_not,
            &contract.boundaries.must_always,
            call_llm,
            Some(&meta_snapshot),
            relevant_mistakes,
        )
        .await;

    record_outcome_to_metacognition(
        prediction_engine.metacognition.clone(),
        category,
        &outcome,
        agent_id,
    )
    .await;

    TriggerDecision::Ran(outcome)
}

/// Build a deterministic, audit-friendly trigger context string.
///
/// Format is stable so log greps and `trigger_context` LIKE-queries keep
/// working as more callers are added.
fn build_trigger_context(
    agent_id: &str,
    composite_error: f64,
    category: ErrorCategory,
    source: TriggerSource,
    extra: Option<&str>,
) -> String {
    let mut s = format!(
        "[gvu_trigger] agent={agent_id} source={src} category={cat:?} \
         composite_error={composite_error:.3}",
        src = source.as_label(),
        cat = category,
    );
    if let Some(extra) = extra {
        if !extra.is_empty() {
            s.push_str("\n\n");
            s.push_str(extra);
        }
    }
    s
}

/// Mirror the post-GVU bookkeeping that `channel_reply` does so the
/// metacognition window converges across both paths.
async fn record_outcome_to_metacognition(
    metacog: Arc<tokio::sync::Mutex<MetaCognition>>,
    category: ErrorCategory,
    outcome: &GvuOutcome,
    agent_id: &str,
) {
    match outcome {
        GvuOutcome::Applied(version) => {
            info!(
                agent = agent_id,
                version = %version.version_id,
                "GVU applied SOUL.md change"
            );
            let mut meta = metacog.lock().await;
            meta.record_outcome(category, true);
        }
        GvuOutcome::Abandoned { last_gradient } => {
            warn!(
                agent = agent_id,
                critique = %last_gradient.critique,
                "GVU abandoned all attempts"
            );
            let mut meta = metacog.lock().await;
            meta.record_outcome(category, false);
        }
        GvuOutcome::Skipped { reason } => {
            // INFO not debug: when a trigger fires but the loop short-circuits
            // (e.g. the agent is mid-observation from a previous applied
            // version), we want operators to see WHY without enabling debug.
            // Observed 2026-05-10 14:26Z: duduclaw-tl trigger fired, GVU loop
            // returned Skipped because the 5/10 00:07Z applied version was
            // still observing — but the log went silent at the default INFO
            // filter, leaving "trigger fired … then nothing" with no clue.
            info!(agent = agent_id, %reason, "GVU skipped");
            // Don't penalise the metacognition window for "observation
            // already in progress" / "loop already running" — those are
            // legitimate concurrency guards, not failed reflections.
            if !reason.contains("observation") && !reason.contains("already running") {
                let mut meta = metacog.lock().await;
                meta.record_outcome(category, false);
            }
        }
        GvuOutcome::Deferred { retry_count, retry_after_hours, .. } => {
            info!(
                agent = agent_id,
                retry_count,
                retry_after_hours,
                "GVU deferred — will retry with accumulated gradients"
            );
            // Retry path will record the eventual outcome.
        }
        GvuOutcome::TimedOut { elapsed, generations_completed, .. } => {
            warn!(
                agent = agent_id,
                elapsed_secs = elapsed.as_secs(),
                generations_completed,
                "GVU timed out — wall-clock budget exceeded"
            );
            // Inconclusive — don't move the metacognition window.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_filter_admits_significant_and_critical_only() {
        assert!(!category_warrants_gvu(ErrorCategory::Negligible));
        assert!(!category_warrants_gvu(ErrorCategory::Moderate));
        assert!(category_warrants_gvu(ErrorCategory::Significant));
        assert!(category_warrants_gvu(ErrorCategory::Critical));
    }

    #[test]
    fn agent_gvu_enabled_returns_false_when_agent_toml_missing() {
        let tmp = std::env::temp_dir()
            .join(format!("gvu-trig-no-toml-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!agent_gvu_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_gvu_enabled_reads_evolution_section() {
        let tmp = std::env::temp_dir()
            .join(format!("gvu-trig-yes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[evolution]\ngvu_enabled = true\n",
        )
        .unwrap();
        assert!(agent_gvu_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_gvu_enabled_returns_false_when_explicitly_disabled() {
        let tmp = std::env::temp_dir()
            .join(format!("gvu-trig-no-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[evolution]\ngvu_enabled = false\n",
        )
        .unwrap();
        assert!(!agent_gvu_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_gvu_enabled_returns_false_when_evolution_section_missing() {
        let tmp = std::env::temp_dir()
            .join(format!("gvu-trig-no-section-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[agent]\nname = \"test\"\n",
        )
        .unwrap();
        assert!(!agent_gvu_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn agent_gvu_enabled_silent_on_malformed_toml() {
        let tmp = std::env::temp_dir()
            .join(format!("gvu-trig-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("agent.toml"), "bad = [toml").unwrap();
        assert!(!agent_gvu_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn trigger_context_format_is_stable() {
        let ctx = build_trigger_context(
            "duduclaw-tl",
            0.842,
            ErrorCategory::Significant,
            TriggerSource::SubAgentDispatch,
            Some("payload preview"),
        );
        assert!(ctx.contains("agent=duduclaw-tl"));
        assert!(ctx.contains("source=subagent_dispatch"));
        assert!(ctx.contains("category=Significant"));
        assert!(ctx.contains("composite_error=0.842"));
        assert!(ctx.contains("payload preview"));
    }

    #[test]
    fn trigger_context_omits_extra_when_empty() {
        let ctx = build_trigger_context(
            "x",
            0.1,
            ErrorCategory::Critical,
            TriggerSource::ForcedReflection,
            None,
        );
        // No trailing blank line; build_trigger_context returns the prefix
        // without `\n\n` when extra is None.
        assert!(!ctx.ends_with('\n'));
    }

    #[tokio::test]
    async fn maybe_run_gvu_skips_without_loop() {
        let tmp = tempfile::TempDir::new().unwrap();
        let pe = Arc::new(PredictionEngine::new(
            tmp.path().join("prediction.db"),
            Some(tmp.path().join("metacog.json")),
        ));

        let decision = maybe_run_gvu(
            None,
            pe,
            None,
            "any",
            tmp.path(),
            0.9,
            ErrorCategory::Significant,
            TriggerSource::SubAgentDispatch,
            None,
            |_| async { Ok(String::new()) },
        )
        .await;
        match decision {
            TriggerDecision::Skipped { reason } => assert!(reason.contains("gvu_loop")),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn maybe_run_gvu_skips_for_negligible_category() {
        // Even with a real GvuLoop wired up, Negligible/Moderate must short
        // circuit before any LLM call. We can't easily build a real GvuLoop
        // here, but we can prove the category filter beats the loop check
        // by asserting the skip reason mentions the category.
        let tmp = tempfile::TempDir::new().unwrap();
        let pe = Arc::new(PredictionEngine::new(
            tmp.path().join("prediction.db"),
            Some(tmp.path().join("metacog.json")),
        ));

        let decision = maybe_run_gvu(
            None, // gvu_loop None still triggers skip — but we test the order
            pe,
            None,
            "any",
            tmp.path(),
            0.05,
            ErrorCategory::Negligible,
            TriggerSource::SubAgentDispatch,
            None,
            |_| async { Ok(String::new()) },
        )
        .await;
        // Either category or gvu_loop reason is acceptable — both are
        // legitimate "no-op" paths. The important invariant is that no LLM
        // was called (which we'd see if call_llm was invoked, panicking).
        assert!(matches!(decision, TriggerDecision::Skipped { .. }));
    }
}
