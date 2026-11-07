//! Authenticated, conversation-scoped live delivery. History survives transport loss.
use super::*;
use std::sync::Weak;

type Targets = Mutex<HashMap<String, (Option<String>, u64)>>;

#[derive(Default)]
pub(in crate::webchat) struct Subscriptions(Mutex<Vec<Weak<Subscription>>>);

pub(in crate::webchat) struct Subscription {
    user: String,
    tx: outbound::OutboundSender,
    targets: Arc<Targets>,
    epoch: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub(in crate::webchat) struct Route {
    targets: Weak<Targets>,
    sid: String,
    pub(in crate::webchat) user: String,
    pub(in crate::webchat) agent: String,
    epoch: u64,
    pub(in crate::webchat) origin_credential: Option<duduclaw_auth::jwt::Claims>,
    pub(in crate::webchat) source: crate::channel_reply::ReplyDeliveryFence,
}

impl Route {
    pub(in crate::webchat) async fn authorized(&self, state: &WebChatState) -> bool {
        if state.credential_validity(self.origin_credential.as_ref(), true).await
            != crate::handlers::live_context::SocketValidity::Valid {
            self.source.refuse();
        }
        if !self.source.valid().await { return false; }
        if state.authorize_target(&self.user, &self.agent, true).await.is_err() {
            self.source.refuse();
            return false;
        }
        true
    }

    pub(super) fn live(&self) -> bool {
        self.with_live(|| ()).is_some()
    }

    pub(super) fn with_live<T>(&self, deliver: impl FnOnce() -> T) -> Option<T> {
        let targets = self.targets.upgrade()?;
        let targets = targets.lock().unwrap();
        if !targets.get(&self.sid).is_some_and(|(_, epoch)| *epoch == self.epoch) { return None; }
        Some(deliver())
    }
}

impl Subscription {
    pub(in crate::webchat) fn resumed_turns(&self) -> Vec<(String, Option<String>)> {
        self.targets.lock().unwrap().iter().map(|(sid, (conv, _))| (sid.clone(), conv.clone())).collect()
    }

    pub(in crate::webchat) fn clear(&self) {
        self.targets.lock().unwrap().clear();
    }

    // Called only after the socket's normal session ownership and ACL gates.
    pub(in crate::webchat) fn subscribe(&self, sid: &str, conv: Option<String>) {
        let mut targets = self.targets.lock().unwrap();
        // Every admitted message subscribes its socket. A later FIFO waiter
        // can finish admission after the preceding reply was queued; keeping
        // the same destination must not invalidate that reply's route.
        if targets.get(sid).is_some_and(|(current, _)| current == &conv) {
            return;
        }
        let epoch = self.epoch.fetch_add(1, Ordering::Relaxed);
        if !targets.contains_key(sid) && targets.len() >= 64 {
            if let Some(oldest) = targets.iter().min_by_key(|(_, (_, epoch))| epoch).map(|(sid, _)| sid.clone()) {
                targets.remove(&oldest);
            }
        }
        targets.insert(sid.into(), (conv, epoch));
    }
}

impl Subscriptions {
    pub(in crate::webchat) fn register(&self, user: &str, tx: outbound::OutboundSender) -> Arc<Subscription> {
        let subscription = Arc::new(Subscription {
            user: user.into(), tx, targets: Arc::new(Mutex::new(HashMap::new())),
            epoch: std::sync::atomic::AtomicU64::new(0),
        });
        let mut entries = self.0.lock().unwrap();
        entries.retain(|entry| entry.strong_count() > 0);
        entries.push(Arc::downgrade(&subscription));
        subscription
    }

    pub(super) fn sinks(&self, user: &str, sid: &str, msg_id: &str, agent: &str, credential: Option<&duduclaw_auth::jwt::Claims>,
        source: crate::channel_reply::ReplyDeliveryFence) -> Vec<TurnSink> {
        self.0.lock().unwrap().iter().filter_map(Weak::upgrade)
            .filter(|subscription| subscription.user == user)
            .filter_map(|subscription| {
                let (conv, epoch) = subscription.targets.lock().unwrap().get(sid).cloned()?;
                Some(TurnSink::new(subscription.tx.clone(), sid, conv, msg_id)
                    .routed(Route { targets: Arc::downgrade(&subscription.targets), sid: sid.into(), user: user.into(), agent: agent.into(), epoch,
                        origin_credential: credential.cloned(), source: source.clone() })
                    .acknowledged())
            }).collect()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::live_context::{SocketRecovery, SocketValidity};

    #[tokio::test]
    async fn repeated_conversation_subscription_keeps_already_queued_reply() {
        for conv in [None, Some("same-tab".to_string())] {
            let registry = Subscriptions::default();
            let (tx, mut rx) = outbound_channel();
            let subscription = registry.register("alice", tx);
            subscription.subscribe("owned", conv.clone());
            let mut turn = registry.sinks("alice", "owned", "first", "finance", None,
                crate::channel_reply::ReplyDeliveryFence::test_live()).pop().unwrap();
            let sending = tokio::spawn(async move {
                turn.send(Message::Text(r#"{"type":"assistant_done","content":"reply"}"#.into())).await
            });
            let frame = rx.recv().await.unwrap();
            // Model completion raced the next message's admission. That
            // message still targets the same conversation on this socket.
            subscription.subscribe("owned", conv.clone());
            let captured = Arc::new(Mutex::new(Vec::new()));
            let writer = futures_util::sink::unfold(captured.clone(), |captured, message: Message| async {
                captured.lock().unwrap().push(message);
                Ok::<_, ()>(captured)
            });
            frame.deliver_authorized(SocketValidity::Valid, &mut SocketRecovery::default(),
                &mut BoundedSocketSink(Box::pin(writer))).await.unwrap();
            assert!(sending.await.unwrap().is_ok());
            let sent = captured.lock().unwrap();
            assert_eq!(sent.len(), 1);
            let Message::Text(text) = &sent[0] else { panic!("expected reply text"); };
            let reply: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(reply["type"], "assistant_done");
            assert_eq!(reply["session_id"], "owned");
            assert_eq!(reply["conv"], serde_json::json!(conv));
            assert_eq!(reply["msg_id"], "first");
        }
    }

    #[tokio::test]
    async fn switched_subscription_discards_already_queued_reply() {
        for (clear, next_conv) in [(false, "new-tab"), (true, "old-tab"), (true, "new-tab")] {
            let registry = Subscriptions::default();
            let (tx, mut rx) = outbound_channel();
            let subscription = registry.register("alice", tx);
            subscription.subscribe("owned", Some("old-tab".into()));
            let mut sinks = registry.sinks("alice", "owned", "m", "finance", None,
                crate::channel_reply::ReplyDeliveryFence::test_live());
            assert_eq!(sinks.len(), 1);
            assert!(registry.sinks("bob", "owned", "m", "finance", None,
                crate::channel_reply::ReplyDeliveryFence::test_live()).is_empty());
            let mut turn = sinks.pop().unwrap();
            let sending = tokio::spawn(async move {
                turn.send(Message::Text(r#"{"type":"assistant_done","content":"private"}"#.into())).await
            });
            let frame = rx.recv().await.unwrap();
            // A changed destination or an explicit reset invalidates the old
            // route, even when the reset subscribes to the same sid and conv.
            if clear { subscription.clear(); }
            subscription.subscribe("owned", Some(next_conv.into()));
            let captured = Arc::new(Mutex::new(Vec::new()));
            let writer = futures_util::sink::unfold(captured.clone(), |captured, message: Message| async {
                captured.lock().unwrap().push(message);
                Ok::<_, ()>(captured)
            });
            frame.deliver_authorized(SocketValidity::Valid, &mut SocketRecovery::default(),
                &mut BoundedSocketSink(Box::pin(writer))).await.unwrap();
            assert!(sending.await.unwrap().is_err());
            assert!(captured.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn subscription_capacity_and_disconnect_are_bounded() {
        let registry = Subscriptions::default();
        let (tx, _rx) = outbound_channel();
        let subscription = registry.register("alice", tx);
        for index in 0..1000 { subscription.subscribe(&format!("s{index}"), None); }
        assert_eq!(subscription.targets.lock().unwrap().len(), 64);
        assert!(!subscription.targets.lock().unwrap().contains_key("s0"));
        drop(subscription);
        assert!(registry.sinks("alice", "s999", "m", "finance", None,
            crate::channel_reply::ReplyDeliveryFence::test_live()).is_empty());
    }
}
