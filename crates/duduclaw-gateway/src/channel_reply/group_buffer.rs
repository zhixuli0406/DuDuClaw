//! Adapter-owned coordinates: never infer group membership from a session prefix.
use super::ReplyContext;

#[derive(Clone)]
pub(crate) struct GroupTurn {
    channel: String,
    scope: String,
    group: String,
    thread: Option<String>,
    message_id: String,
    enabled: bool,
}

tokio::task_local! {
    static CURRENT: GroupTurn;
}

impl GroupTurn {
    pub(crate) async fn capture(
        ctx: &ReplyContext, is_group: bool, channel: &str, scope: &str,
        group: &str, thread: Option<&str>, message_id: &str,
        sender: &str, name: &str, text: &str,
    ) -> Self {
        let turn = Self { channel: channel.into(), scope: scope.into(), group: group.into(),
            thread: thread.map(str::to_owned), message_id: message_id.into(), enabled: is_group };
        if is_group {
            crate::group_context::capture(ctx, channel, scope, group, thread, message_id,
                sender, name, text, "inbound").await;
        }
        turn
    }

    pub(crate) async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        CURRENT.scope(self.clone(), future).await
    }

    /// Scope only final-answer sends. Progress, typing, and refusal notices
    /// outside this scope cannot accidentally become conversation context.
    pub(crate) fn delivery(&self, ctx: &std::sync::Arc<ReplyContext>,
        answered_by: Option<&crate::trigger_route::AnsweredBy>, thread: Option<&str>) -> Delivery {
        let mut turn = self.clone();
        turn.thread = thread.map(str::to_owned);
        Delivery { turn, ctx: ctx.clone(),
            sender_id: answered_by.map(|who| who.id.clone()).unwrap_or_else(|| "system".into()),
            sender_name: answered_by.map(|who| who.display_name.clone()).unwrap_or_else(|| "系統".into()) }
    }

}

#[derive(Clone)]
pub(crate) struct Delivery {
    turn: GroupTurn,
    ctx: std::sync::Arc<ReplyContext>,
    sender_id: String,
    sender_name: String,
}

tokio::task_local! { static DELIVERY: Delivery; }

impl Delivery {
    pub(crate) async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
        DELIVERY.scope(self.clone(), future).await
    }
}

/// Record each accepted provider chunk immediately. A later chunk failure
/// must not erase the messages that group members already received.
/// Providers without message receipts use an explicitly local identifier.
pub(crate) async fn delivered(text: &str, platform_id: Option<&str>) {
    let Ok(delivery) = DELIVERY.try_with(Clone::clone) else { return };
    if !delivery.turn.enabled || text.trim().is_empty() { return; }
    let local_id;
    let id = match platform_id.filter(|id| !id.is_empty()) {
        Some(id) => id,
        None => { local_id = format!("local:{}", uuid::Uuid::new_v4()); &local_id }
    };
    let turn = &delivery.turn;
    crate::group_context::capture(&delivery.ctx, &turn.channel, &turn.scope, &turn.group,
        turn.thread.as_deref(), id, &delivery.sender_id, &delivery.sender_name, text, "bot_reply").await;
}

/// Visible text only; never serialize action URLs, attachment sources, or
/// callback data into the conversation buffer.
pub(crate) fn payload_text(value: &serde_json::Value) -> String {
    fn collect(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields) => for (key, value) in fields {
                if ["text", "content", "description"].contains(&key.as_str()) {
                    if let Some(text) = value.as_str() { out.push(text.to_owned()); continue; }
                }
                if ["embeds", "contents", "body", "elements", "blocks"].contains(&key.as_str()) { collect(value, out); }
            },
            serde_json::Value::Array(values) => for value in values { collect(value, out); },
            _ => (),
        }
    }
    let mut parts = Vec::new();
    collect(value, &mut parts);
    if parts.is_empty() { "[附件]".into() } else { parts.join("\n") }
}

/// Reuse the channel reply reference-block insertion point. Empty/flagged
/// history leaves the triggering message byte-identical and never stops it.
pub(super) fn with_context(user_text: &str, block: &str) -> String {
    crate::knowledge::auto_retrieve::with_passages(user_text,
        (!block.is_empty()).then_some(block))
}

pub(super) async fn prompt(ctx: &ReplyContext) -> String {
    let Ok(turn) = CURRENT.try_with(Clone::clone) else { return String::new(); };
    if !turn.enabled { return String::new(); }
    crate::group_context::prompt_context_before(ctx, &turn.channel, &turn.group, turn.thread.as_deref(), &turn.message_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn group_context_reply_uses_actual_thread_destination() {
        let fixture = crate::decision_notify::native_loop_fixture::NativeCuFixture::new("group-buffer-test").await;
        crate::group_context::set(fixture.home.path(), &crate::group_context::GroupConfig {
            channel: "slack".into(), group_id: "C1".into(), scope_id: "global".into(),
            max_messages: 50, max_age_hours: 24,
        }).unwrap();
        let turn = GroupTurn::capture(&fixture.ctx, true, "slack", "global", "C1", None,
            "m1", "u1", "Alice", "Hello").await;
        let employee = crate::trigger_route::AnsweredBy { id: "alice".into(), display_name: "Alice AI".into(), via_trigger:false, session_id:"slack:group:C1".into() };
        turn.delivery(&fixture.ctx, Some(&employee), Some("m1")).scope(async {
            delivered("Answer", Some("receipt-1")).await;
            // A later failed chunk makes no delivery callback.
        }).await;
        let root = crate::group_context::read(fixture.home.path(), "slack", "C1", None, 50, None).unwrap();
        let thread = crate::group_context::read(fixture.home.path(), "slack", "C1", Some("m1"), 50, None).unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].direction, "inbound");
        assert_eq!(thread.len(), 1);
        assert_eq!(thread[0].direction, "bot_reply");
        assert_eq!(thread[0].text, "Answer");
        assert_eq!(thread[0].message_id, "receipt-1");
        assert_eq!(thread[0].sender_id, "alice");
        assert_eq!(thread[0].sender_name, "Alice AI");
    }

    #[test]
    fn group_context_reference_precedes_only_current_user_message() {
        let block = "<group_recent_context>quoted data</group_recent_context>";
        assert_eq!(with_context("question",block),format!("{block}\n\nquestion"));
        assert_eq!(with_context("question",""),"question");
    }

    #[test]
    fn group_context_delivery_payload_omits_actions_and_attachment_urls() {
        let payload = serde_json::json!({"content":"hello", "embeds":[{"description":"world"}],
            "components":[{"label":"secret action", "url":"https://example.com/secret"}],
            "attachments":[{"url":"https://example.com/private"}]});
        let text = payload_text(&payload);
        assert_eq!(text, "hello\nworld");
        assert_eq!(payload_text(&serde_json::json!({"type":"image","originalContentUrl":"secret"})), "[附件]");
    }

}
