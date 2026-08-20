//! `GET /v1/device/ws` — the box's long-lived relay connection.
//!
//! Handshake: the server issues a random challenge; the device signs it
//! with its registered Ed25519 private key and replies; only then is the
//! connection admitted and registered. At most one live connection per
//! device — a new authenticated connection evicts (kicks) whichever
//! connection currently holds that device's slot (see `registry.rs`).
//!
//! Wire protocol (JSON text frames):
//! - Server → device: `{"type":"challenge","nonce_b64":"..."}` then, once
//!   authenticated, `{"type":"ready"}`, then any queued/live
//!   `{"type":"hook",...}` frames (see `hook.rs::HookFrame`).
//! - Device → server: `{"type":"auth","signature_b64":"..."}` in response
//!   to the challenge, and periodically `{"type":"lan_ip","ip":"..."}` to
//!   report its current LAN address (feeds `/v1/find`).
//! - Standard WebSocket ping/pong is used as the heartbeat in both
//!   directions; the server pings every 30s and closes the connection if no
//!   pong is seen for 60s.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use duduclaw_core::relay_protocol::{ClientFrame, ServerFrame};
use futures_util::sink::Sink;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::error::RelayError;
use crate::registry::{OutboundMessage, OUTBOUND_CHANNEL_CAPACITY};
use crate::state::AppState;
use crate::validate::validate_device_id;

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const PONG_TIMEOUT: Duration = Duration::from_secs(60);
/// Text/binary frames from the device are control-ish (auth + LAN-IP
/// reports) — small and infrequent. 64KB is generous headroom, not a
/// working budget.
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    device_id: String,
}

// `ClientFrame` / `ServerFrame` now live in `duduclaw_core::relay_protocol`
// (WP-E2) — imported above — so the box-side relay client authenticates
// against exactly the same wire shapes this handler speaks. `ServerFrame`
// there deliberately does NOT carry a `hook` variant (see its doc); this
// handler still sends `HookFrame` as its own independent top-level object,
// unchanged from before.

pub async fn device_ws_handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<WsQuery>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let device_id = q.device_id;

    if let Err(e) = validate_device_id(&device_id) {
        return RelayError::BadRequest(e).into_response();
    }

    if !state.ws_rate_limiter.allow(&peer.ip().to_string()).await {
        return RelayError::RateLimited.into_response();
    }

    let pubkey_b64 = match state.db.get_pubkey(&device_id) {
        Ok(Some(k)) => k,
        Ok(None) => return RelayError::NotFound.into_response(),
        Err(e) => return RelayError::from(e).into_response(),
    };
    let pubkey = match crypto::decode_pubkey(&pubkey_b64) {
        Ok(k) => k,
        Err(e) => return RelayError::Internal(e).into_response(),
    };

    let public_ip = resolve_public_ip(&headers, peer);

    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_device_socket(socket, state, device_id, pubkey, public_ip))
        .into_response()
}

/// Resolve the caller's public IP address for `mark_online`.
///
/// Same trust reasoning as `find.rs::resolve_client_ip`: prefer the last
/// `X-Forwarded-For` entry (the one appended by a trusted front-end such as
/// Cloud Run's load balancer), falling back to the raw TCP peer address
/// when no proxy is in front (local/dev runs).
fn resolve_public_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.rsplit(',').next() {
            let candidate = last.trim();
            if !candidate.is_empty() && candidate.parse::<std::net::IpAddr>().is_ok() {
                return candidate.to_string();
            }
        }
    }
    peer.ip().to_string()
}

async fn handle_device_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    device_id: String,
    pubkey: [u8; 32],
    public_ip: String,
) {
    let challenge = crypto::issue_challenge();
    let challenge_frame = ServerFrame::Challenge {
        nonce_b64: challenge.nonce_b64(),
    };
    if send_json(&mut socket, &challenge_frame).await.is_err() {
        return;
    }

    let auth_ok = match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<ClientFrame>(&text) {
            Ok(ClientFrame::Auth { signature_b64 }) => {
                crypto::verify_signature(&pubkey, &challenge, &signature_b64).is_ok()
            }
            _ => false,
        },
        _ => false,
    };

    if !auth_ok {
        tracing::warn!(device_id = %device_id, "device WS authentication failed");
        let _ = send_json(
            &mut socket,
            &ServerFrame::Error {
                message: "authentication failed".to_string(),
            },
        )
        .await;
        let _ = socket.send(Message::Close(None)).await;
        close_gracefully(&mut socket).await;
        return;
    }

    let (tx, mut rx) = tokio::sync::mpsc::channel::<OutboundMessage>(OUTBOUND_CHANNEL_CAPACITY);
    let (conn_id, mut evict_rx) = state.registry.register(&device_id, tx).await;

    if let Err(e) = state.db.mark_online(&device_id, Some(&public_ip)) {
        tracing::warn!(device_id = %device_id, error = %e, "failed to mark device online");
    }

    // Split for the main loop: sends (heartbeat/outbound/pong) and receives
    // (inbound frames) must be pollable concurrently in one `select!`.
    let (mut sink, mut stream) = socket.split();

    if send_json(&mut sink, &ServerFrame::Ready).await.is_err() {
        if state.registry.deregister_if_current(&device_id, conn_id).await {
            let _ = state.db.mark_offline(&device_id);
        }
        return;
    }

    // Flush anything queued while this device was offline, in order.
    for frame in state.offline_queue.drain(&device_id).await {
        if send_json(&mut sink, &frame).await.is_err() {
            break;
        }
    }

    tracing::info!(device_id = %device_id, public_ip = %public_ip, "device connected");

    // `interval()` fires its *first* tick immediately, which would send a
    // redundant Ping right after `ready` + the offline-queue flush.
    // `interval_at` with a first-deadline of "now + interval" waits out the
    // full period before the first heartbeat, as intended.
    let mut heartbeat =
        tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT_INTERVAL, HEARTBEAT_INTERVAL);
    let mut last_pong = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > PONG_TIMEOUT {
                    tracing::warn!(device_id = %device_id, "device heartbeat timeout");
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            _ = &mut evict_rx => {
                tracing::info!(device_id = %device_id, "connection superseded by a newer connection");
                let _ = sink.send(Message::Close(None)).await;
                close_gracefully(&mut stream).await;
                // A newer connection already owns the registry slot and is
                // "online" — don't deregister it and don't mark offline.
                return;
            }
            outbound = rx.recv() => {
                match outbound {
                    Some(OutboundMessage::Hook(frame)) => {
                        if send_json(&mut sink, &frame).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(ClientFrame::LanIp { ip }) = serde_json::from_str::<ClientFrame>(&text) {
                            if ip.parse::<std::net::IpAddr>().is_ok() {
                                if let Err(e) = state.db.update_lan_ip(&device_id, &ip) {
                                    tracing::warn!(device_id = %device_id, error = %e, "failed to record LAN IP");
                                }
                            }
                            // A syntactically invalid `ip` is silently
                            // ignored — this is a post-auth convenience
                            // report from an already-trusted device, not a
                            // security decision; malformed values just
                            // don't update anything.
                        }
                        // Unknown/unparseable frames are ignored (fail-open
                        // for forward compatibility with future box-side
                        // client versions), not treated as fatal.
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if sink.send(Message::Pong(data)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_pong = std::time::Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        tracing::info!(device_id = %device_id, "device disconnected");
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::warn!(device_id = %device_id, error = %e, "device WS read error");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    if state.registry.deregister_if_current(&device_id, conn_id).await {
        if let Err(e) = state.db.mark_offline(&device_id) {
            tracing::warn!(device_id = %device_id, error = %e, "failed to mark device offline");
        }
    }
}

async fn send_json<S, T>(sink: &mut S, value: &T) -> Result<(), axum::Error>
where
    S: Sink<Message, Error = axum::Error> + Unpin,
    T: Serialize,
{
    let text = serde_json::to_string(value).unwrap_or_default();
    sink.send(Message::Text(text.into())).await
}

/// Drain any remaining inbound frames briefly after sending a Close frame,
/// so the peer's own close-handshake reply is consumed before the socket is
/// dropped. Dropping a WebSocket immediately after sending Close races the
/// peer's Close reply still in flight; if it arrives after the local fd is
/// closed, the peer observes a TCP RST ("reset without closing handshake")
/// instead of a clean close. Bounded so a peer that never replies can't hang
/// this task.
async fn close_gracefully<St>(stream: &mut St)
where
    St: futures_util::Stream + Unpin,
{
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        while stream.next().await.is_some() {}
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_public_ip_prefers_last_xff_entry() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 5.6.7.8".parse().unwrap());
        let peer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert_eq!(resolve_public_ip(&headers, peer), "5.6.7.8");
    }

    #[test]
    fn resolve_public_ip_falls_back_to_peer_when_header_absent() {
        let headers = HeaderMap::new();
        let peer: SocketAddr = "203.0.113.1:9999".parse().unwrap();
        assert_eq!(resolve_public_ip(&headers, peer), "203.0.113.1");
    }

    #[test]
    fn resolve_public_ip_falls_back_when_header_is_garbage() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        let peer: SocketAddr = "203.0.113.1:9999".parse().unwrap();
        assert_eq!(resolve_public_ip(&headers, peer), "203.0.113.1");
    }
}
