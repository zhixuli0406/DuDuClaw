//! Shared wire protocol between `duduclaw-relay` (the cloud webhook relay,
//! `crates/duduclaw-relay`) and its box-side WebSocket client
//! (`duduclaw-gateway`'s `relay_client.rs`, WP-E2).
//!
//! These two halves are built and deployed independently — the relay runs on
//! Cloud Run, the client runs inside every gateway — so the wire shapes they
//! agree on must live in exactly one place, not be hand-copied into both
//! crates. Before this module existed the relay defined `HookFrame` /
//! `Channel` / `ClientFrame` / `ServerFrame` / `validate_device_id` locally
//! (`duduclaw-relay/src/{hook,channel,device_ws,validate}.rs`); those files
//! now re-export from here instead of redefining, so the two crates cannot
//! drift out of sync on a field name, a `#[serde(rename)]` tag, or the
//! `device_id` format.
//!
//! See `crates/duduclaw-relay/README.md` for the full protocol narrative
//! (TOFU registration, Ed25519 challenge-response, at-most-one-connection,
//! offline queueing). This module only carries the types/pure validators
//! both sides need — the relay's own connection-lifecycle logic
//! (`crypto.rs`'s `Challenge`/`issue_challenge`, `registry.rs`,
//! `queue.rs`) stays relay-side, since the box never issues challenges or
//! queues frames for anyone.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};

// ── device_id validation ────────────────────────────────────────────────

pub const MIN_DEVICE_ID_LEN: usize = 4;
pub const MAX_DEVICE_ID_LEN: usize = 64;

/// Validate a `device_id`: lowercase ASCII alphanumeric plus `-`/`_`,
/// 4-64 bytes. Deliberately strict (not a general slug validator) — the
/// relay embeds this value verbatim into SQL parameters, log lines, and a
/// URL path segment (`/v1/hook/{channel}/{device_id}`), so a narrow
/// allowlist removes an entire class of injection/log-forging concerns up
/// front. The box-side client must derive a `device_id` that satisfies this
/// same function (see `RelayDeviceIdentity::derive_device_id` in the
/// gateway) — one authority for the format, not two definitions that could
/// silently diverge.
pub fn validate_device_id(id: &str) -> Result<(), String> {
    if id.len() < MIN_DEVICE_ID_LEN || id.len() > MAX_DEVICE_ID_LEN {
        return Err(format!(
            "device_id must be {MIN_DEVICE_ID_LEN}-{MAX_DEVICE_ID_LEN} bytes"
        ));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err("device_id must be lowercase ASCII alphanumeric, '-', or '_' only".to_string());
    }
    Ok(())
}

// ── channel identifiers ─────────────────────────────────────────────────

/// Webhook channel identifiers the relay knows how to route. Only `line` is
/// wired up today (LINE Messaging API webhooks). Adding a new channel is a
/// single new match arm here — the routing (`/v1/hook/{channel}/{device_id}`),
/// body handling, and relay behavior (opaque bytes, no signature
/// verification on the cloud side) stay identical for every channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Line,
}

impl Channel {
    /// Parse a path segment into a known channel. Unknown values are a
    /// caller error (400 on the relay side), not a silent no-op — an
    /// unsupported channel must never be treated as if it forwarded
    /// successfully.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "line" => Some(Channel::Line),
            _ => None,
        }
    }

    /// Lower-cased header names forwarded to the device verbatim, in
    /// addition to the always-forwarded `content-type`. The relay never
    /// inspects or validates their values — it only passes them through so
    /// the device can run the channel's own signature verification with its
    /// own channel secret, which the relay never holds.
    pub fn forwarded_headers(self) -> &'static [&'static str] {
        match self {
            Channel::Line => &["x-line-signature"],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Line => "line",
        }
    }
}

// ── HookFrame (relay → device) ──────────────────────────────────────────

/// A single relayed webhook, framed for delivery over the device WebSocket.
/// This exact JSON shape is the wire contract both halves must agree on
/// bit-for-bit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookFrame {
    #[serde(rename = "type")]
    pub kind: String, // always "hook"
    pub id: String,
    pub channel: String,
    pub headers: BTreeMap<String, String>,
    /// Raw body, base64-encoded — preserves exact bytes regardless of
    /// content, including any non-UTF8 payload.
    pub body_b64: String,
    pub received_at: String,
}

impl HookFrame {
    pub fn new(channel: Channel, headers: BTreeMap<String, String>, body: &[u8]) -> Self {
        Self {
            kind: "hook".to_string(),
            id: uuid::Uuid::new_v4().to_string(),
            channel: channel.as_str().to_string(),
            headers,
            body_b64: BASE64.encode(body),
            received_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ── Device WebSocket frames ─────────────────────────────────────────────

/// Frames the device sends to the relay over `/v1/device/ws`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientFrame {
    /// Answers the server's `challenge` frame.
    #[serde(rename = "auth")]
    Auth { signature_b64: String },
    /// Periodic self-report of the device's current LAN IP (feeds
    /// `/v1/find`'s same-office-network grouping).
    #[serde(rename = "lan_ip")]
    LanIp { ip: String },
}

/// Control frames the relay sends to the device over `/v1/device/ws`. This
/// deliberately does **not** include `hook` frames as a variant: on the wire
/// a `hook` frame is a bare [`HookFrame`] object (which already owns its own
/// `"type": "hook"` field) — nesting it inside a `#[serde(tag = "type")]`
/// variant would fight [`HookFrame`]'s own `type` field for the same JSON
/// key. The relay sends these two shapes as independent top-level objects
/// (see `device_ws.rs::send_json`, called once with a `ServerFrame` and,
/// separately, once with a `HookFrame`); [`parse_device_inbound`] is the
/// client-side counterpart that tells them apart by peeking `"type"` first.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerFrame {
    /// Issued immediately on connect; the device must sign `nonce_b64` (raw
    /// bytes, base64-decoded) with its registered Ed25519 private key and
    /// reply with an `auth` frame.
    #[serde(rename = "challenge")]
    Challenge { nonce_b64: String },
    /// Sent once authentication succeeds — the device may now expect `hook`
    /// frames (any queued while it was offline are flushed right after).
    #[serde(rename = "ready")]
    Ready,
    /// Authentication failed (or some other fatal handshake error); the
    /// relay closes the connection immediately after.
    #[serde(rename = "error")]
    Error { message: String },
}

/// One parsed server → device frame, disambiguated by `"type"` (see
/// [`ServerFrame`]'s doc for why `hook` cannot live inside that enum).
#[derive(Debug, Clone)]
pub enum DeviceInboundFrame {
    Control(ServerFrame),
    Hook(HookFrame),
    /// Valid JSON with an unrecognized/missing `"type"`, or a `"type"` whose
    /// payload didn't parse into the expected shape. Mirrors the relay's own
    /// device_ws.rs handling of frames from the device: fail-open for
    /// forward compatibility, never treated as a fatal connection error.
    Unknown,
}

/// Parse one inbound WebSocket text frame from the relay. Pure and
/// infallible (never panics, never propagates a parse error) — an
/// unparseable or unrecognized frame becomes
/// [`DeviceInboundFrame::Unknown`], exactly like an unrecognized frame from
/// the device is fail-open-ignored server-side.
pub fn parse_device_inbound(text: &str) -> DeviceInboundFrame {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return DeviceInboundFrame::Unknown;
    };
    match value.get("type").and_then(|v| v.as_str()) {
        Some("hook") => serde_json::from_value::<HookFrame>(value)
            .map(DeviceInboundFrame::Hook)
            .unwrap_or(DeviceInboundFrame::Unknown),
        Some("challenge") | Some("ready") | Some("error") => serde_json::from_value::<ServerFrame>(value)
            .map(DeviceInboundFrame::Control)
            .unwrap_or(DeviceInboundFrame::Unknown),
        _ => DeviceInboundFrame::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── device_id ────────────────────────────────────────────────────

    #[test]
    fn accepts_valid_ids() {
        assert!(validate_device_id("box-alpha-01").is_ok());
        assert!(validate_device_id("a_b_c_1234").is_ok());
    }

    #[test]
    fn rejects_too_short() {
        assert!(validate_device_id("ab").is_err());
    }

    #[test]
    fn rejects_too_long() {
        assert!(validate_device_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn rejects_uppercase_and_symbols() {
        assert!(validate_device_id("Box-Alpha").is_err());
        assert!(validate_device_id("box alpha").is_err());
        assert!(validate_device_id("box/alpha").is_err());
        assert!(validate_device_id("../etc/passwd").is_err());
    }

    // ── Channel ──────────────────────────────────────────────────────

    #[test]
    fn parses_known_channel() {
        assert_eq!(Channel::parse("line"), Some(Channel::Line));
    }

    #[test]
    fn rejects_unknown_channel() {
        assert_eq!(Channel::parse("whatsapp"), None);
        assert_eq!(Channel::parse(""), None);
        assert_eq!(Channel::parse("LINE"), None); // case-sensitive, no surprise matches
    }

    #[test]
    fn line_forwards_its_signature_header() {
        assert_eq!(Channel::Line.forwarded_headers(), &["x-line-signature"]);
    }

    // ── HookFrame ────────────────────────────────────────────────────

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

    #[test]
    fn hook_frame_serializes_with_the_wire_shape_the_relay_readme_documents() {
        let mut headers = BTreeMap::new();
        headers.insert("x-line-signature".to_string(), "sig".to_string());
        headers.insert("content-type".to_string(), "application/json".to_string());
        let frame = HookFrame::new(Channel::Line, headers, b"{}");
        let json: serde_json::Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], serde_json::json!("hook"));
        assert_eq!(json["channel"], serde_json::json!("line"));
        assert!(json["id"].is_string());
        assert!(json["body_b64"].is_string());
        assert!(json["received_at"].is_string());
        assert_eq!(json["headers"]["x-line-signature"], serde_json::json!("sig"));
    }

    // ── ClientFrame / ServerFrame wire shape (protocol round-trip) ────
    //
    // These lock the exact JSON both `duduclaw-relay` and the gateway's
    // `relay_client.rs` parse — a renamed field or a dropped `#[serde(tag)]`
    // here would silently desync the two halves, since each is compiled and
    // deployed independently.

    #[test]
    fn client_frame_auth_matches_the_documented_wire_shape() {
        let frame = ClientFrame::Auth { signature_b64: "sig123".to_string() };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "auth", "signature_b64": "sig123" }));
    }

    #[test]
    fn client_frame_lan_ip_matches_the_documented_wire_shape() {
        let frame = ClientFrame::LanIp { ip: "192.168.1.23".to_string() };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "lan_ip", "ip": "192.168.1.23" }));
    }

    #[test]
    fn client_frame_roundtrips_through_json() {
        let frame = ClientFrame::Auth { signature_b64: "xyz".to_string() };
        let text = serde_json::to_string(&frame).unwrap();
        let back: ClientFrame = serde_json::from_str(&text).unwrap();
        match back {
            ClientFrame::Auth { signature_b64 } => assert_eq!(signature_b64, "xyz"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn server_frame_challenge_matches_the_documented_wire_shape() {
        let frame = ServerFrame::Challenge { nonce_b64: "n0nce".to_string() };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "challenge", "nonce_b64": "n0nce" }));
    }

    #[test]
    fn server_frame_ready_matches_the_documented_wire_shape() {
        let json = serde_json::to_value(&ServerFrame::Ready).unwrap();
        assert_eq!(json, serde_json::json!({ "type": "ready" }));
    }

    #[test]
    fn server_frame_error_matches_the_documented_wire_shape() {
        let frame = ServerFrame::Error { message: "authentication failed".to_string() };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "error", "message": "authentication failed" })
        );
    }

    #[test]
    fn server_frame_deserializes_a_relay_style_challenge_frame_literal() {
        // The exact literal from crates/duduclaw-relay/README.md's wire
        // protocol section — proves the gateway's parser accepts what the
        // relay actually documents it sends.
        let text = r#"{"type":"challenge","nonce_b64":"AAAA"}"#;
        let frame: ServerFrame = serde_json::from_str(text).unwrap();
        assert!(matches!(frame, ServerFrame::Challenge { nonce_b64 } if nonce_b64 == "AAAA"));
    }

    // ── parse_device_inbound (client-side frame disambiguation) ──────

    #[test]
    fn parse_device_inbound_recognizes_all_three_control_frames() {
        assert!(matches!(
            parse_device_inbound(r#"{"type":"challenge","nonce_b64":"AAAA"}"#),
            DeviceInboundFrame::Control(ServerFrame::Challenge { nonce_b64 }) if nonce_b64 == "AAAA"
        ));
        assert!(matches!(
            parse_device_inbound(r#"{"type":"ready"}"#),
            DeviceInboundFrame::Control(ServerFrame::Ready)
        ));
        assert!(matches!(
            parse_device_inbound(r#"{"type":"error","message":"nope"}"#),
            DeviceInboundFrame::Control(ServerFrame::Error { message }) if message == "nope"
        ));
    }

    #[test]
    fn parse_device_inbound_recognizes_a_relay_style_hook_frame_literal() {
        // The exact literal from crates/duduclaw-relay/README.md's wire
        // protocol section — proves the gateway's parser accepts what the
        // relay actually documents it sends.
        let text = r#"{"type":"hook","id":"abc","channel":"line","headers":{"content-type":"application/json","x-line-signature":"sig"},"body_b64":"e30=","received_at":"2026-08-18T12:00:00Z"}"#;
        match parse_device_inbound(text) {
            DeviceInboundFrame::Hook(h) => {
                assert_eq!(h.channel, "line");
                assert_eq!(h.headers.get("x-line-signature").map(String::as_str), Some("sig"));
                assert_eq!(BASE64.decode(&h.body_b64).unwrap(), b"{}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn parse_device_inbound_is_fail_open_on_garbage() {
        assert!(matches!(parse_device_inbound("not json"), DeviceInboundFrame::Unknown));
        assert!(matches!(
            parse_device_inbound(r#"{"type":"future_frame_kind","x":1}"#),
            DeviceInboundFrame::Unknown
        ));
        assert!(matches!(parse_device_inbound(r#"{"no_type_field":true}"#), DeviceInboundFrame::Unknown));
        // A "hook"-tagged object missing required HookFrame fields must not
        // panic — it degrades to Unknown just like a malformed control frame.
        assert!(matches!(
            parse_device_inbound(r#"{"type":"hook"}"#),
            DeviceInboundFrame::Unknown
        ));
    }
}
