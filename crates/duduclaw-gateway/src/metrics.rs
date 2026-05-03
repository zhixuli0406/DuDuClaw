//! Prometheus metrics exposition — `GET /metrics`.
//!
//! Lightweight implementation without the `prometheus` crate dependency.
//! Outputs metrics in Prometheus text exposition format.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::RwLock;

/// Global metrics registry.
static METRICS: std::sync::OnceLock<Arc<MetricsRegistry>> = std::sync::OnceLock::new();

/// Get or initialize the global metrics registry.
pub fn global_metrics() -> &'static Arc<MetricsRegistry> {
    METRICS.get_or_init(|| Arc::new(MetricsRegistry::new()))
}

/// Registry holding all Prometheus-compatible metrics.
pub struct MetricsRegistry {
    // Counters
    pub requests_total: AtomicU64,
    pub tokens_input_total: AtomicU64,
    pub tokens_output_total: AtomicU64,
    pub tokens_cache_read_total: AtomicU64,
    pub failover_total: AtomicU64,

    // Gauges (updated by snapshot)
    pub active_sessions: AtomicU64,
    channels_connected: RwLock<Vec<(String, bool)>>,
    budgets: RwLock<Vec<(String, u64)>>,

    // Histogram bins for request duration (ms buckets)
    pub duration_buckets: [AtomicU64; 8], // <100, <250, <500, <1000, <2500, <5000, <10000, +Inf
    pub duration_sum_ms: AtomicU64,

    // Wiki RL Trust Feedback (review BLOCKER R4 m12 + R5 MUST-1).
    // `eviction_total` and `active_conversations` are read live from the
    // tracker at render time — no atomic needed in the registry.
    pub wiki_trust_signals_applied_total: AtomicU64,
    pub wiki_trust_signals_dropped_capped_total: AtomicU64,
    pub wiki_trust_signals_dropped_locked_total: AtomicU64,
    pub wiki_trust_signals_dropped_daily_limit_total: AtomicU64,
    pub wiki_trust_archive_total: AtomicU64,
    pub wiki_trust_recovery_total: AtomicU64,
    pub wiki_trust_federation_partial_total: AtomicU64,
}

const DURATION_BOUNDS_MS: [u64; 7] = [100, 250, 500, 1000, 2500, 5000, 10000];

impl MetricsRegistry {
    fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            tokens_input_total: AtomicU64::new(0),
            tokens_output_total: AtomicU64::new(0),
            tokens_cache_read_total: AtomicU64::new(0),
            failover_total: AtomicU64::new(0),
            active_sessions: AtomicU64::new(0),
            channels_connected: RwLock::new(Vec::new()),
            budgets: RwLock::new(Vec::new()),
            duration_buckets: Default::default(),
            duration_sum_ms: AtomicU64::new(0),
            wiki_trust_signals_applied_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_capped_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_locked_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_daily_limit_total: AtomicU64::new(0),
            wiki_trust_archive_total: AtomicU64::new(0),
            wiki_trust_recovery_total: AtomicU64::new(0),
            wiki_trust_federation_partial_total: AtomicU64::new(0),
        }
    }

    // ── Wiki RL Trust Feedback (review BLOCKER R4 m12) ──────────────

    pub fn wiki_trust_signal_applied(&self) {
        self.wiki_trust_signals_applied_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_capped(&self) {
        self.wiki_trust_signals_dropped_capped_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_locked(&self) {
        self.wiki_trust_signals_dropped_locked_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_daily_limit(&self) {
        self.wiki_trust_signals_dropped_daily_limit_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_archive(&self) {
        self.wiki_trust_archive_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_recovery(&self) {
        self.wiki_trust_recovery_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_federation_partial(&self) {
        self.wiki_trust_federation_partial_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a completed request with duration and token counts.
    pub fn record_request(&self, duration_ms: u64, input_tokens: u64, output_tokens: u64, cache_read: u64) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.tokens_input_total.fetch_add(input_tokens, Ordering::Relaxed);
        self.tokens_output_total.fetch_add(output_tokens, Ordering::Relaxed);
        self.tokens_cache_read_total.fetch_add(cache_read, Ordering::Relaxed);
        self.duration_sum_ms.fetch_add(duration_ms, Ordering::Relaxed);

        // Find the right bucket
        for (i, &bound) in DURATION_BOUNDS_MS.iter().enumerate() {
            if duration_ms < bound {
                self.duration_buckets[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        // +Inf bucket
        self.duration_buckets[7].fetch_add(1, Ordering::Relaxed);
    }

    /// Record a failover event.
    pub fn record_failover(&self) {
        self.failover_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Update channel connection status snapshot.
    pub async fn update_channels(&self, channels: Vec<(String, bool)>) {
        *self.channels_connected.write().await = channels;
    }

    /// Update budget remaining snapshot.
    pub async fn update_budgets(&self, budgets: Vec<(String, u64)>) {
        *self.budgets.write().await = budgets;
    }

    /// Render all metrics in Prometheus text exposition format.
    pub async fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        // Counters
        out.push_str("# HELP duduclaw_requests_total Total number of AI requests.\n");
        out.push_str("# TYPE duduclaw_requests_total counter\n");
        out.push_str(&format!("duduclaw_requests_total {}\n", self.requests_total.load(Ordering::Relaxed)));

        out.push_str("# HELP duduclaw_tokens_total Total tokens by type.\n");
        out.push_str("# TYPE duduclaw_tokens_total counter\n");
        out.push_str(&format!("duduclaw_tokens_total{{type=\"input\"}} {}\n", self.tokens_input_total.load(Ordering::Relaxed)));
        out.push_str(&format!("duduclaw_tokens_total{{type=\"output\"}} {}\n", self.tokens_output_total.load(Ordering::Relaxed)));
        out.push_str(&format!("duduclaw_tokens_total{{type=\"cache_read\"}} {}\n", self.tokens_cache_read_total.load(Ordering::Relaxed)));

        out.push_str("# HELP duduclaw_failover_total Total failover events.\n");
        out.push_str("# TYPE duduclaw_failover_total counter\n");
        out.push_str(&format!("duduclaw_failover_total {}\n", self.failover_total.load(Ordering::Relaxed)));

        // Histogram
        out.push_str("# HELP duduclaw_request_duration_seconds Request duration in seconds.\n");
        out.push_str("# TYPE duduclaw_request_duration_seconds histogram\n");
        let mut cumulative: u64 = 0;
        for (i, &bound) in DURATION_BOUNDS_MS.iter().enumerate() {
            cumulative += self.duration_buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "duduclaw_request_duration_seconds_bucket{{le=\"{:.3}\"}} {}\n",
                bound as f64 / 1000.0,
                cumulative
            ));
        }
        cumulative += self.duration_buckets[7].load(Ordering::Relaxed);
        out.push_str(&format!("duduclaw_request_duration_seconds_bucket{{le=\"+Inf\"}} {cumulative}\n"));
        out.push_str(&format!(
            "duduclaw_request_duration_seconds_sum {:.3}\n",
            self.duration_sum_ms.load(Ordering::Relaxed) as f64 / 1000.0
        ));
        out.push_str(&format!("duduclaw_request_duration_seconds_count {cumulative}\n"));

        // Gauges
        out.push_str("# HELP duduclaw_active_sessions Number of active sessions.\n");
        out.push_str("# TYPE duduclaw_active_sessions gauge\n");
        out.push_str(&format!("duduclaw_active_sessions {}\n", self.active_sessions.load(Ordering::Relaxed)));

        out.push_str("# HELP duduclaw_channel_connected Channel connection status (1=connected, 0=disconnected).\n");
        out.push_str("# TYPE duduclaw_channel_connected gauge\n");
        for (name, connected) in self.channels_connected.read().await.iter() {
            out.push_str(&format!(
                "duduclaw_channel_connected{{channel=\"{name}\"}} {}\n",
                if *connected { 1 } else { 0 }
            ));
        }

        out.push_str("# HELP duduclaw_budget_remaining_cents Remaining budget in cents per account.\n");
        out.push_str("# TYPE duduclaw_budget_remaining_cents gauge\n");
        for (account, cents) in self.budgets.read().await.iter() {
            out.push_str(&format!("duduclaw_budget_remaining_cents{{account=\"{account}\"}} {cents}\n"));
        }

        // ── Wiki RL Trust Feedback (review BLOCKER R4 m12) ──────
        out.push_str("# HELP wiki_trust_signals_applied_total Trust signals successfully applied.\n");
        out.push_str("# TYPE wiki_trust_signals_applied_total counter\n");
        out.push_str(&format!(
            "wiki_trust_signals_applied_total {}\n",
            self.wiki_trust_signals_applied_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_signals_dropped_total Trust signals dropped, by reason.\n");
        out.push_str("# TYPE wiki_trust_signals_dropped_total counter\n");
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"per_conv_cap\"}} {}\n",
            self.wiki_trust_signals_dropped_capped_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"locked\"}} {}\n",
            self.wiki_trust_signals_dropped_locked_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"daily_limit\"}} {}\n",
            self.wiki_trust_signals_dropped_daily_limit_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_eviction_total CitationTracker LRU + age evictions.\n");
        out.push_str("# TYPE wiki_trust_eviction_total counter\n");
        // Read live from the tracker (review R5 MUST-1b: previously this
        // was a dead atomic that never incremented).
        out.push_str(&format!(
            "wiki_trust_eviction_total {}\n",
            duduclaw_memory::feedback::global_tracker().eviction_count()
        ));
        out.push_str("# HELP wiki_trust_archive_total Wiki pages auto-archived (do_not_inject crossed threshold).\n");
        out.push_str("# TYPE wiki_trust_archive_total counter\n");
        out.push_str(&format!(
            "wiki_trust_archive_total {}\n",
            self.wiki_trust_archive_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_recovery_total Wiki pages recovered from quarantine.\n");
        out.push_str("# TYPE wiki_trust_recovery_total counter\n");
        out.push_str(&format!(
            "wiki_trust_recovery_total {}\n",
            self.wiki_trust_recovery_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_federation_partial_total Federation pushes where receiver applied < sent.\n");
        out.push_str("# TYPE wiki_trust_federation_partial_total counter\n");
        out.push_str(&format!(
            "wiki_trust_federation_partial_total {}\n",
            self.wiki_trust_federation_partial_total.load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_active_conversations CitationTracker bucket count.\n");
        out.push_str("# TYPE wiki_trust_active_conversations gauge\n");
        // Read live (review R5 MUST-1c: previously a dead atomic gauge).
        out.push_str(&format!(
            "wiki_trust_active_conversations {}\n",
            duduclaw_memory::feedback::global_tracker().conv_count()
        ));

        out
    }
}

/// Axum handler for `GET /metrics`.
///
/// Restricted to localhost-only access to prevent exposing internal metrics
/// (token costs, agent names, session counts) to external networks.
/// Requires the router to be served with `into_make_service_with_connect_info::<SocketAddr>()`.
pub async fn metrics_handler(
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    if !peer.ip().is_loopback() {
        return (StatusCode::FORBIDDEN, "Metrics only available from localhost").into_response();
    }

    let metrics = global_metrics();
    let body = metrics.render().await;

    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_record_and_render() {
        let registry = MetricsRegistry::new();
        registry.record_request(150, 1000, 500, 800);
        registry.record_request(3000, 2000, 1000, 1500);
        registry.update_channels(vec![
            ("telegram".to_string(), true),
            ("discord".to_string(), false),
        ]).await;

        let output = registry.render().await;
        assert!(output.contains("duduclaw_requests_total 2"));
        assert!(output.contains("duduclaw_tokens_total{type=\"input\"} 3000"));
        assert!(output.contains("duduclaw_channel_connected{channel=\"telegram\"} 1"));
        assert!(output.contains("duduclaw_channel_connected{channel=\"discord\"} 0"));
    }

    #[tokio::test]
    async fn test_histogram_buckets() {
        let registry = MetricsRegistry::new();
        registry.record_request(50, 0, 0, 0);   // <100ms bucket
        registry.record_request(200, 0, 0, 0);  // <250ms bucket
        registry.record_request(15000, 0, 0, 0); // +Inf bucket

        let output = registry.render().await;
        assert!(output.contains("le=\"0.100\"} 1"));
        assert!(output.contains("le=\"0.250\"} 2")); // cumulative
        assert!(output.contains("le=\"+Inf\"} 3"));
    }
}
