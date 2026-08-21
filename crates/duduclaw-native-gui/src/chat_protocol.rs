// S4 — `/ws/chat` wire protocol (pure codec, no I/O).
//
// Source of truth, read directly (not guessed): `crates/duduclaw-gateway/
// src/webchat.rs`'s `ChatMessage` enum (the server's own type) — this is a
// DIFFERENT wire shape from `rpc.rs`'s generic `{"type":"req"/"res"/"event"}`
// protocol on the main `/ws`. `/ws/chat` is its own tagged-enum protocol:
// the first client frame MUST be `{"type":"auth","token":"<jwt>"}`; after
// that the server streams `session_info` once, then `assistant_chunk` /
// `step` / `progress` / `assistant_done` / `error` per turn.
//
// This client-side mirror is deliberately a SUBSET of the full server enum:
// no `attachments` payload building (S4 sends none — see `chat_ws.rs`'s
// "honest stub" list), no re-declaration of fields this client never reads
// (e.g. `Progress::tool`/`detail`, `AssistantDone::artifact` — parsed as
// `serde_json::Value` where kept at all, so an unrecognized/absent field
// never breaks deserialization, matching this crate's `api.rs` convention).

use serde::{Deserialize, Serialize};

/// Frames this client sends. `Auth` is only ever the first frame on a fresh
/// connection (see `chat_ws.rs`); `UserMessage` is sent per turn.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum OutFrame {
    #[serde(rename = "auth")]
    Auth { token: String },
    #[serde(rename = "user_message")]
    UserMessage {
        content: String,
        session_id: Option<String>,
        /// L1 per-agent routing. S4b (`screens/chat/agents_picker.rs`) is
        /// this client's first caller to ever pass `Some(id)` — `None`
        /// stays byte-compatible with S4's pre-picker behavior (the server
        /// treats absent as "the main/default agent").
        agent: Option<String>,
        /// Always empty — file attachments are an explicit S4 scope cut
        /// (see `chat_ws.rs`'s doc comment). `Vec<()>` would serialize as
        /// `[null]` for a non-empty vec, but this is always empty, so the
        /// element type never actually matters; `serde_json::Value` keeps
        /// the struct honest about "this is JSON, not really typed" without
        /// depending on `ChatAttachment` from the gateway crate.
        attachments: Vec<serde_json::Value>,
        conv: Option<String>,
    },
}

pub fn build_auth(token: &str) -> String {
    encode(&OutFrame::Auth { token: token.to_string() })
}

pub fn build_user_message(
    content: &str,
    session_id: Option<String>,
    agent: Option<String>,
    conv: Option<String>,
) -> String {
    encode(&OutFrame::UserMessage {
        content: content.to_string(),
        session_id,
        agent,
        attachments: Vec::new(),
        conv,
    })
}

fn encode(frame: &OutFrame) -> String {
    serde_json::to_string(frame).unwrap_or_else(|_| "{}".to_string())
}

/// Frames this client receives. Matches `ChatMessage`'s server-side variant
/// names/tags exactly; every optional field the server may omit carries
/// `#[serde(default)]` so an older/newer server (a field added upstream)
/// degrades gracefully instead of failing the whole frame.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum InFrame {
    #[serde(rename = "assistant_chunk")]
    AssistantChunk { content: String },
    #[serde(rename = "progress")]
    Progress {
        content: String,
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        conv: Option<String>,
    },
    #[serde(rename = "step")]
    Step {
        phase: String,
        tool: String,
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        conv: Option<String>,
    },
    #[serde(rename = "assistant_done")]
    AssistantDone {
        content: String,
        tokens_used: u32,
        #[serde(default)]
        conv: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        message: String,
        #[serde(default)]
        code: Option<String>,
    },
    #[serde(rename = "session_info")]
    SessionInfo {
        session_id: String,
        agent_name: String,
        agent_icon: String,
        #[serde(default)]
        supports_vision: bool,
        #[serde(default)]
        model: String,
        #[serde(default)]
        refresh: bool,
    },
}

pub fn parse_in_frame(text: &str) -> Result<InFrame, serde_json::Error> {
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_auth_matches_server_shape() {
        let json = build_auth("jwt.tok.en");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "auth");
        assert_eq!(v["token"], "jwt.tok.en");
    }

    #[test]
    fn build_user_message_omits_agent_and_attachments_when_no_agent_selected() {
        let json = build_user_message("hi", Some("sess-1".to_string()), None, Some("conv-1".to_string()));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "user_message");
        assert_eq!(v["content"], "hi");
        assert_eq!(v["session_id"], "sess-1");
        assert_eq!(v["conv"], "conv-1");
        assert!(v["agent"].is_null());
        assert_eq!(v["attachments"], serde_json::json!([]));
    }

    /// S4b: the agent picker's selection round-trips into the wire frame.
    #[test]
    fn build_user_message_carries_selected_agent() {
        let json = build_user_message("hi", None, Some("finance".to_string()), None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["agent"], "finance");
    }

    #[test]
    fn build_user_message_null_session_and_conv_when_absent() {
        let json = build_user_message("hi", None, None, None);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["session_id"].is_null());
        assert!(v["conv"].is_null());
    }

    #[test]
    fn parse_session_info() {
        let text = r#"{"type":"session_info","session_id":"webchat:x","agent_name":"DuDu",
            "agent_icon":"🐾","supports_vision":true,"model":"claude","refresh":false}"#;
        match parse_in_frame(text).unwrap() {
            InFrame::SessionInfo { session_id, agent_name, agent_icon, supports_vision, model, refresh } => {
                assert_eq!(session_id, "webchat:x");
                assert_eq!(agent_name, "DuDu");
                assert_eq!(agent_icon, "🐾");
                assert!(supports_vision);
                assert_eq!(model, "claude");
                assert!(!refresh);
            }
            other => panic!("expected SessionInfo, got {other:?}"),
        }
    }

    #[test]
    fn parse_assistant_chunk() {
        let text = r#"{"type":"assistant_chunk","content":"hi"}"#;
        assert_eq!(parse_in_frame(text).unwrap(), InFrame::AssistantChunk { content: "hi".to_string() });
    }

    #[test]
    fn parse_assistant_done_with_missing_optional_fields() {
        let text = r#"{"type":"assistant_done","content":"done","tokens_used":42}"#;
        match parse_in_frame(text).unwrap() {
            InFrame::AssistantDone { content, tokens_used, conv, model } => {
                assert_eq!(content, "done");
                assert_eq!(tokens_used, 42);
                assert_eq!(conv, None);
                assert_eq!(model, None);
            }
            other => panic!("expected AssistantDone, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_with_code() {
        let text = r#"{"type":"error","message":"nope","code":"agent_forbidden"}"#;
        match parse_in_frame(text).unwrap() {
            InFrame::Error { message, code } => {
                assert_eq!(message, "nope");
                assert_eq!(code, Some("agent_forbidden".to_string()));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn parse_step_frame() {
        let text = r#"{"type":"step","phase":"start","tool":"Read","summary":"/etc/hosts","depth":0,"ts":1}"#;
        match parse_in_frame(text).unwrap() {
            InFrame::Step { phase, tool, summary, .. } => {
                assert_eq!(phase, "start");
                assert_eq!(tool, "Read");
                assert_eq!(summary, Some("/etc/hosts".to_string()));
            }
            other => panic!("expected Step, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_type_is_err() {
        assert!(parse_in_frame(r#"{"type":"bogus"}"#).is_err());
    }

    #[test]
    fn parse_malformed_json_is_err() {
        assert!(parse_in_frame("not json").is_err());
    }
}
