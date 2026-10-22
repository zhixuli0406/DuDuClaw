//! `/交辦` on LINE: the command (with the assignable list as quick-reply
//! buttons that send `/交辦 @name`) and the message an armed `/交辦 @name`
//! was waiting for. The task id comes from the event id, so LINE's webhook
//! redelivery and durable-ingress reruns never create a second task.

use super::*;
use crate::trigger_route::assign::{self, AssignCaller, AssignCommand, AssignReply};
use crate::trigger_route::list::{CallableAgent, LINE_QUICK_REPLY_MAX};
use crate::trigger_route::texts::Lang;

/// LINE quick-reply label limit (characters).
const LABEL_MAX: usize = 20;

fn caller<'a>(
    state: &'a LineState,
    session: &'a str,
    sender: &'a str,
    event_id: &'a str,
    seq: i64,
    fallback: Option<&'a str>,
) -> AssignCaller<'a> {
    AssignCaller {
        home: &state.home_dir,
        channel: "line",
        conversation: session,
        sender,
        event: Some(event_id),
        seq,
        fallback,
        access: crate::trigger_route::access::for_channel_sender(&state.home_dir, "line", sender),
    }
}

/// The conversation key the one-shot store and the task source use (the
/// same `line:<group|room|user>` the `@` router uses).
fn session_of(event: &LineEvent) -> String {
    format!("line:{}", ingress::line_conversation(event))
}

/// Answer a `/交辦` command. `fallback` is the conversation's employee.
pub(crate) async fn command_messages(
    state: &LineState,
    event: &LineEvent,
    cmd: &AssignCommand,
    fallback: &str,
) -> Vec<serde_json::Value> {
    let session = session_of(event);
    let sender = trigger::sender_of(event);
    let event_id = trigger::event_key(event, &session);
    let seq = event.timestamp.unwrap_or_else(crate::trigger_route::pending::next_seq);
    let c = caller(state, &session, sender, &event_id, seq, Some(fallback));
    let reply = assign::run(&state.ctx, cmd, &c).await;
    messages_for(&reply, crate::trigger_route::texts::lang_for(&state.home_dir))
}

/// Turn the message an armed `/交辦 @name` (for `agent`) was waiting for
/// into the task.
pub(crate) async fn pending_messages(
    state: &LineState,
    event: &LineEvent,
    agent: &str,
    text: &str,
    fallback: &str,
) -> Vec<serde_json::Value> {
    let session = session_of(event);
    let sender = trigger::sender_of(event);
    let event_id = trigger::event_key(event, &session);
    let seq = event.timestamp.unwrap_or_else(crate::trigger_route::pending::next_seq);
    let c = caller(state, &session, sender, &event_id, seq, Some(fallback));
    let reply = assign::run_pending(&state.ctx, agent, text, &c).await;
    messages_for(&reply, crate::trigger_route::texts::lang_for(&state.home_dir))
}

/// The LINE messages for one reply: the assignable list as quick-reply
/// buttons, anything else as text.
pub(crate) fn messages_for(reply: &AssignReply, lang: Lang) -> Vec<serde_json::Value> {
    match reply.list.as_deref() {
        Some(agents) if !agents.is_empty() => vec![list_message(&reply.text, agents, lang)],
        _ => vec![serde_json::json!({ "type": "text", "text": reply.text })],
    }
}

/// A text message carrying the full list, with up to 13 buttons that each
/// send `/交辦 @name` (LINE's quick-reply limit; the rest stay in the text).
fn list_message(text: &str, agents: &[CallableAgent], lang: Lang) -> serde_json::Value {
    let items: Vec<serde_json::Value> = agents
        .iter()
        .take(LINE_QUICK_REPLY_MAX)
        .map(|a| {
            serde_json::json!({
                "type": "action",
                "action": {
                    "type": "message",
                    "label": duduclaw_core::truncate_chars(&a.display_name, LABEL_MAX),
                    "text": format!("{} {}", command_word(lang), a.call_text),
                }
            })
        })
        .collect();
    let body = format!(
        "{}\n{}",
        crate::trigger_route::assign_texts::list_intro(lang),
        text
    );
    serde_json::json!({ "type": "text", "text": body, "quickReply": { "items": items } })
}

/// The command a button sends, in the reply language.
fn command_word(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "/assign",
        Lang::ZhTw | Lang::Ja => "/交辦",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_buttons_send_the_assign_command() {
        let agents = vec![CallableAgent {
            id: "sales-rep".into(),
            display_name: "業務專員".into(),
            icon: String::new(),
            call_text: "@業務專員".into(),
        }];
        let reply = AssignReply {
            text: "list".into(),
            list: Some(agents),
            task: None,
        };
        let msgs = messages_for(&reply, Lang::ZhTw);
        assert_eq!(msgs.len(), 1);
        let item = &msgs[0]["quickReply"]["items"][0]["action"];
        assert_eq!(item["text"], "/交辦 @業務專員");
        assert_eq!(item["label"], "業務專員");
        let plain = messages_for(
            &AssignReply {
                text: "x".into(),
                list: None,
                task: None,
            },
            Lang::ZhTw,
        );
        assert!(plain[0].get("quickReply").is_none());
    }
}
