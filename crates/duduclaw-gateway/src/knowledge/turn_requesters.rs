//! Who a live turn is for, keyed like the computer-use turn registry:
//! `(verified employee id, DUDUCLAW_TURN_ID) → RequesterIdentity`, plus
//! dispatched runs keyed `(employee, DUDUCLAW_DISPATCH_RUN_ID)`.
//!
//! The knowledge route reads this to decide the human side of a tool call.
//! A registration lives as long as its [`RequesterGuard`].
//!
//! Producers:
//! - Channel replies: the pipeline resolves the person once per turn
//!   ([`super::requester_map::requester_for_reply`]: the WebChat account,
//!   a verified channel binding, or an unbound sender; public visitors map
//!   to their channel only) and scopes it as [`CURRENT_REQUESTER`]. Every
//!   runtime call of that turn registers it ([`register_current_turn`]) —
//!   the Claude spawn and `runtime_dispatch::run_agent_prompt` for the
//!   other runtimes.
//! - Cron and dispatched runs (sub-employees, goal rounds) register as
//!   `System` on behalf of nobody ([`register_run`]): no person is in the
//!   loop, so they read only public / internal collections granted to the
//!   employee.
//!
//! A tool call whose turn and run are both unregistered serves nobody.

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

tokio::task_local! {
    /// The person the current channel reply turn is for, resolved once by
    /// the reply pipeline before any runtime call.
    pub static CURRENT_REQUESTER: RequesterIdentity;
}

/// The person of the current channel reply turn; `Unknown` outside one.
pub fn current_requester() -> RequesterIdentity {
    CURRENT_REQUESTER
        .try_with(Clone::clone)
        .unwrap_or(RequesterIdentity::Unknown)
}

/// Register the channel turn the current task is answering, for a runtime
/// call of `agent_id`: turn id from the `CURRENT_TURN_ID` task-local (the
/// value the spawns put into `DUDUCLAW_TURN_ID`), person from
/// [`CURRENT_REQUESTER`]. `None` outside a channel reply turn.
pub fn register_current_turn(agent_id: &str) -> Option<RequesterGuard> {
    let who = CURRENT_REQUESTER.try_with(Clone::clone).ok()?;
    let turn_id = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(|t| t.clone())
        .ok()
        .flatten()?;
    register(agent_id, &turn_id, who)
}

/// Register a cron or dispatched run of `agent_id` (key = its
/// `DUDUCLAW_DISPATCH_RUN_ID`) as a system turn on behalf of nobody.
pub fn register_run(agent_id: &str, run_key: &str, kind: &str) -> Option<RequesterGuard> {
    register(
        agent_id,
        run_key,
        RequesterIdentity::System {
            kind: kind.to_string(),
            on_behalf_of: None,
        },
    )
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
/// the verified employee counts, in this order: the turn's registration (a
/// signed-in person, a channel sender), a live channel turn in the
/// computer-use turn record (an unidentified sender of that chat), then the
/// run's registration (a system turn). Anything else is
/// [`RequesterIdentity::Unknown`], which may read nothing.
pub fn resolve(agent_id: &str, turn_id: Option<&str>) -> RequesterIdentity {
    resolve_with_run(agent_id, turn_id, None)
}

/// [`resolve`] with the caller's dispatch run id as the last resort.
pub fn resolve_with_run(
    agent_id: &str,
    turn_id: Option<&str>,
    run_id: Option<&str>,
) -> RequesterIdentity {
    let valid = |t: &&str| crate::computer_use_sessions::turns::valid_turn_id(t);
    if let Some(turn) = turn_id.filter(valid) {
        if let Some(who) = lookup(agent_id, turn) {
            return who;
        }
        if let Some(who) = crate::computer_use_sessions::turns::reply_channel_for(agent_id, turn)
            .and_then(|reply| unbound_sender_of(&reply))
        {
            return who;
        }
    }
    run_id
        .filter(valid)
        .and_then(|run| lookup(agent_id, run))
        .filter(|who| matches!(who, RequesterIdentity::System { .. }))
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
        let who = RequesterIdentity::DashboardUser {
            user_id: "u-7".into(),
        };
        let fut = async { register_current_turn("kb-agent-d") };
        let guard = CURRENT_REQUESTER
            .scope(
                who.clone(),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("turn-42".to_string()), fut),
            )
            .await
            .unwrap();
        assert_eq!(resolve("kb-agent-d", Some("turn-42")), who);
        drop(guard);
        assert_eq!(
            resolve("kb-agent-d", Some("turn-42")),
            RequesterIdentity::Unknown
        );
        // Outside a channel reply turn nothing is registered, even with a
        // turn id in scope (a dispatched run inherits its caller's turn id).
        let none = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(Some("turn-43".to_string()), async {
                register_current_turn("kb-agent-d")
            })
            .await;
        assert!(none.is_none());
        assert!(register_current_turn("kb-agent-d").is_none());
    }

    #[test]
    fn runs_are_system_turns_and_rank_last() {
        let g = register_run("kb-agent-e", "run0123abcd", "cron").unwrap();
        assert!(matches!(
            resolve_with_run("kb-agent-e", None, Some("run0123abcd")),
            RequesterIdentity::System { on_behalf_of: None, .. }
        ));
        // A run id never stands in for a person: a person registered under
        // that id is ignored on the run path.
        let p = register(
            "kb-agent-e",
            "run-person",
            RequesterIdentity::DashboardUser { user_id: "u".into() },
        )
        .unwrap();
        assert_eq!(
            resolve_with_run("kb-agent-e", None, Some("run-person")),
            RequesterIdentity::Unknown
        );
        // Another employee cannot use the run id.
        assert_eq!(
            resolve_with_run("kb-agent-f", None, Some("run0123abcd")),
            RequesterIdentity::Unknown
        );
        // The turn registration wins over the run.
        let t = register(
            "kb-agent-e",
            "turn-77",
            RequesterIdentity::DashboardUser { user_id: "u-1".into() },
        )
        .unwrap();
        assert_eq!(
            resolve_with_run("kb-agent-e", Some("turn-77"), Some("run0123abcd")),
            RequesterIdentity::DashboardUser { user_id: "u-1".into() }
        );
        // An unknown turn id falls through to the run.
        assert!(matches!(
            resolve_with_run("kb-agent-e", Some("turn-unknown"), Some("run0123abcd")),
            RequesterIdentity::System { .. }
        ));
        drop((g, p, t));
    }
}
