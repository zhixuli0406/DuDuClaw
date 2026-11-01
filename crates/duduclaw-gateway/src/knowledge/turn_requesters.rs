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
    crate::runtime_turn::register_current(agent_id);
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
/// `DUDUCLAW_DISPATCH_RUN_ID`).
///
/// - A cron run, or a dispatched run with no upstream turn (cron, goal
///   loop, dashboard work): a system turn on behalf of nobody.
/// - A dispatched run whose upstream turn id is in scope (`CURRENT_TURN_ID`,
///   set by the dispatcher from the bus message): what that turn's person
///   may read, never more ([`delegated`]); an upstream turn the gateway no
///   longer remembers serves nobody. Delegation cannot widen access.
pub fn register_run(agent_id: &str, run_key: &str, kind: &str) -> Option<RequesterGuard> {
    let upstream = (kind == "dispatch")
        .then(|| {
            duduclaw_memory::feedback::CURRENT_TURN_ID
                .try_with(|t| t.clone())
                .ok()
                .flatten()
        })
        .flatten();
    let who = match upstream {
        Some(turn) => origin_of(&turn)
            .map(|o| delegated(o, kind))
            .unwrap_or(RequesterIdentity::Unknown),
        None => RequesterIdentity::System {
            kind: kind.to_string(),
            on_behalf_of: None,
        },
    };
    register(agent_id, run_key, who)
}

/// What a run dispatched from a turn of `who` may act as: the same person's
/// grants outside a 1:1 conversation (`System` on behalf of them), or, for
/// an unbound sender or visitor, that channel only (never 1:1).
pub fn delegated(who: RequesterIdentity, kind: &str) -> RequesterIdentity {
    match who {
        RequesterIdentity::DashboardUser { user_id }
        | RequesterIdentity::ChannelUser {
            user_id: Some(user_id),
            ..
        } => RequesterIdentity::System {
            kind: kind.to_string(),
            on_behalf_of: Some(user_id),
        },
        RequesterIdentity::ChannelUser { channel, chat, .. } => RequesterIdentity::ChannelUser {
            channel,
            chat,
            sender: String::new(),
            user_id: None,
        },
        system @ RequesterIdentity::System { .. } => system,
        RequesterIdentity::Unknown => RequesterIdentity::Unknown,
    }
}

type Origins = HashMap<String, (std::time::Instant, RequesterIdentity)>;

fn origins() -> std::sync::MutexGuard<'static, Origins> {
    static O: OnceLock<Mutex<Origins>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// How long a turn's person is remembered for runs it dispatched.
const ORIGIN_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);
const MAX_ORIGINS: usize = 10_000;

/// Remember who a channel reply turn was for, so runs it dispatches (which
/// carry its turn id) are limited to that person.
pub fn remember_turn_origin(turn_id: &str, who: RequesterIdentity) {
    if !crate::computer_use_sessions::turns::valid_turn_id(turn_id) {
        return;
    }
    let now = std::time::Instant::now();
    let mut m = origins();
    if m.len() >= MAX_ORIGINS {
        m.retain(|_, (at, _)| now.saturating_duration_since(*at) < ORIGIN_TTL);
        if m.len() >= MAX_ORIGINS {
            m.clear();
        }
    }
    m.insert(turn_id.to_string(), (now, who));
}

/// The person of a remembered turn.
pub fn origin_of(turn_id: &str) -> Option<RequesterIdentity> {
    let m = origins();
    m.get(turn_id)
        .filter(|(at, _)| at.elapsed() < ORIGIN_TTL)
        .map(|(_, who)| who.clone())
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
    // Only what `register_run` writes counts on the run path: a system turn
    // or a channel-only sender, never a person.
    run_id
        .filter(valid)
        .and_then(|run| lookup(agent_id, run))
        .filter(|who| {
            matches!(
                who,
                RequesterIdentity::System { .. }
                    | RequesterIdentity::ChannelUser { user_id: None, .. }
            )
        })
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

    #[tokio::test]
    async fn delegated_runs_never_widen_access() {
        let unbound = RequesterIdentity::ChannelUser {
            channel: "line".into(),
            chat: "Ustranger".into(),
            sender: "Ustranger".into(),
            user_id: None,
        };
        remember_turn_origin("line:Ustranger#t-del-1", unbound);
        // B runs because A delegated from that turn: channel-only, never 1:1.
        let g = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(Some("line:Ustranger#t-del-1".to_string()), async {
                register_run("kb-agent-b", "rundel0001", "dispatch")
            })
            .await
            .unwrap();
        let who = resolve_with_run("kb-agent-b", None, Some("rundel0001"));
        assert_eq!(
            who,
            RequesterIdentity::ChannelUser {
                channel: "line".into(),
                chat: "Ustranger".into(),
                sender: String::new(),
                user_id: None,
            }
        );
        let human = duduclaw_knowledge::requester::human_ctx(&who, &super::super::NoUsers);
        assert_eq!(human.mode, duduclaw_knowledge::acl::HumanMode::ChannelOnly);
        assert!(!human.is_private);
        drop(g);
        // A forgotten upstream turn serves nobody.
        let g = duduclaw_memory::feedback::CURRENT_TURN_ID
            .scope(Some("line:x#t-unknown".to_string()), async {
                register_run("kb-agent-b", "rundel0002", "dispatch")
            })
            .await
            .unwrap();
        assert_eq!(
            resolve_with_run("kb-agent-b", None, Some("rundel0002")),
            RequesterIdentity::Unknown
        );
        drop(g);
        // A signed-in person: their own grants, not 1:1.
        assert_eq!(
            delegated(RequesterIdentity::DashboardUser { user_id: "u1".into() }, "dispatch"),
            RequesterIdentity::System {
                kind: "dispatch".into(),
                on_behalf_of: Some("u1".into())
            }
        );
        // No upstream turn (cron, goal loop): a system turn.
        let g = register_run("kb-agent-b", "rundel0003", "dispatch").unwrap();
        assert!(matches!(
            resolve_with_run("kb-agent-b", None, Some("rundel0003")),
            RequesterIdentity::System { on_behalf_of: None, .. }
        ));
        drop(g);
    }

    /// The id a runtime hands to its MCP server for a run is the key the run
    /// was registered under (`claude_runner::invoke_recorded` mints one key
    /// and uses it for both).
    #[tokio::test]
    async fn every_runtime_sees_the_registered_run_key() {
        let key = crate::memory_provenance::new_dispatch_run_key();
        let pairs = crate::memory_provenance::DISPATCH_RUN
            .scope(Some(("cron:bot".to_string(), key.clone())), async {
                crate::memory_provenance::turn_source_env_pairs()
            })
            .await;
        assert!(
            pairs
                .iter()
                .any(|(k, v)| k == duduclaw_core::ENV_DISPATCH_RUN_ID && *v == key),
            "{pairs:?}"
        );
        let _g = register_run("kb-agent-k", &key, "cron").unwrap();
        assert!(matches!(
            resolve_with_run("kb-agent-k", None, Some(&key)),
            RequesterIdentity::System { .. }
        ));
    }
}
