use super::*;
use serde_json::json;

fn rule() -> AutopilotRuleRow {
    AutopilotRuleRow {
        id: "r".into(), name: "Rule".into(), enabled: true,
        trigger_event: "task_created".into(), conditions: "{}".into(), action: "{}".into(),
        created_at: "2026-11-27T00:00:00.000Z".into(), last_triggered_at: None, trigger_count: 0,
        sequence: None, metadata: None, active_window: None, run_policy: None,
        owner_department: None, created_by_user_id: Some("creator".into()),
        updated_by_user_id: None, updated_at: None,
    }
}

#[tokio::test]
async fn v182_upgrade_preserves_rule_and_dispatch_with_null_extensions() {
    let home = tempfile::tempdir().unwrap();
    let conn = Connection::open(home.path().join("autopilot.db")).unwrap();
    conn.execute_batch(include_str!("../../../duduclaw-import/src/autopilot-v182.sql")).unwrap();
    conn.execute_batch("INSERT INTO autopilot_rules (id,name,trigger_event,created_at) VALUES ('r','Rule','task_created','old');
        INSERT INTO autopilot_approval_dispatch
        (dispatch_id,rule_id,rule_name,rule_hash,event_name,event_ref,dedupe_key,frozen_json,frozen_digest,state,created_at,updated_at)
        VALUES ('d','r','Rule','hash','task_created','ref','dedupe','{}','digest','requesting','old','old');").unwrap();
    drop(conn);
    let store = AutopilotStore::open(home.path()).unwrap();
    let rule = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(rule.name, "Rule");
    assert!(rule.active_window.is_none() && rule.run_policy.is_none() && rule.owner_department.is_none());
    assert!(rule.created_by_user_id.is_none() && rule.updated_by_user_id.is_none() && rule.updated_at.is_none());
    assert_eq!(store.list_rules().await.unwrap().len(), 1);
    let dispatch = store.get_dispatch("d").await.unwrap().unwrap();
    assert_eq!(dispatch.frozen_digest, "digest");
    assert_eq!(dispatch.run_id, None);
    assert_eq!(dispatch.step_index, None);
    let mut linked = dispatch.clone();
    linked.dispatch_id = "linked".into();
    linked.dedupe_key = "linked-dedupe".into();
    linked.run_id = Some("run".into());
    linked.step_index = Some(2);
    assert!(store.insert_dispatch(&linked).await.unwrap());
    assert_eq!(store.get_dispatch("linked").await.unwrap(), Some(linked));
}

#[tokio::test]
async fn rule_extensions_require_typed_patch_and_preserve_creator() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let original = rule();
    store.insert_rule(&original).await.unwrap();
    let raw = json!({"owner_department":"x","created_by_user_id":"evil", "updated_by_user_id":"evil",
        "updated_at":"evil", "active_window":{}, "run_policy":{}});
    let unchanged = store.update_rule("r", &raw).await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&unchanged).unwrap(), serde_json::to_value(&original).unwrap());
    let mut patch = RuleExtensionsPatch {
        active_window: Some(Some("{}".into())), run_policy: Some(Some("{}".into())),
        owner_department: Some(Some("sales".into())), updated_by_user_id: Some("editor".into()),
        updated_at: "2026-11-27T01:00:00.000Z".into(),
    };
    let changed = store.set_rule_extensions("r", &patch).await.unwrap().unwrap();
    assert_eq!(changed.active_window.as_deref(), Some("{}"));
    assert_eq!(changed.run_policy.as_deref(), Some("{}"));
    assert_eq!(changed.owner_department.as_deref(), Some("sales"));
    assert_eq!(changed.created_by_user_id.as_deref(), Some("creator"));
    assert_eq!(changed.updated_by_user_id.as_deref(), Some("editor"));
    assert_eq!(changed.updated_at.as_deref(), Some(patch.updated_at.as_str()));
    patch.active_window = None;
    patch.run_policy = Some(None);
    patch.owner_department = Some(None);
    patch.updated_by_user_id = None;
    patch.updated_at = "2026-11-27T02:00:00.000Z".into();
    let cleared = store.set_rule_extensions("r", &patch).await.unwrap().unwrap();
    assert_eq!(cleared.active_window, changed.active_window);
    assert_eq!(cleared.run_policy, None);
    assert_eq!(cleared.owner_department, None);
    assert_eq!(cleared.updated_by_user_id, None);
    assert_eq!(cleared.updated_at, Some(patch.updated_at.clone()));
    assert_eq!(cleared.created_by_user_id.as_deref(), Some("creator"));
    patch.active_window = Some(None);
    assert!(store.set_rule_extensions("r", &patch).await.unwrap().unwrap().active_window.is_none());
    assert!(store.set_rule_extensions("missing", &patch).await.unwrap().is_none());
    let displayed = crate::handlers::autopilot_rule_to_json(&changed);
    assert_eq!(displayed["active_window"], json!({}));
    assert_eq!(displayed["run_policy"], json!({}));
    assert_eq!(displayed["owner_department"], "sales");
    let mut malformed = changed;
    malformed.active_window = Some("invalid JSON".into());
    malformed.run_policy = Some("invalid JSON".into());
    let displayed = crate::handlers::autopilot_rule_to_json(&malformed);
    assert!(displayed["active_window"].is_null() && displayed["run_policy"].is_null());
}

#[tokio::test]
async fn append_step_history_is_retryable_without_bumping_trigger_count() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    store.insert_rule(&rule()).await.unwrap();
    let mut history = AutopilotHistoryRow {
        id: "step:r:0".into(), rule_id: "r".into(), rule_name: "Rule".into(),
        triggered_at: "2026-11-27T00:00:00.000Z".into(), result: "step_done".into(), details: None,
    };
    store.append_step_history(&history).await.unwrap();
    store.append_step_history(&history).await.unwrap();
    let unchanged = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(unchanged.trigger_count, 0);
    assert_eq!(unchanged.last_triggered_at, None);
    assert_eq!(store.list_history(Some("r"), 10).await.unwrap().len(), 1);
    history.id = "fire:r".into();
    store.append_history(&history).await.unwrap();
    let fired = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(fired.trigger_count, 1);
    assert_eq!(fired.last_triggered_at, Some(history.triggered_at));
}
