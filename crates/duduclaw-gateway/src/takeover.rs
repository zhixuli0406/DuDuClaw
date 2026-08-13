//! Human takeover of a channel conversation (W3-1; patterns D1–D5 + D10).
//!
//! ## The behaviour in one paragraph
//!
//! When somebody the dashboard knows as a manager **speaks** in a channel
//! conversation, the AI stops answering that one conversation for a bounded
//! window (default 60 minutes). No button, no mode switch: typing *is* the
//! declaration. This automatic path is **opt-in** (`config.toml [takeover]
//! enabled`, default `false`) — on-by-default silenced the AI whenever an
//! owner/admin talked to their *own* assistant (every Personal-edition chat),
//! which is never a takeover; it is meaningful only for teams whose AI answers
//! others. The explicit `/takeover` command works regardless. While the window
//! is open, every path that
//! could put an AI message into that conversation is skipped, and the goal
//! tasks the conversation spawned are stamped as handled by a human. When the
//! window closes — by timer or by `/takeover end` — the conversation says so.
//!
//! ## Where each pattern lands
//!
//! - **D1 typing is takeover** — [`intercept`], called from the one funnel
//!   every channel's inbound message passes through
//!   (`channel_reply::build_reply_with_session_inner`).
//! - **D2 speaking is the only signal** — the gate is
//!   [`crate::decision_notify::mapped_role`]: an Active dashboard
//!   Admin/Manager reached through a **verified** channel binding. Nothing
//!   else qualifies. There is deliberately no destination-match fallback
//!   (which the button-press gate has for solo operators): applied here it
//!   would mean a solo operator's every message takes over their own
//!   conversation, i.e. the AI would answer exactly once and then go mute
//!   forever. A deployment with no dashboard identities simply has no
//!   typing-takeover, and `/takeover` reports why.
//! - **D3 lifecycle** — bounded window, `/takeover` to query, `/takeover +30m`
//!   to extend, `/takeover end` to hand back, automatic expiry. Nothing
//!   outside `<home>/takeover_state.json` is written; no global setting is
//!   touched (LINE's fourth guarantee, the one that makes staff willing to use
//!   the feature at all).
//! - **D4 atomic three-in-one** — [`begin_takeover`] pauses, claims the
//!   conversation's live goal tasks, and posts one Activity Feed row.
//! - **D5 immediate and total** — [`is_target_paused`] is consulted by every
//!   dispatch path (inventory in the module test
//!   `d5_dispatch_point_inventory_is_documented`), not just by new work.
//! - **D10 identity disclosure** — the channel is told who took over and when
//!   the AI is back. No platform does this for us.
//!
//! ## What the AI does with messages it does not answer
//!
//! It records them. A takeover is a pause, not amnesia: the user's turns are
//! appended to the session so that when the AI resumes it has the whole
//! conversation, including the part the human handled. It stays silent — no
//! "I've been paused" notice — because a human is mid-conversation and a bot
//! interjecting to describe its own state is precisely the interruption this
//! feature removes.

use std::path::Path;

use chrono::{DateTime, Utc};
use duduclaw_auth::models::UserRole;
use duduclaw_core::takeover_state::{
    self, BeginOutcome, BeginRequest, TakeoverConfig, TakeoverRecord,
};
use tracing::{debug, info, warn};

use crate::channel_reply::ReplyContext;
use crate::task_store::{ActivityRow, TaskStore};

/// Activity Feed event for a takeover starting.
pub const EVENT_STARTED: &str = "takeover.started";
/// Activity Feed event for a takeover being handed back.
pub const EVENT_ENDED: &str = "takeover.ended";

/// Fallback display name when the deployment has no better one. Never an
/// internal id — end users read this string.
const FALLBACK_DISPLAY: &str = "管理員";

// ── Identity ────────────────────────────────────────────────────

/// The dashboard display name for a channel account, when that account is an
/// Active Admin/Manager reached through a **verified** binding.
///
/// Authorization itself is [`crate::decision_notify::mapped_role`] — the same
/// predicate every decision button uses, deliberately not re-implemented here.
/// This function only adds the cosmetic half (what to call the person), and
/// degrades to [`FALLBACK_DISPLAY`] rather than leaking a user id.
pub fn manager_display_name(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
) -> Option<String> {
    match crate::decision_notify::mapped_role(home_dir, channel, channel_user_id) {
        Some(UserRole::Admin) | Some(UserRole::Manager) => {}
        _ => return None,
    }
    let display = crate::decision_notify::open_user_db(home_dir)
        .and_then(|db| {
            db.find_verified_user_id_by_channel(channel, channel_user_id)
                .ok()
                .flatten()
                .and_then(|uid| db.get_user(&uid).ok().flatten())
        })
        .map(|u| u.display_name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| FALLBACK_DISPLAY.to_string());
    Some(display)
}

/// Whether this deployment can recognise a manager in a channel at all.
/// Used only to explain `/takeover` to somebody in a deployment that never
/// linked a dashboard identity — never to loosen the gate.
pub fn identity_available(home_dir: &Path) -> bool {
    crate::decision_notify::identity_system_active(home_dir)
}

// ── D5: the predicate every dispatch path consults ──────────────

/// True when an AI message aimed at `(channel, chat_id)` must not be sent.
///
/// Cheap (one small JSON read) and safe to call on every dispatch attempt.
/// Deliberately a free function rather than a method on a handle so the
/// scheduler paths in other crates can call it without plumbing state through.
pub fn is_target_paused(home_dir: &Path, channel: &str, chat_id: &str) -> bool {
    takeover_state::is_target_paused(home_dir, channel, chat_id)
}

/// [`is_target_paused`] plus the record, for callers that want to defer until
/// exactly when the human hands back rather than dropping.
pub fn target_record(home_dir: &Path, channel: &str, chat_id: &str) -> Option<TakeoverRecord> {
    takeover_state::target_record(home_dir, channel, chat_id)
}

/// Log a skipped dispatch. Every D5 skip goes through here so "the AI went
/// quiet" is always explainable from the log, never a silent drop.
pub fn log_skip(path_kind: &str, channel: &str, chat_id: &str, detail: &str) {
    info!(
        path = path_kind,
        %channel,
        %chat_id,
        detail,
        "takeover: dispatch skipped — a human is handling this conversation"
    );
}

// ── D4: the atomic three-in-one ─────────────────────────────────

/// Pause the conversation, claim its live goal tasks, and post one Activity
/// Feed row.
///
/// Ordering is intentional and documented because the three stores cannot be
/// written in one transaction:
/// 1. **Pause first.** It is the only step that is safety-relevant — the whole
///    point is that the AI stops talking *now*. If it fails, nothing else
///    happens and the caller says nothing (announcing a takeover that did not
///    persist is the exact ManyChat failure this feature exists to prevent).
/// 2. **Claim the work.** Best-effort: a task-store hiccup must not un-pause a
///    conversation a human is already typing into.
/// 3. **Post to the feed.** Best-effort, cosmetic.
pub async fn begin_takeover(
    home_dir: &Path,
    conversation: &str,
    agent_id: &str,
    holder_user_id: &str,
    holder_display: &str,
    cfg: &TakeoverConfig,
) -> Result<BeginOutcome, String> {
    let now = Utc::now();
    let req = BeginRequest {
        conversation: conversation.to_string(),
        agent_id: agent_id.to_string(),
        holder_user_id: holder_user_id.to_string(),
        holder_display: holder_display.to_string(),
    };
    // (1) Pause — hard failure.
    let outcome = takeover_state::begin(home_dir, &req, cfg, now)
        .map_err(|e| format!("takeover: could not persist the pause: {e}"))?;

    if !outcome.is_started() {
        // A refresh has nothing new to claim or announce.
        return Ok(outcome);
    }
    let rec = outcome.record().clone();

    // (2) Claim the conversation's live goal tasks — best-effort.
    let decider = decider_id(&rec.channel, &rec.holder_user_id);
    let store = match TaskStore::open(home_dir) {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(error = %e, "takeover: task store unavailable — pause stands, work not claimed");
            None
        }
    };
    let mut claimed: Vec<String> = Vec::new();
    if let Some(store) = &store {
        match store
            .claim_conversation_tasks(&rec.channel, &rec.chat_id, &decider)
            .await
        {
            Ok(ids) => claimed = ids,
            Err(e) => warn!(error = %e, "takeover: claiming conversation tasks failed (non-fatal)"),
        }
    }
    if let Err(e) = takeover_state::record_claimed_tasks(home_dir, conversation, &claimed) {
        debug!(error = %e, "takeover: recording claimed task ids failed (non-fatal)");
    }

    // (3) Activity Feed — best-effort.
    if let Some(store) = &store {
        let summary = if claimed.is_empty() {
            format!(
                "{} 接手了 {} 的對話（{}）",
                rec.holder_display,
                channel_label(&rec.channel),
                rec.chat_id
            )
        } else {
            format!(
                "{} 接手了 {} 的對話（{}），同時接手 {} 件進行中的工作",
                rec.holder_display,
                channel_label(&rec.channel),
                rec.chat_id,
                claimed.len()
            )
        };
        append_activity(
            store,
            EVENT_STARTED,
            &rec.agent_id,
            claimed.first(),
            &summary,
        )
        .await;
    }

    Ok(BeginOutcome::Started(TakeoverRecord {
        claimed_task_ids: claimed,
        ..rec
    }))
}

/// Hand a conversation back early. Returns the record that was holding.
pub async fn end_takeover(
    home_dir: &Path,
    conversation: &str,
) -> Result<Option<TakeoverRecord>, String> {
    let rec = takeover_state::end(home_dir, conversation, Utc::now())
        .map_err(|e| format!("takeover: could not release the pause: {e}"))?;
    if let Some(rec) = &rec {
        if let Ok(store) = TaskStore::open(home_dir) {
            let summary = format!(
                "{} 結束接手，{} 的對話交還 AI（{}）",
                rec.holder_display,
                channel_label(&rec.channel),
                rec.chat_id
            );
            append_activity(&store, EVENT_ENDED, &rec.agent_id, None, &summary).await;
        }
    }
    Ok(rec)
}

/// Stable `claimed_by` value for a human holder — the same
/// `channel:<channel>:<user>` shape `goal_notify::apply_needs_human` already
/// writes, so the board shows one consistent identity whichever route the
/// human took.
pub fn decider_id(channel: &str, holder_user_id: &str) -> String {
    format!("channel:{channel}:{holder_user_id}")
}

// ── D10: what the conversation is told ──────────────────────────

/// Announcement posted into the conversation when a human takes over.
pub fn announce_started(rec: &TakeoverRecord, now: DateTime<Utc>) -> String {
    format!(
        "👤 {} 已接手對話，接下來由真人回覆（約 {} 分鐘）。",
        rec.holder_display,
        rec.minutes_left(now).max(1)
    )
}

/// Announcement posted when the AI resumes.
pub fn announce_resumed() -> String {
    "🤖 AI 已恢復回應。".to_string()
}

// ── D1/D2: the inbound funnel ───────────────────────────────────

/// What [`intercept`] decided about one inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intercepted {
    /// Reply with this text and do not run the AI (the D10 announcement).
    Announce(String),
    /// Run nothing and say nothing. The message was recorded in the session.
    Silent,
}

/// Decide what to do with one inbound channel message, before any AI work.
///
/// `None` ⇒ nothing to do here; the normal AI pipeline continues.
///
/// Fail-closed in the direction that matters: an unrecognised sender never
/// starts a takeover, and any error persisting one means no takeover is
/// claimed (the AI keeps answering, which is the status quo, rather than the
/// conversation going silently mute with nobody told).
pub async fn intercept(
    ctx: &ReplyContext,
    session_id: &str,
    user_id: &str,
    text: &str,
) -> Option<Intercepted> {
    let home = ctx.home_dir.as_path();
    let cfg = TakeoverConfig::from_home(home);
    if !cfg.enabled {
        return None;
    }
    // A conversation with no addressable transport (`default`, internal
    // callers) is out of scope by construction.
    let (channel, _chat_id) = takeover_state::conversation_target(session_id)?;
    if user_id.trim().is_empty() || user_id == "anonymous" {
        // No identity ⇒ cannot be a manager. Still subject to an active
        // takeover, so fall through to the holder check below.
        return holder_silence(ctx, session_id, text, "", None).await;
    }

    let now = Utc::now();
    let existing = takeover_state::active_at(home, session_id, now);
    let manager = manager_display_name(home, &channel, user_id);

    match manager {
        Some(holder_name) => {
            let agent_id = crate::channel_reply::resolve_agent_for_session(ctx, session_id).await;
            match begin_takeover(home, session_id, &agent_id, user_id, &holder_name, &cfg).await {
                Ok(outcome) => {
                    record_turn(ctx, session_id, &agent_id, user_id, text).await;
                    if outcome.is_started() {
                        info!(
                            conversation = session_id,
                            holder = %holder_name,
                            "takeover: started (a manager spoke)"
                        );
                        Some(Intercepted::Announce(announce_started(
                            outcome.record(),
                            now,
                        )))
                    } else {
                        debug!(conversation = session_id, "takeover: window refreshed");
                        Some(Intercepted::Silent)
                    }
                }
                Err(e) => {
                    // Could not persist ⇒ do NOT pretend. Let the AI answer as
                    // it did before this feature existed, and make the failure
                    // loud in the log.
                    warn!(conversation = session_id, error = %e, "takeover: begin failed — AI keeps answering");
                    None
                }
            }
        }
        None => holder_silence(ctx, session_id, text, user_id, existing).await,
    }
}

/// The non-manager branch: silent (but recorded) while somebody holds the
/// conversation, untouched otherwise.
async fn holder_silence(
    ctx: &ReplyContext,
    session_id: &str,
    text: &str,
    user_id: &str,
    existing: Option<TakeoverRecord>,
) -> Option<Intercepted> {
    let existing = existing.or_else(|| takeover_state::active(&ctx.home_dir, session_id))?;
    let agent_id = if existing.agent_id.trim().is_empty() {
        crate::channel_reply::resolve_agent_for_session(ctx, session_id).await
    } else {
        existing.agent_id.clone()
    };
    record_turn(ctx, session_id, &agent_id, user_id, text).await;
    debug!(
        conversation = session_id,
        holder = %existing.holder_display,
        "takeover: inbound message recorded, AI stays silent"
    );
    Some(Intercepted::Silent)
}

/// Append an inbound turn to the session without running the AI, so the
/// conversation the AI resumes into includes what happened while it was away.
///
/// Best-effort: losing one recorded turn is a context regression, not a
/// correctness failure, and must never turn into a visible error for a person
/// who is mid-conversation with a customer.
async fn record_turn(
    ctx: &ReplyContext,
    session_id: &str,
    agent_id: &str,
    user_id: &str,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    if let Err(e) = ctx
        .session_manager
        .get_or_create(session_id, agent_id)
        .await
    {
        debug!(session = session_id, error = %e, "takeover: session ensure failed");
        return;
    }
    // Same sender-prefix convention the normal reply path uses, so the
    // recorded turn is indistinguishable from one the AI answered.
    let body = if user_id.is_empty() || user_id == "anonymous" {
        text.to_string()
    } else {
        format!(
            "{}{user_id}]\n{text}",
            crate::channel_reply::SENDER_PREFIX_OPEN
        )
    };
    let tokens = crate::channel_reply::estimate_tokens_public(&body);
    if let Err(e) = ctx
        .session_manager
        .append_message(session_id, "user", &body, tokens)
        .await
    {
        debug!(session = session_id, error = %e, "takeover: recording the turn failed");
    }
}

// ── Expiry sweeper ──────────────────────────────────────────────

/// Announce every window that closed since the last sweep.
///
/// Returns the number of conversations handed back. Run from a background
/// tick; the sweep itself is a locked read-modify-write, so a second sweeper
/// (or a restart mid-tick) can never announce the same handback twice.
pub async fn sweep_and_announce(home_dir: &Path) -> usize {
    let expired = takeover_state::sweep_expired(home_dir, Utc::now());
    if expired.is_empty() {
        return 0;
    }
    let http = reqwest::Client::new();
    for rec in &expired {
        info!(
            conversation = %rec.conversation,
            holder = %rec.holder_display,
            "takeover: window closed — AI resumes"
        );
        if let Ok(store) = TaskStore::open(home_dir) {
            let summary = format!(
                "接手時間到期，{} 的對話交還 AI（{}）",
                channel_label(&rec.channel),
                rec.chat_id
            );
            append_activity(&store, EVENT_ENDED, &rec.agent_id, None, &summary).await;
        }
        announce_resume_to_channel(home_dir, &http, rec).await;
    }
    expired.len()
}

/// Push the "AI is back" line into the conversation. Best-effort: a channel
/// that cannot be pushed to (no token, non-pushable transport) simply gets no
/// notice — the Activity Feed row above is the durable record either way.
async fn announce_resume_to_channel(home_dir: &Path, http: &reqwest::Client, rec: &TakeoverRecord) {
    let Some(token) =
        crate::goal_notify::channel_token(home_dir, &rec.agent_id, &rec.channel).await
    else {
        debug!(channel = %rec.channel, "takeover: no bot token; resume notice skipped");
        return;
    };
    let _ = crate::goal_notify::send_plain_text(
        home_dir,
        http,
        &rec.channel,
        &token,
        &rec.chat_id,
        &announce_resumed(),
    )
    .await;
}

/// Background driver for [`sweep_and_announce`].
pub struct TakeoverSweeper {
    home_dir: std::path::PathBuf,
}

impl TakeoverSweeper {
    pub fn new(home_dir: std::path::PathBuf) -> Self {
        Self { home_dir }
    }

    /// Tick forever at `interval`. The interval only bounds how late a
    /// handback notice is, never how late the pause lifts — expiry is
    /// evaluated at read time by every consumer, so the AI resumes answering
    /// the moment the window closes whether or not this task has run.
    pub async fn run(self, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let n = sweep_and_announce(&self.home_dir).await;
            if n > 0 {
                debug!(handed_back = n, "takeover sweeper tick");
            }
        }
    }
}

// ── Shared bits ─────────────────────────────────────────────────

/// End-user label for a transport. Internal channel keys never reach a
/// person's screen (product charter #6).
pub fn channel_label(channel: &str) -> &str {
    match channel {
        "telegram" => "Telegram",
        "discord" => "Discord",
        "slack" => "Slack",
        "line" => "LINE",
        "whatsapp" => "WhatsApp",
        "feishu" => "飛書",
        "googlechat" => "Google Chat",
        "teams" => "Teams",
        "wecom" => "企業微信",
        "dingtalk" => "釘釘",
        "webchat" => "網頁聊天",
        other => other,
    }
}

async fn append_activity(
    store: &TaskStore,
    event_type: &str,
    agent_id: &str,
    task_id: Option<&String>,
    summary: &str,
) {
    let row = ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: task_id.cloned(),
        summary: summary.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        metadata: None,
    };
    if let Err(e) = store.append_activity(&row).await {
        debug!(error = %e, "takeover: activity append failed (non-fatal)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::takeover_state::DEFAULT_DURATION_MINUTES;

    fn cfg() -> TakeoverConfig {
        // Behaviour tests opt in explicitly (default is now off).
        TakeoverConfig { enabled: true, ..TakeoverConfig::default() }
    }

    /// D5 inventory, kept next to the predicate it documents. Every entry is
    /// a place that could put an AI message into a conversation; each one
    /// consults the takeover state before dispatching. If a new dispatch path
    /// is added, it belongs here.
    ///
    /// The second list is the deliberate *exception* set, written down for the
    /// same reason: a gap that is a decision reads identically to a gap that
    /// is an oversight unless somebody says which it is. Those three are
    /// cross-cutting **L3** pushes (`NotifyLevel::Act` — urgent, important and
    /// actionable) that are not about the held conversation's own work. The
    /// notification-governance model already exempts L3 from quiet hours for
    /// the same reason, and holding back "this action could be irreversible,
    /// approve?" because a manager happens to be chatting in that channel
    /// would trade a small interruption for a real risk.
    #[test]
    fn d5_dispatch_point_inventory_is_documented() {
        let gated = [
            "channel_reply::build_reply_with_session_inner (inbound reply — silent)",
            "goal_loop::GoalLoopDriver::tick_once (goal task dispatch — frozen)",
            "goal_notify::notify_goal_progress (progress push — deferred)",
            "goal_notify::notify_goal_needs_human (needs-human card — deferred)",
            "goal_notify::notify_goal_observer (observer result — deferred)",
            "goal_notify::notify_agent_plain (generic L1/L2 push — deferred)",
            "autopilot_notify circuit-open card (L2 — deferred)",
            "heartbeat::poll_assigned_tasks (task-board wake-up — skipped)",
            "heartbeat proactive check (unsolicited nudge — dropped)",
            "cron_scheduler::deliver_cron_result (routine output — dropped)",
            "dispatcher::forward_to_channel (bus_queue callback — dropped)",
        ];
        let deliberately_not_gated = [
            "approval_notify (L3 high-risk action approval)",
            "install_notify (L3 install signature request)",
            "channel_alerts (L3 channel-outage alert)",
        ];
        assert_eq!(gated.len(), 11, "update the module docs when this changes");
        assert_eq!(deliberately_not_gated.len(), 3);
    }

    #[tokio::test]
    async fn begin_pauses_claims_and_is_scoped_to_one_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let out = begin_takeover(home, "telegram:1", "alice", "555", "王小明", &cfg())
            .await
            .unwrap();
        assert!(out.is_started());
        assert!(is_target_paused(home, "telegram", "1"));
        assert!(!is_target_paused(home, "telegram", "2"));
        assert!(!is_target_paused(home, "slack", "1"));

        // A second message from the same person refreshes silently.
        let again = begin_takeover(home, "telegram:1", "alice", "555", "王小明", &cfg())
            .await
            .unwrap();
        assert!(!again.is_started());
    }

    fn seed_task(
        id: &str,
        title: &str,
        goal_mode: bool,
        status: &str,
        chat_id: &str,
    ) -> crate::task_store::TaskRow {
        let mut t = crate::task_store::TaskRow::new(
            id.to_string(),
            title.to_string(),
            String::new(),
            "medium".into(),
            "alice".into(),
            "test".into(),
        );
        t.goal_mode = goal_mode;
        t.status = status.to_string();
        t.source_channel = Some("telegram".into());
        t.source_chat_id = Some(chat_id.to_string());
        t
    }

    #[tokio::test]
    async fn begin_claims_the_conversations_live_goal_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let store = TaskStore::open(home).unwrap();

        let live = seed_task("t-live", "跑月報", true, "todo", "1");
        store.insert_task(&live).await.unwrap();
        let other_conv = seed_task("t-other", "別的對話", true, "todo", "2");
        store.insert_task(&other_conv).await.unwrap();
        let finished = seed_task("t-done", "已完成", true, "done", "1");
        store.insert_task(&finished).await.unwrap();
        let ordinary = seed_task("t-plain", "一般看板任務", false, "todo", "1");
        store.insert_task(&ordinary).await.unwrap();

        let out = begin_takeover(home, "telegram:1", "alice", "555", "王小明", &cfg())
            .await
            .unwrap();
        assert_eq!(out.record().claimed_task_ids, vec![live.id.clone()]);

        let got = store.get_task(&live.id).await.unwrap().unwrap();
        assert_eq!(got.claimed_by.as_deref(), Some("channel:telegram:555"));
        assert!(store
            .get_task(&other_conv.id)
            .await
            .unwrap()
            .unwrap()
            .claimed_by
            .is_none());
        assert!(store
            .get_task(&finished.id)
            .await
            .unwrap()
            .unwrap()
            .claimed_by
            .is_none());
        assert!(
            store
                .get_task(&ordinary.id)
                .await
                .unwrap()
                .unwrap()
                .claimed_by
                .is_none(),
            "an ordinary board task is not driven by this conversation"
        );
    }

    #[tokio::test]
    async fn begin_posts_one_activity_row() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        begin_takeover(home, "telegram:1", "alice", "555", "王小明", &cfg())
            .await
            .unwrap();
        let store = TaskStore::open(home).unwrap();
        let (rows, _) = store
            .list_activity(None, Some(EVENT_STARTED), 20, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].summary.contains("王小明"));
        assert!(
            !rows[0].summary.contains("takeover"),
            "user-facing summary must not leak internal vocabulary"
        );
    }

    #[tokio::test]
    async fn end_releases_and_records_the_handback() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        begin_takeover(home, "telegram:1", "alice", "555", "王小明", &cfg())
            .await
            .unwrap();
        let released = end_takeover(home, "telegram:1").await.unwrap();
        assert_eq!(released.map(|r| r.holder_display), Some("王小明".into()));
        assert!(!is_target_paused(home, "telegram", "1"));

        let store = TaskStore::open(home).unwrap();
        let (rows, _) = store
            .list_activity(None, Some(EVENT_ENDED), 20, 0)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn sweeper_hands_back_once_per_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // A window that is already closed.
        let c = TakeoverConfig {
            duration_minutes: 1,
            ..cfg()
        };
        begin_takeover(home, "telegram:1", "alice", "555", "王小明", &c)
            .await
            .unwrap();
        // Rewrite the record so it has expired without waiting a minute.
        let path = takeover_state::state_path(home);
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut state: std::collections::HashMap<String, TakeoverRecord> =
            serde_json::from_str(&raw).unwrap();
        state.get_mut("telegram:1").unwrap().until = Utc::now() - chrono::Duration::minutes(1);
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        assert_eq!(sweep_and_announce(home).await, 1);
        assert_eq!(
            sweep_and_announce(home).await,
            0,
            "a handback is announced exactly once"
        );
        assert!(!is_target_paused(home, "telegram", "1"));
    }

    #[test]
    fn announcements_are_plain_zh_tw_without_internal_vocabulary() {
        let now = Utc::now();
        let rec = TakeoverRecord {
            conversation: "telegram:1".into(),
            channel: "telegram".into(),
            chat_id: "1".into(),
            agent_id: "alice".into(),
            holder_user_id: "555".into(),
            holder_display: "王小明".into(),
            started_at: now,
            until: now + chrono::Duration::minutes(DEFAULT_DURATION_MINUTES),
            claimed_task_ids: vec![],
        };
        let started = announce_started(&rec, now);
        assert!(started.contains("王小明"));
        assert!(started.contains("已接手對話"));
        for internal in [
            "takeover",
            "needs_human",
            "goal_mode",
            "claimed_by",
            "session",
        ] {
            assert!(
                !started.contains(internal),
                "announcement leaked internal term: {internal}"
            );
            assert!(!announce_resumed().contains(internal));
        }
    }

    #[test]
    fn decider_id_matches_the_button_path() {
        assert_eq!(decider_id("telegram", "555"), "channel:telegram:555");
    }

    // ── D2: "speaking is the only signal" is only safe if the identity gate
    //    is genuinely conservative. These are the tests that keep it that
    //    way — every one of them is a way the AI could be silenced by
    //    somebody who is not entitled to do it. ──

    /// Seed a `users.db` with one user and one channel binding.
    fn seed_user(
        home: &Path,
        display: &str,
        role: UserRole,
        channel_user_id: &str,
        verified: bool,
    ) {
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let u = db
            .create_user(
                &format!("{display}@example.com"),
                display,
                "correct-horse-battery-staple",
                role,
            )
            .unwrap();
        db.bind_channel_identity(&u.id, "telegram", channel_user_id, verified)
            .unwrap();
    }

    #[test]
    fn manager_display_is_none_without_a_user_database() {
        let dir = tempfile::tempdir().unwrap();
        assert!(manager_display_name(dir.path(), "telegram", "555").is_none());
        assert!(!identity_available(dir.path()));
    }

    #[test]
    fn a_verified_admin_or_manager_is_recognised_by_display_name() {
        let dir = tempfile::tempdir().unwrap();
        seed_user(dir.path(), "王小明", UserRole::Admin, "555", true);
        assert_eq!(
            manager_display_name(dir.path(), "telegram", "555").as_deref(),
            Some("王小明")
        );

        let dir2 = tempfile::tempdir().unwrap();
        seed_user(dir2.path(), "李主管", UserRole::Manager, "777", true);
        assert_eq!(
            manager_display_name(dir2.path(), "telegram", "777").as_deref(),
            Some("李主管")
        );
    }

    #[test]
    fn an_ordinary_member_never_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        seed_user(dir.path(), "小員工", UserRole::Employee, "999", true);
        assert!(
            manager_display_name(dir.path(), "telegram", "999").is_none(),
            "a non-approver role must never be able to mute the AI"
        );
    }

    #[test]
    fn an_unverified_binding_never_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        seed_user(dir.path(), "王小明", UserRole::Admin, "555", false);
        assert!(
            manager_display_name(dir.path(), "telegram", "555").is_none(),
            "an unverified channel claim is not proof of identity"
        );
    }

    #[test]
    fn an_unknown_account_never_takes_over() {
        let dir = tempfile::tempdir().unwrap();
        seed_user(dir.path(), "王小明", UserRole::Admin, "555", true);
        assert!(manager_display_name(dir.path(), "telegram", "000").is_none());
        // Cross-channel: the same id on another transport is a different
        // person. Exact `(channel, id)` match only.
        assert!(manager_display_name(dir.path(), "slack", "555").is_none());
    }

    // ── D1 end-to-end: what `intercept` actually does to an inbound turn ──

    fn test_ctx(home: &Path) -> ReplyContext {
        let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
            duduclaw_agent::AgentRegistry::new(home.join("agents")),
        ));
        let sessions = std::sync::Arc::new(
            crate::session::SessionManager::new(&home.join("sessions.db")).unwrap(),
        );
        let status: crate::channel_reply::ChannelStatusMap =
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        ReplyContext::new(registry, home.to_path_buf(), sessions, status, tx)
    }

    async fn session_texts(ctx: &ReplyContext, session_id: &str) -> Vec<String> {
        ctx.session_manager
            .get_messages(session_id)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.content)
            .collect()
    }

    #[tokio::test]
    async fn a_manager_speaking_takes_over_and_announces_once() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Auto-takeover is opt-in now; a team that wants it turns it on.
        std::fs::write(home.join("config.toml"), "[takeover]\nenabled = true\n").unwrap();
        seed_user(home, "王小明", UserRole::Admin, "555", true);
        let ctx = test_ctx(home);

        match intercept(&ctx, "telegram:1", "555", "我來處理").await {
            Some(Intercepted::Announce(msg)) => assert!(msg.contains("王小明")),
            other => panic!("expected an announcement, got {other:?}"),
        }
        assert!(is_target_paused(home, "telegram", "1"));

        // The second message refreshes silently — no repeat announcement.
        assert_eq!(
            intercept(&ctx, "telegram:1", "555", "還在查").await,
            Some(Intercepted::Silent)
        );

        // Both of the manager's turns are in the session, so the AI resumes
        // with the whole conversation.
        let texts = session_texts(&ctx, "telegram:1").await;
        assert_eq!(texts.len(), 2);
        assert!(texts[0].contains("我來處理"));
        assert!(texts[1].contains("還在查"));
    }

    #[tokio::test]
    async fn an_ordinary_user_is_recorded_but_never_answered_or_told() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("config.toml"), "[takeover]\nenabled = true\n").unwrap();
        seed_user(home, "王小明", UserRole::Admin, "555", true);
        let ctx = test_ctx(home);
        intercept(&ctx, "telegram:1", "555", "我來處理").await;

        let out = intercept(&ctx, "telegram:1", "999", "請問進度？").await;
        assert_eq!(
            out,
            Some(Intercepted::Silent),
            "a customer's message is neither answered by the AI nor met with a \
             'the bot is paused' notice — a human is mid-conversation"
        );
        let texts = session_texts(&ctx, "telegram:1").await;
        assert!(texts.iter().any(|t| t.contains("請問進度？")));
    }

    #[tokio::test]
    async fn an_ordinary_user_speaking_never_starts_a_takeover() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        seed_user(home, "小員工", UserRole::Employee, "999", true);
        let ctx = test_ctx(home);
        assert_eq!(intercept(&ctx, "telegram:1", "999", "哈囉").await, None);
        assert!(!is_target_paused(home, "telegram", "1"));
        // Nothing was recorded either — the normal reply path owns that turn.
        assert!(session_texts(&ctx, "telegram:1").await.is_empty());
    }

    #[tokio::test]
    async fn a_deployment_with_no_dashboard_identities_never_takes_over() {
        // The solo-operator case. The button-press gate has a
        // destination-match fallback here; takeover deliberately does not —
        // it would mute the AI on the operator's first message, forever.
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        assert_eq!(intercept(&ctx, "telegram:1", "555", "哈囉").await, None);
        assert!(!is_target_paused(dir.path(), "telegram", "1"));
    }

    #[tokio::test]
    async fn the_feature_switch_disables_the_whole_gate() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("config.toml"), "[takeover]\nenabled = false\n").unwrap();
        seed_user(home, "王小明", UserRole::Admin, "555", true);
        let ctx = test_ctx(home);
        assert_eq!(intercept(&ctx, "telegram:1", "555", "我來處理").await, None);
        assert!(!is_target_paused(home, "telegram", "1"));
    }

    #[tokio::test]
    async fn a_conversation_with_no_transport_is_out_of_scope() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        seed_user(home, "王小明", UserRole::Admin, "555", true);
        let ctx = test_ctx(home);
        assert_eq!(intercept(&ctx, "default", "555", "我來處理").await, None);
    }

    #[tokio::test]
    async fn expiry_resumes_the_ai_without_anybody_sweeping_first() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.toml"),
            "[takeover]\nenabled = true\nduration_minutes = 1\n",
        )
        .unwrap();
        seed_user(home, "王小明", UserRole::Admin, "555", true);
        let ctx = test_ctx(home);
        intercept(&ctx, "telegram:1", "555", "我來處理").await;

        // Force the window closed.
        let path = takeover_state::state_path(home);
        let mut state: std::collections::HashMap<String, TakeoverRecord> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        state.get_mut("telegram:1").unwrap().until = Utc::now() - chrono::Duration::minutes(1);
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();

        assert_eq!(
            intercept(&ctx, "telegram:1", "999", "請問進度？").await,
            None,
            "an expired window must let the AI answer again immediately"
        );
        assert!(!is_target_paused(home, "telegram", "1"));
    }

    #[test]
    fn a_manager_without_a_display_name_still_gets_a_human_label() {
        let dir = tempfile::tempdir().unwrap();
        // Empty display name — the fallback must be a person-readable word,
        // never a leaked account id.
        seed_user(dir.path(), "", UserRole::Admin, "555", true);
        let got = manager_display_name(dir.path(), "telegram", "555");
        assert_eq!(got.as_deref(), Some(FALLBACK_DISPLAY));
        assert!(!got.unwrap().contains("555"));
    }
}
