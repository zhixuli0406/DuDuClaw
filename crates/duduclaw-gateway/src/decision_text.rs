//! WP1.6 (ecosystem) — text-reply decisions: replying to a decision card with
//! a bare verb (「同意」/「拒絕」/「重試」…) counts as pressing the button.
//!
//! Why: smartwatch clients (LINE watchOS, Telegram's 2026 watch apps) deliver
//! decision cards to the wrist but cannot render inline buttons — text reply
//! is the only affordance there. This routes those replies through the SAME
//! `decision_notify::dispatch` path as a button press: same authorization,
//! same idempotency, same action-rate accounting. No second decision path.
//!
//! Safety posture:
//! - The verb must be the WHOLE message (exact vocabulary match after
//!   trimming trailing punctuation) — never a substring scan (coding
//!   convention 2), so「我不同意這個方案，先討論」keeps flowing to the agent.
//! - The replied-to message must reverse-match a recorded decision card in
//!   `decision_message_store`; everything else returns `None` and the
//!   message continues to normal chat (fail-open to conversation, fail-closed
//!   on authorization inside `dispatch`).

use std::path::Path;

use crate::decision_action::{DecisionAct, DecisionAction, DecisionSource};

/// The five sources, locally enumerated (the equivalent test-side constant is
/// `#[cfg(test)]`-gated in `decision_action`).
const SOURCES: [DecisionSource; 5] = [
    DecisionSource::Goal,
    DecisionSource::Kickoff,
    DecisionSource::Approval,
    DecisionSource::Install,
    DecisionSource::Autopilot,
];

/// Whole-message decision vocabulary (zh-TW + en). Trailing sentence
/// punctuation is tolerated;（「同意！」）anything else is not a decision.
pub fn parse_decision_verb(text: &str) -> Option<DecisionAct> {
    let t = text
        .trim()
        .trim_end_matches(['。', '！', '!', '.', '～', '~'])
        .trim()
        .to_lowercase();
    match t.as_str() {
        "同意" | "核准" | "批准" | "通過" | "可以" | "好" | "ok" | "approve" | "yes" => {
            Some(DecisionAct::Approve)
        }
        "拒絕" | "駁回" | "不同意" | "不行" | "deny" | "reject" | "no" => {
            Some(DecisionAct::Deny)
        }
        "重試" | "retry" => Some(DecisionAct::Retry),
        "完成" | "done" => Some(DecisionAct::Done),
        "中止" | "取消" | "abort" | "cancel" => Some(DecisionAct::Abort),
        "暫停" | "pause" => Some(DecisionAct::Pause),
        _ => None,
    }
}

fn source_from_namespace(ns: &str) -> Option<DecisionSource> {
    SOURCES.iter().copied().find(|s| s.namespace() == ns)
}

/// Pure resolution half (unit-testable without touching decision stores):
/// `None` ⇒ not a decision reply, flow to normal chat. `Some(Ok(action))` ⇒
/// apply it. `Some(Err(msg))` ⇒ recognized a verb aimed at a real card but it
/// does not apply to that card kind — tell the user instead of silently
/// chatting the verb to the agent.
pub fn resolve_action(
    home_dir: &Path,
    channel: &str,
    chat_id: &str,
    replied_message_id: &str,
    text: &str,
) -> Option<Result<DecisionAction, String>> {
    let act = parse_decision_verb(text)?;
    let (ns, id) = crate::decision_message_store::lookup_decision_by_message(
        home_dir,
        channel,
        chat_id,
        replied_message_id,
    )?;
    let source = source_from_namespace(&ns)?;
    if !act.valid_for(source) {
        return Some(Err(format!(
            "這張卡不支援「{}」——請用卡片上的選項回覆",
            text.trim()
        )));
    }
    Some(Ok(DecisionAction { source, act, id }))
}

/// Full path: resolve, then apply through the shared button-press dispatcher
/// (same authorization + action-rate accounting as a physical press).
pub async fn route_text_reply(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    chat_id: &str,
    replied_message_id: &str,
    text: &str,
) -> Option<Result<String, String>> {
    match resolve_action(home_dir, channel, chat_id, replied_message_id, text)? {
        Ok(action) => {
            Some(crate::decision_notify::dispatch(home_dir, channel, channel_user_id, &action).await)
        }
        Err(msg) => Some(Err(msg)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_card::PushedMessage;

    #[test]
    fn verbs_parse_whole_message_only() {
        assert_eq!(parse_decision_verb("同意"), Some(DecisionAct::Approve));
        assert_eq!(parse_decision_verb(" OK！"), Some(DecisionAct::Approve));
        assert_eq!(parse_decision_verb("拒絕。"), Some(DecisionAct::Deny));
        assert_eq!(parse_decision_verb("重試"), Some(DecisionAct::Retry));
        // Substrings / sentences are conversation, not decisions.
        assert_eq!(parse_decision_verb("我不同意這個方案，先討論"), None);
        assert_eq!(parse_decision_verb("ok 但先等一下"), None);
        assert_eq!(parse_decision_verb(""), None);
    }

    #[test]
    fn resolve_matches_card_and_validates_source() {
        let dir = tempfile::tempdir().unwrap();
        crate::decision_message_store::record_card_message(
            dir.path(),
            DecisionSource::Approval.namespace(),
            "apr-123",
            "telegram",
            "555",
            &PushedMessage { edit_chat_id: "555".into(), message_id: "9001".into() },
        );

        // Right message + valid verb ⇒ action.
        let ok = resolve_action(dir.path(), "telegram", "555", "9001", "同意");
        match ok {
            Some(Ok(a)) => {
                assert_eq!(a.source, DecisionSource::Approval);
                assert_eq!(a.act, DecisionAct::Approve);
                assert_eq!(a.id, "apr-123");
            }
            other => panic!("expected action, got {other:?}"),
        }

        // Verb that doesn't fit the card kind ⇒ explanatory error, not chat.
        assert!(matches!(
            resolve_action(dir.path(), "telegram", "555", "9001", "重試"),
            Some(Err(_))
        ));

        // Wrong message id / non-card reply ⇒ None (flow to agent).
        assert!(resolve_action(dir.path(), "telegram", "555", "9999", "同意").is_none());
        // Plain sentence to the right card ⇒ None (flow to agent).
        assert!(resolve_action(dir.path(), "telegram", "555", "9001", "同意這方向，但預算再談").is_none());
    }
}
