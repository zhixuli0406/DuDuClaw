//! `POST /webhook/mcp-events/{id}` — accepting one MCP Events delivery.
//!
//! Order of checks (cheapest first, nothing parsed before the signature):
//! id shape and existence (404) → body size (413) → per-subscription rate
//! (429) → required headers (400) → timestamp window and HMAC (401, audited)
//! → `X-MCP-Subscription-Id` against the ids the upstream returned (401) →
//! replay cache (a repeated `webhook-id` is acknowledged and dropped) →
//! body. A terminated subscription answers 410 so the upstream stops.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use super::signature;
use super::store::{self, SubStatus};

/// Largest delivery body accepted (the draft's delivery profile: 256 KiB).
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Deliveries per subscription per minute.
pub const RATE_PER_MINUTE: u32 = 120;
/// Event `data` larger than this (serialized) is replaced by a truncated text.
pub const MAX_DATA_BYTES: usize = 16 * 1024;
/// Replay-cache entries kept per process.
const REPLAY_CAP: usize = 20_000;
/// The `events.db` event name.
pub const EVENT_NAME: &str = "mcp.event";

/// The headers a delivery carries.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeliveryHeaders<'a> {
    pub webhook_id: Option<&'a str>,
    pub timestamp: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub subscription_id: Option<&'a str>,
}

/// The HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    /// JSON body.
    pub body: String,
}

impl Answer {
    fn new(status: u16, body: Value) -> Self {
        Self { status, body: body.to_string() }
    }
    fn error(status: u16, msg: &str) -> Self {
        Self::new(status, json!({ "error": msg }))
    }
}

#[derive(Default)]
struct ReceiverState {
    /// (subscription id, webhook-id) → expiry (Unix seconds).
    replay: HashMap<(String, String), i64>,
    /// subscription id → (minute window start, count).
    rate: HashMap<String, (i64, u32)>,
}

fn state() -> &'static Mutex<ReceiverState> {
    static S: OnceLock<Mutex<ReceiverState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(ReceiverState::default()))
}

fn rate_ok(id: &str, now: i64) -> bool {
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    if st.rate.len() > 10_000 {
        st.rate.retain(|_, (start, _)| now - *start < 60);
    }
    let slot = st.rate.entry(id.to_string()).or_insert((now, 0));
    if now - slot.0 >= 60 {
        *slot = (now, 0);
    }
    slot.1 += 1;
    slot.1 <= RATE_PER_MINUTE
}

/// `true` the first time `(id, msg_id)` is seen inside the replay window.
fn first_seen(id: &str, msg_id: &str, now: i64) -> bool {
    let mut st = state().lock().unwrap_or_else(|p| p.into_inner());
    if st.replay.len() >= REPLAY_CAP {
        st.replay.retain(|_, exp| *exp > now);
    }
    let key = (id.to_string(), msg_id.to_string());
    if st.replay.get(&key).is_some_and(|exp| *exp > now) {
        return false;
    }
    // Kept a little longer than the timestamp window, so a replay with an
    // in-window timestamp is always caught.
    st.replay.insert(key, now + 2 * signature::TIMESTAMP_TOLERANCE_SECS + 60);
    true
}

/// Handle one delivery.
pub async fn handle(home: &Path, id: &str, h: DeliveryHeaders<'_>, body: &[u8]) -> Answer {
    handle_at(home, id, h, body, chrono::Utc::now().timestamp()).await
}

/// [`handle`] at a given time (tests).
pub async fn handle_at(home: &Path, id: &str, h: DeliveryHeaders<'_>, body: &[u8], now: i64) -> Answer {
    if !store::is_valid_id(id) {
        return Answer::error(404, "not found");
    }
    let home_owned = home.to_path_buf();
    let id_owned = id.to_string();
    let rec = match tokio::task::spawn_blocking(move || store::get(&home_owned, &id_owned)).await {
        Ok(Ok(Some(r))) => r,
        Ok(Ok(None)) => return Answer::error(404, "not found"),
        _ => {
            tracing::warn!("mcp-events: subscription store unreadable");
            return Answer::error(503, "temporarily unavailable");
        }
    };
    if body.len() > MAX_BODY_BYTES {
        return Answer::error(413, "payload too large");
    }
    if !rate_ok(id, now) {
        return Answer::error(429, "rate limited");
    }
    let (Some(msg_id), Some(ts), Some(sig)) = (h.webhook_id, h.timestamp, h.signature) else {
        return Answer::error(400, "missing webhook headers");
    };
    if msg_id.is_empty() || msg_id.len() > 256 || !msg_id.bytes().all(|b| b.is_ascii_graphic()) {
        return Answer::error(400, "bad webhook-id");
    }
    let secrets = match store::open(home, &rec) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "mcp-events: cannot open the signing secret");
            return Answer::error(503, "temporarily unavailable");
        }
    };
    if let Err(why) = signature::verify(&secrets.accepted(now), msg_id, ts, sig, body, now) {
        super::audit(
            home,
            super::AUDIT_REJECTED,
            &rec.agent_id,
            json!({ "subscription": rec.id, "server": rec.server, "reason": why.as_str() }),
        );
        return Answer::error(401, "signature verification failed");
    }
    // The routing header, when the upstream sends one and we know its ids,
    // must name one of this subscription's upstream subscriptions.
    if let Some(sid) = h.subscription_id {
        let known: Vec<&str> = rec.upstream.iter().filter_map(|u| u.upstream_id.as_deref()).collect();
        if !known.is_empty() && !known.contains(&sid) {
            super::audit(
                home,
                super::AUDIT_REJECTED,
                &rec.agent_id,
                json!({ "subscription": rec.id, "server": rec.server, "reason": "subscription_id" }),
            );
            return Answer::error(401, "unknown subscription id");
        }
    }
    if rec.status == SubStatus::Terminated {
        return Answer::error(410, "subscription ended");
    }
    if !first_seen(id, msg_id, now) {
        // Already accepted: acknowledge so the upstream stops retrying.
        return Answer::new(200, json!({ "ok": true, "duplicate": true }));
    }
    let Ok(Value::Object(msg)) = serde_json::from_slice::<Value>(body) else {
        return Answer::error(400, "body is not a JSON object");
    };

    if let Some(kind) = msg.get("type").and_then(|t| t.as_str()) {
        return control(home, &rec, kind, &msg).await;
    }
    event(home, &rec, &msg, msg_id, now).await
}

async fn control(
    home: &Path,
    rec: &store::EventSubscription,
    kind: &str,
    msg: &serde_json::Map<String, Value>,
) -> Answer {
    match kind {
        "verification" => {
            let Some(challenge) = msg.get("challenge").and_then(|c| c.as_str()).filter(|c| !c.is_empty() && c.len() <= 512)
            else {
                return Answer::error(400, "bad challenge");
            };
            super::audit(home, super::AUDIT_CONTROL, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "type": "verification" }));
            Answer::new(200, json!({ "challenge": challenge }))
        }
        "terminated" => {
            let code = msg.get("error").and_then(|e| e.get("code")).and_then(|c| c.as_i64());
            let (home_o, id) = (home.to_path_buf(), rec.id.clone());
            let _ = tokio::task::spawn_blocking(move || {
                store::update(&home_o, &id, |r| {
                    r.status = SubStatus::Terminated;
                    Ok(true)
                })
            })
            .await;
            super::audit(home, super::AUDIT_CONTROL, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "type": "terminated", "code": code }));
            Answer::new(200, json!({ "ok": true }))
        }
        "gap" => {
            super::audit(home, super::AUDIT_CONTROL, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "type": "gap" }));
            Answer::new(200, json!({ "ok": true }))
        }
        _ => Answer::new(200, json!({ "ok": true, "ignored": true })),
    }
}

/// Turn an accepted `EventOccurrence` into an `events.db` row.
async fn event(
    home: &Path,
    rec: &store::EventSubscription,
    msg: &serde_json::Map<String, Value>,
    msg_id: &str,
    now: i64,
) -> Answer {
    let name = msg.get("name").and_then(|n| n.as_str()).unwrap_or("");
    if !rec.event_types.iter().any(|t| t == name) {
        // Not something this subscription asked for: do not retry it.
        super::audit(home, super::AUDIT_REJECTED, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "reason": "unexpected_event_name" }));
        return Answer::error(410, "event not subscribed");
    }
    let event_id = msg
        .get("eventId")
        .and_then(|v| v.as_str())
        .map(|s| duduclaw_core::truncate_chars(s, 256).to_string())
        .unwrap_or_else(|| msg_id.to_string());
    let raw_data = msg.get("data").cloned().unwrap_or(Value::Null);
    let (data, suspicious, risk) = sanitize_data(&raw_data);
    let timestamp = msg
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| duduclaw_core::truncate_chars(s, 64).to_string())
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(now, 0).map(|d| d.to_rfc3339()).unwrap_or_default());
    let payload = json!({
        "subscription_id": rec.id,
        "agent_id": rec.agent_id,
        "server": rec.server,
        "name": name,
        "event_id": event_id,
        "lane": rec.mode.as_str(),
        "timestamp": timestamp,
        "data": data,
        "suspicious": suspicious,
    });
    let bus = match crate::events_store::EventBusStore::open(home) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "mcp-events: events.db unavailable");
            return Answer::error(503, "temporarily unavailable");
        }
    };
    if let Err(e) = bus.append(EVENT_NAME, &payload.to_string()).await {
        tracing::warn!(error = %e, "mcp-events: could not record the event");
        return Answer::error(503, "temporarily unavailable");
    }
    let (home_o, id) = (home.to_path_buf(), rec.id.clone());
    let _ = tokio::task::spawn_blocking(move || {
        store::update(&home_o, &id, |r| {
            r.deliveries = r.deliveries.saturating_add(1);
            r.last_delivery_at = Some(store::now_rfc3339());
            Ok(true)
        })
    })
    .await;
    super::audit(
        home,
        super::AUDIT_DELIVERED,
        &rec.agent_id,
        json!({ "subscription": rec.id, "server": rec.server, "name": name, "suspicious": suspicious, "risk_score": risk, "bytes": msg.get("data").map(|d| d.to_string().len()).unwrap_or(0) }),
    );
    Answer::new(200, json!({ "ok": true }))
}

/// Scan the event data with `input_guard` (never blocks: the operator chose
/// the source; a hit is marked) and cap it at [`MAX_DATA_BYTES`].
pub fn sanitize_data(data: &Value) -> (Value, bool, u32) {
    let text = data.to_string();
    let scan = duduclaw_security::input_guard::scan_input(&text, duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD);
    let suspicious = !scan.matched_rules.is_empty();
    let capped = if text.len() > MAX_DATA_BYTES {
        json!({ "_truncated": true, "text": duduclaw_core::truncate_bytes(&text, MAX_DATA_BYTES) })
    } else {
        data.clone()
    };
    (capped, suspicious, scan.risk_score)
}

// ── axum route ──────────────────────────────────────────────────────────

/// `POST /webhook/mcp-events/{id}`, always mounted; an unknown id answers 404.
/// The body limit is one byte over [`MAX_BODY_BYTES`] so an oversized body
/// reaches the handler's 413 instead of axum's generic answer.
pub fn router(home: std::path::PathBuf) -> axum::Router {
    use axum::routing::post;
    axum::Router::new()
        .route("/webhook/mcp-events/{id}", post(route_handler))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES + 1))
        .with_state(home)
}

async fn route_handler(
    axum::extract::State(home): axum::extract::State<std::path::PathBuf>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let get = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let h = DeliveryHeaders {
        webhook_id: get("webhook-id"),
        timestamp: get("webhook-timestamp"),
        signature: get("webhook-signature"),
        subscription_id: get("x-mcp-subscription-id"),
    };
    let ans = handle(&home, &id, h, &body).await;
    let status = axum::http::StatusCode::from_u16(ans.status).unwrap_or(axum::http::StatusCode::BAD_REQUEST);
    (
        status,
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        ans.body,
    )
        .into_response()
}
