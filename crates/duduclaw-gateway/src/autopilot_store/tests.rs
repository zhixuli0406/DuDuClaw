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
    let mut original = rule();
    original.action = json!({"type":"steps","steps":[{"type":"notify","message":"hello"}]}).to_string();
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
async fn insert_rule_rejects_extensions_without_steps_envelope() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    for action in [
        r#"{"type":"notify","message":"hello"}"#, "invalid JSON", "[]", "null",
        r#""steps""#, r#"{"type":1}"#, r#"{"type":"Steps"}"#, "{}",
    ] {
        for extension in ["owner_department", "active_window", "run_policy"] {
            let mut row = rule();
            row.action = action.into();
            match extension {
                "owner_department" => row.owner_department = Some("sales".into()),
                "active_window" => row.active_window = Some("{}".into()),
                "run_policy" => row.run_policy = Some("{}".into()),
                _ => unreachable!(),
            }
            assert!(store.insert_rule(&row).await.is_err());
            assert!(store.get_rule("r").await.unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn set_rule_extensions_requires_steps_and_rolls_back_notify_rule() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    original.action = json!({"type":"notify","message":"hello"}).to_string();
    store.insert_rule(&original).await.unwrap();
    let patch = RuleExtensionsPatch {
        active_window: Some(Some("{}".into())), run_policy: None, owner_department: None,
        updated_by_user_id: Some("editor".into()), updated_at: "2026-11-27T01:00:00.000Z".into(),
    };
    assert!(store.set_rule_extensions("r", &patch).await.is_err());
    let unchanged = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&unchanged).unwrap(), serde_json::to_value(&original).unwrap());
    let steps = json!({"type":"steps","steps":[{"type":"notify","message":"hello"}]});
    store.update_rule("r", &json!({"action":steps})).await.unwrap().unwrap();
    let changed = store.set_rule_extensions("r", &patch).await.unwrap().unwrap();
    assert_eq!(changed.active_window.as_deref(), Some("{}"));
    assert_eq!(changed.updated_by_user_id, patch.updated_by_user_id);
    assert_eq!(changed.updated_at, Some(patch.updated_at));
}

#[tokio::test]
async fn update_rule_rejects_plain_action_with_run_policy_and_rolls_back() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    original.action = json!({"type":"steps","steps":[{"type":"notify","message":"hello"}]}).to_string();
    original.run_policy = Some(r#"{"outside_window":"skip"}"#.into());
    store.insert_rule(&original).await.unwrap();
    let fields = json!({"name":"must roll back", "enabled":false,
        "action":{"type":"notify","message":"hello"}});
    assert!(store.update_rule("r", &fields).await.is_err());
    let unchanged = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&unchanged).unwrap(), serde_json::to_value(&original).unwrap());
}

#[tokio::test]
async fn rules_without_extensions_preserve_insert_and_update_semantics() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    // Action validation for legacy rules remains the RPC layer's responsibility.
    original.action = "invalid JSON".into();
    store.insert_rule(&original).await.unwrap();
    let unchanged = store.update_rule("r", &json!({"ignored":true})).await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(&unchanged).unwrap(), serde_json::to_value(&original).unwrap());
    let fields = json!({"name":"Updated", "enabled":false, "trigger_event":"other",
        "conditions":{"key":"value"}, "action":{"type":"notify","message":"hello"},
        "sequence":{"first":{}}, "metadata":{"source":"test"}});
    let changed = store.update_rule("r", &fields).await.unwrap().unwrap();
    assert_eq!(changed.name, "Updated");
    assert!(!changed.enabled);
    assert_eq!(changed.trigger_event, "other");
    assert_eq!(changed.conditions, fields["conditions"].to_string());
    assert_eq!(changed.action, fields["action"].to_string());
    assert_eq!(changed.sequence, Some(fields["sequence"].to_string()));
    assert_eq!(changed.metadata, Some(fields["metadata"].to_string()));
    let cleared = store.update_rule("r", &json!({"sequence":null,"metadata":null})).await.unwrap().unwrap();
    assert!(cleared.sequence.is_none() && cleared.metadata.is_none());
    assert!(cleared.active_window.is_none() && cleared.run_policy.is_none() && cleared.owner_department.is_none());
    assert!(store.update_rule("missing", &fields).await.unwrap().is_none());
    assert!(store.update_rule("missing", &json!({})).await.unwrap().is_none());
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

#[tokio::test]
async fn replace_definition_preserves_database_owned_fields() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    original.action = json!({"type":"steps", "steps":[{"type":"notify"}]}).to_string();
    original.owner_department = Some("sales".into());
    original.metadata = Some(json!({"induced":true}).to_string());
    original.trigger_count = 7;
    original.last_triggered_at = Some("last-fire".into());
    store.insert_rule(&original).await.unwrap();
    let mut definition = original.clone();
    definition.id = "ignored".into();
    definition.name = "Edited".into();
    definition.enabled = false;
    definition.trigger_event = "channel_message".into();
    definition.conditions = json!({"field":"text", "value":"hello"}).to_string();
    definition.sequence = Some(json!({"first":{"event":"task_created"}, "then":{"event":"channel_message"}, "within_secs":60}).to_string());
    definition.active_window = Some(json!({"days":[1], "start":"09:00", "end":"18:00", "timezone":"Asia/Taipei"}).to_string());
    definition.run_policy = Some(json!({"outside_window":"skip"}).to_string());
    definition.owner_department = Some("other".into());
    definition.created_by_user_id = Some("spoofed".into());
    definition.created_at = "spoofed".into();
    definition.metadata = None;
    definition.trigger_count = 0;
    definition.last_triggered_at = None;
    definition.updated_by_user_id = Some("spoofed".into());
    definition.updated_at = Some("spoofed".into());
    let updated = store.replace_rule_definition("r", &definition, Some("editor"), "now").await.unwrap().unwrap();
    let mut expected = definition;
    expected.id = original.id;
    expected.owner_department = original.owner_department;
    expected.created_by_user_id = original.created_by_user_id;
    expected.created_at = original.created_at;
    expected.metadata = original.metadata;
    expected.trigger_count = original.trigger_count;
    expected.last_triggered_at = original.last_triggered_at;
    expected.updated_by_user_id = Some("editor".into());
    expected.updated_at = Some("now".into());
    assert_eq!(serde_json::to_value(&updated).unwrap(), serde_json::to_value(&expected).unwrap());
    assert!(store.replace_rule_definition("missing", &expected, None, "later").await.unwrap().is_none());
}

#[tokio::test]
async fn replace_definition_rolls_back_when_stored_extensions_require_envelope() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    original.action = json!({"type":"steps", "steps":[{"type":"notify"}]}).to_string();
    original.owner_department = Some("sales".into());
    store.insert_rule(&original).await.unwrap();
    let mut definition = original.clone();
    definition.name = "must roll back".into();
    definition.action = json!({"type":"notify"}).to_string();
    // A stale caller cannot clear ownership through this API: validate the
    // actual database row, including ownership, before committing the write.
    definition.owner_department = None;
    assert!(store.replace_rule_definition("r", &definition, Some("editor"), "now").await.is_err());
    let stored = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(stored).unwrap(), serde_json::to_value(original).unwrap());
}

#[tokio::test]
async fn company_definition_replace_rechecks_persisted_department_before_writing() {
    let home = tempfile::tempdir().unwrap();
    let store = AutopilotStore::open(home.path()).unwrap();
    let mut original = rule();
    original.action = json!({"type":"steps", "steps":[{"type":"delegate", "target_agent":"plain", "prompt":"report"}]}).to_string();
    store.insert_rule(&original).await.unwrap();
    let mut stale_definition = original.clone();
    stale_definition.name = "must not write".into();
    store.set_rule_extensions("r", &RuleExtensionsPatch {
        owner_department: Some(Some("sales".into())), active_window: None, run_policy: None,
        updated_by_user_id: None, updated_at: "reassigned".into(),
    }).await.unwrap();
    let before = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(store.replace_company_rule_definition("r", &stale_definition, Some("admin"), "later")
        .await.unwrap_err(), "permission denied");
    let after = store.get_rule("r").await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(after).unwrap(), serde_json::to_value(before).unwrap());
}
