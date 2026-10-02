//! ACL-bound public views: no workspace paths, raw prompts, or source bodies.
use super::*;
use super::super::{artifact::load_verified_artifact, tree::{CostSource, cell_id}};
use std::collections::BTreeMap;
use std::io::Read;
use base64::{Engine, engine::general_purpose::STANDARD};
const MAX_DOWNLOAD: u64 = 16 * 1024 * 1024;
const MAX_QUERY_CELLS: usize = super::super::tree::MAX_PLANNED_CELLS as usize;
const TREE_QUERY_LIMIT_REASON: &str = "discovery tree exceeds the bounded query size";

pub fn catalog(home: &Path, caller: &TrustedCaller, agent: &str) -> Result<Value, String> {
    caller.authorize_create(home, agent)?;
    let config = load_config(home)?;
    let roots = config.approved_workspace_roots.iter().enumerate().map(|(index, root)| {
        let canonical = workspace::canonical_real_directory(root).map_err(|_| "approved root unavailable")?;
        Ok(json!({"id":root_id(&canonical),"label":format!("Workspace {}", index + 1)}))
    }).collect::<Result<Vec<_>, String>>()?;
    let evaluators = config.evaluators.iter().filter(|(_, settings)| settings.sandbox == EvaluatorSandbox::Container)
        .map(|(name, _)| json!({"name":name,"label":name})).collect::<Vec<_>>();
    let enabled = config.attempt.sandbox == AttemptSandbox::Container && !config.account_pool.is_empty();
    let runtimes = if enabled { config.attempt.runtimes.keys().filter_map(|name|
        super::super::agent_spawn::AttemptRunnerFactory::check_runtime_capability(&config.attempt,name).ok()
            .map(|family|family.name().to_string())).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>() } else { vec![] };
    Ok(json!({"roots":roots,"evaluators":evaluators,"runtimes":runtimes,
        "can_create":enabled && !roots.is_empty() && !evaluators.is_empty() && !runtimes.is_empty(),
        "requires_approval":!caller.manager_for(agent)}))
}
async fn authorized_task(home: &Path, store: &TaskStore, caller: &TrustedCaller, run_id: &str) -> Result<TaskRow, String> {
    valid_id(run_id)?;
    let task = store.discovery_by_run(run_id).await?.ok_or("discovery not found")?;
    caller.authorize_task(home,&task)?;
    Ok(task)
}
fn validate_binding(task: &TaskRow, run: &super::super::store::RunRecord) -> Result<(), String> {
    if run.task_id.as_deref() != Some(task.id.as_str()) || run.creator_id.as_deref() != Some(task.created_by.as_str())
        || run.agent_id != task.assigned_to || task.discovery_run_id.as_deref() != Some(run.run_id.as_str()) {
        return Err("discovery attribution mismatch".into());
    }
    Ok(())
}
const MAX_REPORT_BYTES: u64 = 256 * 1024;
/// The only fields public views read from a private run report. Every field
/// defaults, so older reports (without `stop_code`) still parse.
#[derive(Debug, Default, Deserialize)]
struct ReportOutcome {
    #[serde(default)] stop_reason: Option<String>,
    #[serde(default)] stop_code: Option<String>,
    #[serde(default)] isolation_warning: Option<String>,
}
/// Bounded no-follow read. Missing, oversized or unparseable reports yield
/// `None` and never hide the run.
fn report_outcome(home: &Path, run_id: &str) -> Option<ReportOutcome> {
    valid_id(run_id).ok()?;
    let file = open_regular(&home.join("discovery/reports").join(format!("{run_id}.json"))).ok()?;
    if file.metadata().ok()?.len() > MAX_REPORT_BYTES { return None; }
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_REPORT_BYTES { return None; }
    serde_json::from_slice(&bytes).ok()
}
/// Public approval state (`decided` = neutral legacy value when no receipt
/// explains it). `approved` needs a manager receipt; a broker row
/// alone is never authority, and no path here can report approval without one.
struct ApprovalView { status: &'static str, expires_at: Option<String> }
async fn approval_view(home: &Path, store: &TaskStore, broker: &mut Option<ApprovalBroker>,
    task: &TaskRow) -> Result<ApprovalView, String> {
    let Some(id) = task.discovery_approval_id.as_deref() else {
        return Ok(ApprovalView { status: "not_required", expires_at: None });
    };
    if task.status == "pending_approval" {
        // Only pending rows touch the broker, keeping `list` cheap.
        if broker.is_none() {
            *broker = ApprovalBroker::open(home).inspect_err(|error|
                tracing::warn!(error, "discovery approval expiry unavailable")).ok();
        }
        let record = match broker.as_ref() {
            Some(broker) => broker.get(&ApprovalId::from(id.to_owned())).await.unwrap_or(None),
            None => None,
        };
        let expires_at = record.and_then(|record| record.expires_at_epoch())
            .and_then(|epoch| chrono::DateTime::<chrono::Utc>::from_timestamp(epoch, 0))
            .map(|deadline| deadline.to_rfc3339());
        return Ok(ApprovalView { status: "pending", expires_at });
    }
    let reason = task.blocked_reason.as_deref().filter(|_| task.status == "cancelled");
    let status = match reason {
        Some(CANCEL_WITHDRAWN) => "withdrawn",
        Some(CANCEL_APPROVAL_EXPIRED) => "expired",
        Some(CANCEL_APPROVAL_DENIED | CANCEL_APPROVAL_LEGACY) => "denied",
        _ => match store.discovery_decision_receipt(id).await? {
            Some(receipt) if receipt.task_id == task.id && receipt.approve => "approved",
            Some(receipt) if receipt.task_id == task.id => "denied",
            // Cancelled before withdrawal reasons existed: still no authority.
            _ if reason == Some(CANCEL_BY_CALLER) => "withdrawn",
            // No receipt and no explaining cancel reason: neutral, never "approved".
            _ => "decided",
        },
    };
    Ok(ApprovalView { status, expires_at: None })
}
fn cancel_code(task: &TaskRow) -> Option<&'static str> {
    if task.status != "cancelled" { return None; }
    match task.blocked_reason.as_deref()? {
        CANCEL_APPROVAL_DENIED | CANCEL_APPROVAL_LEGACY => Some("approval_denied"),
        CANCEL_APPROVAL_EXPIRED => Some("approval_expired"),
        CANCEL_WITHDRAWN | CANCEL_BY_CALLER => Some("cancelled_by_user"),
        _ => None,
    }
}
fn data(home: &Path, task: &TaskRow, approval: &ApprovalView, include_tree: bool) -> Result<(Value, Vec<Value>, Vec<Value>), String> {
    let frozen = frozen_request(home,task)?;
    let run_id = task.discovery_run_id.as_deref().ok_or("missing run identity")?;
    let store = DiscoveryStore::open(home).map_err(|e| e.to_string())?;
    let run = store.load_run(run_id).map_err(|e| e.to_string())?;
    if let Some(run) = &run { validate_binding(task, run)?; }
    let worlds = store.list_run_worlds(run_id).map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    let mut rounds = Vec::new();
    let (mut subtotal, node_count) = store.run_node_subtotal(run_id).map_err(|e| e.to_string())?;
    let planned_count = worlds.iter().fold(0u64, |count, world| count.saturating_add(
        u64::from(world.branch_count) * (u64::from(world.refine_count) + 1)));
    let tree_available = planned_count <= MAX_QUERY_CELLS as u64 && node_count <= MAX_QUERY_CELLS;
    if include_tree && !tree_available { return Err(TREE_QUERY_LIMIT_REASON.into()); }
    for world in worlds.iter().filter(|_| include_tree) {
        let records = store.load_nodes(run_id, world.round).map_err(|e| e.to_string())?;
        if nodes.len().saturating_add(records.len()) > MAX_QUERY_CELLS { return Err(TREE_QUERY_LIMIT_REASON.into()); }
        let mut completion = Vec::new();
        for node in records {
            if node.evaluated { completion.push(node.cell_id.clone()); }
            nodes.push(json!({"cell_id":node.cell_id,"round":node.round,"branch":node.branch,
                "attempt":node.attempt,"parent_id":node.parent_id,
                "status":if node.valid {"valid"} else if node.evaluated {node.fail_class.as_str()} else {"pending"},
                "score":node.valid_score(),"model":node.model,"cost":node.cost}));
        }
        let grid = (0..world.branch_count).flat_map(|branch|
            (0..=world.refine_count).map(move |attempt| cell_id(world.round, branch, attempt))).collect::<Vec<_>>();
        rounds.push(json!({"round":world.round,"policy_id":world.policy_id,"beta":world.beta,
            "full_grid":grid,"completion":completion,"new_in":[],"new_in_available":false}));
    }
    // Nodes alone omit policy-development calls and infrastructure retries.
    // Until a host-written complete snapshot exists, expose an incomplete subtotal.
    subtotal.usd_source = CostSource::Pending;
    let mut accounting=json!({"reported_usd":null,"estimated_usd":null,"unknown_reserved_usd":null,
        "pending_reserved_usd":null,"unclassified_observed_usd":null,"budget_snapshot_available":false});
    if let Some(budget) = store.load_run_budget_snapshot(run_id).map_err(|_| "discovery accounting unavailable")? {
        subtotal.usd = budget.reported_usd + budget.estimated_usd;
        subtotal.wall_secs = budget.wall_secs;
        subtotal.unknown_calls = budget.unknown_calls;
        subtotal.usd_source = if budget.unclassified_observed_usd > 0.0 {CostSource::Unknown}
            else if budget.pending_calls > 0 { CostSource::Pending }
            else if budget.unknown_calls > 0 { CostSource::Unknown }
            else if budget.estimated_usd > 0.0 { CostSource::Estimated } else { CostSource::Reported };
        accounting=json!({"reported_usd":budget.reported_usd,"estimated_usd":budget.estimated_usd,
            "unknown_reserved_usd":budget.unknown_reserved_usd,"pending_reserved_usd":budget.pending_reserved_usd,
            "unclassified_observed_usd":budget.unclassified_observed_usd,"budget_snapshot_available":true});
    }
    let mut cost=json!(subtotal);
    if let (Some(cost),Some(accounting))=(cost.as_object_mut(),accounting.as_object()) {
        cost.extend(accounting.clone()); cost.insert("token_scope".into(),json!("evaluated_nodes"));
    }
    let run_status=run.as_ref().map(|run|run.status.as_str());
    // A crashed worker can leave the episode ledger running after its task
    // lease expires. The task's terminal authority must win that projection;
    // reading a public view never takes another owner's lease or sweeps it.
    let status = if task.status == "cancelled"
        || (matches!(task.status.as_str(),"done"|"failed")
            && run_status.is_some_and(|status|matches!(status,"running"|"queued"|"in_progress"|"pending_approval"))) {
        task.status.as_str()
    } else {run_status.unwrap_or(&task.status)};
    let verified = matches!(load_verified_artifact(home, run_id), Ok(Some(_)));
    let degraded = run.as_ref().is_some_and(|run| run.has_unconfined || run.status == "degraded");
    let report = run.as_ref().and_then(|_| report_outcome(home, run_id)).unwrap_or_default();
    let isolation_degraded = run.as_ref().is_some_and(|run| run.has_unconfined) || report.isolation_warning.is_some();
    // A cancelled task is explained by `cancel_code`; the run's own stop
    // (usually the cancelled budget) would only mislead.
    let stop_code = if task.status == "cancelled" { None } else {
        super::super::stop_code::normalize(report.stop_code.as_deref()).or_else(||
            super::super::stop_code::classify(run_status.unwrap_or(""), report.stop_reason.as_deref()))
    };
    Ok((json!({"run_id":run_id,"task_id":task.id,"title":task.title,"status":status,
        "approval_status":approval.status,"approval_expires_at":approval.expires_at,
        "current_round":worlds.last().map(|world|world.round).unwrap_or(0),
        "branch_count":frozen.spec.branch_count,"refine_count":frozen.spec.refine_count,
        "runtime":frozen.spec.runtime,"budget":frozen.spec.budget,"cost":cost,
        "tree_available":tree_available,
        "tree_unavailable_reason":if tree_available {None} else {Some(TREE_QUERY_LIMIT_REASON)},
        "can_cancel":matches!(task.status.as_str(),"pending_approval"|"queued"|"in_progress"),
        "artifact_verified":verified,"degraded":degraded,"isolation_degraded":isolation_degraded,
        "stop_code":stop_code,"cancel_code":cancel_code(task)}), nodes, rounds))
}
pub async fn list(home: &Path, caller: &TrustedCaller, agent: Option<&str>, limit: usize) -> Result<Value, String> {
    if !(1..=100).contains(&limit) { return Err("invalid discovery list limit".into()); }
    let store = TaskStore::open(home)?;
    if let Some(agent)=agent {valid_id(agent)?;}
    let (owned,managed,all)=match &caller.principal {
        Principal::User(user)=> {
            let owned=user.agent_access.keys().filter(|agent|user.has_agent_access(agent,AccessLevel::Operator)).cloned().collect::<Vec<_>>();
            let managed=if user.has_role(UserRole::Manager) {owned.clone()} else {vec![]};
            (owned,managed,user.is_admin())
        },
        Principal::Agent(_)=> {
            let owned=store.discovery_creator_assignees(&caller.id()).await?.into_iter()
                .filter(|agent|caller.authorize_create(home,agent).is_ok()).collect::<Vec<_>>();
            (owned,vec![],false)
        }
    };
    let mut runs = Vec::new();
    let mut broker = None;
    for task in store.discovery_visible_tasks(&caller.id(),&owned,&managed,all,agent,limit).await? {
        if caller.authorize_task(home,&task).is_err() { continue; }
        let approval = approval_view(home, &store, &mut broker, &task).await?;
        runs.push(data(home, &task, &approval, false)?.0);
        if runs.len() == limit { break; }
    }
    Ok(json!({"runs":runs}))
}
pub async fn tree(home: &Path, caller: &TrustedCaller, run: &str) -> Result<Value, String> {
    let store = TaskStore::open(home)?;
    let task = authorized_task(home,&store, caller, run).await?;
    let approval = approval_view(home, &store, &mut None, &task).await?;
    let (run, nodes, rounds) = data(home, &task, &approval, true)?;
    Ok(json!({"run":run,"nodes":nodes,"rounds":rounds}))
}
pub async fn cancel(home: &Path, caller: &TrustedCaller, run: &str) -> Result<Value, String> {
    let store = TaskStore::open(home)?;
    let task = authorized_task(home,&store, caller, run).await?;
    // Withdrawing a pending request uses its own CAS so a concurrent manager
    // approval is never relabelled as a withdrawal.
    let withdrawn = task.status == "pending_approval" && store.cancel_pending_discovery(&task.id, CANCEL_WITHDRAWN).await?;
    if !withdrawn && !store.cancel_discovery(&task.id, CANCEL_BY_CALLER).await? {
        return Err("discovery is already terminal".into());
    }
    if let (true, Some(id)) = (withdrawn, task.discovery_approval_id.as_deref()) {
        // The committed task state stays authoritative; `poll` retries this.
        match ApprovalBroker::open(home) {
            Ok(broker) => withdraw_approval(&broker, id, &task.id).await,
            Err(error) => tracing::warn!(error, "discovery approval withdrawal deferred"),
        }
    }
    Ok(json!({"status":"cancelled"}))
}

fn verified_manifest(artifact: &super::super::artifact::VerifiedArtifact) -> Result<BTreeMap<PathBuf, String>, String> {
    let manifest = workspace::manifest(&artifact.root).map_err(|_| "artifact verification failed")?;
    let mut digest = Sha256::new();
    for (path, hash) in &manifest {
        let name = path.as_os_str().as_encoded_bytes();
        digest.update((name.len() as u64).to_be_bytes()); digest.update(name); digest.update(hash.as_bytes());
    }
    if format!("{:x}", digest.finalize()) != artifact.expected_sha256 { return Err("verified artifact changed".into()); }
    Ok(manifest)
}
fn file_id(path: &Path) -> String { format!("file-{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes())) }
fn safe_name(path: &Path) -> Result<String, String> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or("invalid artifact filename")?;
    if name.is_empty() || name.len() > 255 || name.chars().any(|character| character.is_control() || matches!(character,'/'|'\\')) {
        return Err("invalid artifact filename".into());
    }
    Ok(name.into())
}
fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new(); options.read(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)] {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|_| "artifact file unavailable")?;
    let meta = file.metadata().map_err(|_| "artifact file unavailable")?;
    if !meta.is_file() { return Err("artifact must be a regular file".into()); }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 { return Err("artifact hardlink rejected".into()); }
    }
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 { return Err("artifact reparse point rejected".into()); }
    }
    Ok(file)
}
pub async fn artifact(home: &Path, caller: &TrustedCaller, run: &str, selected: Option<&str>) -> Result<Value, String> {
    let task = authorized_task(home,&TaskStore::open(home)?, caller, run).await?;
    if let Some(record) = DiscoveryStore::open(home).map_err(|_|"discovery unavailable")?
        .load_run(run).map_err(|_|"discovery unavailable")? {
        validate_binding(&task, &record)?;
    }
    let artifact = load_verified_artifact(home, run)?.ok_or("verified artifact is not available")?;
    let manifest = verified_manifest(&artifact)?;
    let entries = manifest.into_iter().filter(|(_, hash)| hash != "directory").collect::<Vec<_>>();
    if entries.len() > 4096 { return Err("artifact manifest exceeds bounded size".into()); }
    if let Some(selected) = selected {
        let (path, expected) = entries.iter().find(|(path,_)| file_id(path) == selected).ok_or("artifact file not found")?;
        let name = safe_name(path)?;
        let file = open_regular(&artifact.root.join(path))?;
        if file.metadata().map_err(|_|"artifact file unavailable")?.len() > MAX_DOWNLOAD { return Err("artifact exceeds 16 MiB download limit".into()); }
        let mut bytes = Vec::new();
        file.take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).map_err(|_|"artifact read failed")?;
        if bytes.len() as u64 > MAX_DOWNLOAD || format!("{:x}",Sha256::digest(&bytes)) != *expected {
            return Err("verified artifact changed".into());
        }
        artifact.verify().map_err(|_|"verified artifact changed")?;
        Ok(json!({"run_id":run,"cell_id":artifact.cell_id,"file_id":selected,
            "name":name,"size_bytes":bytes.len(),"content_base64":STANDARD.encode(bytes)}))
    } else {
        let files = entries.iter().map(|(path,_)| {
            let file = open_regular(&artifact.root.join(path))?;
            Ok(json!({"file_id":file_id(path),"name":safe_name(path)?,
                "size_bytes":file.metadata().map_err(|_|"artifact file unavailable")?.len()}))
        }).collect::<Result<Vec<_>,String>>()?;
        artifact.verify().map_err(|_|"verified artifact changed")?;
        Ok(json!({"run_id":run,"cell_id":artifact.cell_id,"verified":true,"files":files}))
    }
}
