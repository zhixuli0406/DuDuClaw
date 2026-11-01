//! Content-free refusal metadata, separate from the reply queued for delivery.
use super::*;

pub(in crate::webchat) struct IdentityUnavailable {
    sid: Option<String>,
    conv: Option<String>,
    msg_id: Option<String>,
    admission: bool,
    downstream_request: bool,
}

impl IdentityUnavailable {
    pub(super) fn new(sid: &str, conv: Option<String>, msg_id: &str, admission: bool) -> Self {
        Self { sid: Some(sid.into()), conv, msg_id: Some(msg_id.into()), admission, downstream_request: false }
    }

    /// Before routing, no client-supplied session has passed ownership checks.
    pub(in crate::webchat) fn before_routing(msg_id: Option<&str>, conv: Option<&str>) -> Message {
        Self {
            sid: None,
            conv: sanitize_conv_nonce(conv),
            msg_id: msg_id.filter(|id| assign_event_id(Some(id)).is_some()).map(str::to_owned),
            admission: true,
            downstream_request: false,
        }.message()
    }

    pub(in crate::webchat) fn downstream(request_id: Option<String>) -> Self {
        Self { sid: None, conv: None, msg_id: request_id, admission: true, downstream_request: true }
    }

    pub(in crate::webchat) fn message(&self) -> Message {
        if self.downstream_request {
            return Message::Text(serde_json::json!({"type":"error", "code":"credential_check_unavailable",
                "message":"Credential check unavailable; retry later", "retryable":true,
                "request_id":self.msg_id}).to_string().into());
        }
        let text = if self.admission {
            "暫時無法確認登入狀態，這則訊息沒有送出，請稍後再試。"
        } else {
            "暫時無法確認登入狀態，無法交付回覆，請稍後再試。"
        };
        let mut frame = serde_json::json!({
            "type": "error", "code": "identity_unavailable", "message": text,
        });
        if let Some(msg_id) = &self.msg_id {
            frame["msg_id"] = msg_id.clone().into();
            frame["request_id"] = msg_id.clone().into();
        }
        if let Some(conv) = &self.conv {
            frame["conv"] = conv.clone().into();
        }
        if let Some(sid) = &self.sid {
            // Preserve the established post-routing frame shape, including null conv.
            frame["conv"] = serde_json::json!(self.conv);
            frame["session_id"] = sid.clone().into();
        }
        Message::Text(frame.to_string().into())
    }
}
