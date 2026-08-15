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
//! - **Idle watchdog + client ping** (D5-W2, [`watchdog_next`]): a TCP
//!   connection to a feed that stopped sending can stay "open" indefinitely,
//!   so inbound silence is measured by two clocks — `ping_interval_secs`
//!   prods the peer, `idle_timeout_secs` gives up and redials.
//! - **Custom headers + DNS re-pin** (D5-W2, [`connect_source`]): the upgrade
//!   request carries the operator's validated `headers`, and every non-loopback
//!   host is re-resolved and re-screened at dial time.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async_tls_with_config};
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
/// Upper bound on the websocket opening handshake (DNS + TCP + TLS + upgrade).
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// D5-W2 — how long the connection loop parks when **neither** watchdog clock
/// is configured. It is not a timeout: the loop is blocked on the socket and
/// this only bounds how long one `select!` arm waits before recomputing.
const WATCHDOG_PARK: Duration = Duration::from_secs(3600);
/// D5-W2 — floor on one watchdog wait, so a clock that is due in microseconds
/// cannot spin the loop.
const WATCHDOG_MIN_WAIT: Duration = Duration::from_millis(50);

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
// Idle watchdog + client ping (D5-W2, pure)
// ═══════════════════════════════════════════════════════════════════════

/// What the connection loop should do next, given how long the socket has been
/// silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogAction {
    /// Nothing is due yet — wait this long (or until a frame arrives).
    Wait(Duration),
    /// Send a client `Ping` frame.
    Ping,
    /// The idle timeout expired: drop this connection and redial.
    Recycle,
}

/// D5-W2 — the two-clock watchdog, as a pure function.
///
/// Both clocks measure **inbound silence**, and any inbound frame (text,
/// binary, ping, pong — a pong is traffic) resets `since_inbound`, which is why
/// a healthy connection never reaches either deadline.
///
/// - `idle_after` fires first when both are due: a connection that has proven
///   itself dead should be recycled, not pinged again.
/// - `ping_every` is measured from the last ping when one has already been sent
///   in this quiet stretch (`since_ping`), otherwise from the last inbound
///   frame — so pinging repeats on its own period instead of firing once.
///
/// `None` for either clock means "disabled" (`*_secs = 0`). With both disabled
/// the loop simply blocks on the socket, which is exactly the pre-D5-W2
/// behavior.
pub fn watchdog_next(
    since_inbound: Duration,
    since_ping: Option<Duration>,
    ping_every: Option<Duration>,
    idle_after: Option<Duration>,
) -> WatchdogAction {
    if let Some(idle) = idle_after {
        if since_inbound >= idle {
            return WatchdogAction::Recycle;
        }
    }
    let mut wait = WATCHDOG_PARK;
    if let Some(idle) = idle_after {
        wait = wait.min(idle.saturating_sub(since_inbound));
    }
    if let Some(ping) = ping_every {
        let since = since_ping.unwrap_or(since_inbound);
        if since >= ping {
            return WatchdogAction::Ping;
        }
        wait = wait.min(ping.saturating_sub(since));
    }
    WatchdogAction::Wait(wait.max(WATCHDOG_MIN_WAIT))
}

/// How one websocket session ended. Distinguishing these matters because they
/// want different reconnect behavior — see [`run_websocket_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionOutcome {
    /// The peer closed the stream (or it simply ended). Reconnect on the
    /// normal backoff.
    Closed,
    /// D5-W2 — the idle watchdog fired. Redial **immediately**: the connection
    /// was not refused, it went quiet, and the whole point is to get a live one
    /// back without waiting out a backoff first.
    IdleTimeout,
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
    home_dir: Arc<std::path::PathBuf>,
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
    // D5-W2 — idle recycles so far, for log throttling only. Deliberately not
    // a counter on `TickHub`: `last_tick_ts` already says the feed went quiet,
    // and a recycle is not a dropped payload.
    let mut recycles: u64 = 0;

    loop {
        let started = Instant::now();
        let outcome =
            stream_websocket(&cfg, &home_dir, &url, &mut state, &tx, &hub, events_bus.as_ref())
                .await;
        let session = started.elapsed();
        let mut recycled = false;

        match outcome {
            // A clean close is still a disconnect: reconnect on the same
            // backoff, but it is not an error and is not counted as one.
            Ok(SessionOutcome::Closed) => debug!(
                source = %cfg.id,
                session_secs = session.as_secs(),
                "tick websocket closed — reconnecting"
            ),
            Ok(SessionOutcome::IdleTimeout) => {
                recycled = true;
                recycles += 1;
                if recycles == 1 || recycles % FAILURE_LOG_EVERY == 0 {
                    warn!(
                        source = %cfg.id,
                        idle_timeout_secs = cfg.idle_timeout_secs,
                        session_secs = session.as_secs(),
                        recycles_total = recycles,
                        "tick websocket idle — no inbound frame within the timeout, redialling"
                    );
                }
            }
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
        // D5-W2 — an idle recycle redials at once; only a *failed* redial
        // (the `Err` arm above, on the next pass) pays the backoff.
        if recycled {
            continue;
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

/// Dial one websocket source: build the upgrade request (with the operator's
/// custom headers), pin DNS, connect, TLS-upgrade, handshake.
///
/// **DNS re-pin (D5-W2)**: for every non-loopback host the name is re-resolved
/// here, the whole answer must be public, and the TCP connection is made
/// straight to those addresses — while TLS still uses the original hostname for
/// SNI and certificate validation, so pinning cannot be used to accept a
/// wrong-name certificate. Loopback (the one place plaintext `ws://` is
/// allowed) skips re-pinning: it is the documented local-relay path, its
/// address is by definition internal, and there is nothing to rebind to.
async fn connect_source(
    cfg: &TickSourceConfig,
    home_dir: &std::path::Path,
    url: &str,
    dns: &mut crate::tick_source::DnsCache,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("url rejected: invalid URL: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "url rejected: missing host".to_string())?;
    // `host_str()` brackets IPv6 literals; `to_socket_addrs` wants them
    // bracketed, a `(host, port)` tuple wants them bare.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "url rejected: no port".to_string())?;

    let mut request = url
        .into_client_request()
        .map_err(|e| format!("invalid websocket request: {e}"))?;
    // D5-W2 custom headers. Transport-owned names (Host / Connection /
    // Upgrade / Sec-WebSocket-*) are refused at config load, so nothing here
    // can overwrite the handshake the library just generated. Values are never
    // logged — only the name is, and only when it fails to encode (which
    // config validation already makes unreachable).
    // WP-H1 P1 — resolved per dial (a reconnect re-reads the backend), and
    // re-validated: a value from Vault/keychain/file never passed through
    // `validate_headers`, so CR/LF injection is checked again there.
    let sm_cfg = crate::tick_headers::load_secret_manager_config(home_dir).await;
    let resolved =
        crate::tick_headers::resolve_header_secrets(&cfg.headers, &sm_cfg, home_dir, &cfg.id).await;
    for (name, value) in &resolved {
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(n), Ok(v)) => {
                request.headers_mut().insert(n, v);
            }
            _ => warn!(
                source = %cfg.id,
                header = %name,
                "tick websocket: header could not be encoded — omitted"
            ),
        }
    }

    let is_loopback = bare == "localhost"
        || bare
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);

    let stream = if is_loopback {
        TcpStream::connect((bare, port))
            .await
            .map_err(|e| format!("tcp connect failed: {e}"))?
    } else {
        // R2 — a reconnect inside the DNS TTL redials the addresses already
        // screened for this host instead of paying for (and waiting on) a
        // fresh lookup on every backoff cycle.
        let key = format!("{host}:{port}");
        let owned_host = host.to_string();
        let addrs =
            crate::tick_source::resolve_with_cache(dns, &key, Instant::now(), || async move {
                crate::web_fetch::resolve_public_addrs(&owned_host, port)
                    .await
                    .map_err(|e| e.to_string())
            })
            .await?;
        TcpStream::connect(&addrs[..])
            .await
            .map_err(|e| format!("tcp connect failed: {e}"))?
    };

    let (ws, _response) = client_async_tls_with_config(request, stream, None, None)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    Ok(ws)
}

/// Hold one websocket connection open, feeding every text frame into the
/// shared payload pipeline. Returns [`SessionOutcome`] for the two normal ways
/// a session ends, and `Err` when the connection could not be established or
/// failed mid-stream.
async fn stream_websocket(
    cfg: &TickSourceConfig,
    home_dir: &std::path::Path,
    url: &str,
    state: &mut SourceState,
    tx: &broadcast::Sender<AutopilotEvent>,
    hub: &Arc<TickHub>,
    events_bus: Option<&Arc<crate::events_store::EventBusStore>>,
) -> Result<SessionOutcome, String> {
    // Fail-closed re-check on the URL actually dialed — the config was
    // validated at load time, but keeping the gate adjacent to the connect
    // call is the same convention `poll_once` follows for `http_poll`.
    crate::tick_config::validate_ws_url(url)?;

    let stream = tokio::time::timeout(
        WS_CONNECT_TIMEOUT,
        connect_source(cfg, home_dir, url, &mut state.dns),
    )
        .await
        .map_err(|_| format!("connect timed out after {}s", WS_CONNECT_TIMEOUT.as_secs()))??;

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
        headers = cfg.headers.len(),
        ping_interval_secs = cfg.ping_interval_secs,
        idle_timeout_secs = cfg.idle_timeout_secs,
        "tick websocket connected"
    );

    // D5-W2 — both clocks, `0` meaning disabled. Config validation guarantees
    // `idle > ping` whenever both are on.
    let ping_every =
        (cfg.ping_interval_secs > 0).then(|| Duration::from_secs(cfg.ping_interval_secs));
    let idle_after =
        (cfg.idle_timeout_secs > 0).then(|| Duration::from_secs(cfg.idle_timeout_secs));

    let mut non_text: u64 = 0;
    let mut last_inbound = Instant::now();
    let mut last_ping: Option<Instant> = None;

    loop {
        let wait = match watchdog_next(
            last_inbound.elapsed(),
            last_ping.map(|t| t.elapsed()),
            ping_every,
            idle_after,
        ) {
            WatchdogAction::Recycle => return Ok(SessionOutcome::IdleTimeout),
            WatchdogAction::Ping => {
                write
                    .send(Message::Ping(Vec::new().into()))
                    .await
                    .map_err(|e| format!("ping failed: {e}"))?;
                last_ping = Some(Instant::now());
                continue;
            }
            WatchdogAction::Wait(d) => d,
        };

        // `StreamExt::next` on a tungstenite stream is cancel-safe (partial
        // frames stay in the library's own read buffer), which is what lets
        // the timer arm win a race without losing data — the same `select!`
        // shape `discord.rs` uses for its heartbeat.
        let received = tokio::select! {
            message = read.next() => message,
            _ = tokio::time::sleep(wait) => continue,
        };
        let Some(message) = received else {
            // Stream ended without a Close frame.
            return Ok(SessionOutcome::Closed);
        };

        // ANY inbound frame is proof of life — text, binary, ping and pong
        // alike — so both clocks restart here, before the frame is even
        // classified.
        last_inbound = Instant::now();
        last_ping = None;

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
                return Ok(SessionOutcome::Closed);
            }
        }
    }
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

    /// A websocket source config aimed at a loopback port, with everything
    /// D5-W2 off by default. Individual tests turn on the one clock they are
    /// exercising.
    fn ws_cfg(id: &str, port: u16) -> TickSourceConfig {
        TickSourceConfig {
            id: id.into(),
            kind: TickKind::Websocket,
            enabled: true,
            interval_secs: 1,
            url: Some(format!("ws://127.0.0.1:{port}/stream")),
            command: None,
            path: None,
            subscribe: Vec::new(),
            headers: BTreeMap::new(),
            ping_interval_secs: 0,
            idle_timeout_secs: 0,
            json_fields: BTreeMap::new(),
            emit_unchanged: false,
            max_events_per_minute: 120,
            persist_every_n: 0,
            baseline_max_age_secs: crate::tick_config::DEFAULT_BASELINE_MAX_AGE_SECS,
            dns_ttl_secs: 0,
        }
    }

    /// Poll `check` until it holds or `limit` elapses. Real time, not paused —
    /// this crate's tests do not depend on the tokio `test-util` feature (see
    /// `tick_source_poll::run_source_counts_a_fetch_failure`).
    async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        check()
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

    // ── D5-W2: idle watchdog + client ping (pure) ────────────

    #[test]
    fn watchdog_does_nothing_while_both_clocks_are_disabled() {
        let action = watchdog_next(Duration::from_secs(86_400), None, None, None);
        assert_eq!(
            action,
            WatchdogAction::Wait(WATCHDOG_PARK),
            "with no clocks configured the loop just blocks on the socket"
        );
    }

    #[test]
    fn watchdog_pings_then_recycles_as_silence_grows() {
        let ping = Some(Duration::from_secs(30));
        let idle = Some(Duration::from_secs(300));

        // Fresh connection: nothing due, wait until the ping deadline.
        assert_eq!(
            watchdog_next(Duration::ZERO, None, ping, idle),
            WatchdogAction::Wait(Duration::from_secs(30))
        );
        // 10 s of silence: 20 s left on the ping clock.
        assert_eq!(
            watchdog_next(Duration::from_secs(10), None, ping, idle),
            WatchdogAction::Wait(Duration::from_secs(20))
        );
        // 30 s of silence and no ping sent yet: ping.
        assert_eq!(
            watchdog_next(Duration::from_secs(30), None, ping, idle),
            WatchdogAction::Ping
        );
        // Ping just sent: the ping clock restarts, the idle clock does not.
        assert_eq!(
            watchdog_next(Duration::from_secs(30), Some(Duration::ZERO), ping, idle),
            WatchdogAction::Wait(Duration::from_secs(30))
        );
        // Another period of silence after that ping: ping again (the prod
        // repeats, it does not fire once and give up).
        assert_eq!(
            watchdog_next(
                Duration::from_secs(60),
                Some(Duration::from_secs(30)),
                ping,
                idle
            ),
            WatchdogAction::Ping
        );
        // The idle deadline wins over another ping once it is reached.
        assert_eq!(
            watchdog_next(
                Duration::from_secs(300),
                Some(Duration::from_secs(30)),
                ping,
                idle
            ),
            WatchdogAction::Recycle
        );
        assert_eq!(
            watchdog_next(Duration::from_secs(999), None, ping, idle),
            WatchdogAction::Recycle
        );
    }

    #[test]
    fn watchdog_waits_on_whichever_clock_is_due_first() {
        let ping = Some(Duration::from_secs(30));
        let idle = Some(Duration::from_secs(300));
        // 290 s in: the idle deadline (10 s away) is nearer than the next ping
        // (30 s away), so the loop must not oversleep past the recycle.
        assert_eq!(
            watchdog_next(Duration::from_secs(290), Some(Duration::ZERO), ping, idle),
            WatchdogAction::Wait(Duration::from_secs(10))
        );
        // Watchdog only: no ping is ever produced.
        assert_eq!(
            watchdog_next(Duration::from_secs(10), None, None, idle),
            WatchdogAction::Wait(Duration::from_secs(290))
        );
        // Ping only: never recycles, however long the silence.
        assert_eq!(
            watchdog_next(
                Duration::from_secs(86_400),
                Some(Duration::ZERO),
                ping,
                None
            ),
            WatchdogAction::Wait(Duration::from_secs(30))
        );
    }

    #[test]
    fn watchdog_wait_never_collapses_to_a_spin() {
        // A deadline that is due in microseconds must not turn into a
        // zero-length sleep the loop then burns CPU on.
        let ping = Some(Duration::from_secs(30));
        let action = watchdog_next(Duration::from_micros(29_999_999), None, ping, None);
        assert_eq!(action, WatchdogAction::Wait(WATCHDOG_MIN_WAIT));
    }

    // ── D5-W2: idle watchdog + ping (live loopback server) ───

    /// A silent-but-open connection must be recycled and redialled. Without
    /// the watchdog this source would sit on a healthy-looking TCP socket
    /// forever and simply never produce another tick.
    #[tokio::test(flavor = "current_thread")]
    async fn websocket_idle_watchdog_redials_a_silent_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let server_accepts = accepts.clone();
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                server_accepts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Complete the handshake, then say nothing at all — the
                // "feed went quiet but the socket is fine" failure mode.
                tokio::spawn(async move {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await {
                        while ws.next().await.is_some() {}
                    }
                });
            }
        });

        let mut cfg = ws_cfg("ws-idle", port);
        cfg.idle_timeout_secs = 1;
        let hub = Arc::new(TickHub::new());
        let (tx, _rx) = broadcast::channel(16);
        let source = tokio::spawn(run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub.clone(), None));

        let redialled = wait_until(Duration::from_secs(15), || {
            accepts.load(std::sync::atomic::Ordering::SeqCst) >= 2
        })
        .await;
        assert!(
            redialled,
            "the idle connection was never recycled (accepts = {})",
            accepts.load(std::sync::atomic::Ordering::SeqCst)
        );

        // An idle recycle is not a dropped payload: it must not inflate the
        // fetch_error counter (D5-W2 adds no new counter, and reusing the
        // wrong one would misreport a quiet feed as a broken endpoint).
        let snap = hub.counters_snapshot("ws-idle").await;
        assert_eq!(snap.dropped_fetch_error, 0, "{snap:?}");

        source.abort();
        server.abort();
    }

    /// The client-side ping must actually leave the socket.
    #[tokio::test(flavor = "current_thread")]
    async fn websocket_pings_a_quiet_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (ping_tx, ping_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut ping_tx = Some(ping_tx);
            while let Some(Ok(message)) = ws.next().await {
                if matches!(message, Message::Ping(_)) {
                    if let Some(tx) = ping_tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        });

        let mut cfg = ws_cfg("ws-ping", port);
        cfg.ping_interval_secs = 1;
        // Watchdog off: this test is about the ping alone, and a recycle would
        // make "did a ping arrive" depend on reconnect timing.
        let hub = Arc::new(TickHub::new());
        let (tx, _rx) = broadcast::channel(16);
        let source = tokio::spawn(run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub, None));

        let pinged = tokio::time::timeout(Duration::from_secs(15), ping_rx).await;
        assert!(pinged.is_ok(), "no client ping reached the server");

        source.abort();
        server.abort();
    }

    /// Inbound traffic resets the idle clock, so a feed that keeps talking is
    /// never recycled — the watchdog must not churn a working connection.
    #[tokio::test(flavor = "current_thread")]
    async fn inbound_frames_keep_the_idle_watchdog_from_firing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let server_accepts = accepts.clone();
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                server_accepts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
                        return;
                    };
                    // Comfortably faster than the 2 s idle timeout.
                    for n in 0..40 {
                        if ws
                            .send(Message::Text(format!("{{\"p\":{n}}}").into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                });
            }
        });

        let mut cfg = ws_cfg("ws-busy", port);
        cfg.idle_timeout_secs = 2;
        cfg.json_fields = pointers(&[("p", "/p")]);
        let hub = Arc::new(TickHub::new());
        let (tx, _rx) = broadcast::channel(64);
        let source = tokio::spawn(run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub.clone(), None));

        // Long enough to cross the 2 s idle deadline twice over if the clock
        // were not being reset by the incoming frames.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(
            hub.len("ws-busy").await >= 5,
            "the feed should have produced ticks throughout"
        );
        assert_eq!(
            accepts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a talking feed must keep its single connection"
        );

        source.abort();
        server.abort();
    }

    // ── D5-W2: custom headers on the upgrade request ─────────

    #[tokio::test(flavor = "current_thread")]
    async fn websocket_upgrade_request_carries_the_configured_headers() {
        use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<Vec<(String, String)>>();

        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut seen_tx = Some(seen_tx);
            let callback = |req: &Request, response: Response| -> Result<Response, ErrorResponse> {
                let captured: Vec<(String, String)> = req
                    .headers()
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.as_str().to_ascii_lowercase(),
                            v.to_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect();
                if let Some(tx) = seen_tx.take() {
                    let _ = tx.send(captured);
                }
                Ok(response)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(socket, callback)
                .await
                .unwrap();
            let _ = ws.send(Message::Text(r#"{"p":1}"#.into())).await;
            let _ = ws.close(None).await;
        });

        let mut cfg = ws_cfg("ws-headers", port);
        cfg.headers = BTreeMap::from([
            ("X-Api-Key".to_string(), "secret-value".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ]);
        let hub = Arc::new(TickHub::new());
        let (tx, _rx) = broadcast::channel(16);
        let source = tokio::spawn(run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub, None));

        let seen = tokio::time::timeout(Duration::from_secs(15), seen_rx)
            .await
            .expect("the server never saw an upgrade request")
            .expect("handshake channel closed");
        let lookup = |name: &str| seen.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());

        assert_eq!(lookup("x-api-key").as_deref(), Some("secret-value"));
        assert_eq!(lookup("accept").as_deref(), Some("application/json"));
        // The custom headers must not have displaced the handshake's own.
        assert!(
            lookup("sec-websocket-key").is_some(),
            "the generated handshake headers survived: {seen:?}"
        );
        assert!(lookup("host").is_some());

        source.abort();
        server.abort();
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
            headers: BTreeMap::new(),
            ping_interval_secs: 0,
            idle_timeout_secs: 0,
            json_fields: pointers(&[("p", "/p")]),
            emit_unchanged: false,
            max_events_per_minute: 120,
            persist_every_n: 0,
            baseline_max_age_secs: crate::tick_config::DEFAULT_BASELINE_MAX_AGE_SECS,
            dns_ttl_secs: 0,
        };
        let source = tokio::spawn(run_source(cfg, Arc::new(std::env::temp_dir()), tx, hub.clone(), None));

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
