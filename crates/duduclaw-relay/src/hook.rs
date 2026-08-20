//! `POST /v1/hook/{channel}/{device_id}` — opaque webhook relay.
//!
//! The relay never verifies signatures, never parses the webhook body as
//! JSON/anything else, and never holds a channel secret (e.g. a LINE
//! channel secret). It forwards the raw body bytes plus an allowlisted set
//! of headers to the owning device over its live WebSocket connection; the
//! device — which does hold the channel secret — is the only party that
//! ever verifies the signature or interprets the payload.
//!
//! Always answers `200` once the request is structurally valid (known
//! channel, registered device, under the size/rate caps), *regardless* of
//! whether the device is currently connected: most webhook senders (LINE
//! included) retry aggressively on non-2xx, and a merely-offline device
//! must not trigger a retry storm. An offline/unreachable device instead
//! gets its frame placed on a small bounded per-device queue, flushed on
//! reconnect (see `queue.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;

use crate::channel::Channel;
use crate::error::RelayError;
use crate::registry::OutboundMessage;
use crate::state::AppState;
use crate::validate::validate_device_id;

/// `HookFrame` now lives in `duduclaw_core::relay_protocol` (WP-E2) so the
/// box-side relay client parses from exactly the same struct definition
/// this handler serializes from. Re-exported here so every existing
/// `crate::hook::HookFrame` reference in this crate (`queue.rs`,
/// `registry.rs`) keeps compiling unchanged.
pub use duduclaw_core::relay_protocol::HookFrame;

/// Hard cap on webhook body size. Webhook payloads (LINE included) are
/// small JSON; 2MB gives generous headroom while bounding memory per
/// request. Also enforced declaratively via `DefaultBodyLimit` on the route
/// (see `lib.rs`) so oversized bodies are rejected before this handler even
/// buffers them — this constant is kept in sync with that layer and
/// re-checked here as defense in depth.
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Requests/minute allowed per device on the webhook-forwarding endpoint.
pub const HOOK_RATE_LIMIT_PER_MINUTE: u32 = 60;

pub async fn hook_handler(
    State(state): State<Arc<AppState>>,
    Path((channel_raw, device_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<serde_json::Value>, RelayError> {
    let channel = Channel::parse(&channel_raw)
        .ok_or_else(|| RelayError::BadRequest(format!("unsupported channel: {channel_raw}")))?;

    validate_device_id(&device_id).map_err(RelayError::BadRequest)?;

    if body.len() > MAX_BODY_BYTES {
        return Err(RelayError::TooLarge);
    }

    if !state.db.device_exists(&device_id)? {
        return Err(RelayError::NotFound);
    }

    if !state.hook_rate_limiter.allow(&device_id).await {
        tracing::warn!(device_id = %device_id, channel = channel.as_str(), "hook rate limit exceeded");
        return Err(RelayError::RateLimited);
    }

    let mut fwd_headers = BTreeMap::new();
    if let Some(ct) = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        fwd_headers.insert("content-type".to_string(), ct.to_string());
    }
    for name in channel.forwarded_headers() {
        if let Some(v) = headers.get(*name).and_then(|v| v.to_str().ok()) {
            fwd_headers.insert((*name).to_string(), v.to_string());
        }
    }

    let frame = HookFrame::new(channel, fwd_headers, &body);

    let delivered = state
        .registry
        .try_send(&device_id, OutboundMessage::Hook(frame.clone()))
        .await;
    if !delivered {
        state.offline_queue.push(&device_id, frame).await;
        tracing::info!(device_id = %device_id, channel = channel.as_str(), "device offline — webhook queued");
    }

    Ok(Json(serde_json::json!({ "status": "ok" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;

    #[test]
    fn hook_frame_roundtrips_body_bytes() {
        let mut headers = BTreeMap::new();
        headers.insert("x-line-signature".to_string(), "abc".to_string());
        let body = b"\x00\x01\xffhello";
        let frame = HookFrame::new(Channel::Line, headers, body);
        assert_eq!(frame.kind, "hook");
        assert_eq!(frame.channel, "line");
        let decoded = BASE64.decode(&frame.body_b64).unwrap();
        assert_eq!(decoded, body);
    }
}
