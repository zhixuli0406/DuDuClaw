//! Link buttons only open the Dashboard. There is no callback/action payload
//! that could be mistaken for the member's authorization confirmation.
use serde_json::{json,Value};
pub(crate) const HISTORY_NOTICE: &str = "已傳送 Google 授權連結";

pub(crate) fn ticket_url(text:&str) -> Option<&str> {
    text.lines().last().filter(|line| line.split_once("#ticket=")
        .is_some_and(|(_,ticket)|ticket.len()==64 && ticket.bytes().all(|b|b.is_ascii_hexdigit())))
}

pub(crate) fn history_text(text:&str) -> String {
    if text.is_empty() { HISTORY_NOTICE.into() } else { format!("{text}\n\n{HISTORY_NOTICE}") }
}

pub(crate) fn markup(channel:&str,url:&str) -> Option<Value> {
    let label="開啟 Google 授權";
    match channel {
        "telegram"=>Some(json!({"inline_keyboard":[[{"text":label,"url":url}]]})),
        "slack"=>Some(json!({"type":"actions","elements":[{"type":"button","text":{"type":"plain_text","text":label},"url":url}]})),
        "discord"=>Some(json!([{"type":1,"components":[{"type":2,"style":5,"label":label,"url":url}]}])),
        "line"=>Some(json!({"items":[{"type":"action","action":{"type":"uri","label":label,"uri":url}}]})),
        _=>None,
    }
}

pub(crate) fn notice(url:&str) -> String {
    format!("請登入同一個 Dashboard 帳號，閱讀資料說明並確認 Google 授權。連結十分鐘內有效；確認授權不代表同意對外傳送。\n{url}")
}
