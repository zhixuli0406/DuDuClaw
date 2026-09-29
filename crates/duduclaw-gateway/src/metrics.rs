//! Prometheus metrics exposition — `GET /metrics`.
//!
//! Lightweight implementation without the `prometheus` crate dependency.
//! Outputs metrics in Prometheus text exposition format.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

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
    pub failover_total: AtomicU64,

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

    // ── Resident sensing (WP4 observability) ──────────────────────────
    /// Tick events successfully emitted, by source id. Source ids are
    /// validated against `^[a-z0-9][a-z0-9-]{0,63}$` at `[tick]` config load
    /// time (`tick_config::is_valid_source_id`), so this label is safe to
    /// interpolate without further escaping.
    pub tick_events: RwLock<std::collections::HashMap<String, u64>>,
    /// Tick payloads refused before becoming an event, by `(source, reason)`.
    /// `reason` is one of the fixed [`crate::tick_source::DropReason`]
    /// strings.
    pub tick_dropped: RwLock<std::collections::HashMap<(String, String), u64>>,
    /// WP3 local-model screening verdicts. Global rather than per-source or
    /// per-rule: `screen` is a rule-level feature usable on any
    /// `trigger_event`, so there is no single source to attribute a verdict
    /// to (see `autopilot_screen` module doc).
    pub tick_screen_pass_total: AtomicU64,
    pub tick_screen_drop_total: AtomicU64,
    pub tick_screen_unavailable_total: AtomicU64,
    /// Rules whose action actually dispatched after a `tick`-triggered fire,
    /// keyed by `rule_id` — never `rule_name`, which is operator-authored
    /// free text and must not become an unescaped Prometheus label.
    pub tick_wakes: RwLock<std::collections::HashMap<String, u64>>,

    // ── H5 (WP-B goal loop hardening): bail-pattern panel telemetry ──
    /// Premature-stop pattern hits, keyed by the fixed pattern name from
    /// `goal_bail_detect::pattern_names()` (a closed, small, code-defined
    /// set — safe to use directly as a Prometheus label, unlike free text).
    pub goal_loop_bail_pattern: RwLock<std::collections::HashMap<String, u64>>,

    // ── Goal intent router (P0, `goal_intent.rs`) ──────────────────────
    /// Outcomes for a channel-side goal suggestion, keyed by the closed,
    /// code-defined outcome token (`suggested` / `accepted` / `plan_first` /
    /// `dismissed` / `expired`) — never free text.
    pub goal_intent: RwLock<std::collections::HashMap<String, u64>>,
    /// L2 grey-band arbitration verdicts, keyed by `(engine, verdict)` — both
    /// closed, code-defined vocabularies (`engine` ∈ {`local`, `reply_tag`};
    /// `verdict` ∈ {`suggested`, `chat`}).
    pub goal_intent_l2: RwLock<std::collections::HashMap<(String, String), u64>>,

    // ── Relay client (WP-E2: box-side webhook relay ingestion) ────────
    /// Gauge: 1 while the box holds an authenticated `duduclaw-relay`
    /// WebSocket session, 0 otherwise (including `[relay] enabled = false`,
    /// which never sets it).
    pub relay_connected: AtomicU64,
    /// Hook frames received from the relay, by `(channel, outcome)`.
    /// `outcome` ∈ {`ok`, `bad_signature`, `unsupported`} — see
    /// `relay_client::inject_line_hook` / `handle_hook_text`.
    pub relay_frames: RwLock<std::collections::HashMap<(String, String), u64>>,
    /// Relay WebSocket (re)connect attempts, including the first connect
    /// after gateway boot. A steady trickle is expected (Cloud Run forces a
    /// disconnect every ~3600s per `crates/duduclaw-relay/README.md`) —
    /// this counter is for rate-of-change dashboards, not an alarm by
    /// itself.
    pub relay_reconnects_total: AtomicU64,

    // ── WP-G1: scheduled backups + device-migration restore ──────────
    /// Scheduled backup runs, by outcome. Fail-open per the WP-G1 spec — a
    /// failure is counted and logged, never fatal to the gateway.
    pub backup_schedule_ok_total: AtomicU64,
    pub backup_schedule_fail_total: AtomicU64,
    /// Restore swap outcomes applied at boot (`perform_pending_restore_swap`).
    pub backup_restore_swap_ok_total: AtomicU64,
    pub backup_restore_swap_fail_total: AtomicU64,
}

impl MetricsRegistry {
    fn new() -> Self {
        Self {
            failover_total: AtomicU64::new(0),
            wiki_trust_signals_applied_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_capped_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_locked_total: AtomicU64::new(0),
            wiki_trust_signals_dropped_daily_limit_total: AtomicU64::new(0),
            wiki_trust_archive_total: AtomicU64::new(0),
            wiki_trust_recovery_total: AtomicU64::new(0),
            wiki_trust_federation_partial_total: AtomicU64::new(0),
            decision_captured_total: AtomicU64::new(0),
            decision_resolved_total: AtomicU64::new(0),
            decision_expired_total: AtomicU64::new(0),
            decision_false_positive_total: AtomicU64::new(0),
            prompt_compression_runs: RwLock::new(std::collections::HashMap::new()),
            prompt_compression_skipped_cache_guard_total: AtomicU64::new(0),
            prompt_compression_cache_break_suspect_total: AtomicU64::new(0),

            tick_events: RwLock::new(std::collections::HashMap::new()),
            tick_dropped: RwLock::new(std::collections::HashMap::new()),
            tick_screen_pass_total: AtomicU64::new(0),
            tick_screen_drop_total: AtomicU64::new(0),
            tick_screen_unavailable_total: AtomicU64::new(0),
            tick_wakes: RwLock::new(std::collections::HashMap::new()),

            goal_loop_bail_pattern: RwLock::new(std::collections::HashMap::new()),

            goal_intent: RwLock::new(std::collections::HashMap::new()),
            goal_intent_l2: RwLock::new(std::collections::HashMap::new()),

            relay_connected: AtomicU64::new(0),
            relay_frames: RwLock::new(std::collections::HashMap::new()),
            relay_reconnects_total: AtomicU64::new(0),

            backup_schedule_ok_total: AtomicU64::new(0),
            backup_schedule_fail_total: AtomicU64::new(0),
            backup_restore_swap_ok_total: AtomicU64::new(0),
            backup_restore_swap_fail_total: AtomicU64::new(0),
        }
    }

    // ── WP-G1: scheduled backups + device-migration restore ──────────

    pub fn backup_schedule_ok(&self) {
        self.backup_schedule_ok_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn backup_schedule_fail(&self) {
        self.backup_schedule_fail_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn backup_restore_swap_ok(&self) {
        self.backup_restore_swap_ok_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn backup_restore_swap_fail(&self) {
        self.backup_restore_swap_fail_total
            .fetch_add(1, Ordering::Relaxed);
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

    // ── Resident sensing helpers (WP4 observability) ──────────────────

    /// Record one tick event emitted by `source`.
    pub async fn tick_event(&self, source: &str) {
        let mut map = self.tick_events.write().await;
        *map.entry(source.to_string()).or_insert(0) += 1;
    }

    /// Record one tick payload refused before becoming an event.
    pub async fn tick_dropped(&self, source: &str, reason: &str) {
        let mut map = self.tick_dropped.write().await;
        *map.entry((source.to_string(), reason.to_string()))
            .or_insert(0) += 1;
    }

    /// Record one WP3 screening verdict. `outcome` must be `"pass"` /
    /// `"drop"` / `"unavailable"`; anything else folds into `unavailable`
    /// (fail-closed observability — an unrecognized outcome must never
    /// vanish from the totals).
    pub fn tick_screen(&self, outcome: &str) {
        let counter = match outcome {
            "pass" => &self.tick_screen_pass_total,
            "drop" => &self.tick_screen_drop_total,
            _ => &self.tick_screen_unavailable_total,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record one rule action dispatching after a `tick`-triggered fire.
    pub async fn tick_wake(&self, rule_id: &str) {
        let mut map = self.tick_wakes.write().await;
        *map.entry(rule_id.to_string()).or_insert(0) += 1;
    }

    // ── H5 (WP-B): bail-pattern panel telemetry ────────────────────────

    /// Record one premature-stop pattern hit for `pattern` (one of
    /// `goal_bail_detect::pattern_names()`).
    pub async fn goal_loop_bail_pattern_hit(&self, pattern: &str) {
        let mut map = self.goal_loop_bail_pattern.write().await;
        *map.entry(pattern.to_string()).or_insert(0) += 1;
    }

    // ── Goal intent router (P0) ─────────────────────────────────────────

    /// Record one `outcome` for `goal_intent_total` (`suggested` /
    /// `accepted` / `plan_first` / `dismissed` / `expired`).
    pub async fn goal_intent_event(&self, outcome: &str) {
        let mut map = self.goal_intent.write().await;
        *map.entry(outcome.to_string()).or_insert(0) += 1;
    }

    /// Record one L2 grey-band verdict for `goal_intent_l2_total`.
    pub async fn goal_intent_l2_event(&self, engine: &str, verdict: &str) {
        let mut map = self.goal_intent_l2.write().await;
        *map.entry((engine.to_string(), verdict.to_string()))
            .or_insert(0) += 1;
    }

    // ── Relay client (WP-E2) ────────────────────────────────────────────

    /// Set the relay-connected gauge. `connected = true` while the box
    /// holds an authenticated relay WebSocket session.
    pub fn set_relay_connected(&self, connected: bool) {
        self.relay_connected
            .store(if connected { 1 } else { 0 }, Ordering::Relaxed);
    }

    /// Record one relayed hook frame outcome. `outcome` should be `"ok"` /
    /// `"bad_signature"` / `"unsupported"`, but any value is accepted
    /// verbatim (a closed, code-defined set at every call site — see
    /// `relay_client.rs` — so no further validation happens here).
    pub async fn relay_frame(&self, channel: &str, outcome: &str) {
        let mut map = self.relay_frames.write().await;
        *map.entry((channel.to_string(), outcome.to_string()))
            .or_insert(0) += 1;
    }

    /// Record one relay WebSocket (re)connect attempt.
    pub fn relay_reconnect(&self) {
        self.relay_reconnects_total.fetch_add(1, Ordering::Relaxed);
    }

    // ── Wiki RL Trust Feedback (review BLOCKER R4 m12) ──────────────

    pub fn wiki_trust_signal_applied(&self) {
        self.wiki_trust_signals_applied_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_capped(&self) {
        self.wiki_trust_signals_dropped_capped_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_locked(&self) {
        self.wiki_trust_signals_dropped_locked_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_signal_dropped_daily_limit(&self) {
        self.wiki_trust_signals_dropped_daily_limit_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_archive(&self) {
        self.wiki_trust_archive_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_recovery(&self) {
        self.wiki_trust_recovery_total
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn wiki_trust_federation_partial(&self) {
        self.wiki_trust_federation_partial_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a failover event.
    pub fn record_failover(&self) {
        self.failover_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Render all metrics in Prometheus text exposition format.
    pub async fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        // Counters
        out.push_str("# HELP duduclaw_failover_total Total failover events.\n");
        out.push_str("# TYPE duduclaw_failover_total counter\n");
        out.push_str(&format!(
            "duduclaw_failover_total {}\n",
            self.failover_total.load(Ordering::Relaxed)
        ));

        // ── Wiki RL Trust Feedback (review BLOCKER R4 m12) ──────
        out.push_str(
            "# HELP wiki_trust_signals_applied_total Trust signals successfully applied.\n",
        );
        out.push_str("# TYPE wiki_trust_signals_applied_total counter\n");
        out.push_str(&format!(
            "wiki_trust_signals_applied_total {}\n",
            self.wiki_trust_signals_applied_total
                .load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_signals_dropped_total Trust signals dropped, by reason.\n");
        out.push_str("# TYPE wiki_trust_signals_dropped_total counter\n");
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"per_conv_cap\"}} {}\n",
            self.wiki_trust_signals_dropped_capped_total
                .load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"locked\"}} {}\n",
            self.wiki_trust_signals_dropped_locked_total
                .load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "wiki_trust_signals_dropped_total{{reason=\"daily_limit\"}} {}\n",
            self.wiki_trust_signals_dropped_daily_limit_total
                .load(Ordering::Relaxed)
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
            self.wiki_trust_federation_partial_total
                .load(Ordering::Relaxed)
        ));
        out.push_str("# HELP wiki_trust_active_conversations CitationTracker bucket count.\n");
        out.push_str("# TYPE wiki_trust_active_conversations gauge\n");
        // Read live (review R5 MUST-1c: previously a dead atomic gauge).
        out.push_str(&format!(
            "wiki_trust_active_conversations {}\n",
            duduclaw_memory::feedback::global_tracker().conv_count()
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
            self.prompt_compression_skipped_cache_guard_total
                .load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP prompt_compression_cache_break_suspect_total Requests where compression likely broke a healthy cache prefix.\n",
        );
        out.push_str("# TYPE prompt_compression_cache_break_suspect_total counter\n");
        out.push_str(&format!(
            "prompt_compression_cache_break_suspect_total {}\n",
            self.prompt_compression_cache_break_suspect_total
                .load(Ordering::Relaxed)
        ));

        // ── Resident sensing (WP4 observability) ──
        out.push_str("# HELP tick_events_total Tick events emitted, by source.\n");
        out.push_str("# TYPE tick_events_total counter\n");
        for (source, count) in self.tick_events.read().await.iter() {
            out.push_str(&format!(
                "tick_events_total{{source=\"{source}\"}} {count}\n"
            ));
        }
        out.push_str(
            "# HELP tick_dropped_total Tick payloads refused before becoming an event, by source and reason.\n",
        );
        out.push_str("# TYPE tick_dropped_total counter\n");
        for ((source, reason), count) in self.tick_dropped.read().await.iter() {
            out.push_str(&format!(
                "tick_dropped_total{{source=\"{source}\",reason=\"{reason}\"}} {count}\n"
            ));
        }
        out.push_str("# HELP tick_screen_total Local-model screening verdicts, by outcome.\n");
        out.push_str("# TYPE tick_screen_total counter\n");
        out.push_str(&format!(
            "tick_screen_total{{outcome=\"pass\"}} {}\n",
            self.tick_screen_pass_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "tick_screen_total{{outcome=\"drop\"}} {}\n",
            self.tick_screen_drop_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "tick_screen_total{{outcome=\"unavailable\"}} {}\n",
            self.tick_screen_unavailable_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP tick_wakes_total Rule actions dispatched after a tick-triggered fire, by rule id.\n",
        );
        out.push_str("# TYPE tick_wakes_total counter\n");
        for (rule_id, count) in self.tick_wakes.read().await.iter() {
            out.push_str(&format!("tick_wakes_total{{rule=\"{rule_id}\"}} {count}\n"));
        }

        // ── H5 (WP-B): bail-pattern panel telemetry ──
        out.push_str(
            "# HELP goal_loop_bail_pattern_total Premature-stop pattern hits, by pattern name.\n",
        );
        out.push_str("# TYPE goal_loop_bail_pattern_total counter\n");
        for (pattern, count) in self.goal_loop_bail_pattern.read().await.iter() {
            out.push_str(&format!(
                "goal_loop_bail_pattern_total{{pattern=\"{pattern}\"}} {count}\n"
            ));
        }

        // ── Goal intent router (P0) ──
        out.push_str(
            "# HELP goal_intent_total Channel-side goal suggestion outcomes, by outcome.\n",
        );
        out.push_str("# TYPE goal_intent_total counter\n");
        for (outcome, count) in self.goal_intent.read().await.iter() {
            out.push_str(&format!(
                "goal_intent_total{{outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        out.push_str(
            "# HELP goal_intent_l2_total L2 grey-band arbitration verdicts, by engine and verdict.\n",
        );
        out.push_str("# TYPE goal_intent_l2_total counter\n");
        for ((engine, verdict), count) in self.goal_intent_l2.read().await.iter() {
            out.push_str(&format!(
                "goal_intent_l2_total{{engine=\"{engine}\",verdict=\"{verdict}\"}} {count}\n"
            ));
        }

        // ── Relay client (WP-E2: box-side webhook relay ingestion) ──
        out.push_str(
            "# HELP relay_connected Whether the box holds an authenticated duduclaw-relay WebSocket session (1) or not (0).\n",
        );
        out.push_str("# TYPE relay_connected gauge\n");
        out.push_str(&format!(
            "relay_connected {}\n",
            self.relay_connected.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP relay_frames_total Hook frames received from the relay, by channel and outcome.\n",
        );
        out.push_str("# TYPE relay_frames_total counter\n");
        for ((channel, outcome), count) in self.relay_frames.read().await.iter() {
            out.push_str(&format!(
                "relay_frames_total{{channel=\"{channel}\",outcome=\"{outcome}\"}} {count}\n"
            ));
        }
        out.push_str("# HELP relay_reconnects_total Relay WebSocket (re)connect attempts.\n");
        out.push_str("# TYPE relay_reconnects_total counter\n");
        out.push_str(&format!(
            "relay_reconnects_total {}\n",
            self.relay_reconnects_total.load(Ordering::Relaxed)
        ));

        // ── WP-G1: scheduled backups + device-migration restore ──────
        out.push_str("# HELP duduclaw_backup_schedule_total Scheduled backup runs, by outcome.\n");
        out.push_str("# TYPE duduclaw_backup_schedule_total counter\n");
        out.push_str(&format!(
            "duduclaw_backup_schedule_total{{outcome=\"ok\"}} {}\n",
            self.backup_schedule_ok_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_backup_schedule_total{{outcome=\"fail\"}} {}\n",
            self.backup_schedule_fail_total.load(Ordering::Relaxed)
        ));
        out.push_str(
            "# HELP duduclaw_backup_restore_swap_total Pending-restore swaps applied at boot, by outcome.\n",
        );
        out.push_str("# TYPE duduclaw_backup_restore_swap_total counter\n");
        out.push_str(&format!(
            "duduclaw_backup_restore_swap_total{{outcome=\"ok\"}} {}\n",
            self.backup_restore_swap_ok_total.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "duduclaw_backup_restore_swap_total{{outcome=\"fail\"}} {}\n",
            self.backup_restore_swap_fail_total.load(Ordering::Relaxed)
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
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;

    if !peer.ip().is_loopback() {
        return (
            StatusCode::FORBIDDEN,
            "Metrics only available from localhost",
        )
            .into_response();
    }

    let metrics = global_metrics();
    let mut body = metrics.render().await;
    // RFC-26: fork metrics live in the cross-process ForkStore (forks run in the
    // MCP-server process). Read them at scrape time and append.
    body.push_str(&render_fork_metrics());

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
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
        out.push_str(&format!(
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"
        ));
    };
    counter(
        &mut out,
        "duduclaw_fork_runs_total",
        "Total forks created.",
        m.forks_total,
    );
    counter(
        &mut out,
        "duduclaw_fork_resolved_total",
        "Forks resolved to a winner.",
        m.forks_resolved,
    );
    counter(
        &mut out,
        "duduclaw_fork_promoted_total",
        "Forks whose winner was promoted.",
        m.forks_promoted,
    );
    counter(
        &mut out,
        "duduclaw_fork_branches_total",
        "Total branches across all forks.",
        m.branches_total,
    );
    out.push_str("# HELP duduclaw_fork_branch_outcome Total branches by terminal outcome.\n");
    out.push_str("# TYPE duduclaw_fork_branch_outcome counter\n");
    out.push_str(&format!(
        "duduclaw_fork_branch_outcome{{outcome=\"finished\"}} {}\n",
        m.branches_finished
    ));
    out.push_str(&format!(
        "duduclaw_fork_branch_outcome{{outcome=\"budget_killed\"}} {}\n",
        m.branches_budget_killed
    ));
    out.push_str(&format!(
        "duduclaw_fork_branch_outcome{{outcome=\"failed\"}} {}\n",
        m.branches_failed
    ));
    out.push_str("# HELP duduclaw_fork_spend_usd_total Aggregate USD spent across all forks.\n");
    out.push_str("# TYPE duduclaw_fork_spend_usd_total counter\n");
    out.push_str(&format!(
        "duduclaw_fork_spend_usd_total {:.6}\n",
        m.aggregate_spent_usd
    ));
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
        store
            .set_resolution("f1", Some("b1"), true, true, 0.25)
            .unwrap();

        let out = render_fork_metrics_from(&path);
        assert!(out.contains("duduclaw_fork_runs_total 1"));
        assert!(out.contains("duduclaw_fork_promoted_total 1"));
        assert!(out.contains("duduclaw_fork_branch_outcome{outcome=\"finished\"} 1"));
        assert!(out.contains("duduclaw_fork_spend_usd_total 0.25"));
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
            r.prompt_compression_skipped_cache_guard_total
                .load(Ordering::Relaxed),
            2
        );
        assert_eq!(
            r.prompt_compression_cache_break_suspect_total
                .load(Ordering::Relaxed),
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

    // ── Resident sensing (WP4 observability) ──────────────────────────

    #[tokio::test]
    async fn tick_event_counts_by_source() {
        let r = MetricsRegistry::new();
        r.tick_event("twse-2330").await;
        r.tick_event("twse-2330").await;
        r.tick_event("other-source").await;
        let map = r.tick_events.read().await;
        assert_eq!(map.get("twse-2330"), Some(&2));
        assert_eq!(map.get("other-source"), Some(&1));
    }

    #[tokio::test]
    async fn tick_dropped_counts_by_source_and_reason() {
        let r = MetricsRegistry::new();
        r.tick_dropped("twse-2330", "rate_cap").await;
        r.tick_dropped("twse-2330", "rate_cap").await;
        r.tick_dropped("twse-2330", "oversize").await;
        r.tick_dropped("other-source", "fetch_error").await;
        let map = r.tick_dropped.read().await;
        assert_eq!(
            map.get(&("twse-2330".to_string(), "rate_cap".to_string())),
            Some(&2)
        );
        assert_eq!(
            map.get(&("twse-2330".to_string(), "oversize".to_string())),
            Some(&1)
        );
        assert_eq!(
            map.get(&("other-source".to_string(), "fetch_error".to_string())),
            Some(&1)
        );
    }

    #[test]
    fn tick_screen_routes_to_the_right_counter_and_folds_unknown_to_unavailable() {
        let r = MetricsRegistry::new();
        r.tick_screen("pass");
        r.tick_screen("pass");
        r.tick_screen("drop");
        r.tick_screen("unavailable");
        r.tick_screen("something_unexpected");
        assert_eq!(r.tick_screen_pass_total.load(Ordering::Relaxed), 2);
        assert_eq!(r.tick_screen_drop_total.load(Ordering::Relaxed), 1);
        // "unavailable" + the unrecognized outcome both fold here.
        assert_eq!(r.tick_screen_unavailable_total.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn tick_wake_counts_by_rule_id() {
        let r = MetricsRegistry::new();
        r.tick_wake("rule-1").await;
        r.tick_wake("rule-1").await;
        r.tick_wake("rule-2").await;
        let map = r.tick_wakes.read().await;
        assert_eq!(map.get("rule-1"), Some(&2));
        assert_eq!(map.get("rule-2"), Some(&1));
    }

    // ── H5 (WP-B): bail-pattern panel telemetry ────────────────────────

    #[tokio::test]
    async fn goal_loop_bail_pattern_counts_by_pattern_name() {
        let r = MetricsRegistry::new();
        r.goal_loop_bail_pattern_hit("stopping_here").await;
        r.goal_loop_bail_pattern_hit("stopping_here").await;
        r.goal_loop_bail_pattern_hit("verdict_line").await;
        let map = r.goal_loop_bail_pattern.read().await;
        assert_eq!(map.get("stopping_here"), Some(&2));
        assert_eq!(map.get("verdict_line"), Some(&1));
    }

    #[tokio::test]
    async fn render_emits_goal_loop_bail_pattern_metric_labels() {
        let r = MetricsRegistry::new();
        r.goal_loop_bail_pattern_hit("giving_up").await;
        let output = r.render().await;
        assert!(output.contains("goal_loop_bail_pattern_total{pattern=\"giving_up\"} 1"));
    }

    #[tokio::test]
    async fn render_emits_resident_sensing_metric_labels() {
        let r = MetricsRegistry::new();
        r.tick_event("twse-2330").await;
        r.tick_dropped("twse-2330", "rate_cap").await;
        r.tick_screen("pass");
        r.tick_wake("rule-1").await;

        let output = r.render().await;
        assert!(output.contains("tick_events_total{source=\"twse-2330\"} 1"));
        assert!(output.contains("tick_dropped_total{source=\"twse-2330\",reason=\"rate_cap\"} 1"));
        assert!(output.contains("tick_screen_total{outcome=\"pass\"} 1"));
        assert!(output.contains("tick_screen_total{outcome=\"drop\"} 0"));
        assert!(output.contains("tick_wakes_total{rule=\"rule-1\"} 1"));
    }

    // ── Goal intent router (P0) ─────────────────────────────────────────

    #[tokio::test]
    async fn goal_intent_event_counts_by_outcome() {
        let r = MetricsRegistry::new();
        r.goal_intent_event("suggested").await;
        r.goal_intent_event("suggested").await;
        r.goal_intent_event("accepted").await;
        let map = r.goal_intent.read().await;
        assert_eq!(map.get("suggested"), Some(&2));
        assert_eq!(map.get("accepted"), Some(&1));
    }

    #[tokio::test]
    async fn goal_intent_l2_event_counts_by_engine_and_verdict() {
        let r = MetricsRegistry::new();
        r.goal_intent_l2_event("reply_tag", "suggested").await;
        r.goal_intent_l2_event("reply_tag", "chat").await;
        r.goal_intent_l2_event("reply_tag", "chat").await;
        let map = r.goal_intent_l2.read().await;
        assert_eq!(
            map.get(&("reply_tag".to_string(), "suggested".to_string())),
            Some(&1)
        );
        assert_eq!(
            map.get(&("reply_tag".to_string(), "chat".to_string())),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn render_emits_goal_intent_metric_labels() {
        let r = MetricsRegistry::new();
        r.goal_intent_event("suggested").await;
        r.goal_intent_l2_event("reply_tag", "suggested").await;
        let output = r.render().await;
        assert!(output.contains("goal_intent_total{outcome=\"suggested\"} 1"));
        assert!(
            output.contains("goal_intent_l2_total{engine=\"reply_tag\",verdict=\"suggested\"} 1")
        );
    }

    // ── Relay client (WP-E2) ─────────────────────────────────────────────

    #[test]
    fn relay_connected_gauge_toggles() {
        let r = MetricsRegistry::new();
        assert_eq!(
            r.relay_connected.load(Ordering::Relaxed),
            0,
            "off by default"
        );
        r.set_relay_connected(true);
        assert_eq!(r.relay_connected.load(Ordering::Relaxed), 1);
        r.set_relay_connected(false);
        assert_eq!(r.relay_connected.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn relay_frame_counts_by_channel_and_outcome() {
        let r = MetricsRegistry::new();
        r.relay_frame("line", "ok").await;
        r.relay_frame("line", "ok").await;
        r.relay_frame("line", "bad_signature").await;
        r.relay_frame("whatsapp", "unsupported").await;
        let map = r.relay_frames.read().await;
        assert_eq!(map.get(&("line".to_string(), "ok".to_string())), Some(&2));
        assert_eq!(
            map.get(&("line".to_string(), "bad_signature".to_string())),
            Some(&1)
        );
        assert_eq!(
            map.get(&("whatsapp".to_string(), "unsupported".to_string())),
            Some(&1)
        );
    }

    #[test]
    fn relay_reconnects_total_increments() {
        let r = MetricsRegistry::new();
        r.relay_reconnect();
        r.relay_reconnect();
        assert_eq!(r.relay_reconnects_total.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn render_emits_relay_client_metric_labels() {
        let r = MetricsRegistry::new();
        r.set_relay_connected(true);
        r.relay_frame("line", "ok").await;
        r.relay_frame("line", "bad_signature").await;
        r.relay_reconnect();
        let output = r.render().await;
        assert!(output.contains("relay_connected 1"));
        assert!(output.contains("relay_frames_total{channel=\"line\",outcome=\"ok\"} 1"));
        assert!(
            output.contains("relay_frames_total{channel=\"line\",outcome=\"bad_signature\"} 1")
        );
        assert!(output.contains("relay_reconnects_total 1"));
    }
}
