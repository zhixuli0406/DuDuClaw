//! Shared "decision card" collapse: once a channel button press settles an
//! inbound HITL decision (goal-loop needs_human/kickoff, generic
//! `ApprovalBroker` approve/deny, install-request approve/deny), the pushed
//! message that carried the buttons is rewritten in place to a short
//! one-line result and its buttons are removed — so a settled decision never
//! leaves a stale, still-clickable card behind.
//!
//! ## Shape
//!
//! - [`DecisionVerb`] + [`result_line`] render the shared settled-state
//!   vocabulary ("已同意" / "已拒絕" / "已婉拒" / "已重試" / "已標記完成" /
//!   "已放棄"), optionally attributed to a resolved decider name and always
//!   timestamped.
//! - [`collapse_body`] combines a freshly-regenerated one-line summary (built
//!   from the underlying record, not parsed out of the original push text —
//!   see the note on [`collapse_body`]) with the result line.
//! - [`edit_channel_message`] is the actual per-platform edit call —
//!   Telegram `editMessageText`, Slack `chat.update`, Discord
//!   `PATCH /channels/{id}/messages/{id}` — all bot-token authenticated so
//!   they work outside the short-lived interaction-response window a live
//!   callback/interaction payload is bound to.
//! - [`collapse_all`] is the orchestrator each notify module's inbound decide
//!   handler spawns (fire-and-forget — an edit is cosmetic and must never
//!   delay or fail the decision that already landed): look up EVERY stored
//!   message reference for that decision (written by the outbound push, see
//!   `decision_message_store`), edit each in place, and when none could be
//!   edited degrade to appending a bare one-line result instead. All of them,
//!   not just the presser's copy — a decision routinely fans out to several
//!   approvers, and the copies nobody pressed are exactly the stale cards
//!   this module exists to remove.
//! - LINE has no message-edit capability at all (confirmed against the
//!   platform's Messaging API — a `postback` never carries an editable
//!   message id, and there is no `editMessage` endpoint), so
//!   [`channel_editable`] excludes it; LINE's existing postback reply
//!   already IS the "one-line result" this module produces for other
//!   channels, so no LINE-specific code path is added here.

use std::path::Path;

use serde_json::json;

use crate::decision_message_store;

/// The zh-TW verb + emoji for a settled decision. The one vocabulary every
/// decision surface draws from: approve → 已同意, high-risk deny → 已拒絕 (heavier tone),
/// install-class deny → 已婉拒 (softer tone), goal-loop retry/done/abort
/// have their own verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionVerb {
    Approved,
    Denied,
    DeclinedInstall,
    Retried,
    MarkedDone,
    Abandoned,
    /// An autopilot rule was disabled from a channel button after its
    /// circuit breaker tripped (`autopilot_notify`).
    Paused,
    /// A human took over a `needs_human` goal task by hand (D6, Intercom
    /// `Loop in teammate` `Take over`) instead of resolving it through one of
    /// the automated exits. Unlike every other verb here, the underlying
    /// task is NOT resolved — it stays `needs_human`; only the channel card
    /// collapses.
    TakenOver,
}

impl DecisionVerb {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "已同意",
            Self::Denied => "已拒絕",
            Self::DeclinedInstall => "已婉拒",
            Self::Retried => "已重試",
            Self::MarkedDone => "已標記完成",
            Self::Abandoned => "已放棄",
            Self::Paused => "已暫停",
            Self::TakenOver => "已接手",
        }
    }

    pub fn emoji(self) -> &'static str {
        match self {
            Self::Approved => "✅",
            Self::Denied => "❌",
            Self::DeclinedInstall => "🙅",
            Self::Retried => "🔁",
            Self::MarkedDone => "✅",
            Self::Abandoned => "⛔",
            Self::Paused => "⏸",
            Self::TakenOver => "👤",
        }
    }

    /// Map a goal-loop needs_human decision string (`"retry"`/`"done"`/
    /// `"abort"`) to its verb. `None` for anything else (fail-closed — an
    /// unrecognised decision string never collapses a card with a bogus
    /// verb).
    pub fn from_goal_decision(decision: &str) -> Option<Self> {
        match decision {
            "retry" => Some(Self::Retried),
            "done" => Some(Self::MarkedDone),
            "abort" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Render the one-line settlement result: "{emoji} {verb}（由 {decider} 於
/// HH:mm）". `decider` is the resolved human display name; when unresolvable
/// (no mapped dashboard identity — the solo-operator press path) the clause
/// is omitted rather than showing a raw internal channel id (project
/// convention: never leak internal identifiers into user-facing text).
pub fn result_line(verb: DecisionVerb, decider: Option<&str>) -> String {
    let time = chrono::Local::now().format("%H:%M");
    match decider.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => format!("{} {}（由 {name} 於 {time}）", verb.emoji(), verb.label()),
        None => format!("{} {}（於 {time}）", verb.emoji(), verb.label()),
    }
}

/// Compose the collapsed card body: a one-line identifying summary
/// (regenerated fresh from the decision's own record — e.g. a task title or
/// an approval's action kind + summary — NOT parsed out of the original
/// pushed message text, so no separate storage of the original text is
/// needed) followed by the settlement result line.
///
/// `summary_line` must already be a single line (CJK-safe truncated by the
/// caller); this function does not re-truncate it.
pub fn collapse_body(summary_line: &str, verb: DecisionVerb, decider: Option<&str>) -> String {
    let summary_line = summary_line.trim();
    let result = result_line(verb, decider);
    if summary_line.is_empty() {
        result
    } else {
        format!("{summary_line}\n\n{result}")
    }
}

/// Channels that support a durable, bot-token-authenticated message edit
/// (as opposed to a short-lived interaction-response token). LINE is
/// deliberately excluded — see the module doc.
pub fn channel_editable(channel: &str) -> bool {
    matches!(channel, "telegram" | "slack" | "discord")
}

/// Resolve the display name of the dashboard identity mapped to
/// `(channel, channel_user_id)`, for attribution in [`result_line`]. `None`
/// when `users.db` does not exist, the channel identity is unverified/
/// unmapped, or the mapped user has no display name — never fabricated.
pub fn resolve_decider_name(home_dir: &Path, channel: &str, channel_user_id: &str) -> Option<String> {
    let path = home_dir.join("users.db");
    if !path.exists() {
        return None;
    }
    let db = duduclaw_auth::UserDb::new(&path).ok()?;
    let uid = db
        .find_verified_user_id_by_channel(channel, channel_user_id)
        .ok()
        .flatten()?;
    let user = db.get_user(&uid).ok().flatten()?;
    let name = user.display_name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// The message identity a successful button push landed on, captured from
/// the platform's send response so a later decide can edit it directly with
/// a bot token (durable — unlike an interaction `response_url`/token, which
/// is single-use and expires within minutes).
///
/// `edit_chat_id` is the id to target when editing. For Telegram/Slack this
/// is the same chat id the message was sent to; for Discord it is the
/// resolved bot↔user DM channel id (which differs from the raw user id used
/// to address the send), captured once at push time so the edit never needs
/// to re-resolve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedMessage {
    pub edit_chat_id: String,
    pub message_id: String,
}

/// Edit a previously-sent message in place, clearing any buttons. Bot-token
/// authenticated (never an interaction `response_url`/token), so it works
/// regardless of how much time has passed since the original push.
///
/// `pub(crate)`: called both from [`collapse_all`] and directly by tests.
pub(crate) async fn edit_channel_message(
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    edit_chat_id: &str,
    message_id: &str,
    text: &str,
) -> Result<(), String> {
    match channel {
        "telegram" => {
            let mid: i64 = message_id
                .parse()
                .map_err(|_| "invalid telegram message_id".to_string())?;
            let url = format!("https://api.telegram.org/bot{token}/editMessageText");
            let body = json!({
                "chat_id": edit_chat_id,
                "message_id": mid,
                "text": text,
                // Explicit empty keyboard — Telegram otherwise PRESERVES the
                // prior reply_markup on an editMessageText call that omits
                // the field, leaving the spent buttons clickable.
                "reply_markup": { "inline_keyboard": [] },
            });
            let resp = http
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| crate::secret_redact::redact_secrets(&e.to_string()).into_owned())?;
            if !resp.status().is_success() {
                return Err(format!("telegram editMessageText HTTP {}", resp.status()));
            }
            Ok(())
        }
        "slack" => {
            let body = json!({
                "channel": edit_chat_id,
                "ts": message_id,
                "text": text,
                // A single section block REPLACES the prior blocks (including
                // the actions block carrying the buttons).
                "blocks": [
                    { "type": "section", "text": { "type": "mrkdwn", "text": text } }
                ],
            });
            let resp = http
                .post("https://slack.com/api/chat.update")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            if data.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(format!(
                    "slack chat.update: {}",
                    data.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
                ));
            }
            Ok(())
        }
        "discord" => {
            let url = format!("https://discord.com/api/v10/channels/{edit_chat_id}/messages/{message_id}");
            let body = json!({ "content": text, "components": [] });
            let resp = http
                .patch(&url)
                .header("Authorization", format!("Bot {token}"))
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("discord PATCH HTTP {}", resp.status()));
            }
            Ok(())
        }
        other => Err(format!("channel {other} has no message-edit capability")),
    }
}

/// Retire EVERY card a settled decision left behind, not only the one the
/// presser happened to be looking at.
///
/// A decision routinely lands on several destinations at once — an install
/// sign-off is pushed to every eligible approver's every linked channel, and
/// a broker approval with no originating conversation fans out to all
/// approvers. Collapsing only the presser's copy left the others sitting
/// there with live-looking buttons: pressing one produced a "已被處理過"
/// toast, but the card still read as pending, which is exactly the stale-card
/// problem in-place collapse exists to remove.
///
/// `token_for` resolves the bot token per channel (the fan-out can span
/// platforms, and different sources resolve tokens differently — an agent's
/// own cascade vs. the global config). `fallback` is where a bare one-line
/// result is appended when NO card could be edited at all; pass `None` to
/// stay silent (the caller already sent its own acknowledgement).
///
/// Best-effort throughout: every per-destination failure is logged and
/// skipped. The decision itself is already durable by the time this runs.
pub(crate) async fn collapse_all<F, Fut>(
    home_dir: &Path,
    http: &reqwest::Client,
    namespace: &str,
    decision_id: &str,
    summary_line: &str,
    verb: DecisionVerb,
    decider: Option<&str>,
    token_for: F,
    fallback: Option<(&str, &str)>,
) where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let cards = decision_message_store::list_card_messages(home_dir, namespace, decision_id);
    let body = collapse_body(summary_line, verb, decider);
    let mut edited = 0usize;

    for card in cards {
        if !channel_editable(&card.channel) {
            continue;
        }
        let Some(token) = token_for(card.channel.clone()).await else {
            continue;
        };
        match edit_channel_message(
            http,
            &card.channel,
            &token,
            &card.pushed.edit_chat_id,
            &card.pushed.message_id,
            &body,
        )
        .await
        {
            Ok(()) => edited += 1,
            Err(e) => tracing::info!(
                channel = %card.channel, %decision_id, error = %e,
                "decision-card: edit-in-place failed for one destination"
            ),
        }
    }

    if edited > 0 {
        return;
    }
    // Nothing was editable (no stored reference, a LINE-only push, or every
    // edit failed): honest degrade — append a bare one-line result where the
    // caller told us to, rather than leaving no visible confirmation.
    let Some((channel, chat_id)) = fallback else {
        return;
    };
    if !channel_editable(channel) {
        // LINE's postback reply already IS the one-line result; appending a
        // second copy would double up.
        return;
    }
    let Some(token) = token_for(channel.to_string()).await else {
        return;
    };
    let line = result_line(verb, decider);
    let _ = crate::goal_notify::send_plain_text(home_dir, http, channel, &token, chat_id, &line).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_goal_decision_maps_known_and_rejects_unknown() {
        assert_eq!(DecisionVerb::from_goal_decision("retry"), Some(DecisionVerb::Retried));
        assert_eq!(DecisionVerb::from_goal_decision("done"), Some(DecisionVerb::MarkedDone));
        assert_eq!(DecisionVerb::from_goal_decision("abort"), Some(DecisionVerb::Abandoned));
        assert_eq!(DecisionVerb::from_goal_decision("garbage"), None);
    }

    #[test]
    fn paused_verb_renders_label_and_emoji() {
        assert_eq!(DecisionVerb::Paused.label(), "已暫停");
        assert_eq!(DecisionVerb::Paused.emoji(), "⏸");
    }

    #[test]
    fn taken_over_verb_renders_label_and_emoji() {
        assert_eq!(DecisionVerb::TakenOver.label(), "已接手");
        assert_eq!(DecisionVerb::TakenOver.emoji(), "👤");
    }

    #[test]
    fn taken_over_result_line_names_the_decider() {
        // D6: the collapsed card must read "👤 <名字> 已接手" — result_line's
        // shared "由 X" clause is how every other verb attributes a decider,
        // so takeover reuses it rather than inventing a second format.
        let line = result_line(DecisionVerb::TakenOver, Some("王小明"));
        assert!(line.starts_with("👤 已接手（由 王小明 於 "));
    }

    #[test]
    fn result_line_with_decider_includes_name_and_time() {
        let line = result_line(DecisionVerb::Approved, Some("王小明"));
        assert!(line.starts_with("✅ 已同意（由 王小明 於 "));
        assert!(line.ends_with('）'));
    }

    #[test]
    fn result_line_without_decider_omits_the_by_clause() {
        let line = result_line(DecisionVerb::Retried, None);
        assert!(line.starts_with("🔁 已重試（於 "));
        assert!(!line.contains("由"));
    }

    #[test]
    fn result_line_treats_blank_decider_as_absent() {
        let line = result_line(DecisionVerb::Abandoned, Some("   "));
        assert!(!line.contains("由"));
    }

    #[test]
    fn collapse_body_joins_summary_and_result() {
        let body = collapse_body("🐾 目標任務：整理客戶月報", DecisionVerb::MarkedDone, Some("Alice"));
        let lines: Vec<&str> = body.split("\n\n").collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "🐾 目標任務：整理客戶月報");
        assert!(lines[1].starts_with("✅ 已標記完成"));
    }

    #[test]
    fn collapse_body_with_empty_summary_is_just_the_result_line() {
        let body = collapse_body("   ", DecisionVerb::Denied, None);
        assert_eq!(body, result_line(DecisionVerb::Denied, None));
    }

    #[test]
    fn channel_editable_excludes_line_and_unknown_channels() {
        assert!(channel_editable("telegram"));
        assert!(channel_editable("slack"));
        assert!(channel_editable("discord"));
        assert!(!channel_editable("line"));
        assert!(!channel_editable("whatsapp"));
        assert!(!channel_editable("googlechat"));
    }

    #[test]
    fn resolve_decider_name_absent_users_db_degrades_to_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_decider_name(dir.path(), "telegram", "555"), None);
    }

    #[tokio::test]
    async fn collapse_all_makes_no_call_when_nothing_was_recorded() {
        // No stored card and a LINE fallback (which has no edit API and whose
        // postback reply already IS the one-line result) ⇒ no HTTP at all.
        // Verified indirectly: this must return promptly without panicking
        // even though the token resolver would yield a bogus token.
        let dir = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        collapse_all(
            dir.path(),
            &http,
            "goal_task",
            "g1",
            "summary",
            DecisionVerb::Retried,
            None,
            |_ch: String| async { Some("bogus-token".to_string()) },
            Some(("line", "u1")),
        )
        .await;
    }

    #[tokio::test]
    async fn collapse_all_skips_destinations_with_no_resolvable_token() {
        // A recorded card whose channel has no bot token must be skipped, not
        // retried with an empty token (which would 401 against the platform).
        let dir = tempfile::tempdir().unwrap();
        let http = reqwest::Client::new();
        decision_message_store::record_card_message(
            dir.path(),
            "goal_task",
            "g1",
            "telegram",
            "555",
            &PushedMessage { edit_chat_id: "555".into(), message_id: "m1".into() },
        );
        collapse_all(
            dir.path(),
            &http,
            "goal_task",
            "g1",
            "summary",
            DecisionVerb::Retried,
            None,
            |_ch: String| async { None },
            None,
        )
        .await;
    }
}
