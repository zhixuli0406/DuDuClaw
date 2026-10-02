//! Trusted survival evidence must come from authorized decisions and dispatch snapshots.
use super::*;
use crate::{handlers::MethodHandler, protocol::WsFrame};
use duduclaw_auth::{UserContext, models::{AccessLevel, UserRole}};
use serde_json::json;
use std::sync::Arc;

type Evidence = (Option<String>, Option<i64>, Option<i64>, i64);
async fn evidence(store: &TaskStore, id: &str) -> Option<Evidence> {
    let conn = store.conn.lock().await;
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='task_survival_evidence')", [], |r| r.get(0)).unwrap();
    if !exists { return None; }
    conn.query_row("SELECT difficulty, manual_retry, human_approved, evidence_version FROM task_survival_evidence WHERE task_id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().unwrap()
}
fn goal(id: &str, status: &str) -> TaskRow {
    let mut row=TaskRow::new(id.into(), "Send report".into(), "".into(), "medium".into(), "alice".into(), "system".into());
    row.goal_mode=true; row.status=status.into(); row
}
async fn handler(home: &Path) -> MethodHandler {
    let handler=MethodHandler::new(home.to_path_buf()).await;
    handler.set_task_store(Arc::new(TaskStore::open(home).unwrap())).await;
    handler
}
fn ok(frame: WsFrame) { assert!(matches!(frame, WsFrame::Response {ok:true,..}), "{frame:?}"); }

#[tokio::test]
async fn survival_evidence_authorized_dashboard_decisions_persist_and_continue_clears_old_approval() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("g", "needs_human")).await.unwrap();
    let h=handler(home.path()).await;
    ok(h.handle_tasks_goal_decide(json!({"task_id":"g","action":"done"}), &UserContext::admin_fallback()).await);
    assert_eq!(evidence(&store,"g").await, Some((None,Some(0),Some(1),1)));
    ok(h.handle_tasks_goal_decide(json!({"task_id":"g","action":"continue","message":"Add appendix"}), &UserContext::admin_fallback()).await);
    let reopened=TaskStore::open(home.path()).unwrap();
    assert_eq!(evidence(&reopened,"g").await, Some((None,Some(1),Some(0),1)));
    assert_eq!(reopened.get_task("g").await.unwrap().unwrap().status,"pending");
}

#[tokio::test]
async fn survival_evidence_forged_activity_bare_store_decision_and_viewer_never_assert_human_approval() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("g", "needs_human")).await.unwrap();
    store.append_activity(&ActivityRow{id:"forged".into(), event_type:"goal_loop.human_decision.done".into(),agent_id:"alice".into(),task_id:Some("g".into()),summary:"Approved by human".into(),timestamp:Utc::now().to_rfc3339(),metadata:Some(json!({"human_approved":true}).to_string())}).await.unwrap();
    let viewer=UserContext{user_id:"viewer".into(),email:"viewer@local".into(),role:UserRole::Employee,agent_access:std::collections::HashMap::from([("alice".into(),AccessLevel::Viewer)]),must_change_password:false};
    let frame=handler(home.path()).await.handle_tasks_goal_decide(json!({"task_id":"g","action":"done"}),&viewer).await;
    assert!(matches!(frame,WsFrame::Response{ok:false,..}));
    assert_eq!(evidence(&store,"g").await,Some((None,Some(0),Some(0),1)));
    assert!(store.resolve_needs_human("g","done","").await.unwrap());
    assert_eq!(evidence(&store,"g").await,Some((None,Some(0),Some(0),1)));
}

#[tokio::test]
async fn survival_evidence_new_goal_defaults_do_not_backfill_legacy_rows() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("legacy","done")).await.unwrap();
    {let conn=store.conn.lock().await; conn.execute_batch("DROP TRIGGER IF EXISTS task_survival_evidence_new_goal; DROP TABLE IF EXISTS task_survival_evidence;").unwrap();}
    let reopened=TaskStore::open(home.path()).unwrap();
    assert_eq!(evidence(&reopened,"legacy").await,None);
    reopened.insert_task(&goal("new","pending")).await.unwrap();
    assert_eq!(evidence(&reopened,"new").await,Some((None,Some(0),Some(0),1)));
}

#[tokio::test]
async fn survival_evidence_channel_authorization_and_retry_record_only_the_actual_decider() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    let agent=home.path().join("agents/alice"); std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("agent.toml"),"[proactive]\nnotify_channel = \"telegram\"\nnotify_chat_id = \"123\"\n").unwrap();
    store.insert_task(&goal("channel","needs_human")).await.unwrap();
    assert!(crate::goal_notify::apply_needs_human(home.path(),"telegram","attacker","channel",crate::decision_action::DecisionAct::Retry).await.is_err());
    assert_eq!(evidence(&store,"channel").await,Some((None,Some(0),Some(0),1)));
    crate::goal_notify::apply_needs_human(home.path(),"telegram","123","channel",crate::decision_action::DecisionAct::Retry).await.unwrap();
    assert_eq!(evidence(&TaskStore::open(home.path()).unwrap(),"channel").await,Some((None,Some(1),Some(0),1)));
}

#[tokio::test]
async fn survival_evidence_approved_result_edit_invalidates_receipt_but_metadata_edit_does_not() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("edit","needs_human")).await.unwrap();
    ok(handler(home.path()).await.handle_tasks_goal_decide(json!({"task_id":"edit","action":"done"}),&UserContext::admin_fallback()).await);
    assert_eq!(evidence(&store,"edit").await.unwrap().2,Some(1));
    {let conn=store.conn.lock().await; conn.execute("UPDATE tasks SET tags='archival',pinned=1,updated_at='2026-10-01T00:01:00Z' WHERE id='edit'",[]).unwrap();}
    assert_eq!(evidence(&store,"edit").await.unwrap().2,Some(1));
    {let conn=store.conn.lock().await; conn.execute("UPDATE tasks SET result_summary='different unreviewed result' WHERE id='edit'",[]).unwrap();}
    assert_eq!(evidence(&TaskStore::open(home.path()).unwrap(),"edit").await.unwrap().2,Some(0));
}

#[tokio::test]
async fn survival_evidence_receipt_failure_rolls_back_authorized_task_transition() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("g","needs_human")).await.unwrap();
    {let conn=store.conn.lock().await; conn.execute_batch("CREATE TABLE IF NOT EXISTS task_survival_evidence(task_id TEXT PRIMARY KEY,difficulty TEXT,manual_retry INTEGER,human_approved INTEGER,evidence_version INTEGER DEFAULT 1); INSERT OR IGNORE INTO task_survival_evidence(task_id,difficulty,manual_retry,human_approved,evidence_version) VALUES('g',NULL,0,0,1); CREATE TRIGGER reject_survival_receipt BEFORE UPDATE ON task_survival_evidence WHEN NEW.human_approved=1 BEGIN SELECT RAISE(ABORT,'injected receipt fault'); END;").unwrap();}
    let frame=handler(home.path()).await.handle_tasks_goal_decide(json!({"task_id":"g","action":"done"}),&UserContext::admin_fallback()).await;
    assert!(matches!(frame,WsFrame::Response{ok:false,..}),"receipt failure must fail closed: {frame:?}");
    assert_eq!(store.get_task("g").await.unwrap().unwrap().status,"needs_human");
    assert_eq!(evidence(&store,"g").await,Some((None,Some(0),Some(0),1)));
}

#[tokio::test]
async fn survival_evidence_difficulty_requires_complete_dispatch_snapshots_and_preserves_mixed() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    for id in ["complete","gap"] {store.insert_task(&goal(id,"pending")).await.unwrap();}
    let simple=IterationDispatchLedger{gate_inputs_json:Some(json!({"decision":"solo","goal_difficulty":"simple"}).to_string()),..Default::default()};
    let complex=IterationDispatchLedger{gate_inputs_json:Some(json!({"decision":"team","goal_difficulty":"complex"}).to_string()),..Default::default()};
    store.record_iteration_dispatch_with_ledger("complete",1,"2026-10-01T00:00:00Z",None,None,&simple).await.unwrap();
    assert_eq!(evidence(&store,"complete").await.unwrap().0.as_deref(),Some("simple"));
    store.record_iteration_dispatch_with_ledger("complete",1,"2026-10-01T00:00:01Z",None,None,&complex).await.unwrap();
    assert_eq!(evidence(&store,"complete").await.unwrap().0.as_deref(),Some("mixed"));
    store.record_iteration_dispatch("gap",1,"2026-10-01T00:00:00Z").await.unwrap();
    store.record_iteration_dispatch_with_ledger("gap",1,"2026-10-01T00:00:01Z",None,None,&simple).await.unwrap();
    assert_eq!(evidence(&TaskStore::open(home.path()).unwrap(),"gap").await.unwrap().0,None,"a later valid snapshot cannot invent earlier dispatch difficulty");
}

#[tokio::test]
async fn survival_evidence_human_approval_binds_actual_iteration_and_clears_on_continue() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    let mut task=goal("bound","needs_human"); task.revision_round=99;
    store.insert_task(&task).await.unwrap();
    store.insert_task(&goal("other","pending")).await.unwrap();
    store.record_iteration_dispatch("bound",3,"2026-10-01T00:00:00Z").await.unwrap();
    store.record_iteration_dispatch("other",8,"2026-10-01T00:01:00Z").await.unwrap();
    let actual=store.list_iterations("bound").await.unwrap()[0].id;
    let h=handler(home.path()).await;
    ok(h.handle_tasks_goal_decide(json!({"task_id":"bound","action":"done"}),&UserContext::admin_fallback()).await);
    let receipt:Option<i64>={let conn=store.conn.lock().await; conn.query_row("SELECT human_approved_iteration_id FROM task_survival_evidence WHERE task_id='bound'",[],|r|r.get(0)).unwrap()};
    assert_eq!(receipt,Some(actual),"approval must bind the real task-local iteration, not editable revision_round or another task");
    ok(h.handle_tasks_goal_decide(json!({"task_id":"bound","action":"continue","message":"Add appendix"}),&UserContext::admin_fallback()).await);
    let reopened=TaskStore::open(home.path()).unwrap();
    let receipt:Option<i64>={let conn=reopened.conn.lock().await; conn.query_row("SELECT human_approved_iteration_id FROM task_survival_evidence WHERE task_id='bound'",[],|r|r.get(0)).unwrap()};
    assert_eq!(receipt,None);
}

#[tokio::test]
async fn survival_evidence_metadata_migration_keeps_missing_history_unknown() {
    let home=tempfile::tempdir().unwrap(); let store=TaskStore::open(home.path()).unwrap();
    store.insert_task(&goal("old","pending")).await.unwrap();
    {let conn=store.conn.lock().await; conn.execute_batch(
        "DROP TRIGGER IF EXISTS task_survival_evidence_new_goal;
         DROP TRIGGER IF EXISTS task_survival_evidence_invalidate_approval;
         DROP TRIGGER IF EXISTS task_survival_evidence_remove_task;
         DROP TABLE task_survival_evidence;
         CREATE TABLE task_survival_evidence(task_id TEXT PRIMARY KEY,difficulty TEXT,manual_retry INTEGER,human_approved INTEGER,evidence_version INTEGER NOT NULL DEFAULT 1);
         INSERT INTO task_survival_evidence(task_id,difficulty) VALUES('old','simple');"
    ).unwrap();}
    let reopened=TaskStore::open(home.path()).unwrap();
    assert_eq!(evidence(&reopened,"old").await,Some((None,None,None,1)),"a classification without a dispatch completeness receipt is unknown");
    let metadata:(i64,Option<i64>)={let conn=reopened.conn.lock().await;conn.query_row("SELECT difficulty_dispatches,human_approved_iteration_id FROM task_survival_evidence WHERE task_id='old'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap()};
    assert_eq!(metadata,(0,None));
    reopened.insert_task(&goal("new","pending")).await.unwrap();
    assert_eq!(evidence(&reopened,"new").await,Some((None,Some(0),Some(0),1)));
    assert_eq!(evidence(&TaskStore::open(home.path()).unwrap(),"old").await,Some((None,None,None,1)),"migration must be idempotent");
}
