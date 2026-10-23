//! Team callable set (`users.manager_user_id`): a Manager calls the
//! employees bound to anyone below them, on messaging channels as in
//! WebChat; a peer cannot; an org change reaches the next message.

use std::path::Path;

use duduclaw_auth::{AccessLevel, GrantSource, UserDb, UserRole};

use super::pending;
use super::tests::registry;
use super::turn::{self, ChannelTurn, TurnDecision};
use super::*;

/// mgr (Manager, LINE `U_mgr`) → lead (Manager) → emp1 (sales-rep Owner);
/// peer (Manager, LINE `U_peer`, no reports); other (finance Owner).
struct Org {
    lead: String,
    emp1: String,
}

fn org(home: &Path) -> Org {
    crate::team_edition::record(home, duduclaw_core::EditionProfile::Enterprise);
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let mk = |name: &str, role| {
        db.create_user(&format!("{name}@test.local"), name, "TeamPass-12345", role)
            .unwrap()
            .id
    };
    let mgr = mk("mgr", UserRole::Manager);
    let lead = mk("lead", UserRole::Manager);
    let emp1 = mk("emp1", UserRole::Employee);
    let peer = mk("peer", UserRole::Manager);
    let other = mk("other", UserRole::Employee);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.bind_agent(&emp1, "sales-rep", AccessLevel::Owner).unwrap();
    db.bind_agent(&other, "finance", AccessLevel::Owner).unwrap();
    db.bind_channel_identity(&mgr, "line", "U_mgr", true).unwrap();
    db.bind_channel_identity(&peer, "line", "U_peer", true).unwrap();
    Org { lead, emp1 }
}

fn conv() -> &'static str {
    Box::leak(format!("line:{}", uuid::Uuid::new_v4()).into_boxed_str())
}

fn line_turn<'a>(conv: &'a str, sender: &'a str, text: &'a str, event: &'a str) -> ChannelTurn<'a> {
    ChannelTurn {
        channel: "line",
        conversation: conv,
        sender,
        text,
        fallback: Some("dudu"),
        event,
        seq: pending::next_seq(),
        quiet: true,
        pending: turn::PendingPolicy::Live,
    }
}

#[tokio::test]
async fn a_manager_on_line_calls_a_skip_level_reports_employee() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "@sales-rep 報價", "e1"));
    assert_eq!(
        d,
        TurnDecision::Agent {
            id: "sales-rep".into(),
            via_trigger: true
        }
    );
    // The grant says where it came from (the audit `via`).
    let access = access::for_channel_sender(t.path(), "line", "U_mgr");
    assert_eq!(
        grant_for(&reg, &access, "sales-rep", Some("dudu")),
        Some(GrantSource::Team {
            via_user: o.emp1.clone(),
            depth: 2
        })
    );
    assert_eq!(
        grant_for(&reg, &access, "sales-rep", Some("dudu")).unwrap().label(),
        format!("team:{}:2", o.emp1)
    );
    // A peer's employee stays refused, with the explicit refusal text.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "@財務小幫手 x", "e2"));
    assert!(matches!(d, TurnDecision::Reply { ref text, kind: turn::ReplyKind::Refusal, .. } if text == FORBIDDEN_TEXT));
}

#[tokio::test]
async fn a_peer_manager_cannot_call_another_managers_team() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let d = turn::decide_channel_turn(
        t.path(),
        &reg,
        line_turn(conv(), "U_peer", "@sales-rep 報價", "p1"),
    );
    assert!(matches!(d, TurnDecision::Reply { kind: turn::ReplyKind::Refusal, .. }), "{d:?}");
}

#[tokio::test]
async fn the_at_list_carries_the_team_and_where_each_grant_comes_from() {
    let (t, reg) = registry(true).await; // sales-rep is public too
    let (t2, reg2) = registry(false).await;
    org(t.path());
    let o = org(t2.path());
    // Not public: dudu (default) + sales-rep (team).
    let access = access::for_channel_sender(t2.path(), "line", "U_mgr");
    assert_eq!(
        callable_grants(&reg2, &access, Some("dudu")),
        vec![
            ("dudu".to_string(), GrantSource::Default),
            (
                "sales-rep".to_string(),
                GrantSource::Team {
                    via_user: o.emp1.clone(),
                    depth: 2
                }
            ),
        ]
    );
    // The peer manager sees no team.
    let peer = access::for_channel_sender(t2.path(), "line", "U_peer");
    assert_eq!(callable_ids(&reg2, &peer, Some("dudu")), vec!["dudu".to_string()]);
    // A public employee is reported as public to someone without a grant.
    let peer_pub = access::for_channel_sender(t.path(), "line", "U_peer");
    assert_eq!(
        callable_grants(&reg, &peer_pub, Some("dudu")),
        vec![
            ("dudu".to_string(), GrantSource::Default),
            ("sales-rep".to_string(), GrantSource::Public),
        ]
    );
    // The `@` list rendered for the channel is the same set as the router.
    let d = turn::decide_channel_turn(t2.path(), &reg2, line_turn(conv(), "U_mgr", "@", "l1"));
    match d {
        TurnDecision::Reply { list: Some(agents), .. } => {
            let ids: Vec<&str> = agents.iter().map(|a| a.id.as_str()).collect();
            assert_eq!(ids, vec!["dudu", "sales-rep"]);
        }
        other => panic!("expected the list, got {other:?}"),
    }
}

#[tokio::test]
async fn clearing_a_manager_is_refused_on_the_very_next_message() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "@sales-rep a", "g1"));
    assert!(matches!(d, TurnDecision::Agent { .. }), "{d:?}");
    // Inside the cache TTL: the generation bump alone invalidates it.
    UserDb::new(&t.path().join("users.db"))
        .unwrap()
        .set_manager(&o.lead, None)
        .unwrap();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "@sales-rep b", "g2"));
    assert!(matches!(d, TurnDecision::Reply { kind: turn::ReplyKind::Refusal, .. }), "{d:?}");
}

#[tokio::test]
async fn a_waiting_one_shot_is_rechecked_against_the_org() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "@sales-rep", "w1"));
    assert!(matches!(d, TurnDecision::Reply { kind: turn::ReplyKind::AskTask, .. }), "{d:?}");
    UserDb::new(&t.path().join("users.db"))
        .unwrap()
        .set_manager(&o.emp1, None)
        .unwrap();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "幫我追款", "w2"));
    assert_eq!(d, TurnDecision::Default, "the one-shot is dropped once the grant is gone");
}

#[tokio::test]
async fn a_chat_command_never_calls_or_arms_anyone() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = conv();
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "/goal @sales-rep", "c1"));
    assert_eq!(d, TurnDecision::Default);
    // Nothing was armed: the next plain message goes to the default employee.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_mgr", "幫我追款", "c2"));
    assert_eq!(d, TurnDecision::Default);
    assert!(!turn::calls_anyone(&reg, "/status @sales-rep"));
}

#[tokio::test]
async fn personal_edition_ignores_leftover_reporting_lines_on_channels() {
    let (t, reg) = registry(false).await;
    org(t.path());
    // An Enterprise → Personal downgrade keeps the users.db rows.
    crate::team_edition::record(t.path(), duduclaw_core::EditionProfile::Personal);
    let d = turn::decide_channel_turn(
        t.path(),
        &reg,
        line_turn(conv(), "U_mgr", "@sales-rep 報價", "pe1"),
    );
    assert!(
        matches!(d, TurnDecision::Reply { kind: turn::ReplyKind::Refusal, .. }),
        "mapped account, no team grant: explicit refusal, got {d:?}"
    );
    let access = access::for_channel_sender(t.path(), "line", "U_mgr");
    assert_eq!(callable_ids(&reg, &access, Some("dudu")), vec!["dudu".to_string()]);
}

#[tokio::test]
async fn only_mapped_senders_are_told_about_a_refusal() {
    let (t, reg) = registry(false).await;
    org(t.path());
    // Unmapped LINE user: a forbidden name is plain text for the default
    // employee, exactly like a name that matches nobody.
    let c = conv();
    let forbidden = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_stranger", "@財務小幫手 x", "u1"));
    let unknown = turn::decide_channel_turn(t.path(), &reg, line_turn(c, "U_stranger", "@沒有這個人 x", "u2"));
    assert_eq!(forbidden, TurnDecision::Default);
    assert_eq!(unknown, TurnDecision::Default);
    assert!(!turn::calls_anyone_as(t.path(), &reg, "line", "U_stranger", "@財務小幫手 x", Some("dudu")));
    assert!(turn::calls_anyone_as(t.path(), &reg, "line", "U_stranger", "@嘟嘟總機 x", Some("dudu")));
    // The mapped peer manager is told.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn(conv(), "U_peer", "@財務小幫手 x", "u3"));
    assert!(matches!(d, TurnDecision::Reply { kind: turn::ReplyKind::Refusal, .. }), "{d:?}");
    assert!(turn::calls_anyone_as(t.path(), &reg, "line", "U_peer", "@財務小幫手 x", Some("dudu")));
}
