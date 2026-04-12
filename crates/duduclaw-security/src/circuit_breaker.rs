//! Circuit breaker with behavioral anomaly detection.
//!
//! Detects bot loops, frequency anomalies, content repetition, and token
//! explosion. Inspired by [NeuralTrust circuit breakers](https://neuraltrust.ai/blog/circuit-breakers)
//! and [Gray Swan AI (arxiv:2406.04313)](https://arxiv.org/pdf/2406.04313).
//!
//! Key insight: traditional circuit breakers only catch HTTP errors.
//! LLM agents can fail with status 200 + valid JSON + confident hallucinations.
//! This implementation detects *behavioral* anomalies.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::killswitch::CircuitBreakerConfig;

/// Circuit breaker state (three-state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Normal operation — all requests pass through.
    Closed,
    /// Probing — limited requests allowed to test recovery.
    HalfOpen,
    /// Tripped — all requests blocked until cooldown.
    Open,
}

/// Reason the circuit breaker tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TripReason {
    /// Too many replies in a short window.
    FrequencyAnomaly { count: u32, window_secs: u64 },
    /// Consecutive replies are too similar (possible loop).
    ContentRepetition { similarity: String },
    /// Token count spiked relative to rolling average.
    TokenExplosion { current: usize, average: usize },
    /// Bot's own output appeared as the next input (echo/loop).
    EchoDetected,
}

impl std::fmt::Display for TripReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TripReason::FrequencyAnomaly { count, window_secs } => {
                write!(f, "frequency anomaly: {count} replies in {window_secs}s")
            }
            TripReason::ContentRepetition { similarity } => {
                write!(f, "content repetition (similarity: {similarity})")
            }
            TripReason::TokenExplosion { current, average } => {
                write!(f, "token explosion: {current} tokens vs {average} avg")
            }
            TripReason::EchoDetected => write!(f, "echo detected (bot loop)"),
        }
    }
}

/// Decision returned by the circuit breaker check.
#[derive(Debug, Clone)]
pub enum BreakerDecision {
    /// Request is allowed.
    Allow,
    /// Request is allowed but should be throttled (half-open probing).
    Throttle,
    /// Request is denied — breaker is open.
    Deny(BreakerState),
    /// Breaker just tripped — escalate to failsafe.
    Trip(TripReason),
}

/// Compute a fast hash of a string for similarity comparison.
fn text_hash(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Compute byte-level bigram Jaccard similarity between two strings.
/// Returns 0.0–1.0. Fast and allocation-light.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() < 2 || b_bytes.len() < 2 {
        return if a == b { 1.0 } else { 0.0 };
    }

    // Use a fixed-size array as a bloom-filter-like structure for bigrams
    // This is approximate but very fast and allocation-free
    let mut a_set = [0u8; 256];
    let mut b_set = [0u8; 256];

    for window in a_bytes.windows(2) {
        let idx = ((window[0] as usize) ^ (window[1] as usize)) & 0xFF;
        a_set[idx] = 1;
    }
    for window in b_bytes.windows(2) {
        let idx = ((window[0] as usize) ^ (window[1] as usize)) & 0xFF;
        b_set[idx] = 1;
    }

    let mut intersection = 0u32;
    let mut union = 0u32;
    for i in 0..256 {
        if a_set[i] == 1 || b_set[i] == 1 {
            union += 1;
        }
        if a_set[i] == 1 && b_set[i] == 1 {
            intersection += 1;
        }
    }

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Per-scope circuit breaker instance.
#[derive(Debug)]
pub struct CircuitBreaker {
    state: BreakerState,
    config: CircuitBreakerConfig,

    /// Timestamps of recent inbound messages (for frequency detection).
    inbound_timestamps: VecDeque<Instant>,
    /// Hashes of recent inbound messages (for repetition detection).
    recent_inbound_hashes: VecDeque<u64>,
    /// Raw text of the last few inbound messages (for similarity).
    recent_inbound_texts: VecDeque<String>,
    /// Token counts of recent replies (for explosion detection).
    recent_token_counts: VecDeque<usize>,
    /// Hash of the last outbound (bot reply) message.
    last_outbound_hash: Option<u64>,
    /// Prefix of last outbound text for secondary echo verification.
    /// Stored to avoid false positives from hash collisions.
    last_outbound_prefix: Option<String>,

    /// When the breaker tripped (for cooldown calculation).
    tripped_at: Option<Instant>,
    /// Reason for the last trip.
    trip_reason: Option<TripReason>,
    /// Number of requests allowed through in half-open state.
    half_open_passes: u32,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given config.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: BreakerState::Closed,
            config,
            inbound_timestamps: VecDeque::with_capacity(64),
            recent_inbound_hashes: VecDeque::with_capacity(8),
            recent_inbound_texts: VecDeque::with_capacity(4),
            recent_token_counts: VecDeque::with_capacity(32),
            last_outbound_hash: None,
            last_outbound_prefix: None,
            tripped_at: None,
            trip_reason: None,
            half_open_passes: 0,
        }
    }

    /// Current breaker state.
    pub fn state(&self) -> BreakerState {
        self.state
    }

    /// The reason for the last trip, if any.
    pub fn trip_reason(&self) -> Option<&TripReason> {
        self.trip_reason.as_ref()
    }

    /// Record an outbound (bot reply) message for echo detection.
    pub fn record_outbound(&mut self, text: &str, token_count: usize) {
        self.last_outbound_hash = Some(text_hash(text));
        // Store a prefix (up to 200 chars) for secondary echo verification,
        // avoiding false positives from hash collisions.
        self.last_outbound_prefix = Some(text.chars().take(200).collect());
        self.recent_token_counts.push_back(token_count);
        if self.recent_token_counts.len() > 32 {
            self.recent_token_counts.pop_front();
        }
    }

    /// Check an inbound message and return the breaker's decision.
    ///
    /// Call this before processing a user message. If the decision is
    /// `Trip`, the caller should escalate to the failsafe manager.
    pub fn check_inbound(&mut self, text: &str) -> BreakerDecision {
        let now = Instant::now();

        // If open, check if cooldown has elapsed → transition to half-open
        if self.state == BreakerState::Open {
            if let Some(tripped) = self.tripped_at {
                let cooldown = Duration::from_secs(self.config.cooldown_secs);
                if now.duration_since(tripped) >= cooldown {
                    self.state = BreakerState::HalfOpen;
                    self.half_open_passes = 0;
                    info!("Circuit breaker transitioning to HalfOpen after cooldown");
                } else {
                    return BreakerDecision::Deny(BreakerState::Open);
                }
            } else {
                return BreakerDecision::Deny(BreakerState::Open);
            }
        }

        // If half-open, allow limited probing requests — but still check for echo
        // to catch bot loops that persist through the probing phase.
        if self.state == BreakerState::HalfOpen {
            // Echo check even during probing — bot loops must not survive half-open
            let hash = text_hash(text);
            if let Some(outbound_hash) = self.last_outbound_hash
                && hash == outbound_hash && !text.is_empty() {
                    let prefix: String = text.chars().take(200).collect();
                    let prefix_match = self
                        .last_outbound_prefix
                        .as_ref()
                        .is_some_and(|p| *p == prefix);
                    if prefix_match {
                        // Bot loop still active — re-trip immediately
                        let reason = TripReason::EchoDetected;
                        self.trip(reason.clone());
                        return BreakerDecision::Trip(reason);
                    }
                }

            if self.half_open_passes >= self.config.half_open_allow_count {
                // Probing succeeded — close the breaker
                self.reset();
                info!("Circuit breaker recovered — closing");
            } else {
                self.half_open_passes += 1;
                self.record_inbound_internal(text, now);
                return BreakerDecision::Throttle;
            }
        }

        // ── Closed state: run anomaly detection ──

        self.record_inbound_internal(text, now);
        let hash = text_hash(text);

        // Check 1: Echo detection (highest priority, immediate trip)
        // Uses hash for fast path + prefix comparison to avoid false positives.
        if let Some(outbound_hash) = self.last_outbound_hash
            && hash == outbound_hash && !text.is_empty() {
                // Secondary check: verify the prefix actually matches to
                // rule out hash collisions (DefaultHasher is not collision-resistant).
                let prefix: String = text.chars().take(200).collect();
                let prefix_match = self
                    .last_outbound_prefix
                    .as_ref()
                    .is_some_and(|p| *p == prefix);
                if prefix_match {
                    let reason = TripReason::EchoDetected;
                    self.trip(reason.clone());
                    return BreakerDecision::Trip(reason);
                }
            }

        // Check 2: Frequency anomaly
        let window = Duration::from_secs(self.config.frequency_window_secs);
        let cutoff = now - window;
        self.inbound_timestamps.retain(|t| *t > cutoff);
        if self.inbound_timestamps.len() > self.config.frequency_max_replies as usize {
            let reason = TripReason::FrequencyAnomaly {
                count: self.inbound_timestamps.len() as u32,
                window_secs: self.config.frequency_window_secs,
            };
            self.trip(reason.clone());
            return BreakerDecision::Trip(reason);
        }

        // Check 3: Content repetition
        if self.recent_inbound_texts.len() >= 3 {
            let texts: Vec<&str> = self.recent_inbound_texts.iter().map(|s| s.as_str()).collect();
            let len = texts.len();
            // Compare last 3 messages pairwise
            let sim_1_2 = if self.config.similarity_threshold >= 1.0 {
                if texts[len - 1] == texts[len - 2] { 1.0 } else { 0.0 }
            } else {
                jaccard_similarity(texts[len - 1], texts[len - 2])
            };
            let sim_2_3 = if self.config.similarity_threshold >= 1.0 {
                if texts[len - 2] == texts[len - 3] { 1.0 } else { 0.0 }
            } else {
                jaccard_similarity(texts[len - 2], texts[len - 3])
            };

            if sim_1_2 >= self.config.similarity_threshold
                && sim_2_3 >= self.config.similarity_threshold
            {
                let reason = TripReason::ContentRepetition {
                    similarity: format!("{:.2}/{:.2}", sim_1_2, sim_2_3),
                };
                self.trip(reason.clone());
                return BreakerDecision::Trip(reason);
            }
        }

        // Check 4: Token explosion (only if we have enough history)
        if self.recent_token_counts.len() >= 5 {
            let avg: usize =
                self.recent_token_counts.iter().sum::<usize>() / self.recent_token_counts.len();
            if let Some(&last) = self.recent_token_counts.back()
                && avg > 0
                    && last as f64 > avg as f64 * self.config.token_explosion_multiplier
                {
                    let reason = TripReason::TokenExplosion {
                        current: last,
                        average: avg,
                    };
                    self.trip(reason.clone());
                    return BreakerDecision::Trip(reason);
                }
        }

        BreakerDecision::Allow
    }

    /// Trip the circuit breaker (Closed → Open).
    fn trip(&mut self, reason: TripReason) {
        warn!("Circuit breaker TRIPPED: {reason}");
        self.state = BreakerState::Open;
        self.tripped_at = Some(Instant::now());
        self.trip_reason = Some(reason);
    }

    /// Force-reset the circuit breaker to Closed state.
    ///
    /// Clears all sliding window data to prevent stale metrics from
    /// immediately re-triggering the breaker after recovery.
    pub fn reset(&mut self) {
        self.state = BreakerState::Closed;
        self.tripped_at = None;
        self.trip_reason = None;
        self.half_open_passes = 0;
        // Clear sliding window data so stale metrics don't re-trip immediately
        self.inbound_timestamps.clear();
        self.recent_inbound_hashes.clear();
        self.recent_inbound_texts.clear();
        // Keep recent_token_counts — the rolling average is still valid
        // Keep last_outbound_hash — echo detection should remain active
    }

    /// Record inbound message metadata.
    fn record_inbound_internal(&mut self, text: &str, now: Instant) {
        self.inbound_timestamps.push_back(now);
        // Cap at 64 entries to prevent unbounded memory growth on long-lived scopes.
        // The frequency check already prunes by window, but this is a hard safety net.
        if self.inbound_timestamps.len() > 64 {
            self.inbound_timestamps.pop_front();
        }

        let hash = text_hash(text);
        self.recent_inbound_hashes.push_back(hash);
        if self.recent_inbound_hashes.len() > 8 {
            self.recent_inbound_hashes.pop_front();
        }

        self.recent_inbound_texts.push_back(text.to_string());
        if self.recent_inbound_texts.len() > 4 {
            self.recent_inbound_texts.pop_front();
        }
    }
}

/// Registry managing per-scope circuit breakers.
pub struct CircuitBreakerRegistry {
    breakers: Arc<RwLock<HashMap<String, CircuitBreaker>>>,
    config: CircuitBreakerConfig,
}

impl CircuitBreakerRegistry {
    /// Create a new registry with the given default config.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            breakers: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Get or create a circuit breaker for the given scope key.
    ///
    /// The scope key is typically `"channel:scope_id"` (e.g., `"telegram:12345"`).
    pub async fn check_inbound(&self, scope: &str, text: &str) -> BreakerDecision {
        let mut map = self.breakers.write().await;
        let breaker = map
            .entry(scope.to_string())
            .or_insert_with(|| CircuitBreaker::new(self.config.clone()));
        breaker.check_inbound(text)
    }

    /// Record an outbound message for echo detection.
    pub async fn record_outbound(&self, scope: &str, text: &str, token_count: usize) {
        let mut map = self.breakers.write().await;
        if let Some(breaker) = map.get_mut(scope) {
            breaker.record_outbound(text, token_count);
        }
    }

    /// Force-reset a specific scope's breaker.
    pub async fn reset(&self, scope: &str) {
        let mut map = self.breakers.write().await;
        if let Some(breaker) = map.get_mut(scope) {
            breaker.reset();
            info!(scope, "Circuit breaker manually reset");
        }
    }

    /// Get the state of a specific scope's breaker.
    pub async fn state(&self, scope: &str) -> BreakerState {
        let map = self.breakers.read().await;
        map.get(scope)
            .map(|b| b.state())
            .unwrap_or(BreakerState::Closed)
    }

    /// Get the trip reason for a specific scope.
    pub async fn trip_reason(&self, scope: &str) -> Option<TripReason> {
        let map = self.breakers.read().await;
        map.get(scope).and_then(|b| b.trip_reason().cloned())
    }

    /// Evict stale breakers (all Closed with no recent activity).
    pub async fn evict_stale(&self, max_age: Duration) {
        let mut map = self.breakers.write().await;
        let cutoff = Instant::now() - max_age;
        map.retain(|_, b| {
            b.state != BreakerState::Closed
                || b.inbound_timestamps.back().is_some_and(|t| *t > cutoff)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            frequency_window_secs: 10,
            frequency_max_replies: 3,
            similarity_threshold: 1.0, // exact match for testing
            token_explosion_multiplier: 3.0,
            cooldown_secs: 1, // short for tests
            half_open_allow_count: 1,
        }
    }

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::new(test_config());
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn allows_normal_traffic() {
        let mut cb = CircuitBreaker::new(test_config());
        assert!(matches!(cb.check_inbound("hello"), BreakerDecision::Allow));
        assert!(matches!(cb.check_inbound("world"), BreakerDecision::Allow));
        assert!(matches!(cb.check_inbound("test"), BreakerDecision::Allow));
    }

    #[test]
    fn trips_on_frequency() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.check_inbound("a");
        cb.check_inbound("b");
        cb.check_inbound("c");
        // 4th message exceeds limit of 3
        let decision = cb.check_inbound("d");
        assert!(matches!(decision, BreakerDecision::Trip(TripReason::FrequencyAnomaly { .. })));
        assert_eq!(cb.state(), BreakerState::Open);
    }

    #[test]
    fn trips_on_echo() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.record_outbound("bot reply", 10);
        let decision = cb.check_inbound("bot reply");
        assert!(matches!(decision, BreakerDecision::Trip(TripReason::EchoDetected)));
    }

    #[test]
    fn trips_on_repetition() {
        let mut config = test_config();
        config.frequency_max_replies = 100; // don't trip on frequency
        let mut cb = CircuitBreaker::new(config);
        cb.check_inbound("same message");
        cb.check_inbound("same message");
        let decision = cb.check_inbound("same message");
        assert!(matches!(
            decision,
            BreakerDecision::Trip(TripReason::ContentRepetition { .. })
        ));
    }

    #[test]
    fn trips_on_token_explosion() {
        let mut config = test_config();
        config.frequency_max_replies = 100;
        let mut cb = CircuitBreaker::new(config);
        // Build up a baseline of small token counts
        for i in 0..6 {
            cb.record_outbound(&format!("msg{i}"), 100);
            cb.check_inbound(&format!("input{i}"));
        }
        // Now record a huge token count
        cb.record_outbound("huge reply", 1000);
        // Next inbound should detect the explosion
        let decision = cb.check_inbound("trigger");
        assert!(matches!(
            decision,
            BreakerDecision::Trip(TripReason::TokenExplosion { .. })
        ));
    }

    #[test]
    fn denies_when_open() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.trip(TripReason::EchoDetected);
        assert_eq!(cb.state(), BreakerState::Open);
        let decision = cb.check_inbound("anything");
        assert!(matches!(decision, BreakerDecision::Deny(BreakerState::Open)));
    }

    #[test]
    fn transitions_to_half_open_after_cooldown() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.state = BreakerState::Open;
        cb.tripped_at = Some(Instant::now() - Duration::from_secs(2)); // past cooldown
        let decision = cb.check_inbound("probe");
        assert!(matches!(decision, BreakerDecision::Throttle));
        assert_eq!(cb.state(), BreakerState::HalfOpen);
    }

    #[test]
    fn reset_closes_breaker() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.trip(TripReason::EchoDetected);
        cb.reset();
        assert_eq!(cb.state(), BreakerState::Closed);
        assert!(cb.trip_reason().is_none());
    }

    #[test]
    fn jaccard_identical() {
        assert_eq!(jaccard_similarity("hello world", "hello world"), 1.0);
    }

    #[test]
    fn jaccard_different() {
        let sim = jaccard_similarity("hello world", "xyz abc 123");
        assert!(sim < 0.5);
    }

    #[test]
    fn jaccard_empty() {
        assert_eq!(jaccard_similarity("", ""), 1.0);
        assert_eq!(jaccard_similarity("abc", ""), 0.0);
    }

    #[test]
    fn half_open_echo_retrips() {
        let mut config = test_config();
        config.frequency_max_replies = 100; // only test echo
        let mut cb = CircuitBreaker::new(config);
        // Trip via echo
        cb.record_outbound("bot says hello", 10);
        cb.check_inbound("bot says hello"); // trips
        assert_eq!(cb.state(), BreakerState::Open);

        // Wait for cooldown → transition to HalfOpen
        cb.tripped_at = Some(Instant::now() - Duration::from_secs(2));
        // Send the echo again during HalfOpen probing
        let decision = cb.check_inbound("bot says hello");
        // Should re-trip, not throttle
        assert!(matches!(decision, BreakerDecision::Trip(TripReason::EchoDetected)));
        assert_eq!(cb.state(), BreakerState::Open);
    }

    #[test]
    fn half_open_non_echo_throttles() {
        let mut config = test_config();
        config.frequency_max_replies = 100;
        let mut cb = CircuitBreaker::new(config);
        cb.record_outbound("bot reply", 10);
        cb.check_inbound("bot reply"); // trips on echo
        assert_eq!(cb.state(), BreakerState::Open);

        cb.tripped_at = Some(Instant::now() - Duration::from_secs(2));
        // Send a NON-echo message during probing
        let decision = cb.check_inbound("different message");
        // Should throttle (allow through for probing)
        assert!(matches!(decision, BreakerDecision::Throttle));
        assert_eq!(cb.state(), BreakerState::HalfOpen);
    }

    #[test]
    fn half_open_to_closed_full_cycle() {
        let mut config = test_config();
        config.half_open_allow_count = 2;
        config.frequency_max_replies = 100;
        let mut cb = CircuitBreaker::new(config);
        // Trip manually
        cb.trip(TripReason::FrequencyAnomaly { count: 10, window_secs: 10 });
        assert_eq!(cb.state(), BreakerState::Open);

        // Cooldown elapsed
        cb.tripped_at = Some(Instant::now() - Duration::from_secs(2));

        // Probe 1: transitions Open → HalfOpen, returns Throttle
        let d1 = cb.check_inbound("probe1");
        assert!(matches!(d1, BreakerDecision::Throttle));
        assert_eq!(cb.state(), BreakerState::HalfOpen);

        // Probe 2: still HalfOpen, returns Throttle
        let d2 = cb.check_inbound("probe2");
        assert!(matches!(d2, BreakerDecision::Throttle));
        assert_eq!(cb.state(), BreakerState::HalfOpen);

        // Probe 3: passes >= allow_count, resets to Closed, runs anomaly checks
        let d3 = cb.check_inbound("probe3");
        assert!(matches!(d3, BreakerDecision::Allow));
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn reset_clears_sliding_windows() {
        let mut cb = CircuitBreaker::new(test_config());
        cb.check_inbound("a");
        cb.check_inbound("b");
        cb.check_inbound("c");
        assert!(!cb.inbound_timestamps.is_empty());
        assert!(!cb.recent_inbound_texts.is_empty());

        cb.reset();
        assert!(cb.inbound_timestamps.is_empty());
        assert!(cb.recent_inbound_texts.is_empty());
        assert!(cb.recent_inbound_hashes.is_empty());
        // token counts and outbound hash are preserved
        assert!(cb.last_outbound_hash.is_none()); // never set
    }

    #[tokio::test]
    async fn registry_creates_on_demand() {
        let reg = CircuitBreakerRegistry::new(test_config());
        let decision = reg.check_inbound("scope1", "hello").await;
        assert!(matches!(decision, BreakerDecision::Allow));
    }

    #[tokio::test]
    async fn registry_independent_scopes() {
        let reg = CircuitBreakerRegistry::new(test_config());
        reg.check_inbound("s1", "a").await;
        reg.check_inbound("s1", "b").await;
        reg.check_inbound("s1", "c").await;
        // s1 should trip
        let d1 = reg.check_inbound("s1", "d").await;
        assert!(matches!(d1, BreakerDecision::Trip(_)));
        // s2 should still be fine
        let d2 = reg.check_inbound("s2", "hello").await;
        assert!(matches!(d2, BreakerDecision::Allow));
    }
}
