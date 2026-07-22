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

    // ── PTY pool (Phase 8 production-rollout observability) ──────────
    /// Total `acquire_and_invoke` calls routed through PTY pool.
    pub pty_pool_acquires_total: AtomicU64,
    /// Of those, how many reused an existing pooled session.
    pub pty_pool_acquires_cache_hit_total: AtomicU64,
    /// Of those, how many spawned a fresh PtySession.
    pub pty_pool_acquires_spawn_total: AtomicU64,
    /// Sessions evicted by reason: idle, unhealthy, shutdown.
    pub pty_pool_evicted_idle_total: AtomicU64,
    pub pty_pool_evicted_unhealthy_total: AtomicU64,
    pub pty_pool_evicted_shutdown_total: AtomicU64,
    /// Invoke outcomes (success / empty_payload / error / timeout).
    pub pty_pool_invokes_ok_total: AtomicU64,
    pub pty_pool_invokes_empty_total: AtomicU64,
    pub pty_pool_invokes_error_total: AtomicU64,
    pub pty_pool_invokes_timeout_total: AtomicU64,
    /// Invoke duration histogram (ms). Bucket bounds shared with the
    /// main request histogram so the dashboard can reuse layouts.
    pub pty_pool_invoke_duration_buckets: [AtomicU64; 8],
    pub pty_pool_invoke_duration_sum_ms: AtomicU64,
    /// Worker subprocess health: counted in `worker_supervisor`.
    pub worker_health_misses_total: AtomicU64,
    pub worker_restarts_total: AtomicU64,
    /// Mode gauge — 0 = in-process, 1 = managed worker. Set at boot.
    pub pty_pool_managed_worker_active: AtomicU64,

    // ── Decision Continuity (RFC-24, Phase 3 observability) ──────────
    /// Decisions captured from outbound enumerated choices.
    pub decision_captured_total: AtomicU64,
    /// Decisions resolved to a chosen option (MCP or auto).
    pub decision_resolved_total: AtomicU64,
    /// Open decisions auto-expired by TTL.
    pub decision_expired_total: AtomicU64,
    /// Captured decisions manually dismissed as false positives (precision signal).
    pub decision_false_positive_total: AtomicU64,

    // ── WP5: cache-aware compression gate (2607.12161) ────────────────
    /// Compression stage runs, keyed by stage name (`turn_trim`,
    /// `drop_oldest_tool_echoes`, `bisect_and_summarize`). A HashMap
    /// (rather than fixed atomics) because the stage set is defined in
    /// `prompt_compression::default_pipeline` and this stays decoupled.
    pub prompt_compression_runs: RwLock<std::collections::HashMap<String, u64>>,
    /// Requests where the cache-aware guard skipped the pipeline entirely
    /// (cache already hot + mild overshoot — see `prompt_compression`).
    pub prompt_compression_skipped_cache_guard_total: AtomicU64,
    /// Requests flagged as a likely cache-break: compressed, and cache
    /// efficiency cratered right after a previously-healthy row for the
    /// same agent.
    pub prompt_compression_cache_break_suspect_total: AtomicU64,
}

const DURATION_BOUNDS_MS: [u64; 7] = [100, 250, 500, 1000, 2500, 5000, 10000];

/// Phase 8 — outcome label for a single PTY pool invoke. Mirrors the
/// labels emitted to Prometheus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyInvokeOutcome {
    Ok,
    EmptyPayload,
    Error,
    Timeout,
}

impl PtyInvokeOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::EmptyPayload => "empty_payload",
            Self::Error => "error",
            Self::Timeout => "timeout",
        }
    }
}

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

            // PTY pool (Phase 8)
            pty_pool_acquires_total: AtomicU64::new(0),
            pty_pool_acquires_cache_hit_total: AtomicU64::new(0),
            pty_pool_acquires_spawn_total: AtomicU64::new(0),
            pty_pool_evicted_idle_total: AtomicU64::new(0),
            pty_pool_evicted_unhealthy_total: AtomicU64::new(0),
            pty_pool_evicted_shutdown_total: AtomicU64::new(0),
            pty_pool_invokes_ok_total: AtomicU64::new(0),
            pty_pool_invokes_empty_total: AtomicU64::new(0),
            pty_pool_invokes_error_total: AtomicU64::new(0),
            pty_pool_invokes_timeout_total: AtomicU64::new(0),
            pty_pool_invoke_duration_buckets: Default::default(),
            pty_pool_invoke_duration_sum_ms: AtomicU64::new(0),
            worker_health_misses_total: AtomicU64::new(0),
            worker_restarts_total: AtomicU64::new(0),
            pty_pool_managed_worker_active: AtomicU64::new(0),
            decision_captured_total: AtomicU64::new(0),
            decision_resolved_total: AtomicU64::new(0),
            decision_expired_total: AtomicU64::new(0),
            decision_false_positive_total: AtomicU64::new(0),
            prompt_compression_runs: RwLock::new(std::collections::HashMap::new()),
            prompt_compression_skipped_cache_guard_total: AtomicU64::new(0),
            prompt_compression_cache_break_suspect_total: AtomicU64::new(0),
        }
    }

    // ── Decision Continuity helpers (RFC-24) ─────────────────────────
    pub fn decision_captured(&self) {
        self.decision_captured_total.fetch_add(1, Ordering::Relaxed);
    }
    pub fn decision_resolved(&self) {
        self.decision_resolved_total.fetch_add(1, Ordering::Relaxed);
    }
    /// Record `n` decisions expired by TTL (rows → decisions is approximate;
    /// callers pass the decision count they observed).
    pub fn decision_expired(&self, n: u64) {
        self.decision_expired_total.fetch_add(n, Ordering::Relaxed);
    }
    pub fn decision_false_positive(&self) {
        self.decision_false_positive_total
            .fetch_add(1, Ordering::Relaxed);
    }

    // ── WP5: cache-aware compression gate (2607.12161) ────────────────

    /// Record one compression stage run (`stage` matches the pipeline's
    /// static stage names, e.g. `"turn_trim"`).
    pub async fn prompt_compression_run(&self, stage: &str) {
        let mut map = self.prompt_compression_runs.write().await;
        *map.entry(stage.to_string()).or_insert(0) += 1;
    }

    /// Record a request where the cache-aware guard skipped the pipeline.
    pub fn prompt_compression_skipped_cache_guard(&self) {
        self.prompt_compression_skipped_cache_guard_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a suspected compression-induced cache break.
    pub fn prompt_compression_cache_break_suspect(&self) {
        self.prompt_compression_cache_break_suspect_total
            .fetch_add(1, Ordering::Relaxed);
    }

    // ── PTY pool helpers (Phase 8 production-rollout observability) ───

    pub fn pty_pool_acquire_cache_hit(&self) {
        self.pty_pool_acquires_total.fetch_add(1, Ordering::Relaxed);
        self.pty_pool_acquires_cache_hit_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn pty_pool_acquire_spawn(&self) {
        self.pty_pool_acquires_total.fetch_add(1, Ordering::Relaxed);
        self.pty_pool_acquires_spawn_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record an eviction. `reason` is one of `idle`, `unhealthy`, or
    /// `shutdown`; unknown reasons fall back to `shutdown`.
    pub fn pty_pool_evict(&self, reason: &str) {
        let counter = match reason {
            "idle" => &self.pty_pool_evicted_idle_total,
            "unhealthy" => &self.pty_pool_evicted_unhealthy_total,
            _ => &self.pty_pool_evicted_shutdown_total,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one invoke outcome.
    pub fn pty_pool_invoke_complete(&self, duration_ms: u64, outcome: PtyInvokeOutcome) {
        match outcome {
            PtyInvokeOutcome::Ok => self.pty_pool_invokes_ok_total.fetch_add(1, Ordering::Relaxed),
            PtyInvokeOutcome::EmptyPayload => {
                self.pty_pool_invokes_empty_total.fetch_add(1, Ordering::Relaxed)
            }
            PtyInvokeOutcome::Error => {
                self.pty_pool_invokes_error_total.fetch_add(1, Ordering::Relaxed)
            }
            PtyInvokeOutcome::Timeout => self
                .pty_pool_invokes_timeout_total
                .fetch_add(1, Ordering::Relaxed),
        };
        self.pty_pool_invoke_duration_sum_ms
            .fetch_add(duration_ms, Ordering::Relaxed);
        for (i, &bound) in DURATION_BOUNDS_MS.iter().enumerate() {
            if duration_ms < bound {
                self.pty_pool_invoke_duration_buckets[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.pty_pool_invoke_duration_buckets[7].fetch_add(1, Ordering::Relaxed);
    }

    pub fn worker_health_miss(&self) {
        self.worker_health_misses_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn worker_restart(&self) {
        self.worker_restarts_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the managed-worker gauge at boot. `active=true` indicates
    /// the gateway is routing PtyPool calls through the subprocess.
    pub fn set_managed_worker_active(&self, active: bool) {
        self.pty_pool_managed_worker_active
            .store(if active { 1 } else { 0 }, Ordering::Relaxed);
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

        // ── PTY pool (Phase 8 production-rollout observability) ──────
        out.push_str(
            "# HELP duduclaw_pty_pool_acquires_total Total acquire calls on the PTY pool, by outcome.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_acquires_total counter\n");
        out.push_str(&format!(
            "duduclaw_pty_pool_acquires_total{{outcome=\"cache_hit\"}} {}\n",
            self.pty_pool_acquires_cache_hit_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_acquires_total{{outcome=\"spawn\"}} {}\n",
            self.pty_pool_acquires_spawn_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_acquires_total{{outcome=\"all\"}} {}\n",
            self.pty_pool_acquires_total.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP duduclaw_pty_pool_evicted_total Total pool evictions by reason.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_evicted_total counter\n");
        out.push_str(&format!(
            "duduclaw_pty_pool_evicted_total{{reason=\"idle\"}} {}\n",
            self.pty_pool_evicted_idle_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_evicted_total{{reason=\"unhealthy\"}} {}\n",
            self.pty_pool_evicted_unhealthy_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_evicted_total{{reason=\"shutdown\"}} {}\n",
            self.pty_pool_evicted_shutdown_total.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP duduclaw_pty_pool_invokes_total Total PTY pool invokes by outcome.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_invokes_total counter\n");
        out.push_str(&format!(
            "duduclaw_pty_pool_invokes_total{{outcome=\"ok\"}} {}\n",
            self.pty_pool_invokes_ok_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_invokes_total{{outcome=\"empty_payload\"}} {}\n",
            self.pty_pool_invokes_empty_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_invokes_total{{outcome=\"error\"}} {}\n",
            self.pty_pool_invokes_error_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_invokes_total{{outcome=\"timeout\"}} {}\n",
            self.pty_pool_invokes_timeout_total.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP duduclaw_pty_pool_invoke_duration_seconds Invoke duration in seconds.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_invoke_duration_seconds histogram\n");
        let mut cumulative: u64 = 0;
        for (i, &bound) in DURATION_BOUNDS_MS.iter().enumerate() {
            cumulative += self.pty_pool_invoke_duration_buckets[i].load(Ordering::Relaxed);
            out.push_str(&format!(
                "duduclaw_pty_pool_invoke_duration_seconds_bucket{{le=\"{:.3}\"}} {}\n",
                bound as f64 / 1000.0,
                cumulative
            ));
        }
        cumulative += self.pty_pool_invoke_duration_buckets[7].load(Ordering::Relaxed);
        out.push_str(&format!(
            "duduclaw_pty_pool_invoke_duration_seconds_bucket{{le=\"+Inf\"}} {cumulative}\n"
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_invoke_duration_seconds_sum {:.3}\n",
            self.pty_pool_invoke_duration_sum_ms.load(Ordering::Relaxed) as f64 / 1000.0
        ));
        out.push_str(&format!(
            "duduclaw_pty_pool_invoke_duration_seconds_count {cumulative}\n"
        ));

        out.push_str(
            "# HELP duduclaw_pty_pool_sessions_active Currently cached PTY pool sessions.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_sessions_active gauge\n");
        out.push_str(&format!(
            "duduclaw_pty_pool_sessions_active {}\n",
            crate::pty_runtime::session_count()
        ));

        out.push_str(
            "# HELP duduclaw_pty_pool_managed_worker_active 1 when routing through subprocess worker, 0 in-process.\n",
        );
        out.push_str("# TYPE duduclaw_pty_pool_managed_worker_active gauge\n");
        out.push_str(&format!(
            "duduclaw_pty_pool_managed_worker_active {}\n",
            self.pty_pool_managed_worker_active.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP duduclaw_worker_health_misses_total Cumulative worker /healthz miss count.\n",
        );
        out.push_str("# TYPE duduclaw_worker_health_misses_total counter\n");
        out.push_str(&format!(
            "duduclaw_worker_health_misses_total {}\n",
            self.worker_health_misses_total.load(Ordering::Relaxed)
        ));

        out.push_str(
            "# HELP duduclaw_worker_restarts_total Cumulative worker subprocess restarts.\n",
        );
        out.push_str("# TYPE duduclaw_worker_restarts_total counter\n");
        out.push_str(&format!(
            "duduclaw_worker_restarts_total {}\n",
            self.worker_restarts_total.load(Ordering::Relaxed)
        ));

        // ── Decision Continuity (RFC-24) ──
        out.push_str(
            "# HELP duduclaw_decision_captured_total Decisions captured from outbound enumerated choices.\n",
        );
        out.push_str("# TYPE duduclaw_decision_captured_total counter\n");
        out.push_str(&format!(
            "duduclaw_decision_captured_total {}\n",
            self.decision_captured_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP duduclaw_decision_resolved_total Decisions resolved to a chosen option.\n",
        );
        out.push_str("# TYPE duduclaw_decision_resolved_total counter\n");
        out.push_str(&format!(
            "duduclaw_decision_resolved_total {}\n",
            self.decision_resolved_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP duduclaw_decision_expired_total Open decisions auto-expired by TTL.\n",
        );
        out.push_str("# TYPE duduclaw_decision_expired_total counter\n");
        out.push_str(&format!(
            "duduclaw_decision_expired_total {}\n",
            self.decision_expired_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP duduclaw_decision_false_positive_total Captured decisions dismissed as false positives.\n",
        );
        out.push_str("# TYPE duduclaw_decision_false_positive_total counter\n");
        out.push_str(&format!(
            "duduclaw_decision_false_positive_total {}\n",
            self.decision_false_positive_total.load(Ordering::Relaxed)
        ));

        // ── WP5: cache-aware compression gate (2607.12161) ──
        out.push_str(
            "# HELP prompt_compression_runs_total Compression pipeline stage runs, by stage.\n",
        );
        out.push_str("# TYPE prompt_compression_runs_total counter\n");
        for (stage, count) in self.prompt_compression_runs.read().await.iter() {
            out.push_str(&format!(
                "prompt_compression_runs_total{{stage=\"{stage}\"}} {count}\n"
            ));
        }
        out.push_str(
            "# HELP prompt_compression_skipped_cache_guard_total Requests where the cache-aware guard skipped compression.\n",
        );
        out.push_str("# TYPE prompt_compression_skipped_cache_guard_total counter\n");
        out.push_str(&format!(
            "prompt_compression_skipped_cache_guard_total {}\n",
            self.prompt_compression_skipped_cache_guard_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP prompt_compression_cache_break_suspect_total Requests where compression likely broke a healthy cache prefix.\n",
        );
        out.push_str("# TYPE prompt_compression_cache_break_suspect_total counter\n");
        out.push_str(&format!(
            "prompt_compression_cache_break_suspect_total {}\n",
            self.prompt_compression_cache_break_suspect_total.load(Ordering::Relaxed)
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
    let mut body = metrics.render().await;
    // RFC-26: fork metrics live in the cross-process ForkStore (forks run in the
    // MCP-server process). Read them at scrape time and append.
    body.push_str(&render_fork_metrics());

    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Render RFC-26 fork metrics from the shared `ForkStore` (`$DUDUCLAW_HOME/fork_store.db`).
/// Returns an empty string when the store is absent/unreadable (forking off).
pub fn render_fork_metrics() -> String {
    let home = match std::env::var_os("DUDUCLAW_HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => return String::new(),
    };
    render_fork_metrics_from(&home.join("fork_store.db"))
}

/// Path-addressable variant for testing.
pub fn render_fork_metrics_from(path: &std::path::Path) -> String {
    if !path.exists() {
        return String::new();
    }
    let store = match duduclaw_fork::ForkStore::open(path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let m = match store.metrics() {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let mut out = String::new();
    let counter = |out: &mut String, name: &str, help: &str, v: u64| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"));
    };
    counter(&mut out, "duduclaw_fork_runs_total", "Total forks created.", m.forks_total);
    counter(&mut out, "duduclaw_fork_resolved_total", "Forks resolved to a winner.", m.forks_resolved);
    counter(&mut out, "duduclaw_fork_promoted_total", "Forks whose winner was promoted.", m.forks_promoted);
    counter(&mut out, "duduclaw_fork_branches_total", "Total branches across all forks.", m.branches_total);
    out.push_str("# HELP duduclaw_fork_branch_outcome Total branches by terminal outcome.\n");
    out.push_str("# TYPE duduclaw_fork_branch_outcome counter\n");
    out.push_str(&format!("duduclaw_fork_branch_outcome{{outcome=\"finished\"}} {}\n", m.branches_finished));
    out.push_str(&format!("duduclaw_fork_branch_outcome{{outcome=\"budget_killed\"}} {}\n", m.branches_budget_killed));
    out.push_str(&format!("duduclaw_fork_branch_outcome{{outcome=\"failed\"}} {}\n", m.branches_failed));
    out.push_str("# HELP duduclaw_fork_spend_usd_total Aggregate USD spent across all forks.\n");
    out.push_str("# TYPE duduclaw_fork_spend_usd_total counter\n");
    out.push_str(&format!("duduclaw_fork_spend_usd_total {:.6}\n", m.aggregate_spent_usd));
    out
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_metrics_absent_store_is_empty() {
        let p = std::path::Path::new("/nonexistent/duduclaw_fork_metrics_test.db");
        assert_eq!(render_fork_metrics_from(p), "");
    }

    #[test]
    fn fork_metrics_render_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fork_store.db");
        let store = duduclaw_fork::ForkStore::open(&path).unwrap();
        store
            .insert_fork(
                &duduclaw_fork::ForkRow {
                    fork_id: "f1".into(),
                    agent_id: "a1".into(),
                    prompt: "p".into(),
                    merge_mode: "auto".into(),
                    resolved: true,
                    winner: Some("b1".into()),
                    promoted: true,
                    aggregate_spent_usd: 0.25,
                    created_at: "2026-06-19T00:00:00Z".into(),
                },
                &[duduclaw_fork::BranchRow {
                    branch_id: "b1".into(),
                    fork_id: "f1".into(),
                    steering: None,
                    budget_usd: 0.5,
                    state: "finished".into(),
                    spent_usd: 0.25,
                    output: String::new(),
                    test_exit_code: None,
                }],
            )
            .unwrap();
        store.set_resolution("f1", Some("b1"), true, true, 0.25).unwrap();

        let out = render_fork_metrics_from(&path);
        assert!(out.contains("duduclaw_fork_runs_total 1"));
        assert!(out.contains("duduclaw_fork_promoted_total 1"));
        assert!(out.contains("duduclaw_fork_branch_outcome{outcome=\"finished\"} 1"));
        assert!(out.contains("duduclaw_fork_spend_usd_total 0.25"));
    }

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

    // ── Phase 8 PTY pool metric tests ─────────────────────────────────

    #[test]
    fn pty_invoke_outcome_as_str_round_trip() {
        assert_eq!(PtyInvokeOutcome::Ok.as_str(), "ok");
        assert_eq!(PtyInvokeOutcome::EmptyPayload.as_str(), "empty_payload");
        assert_eq!(PtyInvokeOutcome::Error.as_str(), "error");
        assert_eq!(PtyInvokeOutcome::Timeout.as_str(), "timeout");
    }

    #[test]
    fn pty_pool_acquire_counters_increment_independently() {
        let r = MetricsRegistry::new();
        r.pty_pool_acquire_cache_hit();
        r.pty_pool_acquire_cache_hit();
        r.pty_pool_acquire_spawn();
        assert_eq!(r.pty_pool_acquires_cache_hit_total.load(Ordering::Relaxed), 2);
        assert_eq!(r.pty_pool_acquires_spawn_total.load(Ordering::Relaxed), 1);
        assert_eq!(r.pty_pool_acquires_total.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn pty_pool_evict_by_reason_routes_to_correct_counter() {
        let r = MetricsRegistry::new();
        r.pty_pool_evict("idle");
        r.pty_pool_evict("idle");
        r.pty_pool_evict("unhealthy");
        r.pty_pool_evict("shutdown");
        r.pty_pool_evict("made_up_reason"); // falls back to shutdown
        assert_eq!(r.pty_pool_evicted_idle_total.load(Ordering::Relaxed), 2);
        assert_eq!(
            r.pty_pool_evicted_unhealthy_total.load(Ordering::Relaxed),
            1
        );
        assert_eq!(r.pty_pool_evicted_shutdown_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn pty_pool_invoke_complete_increments_outcome_counter() {
        let r = MetricsRegistry::new();
        r.pty_pool_invoke_complete(50, PtyInvokeOutcome::Ok);
        r.pty_pool_invoke_complete(300, PtyInvokeOutcome::EmptyPayload);
        r.pty_pool_invoke_complete(10_000, PtyInvokeOutcome::Error);
        r.pty_pool_invoke_complete(60_000, PtyInvokeOutcome::Timeout);
        assert_eq!(r.pty_pool_invokes_ok_total.load(Ordering::Relaxed), 1);
        assert_eq!(r.pty_pool_invokes_empty_total.load(Ordering::Relaxed), 1);
        assert_eq!(r.pty_pool_invokes_error_total.load(Ordering::Relaxed), 1);
        assert_eq!(r.pty_pool_invokes_timeout_total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pty_pool_invoke_complete_buckets_duration() {
        let r = MetricsRegistry::new();
        r.pty_pool_invoke_complete(50, PtyInvokeOutcome::Ok);
        r.pty_pool_invoke_complete(15_000, PtyInvokeOutcome::Ok);
        // 50 ms hits the <100 bucket; 15_000 ms hits the +Inf bucket.
        assert_eq!(
            r.pty_pool_invoke_duration_buckets[0].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            r.pty_pool_invoke_duration_buckets[7].load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            r.pty_pool_invoke_duration_sum_ms.load(Ordering::Relaxed),
            15_050
        );
    }

    #[test]
    fn worker_health_metrics_increment() {
        let r = MetricsRegistry::new();
        r.worker_health_miss();
        r.worker_health_miss();
        r.worker_restart();
        assert_eq!(r.worker_health_misses_total.load(Ordering::Relaxed), 2);
        assert_eq!(r.worker_restarts_total.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn managed_worker_active_gauge_toggles() {
        let r = MetricsRegistry::new();
        assert_eq!(
            r.pty_pool_managed_worker_active.load(Ordering::Relaxed),
            0,
            "should default off"
        );
        r.set_managed_worker_active(true);
        assert_eq!(
            r.pty_pool_managed_worker_active.load(Ordering::Relaxed),
            1
        );
        r.set_managed_worker_active(false);
        assert_eq!(
            r.pty_pool_managed_worker_active.load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn render_emits_pty_pool_metric_labels() {
        let registry = MetricsRegistry::new();
        registry.pty_pool_acquire_spawn();
        registry.pty_pool_evict("idle");
        registry.pty_pool_invoke_complete(150, PtyInvokeOutcome::Ok);
        registry.worker_restart();
        registry.set_managed_worker_active(true);

        let output = registry.render().await;
        assert!(output.contains("duduclaw_pty_pool_acquires_total{outcome=\"spawn\"} 1"));
        assert!(output.contains("duduclaw_pty_pool_evicted_total{reason=\"idle\"} 1"));
        assert!(output.contains("duduclaw_pty_pool_invokes_total{outcome=\"ok\"} 1"));
        assert!(output.contains("duduclaw_worker_restarts_total 1"));
        assert!(output.contains("duduclaw_pty_pool_managed_worker_active 1"));
        assert!(output.contains("duduclaw_pty_pool_invoke_duration_seconds_bucket"));
    }

    // ── WP5: cache-aware compression gate (2607.12161) ──

    #[tokio::test]
    async fn prompt_compression_run_counts_by_stage() {
        let r = MetricsRegistry::new();
        r.prompt_compression_run("turn_trim").await;
        r.prompt_compression_run("turn_trim").await;
        r.prompt_compression_run("drop_oldest_tool_echoes").await;
        let map = r.prompt_compression_runs.read().await;
        assert_eq!(map.get("turn_trim"), Some(&2));
        assert_eq!(map.get("drop_oldest_tool_echoes"), Some(&1));
    }

    #[test]
    fn prompt_compression_skip_and_break_counters_increment() {
        let r = MetricsRegistry::new();
        r.prompt_compression_skipped_cache_guard();
        r.prompt_compression_skipped_cache_guard();
        r.prompt_compression_cache_break_suspect();
        assert_eq!(
            r.prompt_compression_skipped_cache_guard_total.load(Ordering::Relaxed),
            2
        );
        assert_eq!(
            r.prompt_compression_cache_break_suspect_total.load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn render_emits_prompt_compression_metrics() {
        let r = MetricsRegistry::new();
        r.prompt_compression_run("turn_trim").await;
        r.prompt_compression_skipped_cache_guard();
        r.prompt_compression_cache_break_suspect();

        let output = r.render().await;
        assert!(output.contains("prompt_compression_runs_total{stage=\"turn_trim\"} 1"));
        assert!(output.contains("prompt_compression_skipped_cache_guard_total 1"));
        assert!(output.contains("prompt_compression_cache_break_suspect_total 1"));
    }
}
