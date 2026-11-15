//! `needs_reconnect` wording by audience (owner vs anyone else).
use super::*;

const OWNER_TEXT: &str = "你的 Google 連線需要重新連結，請到「我的連線」處理。";

fn reconnect() -> Refusal {
    Refusal::soft("needs_reconnect", OWNER_TEXT)
}

fn member(user_id: Option<&str>) -> RequesterIdentity {
    RequesterIdentity::ChannelUser {
        channel: "googlechat".into(),
        chat: "spaces/A".into(),
        sender: "users/1".into(),
        user_id: user_id.map(str::to_string),
    }
}

#[test]
fn owner_keeps_todays_text() {
    let owner = RequesterIdentity::DashboardUser { user_id: "owner".into() };
    assert_eq!(for_requester(reconnect(), &owner, "owner").message_zh, OWNER_TEXT);
    assert_eq!(for_requester(reconnect(), &member(Some("owner")), "owner").message_zh, OWNER_TEXT);
}

#[test]
fn anyone_else_is_told_whose_connection_it_is() {
    for requester in [
        member(Some("colleague")),
        member(None),
        RequesterIdentity::System { kind: "cron".into(), on_behalf_of: None },
        RequesterIdentity::Unknown,
    ] {
        let refusal = for_requester(reconnect(), &requester, "owner");
        assert_eq!(refusal.message_zh, OWNER_RECONNECT_FOR_OTHERS, "{requester:?}");
        // Code and fallback eligibility are unchanged: only the words move.
        assert_eq!(refusal.code, "needs_reconnect");
        assert!(refusal.fallback_eligible);
    }
    let consent = for_requester(consent_revoked_refusal(), &member(Some("colleague")), "owner");
    assert_eq!(consent.message_zh, OWNER_RECONNECT_FOR_OTHERS);
}

#[test]
fn other_refusals_are_untouched() {
    let refusal = Refusal::hard("scope_missing", "x");
    assert_eq!(for_requester(refusal, &member(Some("colleague")), "owner").message_zh, "x");
}

#[test]
fn check_metadata_owner_text_is_unchanged() {
    // The raw check still speaks to the owner; callers re-word it.
    let src = include_str!("access.rs");
    assert!(src.contains(OWNER_TEXT));
}
