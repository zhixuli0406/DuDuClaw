//! `GET /ws/computer-view?ticket=…`: the dashboard's live view of one
//! tool-driven session (P8), as an RFB stream over a WebSocket for noVNC.
//!
//! The ticket comes from the `computer_sessions.view` / `.takeover` RPCs on
//! the authenticated dashboard socket ([`super::live_ops`]); it is single
//! use and valid for 30 s. On connect the Origin is checked like `/ws`, the
//! ticket is consumed, the identity it was issued to is re-read from
//! `users.db` and authorized again, and the viewer count is capped. While
//! connected the identity is re-checked every 30 s and the connection
//! closes when the session ends or changes.
//!
//! Bytes from the viewer pass the RFB filter ([`super::rfb`]): input
//! messages reach the VNC server only while this viewer's account holds an
//! active takeover lease, and they restart the lease's idle clock. Every
//! connection's start and end is audited (who, how long, how many input
//! messages were forwarded), never what was shown or typed.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};

use super::live_ops::live_identity;
use super::live_view::{
    LiveAction, LiveState, MAX_VIEWERS_PER_SESSION, ViewTicket, authorize, operator_label,
};
use super::rfb::{ClientFilter, Kind, RfbError};
use super::{ComputerUseSessions, Entry, audit_entry, write_audit};

/// The route path.
pub const VIEW_PATH: &str = "/ws/computer-view";
/// Largest viewer WebSocket message.
const MAX_WS_MESSAGE: usize = 1024 * 1024;
/// How often the bridge checks the session and the viewer's identity.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);
const REAUTH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub struct ViewQuery {
    #[serde(default)]
    ticket: String,
}

pub fn router(sessions: Arc<ComputerUseSessions>) -> Router {
    Router::new()
        .route(VIEW_PATH, get(handle))
        .with_state(sessions)
}

/// A viewer that passed every check, holding one of the session's viewer
/// slots until dropped.
pub(crate) struct Admitted {
    pub(crate) entry: Entry,
    pub(crate) ticket: ViewTicket,
    pub(crate) user_id: String,
    pub(crate) label: String,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        self.entry.shared.live.viewers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Ticket, identity, authorization, live session, viewer cap. Pure apart
/// from the identity read; the WebSocket upgrade happens only after this.
pub(crate) fn admit_viewer(
    sessions: &ComputerUseSessions,
    token: &str,
    now: Instant,
) -> Result<Admitted, StatusCode> {
    let ticket = sessions
        .tickets
        .consume(token, now)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let live = live_identity(sessions.home(), &ticket.ctx).ok_or(StatusCode::FORBIDDEN)?;
    if !authorize(&live, &ticket.agent_id, LiveAction::Watch) {
        return Err(StatusCode::FORBIDDEN);
    }
    let entry = sessions
        .live_entry(&ticket.agent_id, Some(&ticket.session_id))
        .ok_or(StatusCode::NOT_FOUND)?;
    let before = entry.shared.live.viewers.fetch_add(1, Ordering::AcqRel);
    let admitted = Admitted {
        entry,
        user_id: live.user_id.clone(),
        label: operator_label(&live),
        ticket,
    };
    if before >= MAX_VIEWERS_PER_SESSION {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Ok(admitted)
}

/// Split viewer bytes into what reaches the VNC server: display messages
/// always, input only while `user_id` holds an active lease (which also
/// restarts its idle clock), dropped kinds never. Returns the bytes to
/// forward and how many input messages were forwarded.
pub(crate) fn filter_viewer_bytes(
    filter: &mut ClientFilter,
    live: &LiveState,
    user_id: &str,
    now: Instant,
    data: &[u8],
) -> Result<(Vec<u8>, u64), RfbError> {
    let mut out = Vec::with_capacity(data.len());
    let mut inputs = 0;
    for msg in filter.feed(data)? {
        match msg.kind {
            Kind::Forward => out.extend_from_slice(&msg.bytes),
            Kind::Input => {
                if live.note_input(user_id, now) {
                    inputs += 1;
                    out.extend_from_slice(&msg.bytes);
                }
            }
            Kind::Drop => {}
        }
    }
    Ok((out, inputs))
}

async fn handle(
    State(sessions): State<Arc<ComputerUseSessions>>,
    Query(query): Query<ViewQuery>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    if !crate::server::check_ws_rate_limit(addr.ip()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if !crate::server::origin_is_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let admitted = match admit_viewer(&sessions, &query.ticket, Instant::now()) {
        Ok(a) => a,
        Err(status) => return status.into_response(),
    };
    ws.protocols(["binary"])
        .max_message_size(MAX_WS_MESSAGE)
        .on_upgrade(move |socket| bridge(socket, sessions, admitted))
}

async fn bridge(socket: WebSocket, sessions: Arc<ComputerUseSessions>, admitted: Admitted) {
    let agent = admitted.ticket.agent_id.clone();
    let session_id = admitted.ticket.session_id.clone();
    let home = sessions.home().to_path_buf();
    let started = Instant::now();
    write_audit(
        home.clone(),
        audit_entry(
            &agent,
            "view_start",
            json!({"session_id": session_id, "operator": admitted.label}),
            None,
        ),
    )
    .await;
    info!(agent = %agent, session = %session_id, "computer-use live view opened");
    let forwarded = run_bridge(socket, &sessions, &admitted).await;
    write_audit(
        home,
        audit_entry(
            &agent,
            "view_stop",
            json!({
                "session_id": session_id,
                "operator": admitted.label,
                "duration_secs": started.elapsed().as_secs(),
                "inputs_forwarded": forwarded,
            }),
            None,
        ),
    )
    .await;
}

/// The byte pump. Returns how many input messages were forwarded.
async fn run_bridge(socket: WebSocket, sessions: &ComputerUseSessions, admitted: &Admitted) -> u64 {
    let Some(access) = admitted.entry.shared.live.access() else {
        return 0;
    };
    let relay = match access.open_relay().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "computer-use live view relay did not open");
            return 0;
        }
    };
    let (mut relay_rx, mut relay_tx) = tokio::io::split(relay);
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut filter = ClientFilter::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut ticks = tokio::time::interval(CHECK_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_auth = Instant::now();
    let mut forwarded = 0u64;
    let live = &admitted.entry.shared.live;
    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                let data = match msg {
                    Some(Ok(Message::Binary(data))) => data,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    _ => break,
                };
                let (bytes, inputs) = match filter_viewer_bytes(&mut filter, live, &admitted.user_id, Instant::now(), &data) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "computer-use live view closed: viewer stream refused");
                        break;
                    }
                };
                forwarded += inputs;
                if !bytes.is_empty() && relay_tx.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            n = relay_rx.read(&mut buf) => {
                match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if ws_tx.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            _ = ticks.tick() => {
                if sessions
                    .live_entry(&admitted.ticket.agent_id, Some(&admitted.ticket.session_id))
                    .is_none()
                {
                    break;
                }
                if last_auth.elapsed() >= REAUTH_INTERVAL {
                    last_auth = Instant::now();
                    let still = live_identity(sessions.home(), &admitted.ticket.ctx)
                        .is_some_and(|l| authorize(&l, &admitted.ticket.agent_id, LiveAction::Watch));
                    if !still {
                        break;
                    }
                }
            }
        }
    }
    let _ = ws_tx.send(Message::Close(None)).await;
    forwarded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use_sessions::live_view::TakeoverLease;

    fn handshake() -> Vec<u8> {
        let mut v = b"RFB 003.008\n".to_vec();
        v.push(2);
        v.extend_from_slice(&[0u8; 16]);
        v.push(1);
        v
    }

    #[test]
    fn only_the_lease_holder_input_is_forwarded() {
        let live = LiveState::default();
        let now = Instant::now();
        let key = [4u8, 1, 0, 0, 0, 0, 0, 0x61];
        let update = [3u8, 1, 0, 0, 0, 0, 0, 10, 0, 10];

        // No lease: the handshake and display requests pass, input does not.
        let mut f = ClientFilter::new();
        let mut data = handshake();
        data.extend_from_slice(&update);
        data.extend_from_slice(&key);
        let (out, inputs) = filter_viewer_bytes(&mut f, &live, "u1", now, &data).unwrap();
        assert_eq!(inputs, 0);
        assert_eq!(out.len(), handshake().len() + update.len());

        // Somebody else's lease: still nothing.
        *live.takeover.lock().unwrap() =
            Some(TakeoverLease::new("u2", "u2", now, Duration::from_secs(600)));
        let (out, inputs) = filter_viewer_bytes(&mut f, &live, "u1", now, &key).unwrap();
        assert!(out.is_empty() && inputs == 0);

        // Own lease: forwarded and counted.
        let (out, inputs) = filter_viewer_bytes(&mut f, &live, "u2", now, &key).unwrap();
        assert_eq!((out.as_slice(), inputs), (&key[..], 1));

        // Expired lease: dropped again.
        let later = now + Duration::from_secs(601);
        let (out, _) = filter_viewer_bytes(&mut f, &live, "u2", later, &key).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn resize_requests_never_reach_the_server_even_for_the_holder() {
        let live = LiveState::default();
        let now = Instant::now();
        *live.takeover.lock().unwrap() =
            Some(TakeoverLease::new("u1", "u1", now, Duration::from_secs(600)));
        let mut f = ClientFilter::new();
        filter_viewer_bytes(&mut f, &live, "u1", now, &handshake()).unwrap();
        let mut resize = vec![251u8, 0, 0, 100, 0, 100, 1, 0];
        resize.extend_from_slice(&[0u8; 16]);
        let (out, _) = filter_viewer_bytes(&mut f, &live, "u1", now, &resize).unwrap();
        assert!(out.is_empty());
    }
}
