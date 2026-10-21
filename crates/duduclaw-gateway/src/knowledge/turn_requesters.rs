//! Who a live turn is for, keyed like the computer-use turn registry:
//! `(verified employee id, DUDUCLAW_TURN_ID) → RequesterIdentity`.
//!
//! The knowledge route reads this to decide the human side of a tool call.
//! A registration lives as long as its [`RequesterGuard`].
//!
//! TODO(knowledge-requester): nothing registers here yet. The channel-reply
//! path resolves the human sender in the trigger-routing work (WebChat
//! `auth_user`, verified channel bindings); once that lands, the spawn site
//! that already calls `computer_use_sessions::turns::register_current_turn`
//! registers the resolved identity here too. Until then the route falls back
//! to the gateway's own record of the turn's reply channel (an unidentified
//! sender: channel grants only) or, with no live turn, to a system turn.

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
    if !duduclaw_core::is_valid_agent_id(agent_id) || !crate::computer_use_sessions::turns::valid_turn_id(turn_id) {
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

/// The requester of a tool call: the registered identity; else, for a live
/// channel turn known to the gateway, an unidentified sender of that chat;
/// else a system turn. Never derived from anything the caller sends except
/// the opaque turn id, which only matches the verified employee's own turns.
pub fn resolve(agent_id: &str, turn_id: Option<&str>) -> RequesterIdentity {
    let Some(turn) = turn_id.filter(|t| crate::computer_use_sessions::turns::valid_turn_id(t)) else {
        return RequesterIdentity::System {
            kind: "agent_turn".into(),
            on_behalf_of: None,
        };
    };
    if let Some(who) = lookup(agent_id, turn) {
        return who;
    }
    match crate::computer_use_sessions::turns::reply_channel_for(agent_id, turn) {
        Some(reply) => {
            let (channel, chat) = reply.split_once(':').unwrap_or((reply.as_str(), ""));
            RequesterIdentity::ChannelUser {
                channel: channel.to_string(),
                chat: chat.to_string(),
                sender: String::new(),
                user_id: None,
            }
        }
        None => RequesterIdentity::System {
            kind: "agent_turn".into(),
            on_behalf_of: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_lookup_and_drop() {
        let who = RequesterIdentity::DashboardUser { user_id: "u1".into() };
        let g = register("kb-agent-a", "turn-1", who.clone()).unwrap();
        assert_eq!(resolve("kb-agent-a", Some("turn-1")), who);
        // Another employee cannot use this turn id.
        assert!(matches!(
            resolve("kb-agent-b", Some("turn-1")),
            RequesterIdentity::System { .. }
        ));
        drop(g);
        assert!(lookup("kb-agent-a", "turn-1").is_none());
        assert!(register("../x", "t", who).is_none());
    }

    #[test]
    fn live_channel_turn_without_identity_is_an_unbound_sender() {
        let _turn = crate::computer_use_sessions::turns::register("kb-agent-c", "turn-9", "line:Cgroup1").unwrap();
        match resolve("kb-agent-c", Some("turn-9")) {
            RequesterIdentity::ChannelUser {
                channel, chat, user_id, ..
            } => {
                assert_eq!((channel.as_str(), chat.as_str(), user_id), ("line", "Cgroup1", None));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            resolve("kb-agent-c", None),
            RequesterIdentity::System { on_behalf_of: None, .. }
        ));
    }
}
