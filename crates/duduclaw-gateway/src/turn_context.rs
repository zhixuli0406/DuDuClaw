//! Gateway-owned live turn context shared by recipient and computer-use gates.
//!
//! Every spawn of an agent CLI that carries a reply channel and a turn id
//! (`DUDUCLAW_REPLY_CHANNEL` / `DUDUCLAW_TURN_ID`) registers
//! `(agent id, turn id) → reply channel` here for as long as that CLI runs;
//! the [`TurnGuard`] removes it when the turn ends, however it ends. The MCP
//! client sends only the opaque turn id it finds in its environment; the
//! route looks it up under the *verified* employee id. No match ⇒ no
//! confirmer ⇒ high-risk actions are refused. An employee can therefore only
//! ever reach a chat that has a live turn of that same employee.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Longest accepted turn id.
pub const MAX_TURN_ID_LEN: usize = 128;
/// Longest accepted reply channel (`<channel>:<chat>`).
const MAX_REPLY_CHANNEL_LEN: usize = 512;

type Key = (String, String);

/// `(agent, turn) → [(registration id, reply channel)]`, newest last. Leaf
/// lock, never held across an `.await`.
fn registry() -> &'static Mutex<
    HashMap<
        Key,
        Vec<(
            u64,
            String,
            Option<crate::approval::DecisionContext>,
            Option<crate::approval::TrustedReplyTarget>,
            Option<crate::computer_use_sessions::attended::AttendedInputProof>,
        )>,
    >,
> {
    static TURNS: OnceLock<
        Mutex<
            HashMap<
                Key,
                Vec<(
                    u64,
                    String,
                    Option<crate::approval::DecisionContext>,
                    Option<crate::approval::TrustedReplyTarget>,
                    Option<crate::computer_use_sessions::attended::AttendedInputProof>,
                )>,
            >,
        >,
    > = OnceLock::new();
    TURNS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `turn_id` has an acceptable shape (non-empty, bounded, no control
/// characters). Anything else never matches a registration.
pub fn valid_turn_id(turn_id: &str) -> bool {
    !turn_id.is_empty()
        && turn_id.len() <= MAX_TURN_ID_LEN
        && !turn_id.chars().any(char::is_control)
}

/// Removes its registration when dropped.
#[derive(Debug)]
pub struct TurnGuard {
    key: Key,
    id: u64,
    _watch_socket: Option<crate::computer_watch_links::delivery::SocketGuard>,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        let mut turns = registry().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(list) = turns.get_mut(&self.key) {
            list.retain(|(id, ..)| *id != self.id);
            if list.is_empty() {
                turns.remove(&self.key);
            }
        }
    }
}

/// Record that `agent_id`'s turn `turn_id` answers in `reply_channel` until
/// the returned guard is dropped. `None` (nothing recorded) when any part is
/// empty or malformed.
pub fn register(agent_id: &str, turn_id: &str, reply_channel: &str) -> Option<TurnGuard> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    if !duduclaw_core::is_valid_agent_id(agent_id)
        || !valid_turn_id(turn_id)
        || reply_channel.is_empty()
        || reply_channel.len() > MAX_REPLY_CHANNEL_LEN
        || reply_channel.chars().any(char::is_control)
    {
        return None;
    }
    let key = (agent_id.to_string(), turn_id.to_string());
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(key.clone())
        .or_default()
        .push((
            id,
            reply_channel.to_string(),
            crate::approval::CURRENT_DECISION_CONTEXT
                .try_with(Clone::clone)
                .ok()
                .flatten(),
            crate::approval::CURRENT_TRUSTED_REPLY_TARGET
                .try_with(Clone::clone)
                .ok()
                .flatten(),
            crate::computer_use_sessions::attended::capture(agent_id, turn_id),
        ));
    Some(TurnGuard { key, id,
        _watch_socket: crate::computer_watch_links::delivery::register_socket(agent_id, turn_id),
    })
}

/// The reply channel of `agent_id`'s live turn `turn_id` (the newest
/// registration when several spawns of one turn overlap). Exact match on
/// both parts.
pub fn reply_channel_for(agent_id: &str, turn_id: &str) -> Option<String> {
    if !valid_turn_id(turn_id) {
        return None;
    }
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(agent_id.to_string(), turn_id.to_string()))
        .and_then(|list| list.last())
        .map(|(_, channel, ..)| channel.clone())
}

/// Register the turn the current task is answering, for a CLI spawn of
/// `agent_id`: the reply channel is the `REPLY_CHANNEL` task-local (else the
/// session id, which the MCP client used to fall back to) and the turn id is
/// the `CURRENT_TURN_ID` task-local — the same values the spawn puts into
/// `DUDUCLAW_REPLY_CHANNEL` / `DUDUCLAW_TURN_ID`. `None` when either is
/// missing.
pub fn register_current_turn(agent_id: &str) -> Option<TurnGuard> {
    let turn_id = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(|t| t.clone())
        .ok()
        .flatten()?;
    let channel = crate::claude_runner::REPLY_CHANNEL
        .try_with(|c| c.clone())
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| {
            duduclaw_memory::feedback::CURRENT_SESSION_ID
                .try_with(|s| s.clone())
                .ok()
                .flatten()
        })?;
    register(agent_id, &turn_id, &channel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrations_are_keyed_by_agent_and_turn_and_end_with_the_guard() {
        let guard = register("turns-alice", "turn-1", "telegram:42").unwrap();
        assert_eq!(
            reply_channel_for("turns-alice", "turn-1").as_deref(),
            Some("telegram:42")
        );
        // Another employee with the same turn id, or another turn, sees nothing.
        assert_eq!(reply_channel_for("turns-bob", "turn-1"), None);
        assert_eq!(reply_channel_for("turns-alice", "turn-2"), None);
        assert_eq!(reply_channel_for("turns-alice", "turn-"), None);
        // Overlapping spawns of one turn: the newest wins, each guard removes
        // only its own registration.
        let second = register("turns-alice", "turn-1", "discord:7").unwrap();
        assert_eq!(
            reply_channel_for("turns-alice", "turn-1").as_deref(),
            Some("discord:7")
        );
        drop(second);
        assert_eq!(
            reply_channel_for("turns-alice", "turn-1").as_deref(),
            Some("telegram:42")
        );
        drop(guard);
        assert_eq!(reply_channel_for("turns-alice", "turn-1"), None);
    }

    #[test]
    fn malformed_parts_are_never_registered() {
        assert!(register("../x", "t", "telegram:1").is_none());
        assert!(register("turns-carol", "", "telegram:1").is_none());
        assert!(register("turns-carol", "t\n", "telegram:1").is_none());
        assert!(
            register(
                "turns-carol",
                &"t".repeat(MAX_TURN_ID_LEN + 1),
                "telegram:1"
            )
            .is_none()
        );
        assert!(register("turns-carol", "t", "").is_none());
        assert!(register("turns-carol", "t", "telegram:1\r\n").is_none());
        assert_eq!(reply_channel_for("turns-carol", "t"), None);
    }

    #[tokio::test]
    async fn the_current_turn_comes_from_the_task_locals() {
        let fut = async { register_current_turn("turns-dave") };
        let guard = crate::claude_runner::REPLY_CHANNEL
            .scope(
                "line:abc".to_string(),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("turn-9".to_string()), fut),
            )
            .await
            .unwrap();
        assert_eq!(
            reply_channel_for("turns-dave", "turn-9").as_deref(),
            Some("line:abc")
        );
        drop(guard);
        // Outside any turn scope nothing is registered.
        assert!(register_current_turn("turns-dave").is_none());
    }
}

/// Principal/account/thread data comes from the verified adapter's registration.
pub fn decision_context_for(
    agent_id: &str,
    turn_id: &str,
) -> Option<crate::approval::DecisionContext> {
    if !valid_turn_id(turn_id) {
        return None;
    }
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(agent_id.into(), turn_id.into()))
        .and_then(|v| v.last())
        .and_then(|(_, _, c, _, _)| c.clone())
}

/// Never reconstruct a transport from a chat id or an employee-supplied token.
pub(crate) fn target_for(
    agent_id: &str,
    turn_id: &str,
) -> Option<crate::approval::TrustedReplyTarget> {
    if !valid_turn_id(turn_id) {
        return None;
    }
    registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&(agent_id.into(), turn_id.into()))
        .and_then(|v| v.last())
        .and_then(|(_, _, _, t, _)| t.clone())
}

/// A verified inbound human message is still live for this exact employee and
/// turn. Principal/transport context alone never establishes this proof.
pub(crate) fn attended_live_for(agent_id:&str,turn_id:&str)->bool {
    if !valid_turn_id(turn_id) {return false;}
    registry().lock().unwrap_or_else(|p|p.into_inner())
        .get(&(agent_id.into(),turn_id.into())).and_then(|list|list.last())
        .and_then(|(_,_,_,_,proof)|proof.as_ref())
        .is_some_and(|proof|proof.valid_for(agent_id,turn_id))
}

/// Only a server-registered human channel turn grants a source recipient.
/// Background executions may carry a reply destination but have no source.
pub fn source_conversation_for(agent_id: &str, turn_id: &str) -> Option<String> {
    use duduclaw_knowledge::requester::RequesterIdentity;
    match crate::knowledge::turn_requesters::lookup(agent_id, turn_id)? {
        RequesterIdentity::ChannelUser { .. } | RequesterIdentity::DashboardUser { .. } => {
            reply_channel_for(agent_id, turn_id)
                .and_then(|value| duduclaw_core::recipient_policy::normalize_recipient(&value))
        }
        RequesterIdentity::System { .. } | RequesterIdentity::Unknown => None,
    }
}

#[cfg(test)]
mod recipient_tests {
    use super::*;
    use duduclaw_knowledge::requester::RequesterIdentity;

    #[test]
    fn sources_are_exact_live_server_turns_and_backgrounds_have_none() {
        let _turn = register("source-bot", "source-turn", "telegram:42:topic#agent:source-bot").unwrap();
        assert_eq!(source_conversation_for("source-bot", "source-turn"), None);
        let _requester = crate::knowledge::turn_requesters::register("source-bot", "source-turn",
            RequesterIdentity::ChannelUser {channel: "telegram".into(), chat: "42".into(), sender: "person".into(), user_id: None}).unwrap();
        assert_eq!(source_conversation_for("source-bot", "source-turn").as_deref(), Some("telegram:42"));
        assert_eq!(source_conversation_for("other-bot", "source-turn"), None);
        assert_eq!(source_conversation_for("source-bot", "unknown"), None);
        let _background = crate::knowledge::turn_requesters::register("source-bot", "source-turn",
            RequesterIdentity::System {kind: "cron".into(), on_behalf_of: None}).unwrap();
        assert_eq!(source_conversation_for("source-bot", "source-turn"), None);
    }

    #[test]
    fn forged_reply_environment_cannot_supply_recipient_source() {
        const CHILD: &str = "DUDUCLAW_RECIPIENT_FORGED_ENV_TEST";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "turn_context::recipient_tests::forged_reply_environment_cannot_supply_recipient_source", "--nocapture"])
                .env(CHILD, "1").env(duduclaw_core::ENV_REPLY_CHANNEL, "line:forged").status().unwrap();
            assert!(status.success());
            return;
        }
        assert_eq!(source_conversation_for("forged-bot", "unknown"), None);
        let _turn = register("forged-bot", "real-turn", "line:real").unwrap();
        let _requester = crate::knowledge::turn_requesters::register("forged-bot", "real-turn",
            RequesterIdentity::ChannelUser {channel: "line".into(), chat: "real".into(), sender: "person".into(), user_id: None}).unwrap();
        assert_eq!(source_conversation_for("forged-bot", "real-turn").as_deref(), Some("line:real"));
    }
}
