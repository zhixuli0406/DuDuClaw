//! Text-only user sends. Resource names and wire shape are closed.
use serde_json::{json, Value};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_identity_overrides_urls_and_cross_space_threads() {
        for bad in [json!({"space":"spaces/A","text":"hi","sender":"victim"}),
            json!({"space":"spaces/A/../B","text":"hi"}),
            json!({"space":"spaces/A","text":"hi","thread":"spaces/B/threads/T"}),
            json!({"space":"spaces/A","text":" "}),
            json!({"space":"spaces/A","text":"hi","token":"secret"})] {
            assert!(parse(&bad).is_err());
        }
        assert!(parse(&json!({"space":"spaces/A","text":"繁體中文 👋","thread":"spaces/A/threads/T"})).is_ok());
    }
    #[test]
    fn endpoint_is_exact_and_never_bot_scope() {
        assert!(endpoint_allowed("https://chat.googleapis.com/v1/spaces/A/messages", "spaces/A"));
        for bad in ["https://chat.googleapis.com.evil/v1/spaces/A/messages",
            "https://chat.googleapis.com/v1/spaces/B/messages",
            "https://chat.googleapis.com/v1/spaces/A/messages?token=bad"] {
            assert!(!endpoint_allowed(bad, "spaces/A"));
        }
        assert!(!crate::google_personal::scopes::permitted("google_chat_send", &["https://www.googleapis.com/auth/chat.bot".into()]));
    }
}

pub const GUIDANCE: &str = "請管理員啟用 Google Chat 傳送，並由本人在「我的連線」開放 Chat 傳送權限；每次傳送都需要本人確認。";
#[derive(Debug)]
pub(crate) struct SendArgs { pub space: String, pub text: String, pub thread: Option<String> }
pub(crate) fn parse(args: &Value) -> Result<SendArgs, String> {
    let object = args.as_object().ok_or("Chat 參數必須是物件。")?;
    if object.keys().any(|k| !matches!(k.as_str(), "space" | "text" | "thread")) {
        return Err("Chat 傳送只接受 space、text、thread。".into());
    }
    let space = args["space"].as_str().filter(|s| super::chat_read::valid_space(s))
        .ok_or("space 必須是有效的既有 spaces/... 資源名稱。")?;
    let text = args["text"].as_str().filter(|s| !s.trim().is_empty())
        .ok_or("text 不可空白。")?;
    let thread = match args.get("thread") {
        None => None,
        Some(Value::String(t)) => {
            let prefix = format!("{space}/threads/");
            let id = t.strip_prefix(&prefix).filter(|id| !id.is_empty() && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                .ok_or("thread 必須是同一空間的既有討論串。")?;
            let _ = id;
            Some(t.clone())
        }
        _ => return Err("thread 必須是資源名稱。".into()),
    };
    let parsed = SendArgs { space: space.into(), text: text.into(), thread };
    if serde_json::to_vec(&parsed.body()).map_err(|_| "Chat 內容無法編碼。")?.len() > 32_000 {
        return Err("Chat 訊息超過 32,000 bytes，請縮短內容。".into());
    }
    Ok(parsed)
}
impl SendArgs {
    pub(crate) fn body(&self) -> Value {
        let mut body = json!({"text": self.text});
        if let Some(thread) = &self.thread { body["thread"] = json!({"name":thread}); }
        body
    }
}
pub(crate) fn endpoint_allowed(url: &str, space: &str) -> bool {
    super::chat_read::messages_endpoint(space).is_ok_and(|expected| expected == url)
}
