//! Who a live turn is for, keyed like the computer-use turn registry:
//! `(verified employee id, DUDUCLAW_TURN_ID) → RequesterIdentity`.
//!
//! The knowledge route reads this to decide the human side of a tool call.
//! A registration lives as long as its [`RequesterGuard`].
//!
//! Producers today: `runtime_dispatch::run_agent_prompt` registers every
//! runtime call made inside a channel turn as an unidentified sender of that
//! chat ([`register_current_turn`]), so every runtime (Claude, Codex, Grok,
//! Antigravity, OpenAI-compatible) gets the same answer.
//!
//! TODO(knowledge-requester): the signed-in person behind a WebChat or
//! channel turn (WebChat `auth_user`, verified channel bindings) comes from
//! the trigger-routing work; once it lands, register that identity here, and
//! register cron / heartbeat / dispatch turns as `System`. Until then such
//! turns read only `channel` grants, and turns with no registered id read
//! nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use duduclaw_knowledge::requester::RequesterIdentity;

type Key = (String, String);

fn registry() -> &'static Mutex<HashMap<Key, Vec<(u64, RequesterIdentity)>>> {
    static R: OnceLock<Mutex<HashMap<Key, Vec<(u64, RequesterIdentity)>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Removes the registration when dropped.
#[derive(Debug)]
pub struct RequesterGuard {
    key: Key,
    id: u64,
}

impl Drop for RequesterGuard {
    fn drop(&mut self) {
        let mut map = registry().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(list) = map.get_mut(&self.key) {
            list.retain(|(id, _)| *id != self.id);
            if list.is_empty() {
                map.remove(&self.key);
            }
        }
    }
}

/// Record that `agent_id`'s turn `turn_id` serves `who`.
pub fn register(agent_id: &str, turn_id: &str, who: RequesterIdentity) -> Option<RequesterGuard> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    if !duduclaw_core::is_valid_agent_id(agent_id)
        || !crate::computer_use_sessions::turns::valid_turn_id(turn_id)
    {
        return None;
    }
    let key = (agent_id.to_string(), turn_id.to_string());
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(key.clone())
        .or_default()
        .push((id, who));
    Some(RequesterGuard { key, id })
}

/// The newest registration for `(agent_id, turn_id)`, exact match.
pub fn lookup(agent_id: &str, turn_id: &str) -> Option<RequesterIdentity> {
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(agent_id.to_string(), turn_id.to_string()))
        .and_then(|l| l.last())
        .map(|(_, who)| who.clone())
}

/// Register the channel turn the current task is answering, for a runtime
/// call of `agent_id`: turn id from the `CURRENT_TURN_ID` task-local, chat
/// from the `REPLY_CHANNEL` task-local (else the session id) — the values
/// the spawns put into `DUDUCLAW_TURN_ID` / `DUDUCLAW_REPLY_CHANNEL`. The
/// sender is not known yet here, so the turn serves an unidentified sender of
/// that chat. `None` outside a channel turn.
pub fn register_current_turn(agent_id: &str) -> Option<RequesterGuard> {
    let turn_id = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(|t| t.clone())
        .ok()
        .flatten()?;
    let reply = crate::claude_runner::REPLY_CHANNEL
        .try_with(|c| c.clone())
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| {
            duduclaw_memory::feedback::CURRENT_SESSION_ID
                .try_with(|s| s.clone())
                .ok()
                .flatten()
        })?;
    register(agent_id, &turn_id, unbound_sender_of(&reply)?)
}

/// An unidentified sender of the chat `<channel>:<chat>`.
fn unbound_sender_of(reply: &str) -> Option<RequesterIdentity> {
    let (channel, chat) = reply.split_once(':')?;
    if channel.is_empty() || chat.is_empty() || reply.chars().any(char::is_control) {
        return None;
    }
    Some(RequesterIdentity::ChannelUser {
        channel: channel.to_string(),
        chat: chat.to_string(),
        sender: String::new(),
        user_id: None,
    })
}

/// The requester of a tool call. Only what the gateway itself registered for
/// the verified employee's turn counts: the identity in this registry (a
/// signed-in person, a channel sender, or a system turn); else, for a live
/// channel turn in the computer-use turn record, an unidentified sender of
/// that chat. A missing, malformed or unregistered turn id is
/// [`RequesterIdentity::Unknown`], which may read nothing.
pub fn resolve(agent_id: &str, turn_id: Option<&str>) -> RequesterIdentity {
    let Some(turn) = turn_id.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t))
    else {
        return RequesterIdentity::Unknown;
    };
    if let Some(who) = lookup(agent_id, turn) {
        return who;
    }
    crate::computer_use_sessions::turns::reply_channel_for(agent_id, turn)
        .and_then(|reply| unbound_sender_of(&reply))
        .unwrap_or(RequesterIdentity::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_lookup_and_drop() {
        let who = RequesterIdentity::DashboardUser {
            user_id: "u1".into(),
        };
        let g = register("kb-agent-a", "turn-1", who.clone()).unwrap();
        assert_eq!(resolve("kb-agent-a", Some("turn-1")), who);
        // Another employee cannot use this turn id.
        assert_eq!(
            resolve("kb-agent-b", Some("turn-1")),
            RequesterIdentity::Unknown
        );
        drop(g);
        assert!(lookup("kb-agent-a", "turn-1").is_none());
        assert!(register("../x", "t", who).is_none());
    }

    #[test]
    fn live_channel_turn_without_identity_is_an_unbound_sender() {
        let _turn =
            crate::computer_use_sessions::turns::register("kb-agent-c", "turn-9", "line:Cgroup1")
                .unwrap();
        match resolve("kb-agent-c", Some("turn-9")) {
            RequesterIdentity::ChannelUser {
                channel,
                chat,
                user_id,
                ..
            } => {
                assert_eq!(
                    (channel.as_str(), chat.as_str(), user_id),
                    ("line", "Cgroup1", None)
                );
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(resolve("kb-agent-c", None), RequesterIdentity::Unknown);
        assert_eq!(
            resolve("kb-agent-c", Some("turn-x")),
            RequesterIdentity::Unknown
        );
        assert_eq!(
            resolve("kb-agent-c", Some("bad\nturn")),
            RequesterIdentity::Unknown
        );
    }

    #[tokio::test]
    async fn the_current_channel_turn_is_registered_from_task_locals() {
        let fut = async { register_current_turn("kb-agent-d") };
        let guard = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(
                Some("turn-42".to_string()),
                crate::claude_runner::REPLY_CHANNEL.scope("line:Cgroup7".to_string(), fut),
            )
            .await
            .unwrap();
        assert!(matches!(
            resolve("kb-agent-d", Some("turn-42")),
            RequesterIdentity::ChannelUser { user_id: None, .. }
        ));
        drop(guard);
        assert_eq!(
            resolve("kb-agent-d", Some("turn-42")),
            RequesterIdentity::Unknown
        );
        // Outside a channel turn nothing is registered.
        assert!(register_current_turn("kb-agent-d").is_none());
    }
}
