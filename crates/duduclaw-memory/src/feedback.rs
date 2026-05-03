//! Wiki RL Trust Feedback — Phase 0/1 primitives.
//!
//! This module hosts the *data types* and *in-memory tracker* that bridge
//! the RAG retrieval path and the prediction-engine feedback loop.
//!
//! Pipeline:
//! ```text
//! [WikiStore::search] → SearchHit (page_path, trust, source_type)
//!         ↓                    record_citation(conv_id, citation)
//! [CitationTracker]            ────────────────────────────────────►
//!         ↓                    drain(conv_id)
//! [TrustFeedbackBus]           ◄────────── PredictionError.composite_error
//!         ↓                    on_prediction_error(...)
//! [WikiTrustStore.upsert_signal] (Phase 2 — see trust_store.rs)
//! ```
//!
//! This file deliberately stays free of SQLite — the persisted state lives in
//! `trust_store.rs` (Phase 2). Keeping the citation tracker pure-memory means
//! a missed feedback (Claude crash before drain) costs at most one conversation
//! of trust adjustments, never corrupted on-disk state.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::wiki::SourceType;

// ---------------------------------------------------------------------------
// Citation tracking — records which wiki pages were used to construct a reply
// ---------------------------------------------------------------------------

/// One RAG retrieval event — a wiki page was surfaced and (presumably)
/// influenced the LLM's reply.
///
/// Stored in `CitationTracker` keyed by `conversation_id` until the
/// prediction engine calculates the post-conversation error and asks the
/// tracker to drain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiCitation {
    /// Path relative to wiki root (e.g. `concepts/cron-scheduling-facts.md`).
    pub page_path: String,
    /// Agent that performed the retrieval. Used by `WikiTrustStore` PK to keep
    /// per-agent trust (Q1 decision: per-agent, not global).
    pub agent_id: String,
    /// Conversation that surfaced this citation. Same id used by the
    /// prediction engine when reporting the error.
    pub conversation_id: String,
    /// Wall-clock retrieval time — used for stale-citation GC.
    pub retrieved_at: DateTime<Utc>,
    /// Trust score at the moment of retrieval. Stored for retrospective
    /// analysis (e.g. "page X had trust 0.6 when it misled, but 0.3 now").
    pub trust_at_cite: f32,
    /// Provenance — used by the feedback bus to scale negative signals
    /// (Phase 5: VerifiedFact pages get reduced negative magnitude).
    /// Typed via `SourceType` rather than free-form string so a refactor
    /// can't silently disable VerifiedFact resistance (review HIGH-code).
    #[serde(default)]
    pub source_type: SourceType,
    /// **Session-scoped** budget id used by `WikiTrustStore`'s
    /// per-conversation cap. Distinct from `conversation_id` (which is the
    /// per-turn drain key) so that the 0.10 cap applies across an entire
    /// session, not per turn (review BLOCKER R2-1: when conversation_id
    /// became per-turn, the cap silently broke into 5+× looser semantics).
    /// `None` outside the channel-reply path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Trust signals — derived from prediction error / GVU verdict
// ---------------------------------------------------------------------------

/// Polarity + magnitude of a feedback signal aimed at a wiki page's trust.
///
/// Cutoffs are intentionally asymmetric so noise stays Neutral and only
/// confident verdicts move trust:
/// - `error < 0.20` → Positive (page contributed correctly)
/// - `0.20 ≤ error < 0.55` → Neutral (no change, no log noise)
/// - `error ≥ 0.55` → Negative (page likely misled)
///
/// Magnitudes are deliberately small (max ±0.10/conv) so a single bad
/// conversation can never bury a previously-trusted page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrustSignal {
    /// Reduce uncertainty / confirm a useful page.
    Positive { magnitude: f32 },
    /// Don't adjust — the signal is too ambiguous.
    Neutral,
    /// Page likely misled the response.
    Negative { magnitude: f32 },
}

impl TrustSignal {
    /// Map a `composite_error` (0.0 – 1.0, higher = worse) to a signal.
    ///
    /// The defaults match `TODO-wiki-rl-trust-feedback.md` Phase 0 spec.
    /// Use `from_composite_error_with_thresholds` if you need tuned cutoffs.
    pub fn from_composite_error(err: f64) -> Self {
        Self::from_composite_error_with_thresholds(err, 0.20, 0.55)
    }

    pub fn from_composite_error_with_thresholds(err: f64, low: f64, high: f64) -> Self {
        if err < low {
            // Map [0, low) → magnitude (0.005 .. 0.02). Lower error → bigger boost.
            let m = (1.0 - err / low).clamp(0.0, 1.0) as f32 * 0.02;
            Self::Positive { magnitude: m.max(0.005) }
        } else if err >= high {
            // Map [high, 1.0] → magnitude (0.02 .. 0.10). Higher error → bigger penalty.
            let span = (1.0 - high).max(1e-6);
            let m = ((err - high) / span).clamp(0.0, 1.0) as f32;
            Self::Negative { magnitude: 0.02 + m * 0.08 }
        } else {
            Self::Neutral
        }
    }

    /// Signed delta to apply to a page's trust.
    /// Positive delta increases trust; negative delta decreases it.
    pub fn delta(&self) -> f32 {
        match self {
            Self::Positive { magnitude } => *magnitude,
            Self::Neutral => 0.0,
            Self::Negative { magnitude } => -magnitude,
        }
    }

    /// Whether this signal would actually mutate trust state.
    pub fn is_actionable(&self) -> bool {
        !matches!(self, Self::Neutral)
    }

    /// Scale a signal's magnitude (useful for boosting GVU-derived signals
    /// in Phase 6, or halving against `VerifiedFact` pages in Phase 5).
    pub fn scaled(self, factor: f32) -> Self {
        let factor = factor.max(0.0);
        match self {
            Self::Positive { magnitude } => Self::Positive { magnitude: magnitude * factor },
            Self::Negative { magnitude } => Self::Negative { magnitude: magnitude * factor },
            Self::Neutral => Self::Neutral,
        }
    }
}

// ---------------------------------------------------------------------------
// CitationTracker — pure-memory bridge from RAG hits to feedback loop
// ---------------------------------------------------------------------------

/// Number of citation entries to retain per conversation.
const MAX_CITATIONS_PER_CONV: usize = 32;

/// Default hard cap on the number of distinct conversation buckets in
/// memory at once. Operators can override via
/// `[wiki.trust_feedback].max_active_conversations` in `config.toml`
/// (review R4 DEBT-3). ~1k × 32 citations × ~250B ≈ 8MB upper bound.
pub const DEFAULT_MAX_ACTIVE_CONVERSATIONS: usize = 1_000;

/// Read-once snapshot of the cap. Adjusted at process start by
/// `set_max_active_conversations`; default 1000.
static MAX_ACTIVE_CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Override the global cap. Idempotent — first call wins; subsequent calls
/// silently ignore. Call once during gateway startup after config load.
pub fn set_max_active_conversations(cap: usize) {
    let _ = MAX_ACTIVE_CAP.set(cap.max(16));
}

fn max_active_cap() -> usize {
    *MAX_ACTIVE_CAP.get().unwrap_or(&DEFAULT_MAX_ACTIVE_CONVERSATIONS)
}

/// In-memory map of `conversation_id → Vec<WikiCitation>`.
///
/// The tracker is intentionally pure-memory:
/// - A crash forfeits at most one conversation's trust adjustments.
/// - No file I/O on every RAG hit (RAG runs hot).
/// - Garbage-collected by `run_gc` for orphaned conversations (no error
///   signal arrived within TTL).
///
/// `Arc<Self>` is the canonical sharing pattern — clone the Arc into
/// retrieval and prediction sites alike.
#[derive(Debug)]
pub struct CitationTracker {
    inner: Arc<Mutex<std::collections::HashMap<String, Vec<WikiCitation>>>>,
    ttl: StdDuration,
    /// Cumulative evictions caused by `MAX_ACTIVE_CONVERSATIONS` cap or
    /// bounded-time GC. Read by the metrics layer; never reset.
    /// (review SHIP-BLOCK R5 m12 dead counter.)
    eviction_count: Arc<std::sync::atomic::AtomicU64>,
}

impl CitationTracker {
    /// Create a tracker with the default 1-hour orphan TTL.
    pub fn new() -> Self {
        Self::with_ttl(StdDuration::from_secs(60 * 60))
    }

    /// Create a tracker with a custom TTL — typically only tests need this.
    pub fn with_ttl(ttl: StdDuration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(std::collections::HashMap::new())),
            ttl,
            eviction_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Total number of LRU/age-based evictions since process start.
    /// (review SHIP-BLOCK R5 m12 dead counter.)
    pub fn eviction_count(&self) -> u64 {
        self.eviction_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record a single citation. Caps to `MAX_CITATIONS_PER_CONV` per
    /// conversation to bound memory under pathological RAG-recall queries.
    ///
    /// Sync (std `Mutex`) — safe to call from both sync prompt-build paths
    /// and async retrieval handlers; lock contention is microsecond-scale
    /// because operations are tiny (`HashMap::entry` + push).
    pub fn record(&self, citation: WikiCitation) {
        let conv_id = citation.conversation_id.clone();
        if let Ok(mut guard) = self.inner.lock() {
            evict_lru_if_full(&mut guard, &conv_id, &self.eviction_count);
            let entry = guard.entry(conv_id).or_default();
            if entry.len() >= MAX_CITATIONS_PER_CONV {
                return;
            }
            entry.push(citation);
        }
    }

    /// Bulk record — preferred when search returns multiple hits at once.
    pub fn record_many<I: IntoIterator<Item = WikiCitation>>(&self, citations: I) {
        if let Ok(mut guard) = self.inner.lock() {
            for c in citations {
                evict_lru_if_full(&mut guard, &c.conversation_id, &self.eviction_count);
                let entry = guard.entry(c.conversation_id.clone()).or_default();
                if entry.len() >= MAX_CITATIONS_PER_CONV {
                    continue;
                }
                entry.push(c);
            }
        }
    }

    /// Drain and return all citations for a conversation. Removes the entry.
    pub fn drain(&self, conversation_id: &str) -> Vec<WikiCitation> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut g| g.remove(conversation_id))
            .unwrap_or_default()
    }

    /// Peek (non-destructive). Useful for diagnostics.
    pub fn peek(&self, conversation_id: &str) -> Vec<WikiCitation> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.get(conversation_id).cloned())
            .unwrap_or_default()
    }

    /// Number of conversations being tracked (orphaned + live).
    pub fn conv_count(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Drop entries older than `ttl`. Returns the number of conversations
    /// reaped. Intended to be called periodically — see `spawn_gc_task`.
    ///
    /// Two reap conditions (review SHIP-BLOCK R5 MUST-2 — bounded-time):
    ///   1. *Stale* — every citation in the bucket is older than `ttl`.
    ///   2. *Aged-out* — the OLDEST citation is older than `2 × ttl`,
    ///      regardless of newer entries. Defends against the keep-alive
    ///      attack where an adversary refreshes a bucket with one citation
    ///      every `ttl-ε` to hold a slot indefinitely.
    pub fn gc(&self) -> usize {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.ttl).unwrap_or_else(|_| chrono::Duration::hours(1));
        let max_age = Utc::now()
            - chrono::Duration::from_std(self.ttl.saturating_mul(2))
                .unwrap_or_else(|_| chrono::Duration::hours(2));
        let mut reaped = 0u64;
        if let Ok(mut guard) = self.inner.lock() {
            guard.retain(|_, citations| {
                let any_fresh = citations.iter().any(|c| c.retrieved_at >= cutoff);
                let oldest = citations.iter().map(|c| c.retrieved_at).min();
                let aged_out = oldest.map(|t| t < max_age).unwrap_or(false);
                let keep = any_fresh && !aged_out;
                if !keep {
                    reaped += 1;
                }
                keep
            });
        }
        if reaped > 0 {
            self.eviction_count
                .fetch_add(reaped, std::sync::atomic::Ordering::Relaxed);
        }
        reaped as usize
    }

    /// Spawn a background task that runs `gc` at a fixed interval.
    /// Returns the JoinHandle so the caller can abort on shutdown.
    pub fn spawn_gc_task(self: Arc<Self>, interval: StdDuration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await; // first tick is immediate — discard
            loop {
                ticker.tick().await;
                let _ = self.gc();
            }
        })
    }
}

impl Default for CitationTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// LRU eviction helper: when the tracker is at the global cap and the
/// incoming citation is for a brand-new conversation, evict the
/// conversation whose oldest citation is the oldest overall. Logs once-per-
/// minute when eviction triggers so operators see DoS pressure.
/// (review HIGH R3 BYPASS-3.) Bumps the tracker's eviction counter so the
/// `wiki_trust_eviction_total` Prometheus metric reflects actual pressure.
fn evict_lru_if_full(
    guard: &mut std::sync::MutexGuard<'_, std::collections::HashMap<String, Vec<WikiCitation>>>,
    new_conv_id: &str,
    eviction_count: &std::sync::atomic::AtomicU64,
) {
    if guard.contains_key(new_conv_id) || guard.len() < max_active_cap() {
        return;
    }
    let victim = guard
        .iter()
        .min_by_key(|(_, cits)| cits.iter().map(|c| c.retrieved_at).min())
        .map(|(k, _)| k.clone());
    if let Some(v) = victim {
        guard.remove(&v);
        eviction_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        log_eviction(&v);
    }
}

fn log_eviction(victim: &str) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST_LOG_SEC: AtomicI64 = AtomicI64::new(0);
    let now_sec = Utc::now().timestamp();
    let last = LAST_LOG_SEC.load(Ordering::Relaxed);
    if now_sec - last < 60 {
        return;
    }
    // (review R4 REGRESSION) Only log if THIS thread won the CAS — otherwise
    // multiple racing threads each see `now_sec - last >= 60` true and all
    // log, defeating the rate limit.
    if LAST_LOG_SEC
        .compare_exchange(last, now_sec, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        tracing::warn!(
            evicted = %victim,
            cap = max_active_cap(),
            "CitationTracker at capacity — evicting LRU conversation (rate-limited log, 1/min)"
        );
    }
}

/// RAII guard that drains the global tracker on drop. Use this when you've
/// recorded citations into the tracker but the downstream feedback path
/// might panic before draining; on stack unwind the guard runs and frees
/// the bucket so a flaky prediction engine can't leak the tracker.
/// (review HIGH R3-3 / arch-MAJOR.)
pub struct DrainOnDrop {
    conversation_id: String,
    disarmed: bool,
}

impl DrainOnDrop {
    pub fn new(conversation_id: String) -> Self {
        Self { conversation_id, disarmed: false }
    }
    /// Disarm: call when the citations have been intentionally drained
    /// elsewhere (the normal happy path). Prevents double-drain.
    /// Fields are private so external callers can't accidentally re-arm
    /// after disarm or skip disarm — only this method controls the flag.
    pub fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for DrainOnDrop {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = global_tracker().drain(&self.conversation_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Per-task turn id — propagated across the dispatcher → sub-agent chain so
// sub-agent RAG hits attribute citations to the originating turn.
// (review B2 — sub-agent dispatch was previously bypassing trust feedback.)
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// Current conversation/turn id for trust feedback. Set by the channel
    /// reply path before invoking sub-agents; read by `claude_runner`.
    /// `None` outside the channel reply task.
    pub static CURRENT_TURN_ID: Option<String>;
    /// Current channel session id — used as the per-conversation cap budget
    /// key. Distinct from `CURRENT_TURN_ID` so the 0.10 cap applies across
    /// the whole session, not per turn (review BLOCKER R2-1).
    pub static CURRENT_SESSION_ID: Option<String>;
}

// ---------------------------------------------------------------------------
// Process-wide singleton — used by RAG retrieval call-sites that don't
// already have an Arc<CitationTracker> in scope.
// ---------------------------------------------------------------------------

static GLOBAL_TRACKER: OnceLock<Arc<CitationTracker>> = OnceLock::new();

/// Lazily-initialised process-wide tracker.
///
/// First call also spawns a 5-minute GC task (orphan TTL = 1h). The GC task
/// requires a Tokio runtime; if no runtime is active when this is first
/// called, GC is skipped and the tracker still functions but never reaps —
/// callers in async contexts should prefer this getter.
pub fn global_tracker() -> Arc<CitationTracker> {
    GLOBAL_TRACKER
        .get_or_init(|| {
            let t = Arc::new(CitationTracker::new());
            // Best-effort: spawn GC if a Tokio runtime is available.
            if tokio::runtime::Handle::try_current().is_ok() {
                let _ = t.clone().spawn_gc_task(StdDuration::from_secs(5 * 60));
            }
            t
        })
        .clone()
}

/// Test-only — replace the singleton (for unit tests that need isolation).
#[cfg(test)]
pub(crate) fn _set_global_tracker_for_test(t: Arc<CitationTracker>) {
    let _ = GLOBAL_TRACKER.set(t);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(conv: &str, page: &str) -> WikiCitation {
        WikiCitation {
            page_path: page.into(),
            agent_id: "agnes".into(),
            conversation_id: conv.into(),
            retrieved_at: Utc::now(),
            trust_at_cite: 0.5,
            source_type: SourceType::Unknown,
            session_id: None,
        }
    }

    // ── TrustSignal mapping ─────────────────────────────────────────

    #[test]
    fn from_composite_error_low_is_positive() {
        let s = TrustSignal::from_composite_error(0.05);
        assert!(matches!(s, TrustSignal::Positive { .. }));
        assert!(s.delta() > 0.0);
    }

    #[test]
    fn from_composite_error_mid_is_neutral() {
        let s = TrustSignal::from_composite_error(0.30);
        assert_eq!(s, TrustSignal::Neutral);
        assert_eq!(s.delta(), 0.0);
        assert!(!s.is_actionable());
    }

    #[test]
    fn from_composite_error_high_is_negative() {
        let s = TrustSignal::from_composite_error(0.80);
        assert!(matches!(s, TrustSignal::Negative { .. }));
        assert!(s.delta() < 0.0);
    }

    #[test]
    fn negative_magnitude_grows_with_error() {
        let mid = TrustSignal::from_composite_error(0.60).delta().abs();
        let max = TrustSignal::from_composite_error(1.00).delta().abs();
        assert!(max > mid);
        // Sanity bound — single-conv impact ≤ 0.10
        assert!(max <= 0.10 + f32::EPSILON);
    }

    #[test]
    fn positive_magnitude_grows_as_error_drops() {
        let near_neutral = TrustSignal::from_composite_error(0.19).delta();
        let near_zero = TrustSignal::from_composite_error(0.00).delta();
        assert!(near_zero >= near_neutral);
        assert!(near_zero <= 0.02 + f32::EPSILON);
    }

    #[test]
    fn signal_scaled_compound() {
        let s = TrustSignal::Negative { magnitude: 0.05 }.scaled(2.0);
        assert_eq!(s.delta(), -0.10);
        let s = TrustSignal::Positive { magnitude: 0.04 }.scaled(0.5);
        assert_eq!(s.delta(), 0.02);
        // Neutral stays neutral regardless of scale
        assert_eq!(TrustSignal::Neutral.scaled(99.0), TrustSignal::Neutral);
    }

    #[test]
    fn signal_scaled_clamps_negative_factor() {
        // Negative factor would flip polarity — guard rejects it.
        let s = TrustSignal::Negative { magnitude: 0.05 }.scaled(-2.0);
        assert_eq!(s.delta(), 0.0);
    }

    // ── CitationTracker ────────────────────────────────────────────

    #[test]
    fn tracker_record_and_drain() {
        let t = CitationTracker::new();
        t.record(cite("c1", "a.md"));
        t.record(cite("c1", "b.md"));
        t.record(cite("c2", "x.md"));

        let drained = t.drain("c1");
        assert_eq!(drained.len(), 2);
        let still = t.drain("c1");
        assert!(still.is_empty(), "drain is destructive");

        let c2 = t.drain("c2");
        assert_eq!(c2.len(), 1);
    }

    #[test]
    fn tracker_caps_per_conv() {
        let t = CitationTracker::new();
        for i in 0..(MAX_CITATIONS_PER_CONV + 10) {
            t.record(cite("c1", &format!("p{i}.md")));
        }
        assert_eq!(t.peek("c1").len(), MAX_CITATIONS_PER_CONV);
    }

    #[test]
    fn tracker_gc_drops_stale() {
        let t = CitationTracker::with_ttl(StdDuration::from_millis(50));
        t.record(cite("c1", "a.md"));
        std::thread::sleep(StdDuration::from_millis(80));
        t.record(cite("c2", "b.md"));

        let reaped = t.gc();
        assert_eq!(reaped, 1);
        assert!(t.peek("c1").is_empty());
        assert_eq!(t.peek("c2").len(), 1);
        assert_eq!(t.eviction_count(), 1);
    }

    #[test]
    fn tracker_gc_age_outs_keep_alive_attacker() {
        // Regression for R5 MUST-2: an adversary refreshing a bucket with a
        // single citation just before TTL expires used to hold the slot
        // forever. Bounded-time eviction reaps any bucket whose oldest
        // citation crossed 2 × TTL regardless of newer entries.
        let t = CitationTracker::with_ttl(StdDuration::from_millis(50));
        // First citation establishes the bucket.
        t.record(cite("attacker", "a.md"));
        // Sleep 70ms (stale by single TTL but inside 2×TTL = 100ms).
        std::thread::sleep(StdDuration::from_millis(70));
        // Refresh attempt: keep the bucket "fresh" by adding a citation.
        t.record(cite("attacker", "b.md"));
        // Now wait until first citation crosses 2 × TTL boundary.
        std::thread::sleep(StdDuration::from_millis(50));
        let reaped = t.gc();
        assert_eq!(reaped, 1, "aged-out bucket must be reaped");
        assert!(t.peek("attacker").is_empty());
    }

    #[test]
    fn tracker_record_many() {
        let t = CitationTracker::new();
        let citations = vec![
            cite("c1", "a.md"),
            cite("c1", "b.md"),
            cite("c2", "x.md"),
        ];
        t.record_many(citations);
        assert_eq!(t.peek("c1").len(), 2);
        assert_eq!(t.peek("c2").len(), 1);
        assert_eq!(t.conv_count(), 2);
    }
}
