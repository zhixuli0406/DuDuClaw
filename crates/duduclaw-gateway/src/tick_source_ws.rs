//! Resident sensing — the `websocket` tick source (D5-W).
//!
//! Split out of [`crate::tick_source`] (which owns the shared payload
//! pipeline: `emit_payload`, [`TickHub`], [`DropReason`], and the three
//! polling kinds) purely to keep both files inside the project's file-size
//! convention. Nothing here re-implements the pipeline: this module only
//! provides a different way of *obtaining* a payload.
//!
//! Where the other three kinds sleep and then fetch, this one holds a
//! connection open and is pushed to. Each **text** frame is one payload and
//! enters [`crate::tick_source::emit_payload`] unchanged, so the 64 KB cap,
//! the `emit_unchanged` dedup, the per-source rate cap (D6), the `json_fields`
//! extraction, the D2 delta trio, the ring buffer and the bus event all behave
//! exactly as they do for `http_poll` / `command` / `file_tail`.
//!
//! What is websocket-specific lives here:
//!
//! - **URL rules** (enforced at config load by
//!   [`crate::tick_config::validate_ws_url`], re-checked before every dial):
//!   `ws://` / `wss://` only, plaintext `ws://` for loopback hosts only,
//!   every other host validated by the shared SSRF gate.
//! - **Reconnect backoff**: exponential from `interval_secs`, capped, jittered.
//! - **Binary frames**: refused and counted as [`DropReason::NonText`] — this
//!   pipeline is text-only, and a silently-swallowed frame would make "my feed
//!   produces nothing" undiagnosable.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::autopilot_engine::AutopilotEvent;
use crate::tick_config::TickSourceConfig;
use crate::tick_source::{DropReason, FAILURE_LOG_EVERY, SourceState, TickHub, emit_payload};

/// D5-W — ceiling on the `websocket` reconnect backoff.
pub const WS_BACKOFF_MAX_SECS: u64 = 60;
/// D5-W — how much of the computed backoff is randomized on top of it
/// (`delay ∈ [base, base × 1.25]`). Prevents every source of a restarted
/// gateway from reconnecting to the same feed on the same millisecond.
const WS_BACKOFF_JITTER_NUM: u64 = 25;
/// Doubling steps past which the backoff is already pinned at
/// [`WS_BACKOFF_MAX_SECS`]; keeps the shift out of overflow territory.
const WS_BACKOFF_MAX_STEPS: u32 = 16;
/// A websocket session that stayed up at least this long counts as healthy:
/// the next reconnect starts the backoff over. Without it, a feed that
/// disconnects once a day would creep toward the 60 s ceiling and stay there.
const WS_STABLE_SESSION: Duration = Duration::from_secs(60);
/// Upper bound on the websocket opening handshake.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

// ═══════════════════════════════════════════════════════════════════════
// Backoff (pure)
// ═══════════════════════════════════════════════════════════════════════

/// D5-W — the `websocket` reconnect backoff, without jitter.
///
/// Starts at `max(1, interval_secs)` (a websocket source never polls, so
/// `interval_secs` is repurposed as this starting delay), doubles per
/// consecutive failed/short session, and is capped at
/// [`WS_BACKOFF_MAX_SECS`]. `retries` is the count of consecutive short
/// sessions so far, so `retries = 0` is the first reconnect after a healthy
/// connection.
pub fn ws_backoff_secs(interval_secs: u64, retries: u32) -> u64 {
    let start = interval_secs.max(crate::tick_config::MIN_INTERVAL_SECS);
    let steps = retries.min(WS_BACKOFF_MAX_STEPS);
    // Saturating throughout: an operator-set `interval_secs` of u64::MAX must
    // clamp to the ceiling, not wrap around to a tight reconnect loop.
    let scaled = start.saturating_mul(1u64.checked_shl(steps).unwrap_or(u64::MAX));
    scaled.min(WS_BACKOFF_MAX_SECS)
}

/// [`ws_backoff_secs`] plus up to +25% jitter. `seed` is injected (rather than
/// drawn inside) so the bounds are testable without a random source — the
/// caller passes the current nanosecond, the same pattern
/// `discord::invalid_session_jitter_ms` uses.
pub fn ws_backoff_delay(interval_secs: u64, retries: u32, seed: u32) -> Duration {
    let base_ms = ws_backoff_secs(interval_secs, retries).saturating_mul(1000);
    let span = base_ms / 100 * WS_BACKOFF_JITTER_NUM;
    let jitter = if span == 0 {
        0
    } else {
        u64::from(seed) % (span + 1)
    };
    Duration::from_millis(base_ms.saturating_add(jitter))
}

// ═══════════════════════════════════════════════════════════════════════
// Runtime
// ═══════════════════════════════════════════════════════════════════════

/// One `websocket` source's resident loop: connect → subscribe → stream text
/// frames into [`emit_payload`] → reconnect with exponential backoff. Like the
/// poll loop it never returns; aborting the task (gateway shutdown) is what
/// stops it, so shutdown behaves identically to every other source kind.
pub(crate) async fn run_websocket_source(
    cfg: TickSourceConfig,
    tx: broadcast::Sender<AutopilotEvent>,
    hub: Arc<TickHub>,
    events_bus: Option<Arc<crate::events_store::EventBusStore>>,
) {
    let Some(url) = cfg.url.clone() else {
        // Config validation makes this impossible; refusing to spin a task on
        // an unconfigured URL is the fail-closed reading of "impossible".
        warn!(source = %cfg.id, "tick websocket source has no url — not started");
        return;
    };
    let mut state = SourceState::new(&cfg);
    // Consecutive short sessions. Reset by a session that stayed up at least
    // `WS_STABLE_SESSION`, so a long-lived feed always reconnects fast.
    let mut retries: u32 = 0;

    loop {
        let started = Instant::now();
        let outcome =
            stream_websocket(&cfg, &url, &mut state, &tx, &hub, events_bus.as_ref()).await;
        let session = started.elapsed();

        match outcome {
            // A clean close is still a disconnect: reconnect on the same
            // backoff, but it is not an error and is not counted as one.
            Ok(()) => debug!(
                source = %cfg.id,
                session_secs = session.as_secs(),
                "tick websocket closed — reconnecting"
            ),
            Err(e) => {
                state.failures += 1;
                if state.failures == 1 || state.failures % FAILURE_LOG_EVERY == 0 {
                    warn!(
                        source = %cfg.id,
                        failures = state.failures,
                        error = %e,
                        "tick websocket connection failed"
                    );
                }
                hub.record_drop(&cfg.id, DropReason::FetchError).await;
                crate::metrics::global_metrics()
                    .tick_dropped(&cfg.id, DropReason::FetchError.as_str())
                    .await;
            }
        }

        if session >= WS_STABLE_SESSION {
            retries = 0;
            state.failures = 0;
        }
        let delay = ws_backoff_delay(cfg.interval_secs, retries, jitter_seed());
        retries = retries.saturating_add(1);
        tokio::time::sleep(delay).await;
    }
}

/// Nanosecond-of-second seed for the backoff jitter. Same source of entropy
/// (and same rationale — no RNG state to carry around a resident loop) as
/// `discord::invalid_session_jitter_ms`.
fn jitter_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Hold one websocket connection open, feeding every text frame into the
/// shared payload pipeline. Returns `Ok(())` when the peer closed the stream
/// and `Err` when the connection could not be established or failed mid-stream.
async fn stream_websocket(
    cfg: &TickSourceConfig,
    url: &str,
    state: &mut SourceState,
    tx: &broadcast::Sender<AutopilotEvent>,
    hub: &Arc<TickHub>,
    events_bus: Option<&Arc<crate::events_store::EventBusStore>>,
) -> Result<(), String> {
    // Fail-closed re-check on the URL actually dialed — the config was
    // validated at load time, but keeping the gate adjacent to the connect
    // call is the same convention `poll_once` follows for `http_poll`.
    crate::tick_config::validate_ws_url(url)?;

    let (stream, _response) =
        tokio::time::timeout(WS_CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| format!("connect timed out after {}s", WS_CONNECT_TIMEOUT.as_secs()))?
            .map_err(|e| format!("connect failed: {e}"))?;

    let (mut write, mut read) = stream.split();

    // D5-W — verbatim subscribe frames, in order. No templating: a feed's
    // subscribe handshake is operator-authored config, sent exactly as written.
    for (index, frame) in cfg.subscribe.iter().enumerate() {
        write
            .send(Message::Text(frame.clone().into()))
            .await
            .map_err(|e| format!("subscribe frame {index} failed: {e}"))?;
    }
    info!(
        source = %cfg.id,
        subscribe_frames = cfg.subscribe.len(),
        "tick websocket connected"
    );

    let mut non_text: u64 = 0;
    while let Some(message) = read.next().await {
        match message.map_err(|e| format!("stream error: {e}"))? {
            // The whole point: one text frame is one payload, entering exactly
            // the same pipeline (size cap → dedup → rate cap → fields → ring
            // buffer → bus) as an `http_poll` body or a `file_tail` line.
            Message::Text(text) => {
                emit_payload(cfg, state, text.as_str(), tx, hub, events_bus).await;
            }
            Message::Binary(bytes) => {
                non_text += 1;
                if non_text == 1 || non_text % FAILURE_LOG_EVERY == 0 {
                    warn!(
                        source = %cfg.id,
                        bytes = bytes.len(),
                        dropped_total = non_text,
                        "tick websocket dropped a binary frame — this pipeline is text-only"
                    );
                }
                hub.record_drop(&cfg.id, DropReason::NonText).await;
                crate::metrics::global_metrics()
                    .tick_dropped(&cfg.id, DropReason::NonText.as_str())
                    .await;
            }
            // Answered explicitly rather than relying on the protocol layer's
            // automatic reply, matching `discord.rs`.
            Message::Ping(payload) => {
                write
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|e| format!("pong failed: {e}"))?;
            }
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(frame) => {
                debug!(
                    source = %cfg.id,
                    close = ?frame.map(|f| f.code),
                    "tick websocket closed by peer"
                );
                return Ok(());
            }
        }
    }
    Ok(())
}

// ─── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick_config::TickKind;
    use crate::tick_source::run_source;
    use std::collections::BTreeMap;

    fn pointers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ── D5-W: websocket backoff ──────────────────────────────

    #[test]
    fn ws_backoff_doubles_from_the_interval_and_caps_at_60s() {
        // `interval_secs = 1` (the floor) — 1, 2, 4, … then pinned at 60.
        let seq: Vec<u64> = (0..9).map(|r| ws_backoff_secs(1, r)).collect();
        assert_eq!(seq, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);

        // A slower source starts slower and reaches the same ceiling.
        let seq: Vec<u64> = (0..5).map(|r| ws_backoff_secs(10, r)).collect();
        assert_eq!(seq, vec![10, 20, 40, 60, 60]);

        // Sub-second config is floored to 1s exactly like the poll kinds (D6).
        assert_eq!(ws_backoff_secs(0, 0), 1);

        // Absurd inputs clamp to the ceiling instead of overflowing into a
        // tight reconnect loop.
        assert_eq!(ws_backoff_secs(u64::MAX, 0), WS_BACKOFF_MAX_SECS);
        assert_eq!(ws_backoff_secs(1, u32::MAX), WS_BACKOFF_MAX_SECS);
    }

    #[test]
    fn ws_backoff_jitter_only_ever_adds_up_to_25_percent() {
        for seed in [0u32, 1, 999, 123_456_789, u32::MAX] {
            for retries in [0u32, 3, 20] {
                let base_ms = ws_backoff_secs(1, retries) * 1000;
                let delay = ws_backoff_delay(1, retries, seed).as_millis() as u64;
                assert!(
                    delay >= base_ms && delay <= base_ms + base_ms / 4,
                    "delay {delay} outside [{base_ms}, {}] (seed={seed}, retries={retries})",
                    base_ms + base_ms / 4
                );
            }
        }
        // The jitter actually varies — a constant would defeat the purpose.
        let spread: std::collections::HashSet<u128> = (0..500)
            .map(|s| ws_backoff_delay(1, 0, s * 7919).as_millis())
            .collect();
        assert!(
            spread.len() > 50,
            "jitter looks clipped: {} values",
            spread.len()
        );
    }

    // ── D5-W: websocket ingest (in-process end-to-end) ───────

    /// Drives a real `tokio-tungstenite` server on a loopback ephemeral port
    /// through the production `run_source` entry point: text frames must
    /// become ticks (ring buffer + counters + bus event), a binary frame must
    /// be dropped and counted, and the configured `subscribe` frame must
    /// actually reach the server.
    #[tokio::test(flavor = "current_thread")]
    async fn websocket_source_streams_text_frames_and_refuses_binary_ones() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (subscribe_tx, subscribe_rx) = tokio::sync::oneshot::channel::<String>();

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            if let Some(Ok(Message::Text(first))) = ws.next().await {
                let _ = subscribe_tx.send(first.to_string());
            }
            ws.send(Message::Text(r#"{"p":100}"#.into())).await.unwrap();
            // Binary frames are refused by the text-only pipeline …
            ws.send(Message::Binary(vec![0xff, 0x00, 0xfe].into()))
                .await
                .unwrap();
            // … and must not break the stream: the next text frame still lands.
            ws.send(Message::Text(r#"{"p":102}"#.into())).await.unwrap();
            let _ = ws.close(None).await;
        });

        let hub = Arc::new(TickHub::new());
        let (tx, mut rx) = broadcast::channel(16);
        let cfg = TickSourceConfig {
            id: "ws-feed".into(),
            kind: TickKind::Websocket,
            enabled: true,
            // For a websocket source this is the backoff start, not a poll
            // period — nothing in this test waits for it.
            interval_secs: 1,
            url: Some(format!("ws://127.0.0.1:{port}/stream")),
            command: None,
            path: None,
            subscribe: vec![r#"{"op":"subscribe","topic":"quotes"}"#.into()],
            json_fields: pointers(&[("p", "/p")]),
            emit_unchanged: false,
            max_events_per_minute: 120,
            persist_every_n: 0,
        };
        let source = tokio::spawn(run_source(cfg, tx, hub.clone(), None));

        // Wait for both text frames to have made it through the pipeline.
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if hub.len("ws-feed").await >= 2 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(settled.is_ok(), "websocket ticks never arrived");

        // The subscribe handshake reached the server verbatim.
        let sent = tokio::time::timeout(Duration::from_secs(5), subscribe_rx)
            .await
            .expect("subscribe frame never arrived")
            .expect("subscribe channel closed");
        assert_eq!(sent, r#"{"op":"subscribe","topic":"quotes"}"#);

        // Ring buffer: two records, with the D2 delta trio on the second.
        let records = hub.recent("ws-feed", 10).await;
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].fields["p"].as_i64(), Some(100));
        assert_eq!(records[1].fields["p"].as_i64(), Some(102));
        assert_eq!(records[1].fields["delta_p"].as_i64(), Some(2));
        assert_eq!(records[1].fields["prev_p"].as_i64(), Some(100));
        assert!(records[0].raw.is_none(), "JSON payload keeps no excerpt");

        // Counters: two emitted, exactly one non-text drop.
        let snap = hub.counters_snapshot("ws-feed").await;
        assert_eq!(snap.events_emitted, 2);
        assert_eq!(snap.dropped_non_text, 1, "the binary frame is counted");
        assert_eq!(snap.dropped_oversize, 0);
        assert_eq!(snap.dropped_rate_cap, 0);
        assert!(snap.last_tick_ts.is_some());

        // Both observations reached the autopilot bus as `tick` events.
        for expected in [100i64, 102] {
            match rx.try_recv().expect("tick event on the bus") {
                AutopilotEvent::Tick { source, fields, .. } => {
                    assert_eq!(source, "ws-feed");
                    assert_eq!(fields["p"].as_i64(), Some(expected));
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        source.abort();
        server.abort();
    }
}
