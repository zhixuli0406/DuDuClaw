//! Removed-name reservation (`duduclaw_core::agent_trash`): a supervisor may
//! remove a subordinate, but may not recreate the same name and thereby hand
//! the seat to an employee without the operator's contract / capabilities.

use super::*;

fn text(res: &Value) -> String {
    res["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn security_events(home: &std::path::Path, kind: &str) -> Vec<Value> {
    fs::read_to_string(home.join("security_audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event_type"] == kind)
        .collect()
}

#[tokio::test]
async fn remove_then_recreate_same_name_is_refused_for_ai_caller() {
    let tmp = delegation_home();
    let home = tmp.path();

    let res = handle_agent_remove(&serde_json::json!({ "agent_id": "sales-rep2" }), home, "sales-lead").await;
    assert_ne!(res["isError"], true, "{res}");

    // Audited as an AI removal, with caller and subject.
    let removed = security_events(home, "agent_removed");
    assert_eq!(removed.len(), 1, "{removed:?}");
    assert_eq!(removed[0]["agent_id"], "sales-lead");
    assert_eq!(removed[0]["details"]["subject"], "sales-rep2");

    let res = handle_create_agent(
        &serde_json::json!({ "name": "sales-rep2", "display_name": "新業務", "reports_to": "sales-lead" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    let msg = text(&res);
    assert!(msg.contains("已被移除") && msg.contains("儀表板") && msg.contains("其他名稱"), "{msg}");
    assert!(!msg.contains('/') && !msg.contains("_trash"), "no paths in the refusal: {msg}");
    assert!(!home.join("agents").join("sales-rep2").exists(), "nothing scaffolded");

    let refused = security_events(home, "agent_name_reserved");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["agent_id"], "sales-lead");
    assert_eq!(refused[0]["details"]["requested_name"], "sales-rep2");
    assert_eq!(refused[0]["details"]["reason"], "removed_to_trash");
}

#[tokio::test]
async fn fresh_name_is_still_allowed() {
    let tmp = delegation_home();
    let home = tmp.path();
    handle_agent_remove(&serde_json::json!({ "agent_id": "sales-rep2" }), home, "sales-lead").await;

    // Shares a prefix with the removed id — must not be caught by it.
    let res = handle_create_agent(
        &serde_json::json!({ "name": "sales-rep", "display_name": "x" }),
        home,
        "sales-lead",
    )
    .await;
    // `sales-rep` exists already, so this is the ordinary duplicate error,
    // not the reservation.
    assert!(!text(&res).contains("保留"), "{res}");

    let res = handle_create_agent(
        &serde_json::json!({ "name": "sales-rep3", "display_name": "業務三" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(home.join("agents").join("sales-rep3").join("agent.toml").exists());
}

#[tokio::test]
async fn trash_entry_with_longer_id_does_not_reserve_shorter_name() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::create_dir_all(home.join("agents/_trash/intern-east_20261002101010")).unwrap();
    let res = handle_create_agent(
        &serde_json::json!({ "name": "intern", "display_name": "實習生" }),
        home,
        "sales-rep",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
}

#[tokio::test]
async fn unlistable_trash_refuses() {
    let tmp = delegation_home();
    let home = tmp.path();
    // `_trash` exists but is not a listable directory.
    fs::write(home.join("agents/_trash"), "x").unwrap();
    let res = handle_create_agent(
        &serde_json::json!({ "name": "intern", "display_name": "實習生" }),
        home,
        "sales-rep",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(text(&res).contains("無法確認"), "{res}");
    assert!(!home.join("agents/intern").exists());
    let refused = security_events(home, "agent_name_reserved");
    assert_eq!(refused[0]["details"]["reason"], "trash_unlistable");
}

/// `mv agents/x <elsewhere>` by hand leaves the `org.toml` record behind;
/// the name is reserved the same way.
#[tokio::test]
async fn directory_moved_away_by_hand_keeps_name_reserved() {
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    fs::rename(home.join("agents/writer"), home.join("writer-moved")).unwrap();

    let res = handle_create_agent(
        &serde_json::json!({ "name": "writer", "display_name": "寫手" }),
        home,
        "ceo",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(text(&res).contains("已被移除"), "{res}");
}

#[test]
fn ai_removal_text_has_no_path_and_no_eraser() {
    let m = crate::mcp::removal_message_for_ai("sales-rep2");
    assert!(m.contains("sales-rep2") && m.contains("管理者") && m.contains("保留"), "{m}");
    assert!(!m.contains("rm") && !m.contains('/') && !m.contains("_trash"), "{m}");
}

#[tokio::test]
async fn removal_result_has_no_path_and_no_eraser() {
    let tmp = delegation_home();
    let home = tmp.path();
    let res = handle_agent_remove(&serde_json::json!({ "agent_id": "writer" }), home, "ceo").await;
    assert_ne!(res["isError"], true, "{res}");
    let msg = text(&res);
    assert!(!msg.contains("rm ") && !msg.contains("rm -rf"), "{msg}");
    assert!(!msg.contains(&home.display().to_string()) && !msg.contains("_trash"), "{msg}");
}
