//! `/交辦` from a chat: who may assign, what the task row carries, the
//! one-shot "next message is the task", and redelivery idempotency.

use std::path::Path;

use duduclaw_auth::{AccessLevel, GrantSource, UserDb, UserRole};

use super::*;
use crate::trigger_route::access;
use crate::trigger_route::tests::registry;
use crate::trigger_route::turn::{self, ChannelTurn, TurnDecision};

/// mgr (Manager, `U_mgr`) → lead (Manager) → emp1 (sales-rep Owner,
/// `U_emp1`); viewer (finance Viewer, `U_viewer`); peer (Manager, `U_peer`).
struct Org {
    mgr: String,
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
    let viewer = mk("viewer", UserRole::Employee);
    let peer = mk("peer", UserRole::Manager);
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.bind_agent(&emp1, "sales-rep", AccessLevel::Owner).unwrap();
    db.bind_agent(&viewer, "finance", AccessLevel::Viewer).unwrap();
    for (user, line_id) in [(&mgr, "U_mgr"), (&emp1, "U_emp1"), (&viewer, "U_viewer"), (&peer, "U_peer")] {
        db.bind_channel_identity(user, "line", line_id, true).unwrap();
    }
    Org { mgr, emp1 }
}

fn conv() -> &'static str {
    Box::leak(format!("line:{}", uuid::Uuid::new_v4()).into_boxed_str())
}

fn caller<'a>(home: &'a Path, conversation: &'a str, sender: &'a str, event: Option<&'a str>) -> AssignCaller<'a> {
    AssignCaller {
        home,
        channel: "line",
        conversation,
        sender,
        event,
        seq: pending::next_seq(),
        fallback: Some("dudu"),
        access: access::for_channel_sender(home, "line", sender),
    }
}

fn cmd(text: &str) -> AssignCommand {
    match crate::chat_commands::parse_command(text, None) {
        Some(crate::chat_commands::ChatCommand::Assign(a)) => a,
        other => panic!("not an assign command: {other:?}"),
    }
}

async fn run_line(reg: &AgentRegistry, c: &AssignCaller<'_>, text: &str) -> AssignReply {
    let decided = plan(reg, Lang::ZhTw, &cmd(text), c);
    execute(c.home, Lang::ZhTw, decided, c).await
}

fn audit_lines(home: &Path, event: &str) -> usize {
    std::fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(&format!("\"{event}\"")))
        .count()
}

#[test]
fn every_spelling_parses_and_lookalikes_do_not() {
    use crate::chat_commands::{is_command, parse_command, ChatCommand};
    for text in ["/交辦", "／交辦", "/assign", "/ASSIGN", "  /交辦  "] {
        assert!(is_command(text), "{text}");
        assert_eq!(parse_command(text, None), Some(ChatCommand::Assign(AssignCommand::List)), "{text}");
    }
    for text in ["/交辦 狀態", "/assign status", "／交辦 Status"] {
        assert_eq!(parse_command(text, None), Some(ChatCommand::Assign(AssignCommand::Status)), "{text}");
    }
    assert_eq!(
        parse_command("/交辦 @sales-rep 整理報價單 || 含比價", None),
        Some(ChatCommand::Assign(AssignCommand::Text("@sales-rep 整理報價單 || 含比價".into())))
    );
    assert!(!crate::chat_commands::is_assign_command("/交辦事項"));
    assert!(!crate::chat_commands::is_assign_command("/assignment"));
    assert!(!crate::chat_commands::is_assign_command("請 /交辦"));
}

#[tokio::test]
async fn an_unmapped_sender_is_asked_to_link_an_account_whatever_they_name() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = conv();
    let x = caller(t.path(), c, "U_x", None);
    for text in ["/交辦", "/交辦 狀態", "/交辦 @嘟嘟總機 x", "/交辦 @沒有這個人 x"] {
        // A fresh conversation each time: on LINE a repeat inside the
        // throttle window gets no answer at all.
        let x = caller(t.path(), conv(), "U_x", None);
        let r = run_line(&reg, &x, text).await;
        assert_eq!(r.text, t::needs_identity(Lang::ZhTw), "{text}");
        assert!(r.task.is_none());
    }
    let again = run_line(&reg, &x, "/交辦").await;
    assert_eq!(again.text, t::needs_identity(Lang::ZhTw));
    let throttled = run_line(&reg, &x, "/交辦").await;
    assert!(throttled.text.is_empty(), "a repeat within the window is silent");
    assert!(audit_lines(t.path(), "human_assign_refused") >= 1);
}

#[tokio::test]
async fn a_manager_assigns_to_a_skip_level_reports_employee() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let m = caller(t.path(), c, "U_mgr", None);
    assert_eq!(
        assign_right(&reg, &m.access, "sales-rep", Some("dudu")),
        AssignRight::Allowed(GrantSource::Team {
            via_user: o.emp1.clone(),
            depth: 2
        })
    );
    let r = run_line(&reg, &m, "/交辦 @sales-rep 整理報價單 || 含三家比價 || 時限:24h").await;
    let task = r.task.clone().expect("task created");
    assert_eq!(task.agent, "sales-rep");
    assert!(r.text.contains("【業務專員】"), "{}", r.text);
    assert!(r.text.contains("依組織關係交辦：emp1的 AI 員工"), "{}", r.text);
    assert!(r.text.contains("進度和結果會回到這個對話"), "{}", r.text);

    let store = crate::task_store::TaskStore::open(t.path()).unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    assert_eq!(row.assigned_to, "sales-rep");
    assert_eq!(row.on_behalf_of.as_deref(), Some(format!("user:{}", o.mgr).as_str()));
    assert_eq!(row.created_via.as_deref(), Some("line"));
    assert_eq!(row.created_by, "assign:line");
    assert_eq!(row.source_channel.as_deref(), Some("line"));
    assert_eq!(Some(row.source_chat_id.as_deref().unwrap()), c.strip_prefix("line:"));
    assert!(row.goal_mode);
    assert_eq!(row.kind, crate::task_store::TaskKind::Goal);
    assert_eq!(row.acceptance_criteria_baseline.as_deref(), Some("含三家比價"));
    assert!(row.deadline_at.is_some());
    assert_eq!(audit_lines(t.path(), "human_assign"), 1);
    // The activity row records who and through what.
    let acts = store.list_activity_for_task(&task.id, 50).await.unwrap();
    let created = acts.iter().find(|a| a.event_type == "goal_loop.created").expect("activity");
    let meta: serde_json::Value = serde_json::from_str(created.metadata.as_deref().unwrap()).unwrap();
    assert_eq!(meta["via"], format!("team:{}:2", o.emp1));
    assert_eq!(meta["created_via"], "line");

    // `/交辦 狀態` lists it with its title.
    let status = run_line(&reg, &m, "/交辦 狀態").await;
    assert!(status.text.contains("整理報價單"), "{}", status.text);
    assert!(status.text.contains(&task.id[..8]), "{}", status.text);
}

#[tokio::test]
async fn a_direct_owner_assigns_and_others_are_refused_with_the_right_text() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = conv();
    let emp1 = caller(t.path(), c, "U_emp1", None);
    let r = run_line(&reg, &emp1, "/交辦 @sales-rep 回覆客戶").await;
    assert!(r.task.is_some(), "{}", r.text);
    assert!(!r.text.contains("依組織關係"), "{}", r.text);

    // Viewer binding: may talk, may not assign.
    let viewer = caller(t.path(), c, "U_viewer", None);
    let r = run_line(&reg, &viewer, "/交辦 @財務小幫手 對帳").await;
    assert_eq!(r.text, t::call_only(Lang::ZhTw, "財務小幫手"));
    assert!(r.task.is_none());
    // The default employee is open for talking only.
    let viewer2 = caller(t.path(), conv(), "U_viewer", None);
    let r = run_line(&reg, &viewer2, "/交辦 @嘟嘟總機 x").await;
    assert_eq!(r.text, t::call_only(Lang::ZhTw, "嘟嘟總機"));
    // A peer manager: no grant at all → the explicit refusal.
    let peer = caller(t.path(), c, "U_peer", None);
    let r = run_line(&reg, &peer, "/交辦 @sales-rep x").await;
    assert_eq!(r.text, texts::forbidden(Lang::ZhTw));
    let r2 = run_line(&reg, &peer, "/交辦 @sales-rep y").await;
    assert!(r2.text.is_empty(), "the repeat inside the window is silent: {}", r2.text);
    // Refusals are audited, a repeat within the minute only once.
    let store = crate::task_store::TaskStore::open(t.path()).unwrap();
    assert_eq!(store.list_tasks(None, Some("finance"), None).await.unwrap().len(), 0);
    let refused = std::fs::read_to_string(t.path().join("security_audit.jsonl")).unwrap();
    let peer_denied = refused
        .lines()
        .filter(|l| l.contains("human_assign_refused") && l.contains("\"denied\""))
        .count();
    assert_eq!(peer_denied, 1);
}

#[tokio::test]
async fn the_list_holds_only_employees_the_sender_may_assign_to() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let m = caller(t.path(), conv(), "U_mgr", None);
    let r = run_line(&reg, &m, "/交辦").await;
    let ids: Vec<&str> = r.list.as_ref().unwrap().iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["sales-rep"]);
    assert!(r.text.contains("@sales-rep") || r.text.contains("@業務專員"), "{}", r.text);
    let v = caller(t.path(), conv(), "U_viewer", None);
    let r = run_line(&reg, &v, "/交辦").await;
    assert!(r.list.is_none());
    assert_eq!(r.text, t::nobody_assignable(Lang::ZhTw));
}

#[tokio::test]
async fn the_name_alone_waits_for_the_next_message_only() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let m = caller(t.path(), c, "U_mgr", Some("arm-1"));
    let r = run_line(&reg, &m, "/交辦 @sales-rep").await;
    assert!(r.text.contains("請說要交辦給「業務專員」的事"), "{}", r.text);
    assert!(r.task.is_none());

    let line_turn = |text: &'static str, event: &'static str| ChannelTurn {
        channel: "line",
        conversation: c,
        sender: "U_mgr",
        text,
        fallback: Some("dudu"),
        event,
        seq: pending::next_seq(),
        quiet: true,
        pending: turn::PendingPolicy::Live,
    };
    // A command in between neither uses nor drops the wait.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn("/status", "n0"));
    assert_eq!(d, TurnDecision::Default);
    // The next message is the task.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn("幫我追款", "n1"));
    assert_eq!(d, TurnDecision::Assign { id: "sales-rep".into() });
    // Recomputing the same event (LINE replay) gives the same answer.
    let mut replay = line_turn("幫我追款", "n1");
    replay.pending = turn::PendingPolicy::Replay(None);
    assert_eq!(
        turn::decide_channel_turn(t.path(), &reg, replay),
        TurnDecision::Assign { id: "sales-rep".into() }
    );
    let next = caller(t.path(), c, "U_mgr", Some("n1"));
    let decided = plan_pending(&reg, Lang::ZhTw, "sales-rep", "幫我追款", &next);
    let r = execute(t.path(), Lang::ZhTw, decided, &next).await;
    let task = r.task.expect("task");
    let store = crate::task_store::TaskStore::open(t.path()).unwrap();
    let row = store.get_task(&task.id).await.unwrap().unwrap();
    assert_eq!(row.description, "幫我追款");
    assert_eq!(row.on_behalf_of, Some(format!("user:{}", o.mgr)));
    // The message after that is ordinary conversation.
    let d = turn::decide_channel_turn(t.path(), &reg, line_turn("今天天氣", "n2"));
    assert_eq!(d, TurnDecision::Default);
}

#[tokio::test]
async fn the_wait_is_rechecked_when_it_is_used() {
    let (t, reg) = registry(false).await;
    let o = org(t.path());
    let c = conv();
    let m = caller(t.path(), c, "U_mgr", Some("arm-2"));
    run_line(&reg, &m, "/交辦 @sales-rep").await;
    // The reporting line changes before the task arrives.
    UserDb::new(&t.path().join("users.db"))
        .unwrap()
        .set_manager(&o.emp1, None)
        .unwrap();
    let next = caller(t.path(), c, "U_mgr", Some("arm-2-next"));
    let r = execute(
        t.path(),
        Lang::ZhTw,
        plan_pending(&reg, Lang::ZhTw, "sales-rep", "幫我追款", &next),
        &next,
    )
    .await;
    assert!(r.task.is_none());
    assert_eq!(r.text, texts::forbidden(Lang::ZhTw));
}

#[tokio::test]
async fn a_redelivered_message_creates_one_task() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = conv();
    let first = caller(t.path(), c, "U_emp1", Some("webhook-ev-1"));
    let a = run_line(&reg, &first, "/交辦 @sales-rep 寄出月報").await;
    let again = caller(t.path(), c, "U_emp1", Some("webhook-ev-1"));
    let b = run_line(&reg, &again, "/交辦 @sales-rep 寄出月報").await;
    assert_eq!(a.task, b.task);
    let store = crate::task_store::TaskStore::open(t.path()).unwrap();
    assert_eq!(store.list_tasks(None, Some("sales-rep"), None).await.unwrap().len(), 1);
}

#[tokio::test]
async fn admission_arms_the_wait_so_the_next_event_finds_it() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = conv();
    let adm = caller(t.path(), c, "U_mgr", Some("adm-1"));
    arm_on_admission(&reg, "/交辦 @sales-rep", &adm);
    let key = pending_key(&adm);
    assert_eq!(
        pending::peek_kind(&key, pending::next_seq()),
        Some(("sales-rep".to_string(), PendingKind::Assign))
    );
    // A command with a task, or a sender without the right, arms nothing.
    let c2 = conv();
    arm_on_admission(&reg, "/交辦 @sales-rep 有內容", &caller(t.path(), c2, "U_mgr", Some("adm-2")));
    arm_on_admission(&reg, "/交辦 @sales-rep", &caller(t.path(), c2, "U_peer", Some("adm-3")));
    assert!(pending::peek(&PendingKey::new("line", c2, "U_mgr"), pending::next_seq()).is_none());
    assert!(pending::peek(&PendingKey::new("line", c2, "U_peer"), pending::next_seq()).is_none());
}

#[test]
fn principals_are_only_account_ids() {
    assert_eq!(principal_for_user("a1b2-c3_d"), Some("user:a1b2-c3_d".into()));
    for bad in ["", "system", "dashboard", "cron", "a:b", "x y", &"a".repeat(65)] {
        assert_eq!(principal_for_user(bad), None, "{bad}");
    }
    assert_eq!(principal_of(&SenderAccess::unmapped()), None);
}
