use super::*;
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
    EmployeeBinding, Lane, Principal, ResourceGrant}, execution::*, ids::{EmployeeId, RunId, Sha256Digest}};
use std::sync::atomic::{AtomicUsize, Ordering};

fn context() -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]), task: None,
        principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(["fork_run", "inspect_branches", "diff_branches", "merge_or_select", "terminate_branch", "fork_cost"].into_iter()
            .map(|name| Text::new(name.into()).unwrap()).collect()).unwrap(),
        resources: List::new(std::iter::once(ResourceGrant::Employee { id: EmployeeId([2;16]) })
            .chain(["parent", "branch-a", "branch-b", "branch-c", "branch-d"].into_iter().map(|handle|
                ResourceGrant::Workspace { handle: Text::new(handle.into()).unwrap() })).collect()).unwrap(),
        policy_revision: 1, gateway_epoch: [4; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [5; 32], jti: [6; 16],
    })
}
fn workspace(name: &str) -> WorkspacePolicy {
    WorkspacePolicy { workspace: Text::new(name.into()).unwrap(), relative_cwd: Text::new(".".into()).unwrap() }
}
fn manifest(byte: char) -> Manifest {
    Manifest::new(BTreeMap::from([("中文.txt".into(), byte.to_string().repeat(64))])).unwrap()
}
fn run(byte: u8) -> RunHandle { RunHandle(RunId([byte; 16])) }
fn create(store: &ForkStore) -> (String, Vec<String>) {
    store.create(&context(), "parent", &manifest('a'), &[workspace("branch-a"), workspace("branch-b")]).unwrap()
}
fn complete(store: &ForkStore, fork: &str, branch: &str, handle: RunHandle, ws: &str, digest: char) {
    store.bind(&context(), fork, branch, handle, &workspace(ws)).unwrap();
    store.started(handle).unwrap();
    store.finish(handle, &RunOutcome::Exited(0), Some(&manifest(digest))).unwrap();
}

#[test]
fn fork_manifest_bounds_paths_and_deterministic_diff() {
    assert_eq!(manifest('a').digest(), manifest('a').digest());
    assert_eq!(manifest('a').diff(&manifest('b')), vec!["中文.txt"]);
    for path in ["../secret", "/etc/passwd", "a//b", "a/./b", "a\\b"] {
        assert!(Manifest::new(BTreeMap::from([(path.into(), "a".repeat(64))])).is_err());
    }
    for hash in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
        assert!(Manifest::new(BTreeMap::from([("file".into(), hash)])).is_err());
    }
    assert!(Manifest::new((0..=MAX_FILES).map(|n| (format!("file{n}"), "a".repeat(64))).collect()).is_err());
    assert!(Manifest::decode(r#"{"files":{},"root":"/"}"#).is_err());
}

#[test]
fn fork_binding_requires_distinct_granted_workspaces_and_exact_actor() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    for workspaces in [vec![workspace("branch-a")], vec![workspace("parent"),workspace("branch-a")],
        vec![workspace("branch-a"),workspace("branch-a")], vec![workspace("unknown"),workspace("branch-b")]] {
        assert!(store.create(&context(), "parent", &manifest('a'), &workspaces).is_err());
    }
    let (fork, branches) = create(&store);
    assert!(store.bind(&context(), &fork, &branches[0], run(3), &workspace("branch-a")).is_err());
    assert!(store.bind(&context(), &fork, &branches[0], run(10), &workspace("branch-b")).is_err());
    store.bind(&context(), &fork, &branches[0], run(10), &workspace("branch-a")).unwrap();
    assert!(store.bind(&context(), &fork, &branches[0], run(11), &workspace("branch-a")).is_err());
    assert!(store.bind(&context(), &fork, &branches[1], run(10), &workspace("branch-b")).is_err());
    let mut record = context().record().clone();
    record.principal = Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 2 };
    assert!(store.branch(&RequestContext::from_verified_record(record), &fork, &branches[0]).is_err());
    let mut record = context().record().clone(); record.employee = EmployeeId([9;16]);
    assert!(store.branch(&RequestContext::from_verified_record(record), &fork, &branches[0]).is_err());
    let mut record = context().record().clone();
    record.employee_binding = EmployeeBinding::Shared(Text::new("tenant".into()).unwrap());
    assert!(store.branch(&RequestContext::from_verified_record(record), &fork, &branches[0]).is_err());
}

#[test]
fn fork_controls_require_exact_branch_and_parent_grants_after_actor_match() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    complete(&store,&fork,&branches[0],run(10),"branch-a",'b');
    store.bind(&context(),&fork,&branches[1],run(11),&workspace("branch-b")).unwrap();
    let mut record = context().record().clone();
    record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
        ResourceGrant::Workspace { handle: Text::new("unrelated".into()).unwrap() }]).unwrap();
    let unrelated = RequestContext::from_verified_record(record);
    assert!(crate::tool_policy::eligible(&unrelated,"terminate_branch"));
    assert!(store.branch(&unrelated,&fork,&branches[0]).is_err());
    assert!(store.request_cancel(&unrelated,&fork,&branches[1]).is_err());
    assert!(store.diff(&unrelated,&fork,&branches[0],&branches[1]).is_err());
    assert!(store.select(&unrelated,&fork,&branches[0],0,&manifest('a').digest()).is_err());
    let mut record = context().record().clone();
    record.resources = List::new(vec![ResourceGrant::Workspace { handle: Text::new("branch-a".into()).unwrap() }]).unwrap();
    assert!(store.select(&RequestContext::from_verified_record(record),&fork,&branches[0],0,&manifest('a').digest()).is_err());
    assert_eq!(store.branch(&context(),&fork,&branches[1]).unwrap().state,"reserved");
    store.select(&context(),&fork,&branches[0],0,&manifest('a').digest()).unwrap();
}

#[test]
fn fork_publication_cas_rejects_stale_parent_revision_and_other_fork() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    assert!(store.select(&context(), &fork, &branches[0], 0, &manifest('a').digest()).is_err());
    complete(&store, &fork, &branches[0], run(10), "branch-a", 'b');
    complete(&store, &fork, &branches[1], run(11), "branch-b", 'c');
    assert_eq!(store.diff(&context(), &fork, &branches[0], &branches[1]).unwrap(), vec!["中文.txt"]);
    assert!(store.select(&context(), &fork, &branches[0], 1, &manifest('a').digest()).is_err());
    assert!(store.select(&context(), &fork, &branches[0], 0, &manifest('b').digest()).is_err());
    assert!(store.select(&context(), &"f".repeat(32), &branches[0], 0, &manifest('a').digest()).is_err());
    assert_eq!(store.select(&context(), &fork, &branches[0], 0, &manifest('a').digest()).unwrap(), (1,manifest('b')));
    assert!(store.select(&context(), &fork, &branches[1], 0, &manifest('a').digest()).is_err());
    assert!(store.select(&context(), &fork, &branches[1], 1, &manifest('b').digest()).is_err());
}

#[test]
fn fork_publication_cas_is_shared_across_forks_of_same_parent() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (first, a) = create(&store);
    let (second, b) = store.create(&context(), "parent", &manifest('a'),
        &[workspace("branch-c"),workspace("branch-d")]).unwrap();
    complete(&store,&first,&a[0],run(10),"branch-a",'b');
    complete(&store,&second,&b[0],run(11),"branch-c",'c');
    store.select(&context(),&first,&a[0],0,&manifest('a').digest()).unwrap();
    assert!(store.select(&context(),&second,&b[0],0,&manifest('a').digest()).is_err());
    assert!(store.select(&context(),&second,&b[0],1,&manifest('b').digest()).is_err());
}

#[tokio::test]
async fn fork_host_controls_reject_ambient_authority_and_wrong_capabilities() {
    use serde_json::json;
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    store.bind(&context(),&fork,&branches[1],run(11),&workspace("branch-b")).unwrap();
    let backend = Arc::new(Backend::default());
    let control = ForkHostControl { store:store.clone(),backend:backend.clone() };
    let args = json!({"fork_id":fork,"branch_id":branches[1]});
    for field in ["root","cwd","run_id","employee_id","principal","workspace","expected_revision"] {
        let mut forged = args.clone(); forged[field] = json!("forged");
        assert!(control.execute(&context(),"terminate_branch",&forged).await.is_err());
    }
    let mut record = context().record().clone(); record.tools = List::new(vec![]).unwrap();
    assert!(control.execute(&RequestContext::from_verified_record(record),"terminate_branch",&args).await.is_err());
    assert_eq!(backend.killed.load(Ordering::SeqCst),0);
    assert!(control.execute(&context(),"terminate_branch",&args).await.is_err());
    assert_eq!(backend.killed.load(Ordering::SeqCst),0);
    assert_eq!(store.branch(&context(),&fork,&branches[1]).unwrap().state,"reserved");
}

#[tokio::test]
async fn fork_v180_deferred_mutations_cannot_publish_or_launch_even_with_valid_authority() {
    use serde_json::json;
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    complete(&store, &fork, &branches[0], run(10), "branch-a", 'b');
    let backend = Arc::new(Backend::default());
    let service = ForkHostControl { store: store.clone(), backend: backend.clone() };
    let before = store.inspect(&context(), &fork).unwrap();
    let args = json!({"fork_id":fork,"branch_id":branches[0],"expected_revision":0,
        "expected_parent_digest":manifest('a').digest(),"prompt":"x"});
    for name in crate::fork_policy::DEFERRED_TOOLS {
        assert!(!service.supports(name));
        assert_eq!(service.execute(&context(), name, &args).await, Err(VerificationError::Unavailable));
        assert_eq!(ForkToolService::execute(&service, &context(), name, &args).await,
            Err(VerificationError::Unavailable));
    }
    assert_eq!(store.inspect(&context(), &fork).unwrap(), before);
    assert_eq!(backend.reserved.load(Ordering::SeqCst), 0);
    assert_eq!(backend.killed.load(Ordering::SeqCst), 0);
    // The retained CAS implementation remains testable; the route did not select.
    assert!(store.select(&context(), &fork, &branches[0], 0, &manifest('a').digest()).is_ok());
}

#[tokio::test]
async fn fork_cost_read_checks_actor_and_grants_and_never_invents_zero_usage() {
    use serde_json::json;
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, _) = create(&store);
    let service = ForkHostControl { store, backend: Arc::new(Backend::default()) };
    for name in crate::fork_policy::READ_TOOLS { assert!(service.supports(name)); }
    let args = json!({"fork_id":fork});
    let value = service.execute(&context(), "fork_cost", &args).await.unwrap();
    assert_eq!(value["measurement_status"], "unavailable");
    assert!(value["aggregate_spent_usd"].is_null());
    assert_eq!(value["branches"].as_array().unwrap().len(), 2);
    assert!(value["branches"].as_array().unwrap().iter().all(|branch| branch["spent_usd"].is_null()));
    for mutation in 0..4 {
        let mut record = context().record().clone();
        match mutation {
            0 => record.principal = Principal::Human { user_id: Text::new("other".into()).unwrap(), generation: 1 },
            1 => record.principal = Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 2 },
            2 => record.resources = List::new(vec![ResourceGrant::Employee { id: record.employee },
                ResourceGrant::Workspace { handle: Text::new("unrelated".into()).unwrap() }]).unwrap(),
            _ => record.tools = List::new(vec![]).unwrap(),
        }
        assert!(service.execute(&RequestContext::from_verified_record(record), "fork_cost", &args).await.is_err());
    }
    assert!(service.execute(&context(), "fork_cost", &json!({"fork_id":"f".repeat(32)})).await.is_err());
}

struct RefusingFactory { preparing: Arc<tokio::sync::Notify>, deny_before_reserve: bool }
impl ForkPlanFactory for RefusingFactory {
    fn authorize<'a>(&'a self, _: &'a RequestContext, _: &'a Branch) -> crate::execution::PolicyFuture<'a, ()> {
        Box::pin(async move { if self.deny_before_reserve { Err(VerificationError::Invalid) } else { Ok(()) } })
    }
    fn prepare<'a>(&'a self, _: &'a RequestContext, _: &'a Branch, _: RunHandle)
        -> crate::execution::PolicyFuture<'a, (SpawnPolicy, crate::resources::AuthorizedHelperWorkspace)> {
        Box::pin(async move { self.preparing.notify_one(); std::future::pending().await })
    }
}

#[tokio::test]
async fn fork_launch_cancellation_cleans_only_its_own_reservation() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    let backend = Arc::new(Backend::default());
    let preparing = Arc::new(tokio::sync::Notify::new());
    let factory = RefusingFactory { preparing:preparing.clone(),deny_before_reserve:true };
    assert!(launch_branch(store.clone(),backend.clone(),&factory,&context(),&fork,&branches[1]).await.is_err());
    assert_eq!(backend.reserved.load(Ordering::SeqCst),0);
    let (s,b,p,f,id) = (store.clone(),backend.clone(),preparing.clone(),fork.clone(),branches[1].clone());
    let task = tokio::spawn(async move {
        launch_branch(s,b,&RefusingFactory { preparing:p,deny_before_reserve:false },&context(),&f,&id).await
    });
    preparing.notified().await; task.abort(); let _ = task.await;
    for _ in 0..20 { if backend.killed.load(Ordering::SeqCst) != 0 { break; } tokio::task::yield_now().await; }
    assert_eq!(backend.reserved.load(Ordering::SeqCst),1);
    assert_eq!(backend.killed.load(Ordering::SeqCst),1);
    assert_eq!(store.branch(&context(),&fork,&branches[1]).unwrap().state,"pending");
}

#[test]
fn fork_cancel_fences_start_and_late_completion() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    assert_eq!(store.request_cancel(&context(), &fork, &branches[0]).unwrap(), None);
    assert!(store.bind(&context(), &fork, &branches[0], run(10), &workspace("branch-a")).is_err());
    store.bind(&context(), &fork, &branches[1], run(11), &workspace("branch-b")).unwrap();
    assert_eq!(store.request_cancel(&context(), &fork, &branches[1]).unwrap(), Some(run(11)));
    assert!(store.started(run(11)).is_err());
    assert_eq!(store.branch(&context(), &fork, &branches[1]).unwrap().state, "cancel_requested");
    store.finish(run(11), &RunOutcome::Exited(0), Some(&manifest('c'))).unwrap();
    assert_eq!(store.branch(&context(), &fork, &branches[1]).unwrap().state, "cancelled");
    assert!(store.select(&context(), &fork, &branches[1], 0, &manifest('a').digest()).is_err());
}

#[test]
fn fork_restart_is_uncertain_never_publishable_or_rebound() {
    let home = tempfile::tempdir().unwrap(); let path = home.path().join("forks.db");
    let store = ForkStore::open(&path).unwrap();
    let (fork, branches) = create(&store);
    store.bind(&context(), &fork, &branches[0], run(10), &workspace("branch-a")).unwrap();
    store.started(run(10)).unwrap(); drop(store);
    let store = ForkStore::open(&path).unwrap();
    assert_eq!(store.branch(&context(), &fork, &branches[0]).unwrap().state, "uncertain");
    assert!(store.finish(run(10), &RunOutcome::Exited(0), Some(&manifest('b'))).is_err());
    assert!(store.bind(&context(), &fork, &branches[0], run(11), &workspace("branch-a")).is_err());
    assert!(store.select(&context(), &fork, &branches[0], 0, &manifest('a').digest()).is_err());
}

#[derive(Default)]
struct Backend { killed: AtomicUsize, reserved: AtomicUsize, spawned: AtomicUsize, fail: bool }
impl ExecutionBackend for Backend {
    fn reserve(&self, _: EmployeeId) -> BackendFuture<'_, RunHandle> {
        Box::pin(async { self.reserved.fetch_add(1,Ordering::SeqCst); Ok(run(11)) })
    }
    fn spawn(&self, plan: SpawnPolicy) -> BackendFuture<'_, RunHandle> {
        Box::pin(async move {
            assert_eq!(plan.run,run(11));
            assert_eq!(plan.class,ExecutionClass::Fork);
            assert_eq!(plan.workspace,workspace("branch-b"));
            let delivery = plan.workspace_delivery.as_ref().expect("branch delivery proof");
            assert_eq!(delivery.run, plan.run.0);
            assert_ne!(delivery.run, context().record().run);
            assert_eq!(delivery.employee, plan.employee);
            assert_eq!(delivery.policy, plan.workspace);
            self.spawned.fetch_add(1,Ordering::SeqCst); Ok(run(11))
        })
    }
    fn take_stdio(&self, _: RunHandle) -> BackendFuture<'_, RunStdio> { Box::pin(async { unreachable!() }) }
    fn take_pty(&self, _: RunHandle) -> BackendFuture<'_, RunPty> { Box::pin(async { unreachable!() }) }
    fn resize(&self, _: RunHandle, _: PtySize) -> BackendFuture<'_, ()> { Box::pin(async { unreachable!() }) }
    fn wait(&self, _: RunHandle, _: WaitOrigin) -> BackendFuture<'_, RunOutcome> { Box::pin(async { unreachable!() }) }
    fn kill(&self, handle: RunHandle) -> BackendFuture<'_, ()> {
        Box::pin(async move {
            assert_eq!(handle, run(11)); self.killed.fetch_add(1, Ordering::SeqCst);
            if self.fail { Err(BackendError::Unavailable) } else { Ok(()) }
        })
    }
}
#[tokio::test]
async fn fork_termination_uses_bound_backend_handle_and_keeps_uncertainty() {
    let home = tempfile::tempdir().unwrap();
    let store = ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork, branches) = create(&store);
    store.bind(&context(), &fork, &branches[1], run(11), &workspace("branch-b")).unwrap();
    let backend = Arc::new(Backend { fail:true, ..Default::default() });
    let mut record = context().record().clone(); record.employee = EmployeeId([9;16]);
    assert!(terminate(store.clone(),backend.clone(), &RequestContext::from_verified_record(record), &fork, &branches[1]).await.is_err());
    assert_eq!(backend.killed.load(Ordering::SeqCst),0);
    assert!(terminate(store.clone(),backend.clone(), &context(), &fork, &branches[1]).await.is_err());
    assert_eq!(backend.killed.load(Ordering::SeqCst),1);
    assert_eq!(store.branch(&context(), &fork, &branches[1]).unwrap().state,"cancel_requested");
    let backend = Arc::new(Backend::default());
    assert_eq!(terminate(store.clone(),backend.clone(), &context(), &fork, &branches[1]).await.unwrap(),"cancel_requested");
    assert_eq!(store.branch(&context(), &fork, &branches[1]).unwrap().state,"cancel_requested");
}

// Uses actual AuthorizedHelperWorkspace construction on a nodev filesystem. The
// backend and runtime remain explicit doubles; this is not root mount delivery.
#[cfg(target_os = "linux")]
mod linux_workspace {
    use super::*;
    struct Factory { roots: crate::resources::ResourceRoots, wrong_workspace: bool, wrong_run: bool }
    impl ForkPlanFactory for Factory {
        fn authorize<'a>(&'a self, _: &'a RequestContext, _: &'a Branch) -> crate::execution::PolicyFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare<'a>(&'a self, context: &'a RequestContext, branch: &'a Branch, reserved: RunHandle)
            -> crate::execution::PolicyFuture<'a, (SpawnPolicy, crate::resources::AuthorizedHelperWorkspace)> {
            Box::pin(async move {
                let proof = self.roots.authorize_helper_workspace(context,&branch.workspace)?;
                Ok((SpawnPolicy {
                    workspace_delivery: None, run:if self.wrong_run { run(99) } else { reserved }, employee:context.record().employee,
                    class:ExecutionClass::Fork, runtime:RuntimeKind::Registered(Text::new("fork-test-only".into()).unwrap()),
                    argv_allowlist:vec![],environment_allowlist:Default::default(),
                    workspace:if self.wrong_workspace { workspace("parent") } else { branch.workspace.clone() },
                    resources:ResourceLimits { memory_bytes:1024,cpu_quota_percent:100,tasks:8,open_files:4096,ptys:0,
                        disk:DiskQuota::LoopFilesystem { bytes:4096,inodes:32 },io_read_bytes_per_sec:1024,
                        io_write_bytes_per_sec:1024,io_read_ops_per_sec:10,io_write_ops_per_sec:10,
                        output_buffer_bytes:1024,output_total_bytes:4096,memfd_total_bytes:4096 },
                    network:NetworkPolicy::Offline,pty:false,capabilities:CapabilityHandover { mcp_bridge:false,egress_forwarder:false },
                    timeouts:RunTimeouts { queue_ms:1000,startup_ms:1000,runtime_ms:1000,pty_idle_ms:None,termination_grace_ms:100 },
                    inputs:RunInputs { prompt:RunInput { max_bytes:0,reader:Box::new(Empty) },attachments:vec![] },
                },proof))
            })
        }
    }
    struct Empty;
    impl RunReader for Empty {
        fn read<'a>(&'a mut self, _: &'a mut [u8]) -> BackendFuture<'a, usize> { Box::pin(async { Ok(0) }) }
    }
    #[tokio::test]
    async fn fork_branch_backend_driver_binds_authorized_workspace_before_spawn() {
        let home = tempfile::tempdir().unwrap();
        let directory = tempfile::tempdir_in("/dev/shm").expect("Linux test needs nodev /dev/shm");
        for (wrong_workspace,wrong_run) in [(true,false),(false,true),(false,false)] {
            let store = ForkStore::open(&home.path().join(format!("{wrong_workspace}-{wrong_run}.db"))).unwrap();
            let (fork,branches) = create(&store);
            let mut roots = crate::resources::ResourceRoots::default();
            roots.insert("branch-b".into(),context().record().employee,std::fs::File::open(directory.path()).unwrap()).unwrap();
            let backend = Arc::new(Backend::default());
            let result = launch_branch(store.clone(),backend.clone(),&Factory { roots,wrong_workspace,wrong_run },
                &context(),&fork,&branches[1]).await;
            if wrong_workspace || wrong_run {
                assert!(result.is_err());
                for _ in 0..20 { if backend.killed.load(Ordering::SeqCst) != 0 { break; } tokio::task::yield_now().await; }
                assert_eq!(backend.killed.load(Ordering::SeqCst),1);
                assert_eq!(backend.spawned.load(Ordering::SeqCst),0);
            } else {
                assert_eq!(result.unwrap(),run(11));
                let branch = store.branch(&context(),&fork,&branches[1]).unwrap();
                assert_eq!(branch.state,"reserved"); assert_eq!(branch.run,Some(run(11)));
                assert_eq!(backend.spawned.load(Ordering::SeqCst),1);
                assert_eq!(backend.killed.load(Ordering::SeqCst),0);
            }
        }
    }
}

#[test]
fn fork_queue_state_is_atomic_cancellable_and_uncertain_after_restart() {
    let home=tempfile::tempdir().unwrap(); let path=home.path().join("forks.db");
    let store=ForkStore::open(&path).unwrap(); let (fork,branches)=create(&store);
    assert!(store.queue(&context(),&fork,&[branches[0].clone(),branches[0].clone()]).is_err());
    assert_eq!(store.branch(&context(),&fork,&branches[0]).unwrap().state,"pending");
    store.queue(&context(),&fork,&branches).unwrap();
    assert_eq!(store.branch(&context(),&fork,&branches[0]).unwrap().state,"queued");
    assert!(store.queue(&context(),&fork,&branches).is_err());
    assert_eq!(store.request_cancel(&context(),&fork,&branches[0]).unwrap(),None);
    assert_eq!(store.branch(&context(),&fork,&branches[0]).unwrap().state,"cancelled");
    drop(store);
    let store=ForkStore::open(&path).unwrap();
    assert_eq!(store.branch(&context(),&fork,&branches[1]).unwrap().state,"uncertain");
    assert!(store.bind(&context(),&fork,&branches[1],run(9),&workspace("branch-b")).is_err());
}

#[test]
fn fork_inspect_exposes_cas_baseline_only_after_all_grants() {
    let home=tempfile::tempdir().unwrap(); let store=ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork,_)=create(&store); let inspected=store.inspect(&context(),&fork).unwrap();
    assert_eq!(inspected["expected_revision"],0);
    assert_eq!(inspected["expected_parent_digest"],manifest('a').digest());
    assert_eq!(inspected["branches"].as_array().unwrap().len(),2);
    for denied in ["parent","branch-a","branch-b"] {
        let mut record=context().record().clone();
        record.resources=List::new(record.resources.as_slice().iter().filter(|grant| !matches!(grant,
            ResourceGrant::Workspace{handle} if handle.as_str()==denied)).cloned().collect()).unwrap();
        assert!(store.inspect(&RequestContext::from_verified_record(record),&fork).is_err());
    }
}

struct RefusingSnapshot;
impl crate::fork_queue::ForkResultSnapshot for RefusingSnapshot {
    fn snapshot<'a>(&'a self,_:&'a RequestContext,_:&'a Branch,_:RunHandle)->crate::execution::PolicyFuture<'a,Manifest> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
}
#[tokio::test]
async fn fork_queue_denial_precedes_reservation_and_durable_queue() {
    let home=tempfile::tempdir().unwrap(); let store=ForkStore::open(&home.path().join("forks.db")).unwrap();
    let (fork,branches)=create(&store); let backend=Arc::new(Backend::default());
    let queue=crate::fork_queue::ForkQueue::new(store.clone(),backend.clone(),Arc::new(RefusingFactory {
        preparing:Arc::new(tokio::sync::Notify::new()),deny_before_reserve:true }),Arc::new(RefusingSnapshot));
    assert!(queue.enqueue(&context(),&fork,branches.clone()).await.is_err());
    assert_eq!(store.branch(&context(),&fork,&branches[0]).unwrap().state,"pending");
    assert_eq!(backend.reserved.load(Ordering::SeqCst),0);
}
