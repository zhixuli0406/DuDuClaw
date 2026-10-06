use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[tokio::test]
async fn shared_store_rolls_back_owner_schema_and_rows_together() {
    let store = WorkflowStore::open_in_memory().unwrap();
    let result: Result<(), String> = store
        .with_transaction(|tx| {
            tx.execute_batch("CREATE TABLE owner_drafts(id TEXT PRIMARY KEY);INSERT INTO owner_drafts VALUES('draft');")
                .map_err(|e| e.to_string())?;
            Err("owner validation failed".into())
        })
        .await;
    assert!(result.is_err());
    assert!(
        !store
            .with_connection(|c| c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='owner_drafts')",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .map_err(|e| e.to_string()))
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn revision_immutable_and_schema_unknown_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    let store = WorkflowStore::open(home.path()).unwrap();
    store
        .with_transaction(|tx| {
            tx.execute("INSERT INTO workflow_revisions VALUES('w',1,'h','{}')", [])
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE workflow_revisions SET revision_hash='different'",
                    [],
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            })
            .await
            .is_err()
    );
    store
        .with_transaction(|tx| {
            tx.execute("UPDATE workflow_schema_meta SET version=99", [])
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    drop(store);
    assert!(WorkflowStore::open(home.path()).is_err());
}
#[test]
fn typed_values_reject_unknown_fields_and_wrong_refs() {
    let schema = TypedSchema::Object {
        properties: BTreeMap::from([("count".into(), TypedSchema::Integer)]),
        required: BTreeSet::from(["count".into()]),
    };
    assert!(
        schema
            .validate(&serde_json::json!({"count":1,"escalate":true}))
            .is_err()
    );
    assert!(schema.validate(&serde_json::json!({"count":"1"})).is_err());
    let mut def = WorkflowDefinition {
        schema_version: 1,
        workflow_id: "weekly".into(),
        revision: 1,
        skill_revision_hash: "skill".into(),
        input_schema: schema.clone(),
        output_schema: schema.clone(),
        required_capabilities: BTreeSet::new(),
        steps: vec![StepDefinition {
            step_id: "one".into(),
            action: StepAction::Process {
                transform: ProcessTransform::Identity,
            },
            input: InputRef::StepOutput {
                step_id: "later".into(),
                pointer: "".into(),
            },
            input_schema: schema.clone(),
            output_schema: schema,
            timeout_seconds: 10,
            max_read_attempts: 1,
        }],
    };
    assert!(def.validate(&BTreeSet::new(), &BTreeSet::new()).is_err());
    def.steps[0].input = InputRef::Literal {
        value: serde_json::json!({"count":1}),
    };
    assert!(def.validate(&BTreeSet::new(), &BTreeSet::new()).is_ok());
    let raw = serde_json::to_string(&def).unwrap();
    assert!(
        WorkflowDefinition::parse(
            &(raw + " trailing prose"),
            &BTreeSet::new(),
            &BTreeSet::new()
        )
        .is_err()
    );
}

#[test]
fn scheduled_trigger_utc_slot_is_unique_across_equivalent_offsets() {
    let a = Trigger::Scheduled {
        cron_id: "weekly".into(),
        timezone: "Asia/Taipei".into(),
        scheduled_at: "2026-10-04T08:00:00+08:00".into(),
    };
    let b = Trigger::Scheduled {
        cron_id: "weekly".into(),
        timezone: "Asia/Taipei".into(),
        scheduled_at: "2026-10-04T00:00:00Z".into(),
    };
    assert_eq!(a.key("workflow", 1).unwrap(), b.key("workflow", 1).unwrap());
}
#[test]
fn typed_reference_contract_rejects_missing_or_incompatible_source() {
    let object = TypedSchema::Object {
        properties: BTreeMap::from([("count".into(), TypedSchema::Integer)]),
        required: BTreeSet::from(["count".into()]),
    };
    let mut d = WorkflowDefinition {
        schema_version: 1,
        workflow_id: "weekly".into(),
        revision: 1,
        skill_revision_hash: "skill".into(),
        input_schema: object,
        output_schema: TypedSchema::Integer,
        required_capabilities: BTreeSet::new(),
        steps: vec![StepDefinition {
            step_id: "one".into(),
            action: StepAction::Process {
                transform: ProcessTransform::Identity,
            },
            input: InputRef::RunInput {
                pointer: "/count".into(),
            },
            input_schema: TypedSchema::Integer,
            output_schema: TypedSchema::Integer,
            timeout_seconds: 10,
            max_read_attempts: 1,
        }],
    };
    assert!(d.validate(&BTreeSet::new(), &BTreeSet::new()).is_ok());
    d.steps[0].input_schema = TypedSchema::String { max_length: 10 };
    assert!(d.validate(&BTreeSet::new(), &BTreeSet::new()).is_err());
    d.steps[0].input_schema = TypedSchema::Integer;
    d.steps[0].input = InputRef::RunInput {
        pointer: "/missing".into(),
    };
    assert!(d.validate(&BTreeSet::new(), &BTreeSet::new()).is_err());
}
