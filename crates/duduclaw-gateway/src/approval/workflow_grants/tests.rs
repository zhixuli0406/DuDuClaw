use super::*;
use crate::workflow::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

#[tokio::test]
async fn broker_premint_revocation_survives_reopen_and_refuses_prepare() {
    let (home, broker, _store, accepted, _run) = fixture().await;
    assert!(
        broker
            .revoke_workflow_activation(
                &accepted.activation_id,
                &accepted.spec_hash,
                "stop before mint"
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(broker.prepare_revision_grant(&accepted).await.is_err());
    drop(broker);
    let reopened = ApprovalBroker::open(home.path()).unwrap();
    assert!(reopened.prepare_revision_grant(&accepted).await.is_err());
    assert!(
        reopened
            .current_revision_grant(&accepted.activation_id)
            .await
            .unwrap()
            .is_none()
    );
}

async fn human_operation(
    broker: &ApprovalBroker,
    store: &WorkflowStore,
    accepted: &AcceptedRevisionGrant,
    run: WorkflowRun,
) -> (GrantRef, String, ExecutionBinding) {
    let (grant, _, mut binding) = operation(broker, store, accepted, run).await;
    let mut next: WorkflowRun = store
        .with_transaction(|tx| {
            let raw: String = tx
                .query_row(
                    "SELECT record_json FROM workflow_runs WHERE run_id=?1",
                    params![binding.run_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            serde_json::from_str(&raw).map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    next.run_id = uuid::Uuid::new_v4().to_string();
    next.trigger = Trigger::Manual {
        request_id: uuid::Uuid::new_v4().to_string(),
    };
    next.trigger_key = next.trigger.key(&next.workflow_id, next.revision).unwrap();
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,'running',?7)",
                params![
                    next.run_id,
                    next.trigger_key,
                    next.workflow_id,
                    next.revision,
                    next.workflow_hash,
                    serde_json::to_string(&next).unwrap(),
                    next.created_at
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    // F1b: the runner reserves the effect before preparing it.
    store
        .charge_step(
            store.path().unwrap().parent().unwrap(),
            &next.run_id,
            "update",
            crate::workflow::cost_ledger::ChargeKind::Effect,
        )
        .await
        .unwrap();
    binding.run_id = next.run_id;
    let payload = json!({"name":"tasks_update","arguments":next.input});
    let approval = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "Ask exact action",
            payload,
            binding.clone(),
        )
        .await
        .unwrap();
    broker
        .decide_bound(&approval, &binding.decision_context, true)
        .await
        .unwrap();
    let id = broker
        .prepare_operation(&approval, "update", Some("human-provider"))
        .await
        .unwrap();
    (grant, id, binding)
}

#[tokio::test]
async fn approved_human_step_is_still_fenced_by_activation_revocation_after_reopen() {
    let (home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = human_operation(&broker, &store, &accepted, run).await;
    let claim = broker
        .claim_operation(&id, &binding, "human-worker", 60)
        .await
        .unwrap();
    broker
        .revoke_workflow_activation(
            &accepted.activation_id,
            &accepted.spec_hash,
            "stop before begin",
        )
        .await
        .unwrap();
    assert!(broker.begin_execution(&claim, &binding).await.is_err());
    assert!(matches!(
        broker
            .inspect_operation(&id)
            .await
            .unwrap()
            .unwrap()
            .authority,
        OperationAuthority::BoundHumanApproval { .. }
    ));
    assert_eq!(
        broker.inspect_operation(&id).await.unwrap().unwrap().state,
        OperationState::Prepared
    );
    drop(broker);
    let reopened = ApprovalBroker::open(home.path()).unwrap();
    assert!(
        reopened
            .claim_operation(&id, &binding, "new-worker", 60)
            .await
            .is_err()
    );
    assert!(reopened.inspect_revision_grant(&grant).await.is_err());
}

#[tokio::test]
async fn human_begin_winner_retains_receipt_and_is_reported_by_revoke() {
    let (_home, broker, store, accepted, run) = fixture().await;
    let (_grant, id, binding) = human_operation(&broker, &store, &accepted, run).await;
    let claim = broker
        .claim_operation(&id, &binding, "human-worker", 60)
        .await
        .unwrap();
    broker.begin_execution(&claim, &binding).await.unwrap();
    let revoked = broker
        .revoke_workflow_activation(
            &accepted.activation_id,
            &accepted.spec_hash,
            "stop future actions",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(revoked.executing_operations, vec![id.clone()]);
    broker
        .settle_operation(
            &claim,
            OperationState::Succeeded,
            Some(json!({"actual_row":"owned"})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        broker.inspect_operation(&id).await.unwrap().unwrap().state,
        OperationState::Succeeded
    );
}

async fn fixture() -> (
    tempfile::TempDir,
    ApprovalBroker,
    WorkflowStore,
    AcceptedRevisionGrant,
    WorkflowRun,
) {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
    std::fs::write(
        home.path().join("agents/alice/agent.toml"),
        "[capabilities]\nallowed_tools=['tasks_update']\n",
    )
    .unwrap();
    let broker = ApprovalBroker::open(home.path()).unwrap();
    let store = WorkflowStore::open(home.path()).unwrap();
    let policy = policy_revision(home.path(), "alice").unwrap();
    let context = DecisionContext {
        channel: "line".into(),
        account_id: "bot".into(),
        conversation_id: "private".into(),
        principal_id: "owner".into(),
    };
    let creator = CreatorGrantSnapshot {
        actor: "alice".into(),
        allowed_tools: BTreeSet::from(["tasks_update".into()]),
        policy_revision: policy.clone(),
    };
    let schema = TypedSchema::Object {
        properties: BTreeMap::from([
            ("task_id".into(), TypedSchema::String { max_length: 100 }),
            ("status".into(), TypedSchema::String { max_length: 20 }),
        ]),
        required: BTreeSet::from(["task_id".into(), "status".into()]),
    };
    let args = json!({"task_id":"owned","status":"done"});
    let definition = WorkflowDefinition {
        schema_version: 1,
        workflow_id: "weekly".into(),
        revision: 1,
        skill_revision_hash: "skill-sha".into(),
        input_schema: schema.clone(),
        output_schema: schema.clone(),
        required_capabilities: BTreeSet::from(["tasks_update".into()]),
        steps: vec![StepDefinition {
            step_id: "update".into(),
            action: StepAction::McpEffect {
                tool: "tasks_update".into(),
                template_id: "owned-update".into(),
            },
            input: InputRef::Literal {
                value: args.clone(),
            },
            input_schema: schema.clone(),
            output_schema: schema.clone(),
            timeout_seconds: 10,
            max_read_attempts: 1,
        }],
    };
    let expiry = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let budget = CostBudget {
        per_run_micros: 1000,
        monthly_micros: 10_000,
        max_consecutive_failures: 3,
    };
    let spec = GrantSpec {
        schema_version: 1,
        workflow_id: "weekly".into(),
        workflow_revision: 1,
        revision_hash: definition.hash(),
        skill_hash: "skill-sha".into(),
        fixtures_digest: "fixtures-sha".into(),
        activation_id: uuid::Uuid::new_v4().to_string(),
        actor: "alice".into(),
        operator_context: context.clone(),
        creator_grant: creator.clone(),
        audience: vec!["owner".into()],
        templates: BTreeMap::from([(
            "owned-update".into(),
            EffectTemplate {
                step_id: "update".into(),
                tool: "tasks_update".into(),
                input_schema: schema,
                resource_scope: BTreeMap::from([("task_id".into(), json!("owned"))]),
                receipt_adapter_version: 1,
            },
        )]),
        input_max_age_seconds: 600,
        fixtures_expires_at: expiry.clone(),
        budget: budget.clone(),
        expires_at: expiry.clone(),
        policy_revision: policy.clone(),
    };
    let payload = json!({
        "kind": "workflow_activation",
        "activation_id": spec.activation_id,
        "spec_hash": spec.hash(),
        "revision_hash": spec.revision_hash,
        "fixtures_digest": spec.fixtures_digest
    });
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: uuid::Uuid::new_v4().to_string(),
        run_origin_kind: "workflow".into(),
        actor_principal: "alice".into(),
        decision_context: context,
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&payload),
        policy_revision: policy.clone(),
        cwd: None,
        environment_hash: "environment-sha".into(),
        file_hashes: Default::default(),
        expires_at: (Utc::now() + chrono::Duration::minutes(10)).to_rfc3339(),
        resume_handler: "workflow_v1".into(),
        resume_version: 1,
    };
    let id = broker
        .request_bound(
            RequestKind::Approval,
            "alice",
            "接受固定流程",
            payload,
            binding.clone(),
        )
        .await
        .unwrap();
    // U3 (F1b): activations are accepted by an Admin in the dashboard.
    broker
        .decide_bound_dashboard(&id, &duduclaw_auth::UserContext::admin_fallback(), true)
        .await
        .unwrap();
    let revision = ApprovedWorkflowRevision {
        definition,
        revision_hash: spec.revision_hash.clone(),
        owner: "alice".into(),
        creator_grant: creator.clone(),
        audience: spec.audience.clone(),
        fixtures_digest: spec.fixtures_digest.clone(),
        acceptance_id: id.to_string(),
        accepted_at: Utc::now().to_rfc3339(),
        expires_at: expiry.clone(),
    };
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_revisions VALUES(?1,1,?2,?3)",
                params![
                    "weekly",
                    revision.revision_hash,
                    serde_json::to_string(&revision).unwrap()
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    let accepted = AcceptedRevisionGrant {
        activation_id: spec.activation_id.clone(),
        acceptance_id: id.to_string(),
        revision,
        spec_hash: spec.hash(),
        spec,
    };
    let trigger = Trigger::Manual {
        request_id: uuid::Uuid::new_v4().to_string(),
    };
    let run = WorkflowRun {
        run_id: uuid::Uuid::new_v4().to_string(),
        trigger_key: trigger.key("weekly", 1).unwrap(),
        trigger,
        workflow_id: "weekly".into(),
        revision: 1,
        workflow_hash: accepted.spec.revision_hash.clone(),
        skill_hash: "skill-sha".into(),
        actor: "alice".into(),
        creator_grant: creator,
        audience: accepted.spec.audience.clone(),
        task: None,
        input: args.clone(),
        input_hash: payload_hash(&args),
        input_observed_at: Utc::now().to_rfc3339(),
        policy_revision: policy,
        environment_hash: "environment-sha".into(),
        grant: None,
        activation_id: Some(accepted.activation_id.clone()),
        deadline_at: (Utc::now() + chrono::Duration::minutes(10)).to_rfc3339(),
        budget,
        status: RunStatus::Running,
        cost: CostBreakdown::default(),
        error_code: None,
        created_at: Utc::now().to_rfc3339(),
        decision_context: Some(accepted.spec.operator_context.clone()),
        failure_class: None,
        cancelled_by: None,
    };
    (home, broker, store, accepted, run)
}
async fn operation(
    broker: &ApprovalBroker,
    store: &WorkflowStore,
    accepted: &AcceptedRevisionGrant,
    mut run: WorkflowRun,
) -> (GrantRef, String, ExecutionBinding) {
    let prepared = broker.prepare_revision_grant(accepted).await.unwrap();
    let active = broker.activate_revision_grant(&prepared).await.unwrap();
    run.grant = Some(active.clone());
    store
        .with_transaction(|tx| {
            tx.execute(
                "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,'running',?7)",
                params![
                    run.run_id,
                    run.trigger_key,
                    run.workflow_id,
                    run.revision,
                    run.workflow_hash,
                    serde_json::to_string(&run).unwrap(),
                    run.created_at
                ],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .unwrap();
    // F1b: the runner reserves the effect in the cost ledger with its running
    // checkpoint; the claim refuses an unreserved effect.
    let home = store.path().unwrap().parent().unwrap().to_path_buf();
    store
        .charge_step(
            &home,
            &run.run_id,
            "update",
            crate::workflow::cost_ledger::ChargeKind::Effect,
        )
        .await
        .unwrap();
    let payload = json!({"name":"tasks_update","arguments":run.input});
    let binding = ExecutionBinding {
        schema_version: 1,
        run_id: run.run_id,
        run_origin_kind: "workflow".into(),
        actor_principal: run.actor,
        decision_context: accepted.spec.operator_context.clone(),
        task_id: None,
        task_revision: None,
        task_snapshot_hash: None,
        payload_hash: payload_hash(&payload),
        policy_revision: run.policy_revision,
        cwd: None,
        environment_hash: run.environment_hash,
        file_hashes: Default::default(),
        expires_at: (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(),
        resume_handler: "workflow_v1".into(),
        resume_version: 1,
    };
    let id = broker
        .prepare_granted_operation(&active, "update", &payload, &binding, Some("provider-key"))
        .await
        .unwrap();
    (active, id, binding)
}

#[tokio::test]
async fn claim_then_revoke_cannot_begin_and_never_mints_human_approval() {
    let (_home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    let claim = broker
        .claim_operation(&id, &binding, "worker", 60)
        .await
        .unwrap();
    broker
        .revoke_revision_grant(&grant, "owner stopped")
        .await
        .unwrap();
    assert!(broker.begin_execution(&claim, &binding).await.is_err());
    assert!(matches!(
        broker
            .inspect_operation(&id)
            .await
            .unwrap()
            .unwrap()
            .authority,
        OperationAuthority::ActiveWorkflowRevisionGrant { .. }
    ));
    let conn = broker.store.conn.lock().await;
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM approvals", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT approval_id FROM approval_operations WHERE operation_id=?1",
            params![id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        ""
    );
}
#[tokio::test]
async fn broker_begin_then_revoke_still_settles_a_hand_written_receipt() {
    let (_home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    let claim = broker
        .claim_operation(&id, &binding, "worker", 60)
        .await
        .unwrap();
    broker.begin_execution(&claim, &binding).await.unwrap();
    let revoked = broker
        .revoke_revision_grant(&grant, "stop next steps")
        .await
        .unwrap();
    assert_eq!(revoked.executing_operations, vec![id.clone()]);
    broker
        .settle_operation(
            &claim,
            OperationState::Succeeded,
            Some(json!({"actual_row":"owned","revision":2})),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        broker.inspect_operation(&id).await.unwrap().unwrap().state,
        OperationState::Succeeded
    );
}
#[tokio::test]
async fn source_hash_scope_and_provider_key_are_immutable() {
    let (_home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    let payload = json!({"name":"tasks_update","arguments":{"task_id":"owned","status":"done"}});
    assert!(
        broker
            .prepare_granted_operation(&grant, "update", &payload, &binding, Some("different"))
            .await
            .is_err()
    );
    let altered =
        json!({"name":"tasks_update","arguments":{"task_id":"not-owned","status":"done"}});
    let mut drift = binding.clone();
    drift.payload_hash = payload_hash(&altered);
    assert!(
        broker
            .prepare_granted_operation(&grant, "update", &altered, &drift, Some("provider-key"))
            .await
            .is_err()
    );
    let conn = broker.store.conn.lock().await;
    assert!(
        conn.execute(
            "UPDATE approval_operations SET authority_source='bound_human_approval' WHERE operation_id=?1",
            params![id]
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "UPDATE workflow_revision_grants SET state='prepared',authority_epoch=authority_epoch+1 WHERE grant_id=?1",
            params![grant.grant_id]
        )
        .is_err()
    );
}
#[tokio::test]
async fn reopen_never_resurrects_revoked_grant_and_unknown_schema_fails() {
    let (home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    broker.revoke_revision_grant(&grant, "stop").await.unwrap();
    drop(broker);
    let broker = ApprovalBroker::open(home.path()).unwrap();
    assert!(
        broker
            .claim_operation(&id, &binding, "worker", 60)
            .await
            .is_err()
    );
    broker
        .store
        .conn
        .lock()
        .await
        .execute("UPDATE approval_authority_schema SET version=42", [])
        .unwrap();
    drop(broker);
    assert!(ApprovalBroker::open(home.path()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_brokers_begin_and_revoke_share_one_authority_transaction() {
    let (home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    let claim = broker
        .claim_operation(&id, &binding, "worker", 60)
        .await
        .unwrap();
    let second = ApprovalBroker::open(home.path()).unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let broker = std::sync::Arc::new(broker);
    let second = std::sync::Arc::new(second);
    let b = barrier.clone();
    let begin_broker = broker.clone();
    let begin_claim = claim.clone();
    let begin_binding = binding.clone();
    let begin = tokio::spawn(async move {
        b.wait().await;
        begin_broker
            .begin_execution(&begin_claim, &begin_binding)
            .await
    });
    let revoke_broker = second.clone();
    let revoke_grant = grant.clone();
    let revoke = tokio::spawn(async move {
        barrier.wait().await;
        revoke_broker
            .revoke_revision_grant(&revoke_grant, "concurrent stop")
            .await
    });
    let (first, revoked) = tokio::join!(begin, revoke);
    let first = first.unwrap();
    revoked.unwrap().unwrap();
    let row = broker.inspect_operation(&id).await.unwrap().unwrap();
    if first.is_ok() {
        assert_eq!(row.state, OperationState::Executing);
        broker
            .settle_operation(
                &claim,
                OperationState::Succeeded,
                Some(json!({"actual":"row"})),
                None,
            )
            .await
            .unwrap();
    } else {
        assert_ne!(row.state, OperationState::Executing);
    }
    assert!(
        second
            .claim_operation(&id, &binding, "later-worker", 60)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn revoke_transaction_failure_rolls_back_epoch_state_and_outbox() {
    let (_home, broker, store, accepted, run) = fixture().await;
    let (grant, id, binding) = operation(&broker, &store, &accepted, run).await;
    broker
        .store
        .conn
        .lock()
        .await
        .execute_batch("CREATE TRIGGER test_revoke_abort AFTER UPDATE ON workflow_revision_grants
            WHEN NEW.state='revoked' BEGIN SELECT RAISE(ABORT,'test rollback'); END;")
        .unwrap();
    assert!(
        broker
            .revoke_revision_grant(&grant, "must roll back")
            .await
            .is_err()
    );
    assert_eq!(
        broker.inspect_revision_grant(&grant).await.unwrap().1,
        "active"
    );
    assert_eq!(
        broker
            .store
            .conn
            .lock()
            .await
            .query_row("SELECT COUNT(*) FROM workflow_grant_outbox", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    let claim = broker
        .claim_operation(&id, &binding, "worker", 60)
        .await
        .unwrap();
    broker.begin_execution(&claim, &binding).await.unwrap();
}

#[tokio::test]
async fn only_fixture_human_operations_may_omit_activation_authority() {
    for case in [
        "fixture",
        "manual",
        "activation_only",
        "grant_only",
        "unknown",
    ] {
        let (_home, broker, store, accepted, run) = fixture().await;
        let (_, _, mut binding) = operation(&broker, &store, &accepted, run).await;
        let original_run_id = binding.run_id.clone();
        binding.run_id = uuid::Uuid::new_v4().to_string();
        store
            .with_transaction(|tx| {
                let raw: String = tx
                    .query_row(
                        "SELECT record_json FROM workflow_runs WHERE run_id=?1",
                        params![original_run_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                let mut run: WorkflowRun = serde_json::from_str(&raw).unwrap();
                if case != "activation_only" {
                    run.activation_id = None;
                }
                if case != "grant_only" {
                    run.grant = None;
                }
                if case == "fixture" {
                    run.trigger = Trigger::Fixture {
                        fixture_id: "authority-check".into(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                    };
                }
                run.run_id = binding.run_id.clone();
                if !matches!(run.trigger, Trigger::Fixture { .. }) {
                    run.trigger = Trigger::Manual {
                        request_id: uuid::Uuid::new_v4().to_string(),
                    };
                }
                run.trigger_key = run.trigger.key(&run.workflow_id, run.revision).unwrap();
                if case != "unknown" {
                    tx.execute(
                        "INSERT INTO workflow_runs VALUES(?1,?2,?3,?4,?5,?6,'running',?7)",
                        params![
                            run.run_id,
                            run.trigger_key,
                            run.workflow_id,
                            run.revision,
                            run.workflow_hash,
                            serde_json::to_string(&run).unwrap(),
                            run.created_at
                        ],
                    )
                    .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let payload =
            json!({"name":"tasks_update","arguments":{"task_id":"owned","status":"done"}});
        let approval = broker
            .request_bound(
                RequestKind::Approval,
                "alice",
                "Exact fixture action",
                payload,
                binding.clone(),
            )
            .await
            .unwrap();
        broker
            .decide_bound(&approval, &binding.decision_context, true)
            .await
            .unwrap();
        let prepared = broker
            .prepare_operation(&approval, "human-authority-case", None)
            .await;
        if case == "fixture" {
            let id = prepared.unwrap();
            let claim = broker
                .claim_operation(&id, &binding, "fixture-worker", 60)
                .await
                .unwrap();
            broker.begin_execution(&claim, &binding).await.unwrap();
        } else {
            let expected = if case == "unknown" {
                "workflow run authority unavailable"
            } else {
                "workflow run activation authority missing"
            };
            assert_eq!(prepared.unwrap_err(), expected, "case {case}");
        }
    }
}


/// U8 (F5-A): the grant's validity is its own; fixture evidence only has to
/// be fresh when the activation is accepted.
#[tokio::test]
async fn grant_outlives_fixture_evidence_but_acceptance_needs_it_fresh() {
    let (_home, broker, _store, mut accepted, _run) = fixture().await;
    accepted.spec.fixtures_expires_at = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    accepted.spec.validate().unwrap();
    assert!(accepted.spec.validate_fresh_fixtures().is_err());
    accepted.spec_hash = accepted.spec.hash();
    let err = broker.prepare_revision_grant(&accepted).await.unwrap_err();
    assert!(err.contains("fixtures"), "{err}");
}
