//! Permission matrix for [`super::effective_collections`].

use super::*;
use crate::limits::KnowledgeConfig;
use crate::requester::test_support::users;
use crate::requester::{RequesterIdentity, agent_ctx, human_ctx};
use crate::store::{NewCollection, create_collection};
use duduclaw_core::knowledge_agent::KnowledgeScope;

fn coll(conn: &mut Connection, name: &str, owner: &str, sensitivity: &str) -> String {
    let n = NewCollection {
        name: name.into(),
        slug: Some(name.into()),
        sensitivity: Some(sensitivity.into()),
        ..Default::default()
    };
    create_collection(conn, &n, owner, &KnowledgeConfig::default())
        .unwrap()
        .id
}

fn grant(conn: &mut Connection, id: &str, extra: &[(PrincipalKind, &str, Role)], owner: &str) {
    let (_, rev) = get_acl(conn, id).unwrap();
    let mut entries = vec![(PrincipalKind::User, owner.to_string(), Role::Owner)];
    entries.extend(extra.iter().map(|(k, i, r)| (*k, i.to_string(), *r)));
    set_acl(conn, id, &entries, rev, owner).unwrap();
}

fn eff(conn: &Connection, human: &HumanCtx, agent: Option<&AgentCtx>) -> Vec<String> {
    effective_collections(conn, human, agent, &Narrowing::default())
        .unwrap()
        .ids()
        .to_vec()
}

fn agent(id: &str, dept: Option<&str>, scope: KnowledgeScope) -> AgentCtx {
    agent_ctx(id, dept, &scope)
}

fn system() -> HumanCtx {
    human_ctx(
        &RequesterIdentity::System {
            kind: "cron".into(),
            on_behalf_of: None,
        },
        &users(&[]),
    )
}

fn unbound(channel: &str, chat: &str) -> HumanCtx {
    human_ctx(
        &RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: chat.into(),
            sender: "Uxyz".into(),
            user_id: None,
        },
        &users(&[]),
    )
}

#[test]
fn default_deny_only_owner_sees_new_collection() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let id = coll(&mut conn, "hr", "owner", "internal");
    assert_eq!(
        eff(&conn, &HumanCtx::dashboard("owner", None, false), None),
        vec![id.clone()]
    );
    assert!(eff(&conn, &HumanCtx::dashboard("other", None, false), None).is_empty());
    assert!(
        eff(
            &conn,
            &system(),
            Some(&agent("bot", None, KnowledgeScope::All))
        )
        .is_empty()
    );
    assert!(
        eff(
            &conn,
            &unbound("line", "U1"),
            Some(&agent("bot", None, KnowledgeScope::All))
        )
        .is_empty()
    );
    assert!(eff(&conn, &HumanCtx::nobody(), None).is_empty());
}

#[test]
fn user_and_department_grants() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let id = coll(&mut conn, "hr", "owner", "internal");
    grant(
        &mut conn,
        &id,
        &[
            (PrincipalKind::User, "alice", Role::Viewer),
            (PrincipalKind::Department, "人資", Role::Editor),
        ],
        "owner",
    );
    assert_eq!(
        eff(&conn, &HumanCtx::dashboard("alice", None, false), None),
        vec![id.clone()]
    );
    assert_eq!(
        eff(
            &conn,
            &HumanCtx::dashboard("bob", Some("人資"), false),
            None
        ),
        vec![id.clone()]
    );
    assert!(
        eff(
            &conn,
            &HumanCtx::dashboard("carol", Some("業務"), false),
            None
        )
        .is_empty()
    );
    // Exact match only: no prefix / substring.
    assert!(
        eff(
            &conn,
            &HumanCtx::dashboard("dave", Some("人資部"), false),
            None
        )
        .is_empty()
    );
    assert!(eff(&conn, &HumanCtx::dashboard("alic", None, false), None).is_empty());
    assert_eq!(
        user_role(&conn, &id, "alice", None, false).unwrap(),
        Some(Role::Viewer)
    );
    assert_eq!(
        user_role(&conn, &id, "bob", Some("人資"), false).unwrap(),
        Some(Role::Editor)
    );
    assert_eq!(
        user_role(&conn, &id, "carol", Some("業務"), false).unwrap(),
        None
    );
    assert_eq!(
        user_role(&conn, &id, "root", None, true).unwrap(),
        Some(Role::Owner)
    );
    assert!(require_role(&conn, &id, "alice", None, false, Role::Editor).is_err());
    assert!(require_role(&conn, "col_missing", "root", None, true, Role::Viewer).is_err());
}

#[test]
fn agent_side_needs_its_own_grant_and_intersects_human_side() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let hr = coll(&mut conn, "hr", "owner", "internal");
    let faq = coll(&mut conn, "faq", "owner", "public");
    grant(
        &mut conn,
        &hr,
        &[
            (PrincipalKind::Department, "人資", Role::Viewer),
            (PrincipalKind::User, "alice", Role::Viewer),
        ],
        "owner",
    );
    grant(
        &mut conn,
        &faq,
        &[
            (PrincipalKind::Agent, "sales-bot", Role::Viewer),
            (PrincipalKind::Channel, "line", Role::Viewer),
        ],
        "owner",
    );
    let hr_bot = agent("hr-bot", Some("人資"), KnowledgeScope::All);
    let sales_bot = agent("sales-bot", Some("業務"), KnowledgeScope::All);
    // No person: internal-or-lower collections granted to the employee.
    assert_eq!(eff(&conn, &system(), Some(&hr_bot)), vec![hr.clone()]);
    assert_eq!(eff(&conn, &system(), Some(&sales_bot)), vec![faq.clone()]);
    // A person who may read hr talking to sales-bot still gets no hr.
    assert!(
        eff(
            &conn,
            &HumanCtx::dashboard("alice", None, false),
            Some(&sales_bot)
        )
        .is_empty()
    );
    assert_eq!(
        eff(
            &conn,
            &HumanCtx::dashboard("alice", None, false),
            Some(&hr_bot)
        ),
        vec![hr.clone()]
    );
    // Admin is all on the human side, never on the agent side.
    assert_eq!(
        eff(
            &conn,
            &HumanCtx::dashboard("root", None, true),
            Some(&sales_bot)
        ),
        vec![faq.clone()]
    );
    assert_eq!(
        eff(&conn, &HumanCtx::dashboard("root", None, true), None).len(),
        2
    );
}

#[test]
fn channel_grants_reach_unbound_senders_only_for_their_chat() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let faq = coll(&mut conn, "faq", "owner", "public");
    let vip = coll(&mut conn, "vip", "owner", "internal");
    grant(
        &mut conn,
        &faq,
        &[
            (PrincipalKind::Channel, "line", Role::Viewer),
            (PrincipalKind::Agent, "bot", Role::Viewer),
        ],
        "owner",
    );
    grant(
        &mut conn,
        &vip,
        &[
            (PrincipalKind::Channel, "line:C123", Role::Viewer),
            (PrincipalKind::Agent, "bot", Role::Viewer),
        ],
        "owner",
    );
    let bot = agent("bot", None, KnowledgeScope::All);
    let mut both = vec![faq.clone(), vip.clone()];
    both.sort();
    assert_eq!(eff(&conn, &unbound("line", "C123"), Some(&bot)), both);
    assert_eq!(
        eff(&conn, &unbound("line", "C999"), Some(&bot)),
        vec![faq.clone()]
    );
    assert!(eff(&conn, &unbound("telegram", "C123"), Some(&bot)).is_empty());
    // A dashboard user does not inherit channel grants.
    assert!(eff(&conn, &HumanCtx::dashboard("someone", None, false), None).is_empty());
}

#[test]
fn group_sessions_and_system_exclude_personal_and_restricted() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let personal = coll(&mut conn, "mine", "alice", "personal");
    let restricted = coll(&mut conn, "secret", "alice", "restricted");
    for id in [&personal, &restricted] {
        grant(
            &mut conn,
            id,
            &[
                (PrincipalKind::Agent, "bot", Role::Viewer),
                (PrincipalKind::Channel, "line", Role::Viewer),
            ],
            "alice",
        );
    }
    let bot = agent("bot", None, KnowledgeScope::All);
    assert_eq!(
        eff(
            &conn,
            &HumanCtx::dashboard("alice", None, false),
            Some(&bot)
        )
        .len(),
        2,
        "1:1 dashboard"
    );
    let group = HumanCtx {
        is_private: false,
        ..HumanCtx::dashboard("alice", None, false)
    };
    assert!(eff(&conn, &group, Some(&bot)).is_empty(), "group session");
    assert!(eff(&conn, &system(), Some(&bot)).is_empty(), "no person");
    // A LINE group (chat id is not the sender) is not private.
    assert!(eff(&conn, &unbound("line", "Cgroup"), Some(&bot)).is_empty());
    let one_to_one = human_ctx(
        &RequesterIdentity::ChannelUser {
            channel: "line".into(),
            chat: "Uxyz".into(),
            sender: "Uxyz".into(),
            user_id: None,
        },
        &users(&[]),
    );
    assert_eq!(eff(&conn, &one_to_one, Some(&bot)).len(), 2);
}

#[test]
fn narrowing_only_shrinks() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let a = coll(&mut conn, "a", "owner", "internal");
    let b = coll(&mut conn, "b", "owner", "internal");
    let c = coll(&mut conn, "c", "owner", "internal");
    for id in [&a, &b] {
        grant(
            &mut conn,
            id,
            &[(PrincipalKind::Agent, "bot", Role::Viewer)],
            "owner",
        );
    }
    let only_a = agent(
        "bot",
        None,
        KnowledgeScope::Only(vec!["a".into(), "c".into(), "ghost".into()]),
    );
    assert_eq!(
        eff(&conn, &system(), Some(&only_a)),
        vec![a.clone()],
        "c is not granted, ghost unknown"
    );
    assert!(
        eff(
            &conn,
            &system(),
            Some(&agent("bot", None, KnowledgeScope::Only(vec![])))
        )
        .is_empty()
    );
    assert!(
        eff(
            &conn,
            &system(),
            Some(&agent("bot", None, KnowledgeScope::Unreadable))
        )
        .is_empty()
    );
    let all = agent("bot", None, KnowledgeScope::All);
    let narrowed = effective_collections(
        &conn,
        &system(),
        Some(&all),
        &Narrowing {
            only: Some(vec!["b".into(), c.clone()]),
        },
    )
    .unwrap();
    assert_eq!(narrowed.ids(), std::slice::from_ref(&b));
    let none = effective_collections(
        &conn,
        &system(),
        Some(&all),
        &Narrowing { only: Some(vec![]) },
    )
    .unwrap();
    assert!(none.is_empty());
}

#[test]
fn malformed_acl_rows_never_grant() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let id = coll(&mut conn, "hr", "owner", "internal");
    conn.execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    for (kind, pid, role) in [
        ("user", "mallory", "superuser"),
        ("agent", "bot", "owner"),
        ("channel", "line", "editor"),
        ("department", "人資", "owner"),
    ] {
        conn.execute(
            "INSERT INTO collection_acl(collection_id, principal_kind, principal_id, role, granted_by, granted_at) VALUES (?1,?2,?3,?4,'x','x')",
            params![id, kind, pid, role],
        )
        .unwrap();
    }
    conn.execute_batch("PRAGMA ignore_check_constraints = OFF;")
        .unwrap();
    assert!(eff(&conn, &HumanCtx::dashboard("mallory", None, false), None).is_empty());
    assert_eq!(user_role(&conn, &id, "mallory", None, false).unwrap(), None);
    assert!(
        eff(
            &conn,
            &system(),
            Some(&agent("bot", None, KnowledgeScope::All))
        )
        .is_empty()
    );
    assert!(
        eff(
            &conn,
            &unbound("line", "U1"),
            Some(&agent("bot", None, KnowledgeScope::All))
        )
        .is_empty()
    );
    assert_eq!(
        user_role(&conn, &id, "d", Some("人資"), false).unwrap(),
        None
    );
}

#[test]
fn unreadable_database_is_an_error_not_an_empty_allow() {
    let conn = Connection::open_in_memory().unwrap(); // no schema
    assert!(
        effective_collections(
            &conn,
            &HumanCtx::dashboard("u", None, true),
            None,
            &Narrowing::default()
        )
        .is_err()
    );
}

#[test]
fn set_acl_rules() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let id = coll(&mut conn, "hr", "owner", "internal");
    let owner = (PrincipalKind::User, "owner".to_string(), Role::Owner);
    assert!(matches!(
        set_acl(&mut conn, &id, std::slice::from_ref(&owner), 7, "owner"),
        Err(KbError::Conflict(_))
    ));
    let rev = set_acl(&mut conn, &id, std::slice::from_ref(&owner), 0, "owner").unwrap();
    assert_eq!(rev, 1);
    assert!(
        set_acl(
            &mut conn,
            &id,
            &[(PrincipalKind::User, "x".into(), Role::Editor)],
            1,
            "owner"
        )
        .is_err(),
        "last owner"
    );
    for bad in [
        (PrincipalKind::Agent, "bot".to_string(), Role::Editor),
        (PrincipalKind::Channel, "line".to_string(), Role::Owner),
        (PrincipalKind::Department, "人資".to_string(), Role::Owner),
        (PrincipalKind::Agent, "../x".to_string(), Role::Viewer),
        (PrincipalKind::Channel, "LINE".to_string(), Role::Viewer),
        (PrincipalKind::Channel, "line:".to_string(), Role::Viewer),
        (PrincipalKind::Department, "a/b".to_string(), Role::Viewer),
    ] {
        assert!(
            set_acl(&mut conn, &id, &[owner.clone(), bad.clone()], 1, "owner").is_err(),
            "{bad:?}"
        );
    }
    assert!(
        set_acl(&mut conn, &id, &[owner.clone(), owner.clone()], 1, "owner").is_err(),
        "duplicate"
    );
    assert_eq!(
        get_acl(&conn, &id).unwrap().1,
        1,
        "failed writes changed nothing"
    );
}

#[test]
fn requester_resolution_fails_closed() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let id = coll(&mut conn, "hr", "alice", "internal");
    let dir = users(&[("alice", Some("人資"), false)]);
    let ok = human_ctx(
        &RequesterIdentity::DashboardUser {
            user_id: "alice".into(),
        },
        &dir,
    );
    assert_eq!(eff(&conn, &ok, None), vec![id.clone()]);
    for req in [
        RequesterIdentity::DashboardUser {
            user_id: "ghost".into(),
        },
        RequesterIdentity::DashboardUser {
            user_id: "broken".into(),
        },
        RequesterIdentity::System {
            kind: "dispatch".into(),
            on_behalf_of: Some("broken".into()),
        },
    ] {
        assert!(
            eff(&conn, &human_ctx(&req, &dir), None).is_empty(),
            "{req:?}"
        );
    }
    let bound = human_ctx(
        &RequesterIdentity::ChannelUser {
            channel: "line".into(),
            chat: "Ualice".into(),
            sender: "Ualice".into(),
            user_id: Some("alice".into()),
        },
        &dir,
    );
    assert_eq!(eff(&conn, &bound, None), vec![id.clone()]);
    let behalf = human_ctx(
        &RequesterIdentity::System {
            kind: "dispatch".into(),
            on_behalf_of: Some("alice".into()),
        },
        &dir,
    );
    assert_eq!(eff(&conn, &behalf, None), vec![id]);
}

#[test]
fn agent_side_alone_decides_whether_the_tools_exist() {
    let mut conn = crate::schema::open_in_memory().unwrap();
    let a = coll(&mut conn, "a", "owner", "personal");
    let b = coll(&mut conn, "b", "owner", "internal");
    grant(
        &mut conn,
        &a,
        &[(PrincipalKind::Agent, "bot", Role::Viewer)],
        "owner",
    );
    grant(
        &mut conn,
        &b,
        &[
            (PrincipalKind::Department, "人資", Role::Editor),
            (PrincipalKind::User, "x", Role::Viewer),
        ],
        "owner",
    );
    let side = |ag: &AgentCtx| agent_side_collections(&conn, ag).unwrap().ids().to_vec();
    let mut both = vec![a.clone(), b.clone()];
    both.sort();
    assert_eq!(
        side(&agent("bot", Some("人資"), KnowledgeScope::All)),
        both,
        "sensitivity is not part of the agent side"
    );
    assert_eq!(
        side(&agent("bot", None, KnowledgeScope::Only(vec!["b".into()]))),
        Vec::<String>::new()
    );
    assert!(
        side(&agent("other", None, KnowledgeScope::All)).is_empty(),
        "user grants do not count"
    );
    assert!(side(&agent("bot", None, KnowledgeScope::Unreadable)).is_empty());
    // Unknown requester serves nobody even with agent grants.
    let unknown = human_ctx(&RequesterIdentity::Unknown, &users(&[]));
    assert!(
        eff(
            &conn,
            &unknown,
            Some(&agent("bot", Some("人資"), KnowledgeScope::All))
        )
        .is_empty()
    );
}
