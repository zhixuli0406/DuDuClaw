//! RM-191 tests for the inbound guard context. Not run here (owner forbids
//! builds); the reviewer runs them.
use super::*;
use duduclaw_security::input_guard::ScanTrust::{Strict, VerifiedMemberOwnConversation as Relaxed};

const RULE_NAMES: [&str; 12] = [
    "instruction_override",
    "system_prompt_extraction",
    "role_hijack",
    "encoding_bypass",
    "tool_abuse",
    "data_exfiltration",
    "termination_manipulation",
    "authority_escalation",
    "memory_poisoning",
    "role_provenance",
    "action_binding",
    "unicode_injection",
];
const LANGUAGES: [Option<&str>; 6] = [None, Some("zh-TW"), Some("zh-CN"), Some("en"), Some("ja-JP"), Some("fr")];

fn dashboard(id: &str) -> RequesterIdentity {
    RequesterIdentity::DashboardUser { user_id: id.into() }
}

fn channel(chat: &str, sender: &str, bound: Option<&str>) -> RequesterIdentity {
    RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: chat.into(),
        sender: sender.into(),
        user_id: bound.map(str::to_string),
    }
}

/// `(case, requester, public, own WebChat conversation, one-to-one, expected)`.
#[test]
fn trust_truth_table() {
    let widget = RequesterIdentity::ChannelUser {
        channel: "widget".into(), chat: "v1".into(), sender: String::new(), user_id: None,
    };
    let livechat = RequesterIdentity::ChannelUser {
        channel: "odoolivechat".into(), chat: "42".into(), sender: String::new(), user_id: None,
    };
    let system = RequesterIdentity::System { kind: "internal".into(), on_behalf_of: None };
    let cases = [
        ("signed-in member, own WebChat", dashboard("u-1"), false, true, false, Relaxed),
        ("signed-in member, not own conversation", dashboard("u-1"), false, false, false, Strict),
        ("signed-in member, public session", dashboard("u-1"), true, true, false, Strict),
        ("empty dashboard id", dashboard(""), false, true, false, Strict),
        ("widget visitor principal", dashboard("widget-visitor:7f1c"), false, true, false, Strict),
        ("widget visitor requester", widget, true, false, false, Strict),
        ("odoo live chat visitor", livechat, true, false, true, Strict),
        ("legacy system principal", dashboard("system"), false, true, true, Strict),
        ("unbound channel sender, 1:1", channel("U1", "U1", None), false, false, true, Strict),
        ("bound sender in a group", channel("Cgroup", "U1", Some("u-1")), false, false, false, Strict),
        ("bound sender in 1:1", channel("U1", "U1", Some("u-1")), false, false, true, Relaxed),
        ("bound sender in 1:1, public flag", channel("U1", "U1", Some("u-1")), true, false, true, Strict),
        ("empty binding", channel("U1", "U1", Some("")), false, false, true, Strict),
        ("system requester", system, false, true, true, Strict),
        ("unknown requester", RequesterIdentity::Unknown, false, true, true, Strict),
    ];
    for (case, requester, public, own, one_to_one, expected) in cases {
        assert_eq!(inbound_guard_trust(&requester, public, own, one_to_one), expected, "{case}");
    }
}

#[test]
fn no_requester_scope_is_strict() {
    // Outside a guarded reply nothing is scoped: requester Unknown, no
    // WebChat principal.
    assert_eq!(inbound_guard_trust_for_turn("line:U1", "U1", "dudu"), Strict);
    let sid = format!("webchat:webchat:{}:c1", crate::webchat::webchat_owner_tag("u-1"));
    assert_eq!(inbound_guard_trust_for_turn(&sid, "u-1", "dudu"), Strict);
}

async fn scoped(principal: Option<&str>, requester: RequesterIdentity, sid: &str, user: &str) -> ScanTrust {
    let (sid, user) = (sid.to_string(), user.to_string());
    let turn = crate::knowledge::turn_requesters::CURRENT_REQUESTER
        .scope(requester, async move { inbound_guard_trust_for_turn(&sid, &user, "dudu") });
    match principal {
        Some(p) => crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(p.to_string(), turn).await,
        None => turn.await,
    }
}

#[tokio::test]
async fn signed_in_member_in_own_webchat_is_relaxed_through_real_signals() {
    let own = format!("webchat:webchat:{}:c1", crate::webchat::webchat_owner_tag("u-1"));
    assert_eq!(scoped(Some("u-1"), dashboard("u-1"), &own, "u-1").await, Relaxed);
    // Another account's conversation: no WebChat origin.
    let other = format!("webchat:webchat:{}:c1", crate::webchat::webchat_owner_tag("u-2"));
    assert_eq!(scoped(Some("u-1"), dashboard("u-1"), &other, "u-1").await, Strict);
    // The legacy token principal in its own conversation stays strict.
    let system = format!("webchat:webchat:{}:c1", crate::webchat::webchat_owner_tag("system"));
    assert_eq!(scoped(Some("system"), dashboard("system"), &system, "system").await, Strict);
    // A widget visitor is a public session.
    let visitor = "widget-visitor:7f1c2a90-0000-4000-8000-0000000000aa";
    let widget = RequesterIdentity::ChannelUser {
        channel: "widget".into(), chat: "7f1c".into(), sender: String::new(), user_id: None,
    };
    assert_eq!(scoped(Some(visitor), widget, "webchat:widget-chat", visitor).await, Strict);
}

#[tokio::test]
async fn bound_messaging_sender_is_relaxed_only_one_to_one() {
    assert_eq!(scoped(None, channel("U1", "U1", Some("u-1")), "line:U1", "U1").await, Relaxed);
    assert_eq!(scoped(None, channel("Cgroup", "U1", Some("u-1")), "line:Cgroup", "U1").await, Strict);
    assert_eq!(scoped(None, channel("U1", "U1", None), "line:U1", "U1").await, Strict);
    // The bound requester must be the sender of this message.
    assert_eq!(scoped(None, channel("U1", "U2", Some("u-1")), "line:U1", "U1").await, Strict);
}

#[test]
fn audit_channel_is_the_session_prefix() {
    assert_eq!(audit_channel("line:U1"), "line");
    assert_eq!(audit_channel("webchat:webchat:abc:c1"), "webchat");
    assert_eq!(audit_channel("default"), "unknown");
}

fn assert_no_internals(text: &str) {
    for name in RULE_NAMES {
        assert!(!text.contains(name), "rule name {name} leaked: {text}");
    }
    for marker in ["score", "/100", "BLOCKED", "rules:"] {
        assert!(!text.contains(marker), "{marker} leaked: {text}");
    }
}

#[test]
fn public_notice_names_no_rule_category_or_score() {
    let all: Vec<String> = RULE_NAMES.iter().map(|r| r.to_string()).collect();
    for language in LANGUAGES {
        let public = blocked_notice(&all, language, true);
        assert_no_internals(&public);
        assert_eq!(public, blocked_notice(&[], language, true), "public text is generic");
        for name in RULE_NAMES {
            assert!(!public.contains(rule_label(name, lang(language))), "{public}");
        }
    }
}

#[test]
fn member_notice_explains_category_and_alternative() {
    let tool = vec!["tool_abuse".to_string()];
    let zh = blocked_notice(&tool, None, false);
    assert!(zh.contains("沒有交給 AI"), "{zh}");
    assert!(zh.contains("可直接執行的指令或程式執行語法"), "{zh}");
    assert!(zh.contains("附加檔案"), "{zh}");
    assert_eq!(zh, blocked_notice(&tool, Some("zh-TW"), false));
    assert!(blocked_notice(&tool, Some("zh-CN"), false).contains("附加文件"));
    assert!(blocked_notice(&tool, Some("en"), false).contains("not passed to the AI"));
    assert!(blocked_notice(&tool, Some("en-US"), false).contains("attach it as a file"));
    assert!(blocked_notice(&tool, Some("ja-JP"), false).contains("AI に渡されませんでした"));
    for language in LANGUAGES {
        let all: Vec<String> = RULE_NAMES.iter().map(|r| r.to_string()).collect();
        assert_no_internals(&blocked_notice(&all, language, false));
    }
}

#[test]
fn every_rule_has_its_own_label_and_labels_are_deduplicated() {
    for language in LANGUAGES {
        let lang = lang(language);
        let fallback = rule_label("", lang);
        for name in RULE_NAMES {
            assert_ne!(rule_label(name, lang), fallback, "{name} {lang:?}");
        }
    }
    let twice = vec!["tool_abuse".to_string(), "tool_abuse".to_string()];
    let text = blocked_notice(&twice, None, false);
    assert_eq!(text.matches("可直接執行的指令或程式執行語法").count(), 1, "{text}");
    // An unknown rule name falls back to a generic label, never the name.
    let unknown = blocked_notice(&["future_rule".to_string()], None, false);
    assert!(unknown.contains("其他可疑內容") && !unknown.contains("future_rule"), "{unknown}");
}
