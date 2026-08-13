//! Cross-process durable store of "which channel message currently carries a
//! pending decision's buttons", keyed by `(namespace, decision_id, channel,
//! chat_id)`.
//!
//! Why this exists: an interaction's own `response_url`/interaction-token
//! (Slack/Discord) is single-use and expires within minutes, and a live
//! callback payload only exists for the exact press that triggered it — none
//! of that survives to let `decision_card::collapse_all` edit the message
//! from a detached background task, or from a press that lands on a
//! different destination than the one that decided first. Recording the
//! `(edit_chat_id, message_id)` pair once, at push time, lets any later
//! decide path find the card again independent of platform interaction
//! lifetimes or process restarts.
//!
//! State lives in `<home>/decision_cards.json`, guarded by
//! [`duduclaw_core::with_file_lock`] (project convention #3: cross-process
//! mutation of a shared file must hold the advisory lock) and written with an
//! atomic temp-file rename, mirroring `duduclaw-core::dispatch_guard`'s
//! pattern for the same class of small durable JSON state.
//!
//! ## Failure posture
//!
//! This is bookkeeping for a cosmetic UI touch-up, not a decision gate. A
//! read/write failure (corrupt file, IO error, lock contention) degrades to
//! "nothing stored" / "write silently skipped" — [`decision_card::collapse_all`]
//! already treats a lookup miss as "append the result instead of editing",
//! so a lost reference only downgrades the UX, never blocks or fails the
//! decision that already landed.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::decision_card::PushedMessage;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CardEntry {
    edit_chat_id: String,
    message_id: String,
    stored_at_ms: i64,
    /// The channel this card was pushed on. `#[serde(default)]` because
    /// entries written before [`list_card_messages`] existed only carried the
    /// destination inside the map key; those are recovered by
    /// [`destination_from_key`] instead.
    #[serde(default)]
    channel: String,
    /// The chat/account id the card was addressed to. Same defaulting note as
    /// [`CardEntry::channel`].
    #[serde(default)]
    chat_id: String,
    /// W2-7: the Discord guild this card's destination channel belonged to,
    /// snapshotted at push time from `discord::guild_id_for_channel` when
    /// `channel == "discord"`. `#[serde(default)]` so every entry written
    /// before this field existed (and every entry for a non-Discord
    /// destination) deserializes to `None` rather than failing — this store's
    /// documented failure posture is "degrade, never error".
    #[serde(default)]
    discord_guild_id: Option<String>,
}

type State = HashMap<String, CardEntry>;

/// Bound the state file's growth: entries older than this are pruned on
/// every write (a card nobody decided within two weeks is not worth editing
/// any more — it will already have auto-denied via the owning store's own
/// TTL and the append-fallback path is fine for anything that slips through).
const STALE_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// Hard cap independent of age, so a burst cannot grow the file unbounded
/// even within the staleness window — oldest entries evicted first.
const MAX_ENTRIES: usize = 5_000;

fn store_path(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("decision_cards.json")
}

fn card_key(namespace: &str, decision_id: &str, channel: &str, chat_id: &str) -> String {
    format!("{namespace}:{decision_id}:{channel}:{chat_id}")
}

fn load_state(path: &Path) -> State {
    // Missing or corrupt state ⇒ empty (fresh). Never propagate an error —
    // see the module-level failure posture note.
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::new(),
    }
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic replace (temp + rename) so a crash mid-write cannot leave a
    // truncated JSON file the next reader would discard.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

fn prune(state: &mut State, now_ms: i64) {
    state.retain(|_, e| now_ms - e.stored_at_ms < STALE_MS);
    if state.len() > MAX_ENTRIES {
        let mut by_age: Vec<(String, i64)> = state.iter().map(|(k, e)| (k.clone(), e.stored_at_ms)).collect();
        by_age.sort_by_key(|(_, t)| *t);
        let excess = state.len() - MAX_ENTRIES;
        for (key, _) in by_age.into_iter().take(excess) {
            state.remove(&key);
        }
    }
}

/// Record the message identity a freshly-pushed decision card landed on.
/// Best-effort: a write failure is logged and swallowed — losing this
/// reference only costs a future append-fallback instead of an in-place
/// edit, never blocks the push that already succeeded.
pub fn record_card_message(
    home_dir: &Path,
    namespace: &str,
    decision_id: &str,
    channel: &str,
    chat_id: &str,
    pushed: &PushedMessage,
) {
    let path = store_path(home_dir);
    let key = card_key(namespace, decision_id, channel, chat_id);
    let now_ms = chrono::Utc::now().timestamp_millis();
    // W2-7: snapshot the Discord guild id at push time (same rationale as
    // `TaskRow::source_discord_guild_id`) — `None` for every other platform
    // and for a Discord channel this gateway hasn't seen a message from yet.
    let discord_guild_id =
        (channel == "discord").then(|| crate::discord::guild_id_for_channel(home_dir, chat_id)).flatten();
    let entry = CardEntry {
        edit_chat_id: pushed.edit_chat_id.clone(),
        message_id: pushed.message_id.clone(),
        stored_at_ms: now_ms,
        channel: channel.to_string(),
        chat_id: chat_id.to_string(),
        discord_guild_id,
    };
    let result = duduclaw_core::with_file_lock(&path, || {
        let mut state = load_state(&path);
        prune(&mut state, now_ms);
        state.insert(key.clone(), entry.clone());
        save_state(&path, &state)
    });
    if let Err(e) = result {
        tracing::debug!(%namespace, %decision_id, %channel, error = %e, "decision-message-store: write failed (non-fatal)");
    }
}

/// WP1.6 (ecosystem, text-reply decisions): reverse lookup — which decision
/// card does a channel message belong to? Linear scan is fine: the store is
/// pruned to `MAX_ENTRIES` and consulted only for messages that REPLY to a
/// bot message. The key layout is `namespace:decision_id:channel:chat_id`;
/// the entry's own channel/chat_id fields anchor the suffix strip so chat ids
/// containing `:` (Teams) cannot skew the parse. Returns `(namespace,
/// decision_id)`.
pub fn lookup_decision_by_message(
    home_dir: &Path,
    channel: &str,
    chat_id: &str,
    message_id: &str,
) -> Option<(String, String)> {
    if message_id.is_empty() {
        return None;
    }
    let state = load_state(&store_path(home_dir));
    for (key, e) in state.iter() {
        if e.channel != channel || e.message_id != message_id {
            continue;
        }
        // Inbound events carry the PLATFORM channel id, which for Discord DM
        // cards is `edit_chat_id` (the bot↔user DM channel) while the store
        // key was built from the original destination (`chat_id` = user id).
        // Accept either — the message id already pins the exact card.
        if e.chat_id != chat_id && e.edit_chat_id != chat_id {
            continue;
        }
        let suffix = format!(":{channel}:{}", e.chat_id);
        let Some(head) = key.strip_suffix(suffix.as_str()) else {
            continue;
        };
        // Decision ids carry no `:` (uuid/token forms), so the first split is
        // exactly the namespace boundary.
        if let Some((namespace, decision_id)) = head.split_once(':') {
            return Some((namespace.to_string(), decision_id.to_string()));
        }
    }
    None
}

/// Look up the stored message identity for a decision card, if any.
pub fn lookup_card_message(
    home_dir: &Path,
    namespace: &str,
    decision_id: &str,
    channel: &str,
    chat_id: &str,
) -> Option<PushedMessage> {
    let path = store_path(home_dir);
    let key = card_key(namespace, decision_id, channel, chat_id);
    let state = load_state(&path);
    state.get(&key).map(|e| PushedMessage {
        edit_chat_id: e.edit_chat_id.clone(),
        message_id: e.message_id.clone(),
    })
}

/// W2-7: the Discord guild id snapshotted for a decision card's destination,
/// when its channel was `"discord"` and the guild was known at push time.
/// `None` for every other platform, for a lookup miss, or for a Discord
/// card pushed before this field existed — never fabricated.
pub fn lookup_card_discord_guild_id(
    home_dir: &Path,
    namespace: &str,
    decision_id: &str,
    channel: &str,
    chat_id: &str,
) -> Option<String> {
    let path = store_path(home_dir);
    let key = card_key(namespace, decision_id, channel, chat_id);
    load_state(&path).get(&key).and_then(|e| e.discord_guild_id.clone())
}

/// One card of a decision, as delivered to a specific destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLocation {
    pub channel: String,
    pub chat_id: String,
    pub pushed: PushedMessage,
}

/// Recover `(channel, chat_id)` from a map key written before [`CardEntry`]
/// carried them explicitly. The key is `{namespace}:{decision_id}:{channel}:{chat_id}`
/// and a channel name never contains `:`, so stripping the known
/// `{namespace}:{decision_id}:` prefix and splitting once is exact.
fn destination_from_key(key: &str, namespace: &str, decision_id: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix(&format!("{namespace}:{decision_id}:"))?;
    let (channel, chat_id) = rest.split_once(':')?;
    if channel.is_empty() || chat_id.is_empty() {
        return None;
    }
    Some((channel.to_string(), chat_id.to_string()))
}

/// Every destination a decision's card was pushed to.
///
/// A decision can land on many destinations at once — an install sign-off
/// fans out to every eligible approver's every linked channel, and a broker
/// approval with no origin conversation fans out to all approvers. Settling
/// it must retire *all* of those cards, not only the one the presser was
/// looking at, so this returns the full set rather than a single lookup.
///
/// Order is unspecified (backed by a `HashMap`); callers treat the results as
/// a set. A read failure or corrupt state degrades to an empty list, matching
/// the module's failure posture.
pub fn list_card_messages(home_dir: &Path, namespace: &str, decision_id: &str) -> Vec<CardLocation> {
    let state = load_state(&store_path(home_dir));
    let prefix = format!("{namespace}:{decision_id}:");
    state
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .filter_map(|(k, e)| {
            let (channel, chat_id) = if e.channel.is_empty() || e.chat_id.is_empty() {
                destination_from_key(k, namespace, decision_id)?
            } else {
                (e.channel.clone(), e.chat_id.clone())
            };
            // The prefix filter above is necessarily loose (a decision id
            // containing `:` could make one decision's prefix match another's
            // key). Recomposing the key and requiring exact equality makes
            // the match precise, so a settle never edits a different
            // decision's card.
            if card_key(namespace, decision_id, &channel, &chat_id) != *k {
                return None;
            }
            Some(CardLocation {
                channel,
                chat_id,
                pushed: PushedMessage {
                    edit_chat_id: e.edit_chat_id.clone(),
                    message_id: e.message_id.clone(),
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let pushed = PushedMessage { edit_chat_id: "chat-1".into(), message_id: "mid-42".into() };
        record_card_message(dir.path(), "approval", "ap-1", "telegram", "555", &pushed);

        let got = lookup_card_message(dir.path(), "approval", "ap-1", "telegram", "555");
        assert_eq!(got, Some(pushed));
    }

    #[test]
    fn lookup_miss_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(lookup_card_message(dir.path(), "approval", "does-not-exist", "telegram", "555"), None);
    }

    // ── W2-7: Discord guild id snapshot on push ──────────────

    #[test]
    fn discord_card_snapshots_guild_id_recorded_by_discord_rs() {
        let dir = tempfile::tempdir().unwrap();
        // Simulates discord.rs having already seen a message from this
        // channel before the approval card was pushed to it.
        crate::discord::record_channel_guild(dir.path(), "chan-1", "guild-1");
        let pushed = PushedMessage { edit_chat_id: "chan-1".into(), message_id: "m1".into() };
        record_card_message(dir.path(), "approval", "ap-1", "discord", "chan-1", &pushed);

        assert_eq!(
            lookup_card_discord_guild_id(dir.path(), "approval", "ap-1", "discord", "chan-1"),
            Some("guild-1".to_string())
        );
    }

    #[test]
    fn discord_card_unknown_channel_has_no_guild_id() {
        let dir = tempfile::tempdir().unwrap();
        let pushed = PushedMessage { edit_chat_id: "chan-9".into(), message_id: "m1".into() };
        record_card_message(dir.path(), "approval", "ap-2", "discord", "chan-9", &pushed);
        assert_eq!(
            lookup_card_discord_guild_id(dir.path(), "approval", "ap-2", "discord", "chan-9"),
            None
        );
    }

    #[test]
    fn non_discord_card_never_carries_a_guild_id() {
        let dir = tempfile::tempdir().unwrap();
        // Even if (hypothetically) some data existed under the same chat_id
        // for Discord, a Telegram-channel card must never pick it up.
        crate::discord::record_channel_guild(dir.path(), "555", "guild-x");
        let pushed = PushedMessage { edit_chat_id: "555".into(), message_id: "m1".into() };
        record_card_message(dir.path(), "approval", "ap-3", "telegram", "555", &pushed);
        assert_eq!(
            lookup_card_discord_guild_id(dir.path(), "approval", "ap-3", "telegram", "555"),
            None
        );
    }

    #[test]
    fn entries_are_namespaced_and_scoped_by_channel_and_chat() {
        let dir = tempfile::tempdir().unwrap();
        let a = PushedMessage { edit_chat_id: "c1".into(), message_id: "m1".into() };
        let b = PushedMessage { edit_chat_id: "c2".into(), message_id: "m2".into() };
        // Same decision_id, different channel ⇒ independent entries (a
        // multi-destination fan-out, e.g. approval_notify pushing the same
        // approval to two admins on two different platforms).
        record_card_message(dir.path(), "approval", "ap-1", "telegram", "555", &a);
        record_card_message(dir.path(), "approval", "ap-1", "slack", "C1", &b);
        assert_eq!(lookup_card_message(dir.path(), "approval", "ap-1", "telegram", "555"), Some(a));
        assert_eq!(lookup_card_message(dir.path(), "approval", "ap-1", "slack", "C1"), Some(b));
        // Different namespace, same decision_id string ⇒ must not collide
        // (a task id and an approval id could theoretically coincide).
        assert_eq!(lookup_card_message(dir.path(), "goal_task", "ap-1", "telegram", "555"), None);
    }

    #[test]
    fn corrupt_state_file_is_treated_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("decision_cards.json"), b"{not json").unwrap();
        assert_eq!(lookup_card_message(dir.path(), "approval", "x", "telegram", "1"), None);
        // A write after a corrupt read must still succeed (treated as fresh).
        let pushed = PushedMessage { edit_chat_id: "c".into(), message_id: "m".into() };
        record_card_message(dir.path(), "approval", "x", "telegram", "1", &pushed);
        assert_eq!(lookup_card_message(dir.path(), "approval", "x", "telegram", "1"), Some(pushed));
    }

    #[test]
    fn list_returns_every_destination_a_decision_was_pushed_to() {
        let dir = tempfile::tempdir().unwrap();
        let a = PushedMessage { edit_chat_id: "c1".into(), message_id: "m1".into() };
        let b = PushedMessage { edit_chat_id: "c2".into(), message_id: "m2".into() };
        let c = PushedMessage { edit_chat_id: "c3".into(), message_id: "m3".into() };
        // Same install request fanned out to two approvers on two platforms…
        record_card_message(dir.path(), "install", "r-1", "telegram", "555", &a);
        record_card_message(dir.path(), "install", "r-1", "slack", "U9", &b);
        // …plus an unrelated decision that must not appear.
        record_card_message(dir.path(), "install", "r-2", "telegram", "777", &c);

        let mut got = list_card_messages(dir.path(), "install", "r-1");
        got.sort_by(|x, y| x.channel.cmp(&y.channel));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].channel, "slack");
        assert_eq!(got[0].chat_id, "U9");
        assert_eq!(got[0].pushed, b);
        assert_eq!(got[1].channel, "telegram");
        assert_eq!(got[1].pushed, a);
    }

    #[test]
    fn list_is_empty_for_an_unknown_decision_and_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_card_messages(dir.path(), "install", "nope").is_empty());
        record_card_message(
            dir.path(),
            "install",
            "r-1",
            "telegram",
            "1",
            &PushedMessage { edit_chat_id: "1".into(), message_id: "m".into() },
        );
        assert!(list_card_messages(dir.path(), "install", "nope").is_empty());
        assert!(list_card_messages(dir.path(), "approval", "r-1").is_empty());
    }

    #[test]
    fn list_recovers_destinations_from_legacy_entries_without_the_fields() {
        // An entry written before the channel/chat_id fields existed: the
        // destination lives only in the map key.
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(dir.path());
        std::fs::write(
            &path,
            br#"{"approval:ap-1:telegram:555":{"edit_chat_id":"555","message_id":"m7","stored_at_ms":9}}"#,
        )
        .unwrap();
        let got = list_card_messages(dir.path(), "approval", "ap-1");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].channel, "telegram");
        assert_eq!(got[0].chat_id, "555");
        assert_eq!(got[0].pushed.message_id, "m7");
        // …and the single-destination lookup still works on it too.
        assert!(lookup_card_message(dir.path(), "approval", "ap-1", "telegram", "555").is_some());
    }

    #[test]
    fn list_never_returns_a_different_decisions_card() {
        // A decision id containing `:` makes the prefix filter ambiguous;
        // the exact key recomposition must reject the impostor.
        let dir = tempfile::tempdir().unwrap();
        record_card_message(
            dir.path(),
            "approval",
            "a:telegram",
            "slack",
            "U1",
            &PushedMessage { edit_chat_id: "U1".into(), message_id: "m1".into() },
        );
        assert!(list_card_messages(dir.path(), "approval", "a").is_empty());
        assert_eq!(list_card_messages(dir.path(), "approval", "a:telegram").len(), 1);
    }

    #[test]
    fn stale_entries_are_pruned_on_next_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = store_path(dir.path());
        let mut state = State::new();
        state.insert(
            "approval:old:telegram:1".to_string(),
            CardEntry {
                edit_chat_id: "1".into(),
                message_id: "m".into(),
                stored_at_ms: 0,
                channel: "telegram".into(),
                chat_id: "1".into(),
                discord_guild_id: None,
            },
        );
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let pushed = PushedMessage { edit_chat_id: "2".into(), message_id: "n".into() };
        record_card_message(dir.path(), "approval", "fresh", "telegram", "2", &pushed);

        let after: State = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!after.contains_key("approval:old:telegram:1"), "stale entry must be pruned");
        assert!(after.contains_key("approval:fresh:telegram:2"));
    }
}
