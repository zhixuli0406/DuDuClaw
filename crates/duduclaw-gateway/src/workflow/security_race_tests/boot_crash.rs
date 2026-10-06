//! Process-crash cases for boot reconciliation: a child test process is killed
//! with `abort()` at a durable boundary, then a fresh service in this process
//! runs the boot pass on the same home.
use super::*;
use std::path::Path;

const SCENARIO_ENV: &str = "DUDUCLAW_BOOT_CRASH_SCENARIO";
const REPORT_ENV: &str = "DUDUCLAW_BOOT_CRASH_REPORT";
const CHILD: &str = "workflow::security_race_tests::boot_crash::boot_crash_child";

/// Child half. Does nothing unless the parent selected a scenario.
#[tokio::test]
async fn boot_crash_child() {
    let Ok(scenario) = std::env::var(SCENARIO_ENV) else { return };
    let report = std::env::var(REPORT_ENV).unwrap();
    let pilot = Pilot::new().await;
    std::fs::write(&report, pilot.home.path().to_string_lossy().as_bytes()).unwrap();
    let request = request(&pilot).await;
    human_accept(&pilot, &request).await;
    let id = request.activation_id.clone();
    if scenario == "after_cron_enable" {
        arm_crash(&scenario);
        let _ = pilot.service.commit_activation(&id).await;
        unreachable!("commit did not reach {scenario}");
    }
    pilot.service.commit_activation(&id).await.unwrap();
    std::fs::write(format!("{report}.activation"), &id).unwrap();
    arm_crash(&scenario);
    match scenario.as_str() {
        "after_revoke_ledger" => {
            let _ = pilot.service.revoke_activation(&id, "boot crash test").await;
        }
        "after_outbox_insert" | "after_queue_enqueue" => {
            let trigger = Trigger::Manual { request_id: uuid::Uuid::new_v4().to_string() };
            let _ = pilot.service.enqueue_trigger(&id, trigger, json!({})).await;
        }
        other => panic!("unknown scenario {other}"),
    }
    unreachable!("child did not reach {scenario}");
}

fn arm_crash(point: &str) {
    // Safety: single-threaded point in this child before the crashing call.
    unsafe { std::env::set_var(CRASH_AT_ENV, point) };
}

struct Crashed {
    home: std::path::PathBuf,
    activation_id: String,
    service: WorkflowService,
}
impl Drop for Crashed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Runs the child and requires that it died by abort, not by finishing.
async fn crash_at(scenario: &str) -> Crashed {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("home");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env(SCENARIO_ENV, scenario)
        .env(REPORT_ENV, &report)
        .env_remove(CRASH_AT_ENV)
        .status()
        .unwrap();
    assert!(!status.success(), "{scenario}: child finished instead of crashing");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(6), "{scenario}: child must die by abort");
    }
    let home = std::path::PathBuf::from(std::fs::read_to_string(&report).unwrap());
    let activation_id = match std::fs::read_to_string(format!("{}.activation", report.display())) {
        Ok(id) => id,
        Err(_) => only_activation(&home),
    };
    let binary = std::fs::canonicalize(std::env::var_os("DUDUCLAW_P1_PILOT_BINARY").unwrap()).unwrap();
    let service = WorkflowService::new(
        home.clone(),
        binary,
        Arc::new(WorkflowStore::open(&home).unwrap()),
        Arc::new(crate::approval::ApprovalBroker::open(&home).unwrap()),
    )
    .unwrap();
    Crashed { home, activation_id, service }
}

fn only_activation(home: &Path) -> String {
    rusqlite::Connection::open(home.join("workflow.db"))
        .unwrap()
        .query_row("SELECT activation_id FROM workflow_activations", [], |r| r.get(0))
        .unwrap()
}

async fn routine_enabled(c: &Crashed) -> bool {
    let record = c.service.activation(&c.activation_id).await.unwrap().unwrap();
    let cron = record.request.cron.as_ref().unwrap();
    let store = crate::cron_store::CronStore::open(&c.home).unwrap();
    store.get(&cron.cron_id).await.unwrap().unwrap().enabled
}

/// Queue rows from workflow; no database yet means no message was handed off.
fn queue_messages(home: &Path) -> Vec<String> {
    let path = home.join("message_queue.db");
    if !path.exists() {
        return Vec::new();
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    let mut q = conn
        .prepare("SELECT id FROM message_queue WHERE sender='workflow' ORDER BY id")
        .unwrap();
    q.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

fn run_rows(home: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(home.join("workflow.db")).unwrap();
    let mut q = conn
        .prepare("SELECT run_id,status FROM workflow_runs WHERE json_extract(record_json,'$.activation_id') IS NOT NULL")
        .unwrap();
    q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
}

/// After boot, a revoked or unarmed activation cannot start anything new.
async fn assert_no_new_effect(c: &Crashed) {
    let runs = run_rows(&c.home);
    let trigger = Trigger::Manual { request_id: uuid::Uuid::new_v4().to_string() };
    assert!(c.service.enqueue_trigger(&c.activation_id, trigger, json!({})).await.is_err());
    assert_eq!(run_rows(&c.home), runs, "a refused trigger created a run");
}

/// Ledger revoked, projection still Active, routine still enabled.
#[tokio::test]
async fn boot_crash_after_revoke_ledger_disables_and_never_revives() {
    let c = crash_at("after_revoke_ledger").await;
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Active);
    assert!(routine_enabled(&c).await);
    let report = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!((report.activations_revoked, report.routines_disabled), (1, 1), "{report:?}");
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Revoked);
    assert!(!routine_enabled(&c).await);
    assert_no_new_effect(&c).await;
    let again = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(again, BootReconcileReport::default(), "second boot changed state");
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Revoked);
}

/// Routine enabled, final projection never written (authority still live).
#[tokio::test]
async fn boot_crash_after_cron_enable_disables_unconfirmed_routine() {
    let c = crash_at("after_cron_enable").await;
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Arming);
    assert!(routine_enabled(&c).await);
    let report = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(report.routines_disabled, 1, "{report:?}");
    assert!(!routine_enabled(&c).await);
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Arming);
    assert_no_new_effect(&c).await;
}

/// Same crash, but a revoker revoked the ledger before compensation ran.
#[tokio::test]
async fn boot_crash_after_cron_enable_then_revoked_fails_closed() {
    let c = crash_at("after_cron_enable").await;
    let spec = c.service.activation(&c.activation_id).await.unwrap().unwrap().request.spec;
    c.service
        .broker
        .revoke_workflow_activation(&c.activation_id, &spec.hash(), "revoked while armed")
        .await
        .unwrap();
    let report = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!((report.activations_revoked, report.routines_disabled), (1, 1), "{report:?}");
    assert_eq!(c.service.activation(&c.activation_id).await.unwrap().unwrap().state, ActivationState::Revoked);
    assert!(!routine_enabled(&c).await);
    assert_no_new_effect(&c).await;
}

/// Outbox row durable, queue message missing: boot sends it once.
#[tokio::test]
async fn boot_crash_after_outbox_insert_delivers_once() {
    let c = crash_at("after_outbox_insert").await;
    let runs = run_rows(&c.home);
    assert_eq!(runs.len(), 1);
    assert!(queue_messages(&c.home).is_empty());
    let first = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(first.outbox_delivered, 1, "{first:?}");
    let second = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(second.outbox_delivered, 0, "{second:?}");
    assert_eq!(queue_messages(&c.home), vec![format!("workflow:{}", runs[0].0)]);
    assert!(routine_enabled(&c).await, "a live routine stays enabled");
}

/// Queue message written, outbox not yet marked: no second message.
#[tokio::test]
async fn boot_crash_after_queue_enqueue_never_duplicates() {
    let c = crash_at("after_queue_enqueue").await;
    let runs = run_rows(&c.home);
    assert_eq!(queue_messages(&c.home), vec![format!("workflow:{}", runs[0].0)]);
    let report = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(report.outbox_delivered, 1, "{report:?}");
    c.service.reconcile_on_boot().await.unwrap();
    assert_eq!(queue_messages(&c.home), vec![format!("workflow:{}", runs[0].0)]);
}

/// Outbox row durable and the ledger revoked before restart: the run is
/// blocked before its one delivery, so dispatching it does nothing.
#[tokio::test]
async fn boot_crash_outbox_with_revoked_authority_blocks_run() {
    let c = crash_at("after_outbox_insert").await;
    let spec = c.service.activation(&c.activation_id).await.unwrap().unwrap().request.spec;
    c.service
        .broker
        .revoke_workflow_activation(&c.activation_id, &spec.hash(), "revoked before boot")
        .await
        .unwrap();
    let report = c.service.reconcile_on_boot().await.unwrap();
    assert_eq!((report.runs_blocked, report.outbox_delivered), (1, 1), "{report:?}");
    let run_id = run_rows(&c.home)[0].0.clone();
    let run = c.service.store.get_run(&run_id).await.unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Blocked);
    let queue = crate::message_queue::MessageQueue::open(&c.home).unwrap();
    let message = queue.get_by_id(&format!("workflow:{run_id}")).await.unwrap().unwrap();
    let after = c.service.dispatch_queue_message(&message).await.unwrap();
    assert_eq!(after.status, RunStatus::Blocked);
    let effects: i64 = rusqlite::Connection::open(c.home.join("approvals.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM approval_operations WHERE run_id=?1", [&run_id], |r| r.get(0))
        .unwrap();
    assert_eq!(effects, 0, "a blocked run reached the operation ledger");
}
