//! Chat-channel decision replies (P0-B, review round F4).
//!
//! One definition of "this chat message is a decision", shared by every
//! channel adapter (Telegram, Discord, Slack, LINE) and by the normal reply
//! pipeline. A message is a decision **only** when its first word is one of
//! the six verbs AND its second word parses as a complete request UUID:
//!
//! ```text
//! 確認 <uuid> | 取消 <uuid> | 回答 <uuid> <answer>
//! approve <uuid> | deny <uuid> | answer <uuid> <answer>
//! ```
//!
//! Everything else is ordinary conversation and must reach the existing
//! handling unchanged: 「確認」 on its own (the employee asked "shall I send
//! it?"), "approve the Q3 budget", and the shipped WP1.6 reply-to-card verbs
//! (「取消」/`approve`/`deny` sent as a reply to a goal or legacy approval
//! card, `decision_text::route_text_reply`). Before F4 the adapters consumed
//! any verb-first message and answered 「請提供完整的請求編號」, which
//! swallowed those messages (review findings M1/M2).
//!
//! Matching is whole-token equality, never a substring scan (coding
//! convention 2).

mod discord_lanes;
mod slack_identity;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub(crate) use discord_lanes::{DiscordLane, current_discord_permits};
#[cfg(test)]
pub(crate) use discord_lanes::{DiscordPermits, with_test_discord_permits};
pub use slack_identity::slack_identity_doctor;
pub(crate) use slack_identity::{
    SLACK_IDENTITY_UNAVAILABLE, SlackIdentityFailure, note_slack_identity_failure,
    note_slack_identity_ok,
};

/// The six decision verbs. The first token must equal one of them exactly.
pub(crate) const DECISION_VERBS: [&str; 6] = ["確認", "取消", "回答", "approve", "deny", "answer"];

/// The one reply for every refused decision attempt whose sender has not yet
/// been shown to be the person the request is bound to: unknown id, a request
/// bound to another account / conversation / person, a sender the channel
/// settings refuse, an identity the adapter could not establish. Using one
/// sentence for all of them means the reply cannot be used to probe which
/// request ids exist or who they belong to (review L2).
pub(crate) const DECISION_REFUSED: &str = "無法處理這個決定：請求編號不存在或已失效，或您不是可以在這裡決定這筆請求的人。需要時請到儀表板查看。";

/// A parsed decision command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StrictDecision<'a> {
    pub verb: &'a str,
    /// The request id, normalised to the lowercase hyphenated form request
    /// ids are stored in (`Uuid::new_v4().to_string()`).
    pub id: String,
    /// Text after the id (the answer for 回答/answer); `None` when blank or
    /// only closing punctuation.
    pub rest: Option<&'a str>,
}

impl StrictDecision<'_> {
    /// 回答 / answer: collects an answer, never authorizes anything.
    pub(crate) fn is_answer(&self) -> bool {
        matches!(self.verb, "回答" | "answer")
    }
    /// 確認 / approve.
    pub(crate) fn approves(&self) -> bool {
        matches!(self.verb, "確認" | "approve")
    }
}

/// Sentence-closing punctuation tolerated after the id or at the end of the
/// command (F5-C, review F4-L5): `確認 <id>。`, `approve <id>.`, `確認 <id>！`.
const CLOSING_PUNCTUATION: [char; 12] = [
    '。', '.', '！', '!', '～', '~', '，', ',', '、', '；', ';', '．',
];

fn only_punctuation(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_whitespace() || CLOSING_PUNCTUATION.contains(&c))
}

/// `Some` only for "verb + complete request UUID [+ rest]".
///
/// Tolerated since F5-C (review F4-L5), so a near-miss is still handled as a
/// decision instead of silently reaching the model and, on Telegram/LINE,
/// queueing behind the action it was meant to release:
/// - any run of whitespace (incl. U+3000) between verb, id and rest;
/// - closing punctuation directly after the id or as the whole rest.
///
/// Still required: the verb is the first word, matched exactly (case
/// sensitive), and the id is a complete UUID. Anything without one is not a
/// decision and is never intercepted.
pub(crate) fn parse_strict_decision(text: &str) -> Option<StrictDecision<'_>> {
    let text = text.trim();
    let verb_end = text.find(char::is_whitespace)?;
    let verb = &text[..verb_end];
    if !DECISION_VERBS.contains(&verb) {
        return None;
    }
    let after_verb = text[verb_end..].trim_start();
    let id_end = after_verb
        .find(char::is_whitespace)
        .unwrap_or(after_verb.len());
    let id_token = after_verb[..id_end].trim_end_matches(CLOSING_PUNCTUATION);
    if id_token.is_empty() {
        return None;
    }
    let id = uuid::Uuid::parse_str(id_token)
        .ok()?
        .hyphenated()
        .to_string();
    let rest = after_verb[id_end..].trim();
    let rest = (!only_punctuation(rest)).then_some(rest);
    Some(StrictDecision { verb, id, rest })
}

/// Shorthand for adapters that only need the yes/no.
pub(crate) fn is_strict_decision(text: &str) -> bool {
    parse_strict_decision(text).is_some()
}

/// Remove a leading `@username` that addresses **this** bot: only at the
/// start, compared ASCII-case-insensitively (Telegram usernames are), and only
/// when followed by whitespace or the end of the text. Anything else is
/// returned unchanged (F5-C, review F4-L4; coding convention 2 — no
/// unanchored replacement for a routing decision).
pub(crate) fn strip_leading_mention<'a>(text: &'a str, bot_username: &str) -> &'a str {
    let trimmed = text.trim_start();
    if bot_username.is_empty() {
        return trimmed;
    }
    let Some(after_at) = trimmed.strip_prefix('@') else {
        return trimmed;
    };
    let end = after_at.find(char::is_whitespace).unwrap_or(after_at.len());
    if after_at[..end].eq_ignore_ascii_case(bot_username) {
        after_at[end..].trim_start()
    } else {
        trimmed
    }
}

/// Test seam key for `channel_reply::inner` (stands in for the model turn).
#[cfg(test)]
pub(crate) fn inner_test_seam_key(session_id: &str) -> String {
    format!("inner-model-seam:{session_id}")
}
/// What the inner test seam returns instead of a model reply.
#[cfg(test)]
pub(crate) const INNER_TEST_SEAM_REPLY: &str = "fixture model turn";

/// Telegram's shared sender accounts. `from` carries one of these when the
/// message was not sent by an identifiable person:
/// - `1087968824` (@GroupAnonymousBot): an anonymous group administrator;
/// - `136817688` (@Channel_Bot): a member posting "as the channel" / as a chat;
/// - `777000` (Telegram service): automatic forwards from a linked channel.
///
/// Every anonymous admin shares the same id, so a reply carrying it can never
/// be one particular person's decision (review L1).
pub(crate) const TELEGRAM_SHARED_SENDER_IDS: [i64; 3] = [1087968824, 136817688, 777000];

/// The Telegram principal a decision may be bound to, or `""` when the sender
/// is not one identifiable person. `""` makes `DecisionContext::validate`
/// fail, so no trusted reply target is created (no high-risk confirmation can
/// be requested from that turn) and no decision is accepted.
///
/// `forwarded` (the message carries `forward_origin`): forwarding someone's
/// text is not the forwarder saying it, so a forwarded message is never a
/// decision either (F5-C, review F4-L8).
pub(crate) fn telegram_decision_principal(
    from_id: Option<i64>,
    from_is_bot: bool,
    has_sender_chat: bool,
    forwarded: bool,
) -> String {
    match from_id {
        Some(id)
            if id > 0
                && !from_is_bot
                && !has_sender_chat
                && !forwarded
                && !TELEGRAM_SHARED_SENDER_IDS.contains(&id) =>
        {
            id.to_string()
        }
        _ => String::new(),
    }
}
