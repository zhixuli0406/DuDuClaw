//! Host-owned fork/run associations. Legacy fork rows are never imported as authority.
//! Host controls are installed separately; the model-branch producer remains gated.
use std::{collections::BTreeMap, path::Path, sync::{Arc, Mutex}};
use duduclaw_isolation::execution::{ExecutionBackend, ExecutionClass, RunHandle, RunOutcome, SpawnPolicy, WorkspacePolicy};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::{RequestContext, VerificationError, resources::AuthorizedHelperWorkspace};

const MAX_FILES: usize = 1000;
const MAX_MANIFEST: usize = 256 * 1024;

/// Fixed file digests, produced by a bounded snapshot worker, never MCP input.
/// A publication switches a host manifest pointer; it does not overwrite files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Manifest { files: BTreeMap<String, String> }
impl Manifest {
    pub fn new(files: BTreeMap<String, String>) -> Result<Self, VerificationError> {
        if files.len() > MAX_FILES { return Err(VerificationError::Invalid); }
        for (path, digest) in &files {
            crate::resources::relative_path(path)?;
            if !hex(digest, 64) { return Err(VerificationError::Invalid); }
        }
        let value = Self { files };
        value.encode()?;
        Ok(value)
    }
    fn encode(&self) -> Result<String, VerificationError> {
        let text = serde_json::to_string(self).map_err(|_| VerificationError::Invalid)?;
        if text.len() > MAX_MANIFEST { return Err(VerificationError::Invalid); }
        Ok(text)
    }
    fn decode(text: &str) -> Result<Self, VerificationError> {
        if text.len() > MAX_MANIFEST { return Err(VerificationError::Invalid); }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input { files: BTreeMap<String, String> }
        let value: Input = serde_json::from_str(text).map_err(|_| VerificationError::Invalid)?;
        Self::new(value.files)
    }
    pub fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(self.encode().expect("validated manifest")))
    }
    pub fn diff(&self, other: &Self) -> Vec<String> {
        let mut paths: std::collections::BTreeSet<_> = self.files.keys().chain(other.files.keys()).cloned().collect();
        paths.retain(|path| self.files.get(path) != other.files.get(path));
        paths.into_iter().collect()
    }
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn identity(context: &RequestContext) -> Result<String, VerificationError> {
    serde_json::to_string(&(context.record().employee, &context.record().principal,
        &context.record().employee_binding)).map_err(|_| VerificationError::Invalid)
}
fn require_workspace(context: &RequestContext, workspace: &str) -> Result<(), VerificationError> {
    if context.record().resources.as_slice().iter().any(|grant| matches!(grant,
        duduclaw_isolation::capability::ResourceGrant::Workspace { handle } if handle.as_str() == workspace)) {
        Ok(())
    } else { Err(VerificationError::Invalid) }
}
fn unavailable(_: rusqlite::Error) -> VerificationError { VerificationError::Unavailable }
fn key(id: &str) -> Result<(), VerificationError> {
    if hex(id, 32) { Ok(()) } else { Err(VerificationError::Invalid) }
}
fn fresh() -> String {
    use rand::RngCore;
    let mut bytes = [0; 16]; rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub id: String,
    pub workspace: WorkspacePolicy,
    pub run: Option<RunHandle>,
    pub state: String,
}

/// Open once at host boot. Reopening never certifies retirement or resumes work.
pub struct ForkStore { connection: Mutex<Connection> }
impl ForkStore {
    pub fn open(path: &Path) -> Result<Arc<Self>, VerificationError> {
        let connection = Connection::open(path).map_err(unavailable)?;
        connection.busy_timeout(std::time::Duration::from_secs(2)).map_err(unavailable)?;
        connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS protected_forks (
            id TEXT PRIMARY KEY, actor TEXT NOT NULL, parent_workspace TEXT NOT NULL,
            baseline TEXT NOT NULL, publication TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0,
            selected TEXT);
          CREATE TABLE IF NOT EXISTS protected_fork_parents (
            employee BLOB NOT NULL, workspace TEXT NOT NULL, publication TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(employee,workspace));
          CREATE TABLE IF NOT EXISTS protected_fork_branches (
            id TEXT PRIMARY KEY, fork TEXT NOT NULL REFERENCES protected_forks(id),
            workspace TEXT NOT NULL UNIQUE, cwd TEXT NOT NULL, run TEXT UNIQUE,
            state TEXT NOT NULL, manifest TEXT);
          CREATE INDEX IF NOT EXISTS protected_fork_by_parent ON protected_fork_branches(fork);
          UPDATE protected_fork_branches SET state='uncertain'
            WHERE state IN ('queued','reserved','running','cancel_requested');").map_err(unavailable)?;
        Ok(Arc::new(Self { connection: Mutex::new(connection) }))
    }

    /// Host snapshot provisioning calls this only after creating distinct roots.
    /// Do not register old name-owned/path-owned forks here.
    pub fn create(&self, context: &RequestContext, parent: &str, baseline: &Manifest,
        workspaces: &[WorkspacePolicy]) -> Result<(String, Vec<String>), VerificationError> {
        if !(2..=4).contains(&workspaces.len()) { return Err(VerificationError::Invalid); }
        let granted = |handle: &str| context.record().resources.as_slice().iter().any(|grant|
            matches!(grant, duduclaw_isolation::capability::ResourceGrant::Workspace { handle: h } if h.as_str() == handle));
        if !granted(parent) { return Err(VerificationError::Invalid); }
        let mut seen = std::collections::BTreeSet::new();
        for workspace in workspaces {
            if workspace.workspace.as_str() == parent || !granted(workspace.workspace.as_str())
                || !seen.insert(workspace.workspace.as_str()) { return Err(VerificationError::Invalid); }
            if !matches!(workspace.relative_cwd.as_str(), "" | ".") {
                crate::resources::relative_path(workspace.relative_cwd.as_str())?;
            }
        }
        let actor = identity(context)?;
        let manifest = baseline.encode()?;
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let count: u64 = transaction.query_row("SELECT count(*) FROM protected_forks", [], |r| r.get(0)).map_err(unavailable)?;
        if count >= 1024 { return Err(VerificationError::Unavailable); }
        transaction.execute("INSERT OR IGNORE INTO protected_fork_parents(employee,workspace,publication) VALUES(?1,?2,?3)",
            params![context.record().employee.0.as_slice(), parent, manifest]).map_err(unavailable)?;
        let (revision, current): (u64, String) = transaction.query_row(
            "SELECT revision,publication FROM protected_fork_parents WHERE employee=?1 AND workspace=?2",
            params![context.record().employee.0.as_slice(),parent], |r| Ok((r.get(0)?,r.get(1)?))).map_err(unavailable)?;
        if current != manifest { return Err(VerificationError::BindingMismatch); }
        let fork = fresh();
        transaction.execute("INSERT INTO protected_forks(id,actor,parent_workspace,baseline,publication,revision) VALUES(?1,?2,?3,?4,?4,?5)",
            params![fork, actor, parent, manifest, revision]).map_err(unavailable)?;
        let mut branches = vec![];
        for workspace in workspaces {
            let id = fresh();
            transaction.execute("INSERT INTO protected_fork_branches(id,fork,workspace,cwd,state) VALUES(?1,?2,?3,?4,'pending')",
                params![id, fork, workspace.workspace.as_str(), workspace.relative_cwd.as_str()]).map_err(unavailable)?;
            branches.push(id);
        }
        transaction.commit().map_err(unavailable)?;
        Ok((fork, branches))
    }

    pub fn branch(&self, context: &RequestContext, fork: &str, branch: &str) -> Result<Branch, VerificationError> {
        key(fork)?; key(branch)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let (workspace, cwd, run, state): (String, String, Option<String>, String) = connection.query_row(
            "SELECT b.workspace,b.cwd,b.run,b.state FROM protected_fork_branches b
             JOIN protected_forks f ON f.id=b.fork WHERE f.id=?1 AND b.id=?2 AND f.actor=?3",
            params![fork, branch, identity(context)?], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))
            .optional().map_err(unavailable)?.ok_or(VerificationError::Invalid)?;
        require_workspace(context, &workspace)?;
        Ok(Branch { id: branch.into(), workspace: WorkspacePolicy {
            workspace: duduclaw_isolation::bounded::Text::new(workspace).map_err(|_| VerificationError::Invalid)?,
            relative_cwd: duduclaw_isolation::bounded::Text::new(cwd).map_err(|_| VerificationError::Invalid)? },
            run: run.map(|run| crate::execution_jobs::parse_handle(&run)).transpose()?, state })
    }

    pub(crate) fn queue(&self, context:&RequestContext, fork:&str, branches:&[String]) -> Result<(),VerificationError> {
        if !(2..=4).contains(&branches.len()) { return Err(VerificationError::Invalid); }
        for branch in branches { self.branch(context,fork,branch)?; }
        let mut connection=self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction=connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        for branch in branches {
            if transaction.execute("UPDATE protected_fork_branches SET state='queued' WHERE id=?1 AND fork=?2 AND state='pending' AND run IS NULL",params![branch,fork]).map_err(unavailable)? != 1 {
                return Err(VerificationError::Invalid);
            }
        }
        transaction.commit().map_err(unavailable)
    }
    pub(crate) fn uncertain(&self, fork:&str, branch:&str) -> Result<(),VerificationError> {
        let connection=self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        connection.execute("UPDATE protected_fork_branches SET state='uncertain',manifest=NULL WHERE fork=?1 AND id=?2 AND state IN ('queued','reserved','running','cancel_requested')",params![fork,branch]).map_err(unavailable)?;
        Ok(())
    }

    /// Exact actor and every returned branch grant are checked before disclosure.
    pub fn inspect(&self, context: &RequestContext, fork: &str) -> Result<serde_json::Value, VerificationError> {
        key(fork)?;
        let (revision,parent_digest,parent) = {
            let connection=self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
            let (revision,manifest,parent):(u64,String,String)=connection.query_row("SELECT f.revision,f.baseline,f.parent_workspace FROM protected_forks f WHERE f.id=?1 AND f.actor=?2",params![fork,identity(context)?],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(unavailable)?.ok_or(VerificationError::Invalid)?;
            (revision,Manifest::decode(&manifest)?.digest(),parent)
        };
        require_workspace(context,&parent)?;
        let ids = {
            let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
            let mut query = connection.prepare("SELECT b.id FROM protected_fork_branches b JOIN protected_forks f ON f.id=b.fork WHERE f.id=?1 AND f.actor=?2 ORDER BY b.id").map_err(unavailable)?;
            query.query_map(params![fork,identity(context)?], |r| r.get::<_,String>(0)).map_err(unavailable)?
                .collect::<Result<Vec<_>,_>>().map_err(unavailable)?
        };
        if ids.is_empty() { return Err(VerificationError::Invalid); }
        let mut branches = vec![];
        for id in ids {
            let branch = self.branch(context,fork,&id)?;
            branches.push(serde_json::json!({"branch_id":branch.id,"workspace":branch.workspace.workspace.as_str(),
                "state":branch.state,"run":branch.run.map(crate::execution_jobs::handle_text)}));
        }
        Ok(serde_json::json!({"fork_id":fork,"branches":branches,"expected_revision":revision,
            "expected_parent_digest":parent_digest,"parent_workspace":parent}))
    }

    /// Persist the reservation before StartRun. Cancellation CAS always sees it.
    pub fn bind(&self, context: &RequestContext, fork: &str, branch: &str,
        run: RunHandle, workspace: &WorkspacePolicy) -> Result<(), VerificationError> {
        let owned = self.branch(context, fork, branch)?;
        if run.0 == context.record().run || owned.workspace != *workspace { return Err(VerificationError::BindingMismatch); }
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        if connection.execute("UPDATE protected_fork_branches SET run=?2,state='reserved'
            WHERE id=?1 AND state IN ('pending','queued') AND run IS NULL", params![branch, crate::execution_jobs::handle_text(run)])
            .map_err(unavailable)? != 1 { return Err(VerificationError::Invalid); }
        Ok(())
    }

    pub fn started(&self, run: RunHandle) -> Result<(), VerificationError> {
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        if connection.execute("UPDATE protected_fork_branches SET state='running' WHERE run=?1 AND state='reserved'",
            [crate::execution_jobs::handle_text(run)]).map_err(unavailable)? != 1 { return Err(VerificationError::Invalid); }
        Ok(())
    }

    /// Called only after root terminal observation and worker snapshot readback.
    /// A cancel request fences publication even if a late Exited(0) arrives.
    pub fn finish(&self, run: RunHandle, outcome: &RunOutcome, manifest: Option<&Manifest>) -> Result<(), VerificationError> {
        let successful = *outcome == RunOutcome::Exited(0) && manifest.is_some();
        let manifest = manifest.map(Manifest::encode).transpose()?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        if connection.execute("UPDATE protected_fork_branches
            SET manifest=CASE WHEN state='cancel_requested' THEN NULL ELSE ?2 END,
                state=CASE WHEN state='cancel_requested' THEN 'cancelled' ELSE ?3 END
            WHERE run=?1 AND state IN ('reserved','running','cancel_requested')",
            params![crate::execution_jobs::handle_text(run), if successful { manifest } else { None },
                if successful { "complete" } else { "failed" }]).map_err(unavailable)? != 1 {
            return Err(VerificationError::Invalid);
        }
        Ok(())
    }

    fn request_cancel(&self, context: &RequestContext, fork: &str, branch: &str) -> Result<Option<RunHandle>, VerificationError> {
        key(fork)?; key(branch)?;
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        // Read the run and change state under the same writer transaction. A
        // pending->reserved race must not make cancellation lose its RunHandle.
        let (run, state, workspace): (Option<String>, String, String) = transaction.query_row(
            "SELECT b.run,b.state,b.workspace FROM protected_fork_branches b JOIN protected_forks f ON f.id=b.fork
             WHERE f.id=?1 AND b.id=?2 AND f.actor=?3", params![fork,branch,identity(context)?],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(unavailable)?.ok_or(VerificationError::Invalid)?;
        require_workspace(context, &workspace)?;
        let result = if matches!(state.as_str(), "pending" | "queued" | "reserved" | "running" | "cancel_requested") {
            transaction.execute("UPDATE protected_fork_branches SET state=CASE WHEN run IS NULL THEN 'cancelled' ELSE 'cancel_requested' END,
                manifest=NULL WHERE id=?1", [branch]).map_err(unavailable)?;
            run.map(|value| crate::execution_jobs::parse_handle(&value)).transpose()?
        } else if state == "cancelled" { None } else { return Err(VerificationError::Invalid); };
        transaction.commit().map_err(unavailable)?;
        Ok(result)
    }

    pub fn diff(&self, context: &RequestContext, fork: &str, left: &str, right: &str) -> Result<Vec<String>, VerificationError> {
        self.branch(context, fork, left)?; self.branch(context, fork, right)?;
        let connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let load = |id| -> Result<Manifest, VerificationError> {
            let text: String = connection.query_row("SELECT manifest FROM protected_fork_branches WHERE id=?1 AND state='complete'",
                [id], |r| r.get(0)).map_err(unavailable)?;
            Manifest::decode(&text)
        };
        Ok(load(left)?.diff(&load(right)?))
    }

    /// Atomic host publication pointer CAS, not a pathname-copy promotion. The
    /// materializer must consume this exact immutable manifest, never a live tree.
    pub fn select(&self, context: &RequestContext, fork: &str, branch: &str,
        expected_revision: u64, expected_parent: &str) -> Result<(u64, Manifest), VerificationError> {
        key(fork)?; key(branch)?;
        if !hex(expected_parent, 64) { return Err(VerificationError::Invalid); }
        let mut connection = self.connection.lock().map_err(|_| VerificationError::Unavailable)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
        let (revision, publication, selected, parent, baseline): (u64, String, Option<String>, String, String) = transaction.query_row(
            "SELECT p.revision,p.publication,f.selected,f.parent_workspace,f.baseline
             FROM protected_forks f JOIN protected_fork_parents p ON p.workspace=f.parent_workspace AND p.employee=?3
             WHERE f.id=?1 AND f.actor=?2 AND f.revision=p.revision",
            params![fork, identity(context)?,context.record().employee.0.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .optional().map_err(unavailable)?.ok_or(VerificationError::Invalid)?;
        if revision != expected_revision || selected.is_some() || publication != baseline
            || Manifest::decode(&publication)?.digest() != expected_parent {
            return Err(VerificationError::BindingMismatch);
        }
        require_workspace(context, &parent)?;
        let (text, workspace): (String, String) = transaction.query_row("SELECT manifest,workspace FROM protected_fork_branches
            WHERE id=?1 AND fork=?2 AND state='complete' AND run IS NOT NULL", params![branch, fork], |r| Ok((r.get(0)?,r.get(1)?)))
            .optional().map_err(unavailable)?.ok_or(VerificationError::Invalid)?;
        require_workspace(context, &workspace)?;
        let manifest = Manifest::decode(&text)?;
        let next = revision.checked_add(1).ok_or(VerificationError::Invalid)?;
        if transaction.execute("UPDATE protected_fork_parents SET publication=?3,revision=?4
            WHERE employee=?1 AND workspace=?2 AND revision=?5 AND publication=?6",
            params![context.record().employee.0.as_slice(),parent,text,next,revision,publication]).map_err(unavailable)? != 1 {
            return Err(VerificationError::BindingMismatch);
        }
        if transaction.execute("UPDATE protected_forks SET publication=?2,selected=?3,revision=?4
            WHERE id=?1 AND revision=?5 AND selected IS NULL", params![fork,text,branch,next,revision]).map_err(unavailable)? != 1 {
            return Err(VerificationError::BindingMismatch);
        }
        transaction.commit().map_err(unavailable)?;
        Ok((next, manifest))
    }
}

/// Host-owned fork service; the CLI retains original handler policy gates.
/// Implementations advertise only producers that are installed and usable.
pub trait ForkToolService: Send + Sync {
    fn supports(&self, name: &str) -> bool;
    fn execute<'a>(&'a self, context: &'a RequestContext, name: &'a str,
        arguments: &'a serde_json::Value) -> crate::execution::PolicyFuture<'a, serde_json::Value>;
}

/// Trusted factory must authorize current handler policy before reservation and
/// recheck it while preparing. It must prove root delivery of the exact branch
/// workspace, not merely assign an opaque handle to SpawnPolicy.
pub trait ForkPlanFactory: Send + Sync {
    fn authorize<'a>(&'a self, context: &'a RequestContext, branch: &'a Branch)
        -> crate::execution::PolicyFuture<'a, ()>;
    fn prepare<'a>(&'a self, context: &'a RequestContext, branch: &'a Branch, run: RunHandle)
        -> crate::execution::PolicyFuture<'a, (SpawnPolicy, AuthorizedHelperWorkspace)>;
}

/// The caller must obtain a root workspace-delivery implementation before using
/// this driver. An AuthorizedHelperWorkspace proves host access, not root mount delivery.
/// The derived delivery proof pins the exact authorized H directory for root.
pub async fn launch_branch(store: Arc<ForkStore>, backend: Arc<dyn ExecutionBackend>,
    factory: &dyn ForkPlanFactory, context: &RequestContext, fork: &str, branch: &str) -> Result<RunHandle, VerificationError> {
    let (checking, ctx, fork_id, branch_id) = (store.clone(),context.clone(),fork.to_owned(),branch.to_owned());
    let owned = crate::authority::verify_blocking(move || checking.branch(&ctx,&fork_id,&branch_id)).await?;
    if !matches!(owned.state.as_str(), "pending" | "queued") { return Err(VerificationError::Invalid); }
    factory.authorize(context, &owned).await?;
    let run = backend.reserve(context.record().employee).await.map_err(|_| VerificationError::Unavailable)?;
    if run.0 == context.record().run { return Err(VerificationError::BindingMismatch); }
    // Only a reservation made here can ever reach cancellation, including if
    // a malicious/broken factory returns a different run in its plan.
    let mut cleanup = LaunchGuard { backend: backend.clone(), run, armed: true };
    let (mut plan, workspace) = factory.prepare(context, &owned, run).await?;
    let checks = plan.run == run && plan.workspace == owned.workspace
        && plan.class == ExecutionClass::Fork && plan.employee == context.record().employee
        && !plan.pty && workspace.permits(context, &plan.workspace);
    if !checks {
        return Err(VerificationError::BindingMismatch);
    }
    plan.workspace_delivery = Some(workspace.delivery_for(run)?);
    let (binding,ctx,fork_id,branch_id,policy) = (store.clone(),context.clone(),fork.to_owned(),branch.to_owned(),plan.workspace.clone());
    crate::authority::verify_blocking(move || binding.bind(&ctx,&fork_id,&branch_id,run,&policy)).await?;
    match backend.spawn(plan).await {
        Ok(actual) if actual == run => {
            let (checking,ctx,fork_id,branch_id)=(store.clone(),context.clone(),fork.to_owned(),branch.to_owned());
            let accepted=crate::authority::verify_blocking(move || {
                let bound=checking.branch(&ctx,&fork_id,&branch_id)?;
                if bound.state=="reserved" && bound.run==Some(run) { Ok(()) }
                else { Err(VerificationError::Invalid) }
            }).await;
            if accepted.is_ok() {
                // Accepted(Queued) is not dispatched. Only the collector's
                // successful TakeIo observation may transition to running.
                cleanup.armed = false; Ok(run)
            } else { Err(VerificationError::Unavailable) }
        }
        _ => {
            // May have started; kill acknowledgement never becomes completion.
            let (context,fork,branch) = (context.clone(),fork.to_owned(),branch.to_owned());
            let _ = crate::authority::verify_blocking(move || store.request_cancel(&context,&fork,&branch)).await;
            let _ = backend.kill(run).await;
            Err(VerificationError::Unavailable)
        }
    }
}

pub async fn terminate(store: Arc<ForkStore>, backend: Arc<dyn ExecutionBackend>,
    context: &RequestContext, fork: &str, branch: &str) -> Result<&'static str, VerificationError> {
    let (context,fork,branch,guard_backend) = (context.clone(),fork.to_owned(),branch.to_owned(),backend.clone());
    let cancellation = crate::authority::verify_blocking(move || {
        let run = store.request_cancel(&context,&fork,&branch)?;
        if run.is_some_and(|run| run.0 == context.record().run) { return Err(VerificationError::Invalid); }
        Ok(run.map(|run| LaunchGuard { backend:guard_backend,run,armed:true }))
    }).await?;
    match cancellation {
        Some(mut cleanup) => {
            backend.kill(cleanup.run).await.map_err(|_| VerificationError::Unavailable)?;
            cleanup.armed = false;
            Ok("cancel_requested")
        }
        None => Ok("cancelled"),
    }
}
struct LaunchGuard { backend: Arc<dyn ExecutionBackend>, run: RunHandle, armed: bool }
impl Drop for LaunchGuard {
    fn drop(&mut self) {
        if self.armed && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let (backend, run) = (self.backend.clone(), self.run);
            runtime.spawn(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), backend.kill(run)).await;
            });
        }
    }
}

/// Authenticated host controls. The surrounding dispatcher must retain its
/// original fork-enabled/scope/approval and fresh principal/resource checks.
/// v1.80 exposes only reads. The mutation implementation below is retained for
/// the future producer, behind the same unconditional protected-tool refusal.
pub struct ForkHostControl {
    pub store: Arc<ForkStore>,
    pub backend: Arc<dyn ExecutionBackend>,
}
impl ForkHostControl {
    pub async fn execute(&self, context: &RequestContext, name: &str, arguments: &serde_json::Value)
        -> Result<serde_json::Value, VerificationError> {
        use serde_json::json;
        if crate::fork_policy::deferred(name) { return Err(VerificationError::Unavailable); }
        if !crate::tool_policy::eligible(context, name) { return Err(VerificationError::Invalid); }
        crate::check_arguments(arguments)?;
        let fields = arguments.as_object().ok_or(VerificationError::Invalid)?;
        let allowed: &[&str] = match name {
            "inspect_branches" | "fork_cost" => &["fork_id"],
            "diff_branches" => &["fork_id", "left_branch_id", "right_branch_id"],
            "merge_or_select" => &["fork_id", "branch_id", "expected_revision", "expected_parent_digest"],
            "terminate_branch" => &["fork_id", "branch_id"],
            _ => return Err(VerificationError::Invalid),
        };
        if fields.len() != allowed.len() || fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(VerificationError::Invalid);
        }
        let string = |key| arguments.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
            .ok_or(VerificationError::Invalid);
        let fork = string("fork_id")?;
        let store = self.store.clone();
        let context = context.clone();
        match name {
            "inspect_branches" => crate::authority::verify_blocking(move || store.inspect(&context,&fork)).await,
            "fork_cost" => crate::authority::verify_blocking(move || {
                // Existing partial records have no measured usage. Never label
                // absent telemetry as zero spend, and authorize before disclosure.
                let inspected = store.inspect(&context, &fork)?;
                let branches = inspected["branches"].as_array().ok_or(VerificationError::Invalid)?
                    .iter().map(|branch| json!({"branch_id":branch["branch_id"],
                        "state":branch["state"],"spent_usd":null})).collect::<Vec<_>>();
                Ok(json!({"fork_id":fork,"measurement_status":"unavailable",
                    "aggregate_spent_usd":null,"branches":branches}))
            }).await,
            "diff_branches" => {
                let (left, right) = (string("left_branch_id")?, string("right_branch_id")?);
                crate::authority::verify_blocking(move || {
                    Ok(json!({"fork_id":fork,"changed_paths":store.diff(&context,&fork,&left,&right)?,
                        "comparison":"recorded_manifest","live_content_verified":false}))
                }).await
            }
            "merge_or_select" => {
                let branch = string("branch_id")?;
                let revision = arguments["expected_revision"].as_u64().ok_or(VerificationError::Invalid)?;
                let parent = string("expected_parent_digest")?;
                crate::authority::verify_blocking(move || {
                    let (revision, manifest) = store.select(&context,&fork,&branch,revision,&parent)?;
                    Ok(json!({"fork_id":fork,"branch_id":branch,"revision":revision,"manifest":manifest.digest(),
                        "status":"manifest_selected","workspace_materialized":false}))
                }).await
            }
            "terminate_branch" => {
                let branch = string("branch_id")?;
                let backend = self.backend.clone();
                // SQLite work stays on the blocking pool. The transaction
                // persists intent before async cancellation can be interrupted.
                let cancellation = crate::authority::verify_blocking(move || {
                    let run = store.request_cancel(&context,&fork,&branch)?;
                    if run.is_some_and(|run| run.0 == context.record().run) { return Err(VerificationError::Invalid); }
                    // If the join result is dropped, this lease still cancels.
                    Ok(run.map(|run| LaunchGuard { backend,run,armed:true }))
                }).await?;
                match cancellation {
                    Some(mut cleanup) => {
                        self.backend.kill(cleanup.run).await.map_err(|_| VerificationError::Unavailable)?;
                        cleanup.armed = false;
                        Ok(json!({"status":"cancel_requested"}))
                    }
                    None => Ok(json!({"status":"cancelled"})),
                }
            }
            _ => Err(VerificationError::Invalid),
        }
    }
}

#[cfg(test)]
#[path = "forks_tests.rs"]
mod tests;

impl ForkToolService for ForkHostControl {
    fn supports(&self, name: &str) -> bool {
        crate::fork_policy::READ_TOOLS.contains(&name)
    }
    fn execute<'a>(&'a self, context: &'a RequestContext, name: &'a str,
        arguments: &'a serde_json::Value) -> crate::execution::PolicyFuture<'a,serde_json::Value> {
        Box::pin(async move { ForkHostControl::execute(self,context,name,arguments).await })
    }
}
