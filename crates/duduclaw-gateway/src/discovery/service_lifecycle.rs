//! Background discovery execution never enters the ordinary worker dispatcher.
use super::*;
use std::time::Duration;
use super::super::{agent_spawn::AttemptRunnerFactory, budget::SharedBudget,
    evaluator::RegisteredEvaluator, maintenance::OperatorLeaseGuard,
    online::{OnlineComponents, RunIdentity, run_with_lease_and_identity},
    policy_runner::{ManagedPolicySource, PythonPolicyRuntime}};

fn expiry() -> String { (chrono::Utc::now() + chrono::Duration::seconds(60)).to_rfc3339() }

pub(super) enum ExecutionMode {
    Production,
    // Only Rust test code can replace adapters. Public requests cannot select
    // this path; claim, lease, validation, watcher and settlement stay shared.
    #[cfg(test)]
    Injected(Box<dyn FnOnce(SharedBudget) -> OnlineComponents + Send>),
}
pub async fn poll(home: &Path) -> Result<(), String> {
    poll_inner(home,ExecutionMode::Production).await
}
pub(super) async fn poll_inner(home: &Path, mode: ExecutionMode) -> Result<(),String> {
    let store = Arc::new(TaskStore::open(home)?);
    let tasks = store.discovery_active_tasks().await?;
    let broker = ApprovalBroker::open(home)?;
    // `list_pending` sweeps TTL expiry first, then returns what is still open.
    let pending = broker.list_pending(None).await?;
    // Crash window of a withdrawal: the task CAS committed but the broker row
    // is still pending. Only rows whose own task is already terminal qualify.
    for record in pending.iter().filter(|record| record.action_kind == "discovery") {
        let Some(task_id) = record.payload.get("task_id").and_then(Value::as_str) else { continue };
        let Ok(Some(task)) = store.get_task(task_id).await else { continue };
        if task.kind == TaskKind::Discovery && task.discovery_approval_id.as_deref() == Some(record.id.as_str())
            && matches!(task.status.as_str(), "cancelled"|"failed"|"done") {
            withdraw_approval(&broker, record.id.as_str(), &task.id).await;
        }
    }
    for task in tasks.iter().filter(|task| task.status == "pending_approval") {
        if let Some(id) = task.discovery_approval_id.as_deref() {
            if let Some(record) = broker.get(&ApprovalId::from(id.to_owned())).await? {
                if record.status == ApprovalStatus::Approved {
                    if let Err(error)=apply_receipted_decision(&store,&record,&ApprovalId::from(id.to_owned())).await {
                        tracing::warn!(task_id=task.id,error,"discovery approval recovery refused");
                    }
                } else if record.status == ApprovalStatus::Denied {
                    store.cancel_discovery(&task.id, CANCEL_APPROVAL_DENIED).await?;
                } else if record.status == ApprovalStatus::Expired {
                    store.cancel_discovery(&task.id, CANCEL_APPROVAL_EXPIRED).await?;
                }
            }
        }
    }
    for task in tasks.iter().filter(|task| task.status == "in_progress") {
        if let Some(lease) = task.lease_expires_at.as_deref() {
            if crate::task_store::lease_is_expired(lease, &chrono::Utc::now().to_rfc3339()) {
                store.interrupt_discovery(&task.id, lease).await?;
            }
        }
    }
    let tasks=store.discovery_active_tasks().await?;
    let Some(task) = tasks.into_iter().rev().find(|task| task.status == "queued" && !task.archived) else { return Ok(()); };
    // One shared host lease excludes CLI/operator runs and concurrent gateways.
    // Busy or unverified authority keeps the task queued and never spawns.
    let lease = match OperatorLeaseGuard::acquire(home) {
        Ok(lease) => lease,
        Err(error) => { tracing::debug!(error, "discovery queue waiting for host lease"); return Ok(()); }
    };
    let worker = format!("discovery-{}", uuid::Uuid::new_v4());
    if !store.claim_discovery(&task.id, &worker, &expiry()).await? { return Ok(()); }
    let home = home.to_path_buf();
    tokio::spawn(async move {
        let result = execute(home.clone(), store.clone(), task.clone(), worker.clone(), lease, mode).await;
        let (success, summary) = match result {
            Ok(report) => (report.best_cell_id.is_some(), json!({"run_id":report.run_id,
                "status":report.status,"best_cell_id":report.best_cell_id,
                "budget":report.budget,"policy_degraded":report.policy_degraded}).to_string()),
            Err(error) => {
                // Detailed diagnostics stay in host logs; do not leak paths to public rows.
                #[cfg(test)]
                eprintln!("discovery lifecycle fixture execution failed: {error}");
                tracing::error!(task_id=task.id, error, "public discovery failed");
                (false, "discovery execution failed; review the operator diagnostics".into())
            }
        };
        if let Err(error) = store.finish_discovery(&task.id, &worker, success, &summary).await {
            tracing::error!(task_id=task.id, error, "discovery terminal task persistence failed");
        }
    });
    Ok(())
}

async fn execute(home: PathBuf, store: Arc<TaskStore>, task: TaskRow, worker: String,
    lease: OperatorLeaseGuard, mode: ExecutionMode) -> Result<super::super::online::RunReport, String> {
    let frozen=frozen_request(&home,&task)?;
    frozen.defaults.bundle.validate()?;
    let config = load_config(&home)?;
    let (mut spec, hash) = validate_spec(&home, &config, &task.assigned_to, &task.description,
        &frozen.spec, frozen.defaults.bundle.beta)?;
    if hash != frozen.scorer_hash { return Err("evaluator changed since discovery approval".into()); }
    let budget = SharedBudget::new(spec.budget)?;
    lease.bind_budget(budget.clone())?;
    let run_id = task.discovery_run_id.clone().ok_or("missing run identity")?;
    let identity = RunIdentity { run_id: run_id.clone(), task_id: Some(task.id.clone()),
        creator_id: task.created_by.clone(), creator_origin: frozen.creator_origin.clone(),
        approved_root_id: Some(frozen.spec.approved_root_id.clone()) };
    // Persistent cancellation is checked from a fresh DB connection state, so
    // another gateway/MCP process can cancel this run without an in-memory token.
    let watch_store = store.clone();
    let watch_budget = budget.clone();
    let watch_task = task.id.clone();
    let watch_worker = worker.clone();
    let stopped = tokio_util::sync::CancellationToken::new();
    let watch_stop = stopped.clone();
    let watcher = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = watch_stop.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    match watch_store.get_task(&watch_task).await {
                        Ok(Some(row)) if row.status == "in_progress" && row.claimed_by.as_deref() == Some(&watch_worker) => {},
                        _ => { watch_budget.cancel(); break; }
                    }
                    if !matches!(watch_store.renew_discovery(&watch_task, &watch_worker, &expiry()).await, Ok(true)) {
                        watch_budget.cancel(); break;
                    }
                }
            }
        }
    });
    struct WatchGuard { stop: tokio_util::sync::CancellationToken, budget: SharedBudget }
    impl Drop for WatchGuard { fn drop(&mut self) { self.stop.cancel(); self.budget.cancel(); } }
    let _guard = WatchGuard { stop: stopped.clone(), budget: budget.clone() };
    let components=match mode {
        ExecutionMode::Production => {
    let factory = AttemptRunnerFactory::for_run(home.clone(), &config, budget.clone(), false).await
        .map_err(|e| e.to_string())?;
    let runner = Arc::from(factory.for_runtime(&spec.runtime).map_err(|e| e.to_string())?);
    let policy_home = home.clone();
    let policy_budget = budget.clone();
    let quota = super::super::attempt_container::QuotaLimits {
        max_run_bytes: config.max_run_bytes, max_total_bytes: config.max_total_bytes,
    };
    let policy_runtime = tokio::task::spawn_blocking(move ||
        PythonPolicyRuntime::detect_scoped_with_quota(&policy_home, &run_id, policy_budget, quota))
        .await.map_err(|_| "policy isolation detection failed")?;
    let policy = Arc::new(ManagedPolicySource::new(policy_runtime));
    if let Some(source) = frozen.defaults.bundle.source.as_deref() {
        if let Err(reason) = policy.install_deployment(source, frozen.defaults.bundle.beta,
            &frozen.defaults.bundle.origin_task_ids) {
            policy.degrade(reason);
            // A rejected deployment uses the documented baseline beta.
            spec.beta = 0.6;
        }
    }
    OnlineComponents { runner,
        evaluator: Arc::new(RegisteredEvaluator::new(home.clone(), config.clone(), false)),
        policy: policy.clone(), dreaming: Some(policy) }
        },
        #[cfg(test)]
        ExecutionMode::Injected(build) => build(budget.clone()),
    };
    let result = run_with_lease_and_identity(home, config, spec, components, budget,
        frozen.scorer_hash, lease, identity).await;
    stopped.cancel();
    let _ = watcher.await;
    result
}
