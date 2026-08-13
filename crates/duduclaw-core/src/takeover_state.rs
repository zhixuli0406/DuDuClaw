//! Per-conversation human-takeover state (W3-1, analogous-product patterns
//! D1–D5 / D10).
//!
//! ## What this is
//!
//! When a verified manager speaks in a channel conversation, the AI stops
//! answering *that one conversation* for a bounded window. This module owns
//! the durable half of that: **who** took over, **which** conversation, and
//! **until when**. Everything policy-shaped (who counts as a manager, what
//! the announcement says, which dispatch paths must consult it) lives in the
//! gateway's `takeover` module — this file is deliberately free of identity
//! and channel knowledge so both the gateway and `duduclaw-agent`'s heartbeat
//! can read it.
//!
//! ## Why a file and not the task store
//!
//! The pause is a property of a *conversation*, not of a task: a conversation
//! may have zero or many tasks attached, and the pause has to be readable from
//! two crates (the gateway's reply/goal/cron/dispatch paths and the agent
//! crate's heartbeat scheduler) without either taking a dependency on the
//! other. `<home>/takeover_state.json` under [`crate::with_file_lock`] is the
//! same shape [`crate::dispatch_guard`] already uses for exactly this reason
//! (project convention #3: cross-process mutation of a shared file holds the
//! advisory lock).
//!
//! ## Scope discipline (LINE's fourth guarantee)
//!
//! LINE's "暫時手動聊天" is trusted by operators because it is explicitly
//! documented as *not* touching the account's global settings. This module
//! keeps that property structurally: there is exactly one mutable thing here —
//! a map keyed by conversation — and no code path writes `config.toml`,
//! `agent.toml`, or any per-agent state. A takeover cannot outlive its
//! `until`, and the state file is empty whenever nobody has taken anything
//! over.
//!
//! ## Failure posture
//!
//! - **Reads** (`active_at`, `is_*_paused`) treat a missing / unreadable /
//!   corrupt file as "no takeover". That is the honest reading: the file only
//!   exists while somebody holds a conversation, so absent ⇒ nobody does. A
//!   read that could not distinguish the two would have to freeze every
//!   conversation on the box the first time the disk hiccups.
//! - **Writes** (`begin`, `extend`, `end`) propagate their error. A caller
//!   must never announce "I've taken over" for a pause that did not persist —
//!   that is the one place where guessing produces the ManyChat failure this
//!   whole feature exists to avoid.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// State file name under `<home>`.
pub const STATE_FILE: &str = "takeover_state.json";

/// Default pause length when a manager speaks (LINE OA uses 1 hour; ManyChat
/// uses 30 minutes — we take the longer, more forgiving default because a
/// human who is mid-conversation being interrupted by the AI is the failure
/// we are buying insurance against).
pub const DEFAULT_DURATION_MINUTES: i64 = 60;

/// Hard ceiling for any single takeover window, whatever the operator config
/// says. A pause is a *temporary* mode; "forever" is a different feature
/// (disable the agent) and must not be reachable by typo.
pub const HARD_MAX_DURATION_MINUTES: i64 = 12 * 60;

/// Largest single `/takeover +<n>m` extension accepted.
pub const MAX_EXTENSION_MINUTES: i64 = 8 * 60;

// ── Config ──────────────────────────────────────────────────────

/// `config.toml [takeover]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct TakeoverConfig {
    /// Master switch for the automatic "a manager spoke ⇒ take over" path.
    /// **Opt-in (default `false`).** On-by-default was a false-trigger for the
    /// most common shape of conversation — an owner/admin talking to *their own*
    /// AI (every Personal-edition conversation, and any admin using the AI as a
    /// personal assistant): each message they sent silenced the AI as if they
    /// were stepping into a conversation it was handling for someone else. The
    /// seamless-takeover feature is meaningful only for teams whose AI answers
    /// *others* (customer/support channels), so it is now opt-in. The explicit
    /// `/takeover` command is unaffected by this switch.
    pub enabled: bool,
    /// Window applied when a manager speaks, and when `/takeover` is used
    /// without an explicit amount.
    pub duration_minutes: i64,
    /// Ceiling a takeover may be extended to (still clamped by
    /// [`HARD_MAX_DURATION_MINUTES`]).
    pub max_duration_minutes: i64,
}

impl Default for TakeoverConfig {
    fn default() -> Self {
        Self {
            // Opt-in: auto-takeover-on-type stays off until a team explicitly
            // turns it on (see the `enabled` field doc for why).
            enabled: false,
            duration_minutes: DEFAULT_DURATION_MINUTES,
            max_duration_minutes: HARD_MAX_DURATION_MINUTES,
        }
    }
}

impl TakeoverConfig {
    /// Load `[takeover]` from `<home>/config.toml`. Parsed in isolation from a
    /// generic `toml::Table` so unrelated malformed config elsewhere can never
    /// make this fail; absent / malformed section ⇒ defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        match table.get("takeover") {
            Some(section) => section
                .clone()
                .try_into::<TakeoverConfig>()
                .unwrap_or_default()
                .clamped(),
            None => Self::default(),
        }
    }

    /// Bring operator-supplied numbers into range. A nonsensical value (0,
    /// negative, absurdly large) becomes the nearest sane one rather than
    /// disabling the feature — an unreadable number in a config file must not
    /// silently turn the pause into "never" or "forever".
    pub fn clamped(mut self) -> Self {
        self.max_duration_minutes = self
            .max_duration_minutes
            .clamp(1, HARD_MAX_DURATION_MINUTES);
        self.duration_minutes = self.duration_minutes.clamp(1, self.max_duration_minutes);
        self
    }
}

// ── Record ──────────────────────────────────────────────────────

/// One conversation currently held by a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeoverRecord {
    /// Conversation key — the channel session id (`<channel>:<chat>[:<thread>]`).
    pub conversation: String,
    /// Transport (`telegram`, `slack`, …), derived from `conversation`.
    pub channel: String,
    /// Addressable chat id on that transport, derived from `conversation`.
    pub chat_id: String,
    /// The AI employee whose conversation this is (Activity Feed attribution).
    pub agent_id: String,
    /// Channel account id of the human holding the conversation.
    pub holder_user_id: String,
    /// Display name to show end users. Never an internal id.
    pub holder_display: String,
    pub started_at: DateTime<Utc>,
    pub until: DateTime<Utc>,
    /// Tasks stamped `claimed_by` when this takeover began (D4).
    #[serde(default)]
    pub claimed_task_ids: Vec<String>,
}

impl TakeoverRecord {
    /// Still holding at `now`.
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        now < self.until
    }

    /// Whole minutes left, floored at 0.
    pub fn minutes_left(&self, now: DateTime<Utc>) -> i64 {
        (self.until - now).num_minutes().max(0)
    }
}

/// What [`begin`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginOutcome {
    /// The conversation was not held; it is now. Callers announce this (D10).
    Started(TakeoverRecord),
    /// The conversation was already held and the window was pushed out. Silent
    /// by design — re-announcing on every message the human types is exactly
    /// the noise ManyChat's pause icon avoids.
    Refreshed(TakeoverRecord),
}

impl BeginOutcome {
    pub fn record(&self) -> &TakeoverRecord {
        match self {
            BeginOutcome::Started(r) | BeginOutcome::Refreshed(r) => r,
        }
    }
    pub fn is_started(&self) -> bool {
        matches!(self, BeginOutcome::Started(_))
    }
}

/// Everything [`begin`] needs that is not derivable from the conversation key.
#[derive(Debug, Clone)]
pub struct BeginRequest {
    pub conversation: String,
    pub agent_id: String,
    pub holder_user_id: String,
    pub holder_display: String,
}

// ── Conversation key → push target ──────────────────────────────

/// Split a conversation key into `(channel, chat_id)`.
///
/// Grammar matches `dispatcher::parse_reply_channel` /
/// `decision_notify::parse_origin`: `<type>:<id>[:<thread>]`, with the
/// `<type>:thread:<id>` marker collapsing to `<id>`. Returns `None` for a key
/// with no transport prefix (`"default"`, `""`) — such a conversation has no
/// addressable target and is therefore never gated by takeover.
pub fn conversation_target(conversation: &str) -> Option<(String, String)> {
    let key = conversation.trim();
    let parts: Vec<&str> = key.splitn(3, ':').collect();
    if parts.len() < 2 {
        return None;
    }
    let channel = parts[0].trim();
    if channel.is_empty() {
        return None;
    }
    let chat_id = if parts.len() == 3 && parts[1] == "thread" {
        parts[2]
    } else {
        parts[1]
    };
    let chat_id = chat_id.trim();
    if chat_id.is_empty() {
        return None;
    }
    Some((channel.to_string(), chat_id.to_string()))
}

// ── Storage ─────────────────────────────────────────────────────

type State = HashMap<String, TakeoverRecord>;

/// Absolute path of the state file for `home_dir`.
pub fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join(STATE_FILE)
}

fn load_state(path: &Path) -> State {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => State::new(),
    }
}

fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // Atomic replace so a crash mid-write cannot leave a truncated file that
    // the next reader would discard — discarding it would silently un-pause
    // every held conversation.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}

/// Drop records whose window has closed. Called on every write so the file
/// stays bounded and expiry is not merely a read-time filter.
fn prune(state: &mut State, now: DateTime<Utc>) -> Vec<TakeoverRecord> {
    let expired: Vec<TakeoverRecord> = state
        .values()
        .filter(|r| !r.is_active_at(now))
        .cloned()
        .collect();
    state.retain(|_, r| r.is_active_at(now));
    expired
}

// ── Reads ───────────────────────────────────────────────────────

/// The record holding `conversation` at `now`, if any.
pub fn active_at(
    home_dir: &Path,
    conversation: &str,
    now: DateTime<Utc>,
) -> Option<TakeoverRecord> {
    let state = load_state(&state_path(home_dir));
    state
        .get(conversation)
        .filter(|r| r.is_active_at(now))
        .cloned()
}

/// [`active_at`] with `now = Utc::now()`.
pub fn active(home_dir: &Path, conversation: &str) -> Option<TakeoverRecord> {
    active_at(home_dir, conversation, Utc::now())
}

/// True when this exact conversation is held.
pub fn is_conversation_paused(home_dir: &Path, conversation: &str) -> bool {
    active(home_dir, conversation).is_some()
}

/// True when **any** held conversation addresses `(channel, chat_id)`.
///
/// This is the predicate every non-inbound dispatch path uses, because those
/// paths know a push destination (a goal task's `source_channel`/
/// `source_chat_id`, a cron `notify_channel`/`notify_chat_id`, a delegation
/// `reply_channel`) rather than a session id. Matching on the destination
/// deliberately over-covers a thread-scoped takeover to the parent chat: the
/// safe direction for D5 is silence, and a person who took over a thread being
/// spared one unrelated push in the same chat is a far smaller harm than the
/// AI talking over them.
pub fn is_target_paused(home_dir: &Path, channel: &str, chat_id: &str) -> bool {
    if channel.trim().is_empty() || chat_id.trim().is_empty() {
        return false;
    }
    let now = Utc::now();
    load_state(&state_path(home_dir)).values().any(|r| {
        r.is_active_at(now)
            && r.channel == channel.trim()
            && (r.chat_id == chat_id.trim() || r.conversation == chat_id.trim())
    })
}

/// The record covering `(channel, chat_id)`, for callers that need `until`
/// (e.g. to defer a notification to exactly when the human hands back).
pub fn target_record(home_dir: &Path, channel: &str, chat_id: &str) -> Option<TakeoverRecord> {
    if channel.trim().is_empty() || chat_id.trim().is_empty() {
        return None;
    }
    let now = Utc::now();
    load_state(&state_path(home_dir))
        .values()
        .filter(|r| {
            r.is_active_at(now)
                && r.channel == channel.trim()
                && (r.chat_id == chat_id.trim() || r.conversation == chat_id.trim())
        })
        // Deterministic pick when a thread and its parent chat are both held:
        // the one that hands back last, so a deferred notice never lands while
        // the other is still in progress.
        .max_by_key(|r| r.until)
        .cloned()
}

/// Every conversation currently held (dashboard read model).
pub fn list_active(home_dir: &Path) -> Vec<TakeoverRecord> {
    let now = Utc::now();
    let mut out: Vec<TakeoverRecord> = load_state(&state_path(home_dir))
        .into_values()
        .filter(|r| r.is_active_at(now))
        .collect();
    out.sort_by(|a, b| a.conversation.cmp(&b.conversation));
    out
}

// ── Writes ──────────────────────────────────────────────────────

/// Start (or refresh) a takeover on `req.conversation`.
///
/// Refresh semantics are ManyChat's: a human who keeps typing keeps the
/// window open, without a second announcement. The refresh always re-bases on
/// `now` rather than adding to the remaining time, so a long conversation
/// cannot creep past `max_duration_minutes`.
pub fn begin(
    home_dir: &Path,
    req: &BeginRequest,
    cfg: &TakeoverConfig,
    now: DateTime<Utc>,
) -> std::io::Result<BeginOutcome> {
    let Some((channel, chat_id)) = conversation_target(&req.conversation) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "takeover: conversation '{}' has no addressable target",
                req.conversation
            ),
        ));
    };
    let cfg = cfg.clamped();
    let path = state_path(home_dir);
    with_state(&path, |state| {
        prune(state, now);
        let until = now + Duration::minutes(cfg.duration_minutes);
        match state.get_mut(&req.conversation) {
            Some(existing) => {
                existing.until = until;
                // A second manager stepping in takes the conversation over
                // from the first — the last person to speak is the one the
                // customer is talking to (Intercom's "control transfer is the
                // message" rule).
                existing.holder_user_id = req.holder_user_id.clone();
                existing.holder_display = req.holder_display.clone();
                Ok(BeginOutcome::Refreshed(existing.clone()))
            }
            None => {
                let rec = TakeoverRecord {
                    conversation: req.conversation.clone(),
                    channel,
                    chat_id,
                    agent_id: req.agent_id.clone(),
                    holder_user_id: req.holder_user_id.clone(),
                    holder_display: req.holder_display.clone(),
                    started_at: now,
                    until,
                    claimed_task_ids: Vec::new(),
                };
                state.insert(req.conversation.clone(), rec.clone());
                Ok(BeginOutcome::Started(rec))
            }
        }
    })
}

/// Attach the task ids claimed for this takeover (D4 step 2). Separate from
/// [`begin`] because the claim itself talks to the task store, which this
/// crate deliberately knows nothing about.
pub fn record_claimed_tasks(
    home_dir: &Path,
    conversation: &str,
    task_ids: &[String],
) -> std::io::Result<()> {
    if task_ids.is_empty() {
        return Ok(());
    }
    let path = state_path(home_dir);
    with_state(&path, |state| {
        if let Some(rec) = state.get_mut(conversation) {
            for id in task_ids {
                if !rec.claimed_task_ids.contains(id) {
                    rec.claimed_task_ids.push(id.clone());
                }
            }
        }
        Ok(())
    })
}

/// Push a held conversation's deadline out by `minutes`.
///
/// Returns `Ok(None)` when the conversation is not held — extending something
/// nobody has taken over is a no-op, not an implicit start: `/takeover +30m`
/// from someone who never took over must not become a takeover.
pub fn extend(
    home_dir: &Path,
    conversation: &str,
    minutes: i64,
    cfg: &TakeoverConfig,
    now: DateTime<Utc>,
) -> std::io::Result<Option<TakeoverRecord>> {
    let cfg = cfg.clamped();
    let minutes = minutes.clamp(1, MAX_EXTENSION_MINUTES);
    let path = state_path(home_dir);
    with_state(&path, |state| {
        prune(state, now);
        let Some(rec) = state.get_mut(conversation) else {
            return Ok(None);
        };
        // Ceiling is measured from `now`, not from `started_at`: the point of
        // the cap is "the AI is never mute for more than this long without
        // somebody re-asserting", and every extension is such an assertion.
        let ceiling = now + Duration::minutes(cfg.max_duration_minutes);
        let proposed = rec.until + Duration::minutes(minutes);
        rec.until = proposed.min(ceiling);
        Ok(Some(rec.clone()))
    })
}

/// End a takeover early. Returns the record that was holding, or `None` when
/// nothing was.
pub fn end(
    home_dir: &Path,
    conversation: &str,
    now: DateTime<Utc>,
) -> std::io::Result<Option<TakeoverRecord>> {
    let path = state_path(home_dir);
    with_state(&path, |state| {
        prune(state, now);
        Ok(state.remove(conversation))
    })
}

/// Remove every window that has closed and return them, so the caller can tell
/// each conversation the AI is back (D10's second half).
///
/// Read-modify-write under the lock, so two sweepers cannot both claim the
/// same expiry and announce twice.
pub fn sweep_expired(home_dir: &Path, now: DateTime<Utc>) -> Vec<TakeoverRecord> {
    let path = state_path(home_dir);
    if !path.exists() {
        return Vec::new();
    }
    with_state(&path, |state| Ok(prune(state, now))).unwrap_or_default()
}

/// Locked read-modify-write. Persists only when the closure succeeded and the
/// state actually changed, so read-shaped callers do not rewrite the file.
fn with_state<T>(
    path: &Path,
    f: impl FnOnce(&mut State) -> std::io::Result<T>,
) -> std::io::Result<T> {
    crate::with_file_lock(path, || {
        let mut state = load_state(path);
        let before = state.clone();
        let out = f(&mut state)?;
        if state != before {
            save_state(path, &state)?;
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(conv: &str) -> BeginRequest {
        BeginRequest {
            conversation: conv.to_string(),
            agent_id: "alice".into(),
            holder_user_id: "555".into(),
            holder_display: "王小明".into(),
        }
    }

    fn cfg() -> TakeoverConfig {
        // These tests exercise the takeover *behaviour*, so opt in explicitly
        // now that the default is off.
        TakeoverConfig { enabled: true, ..TakeoverConfig::default() }
    }

    #[test]
    fn default_is_opt_in_off() {
        // Regression guard: the automatic takeover path must stay off by
        // default so an owner/admin talking to their own AI is never silenced.
        assert!(!TakeoverConfig::default().enabled);
    }

    #[test]
    fn conversation_target_parses_the_three_shapes() {
        assert_eq!(
            conversation_target("telegram:12345"),
            Some(("telegram".into(), "12345".into()))
        );
        assert_eq!(
            conversation_target("telegram:12345:678"),
            Some(("telegram".into(), "12345".into()))
        );
        assert_eq!(
            conversation_target("discord:thread:999"),
            Some(("discord".into(), "999".into()))
        );
        // No transport prefix ⇒ not gateable.
        assert_eq!(conversation_target("default"), None);
        assert_eq!(conversation_target(""), None);
        assert_eq!(conversation_target("telegram:"), None);
    }

    #[test]
    fn begin_then_active_then_expire() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let out = begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        assert!(out.is_started());
        assert!(active_at(dir.path(), "telegram:1", now).is_some());
        // One minute before the window closes: still held.
        let almost = now + Duration::minutes(DEFAULT_DURATION_MINUTES - 1);
        assert!(active_at(dir.path(), "telegram:1", almost).is_some());
        // After: gone, without anybody having to sweep first.
        let after = now + Duration::minutes(DEFAULT_DURATION_MINUTES + 1);
        assert!(active_at(dir.path(), "telegram:1", after).is_none());
    }

    #[test]
    fn second_message_refreshes_without_restarting() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let first = begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        assert!(first.is_started());
        let later = now + Duration::minutes(10);
        let second = begin(dir.path(), &req("telegram:1"), &cfg(), later).unwrap();
        assert!(
            !second.is_started(),
            "a manager's second message must refresh, not re-announce"
        );
        assert_eq!(second.record().started_at, now, "start time is preserved");
        assert_eq!(
            second.record().until,
            later + Duration::minutes(DEFAULT_DURATION_MINUTES),
            "the window re-bases on the latest message"
        );
    }

    #[test]
    fn a_second_manager_takes_the_conversation_over() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        let mut other = req("telegram:1");
        other.holder_user_id = "777".into();
        other.holder_display = "李主管".into();
        let out = begin(dir.path(), &other, &cfg(), now + Duration::minutes(1)).unwrap();
        assert_eq!(out.record().holder_display, "李主管");
        assert_eq!(out.record().holder_user_id, "777");
    }

    #[test]
    fn takeover_is_scoped_to_one_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        assert!(is_conversation_paused(dir.path(), "telegram:1"));
        assert!(
            !is_conversation_paused(dir.path(), "telegram:2"),
            "a sibling conversation on the same channel must be unaffected"
        );
        assert!(!is_conversation_paused(dir.path(), "slack:1"));
        assert!(is_target_paused(dir.path(), "telegram", "1"));
        assert!(!is_target_paused(dir.path(), "telegram", "2"));
        assert!(!is_target_paused(dir.path(), "slack", "1"));
    }

    #[test]
    fn thread_takeover_also_covers_its_parent_chat_target() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1:99"), &cfg(), now).unwrap();
        assert!(
            is_target_paused(dir.path(), "telegram", "1"),
            "pushes addressed at the parent chat must be held back too"
        );
    }

    #[test]
    fn extend_pushes_the_deadline_and_respects_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        let r = extend(dir.path(), "telegram:1", 30, &cfg(), now)
            .unwrap()
            .expect("held conversation extends");
        assert_eq!(r.until, now + Duration::minutes(90));

        // Repeated extension cannot exceed max_duration_minutes from now.
        let c = TakeoverConfig {
            max_duration_minutes: 90,
            ..cfg()
        };
        let r = extend(dir.path(), "telegram:1", MAX_EXTENSION_MINUTES, &c, now)
            .unwrap()
            .unwrap();
        assert_eq!(r.until, now + Duration::minutes(90));
    }

    #[test]
    fn extend_does_not_start_a_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        assert!(extend(dir.path(), "telegram:1", 30, &cfg(), now)
            .unwrap()
            .is_none());
        assert!(!is_conversation_paused(dir.path(), "telegram:1"));
    }

    #[test]
    fn end_releases_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        let released = end(dir.path(), "telegram:1", now).unwrap();
        assert_eq!(released.map(|r| r.holder_display), Some("王小明".into()));
        assert!(!is_conversation_paused(dir.path(), "telegram:1"));
        // Ending twice is a no-op, not an error.
        assert!(end(dir.path(), "telegram:1", now).unwrap().is_none());
    }

    #[test]
    fn sweep_returns_each_expiry_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        begin(dir.path(), &req("telegram:2"), &cfg(), now).unwrap();
        assert!(sweep_expired(dir.path(), now).is_empty());
        let after = now + Duration::minutes(DEFAULT_DURATION_MINUTES + 1);
        let mut swept: Vec<String> = sweep_expired(dir.path(), after)
            .into_iter()
            .map(|r| r.conversation)
            .collect();
        swept.sort();
        assert_eq!(swept, vec!["telegram:1", "telegram:2"]);
        assert!(
            sweep_expired(dir.path(), after).is_empty(),
            "a second sweep must not re-announce the same handback"
        );
    }

    #[test]
    fn claimed_task_ids_are_recorded_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        begin(dir.path(), &req("telegram:1"), &cfg(), now).unwrap();
        record_claimed_tasks(dir.path(), "telegram:1", &["t1".into(), "t2".into()]).unwrap();
        record_claimed_tasks(dir.path(), "telegram:1", &["t2".into(), "t3".into()]).unwrap();
        let r = active_at(dir.path(), "telegram:1", now).unwrap();
        assert_eq!(r.claimed_task_ids, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn begin_refuses_a_conversation_with_no_target() {
        let dir = tempfile::tempdir().unwrap();
        assert!(begin(dir.path(), &req("default"), &cfg(), Utc::now()).is_err());
    }

    #[test]
    fn missing_and_corrupt_state_read_as_no_takeover() {
        let dir = tempfile::tempdir().unwrap();
        assert!(active(dir.path(), "telegram:1").is_none());
        assert!(list_active(dir.path()).is_empty());
        std::fs::write(state_path(dir.path()), b"{ not json").unwrap();
        assert!(active(dir.path(), "telegram:1").is_none());
        assert!(!is_target_paused(dir.path(), "telegram", "1"));
    }

    #[test]
    fn config_clamps_nonsense_values() {
        let c = TakeoverConfig {
            enabled: true,
            duration_minutes: 0,
            max_duration_minutes: -5,
        }
        .clamped();
        assert_eq!(c.max_duration_minutes, 1);
        assert_eq!(c.duration_minutes, 1);

        let c = TakeoverConfig {
            enabled: true,
            duration_minutes: 999_999,
            max_duration_minutes: 999_999,
        }
        .clamped();
        assert_eq!(c.max_duration_minutes, HARD_MAX_DURATION_MINUTES);
        assert_eq!(c.duration_minutes, HARD_MAX_DURATION_MINUTES);
    }

    #[test]
    fn config_from_home_reads_the_section_and_defaults_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            TakeoverConfig::from_home(dir.path()).duration_minutes,
            DEFAULT_DURATION_MINUTES
        );
        std::fs::write(
            dir.path().join("config.toml"),
            "[unrelated]\nx = 1\n\n[takeover]\nenabled = false\nduration_minutes = 15\n",
        )
        .unwrap();
        let c = TakeoverConfig::from_home(dir.path());
        assert!(!c.enabled);
        assert_eq!(c.duration_minutes, 15);

        // Malformed config elsewhere must not break the section.
        std::fs::write(dir.path().join("config.toml"), "this is not toml [[[").unwrap();
        assert_eq!(
            TakeoverConfig::from_home(dir.path()).duration_minutes,
            DEFAULT_DURATION_MINUTES
        );
    }
}
