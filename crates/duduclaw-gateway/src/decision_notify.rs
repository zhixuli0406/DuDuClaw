//! The one pipeline behind every "待辦決定" — a thing that will not move
//! until a person says yes or no.
//!
//! Five HITL entry points feed it (install two-stage sign-off, goal-loop
//! needs_human, the goal-loop kickoff gate, the generic `ApprovalBroker`,
//! autopilot circuit-breaker pause). To the people using the product they are
//! **one object**; which store backs a given one is an internal attribute
//! ([`crate::decision_action::DecisionSource`]), not something a user should
//! have to learn.
//!
//! ## What lives here
//!
//! - **Authorization** — [`authorize_press`] and the identity lookups behind
//!   it. One matrix for all five sources; a per-source difference would be a
//!   security hole waiting to be found, and one of them (goal-loop) was
//!   exactly that: it validated nothing beyond "the id decodes".
//! - **Destination resolution** — [`resolve_targets`] and the origin-session
//!   parsing that feeds it.
//! - **Delivery** — [`deliver`]: buttons where the platform has them, plain
//!   text plus a deep link where it doesn't, and the card's message identity
//!   recorded either way so the settled decision can retire it in place.
//! - **Inbound routing** — [`route_press`]: the single entry every channel
//!   dispatcher calls. Each dispatcher previously carried one near-identical
//!   block per source; a sixth source would have meant a fifth block in each
//!   of four files.
//!
//! ## What deliberately does NOT live here
//!
//! The stores. `approvals.db`, `install_requests`, `TaskStore` and
//! `autopilot.db` have genuinely different state machines (two-stage
//! sign-off, TTL-expiry-as-denial, task status transitions, a circuit-breaker
//! flag). Each owning module keeps its own `apply_*` and its own zh-TW body
//! copy; this module owns everything *between* the store and the channel.

use std::path::Path;

use duduclaw_auth::UserDb;
use duduclaw_auth::models::{UserRole, UserStatus};

use crate::decision_action::{DecisionAct, DecisionAction, DecisionSource};
use crate::notify_governance::NotifyLevel;

// ── Escalation level (W2-4, P4-1) ───────────────────────────────

/// Where each decision source sits on the escalation ladder.
///
/// The rule from `02-ux-methodology.md` P4-1 is "urgent AND important AND
/// actionable AND real ⇒ L3". Applied honestly, that splits the five sources
/// two ways:
///
/// - **L3 (never suppressed)** — `Goal` (an autonomous run has stopped dead
///   waiting for a person), `Approval` (a high-risk, often irreversible
///   action is being held), `Install` (a two-stage sign-off gating software
///   entering the deployment). Each blocks something with a real cost to
///   waiting, and each is exactly the "would you rather be woken than find
///   out eight hours later" case.
/// - **L2 (deferrable)** — `Kickoff` (a goal has not *started*; the cost of
///   starting at 08:00 instead of 03:00 is one night) and `Autopilot` (a rule
///   already stopped itself; the breaker is the safety, the message is the
///   notice).
pub fn notify_level(source: DecisionSource) -> NotifyLevel {
    match source {
        DecisionSource::Goal | DecisionSource::Approval | DecisionSource::Install => {
            NotifyLevel::Act
        }
        DecisionSource::Kickoff | DecisionSource::Autopilot => NotifyLevel::Confirm,
        // A digest is a report: nothing waits on it.
        DecisionSource::Digest => NotifyLevel::Fyi,
    }
}

/// The action-rate stats bucket for a decision source
/// ([`crate::notify_stats`]). One bucket per source, so P4-5's "is this type
/// of notification worth sending" question is answerable per source rather
/// than for "decisions" as an undifferentiated lump.
pub fn notify_type(source: DecisionSource) -> &'static str {
    match source {
        DecisionSource::Goal => "decision.goal",
        DecisionSource::Kickoff => "decision.kickoff",
        DecisionSource::Approval => "decision.approval",
        DecisionSource::Install => "decision.install",
        DecisionSource::Autopilot => "decision.autopilot",
        DecisionSource::Digest => "digest.feedback",
    }
}

/// External channels a pending decision may be pushed to. A session id like
/// `webchat:<conn>#agent:…` names a transport with no bot-push API, so it must
/// never be mistaken for a destination.
pub(crate) fn is_pushable_channel(channel: &str) -> bool {
    matches!(
        channel,
        "telegram" | "slack" | "discord" | "line" | "whatsapp" | "feishu" | "googlechat" | "teams"
    )
}

// ── Identity ────────────────────────────────────────────────────

/// Open `users.db` ONLY if it already exists. `UserDb::new` would create the
/// file; a channel dispatcher (or the `duduclaw mcp-server` child process)
/// must not conjure an auth database as a side effect of handling a press.
pub(crate) fn open_user_db(home_dir: &Path) -> Result<Option<UserDb>, String> {
    let path = home_dir.join("users.db");
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("identity store metadata unavailable".into()),
        Ok(_) => UserDb::new(&path)
            .map(Some)
            .map_err(|_| "identity store unreadable or corrupt".into()),
    }
}

/// New bound decisions distinguish an absent identity deployment from every
/// database/lookup failure. A configured system never downgrades to solo mode.
fn bound_decision_authority(
    home: &Path,
    channel: &str,
    principal: &str,
) -> Result<(Option<UserRole>, bool), String> {
    let Some(db) = open_user_db(home)? else {
        return Ok((None, false));
    };
    let active = !db
        .list_users()
        .map_err(|_| "identity list unavailable")?
        .is_empty();
    let uid = db
        .find_verified_user_id_by_channel(channel, principal)
        .map_err(|_| "verified identity lookup failed")?;
    let role = match uid {
        Some(uid) => db
            .get_user(&uid)
            .map_err(|_| "principal lookup failed")?
            .filter(|u| u.status == UserStatus::Active)
            .map(|u| u.role),
        None => None,
    };
    Ok((role, active))
}

/// Verified channel identities of every Active Admin/Manager — the humans the
/// dashboard would let decide. Empty when `users.db` does not exist (a solo
/// deployment that never onboarded dashboard users).
pub(crate) fn approver_links(home_dir: &Path) -> Vec<(String, String)> {
    let Ok(Some(db)) = open_user_db(home_dir) else {
        return Vec::new();
    };
    let Ok(users) = db.list_users() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in users {
        if u.status != UserStatus::Active || !matches!(u.role, UserRole::Admin | UserRole::Manager)
        {
            continue;
        }
        if let Ok(idents) = db.verified_channels_for_user(&u.id) {
            for i in idents {
                out.push((i.channel, i.channel_user_id));
            }
        }
    }
    out
}

/// The dashboard role of the account that pressed, when it maps to an Active
/// user through a **verified** channel binding.
///
/// Verified only: an unverified `channel_identities` row is an unconfirmed
/// claim typed into a binding form, never proof of identity — filing one
/// against a manager's account would otherwise inherit their rights.
pub(crate) fn mapped_role(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
) -> Option<UserRole> {
    let db = open_user_db(home_dir).ok().flatten()?;
    let uid = db
        .find_verified_user_id_by_channel(channel, channel_user_id)
        .ok()
        .flatten()?;
    let u = db.get_user(&uid).ok().flatten()?;
    (u.status == UserStatus::Active).then_some(u.role)
}

/// Whether this deployment has any channel-reachable approver identity at
/// all. When false, the only authority available is the destination the
/// operator configured — see [`authorize_press`].
pub(crate) fn identity_system_active(home_dir: &Path) -> bool {
    open_user_db(home_dir).is_err() || !approver_links(home_dir).is_empty()
}

// ── Authorization ───────────────────────────────────────────────

/// The authorization verdict for one button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressAuth {
    Allow,
    /// The presser maps to a dashboard user without decision rights.
    DenyNotApprover,
    /// The presser has no usable identity and did not press from the
    /// destination this decision was delivered to.
    DenyUnknown,
}

/// Pure authorization for a decision button press. Fail-closed: anything not
/// explicitly allowed is a denial.
///
/// - A mapped, Active dashboard user decides by **role** — Admin/Manager may
///   decide, everyone else may not. This keeps separation of duties intact:
///   an employee never signs off their own request.
/// - When the deployment has **no channel-reachable approver identity at all**
///   (`identity_system_active == false`), the only authority available is the
///   destination the operator configured (or the conversation that raised the
///   request), so a press from that exact destination is honoured. Without
///   this, a solo operator who never created dashboard users could never
///   decide anything from a phone. It does NOT loosen a configured
///   deployment: as soon as one approver links a channel, role rules apply to
///   everyone.
pub fn authorize_press(
    mapped_role: Option<UserRole>,
    identity_system_active: bool,
    destination_match: bool,
) -> PressAuth {
    match mapped_role {
        Some(UserRole::Admin) | Some(UserRole::Manager) => PressAuth::Allow,
        Some(_) => PressAuth::DenyNotApprover,
        None => {
            if !identity_system_active && destination_match {
                PressAuth::Allow
            } else {
                PressAuth::DenyUnknown
            }
        }
    }
}

/// True when the press came from the **individual account** one of `targets`
/// addresses.
///
/// Deliberately compares `channel_user_id` against each target's chat id and
/// nothing else. Matching the *conversation* instead would mean that when a
/// decision lands in a group, every member of that group satisfies the
/// solo-operator branch of [`authorize_press`] — a back door where any
/// colleague can approve a skill install. Telegram / Discord / LINE report
/// `channel_user_id == chat_id` for a 1:1 DM, which is the destination that
/// branch exists to serve, so DMs are unaffected.
///
/// Consequence, stated plainly: a decision delivered only to a **group** can
/// never clear the solo-operator branch. That deployment must bind a
/// dashboard identity or decide from the dashboard — the correct answer for a
/// shared conversation.
pub(crate) fn destination_matches_any(
    targets: &[(String, String)],
    channel: &str,
    channel_user_id: &str,
) -> bool {
    // Exact equality only — no substring shortcuts for a security decision.
    targets
        .iter()
        .any(|(ch, id)| ch == channel && id == channel_user_id)
}

/// The zh-TW refusal to show for a denied press. `subject` names what the
/// press was trying to do, so the message reads as an answer to the button
/// the person actually pressed.
pub(crate) fn refusal_text(auth: PressAuth, subject: &str) -> String {
    match auth {
        PressAuth::Allow => String::new(),
        PressAuth::DenyNotApprover => format!("您沒有{subject}的權限"),
        PressAuth::DenyUnknown => {
            "此帳號尚未連結儀表板身分，無法操作。請先於儀表板以此通道登入綁定。".to_string()
        }
    }
}

// ── Destination resolution ──────────────────────────────────────

/// Parse a `DUDUCLAW_REPLY_CHANNEL`-style session id into a pushable
/// `(channel, chat_id)` destination.
///
/// Grammar matches `dispatcher::parse_reply_channel`: `<type>:<id>[:<thread>]`,
/// with the `<type>:thread:<id>` marker collapsing to `<id>`. Returns `None`
/// for a malformed value or a non-pushable transport (webchat, dashboard, …) —
/// the caller then falls through to the next link in the chain.
pub(crate) fn parse_origin(reply_channel: &str) -> Option<(String, String)> {
    let rc = reply_channel.trim();
    let parts: Vec<&str> = rc.splitn(3, ':').collect();
    if parts.len() < 2 {
        return None;
    }
    let channel = parts[0].trim();
    if !is_pushable_channel(channel) {
        return None;
    }
    let chat_id = if parts.len() == 3 && parts[1] == "thread" {
        parts[2]
    } else {
        parts[1]
    };
    let chat_id = chat_id.trim();
    // A composed WebChat-style id (`…#agent:…`) is not a chat id.
    if chat_id.is_empty() || chat_id.contains('#') {
        return None;
    }
    Some((channel.to_string(), chat_id.to_string()))
}

/// The conversation a decision was raised from, if any.
///
/// Two lookups, in order: the in-process `REPLY_CHANNEL` task-local (gateway
/// callers — autopilot, wiki ingest, skill activation) and the
/// `DUDUCLAW_REPLY_CHANNEL` env var (the `duduclaw mcp-server` child process,
/// which inherits it from the CLI spawn). The MCP server that files an
/// install-class approval already knows it was reached from
/// `telegram:<chat_id>`, so no extra plumbing is needed to push back there.
pub(crate) fn origin_target() -> Option<(String, String)> {
    if let Ok(rc) = crate::claude_runner::REPLY_CHANNEL.try_with(|ch| ch.clone()) {
        if let Some(t) = parse_origin(&rc) {
            return Some(t);
        }
    }
    let rc = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL).ok()?;
    parse_origin(&rc)
}

/// Pure destination chain: **origin conversation → the agent's own control
/// channel → the linked channels of everyone who may decide**.
///
/// Rationale for the order: pushing back into the conversation that triggered
/// the action is the only destination guaranteed to be watched by the person
/// waiting on it. The agent's `[proactive]` config is the agent-level
/// fallback every other proactive push already uses. The approver fan-out is
/// the last resort so a decision raised by a cron/autopilot path still
/// reaches a human.
///
/// Duplicate destinations are collapsed (an approver whose linked chat IS the
/// origin must not be messaged twice).
pub(crate) fn resolve_targets(
    origin: Option<(String, String)>,
    agent_default: Option<(String, String)>,
    approver_links: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let chosen = match (origin, agent_default) {
        (Some(o), _) => vec![o],
        (None, Some(a)) => vec![a],
        (None, None) => approver_links,
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for t in chosen {
        if t.0.trim().is_empty() || t.1.trim().is_empty() || !is_pushable_channel(&t.0) {
            continue;
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

// ── Outbound delivery ───────────────────────────────────────────

/// Everything the shared delivery path needs that differs per decision. The
/// zh-TW body (including any forward-trajectory line) is composed by the
/// owning module, because only it knows what the decision is about.
pub(crate) struct DecisionCard<'a> {
    pub source: DecisionSource,
    pub decision_id: &'a str,
    /// Already-composed message body, one message worth of zh-TW.
    pub body: &'a str,
    /// Deep link to where this object is handled in the dashboard, or `None`
    /// when no base URL is resolvable. `None` must leave the text exactly as
    /// it reads without the feature — a dangling or empty link is worse than
    /// no link.
    pub link: Option<&'a str>,
    /// What to tell someone whose channel cannot render buttons.
    pub no_button_hint: &'a str,
}

// O5 (2026-09-29): the boolean `deliver()` wrapper was removed. Its four
// callers were the four notification modules, and each of them read the same
// meaning out of it — "not Failed ⇒ my job is done" — which is now
// `Receipt::delivered` in `notify_push`. Callers that need the three-state
// answer use [`deliver_outcome`] below directly.

/// What [`deliver_outcome`] did with a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliverOutcome {
    /// Pushed to the channel now.
    Sent,
    /// Held back by quiet hours and queued. Not a failure, and NOT worth
    /// retrying — a retry would enqueue a duplicate.
    Deferred,
    /// Neither happened.
    Failed,
}

/// [`deliver`] with the deferral distinguishable from a real send. Callers
/// that surface an outcome to a retry loop (the goal loop) use this; callers
/// that only need "did the caller's job get done" use [`deliver`].
pub(crate) async fn deliver_outcome(
    home_dir: &Path,
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    chat_id: &str,
    card: &DecisionCard<'_>,
) -> DeliverOutcome {
    let level = notify_level(card.source);
    // Cards addressed to a specific agent would ideally use that agent's
    // quiet hours; a `DecisionCard` carries no agent id, so the deployment
    // fallback (`config.toml [notify] quiet_hours`) is what applies. Stated
    // rather than silently approximated — per-agent windows for decision
    // cards need an agent id threaded through five call sites, which is a
    // separate change.
    let policy = crate::notify_governance::QuietPolicy {
        window: crate::notify_governance::load_global_window(home_dir),
        tz: crate::notify_governance::NotifyTz::System,
    };
    if let Some(until) = policy.decide(level, chrono::Utc::now()) {
        let queued = crate::notify_governance::enqueue(
            home_dir,
            crate::notify_governance::DeferredNotice {
                id: uuid::Uuid::new_v4().to_string(),
                agent_id: String::new(),
                channel: channel.to_string(),
                chat_id: chat_id.to_string(),
                level: level.as_str().to_string(),
                notify_type: notify_type(card.source).to_string(),
                queued_at: chrono::Utc::now().to_rfc3339(),
                deliver_after: until.to_rfc3339(),
                kind: crate::notify_governance::NoticeKind::Decision,
                text: card.body.to_string(),
                link: card.link.map(str::to_string),
                no_button_hint: Some(card.no_button_hint.to_string()),
                decision_source: Some(card.source.token().to_string()),
                decision_id: Some(card.decision_id.to_string()),
            },
        );
        return if queued {
            DeliverOutcome::Deferred
        } else {
            DeliverOutcome::Failed
        };
    }
    if deliver_now(home_dir, http, channel, token, chat_id, card).await {
        crate::notify_stats::record_push(
            home_dir,
            notify_type(card.source),
            level,
            Some(card.decision_id),
        );
        DeliverOutcome::Sent
    } else {
        DeliverOutcome::Failed
    }
}

/// The un-gated push. Called by [`deliver`] when the card may go out now, and
/// by the deferred-queue drainer when a held-back card's window has ended.
///
/// Records where the card landed so a later settle can retire it in place.
/// Capturing the message identity is best-effort on top of delivery: a
/// platform response that doesn't yield one (LINE always, or an unexpected
/// body) still counts as delivered — it only costs a later append instead of
/// an in-place edit.
pub(crate) async fn deliver_now(
    home_dir: &Path,
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    chat_id: &str,
    card: &DecisionCard<'_>,
) -> bool {
    // D-S1: a Telegram private-chat approval card also gets a `web_app`
    // button opening the Mini App detail view. `None` in every other case —
    // feature off, wrong channel/source, group chat, no https public URL —
    // and the keyboard below is then exactly what it was before the spike.
    let miniapp_url = crate::miniapp::approval_web_app_url(
        home_dir,
        card.source,
        channel,
        chat_id,
        card.decision_id,
    );
    match crate::channel_format::decision_markup_with_miniapp(
        channel,
        card.source,
        card.decision_id,
        miniapp_url.as_deref(),
    ) {
        Some(markup) => {
            // Button-capable channels get the link too, as a fallback for
            // when the buttons themselves fail to render or tap.
            let body = match card.link {
                Some(url) => format!("{}\n\n👉 {url}", card.body),
                None => card.body.to_string(),
            };
            match crate::channel_sender::send_with_markup(
                http, channel, token, chat_id, &body, markup,
            )
            .await
            {
                Ok(pushed) => {
                    if let Some(p) = &pushed {
                        crate::decision_message_store::record_card_message(
                            home_dir,
                            card.source.namespace(),
                            card.decision_id,
                            channel,
                            chat_id,
                            p,
                        );
                    }
                    true
                }
                Err(e) => {
                    tracing::warn!(
                        %channel, decision = %card.decision_id, error = %e,
                        "decision-notify: button push failed"
                    );
                    false
                }
            }
        }
        None => {
            let text = match card.link {
                Some(url) => format!("{}\n\n{}\n👉 {url}", card.body, card.no_button_hint),
                None => format!("{}\n\n{}", card.body, card.no_button_hint),
            };
            // WP1.6: Teams cards are plain text (no button codec), but the
            // sent activity id is still worth recording — quoted-replying to
            // the card is how Teams text-verdict decisions find it. Teams is
            // outside `channel_editable`, so collapse never tries to edit;
            // recording only enables the reverse lookup (same shape as LINE).
            if channel == "teams" {
                return match crate::msteams::send_text_to_conversation_with_id(
                    home_dir, chat_id, &text,
                )
                .await
                {
                    Ok(pushed_id) => {
                        if let Some(id) = pushed_id {
                            crate::decision_message_store::record_card_message(
                                home_dir,
                                card.source.namespace(),
                                card.decision_id,
                                channel,
                                chat_id,
                                &crate::decision_card::PushedMessage {
                                    edit_chat_id: chat_id.to_string(),
                                    message_id: id,
                                },
                            );
                        }
                        true
                    }
                    Err(e) => {
                        tracing::warn!(
                            %channel, decision = %card.decision_id, error = %e,
                            "decision-notify: teams plain push failed"
                        );
                        false
                    }
                };
            }
            crate::channel_sender::send_plain_text(home_dir, http, channel, token, chat_id, &text)
                .await
        }
    }
}

// ── Inbound routing ─────────────────────────────────────────────

/// The single entry every channel dispatcher calls for a button press.
///
/// Returns:
/// - `None` — not a decision button (the dispatcher falls through to its own
///   actions, e.g. `duduclaw:new_session`).
/// - `Some(Ok(ack))` — decision applied; `ack` is the zh-TW line to show.
/// - `Some(Err(msg))` — a refusal or error to show the presser.
///
/// Accepts both the unified encoding and every pre-unification one, so a card
/// already sitting in a channel keeps working (see
/// [`crate::decision_action::parse`]).
pub async fn route_press(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action_data: &str,
) -> Option<Result<String, String>> {
    let action = crate::decision_action::parse(action_data)?;
    Some(dispatch(home_dir, channel, channel_user_id, &action).await)
}

/// Apply an already-decoded decision against its owning store.
///
/// A **successful** apply is what gets recorded as an action for the
/// action-rate metric (P4-5) — a refused or stale press is a failed
/// interaction, not evidence the notification was worth sending.
pub(crate) async fn dispatch(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action: &DecisionAction,
) -> Result<String, String> {
    let outcome = dispatch_inner(home_dir, channel, channel_user_id, action).await;
    if outcome.is_ok() {
        crate::notify_stats::record_action(home_dir, notify_type(action.source), &action.id);
    }
    outcome
}

async fn dispatch_inner(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action: &DecisionAction,
) -> Result<String, String> {
    match action.source {
        DecisionSource::Goal => {
            crate::goal_notify::apply_needs_human(
                home_dir,
                channel,
                channel_user_id,
                &action.id,
                action.act,
            )
            .await
        }
        DecisionSource::Kickoff => {
            crate::goal_notify::apply_kickoff(
                home_dir,
                channel,
                channel_user_id,
                &action.id,
                action.approve(),
            )
            .await
        }
        DecisionSource::Approval => {
            crate::approval_notify::apply_decision(
                home_dir,
                channel,
                channel_user_id,
                &action.id,
                action.approve(),
            )
            .await
        }
        DecisionSource::Install => {
            crate::install_notify::apply_decision(
                home_dir,
                channel,
                channel_user_id,
                &action.id,
                action.approve(),
            )
            .await
        }
        DecisionSource::Autopilot => {
            crate::autopilot_notify::apply_pause(home_dir, channel, channel_user_id, &action.id)
                .await
        }
        DecisionSource::Digest => {
            crate::digest::apply_channel_feedback(
                home_dir,
                channel,
                channel_user_id,
                &action.id,
                action.act,
            )
            .await
        }
    }
}

/// The zh-TW verb pair a settled decision reports, shared by the inline
/// acknowledgement and the collapsed card so a person is told the same word
/// twice. `install`-class refusals read softer ("婉拒") than a high-risk
/// refusal ("拒絕").
pub(crate) fn settled_verb(
    source: DecisionSource,
    act: DecisionAct,
) -> crate::decision_card::DecisionVerb {
    use crate::decision_card::DecisionVerb;
    match (source, act) {
        (_, DecisionAct::Retry) => DecisionVerb::Retried,
        (_, DecisionAct::Done) => DecisionVerb::MarkedDone,
        (_, DecisionAct::Abort) => DecisionVerb::Abandoned,
        (_, DecisionAct::Pause) => DecisionVerb::Paused,
        (_, DecisionAct::Approve) => DecisionVerb::Approved,
        (_, DecisionAct::Takeover) => DecisionVerb::TakenOver,
        (_, DecisionAct::Up | DecisionAct::Down | DecisionAct::Changes) => {
            DecisionVerb::FeedbackRecorded
        }
        (DecisionSource::Install, DecisionAct::Deny) => DecisionVerb::DeclinedInstall,
        (_, DecisionAct::Deny) => DecisionVerb::Denied,
    }
}

// ── Reason vocabulary (W1-6, C4: "reason 是扇出第一公民") ──────────

/// The zh-TW emoji+phrase every decision card opens its first line with — one
/// glance at line 1 tells you WHICH of the five sources raised it, before
/// reading a word of the body. One definition, rendered everywhere a card is
/// composed (`goal_notify`/`approval_notify`/`install_notify`/
/// `autopilot_notify`); the dashboard Inbox's type-badge copy is expected to
/// read the same by eye (07 §6 H4) rather than importing this function.
pub fn reason_prefix(source: DecisionSource) -> &'static str {
    match source {
        DecisionSource::Goal => "🤔 自主任務等你決定",
        DecisionSource::Kickoff => "🚀 新任務要開工",
        DecisionSource::Approval => "⚠️ 高風險動作需要你同意",
        DecisionSource::Install => "📦 安裝申請",
        DecisionSource::Autopilot => "🔁 自動規則已暫停",
        DecisionSource::Digest => "🐾 你不在的時候",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── authorization ──────────────────────────────────────

    #[test]
    fn approvers_are_allowed_by_role() {
        assert_eq!(
            authorize_press(Some(UserRole::Admin), true, false),
            PressAuth::Allow
        );
        assert_eq!(
            authorize_press(Some(UserRole::Manager), true, false),
            PressAuth::Allow
        );
    }

    #[test]
    fn employee_press_is_refused_even_from_the_destination() {
        // Separation of duties: a mapped non-approver never decides, not even
        // in the chat the decision was delivered to.
        assert_eq!(
            authorize_press(Some(UserRole::Employee), true, true),
            PressAuth::DenyNotApprover
        );
        assert_eq!(
            authorize_press(Some(UserRole::Employee), false, true),
            PressAuth::DenyNotApprover
        );
    }

    #[test]
    fn unmapped_press_is_refused_when_approvers_exist() {
        assert_eq!(authorize_press(None, true, true), PressAuth::DenyUnknown);
        assert_eq!(authorize_press(None, true, false), PressAuth::DenyUnknown);
    }

    #[test]
    fn solo_operator_may_decide_from_the_delivered_destination() {
        assert_eq!(authorize_press(None, false, true), PressAuth::Allow);
        assert_eq!(authorize_press(None, false, false), PressAuth::DenyUnknown);
    }

    #[test]
    fn destination_match_requires_exact_channel_and_account() {
        let targets = vec![("telegram".to_string(), "555".to_string())];
        assert!(destination_matches_any(&targets, "telegram", "555"));
        assert!(!destination_matches_any(&targets, "slack", "555"));
        assert!(!destination_matches_any(&targets, "telegram", "666"));
        // No substring shortcuts.
        assert!(!destination_matches_any(&targets, "telegram", "5555"));
        assert!(!destination_matches_any(&[], "telegram", "555"));
    }

    #[test]
    fn group_delivery_is_not_a_back_door_for_its_members() {
        // Card delivered to group chat 555; a member whose own account id is
        // 999 presses. Matching the conversation would let every member
        // decide — it must not.
        let targets = vec![("telegram".to_string(), "555".to_string())];
        assert!(!destination_matches_any(&targets, "telegram", "999"));
        assert_eq!(
            authorize_press(
                None,
                false,
                destination_matches_any(&targets, "telegram", "999")
            ),
            PressAuth::DenyUnknown
        );
    }

    #[test]
    fn refusal_text_names_the_action_and_never_leaks_internals() {
        let no_right = refusal_text(PressAuth::DenyNotApprover, "核准");
        assert_eq!(no_right, "您沒有核准的權限");
        let unknown = refusal_text(PressAuth::DenyUnknown, "核准");
        assert!(unknown.contains("尚未連結儀表板身分"));
        assert!(refusal_text(PressAuth::Allow, "核准").is_empty());
    }

    // ── destinations ───────────────────────────────────────

    #[test]
    fn parse_origin_accepts_channel_sessions() {
        assert_eq!(
            parse_origin("telegram:12345"),
            Some(("telegram".into(), "12345".into()))
        );
        assert_eq!(
            parse_origin("telegram:12345:678"),
            Some(("telegram".into(), "12345".into()))
        );
        assert_eq!(
            parse_origin("discord:thread:999"),
            Some(("discord".into(), "999".into()))
        );
    }

    #[test]
    fn parse_origin_rejects_non_pushable_and_malformed() {
        assert_eq!(parse_origin("webchat:conn-1"), None);
        assert_eq!(parse_origin("webchat:conn#agent:a#conv:n"), None);
        assert_eq!(parse_origin("dashboard:alice"), None);
        assert_eq!(parse_origin("telegram"), None);
        assert_eq!(parse_origin("telegram:"), None);
        assert_eq!(parse_origin(""), None);
    }

    #[test]
    fn targets_prefer_the_origin_conversation() {
        let got = resolve_targets(
            Some(("telegram".into(), "555".into())),
            Some(("slack".into(), "C1".into())),
            vec![("discord".into(), "D1".into())],
        );
        assert_eq!(got, vec![("telegram".to_string(), "555".to_string())]);
    }

    #[test]
    fn targets_fall_back_to_agent_default_then_approvers() {
        let agent_only = resolve_targets(
            None,
            Some(("slack".into(), "C1".into())),
            vec![("discord".into(), "D1".into())],
        );
        assert_eq!(agent_only, vec![("slack".to_string(), "C1".to_string())]);

        let approvers = resolve_targets(
            None,
            None,
            vec![
                ("discord".into(), "D1".into()),
                ("line".into(), "U9".into()),
            ],
        );
        assert_eq!(approvers.len(), 2);

        assert!(resolve_targets(None, None, vec![]).is_empty());
    }

    #[test]
    fn targets_drop_blank_and_unpushable_and_dedupe() {
        let got = resolve_targets(
            None,
            None,
            vec![
                ("telegram".into(), "1".into()),
                ("telegram".into(), "1".into()),
                ("webchat".into(), "x".into()),
                ("telegram".into(), "  ".into()),
                ("".into(), "1".into()),
            ],
        );
        assert_eq!(got, vec![("telegram".to_string(), "1".to_string())]);
    }

    // ── settled vocabulary ─────────────────────────────────

    #[test]
    fn settled_verbs_follow_the_state_vocabulary() {
        use crate::decision_card::DecisionVerb;
        assert_eq!(
            settled_verb(DecisionSource::Goal, DecisionAct::Retry),
            DecisionVerb::Retried
        );
        assert_eq!(
            settled_verb(DecisionSource::Goal, DecisionAct::Done),
            DecisionVerb::MarkedDone
        );
        assert_eq!(
            settled_verb(DecisionSource::Goal, DecisionAct::Abort),
            DecisionVerb::Abandoned
        );
        assert_eq!(
            settled_verb(DecisionSource::Autopilot, DecisionAct::Pause),
            DecisionVerb::Paused
        );
        assert_eq!(
            settled_verb(DecisionSource::Kickoff, DecisionAct::Approve),
            DecisionVerb::Approved
        );
        // An install refusal reads softer than a high-risk refusal.
        assert_eq!(
            settled_verb(DecisionSource::Install, DecisionAct::Deny),
            DecisionVerb::DeclinedInstall
        );
        assert_eq!(
            settled_verb(DecisionSource::Approval, DecisionAct::Deny),
            DecisionVerb::Denied
        );
        assert_eq!(
            settled_verb(DecisionSource::Kickoff, DecisionAct::Deny),
            DecisionVerb::Denied
        );
        assert_eq!(
            settled_verb(DecisionSource::Goal, DecisionAct::Takeover),
            DecisionVerb::TakenOver
        );
    }

    // ── reason vocabulary (W1-6) ────────────────────────────────

    #[test]
    fn reason_prefix_covers_every_source_with_a_distinct_phrase() {
        let all = [
            DecisionSource::Goal,
            DecisionSource::Kickoff,
            DecisionSource::Approval,
            DecisionSource::Install,
            DecisionSource::Autopilot,
        ];
        let mut seen = std::collections::HashSet::new();
        for s in all {
            let p = reason_prefix(s);
            assert!(!p.is_empty());
            assert!(
                seen.insert(p),
                "reason_prefix must be distinct per source: {p}"
            );
        }
    }

    #[test]
    fn reason_prefix_matches_the_agreed_wording() {
        assert_eq!(reason_prefix(DecisionSource::Goal), "🤔 自主任務等你決定");
        assert_eq!(reason_prefix(DecisionSource::Kickoff), "🚀 新任務要開工");
        assert_eq!(
            reason_prefix(DecisionSource::Approval),
            "⚠️ 高風險動作需要你同意"
        );
        assert_eq!(reason_prefix(DecisionSource::Install), "📦 安裝申請");
        assert_eq!(
            reason_prefix(DecisionSource::Autopilot),
            "🔁 自動規則已暫停"
        );
    }

    // ── escalation level (W2-4) ────────────────────────────

    #[test]
    fn every_decision_source_has_a_level_and_the_split_is_the_documented_one() {
        use crate::notify_governance::NotifyLevel;
        assert_eq!(notify_level(DecisionSource::Goal), NotifyLevel::Act);
        assert_eq!(notify_level(DecisionSource::Approval), NotifyLevel::Act);
        assert_eq!(notify_level(DecisionSource::Install), NotifyLevel::Act);
        assert_eq!(notify_level(DecisionSource::Kickoff), NotifyLevel::Confirm);
        assert_eq!(
            notify_level(DecisionSource::Autopilot),
            NotifyLevel::Confirm
        );
    }

    #[test]
    fn l3_decision_sources_can_never_be_held_by_quiet_hours() {
        for s in [
            DecisionSource::Goal,
            DecisionSource::Approval,
            DecisionSource::Install,
        ] {
            assert!(
                !notify_level(s).is_suppressible(),
                "{s:?} blocks work until a person answers; it must never be deferred"
            );
        }
    }

    #[test]
    fn notify_types_are_distinct_and_namespaced_per_source() {
        let all = [
            DecisionSource::Goal,
            DecisionSource::Kickoff,
            DecisionSource::Approval,
            DecisionSource::Install,
            DecisionSource::Autopilot,
        ];
        let mut seen = std::collections::HashSet::new();
        for s in all {
            let t = notify_type(s);
            assert!(
                t.starts_with("decision."),
                "stats buckets are namespaced: {t}"
            );
            assert!(
                seen.insert(t),
                "one bucket per source, so P4-5 is answerable per source: {t}"
            );
        }
    }

    #[tokio::test]
    async fn an_l2_card_inside_quiet_hours_is_queued_with_its_buttons_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let window = crate::notify_governance::tests::window_covering_now();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("[notify]\nquiet_hours = \"{window}\"\n"),
        )
        .unwrap();

        let card = DecisionCard {
            source: DecisionSource::Kickoff,
            decision_id: "apv-123",
            body: "🚀 新任務要開工",
            link: Some("http://localhost:18789/inbox?item=apv-123"),
            no_button_hint: "請至儀表板同意或拒絕。",
        };
        let http = reqwest::Client::new();
        let outcome = deliver_outcome(dir.path(), &http, "telegram", "tok", "555", &card).await;
        assert_eq!(outcome, DeliverOutcome::Deferred);

        let queued = crate::notify_governance::take_due(
            dir.path(),
            chrono::Utc::now() + chrono::Duration::hours(2),
        );
        assert_eq!(queued.len(), 1);
        let n = &queued[0];
        assert_eq!(n.kind, crate::notify_governance::NoticeKind::Decision);
        // Everything the drainer needs to re-render the card with its buttons.
        assert_eq!(n.decision_source.as_deref(), Some("kick"));
        assert_eq!(n.decision_id.as_deref(), Some("apv-123"));
        assert_eq!(n.text, "🚀 新任務要開工");
        assert_eq!(
            n.link.as_deref(),
            Some("http://localhost:18789/inbox?item=apv-123")
        );
        assert_eq!(n.no_button_hint.as_deref(), Some("請至儀表板同意或拒絕。"));
        assert_eq!(
            DecisionSource::from_token(n.decision_source.as_deref().unwrap()),
            Some(DecisionSource::Kickoff)
        );
    }

    #[tokio::test]
    async fn no_quiet_window_means_no_card_is_ever_queued() {
        let dir = tempfile::tempdir().unwrap();
        let card = DecisionCard {
            source: DecisionSource::Kickoff,
            decision_id: "apv-1",
            body: "x",
            link: None,
            no_button_hint: "y",
        };
        let http = reqwest::Client::new();
        // No config.toml ⇒ no window ⇒ it takes the send path (which fails
        // against a bogus token, and that is fine — the assertion is that
        // nothing was queued).
        let _ = deliver_outcome(dir.path(), &http, "line", "tok", "U1", &card).await;
        assert!(
            crate::notify_governance::take_due(
                dir.path(),
                chrono::Utc::now() + chrono::Duration::hours(2)
            )
            .is_empty()
        );
    }

    // ── inbound routing ────────────────────────────────────

    #[tokio::test]
    async fn a_successful_press_records_an_action_and_a_refused_one_does_not() {
        // Action rate must count decisions people actually settled, not
        // presses that bounced off a fail-closed store.
        let dir = tempfile::tempdir().unwrap();
        let out = route_press(
            dir.path(),
            "telegram",
            "u1",
            "duduclaw:decide:apv:ok:missing",
        )
        .await;
        assert!(out.unwrap().is_err(), "a missing row must refuse");
        assert!(
            crate::notify_stats::stats(dir.path(), 30).is_empty(),
            "a refused press is a failed interaction, not evidence the notification worked"
        );
    }

    #[tokio::test]
    async fn route_press_ignores_non_decision_actions() {
        let dir = tempfile::tempdir().unwrap();
        for data in [
            "garbage",
            "duduclaw:new_session",
            "duduclaw:voice_toggle",
            "",
        ] {
            assert!(
                route_press(dir.path(), "telegram", "u1", data)
                    .await
                    .is_none(),
                "must not claim {data}"
            );
        }
    }

    #[tokio::test]
    async fn route_press_claims_both_unified_and_legacy_encodings() {
        // "Claims" = returns Some; the inner Err here is the expected
        // fail-closed answer for a decision that does not exist.
        let dir = tempfile::tempdir().unwrap();
        for data in [
            "duduclaw:decide:apv:ok:missing",
            "duduclaw:approval_ok:missing",
            "duduclaw:decide:auto:pause:missing",
            "duduclaw:autopilot_pause:missing",
        ] {
            let out = route_press(dir.path(), "telegram", "u1", data).await;
            assert!(out.is_some(), "must claim {data}");
            assert!(
                out.unwrap().is_err(),
                "a missing row must refuse, not succeed: {data}"
            );
        }
    }
}

/// A verified inbound adapter supplies all four identity dimensions. The legacy
/// route is intentionally unable to decide bound requests.
pub async fn route_bound_press(
    home: &Path,
    context: &crate::approval::DecisionContext,
    data: &str,
) -> Option<Result<String, String>> {
    let action = crate::decision_action::parse(data)?;
    if action.source != DecisionSource::Approval {
        return route_press(home, &context.channel, &context.principal_id, data).await;
    }
    Some(apply_bound_request(home, context, &action.id, Some(action.approve()), None).await)
}

/// Bounded explicit-ID text commands, handled before model dispatch. Bare yes
/// and A/B are data, never an implicit selection of the latest pending request.
///
/// `None` unless the text is a strict decision command (verb + complete
/// request UUID, `channel_decision_route::parse_strict_decision`): a verb
/// without a valid id is ordinary conversation and is never answered here
/// (F4, review M1).
pub async fn route_bound_text(
    home: &Path,
    context: &crate::approval::DecisionContext,
    text: &str,
) -> Option<Result<String, String>> {
    let command = crate::channel_decision_route::parse_strict_decision(text)?;
    if command.is_answer() {
        let Some(answer) = command.rest else {
            return Some(Err("請在請求編號後填寫答案。".into()));
        };
        Some(
            apply_bound_request(
                home,
                context,
                &command.id,
                None,
                Some(serde_json::json!(answer)),
            )
            .await,
        )
    } else {
        if command.rest.is_some() {
            return Some(Err("核准指令包含多餘內容，請重新確認編號。".into()));
        }
        Some(apply_bound_request(home, context, &command.id, Some(command.approves()), None).await)
    }
}
async fn apply_bound_request(
    home: &Path,
    context: &crate::approval::DecisionContext,
    id: &str,
    approve: Option<bool>,
    answer: Option<serde_json::Value>,
) -> Result<String, String> {
    use crate::channel_decision_route::DECISION_REFUSED;
    let broker = crate::approval::ApprovalBroker::open(home)?;
    let request = crate::approval::ApprovalId::from(id.to_string());
    // Unknown, unreadable and foreign requests read the same (review L2).
    let rec = match broker.get(&request).await {
        Ok(Some(rec)) => rec,
        Ok(None) => return Err(DECISION_REFUSED.into()),
        Err(e) => {
            tracing::warn!(error = %e, "bound decision: request row unreadable");
            return Err(DECISION_REFUSED.into());
        }
    };
    if rec.binding.is_none() {
        if answer.is_some() {
            return Err("這筆舊請求不是可回答的問題".into());
        }
        return crate::approval_notify::apply_decision(
            home,
            &context.channel,
            &context.principal_id,
            id,
            approve.unwrap_or(false),
        )
        .await;
    }
    context
        .validate()
        .map_err(|_| DECISION_REFUSED.to_string())?;
    let binding = rec.binding.as_ref().ok_or("缺少核准身分")?;
    if &binding.decision_context != context {
        return Err(DECISION_REFUSED.into());
    }
    let (role, identity_active) =
        bound_decision_authority(home, &context.channel, &context.principal_id)?;
    if rec.request_kind == crate::approval::RequestKind::Question {
        let answer = answer.ok_or("此請求只收集答案，按同意不會授權動作。")?;
        broker.answer_question(&request, context, answer).await?;
        return Ok("已記錄答案；尚未授權任何動作。".into());
    }
    let auth = authorize_press(role, identity_active, true);
    if auth != PressAuth::Allow {
        return Err(refusal_text(auth, "核准"));
    }
    broker
        .decide_bound(&request, context, approve.ok_or("請使用確認或取消指令")?)
        .await?;
    Ok(if approve == Some(true) {
        "已同意；執行結果仍待核對。"
    } else {
        "已拒絕，動作不會執行。"
    }
    .into())
}

/// Scope is constructed from authenticated adapter metadata, never a client
/// flag that claims a direct message to bypass a group/guild whitelist.
#[derive(Clone, Copy)]
pub(crate) struct DecisionAccessScope<'a> {
    pub channel_id: Option<&'a str>,
    pub guild_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
}
pub(crate) async fn read_trusted_channel_access(
    ctx: &crate::channel_reply::ReplyContext,
    context: &crate::approval::DecisionContext,
    scope: DecisionAccessScope<'_>,
) -> Result<crate::channel_reply::UserAccessPolicy, String> {
    context.validate()?;
    if scope.session_id == Some("") {
        return Err("無法確認此決策的頻道種類；請使用完整頻道資訊的互動或文字指令。".into());
    }
    let setting_scope = scope.guild_id.unwrap_or("global");
    // Decide from this successful authoritative SQL read, not subsequent
    // cache lookups which can turn a missing/read-failed row into allow-all.
    let rows = ctx
        .channel_settings
        .refresh_channel_snapshot(&context.channel, setting_scope)
        .await
        .map_err(|_| "目前頻道權限無法確認".to_string())?;
    let value = |sc: &str, key: &str| {
        rows.iter()
            .find(|(scope, k, _)| scope == sc && k == key)
            .map(|(_, _, v)| v.as_str())
    };
    let list = |raw: Option<&str>| -> Result<Vec<String>, String> {
        raw.map(serde_json::from_str::<Vec<String>>)
            .transpose()
            .map_err(|_| "目前頻道權限資料無法確認".to_string())
            .map(Option::unwrap_or_default)
    };
    if let Some(guild) = scope.guild_id {
        let allowed = list(value("global", "allowed_guilds"))?;
        if !allowed.is_empty() && !allowed.iter().any(|id| id == guild) {
            return Err("此伺服器目前未獲授權".into());
        }
    }
    if let Some(channel) = scope.channel_id {
        let allowed = list(
            value(setting_scope, "allowed_channels")
                .or_else(|| value("global", "allowed_channels")),
        )?;
        if !allowed.is_empty() && !allowed.iter().any(|id| id == channel) {
            return Err("此頻道目前未獲授權".into());
        }
    }
    let allowed = list(value("global", "allowed_users"))?;
    let blocked = list(value("global", "blocked_users"))?;
    let pairing = match value("global", "require_pairing") {
        None | Some("false") => false,
        Some("true") => true,
        _ => return Err("目前配對權限無法確認".into()),
    };
    Ok(crate::channel_reply::UserAccessPolicy {
        allowed: (!allowed.is_empty()).then_some(allowed),
        blocked,
        require_pairing: pairing,
    })
}
pub(crate) async fn check_trusted_decision_access(
    ctx: &crate::channel_reply::ReplyContext,
    context: &crate::approval::DecisionContext,
    scope: DecisionAccessScope<'_>,
    _text: &str,
) -> Result<(), String> {
    let policy = read_trusted_channel_access(ctx, context, scope).await?;
    let session = format!("{}:{}", context.channel, context.conversation_id);
    match ctx
        .access_control
        .check_access_dual(
            &context.principal_id,
            scope.session_id.unwrap_or(&session),
            policy.allowed.as_deref(),
            &policy.blocked,
            policy.require_pairing,
        )
        .await
    {
        crate::access_control::AccessDecision::Allowed => {}
        _ => return Err("此使用者目前未獲授權".into()),
    }
    Ok(())
}
pub(crate) async fn route_verified_bound_press(
    ctx: &crate::channel_reply::ReplyContext,
    context: &crate::approval::DecisionContext,
    data: &str,
    scope: DecisionAccessScope<'_>,
) -> Option<Result<String, String>> {
    let action = crate::decision_action::parse(data)?;
    if action.source == DecisionSource::Digest {
        // P9: a digest feedback press passes the same channel access check
        // as a bound decision (allowed guilds / channels / users, pairing)
        // before anything is read; the press itself is then authorized
        // against the presser's verified dashboard account.
        if context.validate().is_err()
            || check_trusted_decision_access(ctx, context, scope, data)
                .await
                .is_err()
        {
            return Some(Err(crate::channel_decision_route::DECISION_REFUSED.into()));
        }
    }
    if action.source == DecisionSource::Approval {
        // Same order as the text lane (F5-C, review F4-L6): the channel
        // access check runs before the request row is read, for every
        // approval press, so an unknown id and someone else's id cost the
        // same work. The result is applied only to bound rows; legacy
        // (unbound) cards keep their shipped authorization below.
        let access_ok = check_trusted_decision_access(ctx, context, scope, data)
            .await
            .is_ok();
        let broker = match crate::approval::ApprovalBroker::open(&ctx.home_dir) {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        };
        let row = match broker
            .get(&crate::approval::ApprovalId::from(action.id.clone()))
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                return Some(Err(crate::channel_decision_route::DECISION_REFUSED.into()));
            }
            Err(e) => {
                tracing::warn!(error = %e, "bound press: request row unreadable");
                return Some(Err(crate::channel_decision_route::DECISION_REFUSED.into()));
            }
        };
        if row.binding.is_some() && !access_ok {
            return Some(Err(crate::channel_decision_route::DECISION_REFUSED.into()));
        }
        // Only the successfully parsed persisted row can identify a legacy
        // domain request. Corrupt/non-NULL bindings never downgrade here.
    }
    route_bound_press(&ctx.home_dir, context, data).await
}

/// Strict request replies are control traffic, processed by the authenticated
/// receiver before ordinary conversation FIFO execution can wait for them.
///
/// `None` (the message continues to the normal pipeline, unchanged) unless
/// the text is a strict decision command: verb + complete request UUID
/// (`channel_decision_route::parse_strict_decision`). A verb-first message
/// without one — 「確認」 answering "shall I send it?", "approve the Q3
/// budget", a WP1.6 reply to an old card — is conversation (F4, review
/// M1/M2).
///
/// Every refusal decided before the sender is shown to be the person the
/// request is bound to returns the same sentence
/// (`channel_decision_route::DECISION_REFUSED`), and the channel access check
/// runs before the request row is read, so the reply reveals neither whether
/// an id exists nor whose it is (review L2).
pub(crate) async fn route_trusted_decision_fastlane_with_scope(
    ctx: &crate::channel_reply::ReplyContext,
    context: &crate::approval::DecisionContext,
    text: &str,
    scope: DecisionAccessScope<'_>,
) -> Option<Result<String, String>> {
    use crate::channel_decision_route::DECISION_REFUSED;
    let command = crate::channel_decision_route::parse_strict_decision(text)?;
    let refused = || Some(Err(DECISION_REFUSED.to_string()));
    if context.validate().is_err() {
        return refused();
    }
    if check_trusted_decision_access(ctx, context, scope, text)
        .await
        .is_err()
    {
        return refused();
    }
    let broker = match crate::approval::ApprovalBroker::open(&ctx.home_dir) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "decision fastlane: approvals store unavailable");
            return Some(Err("目前無法讀取核准資料，請稍後再試。".into()));
        }
    };
    let row = match broker
        .get(&crate::approval::ApprovalId::from(command.id.clone()))
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return refused(),
        Err(e) => {
            tracing::warn!(error = %e, "decision fastlane: request row unreadable");
            return refused();
        }
    };
    let Some(binding) = &row.binding else {
        return refused();
    };
    if &binding.decision_context != context {
        return refused();
    }
    // From here the sender is the person the request is bound to, in the same
    // account and conversation, so specific answers reveal nothing new.
    if row.status != crate::approval::ApprovalStatus::Pending {
        return Some(Err("請求已決定或失效。".into()));
    }
    route_bound_text(&ctx.home_dir, context, text).await
}

struct ChannelWork {
    key: String,
    future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    _slot: tokio::sync::OwnedSemaphorePermit,
}
/// A bounded conversation scheduler: four active conversations, one active
/// job per conversation, and at most 32 accepted jobs including queued work.
/// Dropping the receiver owner cancels all jobs and their trusted scopes.
pub(crate) struct BoundedChannelExecutor {
    sender: tokio::sync::mpsc::Sender<ChannelWork>,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    dispatcher: tokio::task::JoinHandle<()>,
}
impl BoundedChannelExecutor {
    pub(crate) fn new() -> Self {
        use futures_util::FutureExt;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<ChannelWork>(32);
        let dispatcher = tokio::spawn(async move {
            let mut pending = std::collections::VecDeque::<ChannelWork>::new();
            let mut active = std::collections::HashSet::<String>::new();
            let mut jobs = tokio::task::JoinSet::new();
            loop {
                while jobs.len() < 4 {
                    let Some(pos) = pending.iter().position(|work| !active.contains(&work.key))
                    else {
                        break;
                    };
                    let work = pending.remove(pos).unwrap();
                    active.insert(work.key.clone());
                    jobs.spawn(async move {
                        let key = work.key;
                        let _ = std::panic::AssertUnwindSafe(work.future)
                            .catch_unwind()
                            .await;
                        drop(work._slot);
                        key
                    });
                }
                if receiver.is_closed() && pending.is_empty() && jobs.is_empty() {
                    break;
                }
                tokio::select! {
                    Some(work) = receiver.recv() => pending.push_back(work),
                    Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                        if let Ok(key) = result {
                            active.remove(&key);
                        }
                    },
                    else => break,
                }
            }
        });
        Self {
            sender,
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(32)),
            dispatcher,
        }
    }
    pub(crate) fn try_submit<F: std::future::Future<Output = ()> + Send + 'static>(
        &self,
        key: String,
        future: F,
    ) -> Result<(), String> {
        if key.is_empty() || key.len() > 1024 {
            return Err("invalid conversation key".into());
        }
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "channel work queue full")?;
        self.sender
            .try_send(ChannelWork {
                key,
                future: Box::pin(future),
                _slot: slot,
            })
            .map_err(|_| "channel work queue unavailable".into())
    }
}
impl Drop for BoundedChannelExecutor {
    fn drop(&mut self) {
        self.dispatcher.abort();
    }
}

#[cfg(test)]
pub(crate) mod native_loop_fixture {
    use super::*;
    use crate::computer_use::{ComputerAction, ComputerUseError};
    use crate::computer_use_orchestrator::{MaskedScreenshot, OrchestratorControl};
    use crate::computer_use_sessions::{ComputerUseSessions, SessionBackend};
    use std::sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    };
    type Job = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    type Pause = (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
    fn pauses() -> &'static Mutex<std::collections::HashMap<String, Pause>> {
        static PAUSES: OnceLock<Mutex<std::collections::HashMap<String, Pause>>> = OnceLock::new();
        PAUSES.get_or_init(Default::default)
    }
    pub(crate) fn pause_before_inner(token: &str) -> Pause {
        let pause = (
            Arc::new(tokio::sync::Notify::new()),
            Arc::new(tokio::sync::Notify::new()),
        );
        pauses().lock().unwrap().insert(token.into(), pause.clone());
        pause
    }
    pub(crate) async fn wait_before_inner(token: &str) {
        let pause = pauses().lock().unwrap().remove(token);
        if let Some((entered, resume)) = pause {
            entered.notify_one();
            resume.notified().await;
        }
    }

    fn jobs() -> &'static Mutex<
        std::collections::HashMap<String, std::collections::VecDeque<(Option<String>, Job)>>,
    > {
        static JOBS: OnceLock<
            Mutex<
                std::collections::HashMap<
                    String,
                    std::collections::VecDeque<(Option<String>, Job)>,
                >,
            >,
        > = OnceLock::new();
        JOBS.get_or_init(Default::default)
    }
    pub(crate) fn take_job(token: &str) -> Option<Job> {
        jobs()
            .lock()
            .unwrap()
            .get_mut(token)?
            .pop_front()
            .map(|(_, job)| job)
    }
    pub(crate) fn install_job<F: std::future::Future<Output = ()> + Send + 'static>(
        token: &str,
        work: F,
    ) -> JobGuard {
        jobs()
            .lock()
            .unwrap()
            .entry(token.to_owned())
            .or_default()
            .push_back((None, Box::pin(work)));
        JobGuard(token.to_owned())
    }
    pub(crate) fn take_job_for(token: &str, text: &str) -> Option<Job> {
        let mut map = jobs().lock().unwrap();
        let queue = map.get_mut(token)?;
        let pos = queue
            .iter()
            .position(|(name, _)| name.as_deref().is_none_or(|name| name == text))?;
        queue.remove(pos).map(|(_, job)| job)
    }
    pub(crate) fn install_named_job<F: std::future::Future<Output = ()> + Send + 'static>(
        token: &str,
        text: &str,
        work: F,
    ) -> JobGuard {
        jobs()
            .lock()
            .unwrap()
            .entry(token.to_owned())
            .or_default()
            .push_back((Some(text.to_owned()), Box::pin(work)));
        JobGuard(token.to_owned())
    }
    pub(crate) struct JobGuard(String);
    impl Drop for JobGuard {
        fn drop(&mut self) {
            jobs().lock().unwrap().remove(&self.0);
        }
    }
    pub(crate) async fn pending_native_request(
        fixture: &NativeCuFixture,
        context: crate::approval::DecisionContext,
    ) -> crate::approval::ApprovalId {
        use crate::approval::*;
        let payload = serde_json::json!({"action":"native-acl-fixture"});
        let binding = ExecutionBinding {
            schema_version: 1,
            run_id: uuid::Uuid::new_v4().to_string(),
            run_origin_kind: "workflow".into(),
            actor_principal: "alice".into(),
            decision_context: context,
            task_id: None,
            task_revision: None,
            task_snapshot_hash: None,
            payload_hash: payload_hash(&payload),
            policy_revision: policy_revision(fixture.home.path(), "alice").unwrap(),
            cwd: None,
            environment_hash: payload_hash(&serde_json::json!({"fixture":true})),
            file_hashes: Default::default(),
            expires_at: (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
            resume_handler: "computer_reobserve_v1".into(),
            resume_version: 1,
        };
        let broker = ApprovalBroker::open(fixture.home.path()).unwrap();
        let id = broker
            .request_bound(
                RequestKind::Approval,
                "alice",
                "native ACL",
                payload,
                binding,
            )
            .await
            .unwrap();
        broker
            .prepare_operation(&id, "native-acl", None)
            .await
            .unwrap();
        id
    }
    // Prime the cache with access, then revoke through a second authoritative
    // SQL writer. The adapter must not keep the previously allowed snapshot.
    pub(crate) async fn revoke_native_access(
        fixture: &NativeCuFixture,
        channel: &str,
        scope: &str,
        mode: &str,
    ) {
        if mode == "binding" {
            let db = rusqlite::Connection::open(fixture.home.path().join("approvals.db")).unwrap();
            db.execute("UPDATE approvals SET binding_json='{'", [])
                .unwrap();
            return;
        }
        let (key, before, after) = match mode {
            "user" => ("allowed_users", r#"["H1","11"]"#, r#"["OTHER"]"#),
            "channel" => ("allowed_channels", r#"["C1","-42"]"#, r#"["OTHER"]"#),
            "guild" => ("allowed_guilds", r#"["G1"]"#, r#"["OTHER"]"#),
            "blocked" => ("blocked_users", "[]", r#"["H1","11"]"#),
            "pairing" => ("require_pairing", "false", "true"),
            "corrupt" => ("blocked_users", "[]", "invalid-json"),
            "db" | "allow" => ("blocked_users", "[]", "[]"),
            _ => panic!("unknown revocation fixture"),
        };
        let setting_scope = if mode == "channel" { scope } else { "global" };
        fixture
            .ctx
            .channel_settings
            .set(channel, setting_scope, key, before)
            .await
            .unwrap();
        assert!(
            fixture
                .ctx
                .channel_settings
                .get(channel, setting_scope, key)
                .await
                .is_some()
        );
        let db = rusqlite::Connection::open(fixture.home.path().join("sessions.db")).unwrap();
        if mode == "db" {
            db.execute("DROP TABLE channel_settings", []).unwrap();
        } else {
            db.execute(
                "UPDATE channel_settings SET value=?1 WHERE channel_type=?2 AND scope_id=?3 AND key=?4",
                rusqlite::params![after, channel, setting_scope, key]
            )
            .unwrap();
        }
    }
    pub(crate) async fn assert_native_pending(
        fixture: &NativeCuFixture,
        id: &crate::approval::ApprovalId,
    ) {
        use crate::approval::*;
        let broker = ApprovalBroker::open(fixture.home.path()).unwrap();
        let raw = rusqlite::Connection::open(fixture.home.path().join("approvals.db")).unwrap();
        let status: String = raw
            .query_row(
                "SELECT status FROM approvals WHERE id=?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");
        // Corrupt bindings must be refused, never read as unbound legacy.
        if let Ok(row) = broker.get(id).await {
            assert_eq!(row.unwrap().status, ApprovalStatus::Pending);
        }

        let operations = broker.list_operations().await.unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].state, OperationState::Prepared);
        assert_eq!(fixture.executed.load(Ordering::SeqCst), 0);
    }
    pub(crate) async fn assert_native_approved(
        fixture: &NativeCuFixture,
        id: &crate::approval::ApprovalId,
    ) {
        use crate::approval::*;
        let broker = ApprovalBroker::open(fixture.home.path()).unwrap();
        assert_eq!(
            broker.get(id).await.unwrap().unwrap().status,
            ApprovalStatus::Approved
        );
        assert_eq!(
            broker.list_operations().await.unwrap()[0].state,
            OperationState::Prepared
        );
        assert_eq!(
            fixture.executed.load(Ordering::SeqCst),
            0,
            "approval is not an execution receipt"
        );
    }
    pub(crate) struct NativeCuFixture {
        pub home: tempfile::TempDir,
        pub ctx: Arc<crate::channel_reply::ReplyContext>,
        pub manager: Arc<ComputerUseSessions>,
        pub executed: Arc<AtomicUsize>,
    }
    struct Backend {
        executed: Arc<AtomicUsize>,
        control: Arc<OrchestratorControl>,
    }
    #[async_trait::async_trait]
    impl SessionBackend for Backend {
        async fn start(&mut self) -> Result<(), ComputerUseError> {
            Ok(())
        }
        async fn register(&mut self, _: &str) -> Result<(), ComputerUseError> {
            Ok(())
        }
        async fn stop(&mut self) {}
        async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
            use base64::Engine;
            let mut buffer = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(image::RgbaImage::new(2, 2))
                .write_to(&mut buffer, image::ImageFormat::Png)
                .unwrap();
            Ok(MaskedScreenshot {
                png_base64: base64::engine::general_purpose::STANDARD.encode(buffer.into_inner()),
                full_mask: None,
            })
        }
        async fn window_title(&self) -> Result<String, String> {
            Ok("Bank Login".into())
        }
        async fn execute(&self, _: &ComputerAction) -> Result<(), ComputerUseError> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn control(&self) -> Arc<OrchestratorControl> {
            self.control.clone()
        }
    }
    impl NativeCuFixture {
        pub(crate) async fn new(token: &str) -> Self {
            let home = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
            std::fs::write(
                home.path().join("agents/alice/agent.toml"),
                "[agent]\nname='alice'\n[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\nallowed_apps=['Trusted Window']\n"
            )
            .unwrap();
            std::fs::write(
                home.path().join("config.toml"),
                format!("[channels]\ntelegram_bot_token='{token}'\n"),
            )
            .unwrap();
            let registry = duduclaw_agent::AgentRegistry::new(home.path().join("agents"));
            let sessions = Arc::new(
                crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap(),
            );
            let (event_tx, _) = tokio::sync::broadcast::channel(16);
            let ctx = Arc::new(crate::channel_reply::ReplyContext::new(
                Arc::new(tokio::sync::RwLock::new(registry)),
                home.path().to_path_buf(),
                sessions,
                Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
                event_tx,
            ));
            let executed = Arc::new(AtomicUsize::new(0));
            let count = executed.clone();
            let factory: crate::computer_use_sessions::BackendFactory = Arc::new(move |_, _, _| {
                Box::new(Backend {
                    executed: count.clone(),
                    control: Arc::new(OrchestratorControl::new()),
                })
            });
            let manager = Arc::new(ComputerUseSessions::with_parts(
                home.path().to_path_buf(),
                factory,
                std::time::Duration::from_secs(120),
            ));
            Self {
                home,
                ctx,
                manager,
                executed,
            }
        }
        pub(crate) fn work(
            &self,
            turn: &str,
        ) -> impl std::future::Future<Output = ()> + Send + 'static {
            let manager = self.manager.clone();
            let turn = turn.to_owned();
            async move {
                let context = crate::approval::CURRENT_DECISION_CONTEXT
                    .try_with(Clone::clone)
                    .unwrap()
                    .unwrap();
                let guard = crate::computer_use_sessions::turns::register(
                    "alice",
                    &turn,
                    &format!("{}:{}", context.channel, context.conversation_id),
                )
                .unwrap();
                let started = manager
                    .start(
                        "alice",
                        crate::computer_use_sessions::StartRequest {
                            turn_id: Some(turn.clone()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                let id = started["session_id"].as_str().unwrap();
                manager
                    .action(
                        "alice",
                        Some(id),
                        Some(&turn),
                        &crate::computer_use_sessions::actions::ActionRequest::Type {
                            text: "private marker".into(),
                        },
                    )
                    .await
                    .unwrap();
                manager.stop("alice", Some(id)).await.unwrap();
                drop(guard);
            }
        }
        pub(crate) async fn pending_id(&self) -> String {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if let Ok(b) = crate::approval::ApprovalBroker::open(self.home.path()) {
                        if let Ok(rows) = b.list_pending(Some("alice")).await {
                            if let Some(row) = rows.first() {
                                return row.id.as_str().to_owned();
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("actual CU never reached persisted approval wait")
        }
        pub(crate) async fn wait_executed(&self) {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if self.executed.load(Ordering::SeqCst) == 1 {
                        if let Ok(broker) = crate::approval::ApprovalBroker::open(self.home.path())
                        {
                            if let Ok(rows) = broker.list_operations().await {
                                if rows.iter().any(|row| {
                                    row.state == crate::approval::OperationState::Succeeded
                                }) {
                                    break;
                                }
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("receiver did not consume subsequent confirmation while CU was awaiting it");
        }
    }
}

#[cfg(test)]
mod bounded_execution_tests {
    use super::*;
    #[tokio::test]
    async fn accepted_work_is_bounded_and_panics_release_conversation_fifo() {
        let executor = BoundedChannelExecutor::new();
        for _ in 0..32 {
            executor
                .try_submit("blocked".into(), std::future::pending())
                .unwrap();
        }
        assert!(executor.try_submit("other".into(), async {}).is_err());
        drop(executor);
        let executor = BoundedChannelExecutor::new();
        executor
            .try_submit("same".into(), async {
                panic!("injected normal worker error");
            })
            .unwrap();
        let (done, received) = tokio::sync::oneshot::channel();
        executor
            .try_submit("same".into(), async move {
                done.send(()).unwrap();
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), received)
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    async fn dropping_receiver_cancels_active_work_and_clears_live_turn_target() {
        let executor = BoundedChannelExecutor::new();
        let context = crate::approval::DecisionContext {
            channel: "slack".into(),
            account_id: "bot".into(),
            principal_id: "user".into(),
            conversation_id: "channel:thread".into(),
        };
        let target = crate::approval::TrustedReplyTarget::new(
            context,
            "secret".into(),
            "channel".into(),
            Some("thread".into()),
        );
        let (ready, received) = tokio::sync::oneshot::channel();
        executor
            .try_submit(
                "channel".into(),
                crate::approval::scope_trusted_reply(target, async move {
                    let _guard = crate::computer_use_sessions::turns::register(
                        "executor-cancelled",
                        "turn",
                        "slack:channel:thread",
                    )
                    .unwrap();
                    ready.send(()).unwrap();
                    std::future::pending::<()>().await;
                }),
            )
            .unwrap();
        received.await.unwrap();
        assert!(
            crate::computer_use_sessions::turns::target_for("executor-cancelled", "turn").is_some()
        );
        drop(executor);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while crate::computer_use_sessions::turns::target_for("executor-cancelled", "turn")
                .is_some()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
