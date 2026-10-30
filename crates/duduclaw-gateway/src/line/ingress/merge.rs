//! Which waiting LINE messages may be answered together, and the one turn
//! they become (see `channel_ingress::merge` for the ledger side, B10).
//!
//! Only plain text messages of a one-to-one chat are folded: no group or
//! room (several people, mention-only rules), no chat command, no `@` call,
//! no quoted reply (a quoted decision card is a button press), no media and
//! no redelivered webhook. The turn's input lists every message in order
//! under one line that asks for an answer to all of them, and the reply
//! goes out with the newest message's reply token.

use super::super::*;
use crate::channel_ingress::MergedEvent;
use crate::trigger_route::texts::Lang;

/// May this provider event be answered together with its neighbours?
pub(crate) fn mergeable(event: &LineEvent) -> bool {
    let one_to_one = event
        .source
        .as_ref()
        .is_some_and(|s| s.source_type.as_deref() == Some("user") && s.user_id.is_some());
    let Some(msg) = event.message.as_ref() else {
        return false;
    };
    let text = msg.text.as_deref().unwrap_or("").trim();
    event.event_type == "message"
        && one_to_one
        && !is_redelivery(event)
        && event.reply_token.is_some()
        && msg.msg_type == "text"
        && msg.quoted_message_id.is_none()
        && !text.is_empty()
        && !text.contains(['@', '＠'])
        && !crate::chat_commands::is_command(text)
}

/// [`mergeable`] on a stored event (`payload.event`).
pub(crate) fn mergeable_json(event: &serde_json::Value) -> bool {
    !crate::channel_ingress::credential_payload::is_encrypted(event)
        && serde_json::from_value::<LineEvent>(event.clone()).is_ok_and(|e| mergeable(&e))
}

fn is_redelivery(event: &LineEvent) -> bool {
    event
        .delivery_context
        .as_ref()
        .is_some_and(|c| c.is_redelivery)
}

/// The line above the listed messages (instruction to the model).
fn header(lang: Lang, n: usize) -> String {
    match lang {
        Lang::ZhTw => format!("（使用者在收到回覆前連續傳了 {n} 則訊息，請一次回覆每一則）"),
        Lang::En => format!(
            "(The user sent {n} messages in a row before getting a reply. Answer every one of them in this reply.)"
        ),
        Lang::Ja => format!(
            "（ユーザーは返信を待つ間に {n} 件のメッセージを続けて送りました。すべてに一度に答えてください）"
        ),
    }
}

/// The input text of a turn that answers `texts` (oldest first).
///
/// The model sees one user message, numbered, not separate messages
/// (B10 review M4, accepted). Every text comes from the same one-to-one
/// sender (`mergeable` + the ledger's conversation filter), so a message
/// that imitates the numbering or the header line can only confuse that
/// sender's own turn — the same reach as writing anything in one message.
pub(crate) fn combined_text(lang: Lang, texts: &[String]) -> String {
    let mut out = header(lang, texts.len());
    for (i, t) in texts.iter().enumerate() {
        out.push_str(&format!("\n{}. {}", i + 1, t.trim()));
    }
    out
}

fn text_of(event: &serde_json::Value) -> String {
    event["message"]["text"].as_str().unwrap_or("").to_string()
}

/// The event one merged turn runs as: the newest message (its reply token
/// and timestamp) carrying the combined text, plus that message's receipt
/// time. `None` when nothing was merged.
pub(crate) fn combined_event(
    head_event: &serde_json::Value,
    merged: &[MergedEvent],
    lang: Lang,
) -> Option<(LineEvent, i64)> {
    let newest = merged.last()?;
    let texts: Vec<String> = std::iter::once(text_of(head_event))
        .chain(merged.iter().map(|m| text_of(&m.event)))
        .collect();
    let mut event = newest.event.clone();
    event["message"]["text"] = serde_json::Value::String(combined_text(lang, &texts));
    // Native mentions index into the newest message's own text only.
    if let Some(msg) = event["message"].as_object_mut() {
        msg.remove("mention");
    }
    let parsed = serde_json::from_value::<LineEvent>(event).ok()?;
    Some((parsed, newest.received_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(text: &str, source: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "message", "webhookEventId": "w", "replyToken": format!("rt-{text}"),
            "timestamp": 1_000, "deliveryContext": {"isRedelivery": false},
            "source": source,
            "message": {"type": "text", "id": "1", "text": text}
        })
    }

    fn user() -> serde_json::Value {
        serde_json::json!({"type": "user", "userId": "U1"})
    }

    #[test]
    fn only_plain_one_to_one_text_is_mergeable() {
        assert!(mergeable_json(&ev("請問營業時間", user())));
        let group = serde_json::json!({"type": "group", "groupId": "G", "userId": "U1"});
        assert!(!mergeable_json(&ev("請問營業時間", group)));
        assert!(!mergeable_json(&ev("@業務 報價", user())));
        assert!(!mergeable_json(&ev("＠業務 報價", user())));
        assert!(!mergeable_json(&ev("/new", user())));
        let mut quoted = ev("同意", user());
        quoted["message"]["quotedMessageId"] = "q1".into();
        assert!(!mergeable_json(&quoted));
        let mut redelivered = ev("hi", user());
        redelivered["deliveryContext"]["isRedelivery"] = true.into();
        assert!(!mergeable_json(&redelivered));
        let mut image = ev("x", user());
        image["message"]["type"] = "image".into();
        assert!(!mergeable_json(&image));
    }

    #[test]
    fn combined_event_lists_every_message_and_uses_the_newest_token() {
        let head = ev("第一則", user());
        let merged = vec![
            MergedEvent { id: "a".into(), received_at: 20, event: ev("第二則", user()) },
            MergedEvent { id: "b".into(), received_at: 30, event: ev("第三則", user()) },
        ];
        let (event, received) = combined_event(&head, &merged, Lang::ZhTw).unwrap();
        assert_eq!(received, 30);
        assert_eq!(event.reply_token.as_deref(), Some("rt-第三則"));
        let text = event.message.unwrap().text.unwrap();
        assert_eq!(
            text,
            "（使用者在收到回覆前連續傳了 3 則訊息，請一次回覆每一則）\n1. 第一則\n2. 第二則\n3. 第三則"
        );
        assert!(combined_event(&head, &[], Lang::En).is_none());
    }
}
