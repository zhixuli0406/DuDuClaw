//! WP-E2 — box-side relay client.
//!
//! The long-lived WebSocket connection to `duduclaw-relay`
//! (`crates/duduclaw-relay`) that lets a NAT/CGNAT'd gateway receive
//! inbound webhooks (LINE today) it otherwise could not accept directly.
//! See `crates/duduclaw-relay/README.md` for the cloud-side narrative; the
//! wire types this client speaks (`HookFrame`/`ClientFrame`/`ServerFrame`/
//! `validate_device_id`) live in `duduclaw_core::relay_protocol` so the two
//! independently-deployed halves cannot drift on a field name or a
//! `#[serde(rename)]` tag.
//!
//! Disabled by default (`config.toml [relay] enabled = false`); in
//! appliance mode (`DUDUCLAW_APPLIANCE=1`) it defaults ON against
//! [`relay_config::OFFICIAL_RELAY_URL`] unless the operator overrides it —
//! see `relay_config.rs`.
//!
//! Flow: load-or-generate an Ed25519 device identity
//! (`relay_device::RelayDeviceIdentity`) → TOFU-register with the relay
//! (idempotent, safe to retry every reconnect) → open the WSS connection →
//! answer the server's Ed25519 challenge → periodically report the box's
//! LAN IP (feeds `/v1/find`) → for every `HookFrame` with `channel ==
//! "line"`, run it through the SAME signature-verification + event-dispatch
//! path the direct `/webhook/line` HTTP route uses
//! (`line::handle_line_webhook` — never a second copy). Any other channel
//! is dropped and counted as `unsupported`.
//!
//! Reconnects with the same exponential-backoff shape as the resident-sensing
//! websocket source (`tick_source_ws::ws_backoff_delay` — 1s start, ×2,
//! capped at 60s, up to +25% jitter, reset after a ≥60s session). The
//! relay's own Cloud Run deployment forces a disconnect roughly every 3600s
//! (`crates/duduclaw-relay/README.md`) — that is expected steady-state
//! behavior, not a failure, so a routine reconnect is logged at `debug`,
//! never `warn`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use duduclaw_core::relay_protocol::{ClientFrame, DeviceInboundFrame, HookFrame, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::channel_reply::ReplyContext;
use crate::line::LineState;
use crate::relay_config::RelayConfig;
use crate::relay_device::RelayDeviceIdentity;

/// Starting delay for the reconnect backoff — reused as the `interval_secs`
/// argument to `tick_source_ws::ws_backoff_secs`/`ws_backoff_delay`, which
/// already implements exactly the shape this client wants (×2 per retry,
/// capped at 60s, up to +25% jitter). No duplicate backoff implementation.
const BACKOFF_START_SECS: u64 = 1;
/// A session alive at least this long resets the backoff retry counter —
/// mirrors `tick_source_ws::WS_STABLE_SESSION`. The relay's own ~3600s
/// forced Cloud Run cutoff is far above this, so a normal healthy session
/// always resets it.
const STABLE_SESSION: Duration = Duration::from_secs(60);
/// How often the box reports its LAN IP to the relay (feeds `/v1/find`).
const LAN_IP_REPORT_INTERVAL: Duration = Duration::from_secs(300);
/// Upper bound on connect + challenge-response handshake latency.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Spawn the relay client task iff `[relay] enabled` resolves true (see
/// `relay_config::RelayConfig::from_home` for the appliance-aware default).
/// No-op — spawns nothing — otherwise; the caller does not need to
/// pre-check the config.
pub fn spawn_relay_client(home_dir: &Path, ctx: Arc<ReplyContext>) {
    let cfg = RelayConfig::from_home(home_dir);
    if !cfg.enabled {
        info!("relay client: disabled ([relay] enabled = false) — not started");
        return;
    }
    if cfg.url.trim().is_empty() {
        warn!("relay client: enabled but [relay] url is empty — not started");
        return;
    }
    let home_dir = home_dir.to_path_buf();
    tokio::spawn(async move {
        run_relay_client(home_dir, ctx, cfg).await;
    });
}

/// The outer reconnect loop. Never returns — aborted only by gateway
/// shutdown (task drop), same lifecycle as every other resident background
/// loop in the gateway (heartbeat scheduler, tick sources, ...).
async fn run_relay_client(home_dir: PathBuf, ctx: Arc<ReplyContext>, cfg: RelayConfig) {
    let key_path = RelayDeviceIdentity::default_key_path(&home_dir);
    let identity = match RelayDeviceIdentity::load_or_generate(&key_path) {
        Ok((identity, generated)) => {
            if generated {
                info!(device_id = %identity.device_id(), "relay client: generated a new device identity");
            }
            identity
        }
        Err(e) => {
            warn!("relay client: failed to load/generate device identity ({e}) — not started");
            return;
        }
    };
    info!(device_id = %identity.device_id(), url = %cfg.url, "relay client starting");

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let mut retries: u32 = 0;
    loop {
        // TOFU register on every attempt — idempotent and safe (same-key
        // re-registration is a no-op 200 on the relay side, see
        // `duduclaw-relay/src/device_register.rs`), so a transient relay
        // restart never leaves this box permanently unregistered.
        if let Err(e) = register_device(&http, &cfg.url, &identity, cfg.device_name.as_deref()).await {
            warn!(device_id = %identity.device_id(), error = %e, "relay client: device registration failed");
            crate::metrics::global_metrics().relay_reconnect();
            retries = back_off(retries).await;
            continue;
        }

        crate::metrics::global_metrics().relay_reconnect();
        let started = Instant::now();
        let outcome = run_session(&cfg.url, &identity, &home_dir, &ctx).await;
        let alive = started.elapsed();
        crate::metrics::global_metrics().set_relay_connected(false);

        match outcome {
            Ok(()) => debug!(
                device_id = %identity.device_id(),
                session_secs = alive.as_secs(),
                "relay client: session ended — reconnecting"
            ),
            Err(e) => warn!(
                device_id = %identity.device_id(),
                error = %e,
                session_secs = alive.as_secs(),
                "relay client: session error — reconnecting"
            ),
        }

        if alive >= STABLE_SESSION {
            retries = 0;
        }
        retries = back_off(retries).await;
    }
}

/// Sleep out one backoff step and return the incremented retry counter.
async fn back_off(retries: u32) -> u32 {
    let delay = crate::tick_source_ws::ws_backoff_delay(BACKOFF_START_SECS, retries, jitter_seed());
    tokio::time::sleep(delay).await;
    retries.saturating_add(1)
}

/// Nanosecond-of-second seed for the backoff jitter — same source of
/// entropy (no RNG state to carry around a resident loop) as
/// `tick_source_ws::jitter_seed` / `discord::invalid_session_jitter_ms`.
fn jitter_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// `POST /v1/device/register` (TOFU). Safe to call repeatedly with the same
/// key — the relay treats it as idempotent.
async fn register_device(
    http: &reqwest::Client,
    base_url: &str,
    identity: &RelayDeviceIdentity,
    device_name: Option<&str>,
) -> Result<(), String> {
    let url = crate::relay_config::register_endpoint(base_url);
    let mut body = serde_json::json!({
        "device_id": identity.device_id(),
        "pubkey_b64": identity.pubkey_b64(),
    });
    if let Some(name) = device_name {
        body["name"] = serde_json::json!(name);
    }
    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("register request failed: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("register rejected: HTTP {}", resp.status()))
    }
}

/// One connect → authenticate → stream session. Returns `Ok(())` on any
/// clean disconnect (peer close, end of stream) and `Err` on a protocol or
/// transport failure — both are always followed by a reconnect in
/// [`run_relay_client`]'s outer loop; the distinction only affects the log
/// level the caller uses.
async fn run_session(
    base_url: &str,
    identity: &RelayDeviceIdentity,
    home_dir: &Path,
    ctx: &Arc<ReplyContext>,
) -> Result<(), String> {
    let ws_url = crate::relay_config::hook_ws_endpoint(base_url, identity.device_id())
        .map_err(|e| format!("bad relay url: {e}"))?;

    let (ws_stream, _resp) = tokio::time::timeout(HANDSHAKE_TIMEOUT, tokio_tungstenite::connect_async(&ws_url))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("ws connect failed: {e}"))?;

    let (mut sink, mut stream) = ws_stream.split();

    // ── Ed25519 challenge-response ──
    let challenge_text = next_text_frame(&mut stream, HANDSHAKE_TIMEOUT).await?;
    let nonce_b64 = match duduclaw_core::relay_protocol::parse_device_inbound(&challenge_text) {
        DeviceInboundFrame::Control(ServerFrame::Challenge { nonce_b64 }) => nonce_b64,
        DeviceInboundFrame::Control(ServerFrame::Error { message }) => {
            return Err(format!("relay rejected connection before auth: {message}"))
        }
        other => return Err(format!("expected a challenge frame, got {other:?}")),
    };
    let nonce = BASE64
        .decode(&nonce_b64)
        .map_err(|e| format!("bad challenge nonce base64: {e}"))?;
    let signature_b64 = identity.sign(&nonce);
    send_client_frame(&mut sink, &ClientFrame::Auth { signature_b64 }).await?;

    let ready_text = next_text_frame(&mut stream, HANDSHAKE_TIMEOUT).await?;
    match duduclaw_core::relay_protocol::parse_device_inbound(&ready_text) {
        DeviceInboundFrame::Control(ServerFrame::Ready) => {}
        DeviceInboundFrame::Control(ServerFrame::Error { message }) => {
            return Err(format!("relay auth rejected: {message}"))
        }
        other => return Err(format!("expected a ready frame, got {other:?}")),
    }

    info!(device_id = %identity.device_id(), "relay client: connected and authenticated");
    crate::metrics::global_metrics().set_relay_connected(true);

    let line_state = LineState::new(home_dir, ctx.clone());

    let mut lan_ip_interval = tokio::time::interval(LAN_IP_REPORT_INTERVAL);
    // `interval()`'s first tick fires immediately — consume it so the first
    // report happens on schedule (5 min in), not the instant `ready` lands.
    lan_ip_interval.tick().await;

    loop {
        tokio::select! {
            _ = lan_ip_interval.tick() => {
                if let Some(ip) = local_private_ipv4() {
                    let frame = ClientFrame::LanIp { ip: ip.to_string() };
                    send_client_frame(&mut sink, &frame).await?;
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        handle_hook_text(&text, &line_state).await;
                    }
                    Some(Ok(Message::Ping(data))) => {
                        sink.send(Message::Pong(data)).await.map_err(|e| format!("ws pong failed: {e}"))?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => return Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(format!("ws read error: {e}")),
                }
            }
        }
    }
}

async fn next_text_frame<S>(stream: &mut S, timeout: Duration) -> Result<String, String>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    match tokio::time::timeout(timeout, stream.next()).await {
        Err(_) => Err("timed out waiting for a frame".to_string()),
        Ok(None) => Err("connection closed before expected frame".to_string()),
        Ok(Some(Err(e))) => Err(format!("ws read error: {e}")),
        Ok(Some(Ok(Message::Text(text)))) => Ok(text.to_string()),
        Ok(Some(Ok(other))) => Err(format!("expected a text frame, got {other:?}")),
    }
}

async fn send_client_frame<S>(sink: &mut S, frame: &ClientFrame) -> Result<(), String>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let text = serde_json::to_string(frame).unwrap_or_default();
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|e| format!("ws send failed: {e}"))
}

/// Dispatch one inbound text frame. Only `hook` frames matter here — a
/// stray `challenge`/`ready`/`error` this deep in the session (the relay
/// never re-sends them post-handshake) is silently ignored, matching the
/// relay's own fail-open handling of unrecognized device→server frames.
async fn handle_hook_text(text: &str, line_state: &LineState) {
    match duduclaw_core::relay_protocol::parse_device_inbound(text) {
        DeviceInboundFrame::Hook(frame) => route_hook(frame, line_state).await,
        DeviceInboundFrame::Control(_) => {}
        DeviceInboundFrame::Unknown => {
            warn!("relay client: unparseable frame from relay — dropped");
        }
    }
}

/// Route one [`HookFrame`] by its `channel`. Only `line` is wired up
/// (`Channel::forwarded_headers` / the relay's own `Channel::parse` don't
/// know about anything else yet); every other value is dropped and counted
/// as `unsupported`, never silently accepted.
async fn route_hook(frame: HookFrame, line_state: &LineState) {
    match frame.channel.as_str() {
        "line" => inject_line_hook(frame, line_state).await,
        other => {
            debug!(channel = other, frame_id = %frame.id, "relay client: unsupported channel — dropped");
            crate::metrics::global_metrics().relay_frame(other, "unsupported").await;
        }
    }
}

/// Decode a `line`-channel [`HookFrame`] and run it through the SAME
/// verify+dispatch entry point the direct `/webhook/line` HTTP route uses
/// (`line::handle_line_webhook`) — fail-closed: any signature-verification
/// failure (missing/invalid `X-Line-Signature`, unparseable body, LINE not
/// configured with a secret) is dropped and counted, never retried, never
/// forwarded to the agent pipeline.
async fn inject_line_hook(frame: HookFrame, line_state: &LineState) {
    let body = match BASE64.decode(&frame.body_b64) {
        Ok(b) => b,
        Err(e) => {
            warn!(frame_id = %frame.id, error = %e, "relay client: hook frame body_b64 did not decode — dropped");
            crate::metrics::global_metrics().relay_frame("line", "bad_signature").await;
            return;
        }
    };

    let mut headers = HeaderMap::new();
    for (name, value) in &frame.headers {
        match (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
            (Ok(name), Ok(value)) => {
                headers.insert(name, value);
            }
            _ => {
                // A header the relay forwarded that doesn't survive
                // re-parsing as an HTTP header is dropped from the
                // reconstructed request, not fatal to the whole frame —
                // `handle_line_webhook` will simply see it as absent (which,
                // for `x-line-signature`, fails closed on its own).
                warn!(frame_id = %frame.id, header = %name, "relay client: header did not survive re-parse — dropped");
            }
        }
    }

    let status = crate::line::handle_line_webhook(line_state.clone(), &headers, Bytes::from(body)).await;
    let outcome = if status.is_success() { "ok" } else { "bad_signature" };
    if outcome == "bad_signature" {
        warn!(frame_id = %frame.id, status = %status, "relay client: LINE webhook verification failed — dropped");
    }
    crate::metrics::global_metrics().relay_frame("line", outcome).await;
}

/// The box's LAN-facing IPv4 address, if it has one. Uses the standard
/// dependency-free "UDP connect" trick (no packet actually leaves the
/// host — `connect()` on a UDP socket only asks the OS to pick the local
/// address it would route through to reach the given destination), which
/// works identically on macOS/Linux/Windows without a platform-specific
/// interface-enumeration API. `None` on any failure — this is a best-effort
/// convenience report (`/v1/find`'s "same office network" grouping), never
/// load-bearing.
fn local_private_ipv4() -> Option<std::net::Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if is_reportable_lan_ipv4(v4) => Some(v4),
        _ => None,
    }
}

/// Pure classifier split out of [`local_private_ipv4`] so the "which
/// addresses are worth reporting" decision is unit-testable without a real
/// socket. Private (RFC 1918) and not loopback/link-local.
fn is_reportable_lan_ipv4(ip: std::net::Ipv4Addr) -> bool {
    ip.is_private() && !ip.is_loopback() && !ip.is_link_local()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    // ── is_reportable_lan_ipv4 (pure) ─────────────────────────────────

    #[test]
    fn reports_typical_private_ranges() {
        assert!(is_reportable_lan_ipv4(Ipv4Addr::new(192, 168, 1, 23)));
        assert!(is_reportable_lan_ipv4(Ipv4Addr::new(10, 0, 0, 5)));
        assert!(is_reportable_lan_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
    }

    #[test]
    fn excludes_loopback_link_local_and_public() {
        assert!(!is_reportable_lan_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_reportable_lan_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(!is_reportable_lan_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    // ── handle_hook_text / route_hook / inject_line_hook ──────────────
    //
    // These exercise the fail-closed injection path with a hand-built
    // `LineState` (no real ReplyContext side effects reachable before the
    // signature check runs — see `line::handle_line_webhook`'s doc).

    /// The `relay_frame` counters live in the process-global metrics
    /// registry (`crate::metrics::global_metrics()`), so tests that assert
    /// on a specific `(channel, outcome)` before/after delta must not run
    /// concurrently with each other — otherwise two tests incrementing the
    /// same key interleave and one observes the other's increment. Mirrors
    /// `tick_config.rs`'s `ENV_LOCK` pattern for a different flavor of
    /// shared global state (there: process env vars; here: an in-process
    /// counter map). `tokio::sync::Mutex` (not `std::sync::Mutex`) because
    /// the guard is held across `.await` points.
    static METRICS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn test_ctx(home: &std::path::Path) -> Arc<ReplyContext> {
        let registry = Arc::new(tokio::sync::RwLock::new(duduclaw_agent::AgentRegistry::new(
            home.join("agents"),
        )));
        let sessions = Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        let status: crate::channel_reply::ChannelStatusMap =
            Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        Arc::new(ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx))
    }

    fn write_line_config(home: &std::path::Path, secret: &str, token: &str) {
        std::fs::write(
            home.join("config.toml"),
            format!("[channels]\nline_channel_token = \"{token}\"\nline_channel_secret = \"{secret}\"\n"),
        )
        .unwrap();
    }

    fn line_signature(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        BASE64.encode(mac.finalize().into_bytes())
    }

    fn hook_frame_for(channel: &str, headers: std::collections::BTreeMap<String, String>, body: &[u8]) -> String {
        let frame = serde_json::json!({
            "type": "hook",
            "id": "test-frame-1",
            "channel": channel,
            "headers": headers,
            "body_b64": BASE64.encode(body),
            "received_at": "2026-08-18T12:00:00Z",
        });
        frame.to_string()
    }

    #[tokio::test]
    async fn valid_line_signature_yields_ok_outcome() {
        let _guard = METRICS_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        write_line_config(dir.path(), "test-secret", "test-token");
        let ctx = test_ctx(dir.path());
        let line_state = LineState::new(dir.path(), ctx);

        let body = br#"{"events":[]}"#;
        let sig = line_signature("test-secret", body);
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("x-line-signature".to_string(), sig);
        let text = hook_frame_for("line", headers, body);

        let before = relay_frame_count("line", "ok").await;
        handle_hook_text(&text, &line_state).await;
        let after = relay_frame_count("line", "ok").await;
        assert_eq!(after, before + 1, "a validly-signed frame must count as ok");
    }

    #[tokio::test]
    async fn invalid_line_signature_yields_bad_signature_outcome() {
        let _guard = METRICS_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        write_line_config(dir.path(), "test-secret", "test-token");
        let ctx = test_ctx(dir.path());
        let line_state = LineState::new(dir.path(), ctx);

        let body = br#"{"events":[]}"#;
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert("x-line-signature".to_string(), "totally-wrong-signature".to_string());
        let text = hook_frame_for("line", headers, body);

        let before = relay_frame_count("line", "bad_signature").await;
        handle_hook_text(&text, &line_state).await;
        let after = relay_frame_count("line", "bad_signature").await;
        assert_eq!(after, before + 1, "an invalid signature must fail closed and be counted");
    }

    #[tokio::test]
    async fn malformed_body_b64_is_dropped_as_bad_signature() {
        let _guard = METRICS_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        write_line_config(dir.path(), "test-secret", "test-token");
        let ctx = test_ctx(dir.path());
        let line_state = LineState::new(dir.path(), ctx);

        let text = serde_json::json!({
            "type": "hook",
            "id": "test-frame-2",
            "channel": "line",
            "headers": {},
            "body_b64": "not-valid-base64!!!",
            "received_at": "2026-08-18T12:00:00Z",
        })
        .to_string();

        let before = relay_frame_count("line", "bad_signature").await;
        handle_hook_text(&text, &line_state).await;
        let after = relay_frame_count("line", "bad_signature").await;
        assert_eq!(after, before + 1);
    }

    #[tokio::test]
    async fn unsupported_channel_is_dropped_and_counted() {
        let _guard = METRICS_TEST_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let line_state = LineState::new(dir.path(), ctx);

        let text = hook_frame_for("whatsapp", std::collections::BTreeMap::new(), b"{}");

        let before = relay_frame_count("whatsapp", "unsupported").await;
        handle_hook_text(&text, &line_state).await;
        let after = relay_frame_count("whatsapp", "unsupported").await;
        assert_eq!(after, before + 1);
    }

    #[tokio::test]
    async fn garbage_text_does_not_panic_and_touches_no_counter() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let line_state = LineState::new(dir.path(), ctx);
        // Must simply return — no panic, no counter side effect for input
        // that isn't even a recognizable frame shape.
        handle_hook_text("not json at all", &line_state).await;
        handle_hook_text(r#"{"type":"ready"}"#, &line_state).await;
    }

    async fn relay_frame_count(channel: &str, outcome: &str) -> u64 {
        crate::metrics::global_metrics()
            .relay_frames
            .read()
            .await
            .get(&(channel.to_string(), outcome.to_string()))
            .copied()
            .unwrap_or(0)
    }

    // ── reconnect backoff (pure — reused from tick_source_ws, not
    //    reimplemented; this just documents/locks the shape this client
    //    actually gets by calling it with BACKOFF_START_SECS) ──────────

    #[test]
    fn backoff_starts_at_one_second_doubles_and_caps_at_60s() {
        use crate::tick_source_ws::ws_backoff_secs;
        assert_eq!(ws_backoff_secs(BACKOFF_START_SECS, 0), 1);
        assert_eq!(ws_backoff_secs(BACKOFF_START_SECS, 1), 2);
        assert_eq!(ws_backoff_secs(BACKOFF_START_SECS, 2), 4);
        assert_eq!(ws_backoff_secs(BACKOFF_START_SECS, 6), 64u64.min(60));
        assert_eq!(ws_backoff_secs(BACKOFF_START_SECS, 10), 60, "capped at 60s");
    }

    #[test]
    fn backoff_delay_never_exceeds_60s_plus_25_percent_jitter() {
        use crate::tick_source_ws::ws_backoff_delay;
        for retries in 0..12 {
            for seed in [0u32, 12345, u32::MAX] {
                let d = ws_backoff_delay(BACKOFF_START_SECS, retries, seed);
                assert!(
                    d <= Duration::from_millis(60_000 + 60_000 / 4),
                    "retries={retries} seed={seed} delay={d:?} exceeds the 60s+25% ceiling"
                );
            }
        }
    }

    // ── live end-to-end: a real duduclaw-relay server, not a mock ──────
    //
    // This is the strongest protocol-compatibility proof available: it does
    // not just check that both sides agree on a Rust type (relay_protocol.rs
    // already locks that at the type level), it drives the actual
    // `duduclaw-relay` router — TOFU registration, the real Ed25519
    // challenge-response over a real WebSocket upgrade, and a real
    // `/v1/hook/line/{device_id}` POST exactly as LINE would send one — and
    // asserts the gateway's client produces the same outcome the direct
    // `/webhook/line` HTTP route would.

    async fn spawn_real_relay() -> (String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = duduclaw_relay::db::RelayDb::open(&dir.path().join("relay.db")).unwrap();
        let state = duduclaw_relay::state::AppState::new(db);
        let app = duduclaw_relay::build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        // Give the accept loop a moment to be ready (same margin the relay's
        // own integration tests use).
        tokio::time::sleep(Duration::from_millis(30)).await;
        (format!("http://{addr}"), dir)
    }

    async fn wait_until<F: Fn() -> bool>(cond: F, what: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !cond() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for: {what}"));
    }

    #[tokio::test]
    async fn relay_client_round_trips_a_real_line_webhook_through_a_real_relay_server() {
        let _guard = METRICS_TEST_LOCK.lock().await;
        let (base_url, _relay_dir) = spawn_real_relay().await;

        let home = tempfile::tempdir().unwrap();
        write_line_config(home.path(), "e2e-secret", "e2e-token");
        let ctx = test_ctx(home.path());

        let key_path = RelayDeviceIdentity::default_key_path(home.path());
        let (identity, _generated) = RelayDeviceIdentity::load_or_generate(&key_path).unwrap();
        let device_id = identity.device_id().to_string();

        let http = reqwest::Client::new();
        register_device(&http, &base_url, &identity, Some("e2e-box"))
            .await
            .expect("registration against the real relay must succeed");

        // Drive one session against the real relay in the background.
        let session_home = home.path().to_path_buf();
        let session_url = base_url.clone();
        let session_ctx = ctx.clone();
        let session_handle = tokio::spawn(async move {
            let _ = run_session(&session_url, &identity, &session_home, &session_ctx).await;
        });

        wait_until(
            || {
                crate::metrics::global_metrics()
                    .relay_connected
                    .load(std::sync::atomic::Ordering::Relaxed)
                    == 1
            },
            "relay client to authenticate against the real relay",
        )
        .await;

        // POST a real LINE-shaped webhook straight at the relay, exactly as
        // LINE's own servers would — proves the relay's own header
        // allowlist + body forwarding round-trips through this client's
        // `HookFrame` parsing and into a correct signature verdict.
        let body: &[u8] = br#"{"events":[]}"#;
        let sig = line_signature("e2e-secret", body);
        let before_ok = relay_frame_count("line", "ok").await;
        let resp = http
            .post(format!("{base_url}/v1/hook/line/{device_id}"))
            .header("x-line-signature", sig)
            .header("content-type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "the relay always answers 200 once structurally valid");

        tokio::time::timeout(Duration::from_secs(5), async {
            while relay_frame_count("line", "ok").await <= before_ok {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("relay client did not process the forwarded hook frame in time");

        session_handle.abort();
    }
}
