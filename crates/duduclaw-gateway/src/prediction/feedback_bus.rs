//! Trust feedback bus — bridges prediction error to wiki trust state.
//!
//! Phase 2 of the wiki RL trust feedback system. Lives in the gateway because
//! `PredictionError` is a gateway type, but logically it's a thin dispatcher:
//!
//! ```text
//! PredictionError.composite_error
//!         │   ↓ TrustSignal::from_composite_error
//!         │ ┌──────────────────┐
//!         └►│ TrustFeedbackBus │
//!           └──┬───────────────┘
//!              │ drain global CitationTracker by conversation_id
//!              │ for each cited page → WikiTrustStore.upsert_signal
//!              ▼
//!     wiki_trust_state row updated, audit history appended
//! ```
//!
//! The bus is *fail-soft*: if the trust store is not initialised, or any
//! single page write fails, the bus logs and continues. The prediction loop
//! must never be blocked by trust feedback.

use std::sync::Arc;

use tracing::{debug, info, warn};

use duduclaw_memory::feedback::{global_tracker, CitationTracker, TrustSignal};
use duduclaw_memory::trust_store::{global_trust_store, WikiTrustStore};

use super::engine::PredictionError;

/// Stateless dispatcher — holds the singleton tracker + trust store.
#[derive(Clone)]
pub struct TrustFeedbackBus {
    tracker: Arc<CitationTracker>,
    store: Arc<WikiTrustStore>,
}

impl TrustFeedbackBus {
    /// Construct using the process-wide singletons. Returns `None` if the
    /// trust store has not been initialised — caller should skip wiring the
    /// bus rather than fail loudly (other prediction features remain useful
    /// even without trust feedback).
    pub fn from_globals() -> Option<Self> {
        let tracker = global_tracker();
        let store = global_trust_store()?;
        Some(Self { tracker, store })
    }

    /// Construct with explicit dependencies (used by tests + for future
    /// per-process injection should we drop the singleton pattern).
    pub fn new(tracker: Arc<CitationTracker>, store: Arc<WikiTrustStore>) -> Self {
        Self { tracker, store }
    }

    /// Apply a finalised `PredictionError` to the citations recorded for the
    /// matching conversation. Drains the tracker, so re-calling for the same
    /// conversation_id is a no-op.
    pub fn on_prediction_error(
        &self,
        conversation_id: &str,
        agent_id: &str,
        error: &PredictionError,
    ) -> usize {
        self.dispatch(conversation_id, agent_id, error.composite_error, 1.0)
    }

    /// Phase 6: high-confidence verdict from the GVU loop deserves a stronger
    /// signal than a raw prediction-error gradient. Apply a 2× magnitude
    /// multiplier on top of the standard mapping, then dispatch.
    pub fn on_gvu_outcome(
        &self,
        conversation_id: &str,
        agent_id: &str,
        composite_error: f64,
    ) -> usize {
        self.dispatch(conversation_id, agent_id, composite_error, 2.0)
    }

    fn dispatch(
        &self,
        conversation_id: &str,
        agent_id: &str,
        composite_error: f64,
        magnitude_factor: f32,
    ) -> usize {
        let signal = TrustSignal::from_composite_error(composite_error).scaled(magnitude_factor);
        if !signal.is_actionable() {
            // Even on Neutral we drain to release memory — the citations
            // for this conversation are no longer needed.
            let _ = self.tracker.drain(conversation_id);
            debug!(
                conv = conversation_id,
                composite = composite_error,
                "trust feedback: neutral signal, drained without write"
            );
            return 0;
        }

        let citations = self.tracker.drain(conversation_id);
        if citations.is_empty() {
            return 0;
        }

        let mut applied = 0usize;
        for citation in &citations {
            // Q1: per-agent trust — citation already carries agent_id.
            // Verify it matches the prediction's agent_id (defensive — in
            // practice, both come from the same channel turn). If mismatched,
            // prefer the citation's agent_id since that's where the trust
            // mutation should land.
            if citation.agent_id != agent_id {
                debug!(
                    citation_agent = %citation.agent_id,
                    pred_agent = agent_id,
                    "trust feedback: citation agent mismatch, using citation's agent"
                );
            }
            // Phase 5: scale negative magnitude for VerifiedFact pages so
            // a malicious user can't bury authoritative concepts via
            // manufactured dissatisfaction. (review HIGH-code: was a
            // fragile string compare; now type-safe via SourceType enum.)
            let scaled_signal = if matches!(signal, TrustSignal::Negative { .. })
                && citation.source_type == duduclaw_memory::SourceType::VerifiedFact
            {
                signal.scaled(0.5)
            } else {
                signal
            };

            // Use citation.session_id as the per-conversation cap budget key
            // (review BLOCKER R2-1). Falls back to conversation_id if absent
            // — that's the case for non-channel callers like cron tasks,
            // where per-turn cap is the safer default than no cap.
            let cap_key = citation
                .session_id
                .as_deref()
                .unwrap_or(conversation_id);
            match self.store.upsert_signal(
                &citation.page_path,
                &citation.agent_id,
                scaled_signal,
                Some(cap_key),
                Some(composite_error),
            ) {
                Ok(duduclaw_memory::UpsertResult::Applied(outcome)) => {
                    applied += 1;
                    crate::metrics::global_metrics().wiki_trust_signal_applied();
                    if outcome.became_archived {
                        crate::metrics::global_metrics().wiki_trust_archive();
                        info!(
                            page = %outcome.page_path,
                            agent = %outcome.agent_id,
                            new_trust = outcome.new_trust,
                            "wiki page auto-archived (do_not_inject) by trust feedback"
                        );
                    }
                    if outcome.became_recovered {
                        crate::metrics::global_metrics().wiki_trust_recovery();
                        info!(
                            page = %outcome.page_path,
                            agent = %outcome.agent_id,
                            new_trust = outcome.new_trust,
                            "wiki page recovered from quarantine"
                        );
                    }
                }
                // R5 review: each skip path now has its own counter so
                // operators can distinguish "page locked" from "user
                // exhausted budget" from "rate limit hit".
                Ok(duduclaw_memory::UpsertResult::SkippedLocked) => {
                    crate::metrics::global_metrics().wiki_trust_signal_dropped_locked();
                }
                Ok(duduclaw_memory::UpsertResult::SkippedConvCap) => {
                    crate::metrics::global_metrics().wiki_trust_signal_dropped_capped();
                }
                Ok(duduclaw_memory::UpsertResult::SkippedDailyLimit) => {
                    crate::metrics::global_metrics().wiki_trust_signal_dropped_daily_limit();
                }
                Ok(duduclaw_memory::UpsertResult::SkippedNeutral) => {
                    // Neutral signals never reach upsert (we filtered above);
                    // treat as no-op without a counter bump.
                }
                Err(e) => {
                    warn!(
                        page = %citation.page_path,
                        agent = %citation.agent_id,
                        error = %e,
                        "wiki trust upsert failed (non-fatal)"
                    );
                }
            }
        }
        debug!(
            conv = conversation_id,
            citations = citations.len(),
            applied,
            composite = composite_error,
            signal = ?signal,
            "trust feedback bus dispatched"
        );
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_memory::feedback::{TrustSignal, WikiCitation};

    fn dummy_error(composite: f64) -> PredictionError {
        use chrono::Utc;
        use crate::prediction::engine::{ErrorCategory, Prediction};
        use crate::prediction::metrics::{ConversationMetrics, FeedbackDetail};

        let prediction = Prediction {
            expected_satisfaction: 0.5,
            expected_follow_up_rate: 0.5,
            expected_topic: None,
            confidence: 0.5,
            timestamp: Utc::now(),
        };
        let actual = ConversationMetrics {
            session_id: "test".into(),
            user_id: "u".into(),
            agent_id: "a".into(),
            message_count: 0,
            user_message_count: 0,
            assistant_message_count: 0,
            avg_assistant_response_length: 0.0,
            total_tokens: 0,
            response_time_ms: 0,
            user_follow_ups: 0,
            user_corrections: 0,
            feedback_details: FeedbackDetail::default(),
            detected_language: "zh-TW".into(),
            extracted_topics: vec![],
            ended_naturally: true,
            feedback_signal: None,
            timestamp: Utc::now(),
            user_text: String::new(),
        };
        PredictionError {
            delta_satisfaction: 0.0,
            topic_surprise: 0.0,
            unexpected_correction: false,
            unexpected_follow_up: false,
            task_completion_failure: false,
            composite_error: composite,
            category: if composite >= 0.55 {
                ErrorCategory::Significant
            } else {
                ErrorCategory::Negligible
            },
            prediction,
            actual,
        }
    }

    fn page_citation(conv: &str, page: &str, agent: &str) -> WikiCitation {
        WikiCitation {
            page_path: page.into(),
            agent_id: agent.into(),
            conversation_id: conv.into(),
            retrieved_at: chrono::Utc::now(),
            trust_at_cite: 0.5,
            source_type: duduclaw_memory::SourceType::Unknown,
            session_id: None,
        }
    }

    #[test]
    fn negative_signal_lowers_trust_for_all_cited_pages() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());

        tracker.record(page_citation("conv-1", "concepts/a.md", "agnes"));
        tracker.record(page_citation("conv-1", "sources/b.md", "agnes"));

        let err = dummy_error(0.85);
        let applied = bus.on_prediction_error("conv-1", "agnes", &err);
        assert_eq!(applied, 2);

        let a = store.get("concepts/a.md", "agnes").unwrap().unwrap();
        let b = store.get("sources/b.md", "agnes").unwrap().unwrap();
        assert!(a.trust < 0.5);
        assert!(b.trust < 0.5);
        assert!(a.error_signal_count >= 1);
        assert!(b.error_signal_count >= 1);
    }

    #[test]
    fn positive_signal_raises_trust() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());
        tracker.record(page_citation("conv-2", "concepts/a.md", "agnes"));

        bus.on_prediction_error("conv-2", "agnes", &dummy_error(0.05));

        let a = store.get("concepts/a.md", "agnes").unwrap().unwrap();
        assert!(a.trust > 0.5);
        assert_eq!(a.success_signal_count, 1);
    }

    #[test]
    fn neutral_signal_drains_without_writing() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());
        tracker.record(page_citation("conv-3", "concepts/a.md", "agnes"));

        let applied = bus.on_prediction_error("conv-3", "agnes", &dummy_error(0.30));
        assert_eq!(applied, 0);
        // Tracker drained → no second-application possible.
        assert!(tracker.peek("conv-3").is_empty());
        // No row should exist in trust store either.
        assert!(store.get("concepts/a.md", "agnes").unwrap().is_none());
    }

    #[test]
    fn missing_citations_for_conv_is_noop() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker, store);
        let applied = bus.on_prediction_error("never-cited", "agnes", &dummy_error(0.85));
        assert_eq!(applied, 0);
    }

    #[test]
    fn gvu_outcome_doubles_magnitude() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());

        // Same composite_error feeding both paths.
        tracker.record(page_citation("c-pred", "p-pred.md", "agnes"));
        tracker.record(page_citation("c-gvu", "p-gvu.md", "agnes"));

        bus.on_prediction_error("c-pred", "agnes", &dummy_error(0.85));
        bus.on_gvu_outcome("c-gvu", "agnes", 0.85);

        let pred = store.get("p-pred.md", "agnes").unwrap().unwrap();
        let gvu = store.get("p-gvu.md", "agnes").unwrap().unwrap();
        // Both signals are clamped by the per-conversation cap (default 0.10),
        // so doubling can saturate at the cap rather than fully doubling.
        // Verify GVU has fallen *at least as much* (and ideally more, which
        // is what the cap allows).
        assert!(gvu.trust <= pred.trust, "GVU magnitude should not be smaller");
    }

    #[test]
    fn verified_fact_resists_negative_signal() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());

        let mut verified = page_citation("c1", "concepts/cron-facts.md", "agnes");
        verified.source_type = duduclaw_memory::SourceType::VerifiedFact;
        let unknown = page_citation("c1", "sources/old-talk.md", "agnes");
        tracker.record(verified);
        tracker.record(unknown);

        bus.on_prediction_error("c1", "agnes", &dummy_error(0.85));

        let v = store.get("concepts/cron-facts.md", "agnes").unwrap().unwrap();
        let u = store.get("sources/old-talk.md", "agnes").unwrap().unwrap();
        // VerifiedFact's negative magnitude is halved → its trust drops less.
        assert!(v.trust > u.trust, "verified_fact should retain more trust");
    }

    #[test]
    fn signal_magnitude_matches_composite_error_curve() {
        let tracker = Arc::new(CitationTracker::new());
        let store = Arc::new(WikiTrustStore::in_memory().unwrap());
        let bus = TrustFeedbackBus::new(tracker.clone(), store.clone());

        tracker.record(page_citation("c1", "p1.md", "agnes"));
        tracker.record(page_citation("c2", "p2.md", "agnes"));

        bus.on_prediction_error("c1", "agnes", &dummy_error(1.00));
        bus.on_prediction_error("c2", "agnes", &dummy_error(0.60));

        let p1 = store.get("p1.md", "agnes").unwrap().unwrap();
        let p2 = store.get("p2.md", "agnes").unwrap().unwrap();
        // Higher composite error → bigger drop in trust.
        assert!(p1.trust < p2.trust, "1.00 should hit harder than 0.60");
    }
}
