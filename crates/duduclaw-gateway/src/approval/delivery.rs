//! Exact transport targets captured by authenticated inbound adapters. Tokens
//! live only in memory, are never serializable/debuggable, and never fall back.
use super::{CURRENT_DECISION_CONTEXT, DecisionContext};
use crate::channel_sender::{ChannelSendError, ChannelSender};
use async_trait::async_trait;

#[derive(Clone)]
pub(crate) struct TrustedReplyTarget {
    pub context: DecisionContext,
    pub conversation_audience: super::ConversationAudience,
    token: String,
    chat_id: String,
    thread_id: Option<String>,
    ingress_run_id: Option<String>,
    decision_access_scope: Option<(Option<String>, Option<String>, Option<String>)>,
    user_access_policy: Option<crate::channel_reply::UserAccessPolicy>,
}
impl TrustedReplyTarget {
    /// Persist only the exact transport address and a token digest. The
    /// credential itself remains in its configured secret store.
    pub(crate) fn notice_descriptor(&self) -> serde_json::Value {
        use sha2::{Digest, Sha256};
        serde_json::json!({
            "context": self.context, "chat_id": self.chat_id,
            "thread_id": self.thread_id,
            "token_sha256": hex::encode(Sha256::digest(self.token.as_bytes())),
        })
    }

    /// Background notifications never switch bot accounts after restart.
    pub(crate) async fn restore_notice(home: &std::path::Path, value: &serde_json::Value) -> Result<Self, String> {
        use sha2::{Digest, Sha256};
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stored {
            context: DecisionContext,
            chat_id: String,
            thread_id: Option<String>,
            token_sha256: String,
        }
        let saved: Stored = serde_json::from_value(value.clone()).map_err(|_| "通知目的地無法辨識。")?;
        saved.context.validate()?;
        if saved.token_sha256.len() != 64 { return Err("通知帳號無法辨識。".into()); }
        for token in crate::config_crypto::channel_dm_token_candidates(home, &saved.context.channel).await {
            if hex::encode(Sha256::digest(token.as_bytes())) == saved.token_sha256 {
                return Self::new(saved.context, token, saved.chat_id, saved.thread_id);
            }
        }
        Err("原對話的通知帳號已變更，請至控制台查看逐字稿。".into())
    }

    /// Attach to the same bot/chat/thread as the captured inbound turn.
    pub(crate) async fn send_notice_document(&self, http: &reqwest::Client, data: &[u8], filename: &str) -> Result<(), String> {
        let part = || reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_owned()).mime_str("text/markdown")
            .map_err(|_| "無法準備逐字稿附件。".to_string());
        let response = match self.context.channel.as_str() {
            "telegram" => {
                let mut form = reqwest::multipart::Form::new().text("chat_id", self.chat_id.clone()).part("document", part()?);
                if let Some(thread) = &self.thread_id { form = form.text("message_thread_id", thread.clone()); }
                http.post(format!("{}/sendDocument", crate::telegram::telegram_poll_api_base(&self.token)))
                    .multipart(form).send().await
            }
            "discord" => {
                let form = reqwest::multipart::Form::new().part("files[0]", part()?)
                    .text("payload_json", serde_json::json!({"allowed_mentions":{"parse":[]}}).to_string());
                http.post(format!("https://discord.com/api/v10/channels/{}/messages", self.chat_id))
                    .header("Authorization", format!("Bot {}", self.token)).multipart(form).send().await
            }
            "slack" => {
                let response = http.post("https://slack.com/api/files.getUploadURLExternal")
                    .bearer_auth(&self.token).form(&[("filename", filename.to_owned()), ("length", data.len().to_string())])
                    .send().await.map_err(|_| "無法傳送逐字稿附件。")?;
                if !response.status().is_success() { return Err("無法傳送逐字稿附件。".into()); }
                let value: serde_json::Value = response.json().await.map_err(|_| "無法讀取附件傳送結果。")?;
                if value["ok"] != true { return Err("無法傳送逐字稿附件。".into()); }
                let url = value["upload_url"].as_str().ok_or("無法取得附件上傳位置。")?;
                let parsed = url::Url::parse(url).map_err(|_| "附件上傳位置無效。")?;
                if parsed.scheme() != "https" || parsed.host_str() != Some("files.slack.com")
                    || !parsed.username().is_empty() || parsed.password().is_some() || parsed.port().is_some_and(|p| p != 443) {
                    return Err("附件上傳位置無效。".into());
                }
                let id = value["file_id"].as_str().ok_or("無法取得附件編號。")?;
                let uploaded = http.post(url).body(data.to_vec()).send().await.map_err(|_| "無法上傳逐字稿附件。")?;
                if !uploaded.status().is_success() { return Err("無法上傳逐字稿附件。".into()); }
                let mut body = serde_json::json!({"files":[{"id":id,"title":filename}],"channel_id":self.chat_id});
                if let Some(thread) = &self.thread_id { body["thread_ts"] = thread.clone().into(); }
                http.post("https://slack.com/api/files.completeUploadExternal")
                    .bearer_auth(&self.token).json(&body).send().await
            }
            _ => return Err("此通道無法直接傳送逐字稿附件。".into()),
        }.map_err(|_| "無法傳送逐字稿附件。")?;
        if !response.status().is_success() { return Err("無法傳送逐字稿附件。".into()); }
        if matches!(self.context.channel.as_str(), "telegram" | "slack") {
            let value: serde_json::Value = response.json().await.map_err(|_| "無法讀取附件傳送結果。")?;
            if value["ok"] != true { return Err("無法傳送逐字稿附件。".into()); }
        }
        Ok(())
    }

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
            conversation_audience: Default::default(),
            context,
            token,
            chat_id,
            thread_id,
            ingress_run_id: None,
            decision_access_scope: None,
            user_access_policy: None,
        })
    }
    /// The caller must supply the chat type from its authenticated adapter,
    /// never from message text, tool arguments or a missing-group heuristic.
    pub(crate) fn with_chat_type(mut self, chat_type: Option<&str>) -> Self {
        self.conversation_audience = super::mail_requester::adapter_audience(
            &self.context.channel, chat_type,
        );
        self
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
            watch_notice: false,
        })
    }
    pub(crate) async fn send_watch_notice(&self, text: &str) -> Result<(), ChannelSendError> {
        BoundDecisionSender { target: self.clone(), http: reqwest::Client::new(), watch_notice: true }
            .send_text(text).await
    }
    fn watch_body(&self, text: &str) -> serde_json::Value {
        let mut body = self.body(text);
        match self.context.channel.as_str() {
            "telegram" => body["link_preview_options"] = serde_json::json!({"is_disabled":true}),
            "slack" => {
                body["unfurl_links"] = serde_json::json!(false);
                body["unfurl_media"] = serde_json::json!(false);
            }
            _ => {}
        }
        body
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
    watch_notice: bool,
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
        req = req.json(&if self.watch_notice { t.watch_body(text) } else { t.body(text) });
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

    #[tokio::test]
    async fn meeting_notice_descriptor_has_no_credential_and_refuses_missing_original_account() {
        let target = TrustedReplyTarget::new(DecisionContext {
            channel:"telegram".into(), account_id:"bot-A".into(),
            conversation_id:"chat:7".into(), principal_id:"person".into(),
        }, "synthetic-private-token".into(), "chat".into(), Some("7".into())).unwrap();
        let descriptor = target.notice_descriptor();
        assert!(!descriptor.to_string().contains("synthetic-private-token"));
        assert_eq!(descriptor["thread_id"], "7");
        assert_eq!(descriptor["context"]["account_id"], "bot-A");
        let home = tempfile::tempdir().unwrap();
        assert!(TrustedReplyTarget::restore_notice(home.path(), &descriptor).await.is_err());
    }
    #[test]
    fn browser_notice_keeps_exact_thread_and_disables_supported_previews() {
        for (channel, thread, key) in [("telegram","17","message_thread_id"),("slack","123.456","thread_ts")] {
            let target = TrustedReplyTarget::new(DecisionContext { channel:channel.into(),
                account_id:"bot-A".into(),conversation_id:format!("chat:{thread}"),principal_id:"human".into() },
                "private-bot-A".into(),"chat".into(),Some(thread.into())).unwrap();
            let watch = target.watch_body("https://dashboard.example/computer/alice?l=context.tag");
            assert_eq!(watch[key], target.body("normal")[key]);
            assert_eq!(target.context.account_id,"bot-A");
            assert!(!watch.to_string().contains("private-bot-A"));
            if channel == "telegram" { assert_eq!(watch["link_preview_options"]["is_disabled"],true); }
            else { assert_eq!(watch["unfurl_links"],false); assert_eq!(watch["unfurl_media"],false); }
            assert!(target.body("normal").get("link_preview_options").is_none());
            assert!(target.body("normal").get("unfurl_links").is_none());
        }
    }
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
