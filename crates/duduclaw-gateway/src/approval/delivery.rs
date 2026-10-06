//! Exact transport targets captured by authenticated inbound adapters. Tokens
//! live only in memory, are never serializable/debuggable, and never fall back.
use super::{CURRENT_DECISION_CONTEXT, DecisionContext};
use crate::channel_sender::{ChannelSendError, ChannelSender};
use async_trait::async_trait;

#[derive(Clone)]
pub(crate) struct TrustedReplyTarget {
    pub context: DecisionContext,
    token: String,
    chat_id: String,
    thread_id: Option<String>,
    ingress_run_id: Option<String>,
    decision_access_scope: Option<(Option<String>, Option<String>, Option<String>)>,
    user_access_policy: Option<crate::channel_reply::UserAccessPolicy>,
}
impl TrustedReplyTarget {
    pub(crate) fn new(
        context: DecisionContext,
        token: String,
        chat_id: String,
        thread_id: Option<String>,
    ) -> Result<Self, String> {
        context.validate()?;
        if token.is_empty()
            || chat_id.is_empty()
            || !matches!(
                context.channel.as_str(),
                "telegram" | "line" | "slack" | "discord"
            )
        {
            return Err("unsupported exact confirmation target".into());
        }
        let conversation = thread_id
            .as_ref()
            .map_or_else(|| chat_id.clone(), |thread| format!("{chat_id}:{thread}"));
        if context.conversation_id != conversation
            || (context.channel == "telegram"
                && thread_id
                    .as_deref()
                    .is_some_and(|thread| thread.parse::<i64>().map_or(true, |id| id <= 0)))
            || (matches!(context.channel.as_str(), "line" | "discord") && thread_id.is_some())
        {
            return Err("confirmation conversation does not match transport target".into());
        }
        Ok(Self {
            context,
            token,
            chat_id,
            thread_id,
            ingress_run_id: None,
            decision_access_scope: None,
            user_access_policy: None,
        })
    }
    pub(crate) fn with_user_access_policy(
        mut self,
        policy: crate::channel_reply::UserAccessPolicy,
    ) -> Self {
        self.user_access_policy = Some(policy);
        self
    }
    pub(crate) fn user_access_policy(&self) -> Option<&crate::channel_reply::UserAccessPolicy> {
        self.user_access_policy.as_ref()
    }
    /// Metadata from the authenticated adapter, retained across turn awaits.
    pub(crate) fn with_decision_access_scope(
        mut self,
        scope: crate::decision_notify::DecisionAccessScope<'_>,
    ) -> Self {
        self.decision_access_scope = Some((
            scope.channel_id.map(str::to_owned),
            scope.guild_id.map(str::to_owned),
            scope.session_id.map(str::to_owned),
        ));
        self
    }
    pub(crate) fn decision_access_scope(
        &self,
    ) -> Option<crate::decision_notify::DecisionAccessScope<'_>> {
        self.decision_access_scope
            .as_ref()
            .map(
                |(channel, guild, session)| crate::decision_notify::DecisionAccessScope {
                    channel_id: channel.as_deref(),
                    guild_id: guild.as_deref(),
                    session_id: session.as_deref(),
                },
            )
    }
    /// Only the authenticated durable adapter may attach its accepted event run.
    pub(crate) fn with_ingress_run(mut self, run_id: &str) -> Result<Self, String> {
        let digest = run_id.len() == 64
            && run_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !digest && uuid::Uuid::parse_str(run_id).is_err() {
            return Err("invalid server ingress run ID".into());
        }
        self.ingress_run_id = Some(run_id.to_owned());
        Ok(self)
    }
    pub(crate) fn ingress_run_id(&self) -> Option<&str> {
        self.ingress_run_id.as_deref()
    }
    pub(crate) fn sender(&self, http: reqwest::Client) -> Box<dyn ChannelSender> {
        Box::new(BoundDecisionSender {
            target: self.clone(),
            http,
        })
    }
    fn body(&self, text: &str) -> serde_json::Value {
        let mut body = match self.context.channel.as_str() {
            "telegram" => serde_json::json!({"chat_id":self.chat_id,"text":text}),
            "slack" => serde_json::json!({"channel":self.chat_id,"text":text}),
            "discord" => serde_json::json!({"content":text,"allowed_mentions":{"parse":[]}}),
            "line" => {
                serde_json::json!({"to":self.chat_id,"messages":[{"type":"text","text":text}]})
            }
            _ => unreachable!(),
        };
        if let Some(thread) = &self.thread_id {
            if self.context.channel == "telegram" {
                if let Ok(id) = thread.parse::<i64>() {
                    body["message_thread_id"] = serde_json::json!(id);
                }
            } else if self.context.channel == "slack" {
                body["thread_ts"] = serde_json::json!(thread);
            }
        }
        body
    }
}
tokio::task_local! { pub(crate) static CURRENT_TRUSTED_REPLY_TARGET: Option<TrustedReplyTarget>; }

pub(crate) async fn scope_trusted_reply<F: std::future::Future>(
    target: Result<TrustedReplyTarget, String>,
    future: F,
) -> F::Output {
    match target {
        Ok(target) => {
            CURRENT_DECISION_CONTEXT
                .scope(
                    Some(target.context.clone()),
                    CURRENT_TRUSTED_REPLY_TARGET.scope(Some(target), future),
                )
                .await
        }
        Err(_) => {
            CURRENT_DECISION_CONTEXT
                .scope(None, CURRENT_TRUSTED_REPLY_TARGET.scope(None, future))
                .await
        }
    }
}
struct BoundDecisionSender {
    target: TrustedReplyTarget,
    http: reqwest::Client,
}
#[async_trait]
impl ChannelSender for BoundDecisionSender {
    fn channel_type(&self) -> &'static str {
        match self.target.context.channel.as_str() {
            "telegram" => "telegram",
            "line" => "line",
            "slack" => "slack",
            "discord" => "discord",
            _ => "unsupported",
        }
    }
    async fn send_text(&self, text: &str) -> Result<(), ChannelSendError> {
        let t = &self.target;
        let mut req = match t.context.channel.as_str() {
            "telegram" => self.http.post(delivery_url(
                &t.token,
                &format!("https://api.telegram.org/bot{}/sendMessage", t.token),
            )),
            "line" => self
                .http
                .post(delivery_url(
                    &t.token,
                    "https://api.line.me/v2/bot/message/push",
                ))
                .bearer_auth(&t.token),
            "slack" => self
                .http
                .post(delivery_url(
                    &t.token,
                    "https://slack.com/api/chat.postMessage",
                ))
                .bearer_auth(&t.token),
            "discord" => self
                .http
                .post(delivery_url(
                    &t.token,
                    &format!(
                        "https://discord.com/api/v10/channels/{}/messages",
                        t.chat_id
                    ),
                ))
                .header("Authorization", format!("Bot {}", t.token)),
            _ => return Err(ChannelSendError("unsupported exact account route".into())),
        };
        req = req.json(&t.body(text));
        let response = req
            .send()
            .await
            .map_err(|_| ChannelSendError("confirmation transport unavailable".into()))?;
        if !response.status().is_success() {
            return Err(ChannelSendError("confirmation delivery refused".into()));
        }
        if matches!(t.context.channel.as_str(), "slack" | "telegram") {
            let value: serde_json::Value = response
                .json()
                .await
                .map_err(|_| ChannelSendError("confirmation receipt unreadable".into()))?;
            if value["ok"] != true {
                return Err(ChannelSendError(
                    "confirmation provider refused delivery".into(),
                ));
            }
        }
        Ok(())
    }
    async fn send_photo(&self, _png: &[u8], _caption: &str) -> Result<(), ChannelSendError> {
        Err(ChannelSendError(
            "bound screenshots are not delivered by this transport".into(),
        ))
    }
    async fn request_confirmation(
        &self,
        _prompt: &str,
        _screenshot: Option<&[u8]>,
        _timeout_secs: u64,
    ) -> Result<bool, ChannelSendError> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn telegram_and_slack_exact_target_never_drop_thread_or_change_account() {
        for (channel, thread, key) in [
            ("telegram", "17", "message_thread_id"),
            ("slack", "123.456", "thread_ts"),
        ] {
            let context = DecisionContext {
                channel: channel.into(),
                account_id: "bot-A".into(),
                conversation_id: format!("chat:{thread}"),
                principal_id: "human".into(),
            };
            let target = TrustedReplyTarget::new(
                context,
                "private-token-A".into(),
                "chat".into(),
                Some(thread.into()),
            )
            .unwrap();
            let body = target.body("approval request");
            assert!(!body[key].is_null());
            assert_eq!(target.token, "private-token-A");
            assert_eq!(target.chat_id, "chat");
            assert!(!body.to_string().contains("private-token-A"));
        }
    }
}

#[cfg(test)]
mod live_target_tests {
    use super::*;
    #[tokio::test]
    async fn bound_target_is_owned_by_exact_employee_turn_and_preserves_thread() {
        let ctx = DecisionContext {
            channel: "telegram".into(),
            account_id: "telegram:alice|123".into(),
            conversation_id: "42:17".into(),
            principal_id: "human".into(),
        };
        let target = TrustedReplyTarget::new(
            ctx.clone(),
            "token-A".into(),
            "42".into(),
            Some("17".into()),
        );
        let guard = scope_trusted_reply(target, async {
            crate::computer_use_sessions::turns::register(
                "target-alice",
                "turn-A",
                "telegram:42:17",
            )
            .unwrap()
        })
        .await;
        let recorded =
            crate::computer_use_sessions::turns::target_for("target-alice", "turn-A").unwrap();
        assert_eq!(recorded.context, ctx);
        assert_eq!(recorded.body("confirm")["message_thread_id"], 17);
        assert!(crate::computer_use_sessions::turns::target_for("target-bob", "turn-A").is_none());
        assert!(
            crate::computer_use_sessions::turns::target_for("target-alice", "turn-B").is_none()
        );
        drop(guard);
        assert!(
            crate::computer_use_sessions::turns::target_for("target-alice", "turn-A").is_none()
        );
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    fn target(account: &str) -> Result<TrustedReplyTarget, String> {
        TrustedReplyTarget::new(
            DecisionContext {
                channel: "telegram".into(),
                account_id: account.into(),
                conversation_id: "42:17".into(),
                principal_id: "human".into(),
            },
            format!("token-{account}"),
            "42".into(),
            Some("17".into()),
        )
    }
    fn account() -> Option<String> {
        CURRENT_DECISION_CONTEXT
            .try_with(Clone::clone)
            .ok()
            .flatten()
            .map(|c| c.account_id)
    }
    #[tokio::test]
    async fn nested_invalid_scope_never_borrows_outer_authority_and_error_restores_outer() {
        scope_trusted_reply(
            target("outer").map(|t| {
                t.with_decision_access_scope(crate::decision_notify::DecisionAccessScope {
                    channel_id: Some("42"),
                    guild_id: None,
                    session_id: None,
                })
            }),
            async {
                assert_eq!(account().as_deref(), Some("outer"));
                assert_eq!(
                    CURRENT_TRUSTED_REPLY_TARGET
                        .try_with(Clone::clone)
                        .unwrap()
                        .unwrap()
                        .decision_access_scope()
                        .unwrap()
                        .channel_id,
                    Some("42")
                );
                let result: Result<(), ()> = scope_trusted_reply(Err("invalid".into()), async {
                    assert!(account().is_none());
                    assert!(
                        CURRENT_TRUSTED_REPLY_TARGET
                            .try_with(Clone::clone)
                            .unwrap()
                            .is_none()
                    );
                    Err(())
                })
                .await;
                assert!(result.is_err());
                assert_eq!(account().as_deref(), Some("outer"));
            },
        )
        .await;
        assert!(account().is_none());
    }
    #[tokio::test]
    async fn interleaved_accounts_and_cancelled_future_cannot_leak_scope() {
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let a = barrier.clone();
        let b = barrier.clone();
        tokio::join!(
            scope_trusted_reply(target("A"), async move {
                a.wait().await;
                tokio::task::yield_now().await;
                assert_eq!(account().as_deref(), Some("A"));
            }),
            scope_trusted_reply(target("B"), async move {
                b.wait().await;
                tokio::task::yield_now().await;
                assert_eq!(account().as_deref(), Some("B"));
            })
        );
        assert!(account().is_none());
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(scope_trusted_reply(target("cancelled"), async move {
            let guard = crate::computer_use_sessions::turns::register(
                "scope-cancelled",
                "turn",
                "telegram:42:17",
            )
            .unwrap();
            ready_tx.send(()).unwrap();
            std::future::pending::<()>().await;
            drop(guard);
        }));
        ready_rx.await.unwrap();
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        assert!(
            crate::computer_use_sessions::turns::target_for("scope-cancelled", "turn").is_none()
        );
        assert!(account().is_none());
    }
}

#[cfg(test)]
mod run_link_tests {
    use super::*;
    #[test]
    fn server_ingress_digest_is_preserved_without_exposing_credentials() {
        let target = TrustedReplyTarget::new(
            DecisionContext {
                channel: "line".into(),
                account_id: "bot-A".into(),
                conversation_id: "group".into(),
                principal_id: "user".into(),
            },
            "secret".into(),
            "group".into(),
            None,
        )
        .unwrap();
        let digest = "a".repeat(64);
        let target = target.with_ingress_run(&digest).unwrap();
        assert_eq!(target.ingress_run_id(), Some(digest.as_str()));
        assert!(!target.body("question").to_string().contains("secret"));
        assert!(target.with_ingress_run("client arbitrary value").is_err());
    }
}

// The registered test-only route cannot be configured in production or by env.
fn delivery_url(token: &str, canonical: &str) -> String {
    #[cfg(test)]
    {
        crate::test_channel_provider::url(token, canonical)
    }
    #[cfg(not(test))]
    {
        let _ = token;
        canonical.to_owned()
    }
}

#[cfg(test)]
mod provider_roundtrip_tests {
    use super::*;
    use crate::test_channel_provider::TestChannelProvider;
    use std::sync::Arc;
    #[tokio::test]
    async fn interleaved_account_thread_transport_scope_never_crosses_and_error_cleans_up() {
        let a = TestChannelProvider::start().await;
        let b = TestChannelProvider::start().await;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let target = |token: &str, account: &str, thread: &str| {
            TrustedReplyTarget::new(
                DecisionContext {
                    channel: "slack".into(),
                    account_id: account.into(),
                    conversation_id: format!("same-chat:{thread}"),
                    principal_id: "same-human".into(),
                },
                token.into(),
                "same-chat".into(),
                Some(thread.into()),
            )
        };
        let one = target(&a.token, "account-A", "thread-A");
        let two = target(&b.token, "account-B", "thread-B");
        let wait = barrier.clone();
        let first = tokio::spawn(scope_trusted_reply(one, async move {
            wait.wait().await;
            tokio::task::yield_now().await;
            CURRENT_TRUSTED_REPLY_TARGET
                .with(Clone::clone)
                .unwrap()
                .sender(reqwest::Client::new())
                .send_text("A")
                .await
                .unwrap();
        }));
        let second = tokio::spawn(scope_trusted_reply(two, async move {
            barrier.wait().await;
            scope_trusted_reply(Err("invalid nested route".into()), async {
                assert!(CURRENT_TRUSTED_REPLY_TARGET.with(Clone::clone).is_none());
                assert!(CURRENT_DECISION_CONTEXT.with(Clone::clone).is_none());
            })
            .await;
            CURRENT_TRUSTED_REPLY_TARGET
                .with(Clone::clone)
                .unwrap()
                .sender(reqwest::Client::new())
                .send_text("B")
                .await
                .unwrap();
        }));
        first.await.unwrap();
        second.await.unwrap();
        for (provider, text, thread) in [(&a, "A", "thread-A"), (&b, "B", "thread-B")] {
            let requests = provider.requests();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].body["channel"], "same-chat");
            assert_eq!(requests[0].body["thread_ts"], thread);
            assert_eq!(requests[0].body["text"], text);
            assert_eq!(
                requests[0].authorization,
                Some(format!("Bearer {}", provider.token))
            );
        }
        b.refuse();
        let result = scope_trusted_reply(target(&b.token, "account-B", "thread-B"), async {
            CURRENT_TRUSTED_REPLY_TARGET
                .with(Clone::clone)
                .unwrap()
                .sender(reqwest::Client::new())
                .send_text("refused")
                .await
        })
        .await;
        assert!(result.is_err());
        assert!(CURRENT_TRUSTED_REPLY_TARGET.try_with(Clone::clone).is_err());
        assert!(CURRENT_DECISION_CONTEXT.try_with(Clone::clone).is_err());
        assert_eq!(
            a.requests().len(),
            1,
            "error must not fall back to another account"
        );
        assert_eq!(b.requests().len(), 2);
    }
}
