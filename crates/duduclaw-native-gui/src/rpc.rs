// S4 — generic RPC call layer over the main authenticated `/ws` connection.
//
// Wire protocol source of truth (copied field-for-field, not guessed):
// `crates/duduclaw-gateway/src/protocol.rs::WsFrame` — the server's own type
// — cross-checked against `web/src/lib/ws-client.ts`'s `WsFrame` (the
// client mirror the web dashboard already ships). Three tagged variants:
//   {"type":"req","id","method","params"}
//   {"type":"res","id","ok","payload"?,"error"?}
//   {"type":"event","event","payload","seq"?,"state_version"?}
//
// This module is pure protocol logic — frame (de)serialization, a
// `CallError` classification, and `PendingCalls` (a plain, non-async
// request-id → response-channel registry). No socket, no tokio I/O: those
// live in `ws_status.rs`, which owns the actual connection and drives this
// module's types. Kept separate so the codec is unit-testable without a
// live gateway (see the tests below) and so `ws_status.rs` doesn't grow a
// second responsibility inline.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

/// The three frame shapes `WsFrame` can take, verbatim from
/// `protocol.rs`/`ws-client.ts` (see module doc comment). `Request` is only
/// ever serialized (this client sends requests, never receives them);
/// `Response`/`Event` are only ever deserialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsFrame {
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: Value,
    },
    #[serde(rename = "res")]
    Response {
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        payload: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        error: Option<Value>,
    },
    #[serde(rename = "event")]
    Event {
        event: String,
        payload: Value,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        seq: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        state_version: Option<u64>,
    },
}

/// Build the JSON text for a `req` frame — the only frame this client ever
/// sends past the initial `connect` handshake (which `ws_status.rs` already
/// builds inline; not routed through here to avoid a pointless dependency
/// loop between the two).
pub fn build_request(id: &str, method: &str, params: Value) -> String {
    let frame = WsFrame::Request { id: id.to_string(), method: method.to_string(), params };
    // `serde_json::to_string` on a plain enum of owned Strings/Values cannot
    // fail (no non-finite floats, no non-string map keys) — `unwrap_or`
    // still degrades to a harmless empty-object request rather than
    // panicking if that invariant is ever violated by a future field.
    serde_json::to_string(&frame).unwrap_or_else(|_| "{}".to_string())
}

/// Parse one incoming Text frame. Malformed JSON / an unrecognized `type`
/// tag is the caller's problem to log and drop (this crate's established
/// "never panic on wire data" convention) — returning `Err` rather than a
/// best-effort partial frame.
pub fn parse_frame(text: &str) -> Result<WsFrame, serde_json::Error> {
    serde_json::from_str(text)
}

/// Failure classification for [`crate::ws_status::call`] — mirrors the
/// task brief's own vocabulary (id generation / pending map / timeout /
/// disconnect-clears-pending).
#[derive(Debug, Clone)]
pub enum CallError {
    /// No authenticated connection was live when the call was issued — it
    /// was never sent at all (S4 does not queue calls made while
    /// disconnected; see `ws_status.rs`'s `dispatch_call` doc comment).
    NotConnected,
    /// No `res` frame arrived within the call's timeout window.
    Timeout,
    /// The connection dropped while this call was still pending a response.
    Disconnected,
    /// The server replied with `ok:false` — carries its `error` payload
    /// verbatim (shape is method-specific, e.g. `{code, message}`). No S4
    /// caller inspects this payload yet (see [`crate::ws_status::call`]'s
    /// doc comment on why the RPC layer has no consumer this pass) — kept
    /// for whichever future page is the first to actually call something.
    #[allow(dead_code)]
    Rejected(Value),
}

/// One request id → its caller's response channel, plus the monotonic
/// counter that mints ids. A plain struct (no internal locking) — the owner
/// in `ws_status.rs` wraps it in an `Arc<Mutex<..>>` since it's touched from
/// both the read loop (resolving) and the command loop (inserting) of a
/// single-threaded tokio runtime that still needs `Send + 'static` futures.
#[derive(Default)]
pub struct PendingCalls {
    inner: HashMap<String, oneshot::Sender<Result<Value, CallError>>>,
    next_id: u64,
}

impl PendingCalls {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint the next request id. `native-<n>` keeps this client's ids
    /// visually distinguishable from the web dashboard's bare-integer ids
    /// in any shared server-side logging, without risking a collision (the
    /// two clients never share a connection).
    pub fn next_id(&mut self) -> String {
        self.next_id += 1;
        format!("native-{}", self.next_id)
    }

    pub fn insert(&mut self, id: String, respond_to: oneshot::Sender<Result<Value, CallError>>) {
        self.inner.insert(id, respond_to);
    }

    /// Remove and return one pending call's response channel without
    /// resolving it — the caller decides what (if anything) to send. Used
    /// by `ws_status.rs`'s `dispatch_call` for the "the send itself failed"
    /// path, which is neither a normal resolve nor a timeout.
    pub fn take(&mut self, id: &str) -> Option<oneshot::Sender<Result<Value, CallError>>> {
        self.inner.remove(id)
    }

    /// Resolve a pending call from an incoming `res` frame. Returns `true`
    /// if a pending call with this id existed (an unknown id — e.g. a
    /// timeout that already fired and removed it — is silently ignored,
    /// not an error: the timeout path and this path race on purpose, see
    /// `PendingCalls`' own doc comment).
    pub fn resolve(&mut self, id: &str, ok: bool, payload: Option<Value>, error: Option<Value>) -> bool {
        let Some(respond_to) = self.inner.remove(id) else {
            return false;
        };
        let result = if ok {
            Ok(payload.unwrap_or(Value::Null))
        } else {
            Err(CallError::Rejected(error.unwrap_or(Value::Null)))
        };
        let _ = respond_to.send(result);
        true
    }

    /// Timeout path: remove and resolve exactly one id with
    /// [`CallError::Timeout`], if it's still pending. Returns `true` if it
    /// was (i.e. the timeout actually fired first, not the response).
    pub fn timeout_one(&mut self, id: &str) -> bool {
        let Some(respond_to) = self.inner.remove(id) else {
            return false;
        };
        let _ = respond_to.send(Err(CallError::Timeout));
        true
    }

    /// Drain every still-pending call and reject all of them with
    /// [`CallError::Disconnected`] — the task brief's "連線斷開時 pending
    /// 全部以錯誤結清" requirement.
    pub fn reject_all_disconnected(&mut self) {
        for (_, respond_to) in self.inner.drain() {
            let _ = respond_to.send(Err(CallError::Disconnected));
        }
    }

    #[cfg(test)]
    pub fn pending_count(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_matches_protocol_shape() {
        let json = build_request("native-1", "ping", serde_json::json!({}));
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "req");
        assert_eq!(v["id"], "native-1");
        assert_eq!(v["method"], "ping");
        assert_eq!(v["params"], serde_json::json!({}));
    }

    #[test]
    fn parse_res_frame_ok() {
        let text = r#"{"type":"res","id":"native-1","ok":true,"payload":{"pong":true}}"#;
        match parse_frame(text).unwrap() {
            WsFrame::Response { id, ok, payload, error } => {
                assert_eq!(id, "native-1");
                assert!(ok);
                assert_eq!(payload, Some(serde_json::json!({"pong": true})));
                assert_eq!(error, None);
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn parse_res_frame_error() {
        let text = r#"{"type":"res","id":"native-2","ok":false,"error":{"code":"denied"}}"#;
        match parse_frame(text).unwrap() {
            WsFrame::Response { ok, error, .. } => {
                assert!(!ok);
                assert_eq!(error, Some(serde_json::json!({"code": "denied"})));
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[test]
    fn parse_event_frame() {
        let text = r#"{"type":"event","event":"agent.started","payload":{"agent_id":"a1"}}"#;
        match parse_frame(text).unwrap() {
            WsFrame::Event { event, payload, seq, state_version } => {
                assert_eq!(event, "agent.started");
                assert_eq!(payload, serde_json::json!({"agent_id": "a1"}));
                assert_eq!(seq, None);
                assert_eq!(state_version, None);
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn parse_malformed_json_is_err() {
        assert!(parse_frame("not json").is_err());
    }

    #[test]
    fn parse_unknown_type_tag_is_err() {
        assert!(parse_frame(r#"{"type":"bogus"}"#).is_err());
    }

    #[tokio::test]
    async fn pending_calls_resolves_by_id() {
        let mut pending = PendingCalls::new();
        let id = pending.next_id();
        let (tx, rx) = oneshot::channel();
        pending.insert(id.clone(), tx);
        assert_eq!(pending.pending_count(), 1);

        let resolved = pending.resolve(&id, true, Some(serde_json::json!({"pong": true})), None);
        assert!(resolved);
        assert_eq!(pending.pending_count(), 0);

        let result = rx.await.unwrap();
        assert_eq!(result.unwrap(), serde_json::json!({"pong": true}));
    }

    #[tokio::test]
    async fn pending_calls_resolve_unknown_id_is_noop() {
        let mut pending = PendingCalls::new();
        assert!(!pending.resolve("no-such-id", true, None, None));
    }

    #[tokio::test]
    async fn pending_calls_rejects_on_error_response() {
        let mut pending = PendingCalls::new();
        let id = pending.next_id();
        let (tx, rx) = oneshot::channel();
        pending.insert(id.clone(), tx);

        pending.resolve(&id, false, None, Some(serde_json::json!({"code": "denied"})));
        match rx.await.unwrap() {
            Err(CallError::Rejected(v)) => assert_eq!(v, serde_json::json!({"code": "denied"})),
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pending_calls_reject_all_disconnected_drains_every_entry() {
        let mut pending = PendingCalls::new();
        let mut receivers = Vec::new();
        for _ in 0..3 {
            let id = pending.next_id();
            let (tx, rx) = oneshot::channel();
            pending.insert(id, tx);
            receivers.push(rx);
        }
        assert_eq!(pending.pending_count(), 3);

        pending.reject_all_disconnected();
        assert_eq!(pending.pending_count(), 0);

        for rx in receivers {
            match rx.await.unwrap() {
                Err(CallError::Disconnected) => {}
                other => panic!("expected Disconnected, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn pending_calls_timeout_one_only_fires_if_still_pending() {
        let mut pending = PendingCalls::new();
        let id = pending.next_id();
        let (tx, rx) = oneshot::channel();
        pending.insert(id.clone(), tx);

        // The response arrives first — timeout_one for the same id must be
        // a no-op (the race the doc comment describes).
        pending.resolve(&id, true, Some(serde_json::json!(1)), None);
        assert!(!pending.timeout_one(&id));
        assert_eq!(rx.await.unwrap().unwrap(), serde_json::json!(1));
    }
}
