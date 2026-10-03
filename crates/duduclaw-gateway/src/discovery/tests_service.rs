//! Real database and authenticated-caller fixtures; no provider or Docker calls.
use super::*;
use crate::approval::ApprovalStore;
use std::collections::HashMap;
fn context(id: &str, role: UserRole) -> UserContext {
    UserContext {user_id:id.into(),email:format!("{id}@example.invalid"),role,
        agent_access:HashMap::from([("worker".into(),AccessLevel::Operator)]),must_change_password:false}
}
fn user(id: &str, role: UserRole) -> TrustedCaller {
    TrustedCaller::from_user(&context(id,role)).unwrap()
}
fn fixture() -> (tempfile::TempDir, PublicDiscoverySpec, ApprovalBroker) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join("agents/worker")).unwrap();
    std::fs::write(home.join("agents/worker/agent.toml"), "[agent]\nname='worker'\n").unwrap();
    let root = home.join("approved"); std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("solution.txt"),"baseline").unwrap();
    workspace::create_private_directory(&home.join("discovery")).unwrap();
    let scorer = home.join("discovery/evaluators/score"); std::fs::create_dir_all(&scorer).unwrap();
    std::fs::write(scorer.join("score"),"trusted mock scorer; never executed").unwrap();
    let config = DiscoveryConfig { approved_workspace_roots:vec![root.clone()], account_pool:vec!["dedicated-fixture".into()],
        attempt:super::super::config::AttemptSettings { runtimes:std::collections::BTreeMap::from([
            ("claude".into(),super::super::config::AttemptRuntimeConfig { image:"fixture:never-run".into(),
                executable:"/usr/bin/claude".into(),base_url:None,provider:None })]), ..Default::default() },
        evaluators:std::collections::BTreeMap::from([("score".into(),super::super::config::EvaluatorConfig {
            command:vec![scorer.join("score").to_string_lossy().into_owned()], sha256:workspace::directory_sha256(&scorer).unwrap(),
            sandbox:EvaluatorSandbox::Container,image:Some("fixture:never-run".into()),good_solution:root.clone(),cheating_solution:root.clone(),
            timeout_secs:10,memory_bytes:256*1024*1024,pids:64,scratch_bytes:1024*1024,test_data:None,timing_sensitive:false })]),
        ..Default::default() };
    let table = std::collections::BTreeMap::from([("discovery",config)]);
    std::fs::write(home.join("config.toml"),toml::to_string(&table).unwrap()).unwrap();
    let spec = PublicDiscoverySpec { approved_root_id:root_id(&root),evaluator:"score".into(),runtime:"claude".into(),model:"fixture-model".into(),
        branch_count:2,refine_count:1,max_parallelism:2,direction:Direction::Max,
        budget:RunBudget {max_agent_calls:10,max_usd:1.0,max_wall_secs:30,max_rounds:2} };
    (dir,spec,ApprovalBroker::new(Arc::new(ApprovalStore::open_in_memory().unwrap())))
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn release_grid_limit_rejects_single_and_cumulative_oversize_before_approval() {
    for (width, refine, rounds) in [(1000, 20, 1), (1000, 10, 2)] {
        let (dir, mut spec, broker) = fixture();
        let home = dir.path().canonicalize().unwrap();
        let store = TaskStore::open(&home).unwrap();
        spec.branch_count = width;
        spec.refine_count = refine;
        spec.budget.max_rounds = rounds;
        let result = create(&home, &store, &broker, &user("employee", UserRole::Employee),
            "worker", "Oversize", "Improve fixture", spec).await;
        assert!(result.is_err(), "all planned rounds must remain queryable");
        assert!(broker.list_pending(None).await.unwrap().is_empty());
        assert_eq!(list(&home, &user("manager", UserRole::Manager), None, 20).await.unwrap()["runs"], json!([]));
    }
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn release_grid_limit_accepts_exact_cumulative_boundary() {
    let (dir, mut spec, broker) = fixture();
    let home = dir.path().canonicalize().unwrap();
    let store = TaskStore::open(&home).unwrap();
    spec.branch_count = 1000;
    spec.refine_count = 9;
    spec.budget.max_rounds = 2;
    let created = create(&home, &store, &broker, &user("manager", UserRole::Manager),
        "worker", "Boundary", "Improve fixture", spec).await.unwrap();
    assert_eq!(created.status, "queued");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn release_grid_limit_legacy_oversize_tree_does_not_break_visible_list() {
    let (dir, spec, broker) = fixture();
    let home = dir.path().canonicalize().unwrap();
    let task_store = TaskStore::open(&home).unwrap();
    let manager = user("manager", UserRole::Manager);
    let normal = create(&home, &task_store, &broker, &manager, "worker", "Normal", "Improve fixture", spec.clone()).await.unwrap();
    let legacy = create(&home, &task_store, &broker, &manager, "worker", "Legacy", "Improve fixture", spec.clone()).await.unwrap();
    let store = DiscoveryStore::open(&home).unwrap();
    store.create_run(&legacy.run_id, "Improve fixture", "worker", "score", &"a".repeat(64), Direction::Max, &spec.budget).unwrap();
    store.attach_run_identity(&super::super::online::RunIdentity {
        run_id: legacy.run_id.clone(), task_id: Some(legacy.task_id.clone()), creator_id: manager.id(),
        creator_origin: manager.origin().into(), approved_root_id: Some(spec.approved_root_id.clone()),
    }).unwrap();
    store.insert_world(&super::super::tree::World {
        schema: super::super::tree::WORLD_SCHEMA.into(), run_id: legacy.run_id.clone(), round: 1,
        direction: Direction::Max, baseline_score: 1.0, branch_count: 1000, refine_count: 20,
        max_parallelism: 1, policy_id: "legacy".into(), beta: 0.6,
    }).unwrap();
    let node = serde_json::from_value::<super::super::tree::Node>(json!({
        "schema":super::super::tree::NODE_SCHEMA,"run_id":legacy.run_id,"round":1,
        "cell_id":"r1-b0-a0","branch":0,"attempt":0,"seq":1,
        "evaluated":true,"valid":true,"score":2.0,"fail_class":"ok","visible_set":[],
        "cost":{"usd":0.125,"input_tokens":17,"unknown_calls":1}
    })).unwrap();
    store.insert_nodes(&[node]).unwrap();
    let listing = list(&home, &manager, None, 20).await.expect("one oversized historical grid must not poison the list");
    let ids = listing["runs"].as_array().unwrap().iter().map(|run| run["run_id"].as_str().unwrap()).collect::<Vec<_>>();
    assert!(ids.contains(&normal.run_id.as_str()));
    assert!(ids.contains(&legacy.run_id.as_str()), "retain the historical run rather than silently hiding it");
    let summary = listing["runs"].as_array().unwrap().iter().find(|run| run["run_id"] == legacy.run_id).unwrap();
    assert_eq!(summary["tree_available"], false);
    assert!(summary["tree_unavailable_reason"].as_str().unwrap().contains("bounded query"));
    assert_eq!(summary["cost"]["usd"], 0.125);
    assert_eq!(summary["cost"]["input_tokens"], 17);
    assert_eq!(summary["cost"]["usd_source"], "pending", "node subtotal must not impersonate a complete run bill");
    assert!(tree(&home, &manager, &legacy.run_id).await.unwrap_err().contains("bounded query"));
    assert_eq!(list(&home, &user("stranger", UserRole::Employee), None, 20).await.unwrap()["runs"], json!([]));
}
#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn public_discovery_rejects_body_authority_and_arbitrary_paths() {
    let (_dir,spec,_broker)=fixture();
    for field in ["operator","starting_workspace","creator_origin","approval_id","beta","policy_source"] {
        let mut json=serde_json::to_value(&spec).unwrap(); json[field]=json!("attacker-controlled");
        assert!(serde_json::from_value::<PublicDiscoverySpec>(json).is_err(),"must reject {field}");
    }
}
#[test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
fn public_discovery_rejects_disabled_or_unverified_agent_identity() {
    let (dir,_,_)=fixture();
    assert!(TrustedCaller::from_signed_agent(dir.path(),"worker",None).is_err());
    duduclaw_core::ensure_identity_key(dir.path()).unwrap();
    assert!(TrustedCaller::from_signed_agent(dir.path(),"worker",Some("forged")).is_err());
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_employee_approval_binds_frozen_request_and_never_self_grants() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    assert_eq!(created.status,"pending_approval");
    let id=ApprovalId::from(created.approval_id.unwrap());
    broker.decide(&id,true,"agent:worker").await.unwrap();
    assert!(authorize_approved_request(&store,&broker,&employee,&id).await.is_err());
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"pending_approval");
    let manager=user("manager",UserRole::Manager);
    assert!(authorize_approved_request(&store,&broker,&manager,&id).await.is_err());
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let id=ApprovalId::from(created.approval_id.unwrap());
    prepare_approval_decision(&store,&broker,&manager,&id,true).await.unwrap();
    broker.decide(&id,true,"dashboard:manager").await.unwrap();
    authorize_approved_request(&store,&broker,&manager,&id).await.unwrap();
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"queued");
    assert!(store.atomic_claim(&created.task_id,"ordinary", "2026-09-30T10:00:00Z", "2026-09-30T10:05:00Z").await.unwrap()
        ==crate::task_store::ClaimOutcome::NotClaimable);
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_manager_and_owner_query_acl_precede_artifact_lookup() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let manager=user("manager",UserRole::Manager);
    let created=create(&home,&store,&broker,&manager,"worker","Explore","Improve fixture",spec).await.unwrap();
    assert_eq!(created.status,"queued"); assert!(created.approval_id.is_none());
    let stranger=user("stranger",UserRole::Employee);
    assert_eq!(list(&home,&stranger,None,20).await.unwrap()["runs"],json!([]));
    assert_eq!(artifact(&home,&stranger,&created.run_id,None).await.unwrap_err(),"permission denied");
    let payload=tree(&home,&manager,&created.run_id).await.unwrap();
    assert!(!payload.to_string().contains(home.to_str().unwrap()));
    let second_store=TaskStore::open(&home).unwrap();
    assert!(second_store.claim_discovery(&created.task_id,"dedicated", "2099-09-30T10:05:00Z").await.unwrap());
    cancel(&home,&manager,&created.run_id).await.unwrap();
    assert!(!second_store.finish_discovery(&created.task_id,"dedicated",true,"late success").await.unwrap());
    assert!(store.update_task(&created.task_id,&json!({"status":"done"})).await.is_err());
    assert!(store.complete_task(&created.task_id,"fake success","ordinary").await.unwrap().is_none());
}
#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_public_none_and_unknown_root_fail_before_approval() {
    let (dir,mut spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    spec.approved_root_id="/etc".into();
    assert!(create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec.clone()).await.is_err());
    assert!(broker.list_pending(None).await.unwrap().is_empty());
    let mut config=load_config(&home).unwrap(); config.attempt.sandbox=AttemptSandbox::None;
    config.allow_unconfined=true; config.attempt.allow_shared_account_pool=true;
    std::fs::write(home.join("config.toml"),toml::to_string(&std::collections::BTreeMap::from([("discovery",config)])).unwrap()).unwrap();
    assert!(create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.is_err());
    assert!(broker.list_pending(None).await.unwrap().is_empty());
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_artifact_download_uses_checkpoint_digest_opaque_ids_and_owner_acl() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let manager=user("manager",UserRole::Manager); let task_store=TaskStore::open(&home).unwrap();
    let created=create(&home,&task_store,&broker,&manager,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    let root=home.join("discovery/artifacts").join(&created.run_id).join("r1-b0-a0/ws");
    workspace::create_private_directory(&root).unwrap();
    std::fs::write(root.join("answer.txt"),b"verified answer").unwrap();
    let store=DiscoveryStore::open(&home).unwrap();
    store.create_run(&created.run_id,"Improve fixture","worker","score",&"a".repeat(64),Direction::Max,&spec.budget).unwrap();
    store.attach_run_identity(&super::super::online::RunIdentity {run_id:created.run_id.clone(),
        task_id:Some(created.task_id.clone()),creator_id:manager.id(),creator_origin:manager.origin().into(),
        approved_root_id:Some(spec.approved_root_id.clone())}).unwrap();
    store.finish_run(&created.run_id,"complete",Some("r1-b0-a0")).unwrap();
    store.record_verified_artifact(&created.run_id,"r1-b0-a0",&workspace::directory_sha256(&root).unwrap()).unwrap();
    let metadata=artifact(&home,&manager,&created.run_id,None).await.unwrap();
    assert!(!metadata.to_string().contains(home.to_str().unwrap()));
    assert_eq!(metadata["files"][0]["name"],"answer.txt");
    let file_id=metadata["files"][0]["file_id"].as_str().unwrap();
    let download=artifact(&home,&manager,&created.run_id,Some(file_id)).await.unwrap();
    assert_eq!(download["content_base64"],"dmVyaWZpZWQgYW5zd2Vy");
    assert!(artifact(&home,&manager,&created.run_id,Some("../../secret")).await.is_err());
    let stranger=user("other",UserRole::Employee);
    assert_eq!(artifact(&home,&stranger,&created.run_id,Some(file_id)).await.unwrap_err(),"permission denied");
    std::fs::write(root.join("answer.txt"),b"mutated").unwrap();
    assert!(artifact(&home,&manager,&created.run_id,Some(file_id)).await.is_err());
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_signed_agent_always_waits_for_manager_even_with_autonomy_full() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let key=duduclaw_core::ensure_identity_key(&home).unwrap();
    let token=duduclaw_core::mint_identity_token(&key,"worker");
    let caller=TrustedCaller::from_signed_agent(&home,"worker",Some(&token)).unwrap();
    std::fs::write(home.join("agents/worker/agent.toml"),"[agent]\nname='worker'\n[capabilities]\nautonomy='full'\n").unwrap();
    let store=TaskStore::open(&home).unwrap();
    let created=create(&home,&store,&broker,&caller,"worker","Explore","Improve fixture",spec).await.unwrap();
    assert_eq!(created.status,"pending_approval");
    let approval=ApprovalId::from(created.approval_id.unwrap());
    assert!(validate_approval_decision(&store,&broker,&caller,&approval).await.is_err());
    assert_eq!(broker.get(&approval).await.unwrap().unwrap().status,ApprovalStatus::Pending);
}

// Persist an actual broker-created record without invoking a channel notifier.
fn persist_approval(home: &Path, record: &crate::approval::ApprovalRecord) {
    let _store = ApprovalStore::open(home).unwrap();
    let conn = rusqlite::Connection::open(home.join("approvals.db")).unwrap();
    conn.execute("INSERT INTO approvals(id,agent_id,action_kind,summary,payload,status,created_at,decided_at,decided_by,ttl_seconds) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![record.id.as_str(),record.agent_id,record.action_kind,record.summary,
            record.payload.to_string(),record.status.as_str(),record.created_at,record.decided_at,record.decided_by,record.ttl_seconds]).unwrap();
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_approval_list_and_task_serialization_never_publish_frozen_policy_source() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let config=load_config(&home).unwrap();
    let (_,hash)=validate_spec(&home,&config,"worker","Improve fixture",&spec,0.6).unwrap();
    let ns=DefaultsNamespace {agent_id:"worker".into(),scorer_name:"score".into(),scorer_hash:hash,
        direction:spec.direction,approved_root_id:spec.approved_root_id.clone(),runtime:spec.runtime.clone(),configured_model:spec.model.clone()};
    let defaults=DiscoveryStore::open(&home).unwrap(); defaults.load_discovery_default(&ns).unwrap();
    let source="class OptimalPolicy:\n    private_provenance = 'PRIVATE_POLICY_SOURCE_CANARY'\n";
    let bundle=super::super::night::FrozenDefaults {policy_id:"fixture-policy".into(),source:Some(source.into()),
        source_sha256:Some(format!("{:x}",Sha256::digest(source.as_bytes()))),beta:0.6,
        knobs:Default::default(),origin_task_ids:vec!["fixture-origin".into()]};
    bundle.validate().unwrap();
    rusqlite::Connection::open(defaults.db_path()).unwrap().execute(
        "INSERT INTO discovery_defaults(namespace,version,bundle,updated_at) VALUES(?1,1,?2,?3)",
        rusqlite::params![ns.key().unwrap(),serde_json::to_string(&bundle).unwrap(),chrono::Utc::now().to_rfc3339()]).unwrap();
    let store=TaskStore::open(&home).unwrap();
    let created=create(&home,&store,&broker,&user("employee",UserRole::Employee),"worker","Explore","Improve fixture",spec).await.unwrap();
    let record=broker.get(&ApprovalId::from(created.approval_id.unwrap())).await.unwrap().unwrap();
    persist_approval(&home,&record);
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    let response=handler.handle_approvals_list(json!({"action_kind":"discovery"})).await;
    let response=serde_json::to_value(response).unwrap();
    assert!(response.to_string().contains(record.id.as_str()),"exercise the real approvals.list output");
    assert!(!response.to_string().contains("PRIVATE_POLICY_SOURCE_CANARY"),"approvals.list must not disclose policy source");
    let row=store.get_task(&created.task_id).await.unwrap().unwrap();
    assert!(!row.discovery_spec_json.as_deref().unwrap().contains("PRIVATE_POLICY_SOURCE_CANARY"),"tasks.db may contain only public request metadata");
    let public=public_frozen(&row).unwrap();
    assert!(defaults.load_frozen_request(&row.id,row.discovery_run_id.as_deref().unwrap(),&public.frozen_sha256).unwrap()
        .contains("PRIVATE_POLICY_SOURCE_CANARY"),"the protected database retains the source bound to the public digest");
    let raw: String=rusqlite::Connection::open(home.join("tasks.db")).unwrap().query_row(
        "SELECT discovery_spec_json FROM tasks WHERE id=?1",rusqlite::params![row.id],|r|r.get(0)).unwrap();
    assert!(!raw.contains("PRIVATE_POLICY_SOURCE_CANARY"));
    assert!(serde_json::to_value(row).unwrap().get("discovery_spec_json").is_none(),"generic task serialization must hide the frozen specification");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_legal_delegating_creator_retains_query_and_cancel_access() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join("agents/lead")).unwrap();
    std::fs::write(home.join("agents/lead/agent.toml"),"[agent]\nname='lead'\n").unwrap();
    std::fs::write(home.join("org.toml"),"schema=1\n[agents.lead]\nreports_to=''\n[agents.worker]\nreports_to='lead'\n").unwrap();
    let key=duduclaw_core::ensure_identity_key(&home).unwrap();
    let token=duduclaw_core::mint_identity_token(&key,"lead");
    let caller=TrustedCaller::from_signed_agent(&home,"lead",Some(&token)).unwrap();
    let store=TaskStore::open(&home).unwrap();
    let created=create(&home,&store,&broker,&caller,"worker","Explore","Improve fixture",spec).await.unwrap();
    assert_eq!(created.status,"pending_approval");
    assert_eq!(tree(&home,&caller,&created.run_id).await.unwrap()["run"]["run_id"],created.run_id);
    assert_eq!(list(&home,&caller,None,20).await.unwrap()["runs"].as_array().unwrap().len(),1);
    cancel(&home,&caller,&created.run_id).await.unwrap();
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"cancelled");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_active_reconciliation_is_not_hidden_by_thousand_terminal_rows() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let created=create(&home,&store,&broker,&user("manager",UserRole::Manager),"worker","Explore","Improve fixture",spec).await.unwrap();
    let mut active=store.get_task(&created.task_id).await.unwrap().unwrap();
    store.cancel_discovery(&active.id,"fixture seed").await.unwrap();
    active.id="old-active".into(); active.discovery_run_id=Some("old-active-run".into());
    active.status="in_progress".into(); active.claimed_by=Some("dead-worker".into());
    active.lease_expires_at=Some("2000-01-01T00:00:00Z".into()); active.created_at="2000-01-01T00:00:00Z".into();
    store.insert_task(&active).await.unwrap();
    for index in 0..1001 {
        let mut terminal=active.clone(); terminal.id=format!("terminal-{index}");
        terminal.discovery_run_id=Some(format!("terminal-run-{index}")); terminal.status="done".into();
        terminal.created_at="2026-09-30T00:00:00Z".into(); store.insert_task(&terminal).await.unwrap();
    }
    poll(&home).await.unwrap();
    assert_eq!(store.get_task("old-active").await.unwrap().unwrap().status,"failed","every active lease must be reconciled regardless of terminal history size");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_manager_decision_recovers_after_broker_commit_before_task_authorization() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let manager=user("manager",UserRole::Manager);
    let created=create(&home,&store,&broker,&user("employee",UserRole::Employee),"worker","Explore","Improve fixture",spec).await.unwrap();
    let id=ApprovalId::from(created.approval_id.unwrap());
    persist_approval(&home,&broker.get(&id).await.unwrap().unwrap());
    let durable_broker=ApprovalBroker::open(&home).unwrap();
    prepare_approval_decision(&store,&durable_broker,&manager,&id,true).await.unwrap();
    durable_broker.decide(&id,true,"dashboard:manager").await.unwrap();
    // A separate operator owns the runner slot: reconciliation cannot execute.
    let _lease=super::super::maintenance::OperatorLeaseGuard::acquire(&home).unwrap();
    drop(store); drop(durable_broker);
    poll(&home).await.unwrap();
    let reopened=TaskStore::open(&home).unwrap();
    assert_eq!(reopened.get_task(&created.task_id).await.unwrap().unwrap().status,"queued","durable human authorization must survive the cross-database commit window");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_catalog_and_create_share_the_production_runtime_capability_gate() {
    for runtime in ["codex","antigravity","agy","grok"] {
        let (dir,mut spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
        let mut config=load_config(&home).unwrap();
        let configured=config.attempt.runtimes["claude"].clone();
        for name in ["codex","antigravity","grok"] { config.attempt.runtimes.insert(name.into(),configured.clone()); }
        std::fs::write(home.join("config.toml"),toml::to_string(&std::collections::BTreeMap::from([("discovery",config)])).unwrap()).unwrap();
        let caller=user("manager",UserRole::Manager);
        let offered=catalog(&home,&caller,"worker").unwrap();
        assert_eq!(offered["runtimes"],json!(["antigravity","claude","codex","grok"]),"every configured family runs under the host-side contract");
        let store=TaskStore::open(&home).unwrap();
        spec.runtime=runtime.into();
        assert!(create(&home,&store,&broker,&caller,"worker","Explore","Improve fixture",spec.clone()).await.is_ok(),"public create accepts {runtime}");
        assert_eq!(store.discovery_tasks().await.unwrap().len(),1);
        let canonical=if runtime=="agy" {"antigravity"} else {runtime};
        let frozen:Value=serde_json::from_str(store.discovery_tasks().await.unwrap()[0].discovery_spec_json.as_deref().unwrap()).unwrap();
        assert_eq!(frozen["spec"]["runtime"],canonical,"the alias is stored under the family name");
        let listed=list(&home,&caller,Some("worker"),10).await.unwrap();
        assert_eq!(listed["runs"][0]["runtime"],canonical);
    }
    // Unknown and unconfigured runtimes are still refused by the same gate.
    let (dir,mut spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let caller=user("manager",UserRole::Manager);
    assert_eq!(catalog(&home,&caller,"worker").unwrap()["runtimes"],json!(["claude"]));
    let store=TaskStore::open(&home).unwrap();
    for runtime in ["grok","cursor"] {
        spec.runtime=runtime.into();
        assert!(create(&home,&store,&broker,&caller,"worker","Explore","Improve fixture",spec.clone()).await.is_err(),"{runtime} must be refused");
    }
    assert!(store.discovery_tasks().await.unwrap().is_empty());
}

fn rpc_payload(frame: crate::protocol::WsFrame) -> Value {
    match frame {
        crate::protocol::WsFrame::Response {ok:true,payload:Some(payload),..}=>payload,
        other=>panic!("unexpected real RPC response: {other:?}"),
    }
}
struct JourneyPolicy;
impl super::super::contracts::PolicySource for JourneyPolicy {
    fn current(&self)->super::super::contracts::PolicyVersion {
        super::super::contracts::PolicyVersion {policy_id:super::super::policy::BASELINE_POLICY_ID.into(),source_sha256:None}
    }
    fn instantiate(&self,_:f64)->Result<Box<dyn super::super::policy::ExplorationPolicy+Send>,super::super::contracts::PolicyDegraded> {
        Ok(Box::new(super::super::policy::BaselineParallelRefine))
    }
    fn degraded(&self)->Option<super::super::contracts::PolicyDegraded> {None}
}
struct JourneyRunner {
    budget:super::super::budget::SharedBudget,
    calls:Arc<std::sync::atomic::AtomicUsize>, blocked:bool,
}
#[async_trait::async_trait]
impl super::super::contracts::AttemptRunner for JourneyRunner {
    async fn run_attempt(&self,request:&super::super::contracts::AttemptRequest)
        ->Result<super::super::contracts::AttemptOutcome,super::super::contracts::AttemptInfraError> {
        let call=self.budget.reserve_call()?;
        self.calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        if self.blocked {
            self.budget.cancelled().await;
            self.budget.finish_accounted_call(call,0.0,super::super::tree::CostSource::Estimated);
            return Err(super::super::contracts::AttemptInfraError::BudgetExhausted);
        }
        let attempt=request.cell_id.rsplit("-a").next().unwrap().parse::<u32>().unwrap();
        std::fs::write(request.node_dir.join("score.txt"),(1+attempt).to_string()).unwrap();
        std::fs::write(request.node_dir.join("candidate.txt"),format!("offline candidate {}",request.cell_id)).unwrap();
        self.budget.finish_accounted_call(call,0.01,super::super::tree::CostSource::Estimated);
        Ok(super::super::contracts::AttemptOutcome {
            cost:super::super::tree::NodeCost {usd:0.01,usd_source:super::super::tree::CostSource::Estimated,..Default::default()},
            runtime:"offline-fixture".into(),model:"offline-fixture-observed".into(),
            // Mocks provide no OS isolation evidence. The real ledger must
            // honestly mark this test run unconfined/degraded.
            isolation:super::super::contracts::IsolationBackend::None,
            final_text:"offline candidate ready".into(),timed_out:false,infra_retries:0,
        })
    }
}
struct JourneyEvaluator;
#[async_trait::async_trait]
impl super::super::contracts::Evaluator for JourneyEvaluator {
    async fn score(&self,request:&super::super::contracts::ScoreRequest)->super::super::contracts::ScoreOutcome {
        let value=std::fs::read_to_string(request.node_dir.join("score.txt")).ok()
            .and_then(|value|value.parse::<f64>().ok()).unwrap_or(0.0);
        super::super::contracts::ScoreOutcome {evaluated:true,valid:true,score:Some(value),
            fail_class:super::super::tree::FailClass::Ok,diagnostics:None,
            isolation:super::super::contracts::IsolationBackend::None,wall_secs:0.0}
    }
}
fn journey_mode(calls:Arc<std::sync::atomic::AtomicUsize>,blocked:bool)->lifecycle::ExecutionMode {
    lifecycle::ExecutionMode::Injected(Box::new(move |budget|super::super::online::OnlineComponents {
        runner:Arc::new(JourneyRunner {budget,calls,blocked}),evaluator:Arc::new(JourneyEvaluator),
        policy:Arc::new(JourneyPolicy),dreaming:None,
    }))
}
async fn wait_terminal(home:&Path,task_id:&str)->TaskRow {
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let row=TaskStore::open(home).unwrap().get_task(task_id).await.unwrap().unwrap();
            if !matches!(row.status.as_str(),"queued"|"in_progress"|"pending_approval") {return row;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("the shared production lifecycle must settle")
}

#[tokio::test]
async fn discovery_real_rpc_approval_queue_online_terminal_tree_and_artifact_journey() {
    crate::discovery::maintenance::require_discovery_sweep_client!();
    let (dir,mut spec,_broker)=fixture(); let home=dir.path().canonicalize().unwrap(); spec.budget.max_rounds=1;
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    handler.set_task_store(Arc::new(TaskStore::open(&home).unwrap())).await;
    let employee=context("employee",UserRole::Employee); let manager=context("manager",UserRole::Manager);
    let catalog=rpc_payload(handler.handle("discovery.catalog",json!({"agent_id":"worker"}),&employee).await);
    assert_eq!(catalog["roots"][0]["id"],spec.approved_root_id);
    let created=rpc_payload(handler.handle("tasks.create",json!({"kind":"discovery","assigned_to":"worker",
        "title":"Explore","description":"Improve fixture","discovery":spec}),&employee).await);
    assert_eq!(created["status"],"pending_approval");
    let task=created["task_id"].as_str().unwrap(); let run=created["run_id"].as_str().unwrap();
    let approval=created["approval_id"].as_str().unwrap();
    rpc_payload(handler.handle("approvals.decide",json!({"id":approval,"approve":true}),&manager).await);
    let store=TaskStore::open(&home).unwrap();
    assert_eq!(store.get_task(task).await.unwrap().unwrap().status,"queued");
    assert_eq!(store.atomic_claim(task,"ordinary","2026-09-30T00:00:00Z","2099-09-30T00:00:00Z").await.unwrap(),crate::task_store::ClaimOutcome::NotClaimable);
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    lifecycle::poll_inner(&home,journey_mode(calls.clone(),false)).await.unwrap();
    let row=wait_terminal(&home,task).await;
    assert_eq!(row.status,"done","the real queue callback must run Online, not synthesize a terminal row");
    assert!(row.claimed_by.as_deref().unwrap().starts_with("discovery-"));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),4);
    let view=rpc_payload(handler.handle("discovery.tree",json!({"run_id":run}),&employee).await);
    assert_eq!(view["nodes"].as_array().unwrap().len(),4);
    assert_eq!(view["rounds"][0]["completion"].as_array().unwrap().len(),4);
    assert_eq!(view["run"]["artifact_verified"],true); assert_eq!(view["run"]["degraded"],true);
    assert_eq!(view["run"]["isolation_degraded"],true,"mock attempts carry no isolation evidence");
    assert_eq!(view["run"]["stop_code"],Value::Null); assert_eq!(view["run"]["approval_status"],"approved");
    assert_eq!(view["run"]["cost"]["usd_source"],"estimated");
    assert_eq!(view["run"]["cost"]["token_scope"],"evaluated_nodes");
    assert!(view["nodes"].as_array().unwrap().iter().all(|node|node["model"]=="offline-fixture-observed"));
    let artifact=rpc_payload(handler.handle("discovery.artifact",json!({"run_id":run}),&employee).await);
    let file=artifact["files"].as_array().unwrap().iter().find(|file|file["name"]=="candidate.txt").unwrap();
    let bytes=rpc_payload(handler.handle("discovery.artifact",json!({"run_id":run,"file_id":file["file_id"]}),&employee).await);
    assert!(!bytes["content_base64"].as_str().unwrap().is_empty());
    assert!(!view.to_string().contains(home.to_str().unwrap()));
    // Reopened service/DB state retains the exact checkpoint and terminal task.
    poll(&home).await.unwrap();
    assert_eq!(tree(&home,&user("employee",UserRole::Employee),run).await.unwrap()["run"]["artifact_verified"],true);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),4,"restart cannot claim a completed run again");
}

#[tokio::test]
async fn discovery_real_rpc_cancellation_stops_shared_budget_and_survives_restarted_poll() {
    crate::discovery::maintenance::require_discovery_sweep_client!();
    let (dir,mut spec,_broker)=fixture(); let home=dir.path().canonicalize().unwrap(); spec.budget.max_rounds=1;
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    handler.set_task_store(Arc::new(TaskStore::open(&home).unwrap())).await;
    let manager=context("manager",UserRole::Manager);
    let created=rpc_payload(handler.handle("tasks.create",json!({"kind":"discovery","assigned_to":"worker",
        "title":"Explore","description":"Improve fixture","discovery":spec}),&manager).await);
    let task=created["task_id"].as_str().unwrap(); let run=created["run_id"].as_str().unwrap();
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    lifecycle::poll_inner(&home,journey_mode(calls.clone(),true)).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        while calls.load(std::sync::atomic::Ordering::SeqCst)==0 {tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
    }).await.unwrap();
    rpc_payload(handler.handle("discovery.cancel",json!({"run_id":run}),&manager).await);
    assert_eq!(wait_terminal(&home,task).await.status,"cancelled");
    // Wait for the actual Online cancellation path to leave running status.
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let record=DiscoveryStore::open(&home).unwrap().load_run(run).unwrap().unwrap();
            if record.status!="running" {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let stopped=calls.load(std::sync::atomic::Ordering::SeqCst);
    poll(&home).await.unwrap();
    assert_eq!(TaskStore::open(&home).unwrap().get_task(task).await.unwrap().unwrap().status,"cancelled");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),stopped);
    let view=rpc_payload(handler.handle("discovery.tree",json!({"run_id":run}),&manager).await);
    assert_eq!(view["run"]["status"],"cancelled"); assert_eq!(view["run"]["can_cancel"],false);
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_expired_task_overrides_stale_running_ledger_in_real_rpc_tree_and_list() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let manager=user("manager",UserRole::Manager); let store=TaskStore::open(&home).unwrap();
    let created=create(&home,&store,&broker,&manager,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    assert!(store.claim_discovery(&created.task_id,"crashed-worker","2000-01-01T00:00:00Z").await.unwrap());
    let task=store.get_task(&created.task_id).await.unwrap().unwrap();
    let frozen=frozen_request(&home,&task).unwrap();
    let ledger=DiscoveryStore::open(&home).unwrap();
    ledger.create_run(&created.run_id,"Improve fixture","worker","score",&frozen.scorer_hash,spec.direction,&spec.budget).unwrap();
    ledger.attach_run_identity(&super::super::online::RunIdentity {run_id:created.run_id.clone(),task_id:Some(created.task_id.clone()),
        creator_id:manager.id(),creator_origin:manager.origin().into(),approved_root_id:Some(spec.approved_root_id)}).unwrap();
    // A still-valid host operator lease prevents startup from sweeping. Poll
    // must reconcile the expired task without taking or cleaning that owner.
    let _lease=super::super::maintenance::OperatorLeaseGuard::acquire(&home).unwrap();
    super::super::maintenance::on_gateway_start(&home).unwrap();
    assert_eq!(ledger.load_run(&created.run_id).unwrap().unwrap().status,"running");
    poll(&home).await.unwrap();
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"failed");
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    handler.set_task_store(Arc::new(TaskStore::open(&home).unwrap())).await;
    let ctx=context("manager",UserRole::Manager);
    let view=rpc_payload(handler.handle("discovery.tree",json!({"run_id":created.run_id}),&ctx).await);
    assert_eq!(view["run"]["status"],"failed","a terminal task cannot be displayed as running by its stale ledger");
    assert_eq!(view["run"]["can_cancel"],false);
    let listed=rpc_payload(handler.handle("discovery.list",json!({"agent_id":"worker"}),&ctx).await);
    assert_eq!(listed["runs"][0]["status"],"failed");
    assert_eq!(listed["runs"][0]["can_cancel"],false);
    // The public projection must not acquire the host lease or destructively
    // sweep another owner's possible processes just to correct its status.
    assert_eq!(ledger.load_run(&created.run_id).unwrap().unwrap().status,"running");
}

fn seed_finished_run(home: &Path, caller: &TrustedCaller, created: &CreatedDiscovery,
    spec: &PublicDiscoverySpec, status: &str, report: Option<Value>) -> DiscoveryStore {
    let store=DiscoveryStore::open(home).unwrap();
    store.create_run(&created.run_id,"Improve fixture","worker","score",&"a".repeat(64),Direction::Max,&spec.budget).unwrap();
    store.attach_run_identity(&super::super::online::RunIdentity {run_id:created.run_id.clone(),
        task_id:Some(created.task_id.clone()),creator_id:caller.id(),creator_origin:caller.origin().into(),
        approved_root_id:Some(spec.approved_root_id.clone())}).unwrap();
    store.finish_run(&created.run_id,status,None).unwrap();
    if let Some(report)=report {
        let reports=home.join("discovery/reports"); workspace::create_private_directory(&reports).unwrap();
        std::fs::write(reports.join(format!("{}.json",created.run_id)),serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    store
}
fn listed_run(listing: &Value, run_id: &str) -> Value {
    listing["runs"].as_array().unwrap().iter().find(|run|run["run_id"]==run_id).unwrap().clone()
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_legacy_no_account_report_is_not_shown_as_missing_isolation() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let manager=user("manager",UserRole::Manager);
    let created=create(&home,&store,&broker,&manager,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    // Exact shape of the report from the live run, written before `stop_code` existed.
    let reason="no usable account (all cooling down, exhausted or auth-dead)";
    seed_finished_run(&home,&manager,&created,&spec,"degraded",Some(json!({"run_id":created.run_id,"status":"degraded",
        "stop_reason":reason,"best_cell_id":null,"best_score":null,"artifact":null,
        "budget":{"agent_calls":0},"rounds":[],"policy_degraded":null,"isolation_warning":null})));
    for run in [listed_run(&list(&home,&manager,None,20).await.unwrap(),&created.run_id),
        tree(&home,&manager,&created.run_id).await.unwrap()["run"].clone()] {
        assert_eq!(run["stop_code"],"no_account");
        assert_eq!(run["isolation_degraded"],false,"an account outage is not an isolation loss");
        assert_eq!(run["degraded"],true,"the legacy key keeps its meaning");
        assert_eq!(run["approval_status"],"not_required"); assert_eq!(run["approval_expires_at"],Value::Null);
        assert_eq!(run["cancel_code"],Value::Null);
        assert!(!run.to_string().contains(reason),"raw stop reasons stay private");
    }
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_isolation_degraded_tracks_only_real_isolation_loss() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let manager=user("manager",UserRole::Manager);
    let unconfined=create(&home,&store,&broker,&manager,"worker","A","Improve fixture",spec.clone()).await.unwrap();
    seed_finished_run(&home,&manager,&unconfined,&spec,"degraded",None).mark_unconfined(&unconfined.run_id).unwrap();
    let warned=create(&home,&store,&broker,&manager,"worker","B","Improve fixture",spec.clone()).await.unwrap();
    seed_finished_run(&home,&manager,&warned,&spec,"degraded",Some(json!({"stop_reason":"integrity_changed: /private/host/path",
        "stop_code":"integrity_changed","isolation_warning":"experimental unconfined attempts"})));
    let forged=create(&home,&store,&broker,&manager,"worker","C","Improve fixture",spec.clone()).await.unwrap();
    seed_finished_run(&home,&manager,&forged,&spec,"degraded",Some(json!({"stop_reason":"cleanup secret /etc/x","stop_code":"/etc/x"})));
    let oversized=create(&home,&store,&broker,&manager,"worker","D","Improve fixture",spec.clone()).await.unwrap();
    seed_finished_run(&home,&manager,&oversized,&spec,"budget_exhausted",None);
    // Valid JSON whose stored token would otherwise win: only the size cap rejects it.
    let padded=json!({"stop_code":"no_account","pad":"x".repeat(256*1024)});
    std::fs::write(home.join("discovery/reports").join(format!("{}.json",oversized.run_id)),serde_json::to_vec(&padded).unwrap()).unwrap();
    let small=create(&home,&store,&broker,&manager,"worker","E","Improve fixture",spec.clone()).await.unwrap();
    seed_finished_run(&home,&manager,&small,&spec,"budget_exhausted",Some(json!({"stop_code":"no_account","pad":"x".repeat(1024)})));
    let listing=list(&home,&manager,None,20).await.unwrap();
    let run=listed_run(&listing,&unconfined.run_id);
    assert_eq!(run["isolation_degraded"],true); assert_eq!(run["stop_code"],Value::Null);
    let run=listed_run(&listing,&warned.run_id);
    assert_eq!(run["isolation_degraded"],true); assert_eq!(run["stop_code"],"integrity_changed");
    let run=listed_run(&listing,&forged.run_id);
    assert_eq!(run["stop_code"],"other","an out-of-vocabulary stored token never reaches the public payload");
    assert_eq!(run["isolation_degraded"],false);
    let run=listed_run(&listing,&oversized.run_id);
    assert_eq!(run["stop_code"],"budget_exhausted","an over-cap report falls back to the run status");
    assert_eq!(listed_run(&listing,&small.run_id)["stop_code"],"no_account","an under-cap report is read");
    assert!(!listing.to_string().contains("/private/host/path") && !listing.to_string().contains("/etc/x"));
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_approval_status_pending_approved_denied_and_legacy() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let employee=user("employee",UserRole::Employee); let manager=user("manager",UserRole::Manager);
    let pending=create(&home,&store,&broker,&employee,"worker","Pending","Improve fixture",spec.clone()).await.unwrap();
    let record=broker.get(&ApprovalId::from(pending.approval_id.clone().unwrap())).await.unwrap().unwrap();
    persist_approval(&home,&record);
    let run=tree(&home,&employee,&pending.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"pending");
    let expires=chrono::DateTime::parse_from_rfc3339(run["approval_expires_at"].as_str().unwrap()).unwrap();
    assert_eq!(Some(expires.timestamp()),record.expires_at_epoch());
    assert_eq!(listed_run(&list(&home,&employee,None,20).await.unwrap(),&pending.run_id)["approval_expires_at"],run["approval_expires_at"]);
    assert_eq!(run["can_cancel"],true); assert_eq!(run["cancel_code"],Value::Null);

    let approved=create(&home,&store,&broker,&employee,"worker","Approved","Improve fixture",spec.clone()).await.unwrap();
    let id=ApprovalId::from(approved.approval_id.unwrap());
    prepare_approval_decision(&store,&broker,&manager,&id,true).await.unwrap();
    broker.decide(&id,true,"dashboard:manager").await.unwrap();
    authorize_approved_request(&store,&broker,&manager,&id).await.unwrap();
    let run=tree(&home,&employee,&approved.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"approved"); assert_eq!(run["approval_expires_at"],Value::Null);

    let denied=create(&home,&store,&broker,&employee,"worker","Denied","Improve fixture",spec.clone()).await.unwrap();
    let id=ApprovalId::from(denied.approval_id.unwrap());
    prepare_approval_decision(&store,&broker,&manager,&id,false).await.unwrap();
    broker.decide(&id,false,"dashboard:manager").await.unwrap();
    authorize_approved_request(&store,&broker,&manager,&id).await.unwrap();
    let run=tree(&home,&employee,&denied.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["status"],"cancelled"); assert_eq!(run["approval_status"],"denied");
    assert_eq!(run["cancel_code"],"approval_denied"); assert_eq!(run["approval_expires_at"],Value::Null);

    let legacy=create(&home,&store,&broker,&employee,"worker","Legacy","Improve fixture",spec).await.unwrap();
    assert!(store.cancel_discovery(&legacy.task_id,"approval denied or expired").await.unwrap());
    let run=tree(&home,&employee,&legacy.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"denied"); assert_eq!(run["cancel_code"],"approval_denied");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_ttl_expiry_is_reported_as_expired_not_denied() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let mut record=broker.get(&ApprovalId::from(created.approval_id.clone().unwrap())).await.unwrap().unwrap();
    record.created_at="2000-01-01T00:00:00Z".into();
    persist_approval(&home,&record);
    let _lease=super::super::maintenance::OperatorLeaseGuard::acquire(&home).unwrap();
    poll(&home).await.unwrap();
    let row=store.get_task(&created.task_id).await.unwrap().unwrap();
    assert_eq!(row.status,"cancelled"); assert_eq!(row.blocked_reason.as_deref(),Some(CANCEL_APPROVAL_EXPIRED));
    let run=tree(&home,&employee,&created.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"expired"); assert_eq!(run["cancel_code"],"approval_expired");
    assert_eq!(run["approval_expires_at"],Value::Null);
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_withdrawal_resolves_the_manager_card_and_can_never_authorize() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let employee=user("employee",UserRole::Employee); let manager=user("manager",UserRole::Manager);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    let id=ApprovalId::from(created.approval_id.clone().unwrap());
    persist_approval(&home,&broker.get(&id).await.unwrap().unwrap());
    let durable=ApprovalBroker::open(&home).unwrap();
    assert_eq!(durable.list_pending(None).await.unwrap().len(),1);
    cancel(&home,&employee,&created.run_id).await.unwrap();
    let record=durable.get(&id).await.unwrap().unwrap();
    assert_eq!(record.status,ApprovalStatus::Denied,"withdrawal is a DENY-type terminal state");
    assert_eq!(record.decided_by.as_deref(),Some(WITHDRAWN_DECIDER));
    assert!(durable.list_pending(None).await.unwrap().is_empty(),"the manager inbox no longer holds a dead card");
    assert!(store.discovery_decision_receipt(id.as_str()).await.unwrap().is_none(),"withdrawal never mints a receipt");
    assert!(prepare_approval_decision(&store,&durable,&manager,&id,true).await.is_err());
    assert!(authorize_approved_request(&store,&durable,&manager,&id).await.is_err());
    assert!(store.discovery_decision_receipt(id.as_str()).await.unwrap().is_none());
    let _lease=super::super::maintenance::OperatorLeaseGuard::acquire(&home).unwrap();
    poll(&home).await.unwrap();
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"cancelled","a later decide cannot start the run");
    let run=tree(&home,&employee,&created.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"withdrawn"); assert_eq!(run["cancel_code"],"cancelled_by_user");
    assert_eq!(run["approval_expires_at"],Value::Null); assert_eq!(run["stop_code"],Value::Null);

    // Crash window: task CAS committed, broker row still pending -> poll resolves it.
    let crashed=create(&home,&store,&broker,&employee,"worker","Crash","Improve fixture",spec).await.unwrap();
    let crashed_id=ApprovalId::from(crashed.approval_id.unwrap());
    persist_approval(&home,&broker.get(&crashed_id).await.unwrap().unwrap());
    assert!(store.cancel_pending_discovery(&crashed.task_id,CANCEL_WITHDRAWN).await.unwrap());
    assert_eq!(durable.get(&crashed_id).await.unwrap().unwrap().status,ApprovalStatus::Pending);
    poll(&home).await.unwrap();
    let record=durable.get(&crashed_id).await.unwrap().unwrap();
    assert_eq!(record.status,ApprovalStatus::Denied); assert_eq!(record.decided_by.as_deref(),Some(WITHDRAWN_DECIDER));
    assert!(store.discovery_decision_receipt(crashed_id.as_str()).await.unwrap().is_none());
    assert!(!store.cancel_pending_discovery(&created.task_id,CANCEL_WITHDRAWN).await.unwrap(),"terminal tasks stay terminal");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_cancel_after_manager_approval_keeps_approved_and_broker_row() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap();
    let employee=user("employee",UserRole::Employee); let manager=user("manager",UserRole::Manager);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let id=ApprovalId::from(created.approval_id.clone().unwrap());
    persist_approval(&home,&broker.get(&id).await.unwrap().unwrap());
    let durable=ApprovalBroker::open(&home).unwrap();
    prepare_approval_decision(&store,&durable,&manager,&id,true).await.unwrap();
    durable.decide(&id,true,"dashboard:manager").await.unwrap();
    authorize_approved_request(&store,&durable,&manager,&id).await.unwrap();
    assert_eq!(store.get_task(&created.task_id).await.unwrap().unwrap().status,"queued");
    // CAS loser: no longer pending, so the general cancel path applies.
    assert!(!store.cancel_pending_discovery(&created.task_id,CANCEL_WITHDRAWN).await.unwrap());
    cancel(&home,&employee,&created.run_id).await.unwrap();
    let row=store.get_task(&created.task_id).await.unwrap().unwrap();
    assert_eq!(row.status,"cancelled"); assert_eq!(row.blocked_reason.as_deref(),Some(CANCEL_BY_CALLER));
    let record=durable.get(&id).await.unwrap().unwrap();
    assert_eq!(record.status,ApprovalStatus::Approved); assert_eq!(record.decided_by.as_deref(),Some("dashboard:manager"));
    let run=listed_run(&list(&home,&employee,None,20).await.unwrap(),&created.run_id);
    assert_eq!(run["approval_status"],"approved"); assert_eq!(run["cancel_code"],"cancelled_by_user");
    assert_eq!(run["stop_code"],Value::Null); assert_eq!(run["approval_expires_at"],Value::Null);
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_withdrawn_is_visible_through_list() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    persist_approval(&home,&broker.get(&ApprovalId::from(created.approval_id.clone().unwrap())).await.unwrap().unwrap());
    cancel(&home,&employee,&created.run_id).await.unwrap();
    let run=listed_run(&list(&home,&employee,None,20).await.unwrap(),&created.run_id);
    assert_eq!(run["status"],"cancelled"); assert_eq!(run["approval_status"],"withdrawn");
    assert_eq!(run["cancel_code"],"cancelled_by_user"); assert_eq!(run["approval_expires_at"],Value::Null);
    assert_eq!(run["can_cancel"],false);
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_unexplained_decision_without_receipt_is_neutral_never_approved() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let id=created.approval_id.clone().unwrap();
    // Force each non-cancelled state without a receipt (e.g. a broker approve that bypassed the receipt).
    for status in ["queued","in_progress","done","failed"] {
        rusqlite::Connection::open(home.join("tasks.db")).unwrap().execute(
            "UPDATE tasks SET status=?2 WHERE id=?1",rusqlite::params![created.task_id,status]).unwrap();
        let run=tree(&home,&employee,&created.run_id).await.unwrap()["run"].clone();
        assert_eq!(run["approval_status"],"decided","{status} without a receipt is neither approved nor denied");
        assert_eq!(run["cancel_code"],Value::Null);
    }
    // A broker row that says approved is still not authority for the public view.
    broker.decide(&ApprovalId::from(id.clone()),true,"dashboard:manager").await.unwrap();
    persist_approval(&home,&broker.get(&ApprovalId::from(id.clone())).await.unwrap().unwrap());
    assert_eq!(tree(&home,&employee,&created.run_id).await.unwrap()["run"]["approval_status"],"decided");
    // A cancel with an unknown reason and no receipt stays neutral too.
    rusqlite::Connection::open(home.join("tasks.db")).unwrap().execute(
        "UPDATE tasks SET status='cancelled',blocked_reason='fixture seed' WHERE id=?1",rusqlite::params![created.task_id]).unwrap();
    let run=tree(&home,&employee,&created.run_id).await.unwrap()["run"].clone();
    assert_eq!(run["approval_status"],"decided"); assert_eq!(run["cancel_code"],Value::Null);
    assert!(store.discovery_decision_receipt(&id).await.unwrap().is_none());
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_withdraw_refuses_a_record_bound_to_another_task() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let id=ApprovalId::from(created.approval_id.clone().unwrap());
    persist_approval(&home,&broker.get(&id).await.unwrap().unwrap());
    let durable=ApprovalBroker::open(&home).unwrap();
    withdraw_approval(&durable,id.as_str(),"some-other-task").await;
    assert_eq!(durable.get(&id).await.unwrap().unwrap().status,ApprovalStatus::Pending,"a mismatched binding changes nothing");
    withdraw_approval(&durable,id.as_str(),&created.task_id).await;
    let record=durable.get(&id).await.unwrap().unwrap();
    assert_eq!(record.status,ApprovalStatus::Denied); assert_eq!(record.decided_by.as_deref(),Some(WITHDRAWN_DECIDER));
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_generic_task_remove_and_handoff_cannot_touch_discovery_rows() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec.clone()).await.unwrap();
    let id=ApprovalId::from(created.approval_id.clone().unwrap());
    persist_approval(&home,&broker.get(&id).await.unwrap().unwrap());
    let ledger=DiscoveryStore::open(&home).unwrap();
    ledger.create_run(&created.run_id,"Improve fixture","worker","score",&"a".repeat(64),Direction::Max,&spec.budget).unwrap();
    ledger.attach_run_identity(&super::super::online::RunIdentity {run_id:created.run_id.clone(),
        task_id:Some(created.task_id.clone()),creator_id:employee.id(),creator_origin:employee.origin().into(),
        approved_root_id:Some(spec.approved_root_id.clone())}).unwrap();
    // Store-level: every caller of the generic delete is covered.
    assert_eq!(store.remove_task(&created.task_id).await.unwrap_err(),"discovery tasks require the dedicated lifecycle service");
    // Real RPC surfaces the refusal to an operator-level employee.
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    handler.set_task_store(Arc::new(TaskStore::open(&home).unwrap())).await;
    match handler.handle("tasks.remove",json!({"task_id":created.task_id}),&context("employee",UserRole::Employee)).await {
        crate::protocol::WsFrame::Response {ok:false,error:Some(error),..}=>
            assert!(error.to_string().contains("discovery tasks require the dedicated lifecycle service"),"{error}"),
        other=>panic!("tasks.remove must refuse a discovery row: {other:?}"),
    }
    // WP4 hand-off must not reassign a run bound to its original agent.
    assert_eq!(store.reassign_open_tasks("worker","successor","2026-10-01T00:00:00Z").await.unwrap(),0);
    let row=store.get_task(&created.task_id).await.unwrap().unwrap();
    assert_eq!(row.status,"pending_approval"); assert_eq!(row.assigned_to,"worker");
    assert_eq!(ledger.load_run(&created.run_id).unwrap().unwrap().task_id.as_deref(),Some(created.task_id.as_str()));
    assert_eq!(ApprovalBroker::open(&home).unwrap().get(&id).await.unwrap().unwrap().status,ApprovalStatus::Pending);
    let run=listed_run(&list(&home,&employee,None,20).await.unwrap(),&created.run_id);
    assert_eq!(run["approval_status"],"pending","the run stays visible and bound");
    // Ordinary tasks keep the generic delete and hand-off.
    let ordinary=TaskRow::new("ordinary-1".into(),"Plain".into(),"Plain work".into(),"medium".into(),"worker".into(),"user:employee".into());
    store.insert_task(&ordinary).await.unwrap();
    assert_eq!(store.reassign_open_tasks("worker","successor","2026-10-01T00:00:00Z").await.unwrap(),1);
    assert!(store.remove_task("ordinary-1").await.unwrap());
    assert!(store.get_task("ordinary-1").await.unwrap().is_none());
    assert!(!store.remove_task("missing-task").await.unwrap(),"an unknown id is still not-found, not a refusal");
}

#[tokio::test]
#[cfg_attr(not(unix), ignore = "discovery is unix-only: private-ACL, link-count and flock checks fail closed on this platform")]
async fn discovery_rows_carry_kind_through_the_real_tasks_list_rpc() {
    let (dir,spec,broker)=fixture(); let home=dir.path().canonicalize().unwrap();
    let store=TaskStore::open(&home).unwrap(); let employee=user("employee",UserRole::Employee);
    let created=create(&home,&store,&broker,&employee,"worker","Explore","Improve fixture",spec).await.unwrap();
    let ordinary=TaskRow::new("ordinary-kind".into(),"Plain".into(),"Plain work".into(),"medium".into(),"worker".into(),"user:manager".into());
    store.insert_task(&ordinary).await.unwrap();
    let handler=crate::handlers::MethodHandler::new(home.clone()).await;
    handler.set_task_store(Arc::new(TaskStore::open(&home).unwrap())).await;
    let listed=rpc_payload(handler.handle("tasks.list",json!({"agent_id":"worker"}),&context("manager",UserRole::Manager)).await);
    let rows=listed.get("tasks").and_then(Value::as_array).or_else(||listed.as_array()).expect("tasks array");
    let kind_of=|id:&str|rows.iter().find(|row|row["id"]==id).unwrap_or_else(||panic!("{id} missing from {listed}"))["kind"].clone();
    assert_eq!(kind_of(&created.task_id),"discovery");
    assert_eq!(kind_of("ordinary-kind"),"task");
}
