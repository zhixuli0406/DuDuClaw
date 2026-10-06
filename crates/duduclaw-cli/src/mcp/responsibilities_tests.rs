//! P2-A C9 / ET5.3 (MCP side): employees reach only their own
//! responsibilities, can schedule a follow-up only during their own open
//! occurrence, system-sender names are not identities, operators cannot act
//! as an employee, there is no steering tool, and the tools stay hidden while
//! the feature is off.

use super::*;
use serde_json::json;
use std::fs;

fn write_agent(home: &std::path::Path, name: &str, department: &str) {
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("agent.toml"),
        format!(
            "[agent]\nname = \"{name}\"\ndisplay_name = \"{name}\"\nrole = \"specialist\"\n\
             status = \"active\"\ntrigger = \"@{name}\"\nreports_to = \"\"\nicon = \"🤖\"\n\
             department = \"{department}\"\n"
        ),
    )
    .unwrap();
}

fn home(enabled: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("config.toml"),
        format!("[dispatch]\nenabled = true\n\n[responsibilities]\nenabled = {enabled}\n"),
    )
    .unwrap();
    write_agent(dir.path(), "sales-rep", "業務");
    write_agent(dir.path(), "mkt-rep", "行銷");
    dir
}

async fn responsibility_of(home: &std::path::Path, owner: &str) -> String {
    let store = duduclaw_gateway::task_store::TaskStore::open(home).unwrap();
    let now = chrono::Utc::now();
    let input = duduclaw_gateway::responsibility::service::ResponsibilityInput {
        owner_agent_id: owner.into(),
        objective: "每天整理信件".into(),
        acceptance_template: "摘要".into(),
        source_refs: vec![],
        notification_policy: None,
        schedule: Some(duduclaw_gateway::responsibility::service::ScheduleSpec {
            cron: "0 0 9 * * *".into(),
            timezone: "Asia/Taipei".into(),
        }),
        event_subscriptions: vec![],
        occurrence_hours: 4,
        occurrence_cost_cap_cents: 100,
        budget_period: "day".into(),
        budget_timezone: "Asia/Taipei".into(),
        period_cost_limit_cents: 1000,
        period_occurrence_limit: 5,
        min_wake_interval_secs: 300,
        max_consecutive_failures: 3,
        stop_at: now + chrono::Duration::days(5),
    };
    duduclaw_gateway::responsibility::service::create(&store, home, &input, "op", now)
        .await
        .unwrap()
        .responsibility_id
}

fn text(v: &Value) -> String {
    v.to_string()
}

fn is_error(v: &Value) -> bool {
    v.get("isError").and_then(|b| b.as_bool()) == Some(true)
}

#[tokio::test]
async fn tools_refuse_while_the_feature_is_off() {
    let h = home(false);
    let args = json!({});
    let out = handle_responsibility_get(&args, h.path(), RecordActor::Agent("sales-rep")).await;
    assert!(is_error(&out), "{out}");
}

#[tokio::test]
async fn another_employees_responsibility_reads_as_not_found() {
    let h = home(true);
    let id = responsibility_of(h.path(), "sales-rep").await;
    let args = json!({"responsibility_id": id});
    let own = handle_responsibility_get(&args, h.path(), RecordActor::Agent("sales-rep")).await;
    assert!(!is_error(&own), "{own}");
    let other = handle_responsibility_get(&args, h.path(), RecordActor::Agent("mkt-rep")).await;
    assert!(is_error(&other));
    assert!(text(&other).contains("找不到"), "{other}");
    let list = handle_responsibility_get(&json!({}), h.path(), RecordActor::Agent("mkt-rep")).await;
    assert!(
        !text(&list).contains(&id),
        "list shows only the caller's own: {list}"
    );
}

#[tokio::test]
async fn followup_needs_an_open_occurrence_and_an_employee_identity() {
    let h = home(true);
    let id = responsibility_of(h.path(), "sales-rep").await;
    let due = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let args = json!({"responsibility_id": id, "due_at": due});
    let no_occ =
        handle_responsibility_followup(&args, h.path(), RecordActor::Agent("sales-rep")).await;
    assert!(is_error(&no_occ), "{no_occ}");
    let sys = handle_responsibility_followup(&args, h.path(), RecordActor::Agent("cron")).await;
    assert!(is_error(&sys));
    assert!(text(&sys).contains("系統保留名稱"), "{sys}");
    let op = handle_responsibility_followup(&args, h.path(), RecordActor::Operator("ceo")).await;
    assert!(is_error(&op));
    let other =
        handle_responsibility_followup(&args, h.path(), RecordActor::Agent("mkt-rep")).await;
    assert!(is_error(&other));
    let store = duduclaw_gateway::task_store::TaskStore::open(h.path()).unwrap();
    let armed = store.list_wakeups(&id).await.unwrap();
    assert!(
        armed.iter().all(|w| !w.armed_by.starts_with("agent:")),
        "no employee wake-up was armed: {armed:?}"
    );
}

#[test]
fn there_is_no_employee_steering_tool_and_no_widening_tool() {
    let names: Vec<&str> = super::tools_def::tools().map(|t| t.name).collect();
    for n in &names {
        assert!(!n.contains("steer"), "employee steering tool exposed: {n}");
    }
    for n in RESPONSIBILITY_TOOLS {
        assert!(names.contains(&n), "{n} declared");
    }
    for widening in [
        "responsibility_create",
        "responsibility_enable",
        "responsibility_resume",
        "responsibility_update",
        "responsibility_clear_failures",
    ] {
        assert!(!names.contains(&widening), "{widening} must not exist");
    }
}
