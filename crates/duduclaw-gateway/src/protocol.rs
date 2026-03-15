use serde::{Deserialize, Serialize};

/// OpenClaw-compatible WebSocket frame types.
///
/// All messages over the WebSocket connection are encoded as JSON and tagged
/// with a `type` field that is one of `"req"`, `"res"`, or `"event"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsFrame {
    /// A request sent by the client.
    #[serde(rename = "req")]
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    },

    /// A response sent by the server in reply to a request.
    #[serde(rename = "res")]
    Response {
        id: String,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<serde_json::Value>,
    },

    /// A server-initiated event pushed to the client.
    #[serde(rename = "event")]
    Event {
        event: String,
        payload: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        state_version: Option<u64>,
    },
}

impl WsFrame {
    /// Build a successful response frame.
    pub fn ok_response(id: &str, payload: serde_json::Value) -> Self {
        Self::Response {
            id: id.to_owned(),
            ok: true,
            payload: Some(payload),
            error: None,
        }
    }

    /// Build an error response frame.
    pub fn error_response(id: &str, error: &str) -> Self {
        Self::Response {
            id: id.to_owned(),
            ok: false,
            payload: None,
            error: Some(serde_json::Value::String(error.to_owned())),
        }
    }

    /// Build a server-initiated event frame.
    pub fn event(name: &str, payload: serde_json::Value) -> Self {
        Self::Event {
            event: name.to_owned(),
            payload,
            seq: None,
            state_version: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_roundtrip() {
        let json = r#"{"type":"req","id":"1","method":"hello","params":{}}"#;
        let frame: WsFrame = serde_json::from_str(json).unwrap();
        match &frame {
            WsFrame::Request { id, method, .. } => {
                assert_eq!(id, "1");
                assert_eq!(method, "hello");
            }
            _ => panic!("expected Request"),
        }
        // Roundtrip
        let serialized = serde_json::to_string(&frame).unwrap();
        let _: WsFrame = serde_json::from_str(&serialized).unwrap();
    }

    #[test]
    fn test_ok_response() {
        let frame = WsFrame::ok_response("42", serde_json::json!({"status": "ok"}));
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""ok":true"#));
        assert!(!json.contains(r#""error""#));
    }

    #[test]
    fn test_error_response() {
        let frame = WsFrame::error_response("42", "not found");
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains("not found"));
        assert!(!json.contains(r#""payload""#));
    }

    #[test]
    fn test_event_frame() {
        let frame = WsFrame::event("agent.started", serde_json::json!({"agent_id": "a1"}));
        let json = serde_json::to_string(&frame).unwrap();
        assert!(json.contains(r#""type":"event""#));
        assert!(!json.contains(r#""seq""#));
    }
}
