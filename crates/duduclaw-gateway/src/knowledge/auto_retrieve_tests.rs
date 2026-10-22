//! Automatic retrieval: who gets which passages, the threshold, the budget,
//! `kb_only`, and the rules flag.

use super::*;
use crate::knowledge::test_fixtures::{agent, collection};
use duduclaw_auth::UserRole;
use duduclaw_knowledge::acl::{PrincipalKind, Role};

struct Home {
    dir: tempfile::TempDir,
    users: Arc<UserDb>,
    hr_user: String,
    sales_user: String,
}

const HR_TEXT: &str = "特休假依年資計算，滿一年者給予七日特休假。";
const FAQ_TEXT: &str = "產品退貨請在收到商品七日內辦理，運費由公司負擔。";

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let h = dir.path();
    let users = Arc::new(UserDb::new(&h.join("users.db")).unwrap());
    let hr = users
        .create_user(
            "hr@example.test",
            "HR",
            "password-123456",
            UserRole::Employee,
        )
        .unwrap();
    users.set_department(&hr.id, Some("人資")).unwrap();
    let sales = users
        .create_user(
            "sales@example.test",
            "Sales",
            "password-123456",
            UserRole::Employee,
        )
        .unwrap();
    agent(h, "hr-bot", "[agent]\nname='hr-bot'\ndepartment='人資'\n");
    agent(
        h,
        "kb-bot",
        "[agent]\nname='kb-bot'\ndepartment='人資'\n[knowledge]\nanswer_mode='kb_only'\n",
    );
    agent(
        h,
        "tight-bot",
        "[agent]\nname='tight-bot'\ndepartment='人資'\n[knowledge]\ninject_budget_chars=500\ntop_k=1\n",
    );
    agent(h, "plain-bot", "[agent]\nname='plain-bot'\n");
    // hr-handbook: internal, the 人資 department (employees and people).
    collection(
        h,
        "hr-handbook",
        "internal",
        &[(PrincipalKind::Department, "人資", Role::Viewer)],
        HR_TEXT,
    );
    // product-faq: the HR employees plus every LINE sender.
    collection(
        h,
        "product-faq",
        "internal",
        &[
            (PrincipalKind::Department, "人資", Role::Viewer),
            (PrincipalKind::Channel, "line", Role::Viewer),
        ],
        FAQ_TEXT,
    );
    // personal notes of the HR user, granted to the HR employees too.
    collection(
        h,
        "hr-personal",
        "personal",
        &[
            (PrincipalKind::Department, "人資", Role::Viewer),
            (PrincipalKind::User, hr.id.as_str(), Role::Viewer),
        ],
        "特休假的個人備忘：我還有三天特休假。",
    );
    collection(
        h,
        "hr-secret",
        "restricted",
        &[
            (PrincipalKind::Department, "人資", Role::Viewer),
            (PrincipalKind::User, hr.id.as_str(), Role::Viewer),
        ],
        "特休假機密：主管名單。",
    );
    Home {
        dir,
        users,
        hr_user: hr.id,
        sales_user: sales.id,
    }
}

fn run(
    h: &Home,
    agent_id: &str,
    requester: RequesterIdentity,
    query: &str,
    turn: &str,
) -> TurnKnowledge {
    prepare_blocking(&TurnInput {
        home: h.dir.path().to_path_buf(),
        users: Some(h.users.clone()),
        agent_id: agent_id.into(),
        agent_dir: agent_dir_for(h.dir.path(), agent_id),
        requester,
        query: query.into(),
        turn_id: turn.into(),
    })
    .unwrap()
}

fn webchat(user: &str) -> RequesterIdentity {
    RequesterIdentity::DashboardUser {
        user_id: user.into(),
    }
}

fn line(chat: &str, sender: &str, user: Option<&str>) -> RequesterIdentity {
    RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: chat.into(),
        sender: sender.into(),
        user_id: user.map(str::to_string),
    }
}

fn sources_in(block: &str) -> Vec<String> {
    block
        .split("source=\"")
        .skip(1)
        .filter_map(|s| s.split('"').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn webchat_user_with_access_gets_numbered_passages() {
    let h = home();
    let _ledger = crate::knowledge::ledger::open("hr-bot", "turn-w1").unwrap();
    let t = run(&h, "hr-bot", webchat(&h.hr_user), "特休假怎麼算", "turn-w1");
    assert!(t.rules);
    let block = t.block.unwrap();
    let srcs = sources_in(&block);
    assert!(srcs.contains(&"hr-handbook.txt".to_string()), "{block}");
    // 1:1 WebChat: personal collections are readable, restricted never injected.
    assert!(srcs.contains(&"hr-personal.txt".to_string()), "{block}");
    assert!(!srcs.contains(&"hr-secret.txt".to_string()), "{block}");
    assert!(block.contains("<passage n=\"1\""));
    // The ledger holds the same numbers the block shows.
    let entries = crate::knowledge::ledger::entries("hr-bot", "turn-w1");
    assert_eq!(entries.len(), srcs.len());
    assert!(t.direct_reply.is_none());
}

#[test]
fn webchat_user_without_access_gets_nothing() {
    let h = home();
    let t = run(
        &h,
        "hr-bot",
        webchat(&h.sales_user),
        "特休假怎麼算",
        "turn-w2",
    );
    assert!(t.rules, "the employee has collections, so the rules apply");
    assert!(t.block.is_none());
    let t = run(
        &h,
        "hr-bot",
        webchat("no-such-user"),
        "特休假怎麼算",
        "turn-w3",
    );
    assert!(t.block.is_none());
}

#[test]
fn unbound_line_sender_only_gets_channel_granted_collections() {
    let h = home();
    let t = run(
        &h,
        "hr-bot",
        line("Ustranger", "Ustranger", None),
        "退貨要幾天內辦理",
        "turn-l1",
    );
    let block = t.block.unwrap();
    assert_eq!(sources_in(&block), vec!["product-faq.txt".to_string()]);
    let t = run(
        &h,
        "hr-bot",
        line("Ustranger", "Ustranger", None),
        "特休假怎麼算",
        "turn-l2",
    );
    // Nothing about leave is readable for a stranger: "none" block.
    assert_eq!(
        t.block.as_deref(),
        Some("<knowledge_passages status=\"none\"/>")
    );
}

#[test]
fn bound_line_sender_reads_their_collections_but_groups_exclude_personal() {
    let h = home();
    let t = run(
        &h,
        "hr-bot",
        line("Ubound", "Ubound", Some(&h.hr_user)),
        "特休假",
        "turn-b1",
    );
    let srcs = sources_in(&t.block.unwrap());
    assert!(srcs.contains(&"hr-personal.txt".to_string()), "{srcs:?}");
    let t = run(
        &h,
        "hr-bot",
        line("Cgroup1", "Ubound", Some(&h.hr_user)),
        "特休假",
        "turn-b2",
    );
    let srcs = sources_in(&t.block.unwrap());
    assert!(srcs.contains(&"hr-handbook.txt".to_string()), "{srcs:?}");
    assert!(
        !srcs.contains(&"hr-personal.txt".to_string()),
        "group chat: {srcs:?}"
    );
    assert!(!srcs.contains(&"hr-secret.txt".to_string()));
}

#[test]
fn system_turns_read_internal_collections_granted_to_the_employee() {
    let h = home();
    let sys = RequesterIdentity::System {
        kind: "cron".into(),
        on_behalf_of: None,
    };
    let t = run(&h, "hr-bot", sys, "特休假", "turn-s1");
    let srcs = sources_in(&t.block.unwrap());
    assert!(srcs.contains(&"hr-handbook.txt".to_string()));
    assert!(!srcs.contains(&"hr-personal.txt".to_string()));
    let t = run(
        &h,
        "hr-bot",
        RequesterIdentity::Unknown,
        "特休假",
        "turn-s2",
    );
    assert!(t.block.is_none());
}

#[test]
fn threshold_and_kb_only() {
    let h = home();
    // Nothing matches: assist ⇒ "none" block; kb_only ⇒ fixed reply.
    let t = run(
        &h,
        "hr-bot",
        webchat(&h.hr_user),
        "量子電腦的原理",
        "turn-t1",
    );
    assert_eq!(
        t.block.as_deref(),
        Some("<knowledge_passages status=\"none\"/>")
    );
    assert!(t.direct_reply.is_none());
    let t = run(
        &h,
        "kb-bot",
        webchat(&h.hr_user),
        "量子電腦的原理",
        "turn-t2",
    );
    assert_eq!(t.direct_reply.as_deref(), Some(inject::KB_ONLY_NO_MATCH));
    // kb_only with no readable collection: says so without naming any.
    let t = run(&h, "kb-bot", webchat(&h.sales_user), "特休假", "turn-t3");
    assert_eq!(
        t.direct_reply.as_deref(),
        Some(inject::KB_ONLY_NO_COLLECTIONS)
    );
    // kb_only with a match answers through the model.
    let t = run(&h, "kb-bot", webchat(&h.hr_user), "特休假", "turn-t4");
    assert!(t.direct_reply.is_none() && t.block.is_some());
    // Slash commands are never looked up.
    let t = run(&h, "hr-bot", webchat(&h.hr_user), "/status", "turn-t5");
    assert!(t.block.is_none() && t.rules);
}

#[test]
fn budget_and_top_k_bound_the_block() {
    let h = home();
    let t = run(&h, "tight-bot", webchat(&h.hr_user), "特休假", "turn-g1");
    let block = t.block.unwrap();
    assert_eq!(block.matches("<passage ").count(), 1);
    assert!(block.chars().count() < 900, "{block}");
}

#[test]
fn employee_without_collections_gets_no_rules() {
    let h = home();
    let t = run(&h, "plain-bot", webchat(&h.hr_user), "特休假", "turn-p1");
    assert_eq!(t, TurnKnowledge::default());
    // Turned off globally: rules stay (the tools still work), no lookup.
    std::fs::write(
        h.dir.path().join("config.toml"),
        "[knowledge]\nauto_retrieve = false\n",
    )
    .unwrap();
    let t = run(&h, "hr-bot", webchat(&h.hr_user), "特休假", "turn-p2");
    assert!(t.rules && t.block.is_none());
}

#[test]
fn rules_and_passages_are_placed() {
    assert!(with_rules("SOUL".into(), true).ends_with(inject::RULES_SECTION));
    assert_eq!(with_rules("SOUL".into(), false), "SOUL");
    assert_eq!(with_passages("問題", Some("<k/>")), "<k/>\n\n問題");
    assert_eq!(with_passages("問題", None), "問題");
}

#[test]
fn every_lookup_is_logged() {
    let h = home();
    run(&h, "hr-bot", webchat(&h.hr_user), "特休假", "turn-a1");
    let conn = schema::open_home(h.dir.path()).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_log WHERE op = 'auto_inject' AND turn_id = 'turn-a1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}
