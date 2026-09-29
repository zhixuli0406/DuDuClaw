//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Resident sensing observability (WP4) ────────────────

    /// `ticks.sources` — every configured `[[tick.sources]]` entry (whether
    /// currently active or not — the master `[tick] enabled` switch and each
    /// source's own `enabled` flag are both surfaced so the dashboard can
    /// tell "off" from "never emitted"), joined with its live counters from
    /// the shared `TickHub`. Zero counts and `null` timestamps, never an
    /// error, when the hub was never wired (feature not started this boot).
    pub(crate) async fn handle_ticks_sources(&self) -> WsFrame {
        let cfg = crate::tick_config::TickConfig::from_home(&self.home_dir);
        let hub = self.tick_hub.read().await.clone();

        let mut sources = Vec::with_capacity(cfg.sources.len());
        for s in &cfg.sources {
            let (counters, events_per_minute_approx) = match &hub {
                Some(hub) => (
                    hub.counters_snapshot(&s.id).await,
                    hub.events_per_minute_approx(&s.id).await,
                ),
                None => (crate::tick_source::SourceCountersSnapshot::default(), 0.0),
            };
            sources.push(json!({
                "id": s.id,
                "kind": s.kind.as_str(),
                "enabled": s.enabled,
                "interval_secs": s.interval_secs,
                "max_events_per_minute": s.max_events_per_minute,
                // D5-W2 — how many custom headers this source sends. The
                // COUNT only: header values are credentials and never leave
                // the config file (not through this RPC, not through a log).
                "headers_count": s.headers.len(),
                "last_tick_ts": counters.last_tick_ts,
                "events_per_minute_approx": events_per_minute_approx,
                "events_emitted_total": counters.events_emitted,
                "dropped": {
                    "rate_cap": counters.dropped_rate_cap,
                    "unchanged": counters.dropped_unchanged,
                    "oversize": counters.dropped_oversize,
                    "fetch_error": counters.dropped_fetch_error,
                    // D5-W — websocket-only: binary frames the text pipeline
                    // refuses. Always present (0 for the polling kinds) so the
                    // dashboard's dropped total never has to guess a key.
                    "non_text": counters.dropped_non_text,
                    // F3 — payloads that resolved none of the configured
                    // `json_fields` (a feed's control/heartbeat frames).
                    "no_fields": counters.dropped_no_fields,
                },
            }));
        }

        let screen = match &hub {
            Some(hub) => {
                let (pass, drop, unavailable) = hub.screen_counts();
                json!({ "pass": pass, "drop": drop, "unavailable": unavailable })
            }
            None => json!({ "pass": 0, "drop": 0, "unavailable": 0 }),
        };

        WsFrame::ok_response(
            "",
            json!({
                "enabled": cfg.enabled,
                "allow_command_sources": cfg.allow_command_sources,
                // O15 — the named knob bundle in effect, or `null` for "no
                // preset" (every default is the historical one). Read-only:
                // the card displays it, `config.toml` owns it.
                "preset": cfg.preset.map(|p| p.as_str()),
                "sources": sources,
                "screen": screen,
            }),
        )
    }

    /// `ticks.recent` — up to [`crate::tick_source::TICK_RECENT_RPC_LIMIT`]
    /// (50) most recent buffered observations for one source, oldest first.
    /// An unknown source, or the hub never having been wired, both resolve
    /// to an empty `records` array — a source with no history yet is not an
    /// error.
    pub(crate) async fn handle_ticks_recent(&self, params: Value) -> WsFrame {
        let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
        if source.is_empty() {
            return WsFrame::error_response("", "source is required");
        }
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).min(crate::tick_source::TICK_RECENT_RPC_LIMIT))
            .unwrap_or(crate::tick_source::TICK_RECENT_RPC_LIMIT);

        let hub = self.tick_hub.read().await.clone();
        let records = match &hub {
            Some(hub) => hub.recent(source, limit).await,
            None => Vec::new(),
        };
        let entries: Vec<Value> = records
            .iter()
            .map(|r| {
                json!({
                    "ts": r.ts,
                    "fields": Value::Object(r.fields.clone()),
                    "raw": r.raw,
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "source": source, "records": entries }))
    }
}
