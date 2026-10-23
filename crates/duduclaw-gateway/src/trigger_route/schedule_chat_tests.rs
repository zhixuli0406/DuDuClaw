//! `/交辦 … || 時間:…` and `/交辦 排程` from a chat.

use std::path::Path;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_auth::{AccessLevel, UserDb, UserRole};

use super::*;
use crate::trigger_route::assign::{AssignCommand, AssignReply, execute, plan};
use crate::trigger_route::tests::registry;
use crate::trigger_route::{access, pending};

/// mgr (Manager, `U_mgr`) → lead → emp1 (sales-rep Owner); peer (`U_peer`).
fn org(home: &Path) -> (String, String) {
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
    db.set_manager(&lead, Some(&mgr)).unwrap();
    db.set_manager(&emp1, Some(&lead)).unwrap();
    db.bind_agent(&emp1, "sales-rep", AccessLevel::Owner)
        .unwrap();
    db.bind_channel_identity(&mgr, "line", "U_mgr", true)
        .unwrap();
    db.bind_channel_identity(&peer, "line", "U_peer", true)
        .unwrap();
    (mgr, peer)
}

fn caller<'a>(home: &'a Path, sender: &'a str) -> AssignCaller<'a> {
    let conversation: &'static str =
        Box::leak(format!("line:{}", uuid::Uuid::new_v4()).into_boxed_str());
    AssignCaller {
        home,
        channel: "line",
        conversation,
        sender,
        event: None,
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

async fn run(reg: &AgentRegistry, c: &AssignCaller<'_>, text: &str) -> AssignReply {
    let decided = plan(reg, Lang::ZhTw, &cmd(text), c);
    execute(c.home, Lang::ZhTw, decided, c).await
}

#[test]
fn the_schedule_segment_is_taken_out_and_the_rest_kept() {
    assert_eq!(
        split_schedule("@sales-rep 寫週報 || 時間:每週一 9:00 || 附上數字"),
        (
            Some("每週一 9:00".into()),
            "@sales-rep 寫週報 || 附上數字".into()
        )
    );
    assert_eq!(
        split_schedule("寫週報 ||排程：平日 9點"),
        (Some("平日 9點".into()), "寫週報 ".into())
    );
    assert_eq!(
        split_schedule("寫週報 || 時限:24h"),
        (None, "寫週報 || 時限:24h".into()),
        "時限 is a duration"
    );
    assert_eq!(
        split_schedule("時間:明天 寫週報"),
        (None, "時間:明天 寫週報".into()),
        "the first segment is the task"
    );
    let outcome = r#"x || outcome:{"a":"b || at:1"}"#;
    assert_eq!(
        split_schedule(outcome),
        (None, outcome.into()),
        "nothing inside an outcome payload is read"
    );
}

#[test]
fn schedules_subcommands_parse() {
    use crate::chat_commands::{ChatCommand, parse_command};
    let s = |t: &str| parse_command(t, None);
    assert_eq!(
        s("/交辦 排程"),
        Some(ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::List
        )))
    );
    assert_eq!(
        s("/assign schedules"),
        Some(ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::List
        )))
    );
    assert_eq!(
        s("/交辦 排程 暫停 #ab12cd34"),
        Some(ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::Pause("ab12cd34".into())
        )))
    );
    assert_eq!(
        s("/assign schedule remove ab12cd34"),
        Some(ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::Remove("ab12cd34".into())
        )))
    );
    assert_eq!(
        s("/交辦 排程 亂打"),
        Some(ChatCommand::Assign(AssignCommand::Schedules(
            ScheduleCommand::Usage
        )))
    );
    // `排程報告` is a task text, not the sub-command.
    assert_eq!(
        s("/交辦 排程報告"),
        Some(ChatCommand::Assign(AssignCommand::Text("排程報告".into())))
    );
}

#[tokio::test]
async fn a_team_manager_schedules_lists_pauses_and_removes_from_a_chat() {
    let (t, reg) = registry(false).await;
    let (mgr, _) = org(t.path());
    let c = caller(t.path(), "U_mgr");
    let r = run(&reg, &c, "/交辦 @sales-rep 寫週報 || 時間:每週一 9:00").await;
    assert!(r.text.contains("已排程"), "{}", r.text);
    assert!(r.text.contains("每週一 09:00"), "{}", r.text);
    assert!(r.task.is_none(), "nothing is created now");

    let store = TaskStore::open(t.path()).unwrap();
    let rows = store.list_assign_schedules(Some(&mgr), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(
        (
            row.agent_id.as_str(),
            row.kind.as_str(),
            row.created_via.as_str()
        ),
        ("sales-rep", "goal", "line")
    );
    assert_eq!(row.source_channel.as_deref(), Some("line"));
    assert!(
        store
            .list_by_principal(&format!("user:{mgr}"), false, 10)
            .await
            .unwrap()
            .is_empty()
    );

    let list = run(&reg, &c, "/交辦 排程").await.text;
    let short = duduclaw_core::truncate_chars(&row.id, 8);
    assert!(
        list.contains(&format!("#{short}")) && list.contains("業務專員"),
        "{list}"
    );
    let paused = run(&reg, &c, &format!("/交辦 排程 暫停 {short}"))
        .await
        .text;
    assert!(paused.contains("已暫停"), "{paused}");
    assert_eq!(
        store
            .get_assign_schedule(&row.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "paused"
    );
    let removed = run(&reg, &c, &format!("/交辦 排程 刪除 #{short}"))
        .await
        .text;
    assert!(removed.contains("已刪除"), "{removed}");
    assert_eq!(
        run(&reg, &c, "/交辦 排程").await.text,
        t::list_empty(Lang::ZhTw)
    );
}

#[tokio::test]
async fn a_peer_cannot_schedule_or_touch_someone_elses_schedule() {
    let (t, reg) = registry(false).await;
    let (mgr, _) = org(t.path());
    let mgr_c = caller(t.path(), "U_mgr");
    run(&reg, &mgr_c, "/交辦 @sales-rep 寫週報 || 排程:平日 9:00").await;
    let peer = caller(t.path(), "U_peer");
    let refused = run(&reg, &peer, "/交辦 @sales-rep 寫週報 || 排程:平日 9:00").await;
    assert!(!refused.text.contains("已排程"), "{}", refused.text);
    let store = TaskStore::open(t.path()).unwrap();
    let rows = store.list_assign_schedules(None, 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].creator_user_id, mgr);
    let short = duduclaw_core::truncate_chars(&rows[0].id, 8);
    let r = run(&reg, &peer, &format!("/交辦 排程 刪除 {short}"))
        .await
        .text;
    assert_eq!(
        r,
        t::manage_error(Lang::ZhTw, &crate::assign_schedule::ManageError::NotFound)
    );
    assert_eq!(
        store
            .get_assign_schedule(&rows[0].id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
}

#[tokio::test]
async fn a_bad_time_or_a_non_admin_cron_is_explained() {
    let (t, reg) = registry(false).await;
    org(t.path());
    let c = caller(t.path(), "U_mgr");
    let bad = run(&reg, &c, "/交辦 @sales-rep 寫週報 || 時間:下禮拜找時間")
        .await
        .text;
    assert_eq!(bad, t::when_usage(Lang::ZhTw));
    let cron = run(&reg, &c, "/交辦 @sales-rep 寫週報 || 排程:cron 0 9 * * 1")
        .await
        .text;
    assert!(cron.contains("管理員"), "{cron}");
    let store = TaskStore::open(t.path()).unwrap();
    assert!(
        store
            .list_assign_schedules(None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
