//! Google Chat's permission scope, conversation history and reply destination.

use serde_json::Value;

pub(crate) enum TurnLease {
    Dedicated { _guard: tokio::sync::OwnedRwLockReadGuard<()> },
    Shared { _guard: tokio::sync::OwnedRwLockWriteGuard<()> },
}

/// Built once from an authenticated HTTP event or authoritative Pub/Sub read.
/// Fields stay private so delivery and history cannot select different threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoogleChatConversation {
    space: String,
    gate_session: String,
    session_key: String,
    thread: Option<String>,
    is_dm: bool,
}

impl GoogleChatConversation {
    pub(crate) fn from_event(event: &Value, is_dm: bool, always_thread: bool) -> Result<Self, &'static str> {
        let space = event.pointer("/message/space/name").or_else(|| event.pointer("/space/name"))
            .and_then(Value::as_str).ok_or("missing space")?;
        if !crate::google_workspace::chat_read::valid_space(space) { return Err("invalid space"); }
        for path in ["/message/space/name", "/space/name"] {
            if event.pointer(path).is_some_and(|name| name.as_str() != Some(space)) {
                return Err("conflicting spaces");
            }
        }
        // Validate even an unused DM thread: it must never carry a forged
        // destination into decision routing, notices or a later placement change.
        let carried_thread = match event.pointer("/message/thread") {
            None => None,
            Some(thread) => {
                let name = thread.get("name").and_then(Value::as_str).ok_or("invalid thread")?;
                if !valid_thread(space, name) { return Err("thread does not belong to space"); }
                Some(name)
            }
        };
        let threaded = !is_dm || always_thread
            || event.pointer("/message/threadReply").and_then(Value::as_bool) == Some(true);
        let thread = if threaded { Some(carried_thread.ok_or("missing thread")?.to_owned()) } else { None };
        let gate_session = format!("googlechat:{space}");
        let session_key = match &thread {
            Some(thread) => format!("{gate_session}:thread:{thread}"),
            None => gate_session.clone(),
        };
        // No data migration: old group sessions remain untouched legacy history;
        // thread keys start empty, while unthreaded DMs keep their exact key.
        Ok(Self { space: space.into(), gate_session, session_key, thread, is_dm })
    }

    pub(crate) fn space(&self) -> &str { &self.space }
    pub(crate) fn gate_session(&self) -> &str { &self.gate_session }
    pub(crate) fn session_key(&self) -> &str { &self.session_key }
    pub(crate) fn delivery_thread(&self) -> Option<&str> { self.thread.as_deref() }
    pub(crate) fn is_dm(&self) -> bool { self.is_dm }
    pub(crate) fn session_for(&self, agent: Option<&str>) -> String {
        agent.map_or_else(|| self.session_key.clone(), |id| crate::trigger_route::called_session(&self.session_key, id))
    }

    pub(crate) fn queue_keys(&self, app_agent: Option<&str>) -> (String, String) {
        // Keep each app independent while serializing all senders in one
        // thread of that app. Shared-app routing is resolved inside its queue.
        (serde_json::json!([app_agent, self.space]).to_string(),
            serde_json::json!([app_agent, self.session_key]).to_string())
    }

    /// Dedicated apps have disjoint employee buckets and may run together.
    /// A shared app can route to any of them, so hold the whole thread until
    /// routing, commands, model work and delivery finish. Entry queues still
    /// serialize senders of each app and enforce their original admission caps.
    pub(crate) async fn turn_lease(&self, home: &std::path::Path, own_app: bool) -> std::io::Result<TurnLease> {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex, OnceLock, Weak};
        type Locks = HashMap<(std::path::PathBuf, String), Weak<tokio::sync::RwLock<()>>>;
        static LOCKS: OnceLock<Mutex<Locks>> = OnceLock::new();
        let key = (home.canonicalize()?, self.session_key.clone());
        let lock = {
            let mut locks = LOCKS.get_or_init(Default::default).lock()
                .map_err(|_| std::io::Error::other("Google Chat thread lock unavailable"))?;
            locks.retain(|_, lock| lock.strong_count() > 0);
            let slot = locks.entry(key).or_default();
            match slot.upgrade() {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::RwLock::new(()));
                    *slot = Arc::downgrade(&lock);
                    lock
                }
            }
        };
        Ok(if own_app { TurnLease::Dedicated { _guard: lock.read_owned().await } }
            else { TurnLease::Shared { _guard: lock.write_owned().await } })
    }
}

fn valid_thread(space: &str, thread: &str) -> bool {
    thread.strip_prefix(space).and_then(|tail| tail.strip_prefix("/threads/"))
        .is_some_and(|id| (1..=128).contains(&id.len())
            && !matches!(id, "." | "..")
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
}

/// Recover only the original space from a canonical session key. This checks
/// the full thread and agent suffix before it can participate in attestation.
pub(crate) fn permission_session(session: &str) -> Option<&str> {
    let base = match session.split_once("#agent:") {
        Some((base, agent)) if duduclaw_core::is_valid_agent_id(agent) => base,
        Some(_) => return None,
        None => session,
    };
    let (gate, thread) = base.split_once(":thread:").map_or((base, None), |(gate, thread)| (gate, Some(thread)));
    let space = gate.strip_prefix("googlechat:")?;
    if !crate::google_workspace::chat_read::valid_space(space)
        || thread.is_some_and(|thread| !valid_thread(space, thread)) { return None; }
    Some(gate)
}

pub(crate) fn space_from_session(session: &str) -> Option<&str> {
    permission_session(session)?.strip_prefix("googlechat:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(space: &str, thread: &str, reply: bool) -> serde_json::Value {
        json!({"space":{"name":space},"message":{"space":{"name":space},
            "thread":{"name":thread},"threadReply":reply}})
    }

    #[test]
    fn rm182_group_threads_share_participants_but_isolate_history_and_agents() {
        let a = GoogleChatConversation::from_event(&event("spaces/A", "spaces/A/threads/one", false), false, false).unwrap();
        let b = GoogleChatConversation::from_event(&event("spaces/A", "spaces/A/threads/two", true), false, false).unwrap();
        assert_eq!(a.gate_session(), "googlechat:spaces/A");
        assert_eq!(a.space(), "spaces/A");
        assert_eq!(a.delivery_thread(), Some("spaces/A/threads/one"));
        assert_ne!(a.session_key(), b.session_key());
        assert!(a.session_key().contains("spaces/A/threads/one"));
        assert_ne!(a.session_for(Some("sales")), a.session_for(Some("hr")));
        assert_eq!(a.session_for(Some("sales")), crate::trigger_route::called_session(a.session_key(), "sales"));
        let mut other_sender = event("spaces/A", "spaces/A/threads/one", false);
        other_sender["message"]["sender"] = json!({"name":"users/222"});
        let same = GoogleChatConversation::from_event(&other_sender, false, false).unwrap();
        assert_eq!(a.session_key(), same.session_key());
        assert_eq!(a.queue_keys(Some("sales")).1, same.queue_keys(Some("sales")).1);
        assert_ne!(a.queue_keys(Some("sales")).1, same.queue_keys(Some("hr")).1);
        assert_ne!(a.queue_keys(None).1, same.queue_keys(Some("sales")).1);
    }

    #[test]
    fn rm182_dm_main_key_survives_and_thread_placement_selects_a_thread() {
        let root = event("spaces/DM", "spaces/DM/threads/root", false);
        let main = GoogleChatConversation::from_event(&root, true, false).unwrap();
        assert_eq!(main.session_key(), "googlechat:spaces/DM");
        assert_eq!(main.delivery_thread(), None);
        let placed = GoogleChatConversation::from_event(&root, true, true).unwrap();
        let replied = GoogleChatConversation::from_event(&event("spaces/DM", "spaces/DM/threads/root", true), true, false).unwrap();
        assert_eq!(placed.session_key(), replied.session_key());
        assert_ne!(main.session_key(), placed.session_key());
        assert_eq!(placed.gate_session(), main.gate_session());
        assert_eq!(GoogleChatConversation::from_event(&json!({"space":{"name":"spaces/DM"},"message":{}}), true, false).unwrap().session_key(), main.session_key());
    }

    #[test]
    fn rm182_forged_threads_and_conflicting_spaces_are_refused_even_for_main_dm() {
        for is_dm in [false, true] {
            for thread in ["spaces/OTHER/threads/t", "spaces/A/threads/", "spaces/A/threads/../x",
                "spaces/A/threads/t#agent:admin", "spaces/A/threads/t?x=1", "spaces/A/threads/t/extra"] {
                assert!(GoogleChatConversation::from_event(&event("spaces/A", thread, false), is_dm, false).is_err(), "{thread}");
            }
        }
        let mut conflict = event("spaces/A", "spaces/A/threads/t", false);
        conflict["space"]["name"] = json!("spaces/B");
        assert!(GoogleChatConversation::from_event(&conflict, true, false).is_err());
        for malformed in [json!({"thread":{}}), json!({"thread":{"name":12}})] {
            let e = json!({"space":{"name":"spaces/A"}, "message":malformed});
            assert!(GoogleChatConversation::from_event(&e, true, false).is_err());
        }
        assert!(GoogleChatConversation::from_event(&json!({"space":{"name":"spaces/A"},"message":{}}), false, false).is_err());
    }

    #[test]
    fn rm182_session_scope_parser_validates_thread_before_attesting_original_space() {
        let c = GoogleChatConversation::from_event(&event("spaces/A", "spaces/A/threads/t", true), true, false).unwrap();
        for key in [c.session_key().to_owned(), c.session_for(Some("sales")), c.gate_session().to_owned()] {
            assert_eq!(space_from_session(&key), Some("spaces/A"));
            assert_eq!(permission_session(&key), Some("googlechat:spaces/A"));
        }
        for key in ["googlechat:spaces/A:thread:spaces/B/threads/t", "googlechat:spaces/A:other:t",
            "googlechat:spaces/A:thread:spaces/A/threads/t#agent:bad#agent:x", "googlechat:spaces/A/threads/t"] {
            assert_eq!(space_from_session(key), None, "{key}");
        }
    }

    #[tokio::test]
    async fn rm182_queue_serializes_different_senders_and_apps_in_one_thread() {
        let home = tempfile::tempdir().unwrap();
        let c = GoogleChatConversation::from_event(&event("spaces/A", "spaces/A/threads/t", true), false, false).unwrap();
        let queue = crate::conversation_queue::ConversationQueue::default();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let (g1, k1) = c.queue_keys(None);
        let (g2, k2) = c.queue_keys(Some("sales"));
        let shared = c.clone();
        let shared_home = home.path().to_owned();
        queue.submit_in(&g1, &k1, async move {
            let _lease = shared.turn_lease(&shared_home, false).await.unwrap();
            entered.send(()).unwrap(); wait.await.unwrap();
        }).unwrap();
        started.await.unwrap();
        let (second, mut second_started) = tokio::sync::oneshot::channel();
        let dedicated_home = home.path().to_owned();
        queue.submit_in(&g2, &k2, async move {
            let _lease = c.turn_lease(&dedicated_home, true).await.unwrap();
            second.send(()).unwrap(); done.send(()).unwrap();
        }).unwrap();
        tokio::task::yield_now().await;
        assert!(matches!(second_started.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        release.send(()).unwrap();
        finished.await.unwrap();
        second_started.await.unwrap();
    }
}
