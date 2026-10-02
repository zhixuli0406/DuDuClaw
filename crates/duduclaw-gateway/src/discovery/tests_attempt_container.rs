use super::*;

fn fixture() -> (tempfile::TempDir, AttemptRequest, AttemptSettings, AttemptRuntimeConfig) {
    let home = tempfile::tempdir().unwrap();
    let run = home.path().join("discovery/runs/run-1");
    let node = run.join("r1/b0/a0/ws");
    let visible = run.join("r1/b1/a0/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    super::super::workspace::create_private_directory(&visible).unwrap();
    std::fs::write(visible.join("completed.txt"), "immutable input").unwrap();
    let request = AttemptRequest {
        run_id:"run-1".into(),cell_id:"r1-b0-a0".into(),node_dir:node,
        run_dir:run,read_workspaces:vec![visible],prompt:"same prompt".into(),
        agent_id:"worker".into(),model:Some("model".into()),
        timeout:Duration::from_secs(3),max_turns:1,account_pool:vec!["dedicated".into()],
    };
    let runtime = AttemptRuntimeConfig { image:format!("test/image@sha256:{}", "a".repeat(64)), executable:PathBuf::from("/opt/runtime/claude"),base_url:None,provider:None };
    (home, request, AttemptSettings::default(), runtime)
}

#[test]
fn container_has_hard_caps_trusted_pid1_and_only_explicit_snapshot_binds() {
    let (home, request, settings, runtime) = fixture();
    let prepared = prepare(home.path(), &request, &settings, QuotaLimits::default(), &runtime,
        &["-p".into()], &BTreeMap::new(), request.timeout).unwrap();
    let args = prepared.create.get_args().map(|arg|arg.to_string_lossy().into_owned()).collect::<Vec<_>>();
    for flag in ["create", "--read-only", "--user", "--memory", "--pids-limit", "--cpus", "--tmpfs", "--cap-drop", "--security-opt", "--entrypoint"] {
        assert!(args.iter().any(|value|value==flag), "missing hard boundary: {flag}");
    }
    assert!(!args.windows(2).any(|pair|pair[0]=="--user" && pair[1].split(':').next()==Some("0")));
    assert!(args.iter().any(|arg|arg=="python3"));
    let tmpfs=args.windows(2).find(|pair|pair[0]=="--tmpfs").map(|pair|pair[1].clone()).unwrap();
    let options=tmpfs.trim_start_matches("/tmp:").split(',').collect::<Vec<_>>();
    for option in ["exec","nosuid","nodev"] { assert!(options.contains(&option),"tmpfs lacks {option}: {tmpfs}"); }
    assert!(!options.contains(&"noexec"),"CLIs extract helper binaries into HOME/TMPDIR");
    assert!(args.iter().any(|arg|arg.contains("com.duduclaw.discovery.role=attempt")));
    let mounts = args.windows(2).filter(|pair|pair[0]=="--mount").map(|pair|pair[1].clone()).collect::<Vec<_>>();
    assert_eq!(mounts.len(), 3);
    assert!(mounts.iter().any(|mount| mount.contains("dst=/dudu-runtime,readonly")));
    let own = request.node_dir.canonicalize().unwrap();
    assert!(mounts.iter().any(|mount|mount.contains(&format!("src={},",own.display())) && !mount.contains("readonly")));
    let completed = request.read_workspaces[0].canonicalize().unwrap();
    assert!(mounts.iter().any(|mount|mount.contains(&format!("dst={},",completed.display())) && mount.contains("readonly") && !mount.contains(&format!("src={},",completed.display()))));
    assert!(!mounts.iter().any(|mount|mount.contains(&format!("src={},",request.run_dir.display())) || mount.contains("evaluators")));
}

#[test]
fn mutable_tags_zero_caps_host_executable_and_ledger_read_are_refused() {
    let (home, request, mut settings, mut runtime) = fixture();
    runtime.image="image:latest".into();
    assert!(prepare(home.path(), &request, &settings, QuotaLimits::default(), &runtime, &[], &BTreeMap::new(), request.timeout).is_err());
    runtime.image=format!("image@sha256:{}", "b".repeat(64));
    runtime.executable=home.path().join("host-binary");
    assert!(prepare(home.path(), &request, &settings, QuotaLimits::default(), &runtime, &[], &BTreeMap::new(), request.timeout).is_err());
    runtime.executable=PathBuf::from("/opt/runtime/claude");settings.pids=0;
    assert!(prepare(home.path(), &request, &settings, QuotaLimits::default(), &runtime, &[], &BTreeMap::new(), request.timeout).is_err());
    settings.pids=64;let mut invalid=request.clone();invalid.read_workspaces=vec![request.run_dir.clone()];
    assert!(prepare(home.path(), &invalid, &settings, QuotaLimits::default(), &runtime, &[], &BTreeMap::new(), request.timeout).is_err());
}

#[cfg(unix)]
fn mock(mode:&str) -> (tempfile::TempDir, PreparedAttemptContainer, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let (home,request,settings,runtime)=fixture();
    let mut prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).unwrap();
    let client=home.path().join("docker-mock");let log=home.path().join("calls.log");
    std::fs::write(&client,"#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$LOG\"\ncase \"$1\" in\ncreate) if [ \"$MODE\" = bad_id ]; then echo bad; else printf '%064d\\n' 1; fi;;\nstart) if [ \"$MODE\" = hang ]; then sleep 30; else cat >/dev/null; echo '{\"type\":\"result\",\"result\":\"done\",\"total_cost_usd\":0.001}'; fi;;\nrm) if [ \"$MODE\" = cleanup_fail ]; then exit 2; fi;;\n*) exit 3;;\nesac\n").unwrap();
    std::fs::set_permissions(&client,std::fs::Permissions::from_mode(0o700)).unwrap();
    let args=prepared.create.get_args().map(|arg|arg.to_owned()).collect::<Vec<_>>();
    prepared.create=Command::new(client);prepared.create.args(args).env_clear().env("LOG",&log).env("MODE",mode).env("PATH","/usr/bin:/bin");
    (home,prepared,log)
}

#[cfg(unix)]
#[tokio::test]
async fn lifecycle_confirms_create_and_cleanup_before_success_and_blocks_on_cleanup_failure() {
    let (_home,prepared,log)=mock("ok");
    let output=prepared.run(b"prompt",Duration::from_secs(3), |_|true).await.unwrap();assert!(output.status.success());
    assert_eq!(std::fs::read_to_string(log).unwrap(),"create\nstart\nrm\n");
    let (_home,prepared,log)=mock("bad_id");
    assert!(prepared.run(b"prompt",Duration::from_secs(3), |_|true).await.is_err());
    assert_eq!(std::fs::read_to_string(log).unwrap(),"create\nrm\n");
    let (home,prepared,_)=mock("cleanup_fail");
    assert!(matches!(prepared.run(b"prompt",Duration::from_secs(3), |_|true).await,Err(AttemptInfraError::CleanupFailed(_))));
    assert!(super::super::maintenance::check_clean(home.path()).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_and_dropped_future_finish_synchronous_cleanup() {
    let (_home,prepared,log)=mock("hang");let started=Instant::now();
    let output=prepared.run(b"prompt",Duration::from_secs(2), |_|true).await.unwrap();assert!(output.timed_out);assert!(started.elapsed()<Duration::from_secs(5));
    assert!(std::fs::read_to_string(log).unwrap().ends_with("rm\n"));
    let (_home,prepared,log)=mock("hang");
    let task=tokio::spawn(async move {prepared.run(b"prompt",Duration::from_secs(30), |_|true).await});
    let deadline=Instant::now()+Duration::from_secs(5);
    while !std::fs::read_to_string(&log).unwrap_or_default().contains("start\n") {
        assert!(Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(10)).await;
    }
    task.abort();let _=task.await;
    assert!(std::fs::read_to_string(log).unwrap().ends_with("rm\n"));
}

#[test]
fn published_read_copy_is_immutable_and_tamper_prevents_result_delivery() {
    let (home,request,settings,runtime)=fixture();
    let prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).unwrap();
    std::fs::write(request.read_workspaces[0].join("completed.txt"),"changed after prepare").unwrap();
    assert!(prepared.guards.iter().try_for_each(IntegrityGuard::verify).is_err());
    let mount=prepared.create.get_args().map(|v|v.to_string_lossy()).find(|v|v.contains("dst=") && v.contains("b1") && v.contains("readonly")).unwrap().into_owned();
    let snapshot=mount.split("src=").nth(1).unwrap().split(',').next().unwrap();
    assert_eq!(std::fs::read_to_string(Path::new(snapshot).join("completed.txt")).unwrap(),"immutable input");
}

/// Real Docker, zero providers. A Python image with an operator-pinned digest
/// is required; detached descendants cannot survive namespace PID-1 exit.
#[tokio::test]
#[ignore = "requires local Docker and DUDU_DISCOVERY_ATTEMPT_IMAGE pinned Python image"]
async fn real_container_all_exit_paths_kill_detached_descendants_without_model_calls() {
    let image=std::env::var("DUDU_DISCOVERY_ATTEMPT_IMAGE").expect("set pinned Python image digest");
    for mode in ["normal","timeout","cancel"] {
        let (home,request,settings,mut runtime)=fixture();runtime.image=image.clone();runtime.executable=PathBuf::from("/usr/local/bin/python3");
        let marker=request.node_dir.join("detached-wrote.txt");
        let detached=format!("import time,pathlib;time.sleep(4);pathlib.Path({:?}).write_text('escaped')",marker.to_str().unwrap());
        let script=format!("import subprocess,sys,time;subprocess.Popen([sys.executable,'-c',{:?}],start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print('{{\\\"type\\\":\\\"result\\\",\\\"result\\\":\\\"done\\\"}}',flush=True);{}",detached,if mode=="normal"{"sys.exit(0)"}else{"time.sleep(60)"});
        let timeout=if mode=="normal" {Duration::from_secs(15)} else {Duration::from_secs(2)};
        let prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&["-I".into(),"-S".into(),"-B".into(),"-c".into(),script],&BTreeMap::new(),timeout).unwrap();
        if mode=="cancel" {
            let task=tokio::spawn(async move {prepared.run(b"",timeout, |_|true).await});
            tokio::time::sleep(Duration::from_secs(1)).await;task.abort();let _=task.await;
        } else {
            let output=prepared.run(b"",timeout, |_|true).await.unwrap();
            if mode=="normal" {assert!(output.status.success());}
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!marker.exists(),"detached process survived {mode}");
        assert!(super::super::maintenance::check_clean(home.path()).is_ok());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn expired_prepared_container_never_spawns_even_when_caller_supplies_fresh_timeout() {
    let (_home,mut prepared,log)=mock("ok");
    prepared.deadline=Instant::now()-Duration::from_millis(1);
    assert!(matches!(prepared.run(b"prompt",Duration::from_secs(30), |_|true).await,Err(AttemptInfraError::BudgetExhausted)));
    assert!(!log.exists());
}

#[test]
fn host_allocated_policy_developer_request_has_only_own_workspace_and_policy_scope() {
    let (home,mut request,settings,runtime)=fixture();
    let developer=create_policy_development_workspace(home.path(),&request.run_id).unwrap();
    let node=developer.path().join("a1/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    std::fs::write(node.join("method.py"),"# synthetic policy").unwrap();
    request.run_dir=developer.path().canonicalize().unwrap();request.node_dir=node;request.read_workspaces.clear();request.cell_id="policy-r1-v1".into();
    let prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).unwrap();
    let args=prepared.create.get_args().map(|arg|arg.to_string_lossy()).collect::<Vec<_>>();
    assert!(args.iter().any(|arg|arg.contains("com.duduclaw.discovery.role=policy")));
    let mounts=args.windows(2).filter(|pair|pair[0]=="--mount").map(|pair|pair[1].as_ref()).collect::<Vec<_>>();
    assert_eq!(mounts.len(),2);
    assert!(mounts.iter().any(|mount|mount.contains(&format!("src={},",request.node_dir.display()))));
    assert!(!mounts.iter().any(|mount|mount.contains(&format!("src={},",request.run_dir.display()))));
}

#[test]
fn snapshots_respect_aggregate_run_and_global_quota_including_other_prepared_calls() {
    let (home,request,settings,runtime)=fixture();
    let existing=super::super::workspace::tree_bytes(&request.run_dir).unwrap();
    let too_small=QuotaLimits {max_run_bytes:existing,max_total_bytes:1024*1024};
    assert!(prepare(home.path(),&request,&settings,too_small,&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
    let first=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).unwrap();
    let discovery=home.path().join("discovery");
    let used=super::super::workspace::tree_bytes(&discovery).unwrap();
    let no_more=QuotaLimits {max_run_bytes:1024*1024,max_total_bytes:used};
    assert!(prepare(home.path(),&request,&settings,no_more,&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
    drop(first);
}

#[test]
fn policy_development_rejects_foreign_or_shared_sessions_and_cross_workspace_reads() {
    let (home,mut request,settings,runtime)=fixture();
    let developer=create_policy_development_workspace(home.path(),&request.run_id).unwrap();
    let node=developer.path().join("a1/ws");
    super::super::workspace::create_private_directory(&node).unwrap();
    request.run_dir=developer.path().canonicalize().unwrap();request.node_dir=node.clone();request.cell_id="policy-r1-v1".into();
    assert!(prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
    request.read_workspaces.clear();request.cell_id="policy-r1-v2".into();
    assert!(prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
    request.cell_id="policy-r1-v1".into();
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(developer.path(),std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
        std::fs::set_permissions(developer.path(),std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let foreign=tempfile::tempdir().unwrap();
    let foreign_node=foreign.path().join("a1/ws");super::super::workspace::create_private_directory(&foreign_node).unwrap();
    request.run_dir=foreign.path().into();request.node_dir=foreign_node;
    assert!(prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
}

#[test]
fn quota_counts_artifacts_policy_sessions_and_reports_before_allocating_a_snapshot() {
    for association in ["artifacts","policy-development","reports"] {
        let (home,request,settings,runtime)=fixture();
        let discovery=home.path().join("discovery");
        let file=if association=="reports" {
            let parent=discovery.join("reports");super::super::workspace::create_private_directory(&parent).unwrap();
            parent.join("run-1.json")
        } else {
            let parent=discovery.join(association).join("run-1");super::super::workspace::create_private_directory(&parent).unwrap();
            parent.join("payload")
        };
        std::fs::write(file,vec![0u8;2048]).unwrap();
        assert!(prepare(home.path(),&request,&settings,QuotaLimits {max_run_bytes:1024,max_total_bytes:1024*1024},&runtime,&[],&BTreeMap::new(),Duration::from_secs(5)).is_err());
        assert!(!discovery.join("attempt-snapshots/run-1").exists(),"quota failed after snapshot allocation: {association}");
    }
}

#[cfg(unix)]
#[test]
fn snapshot_quota_uses_the_workspace_sidecar_and_waits_only_within_deadline() {
    let (home,request,settings,runtime)=fixture();
    let path=home.path().join("discovery/.quota.lock");
    let (ready_tx,ready_rx)=std::sync::mpsc::channel();let (release_tx,release_rx)=std::sync::mpsc::channel();
    let holder=std::thread::spawn(move||duduclaw_core::with_file_lock(&path,||{
        ready_tx.send(()).unwrap();release_rx.recv_timeout(Duration::from_secs(3)).unwrap();Ok(())
    }).unwrap());
    ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started=Instant::now();
    let result=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),Duration::from_millis(100));
    release_tx.send(()).unwrap();holder.join().unwrap();
    assert!(result.is_err());assert!(started.elapsed()<Duration::from_secs(1));
    assert!(!home.path().join("discovery/attempt-snapshots/run-1").exists());
}

#[test]
fn controlled_bundle_preserves_private_permissions_and_checks_pre_and_post_write_quota() {
    let (home,request,_,_)=fixture();
    let limits=QuotaLimits {max_run_bytes:1024,max_total_bytes:4096};
    let mut entered=false;
    assert!(allocate_controlled_snapshot(home.path(),&request.run_id,limits,Instant::now()+Duration::from_secs(5),2048,|_|{
        entered=true;Ok(())
    }).is_err());assert!(!entered);
    // Generated content may be larger than a caller's estimate; it must not
    // be published or retained after the actual-size postcondition fails.
    assert!(allocate_controlled_snapshot(home.path(),&request.run_id,limits,Instant::now()+Duration::from_secs(5),0,|path|{
        std::fs::write(path.join("bundle.py"),vec![0u8;2048]).map_err(failure)
    }).is_err());
    let root=home.path().join("discovery/attempt-snapshots/run-1");
    assert_eq!(tree_bytes(&root).unwrap(),0);
    let (private,())=allocate_controlled_snapshot(home.path(),&request.run_id,limits,Instant::now()+Duration::from_secs(5),2,|path|{
        std::fs::write(path.join("bundle.py"),b"ok").map_err(failure)
    }).unwrap();
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(private.path()).unwrap().permissions().mode()&0o777,0o700);}
    assert_eq!(tree_bytes(&root).unwrap(),2);drop(private);assert_eq!(tree_bytes(&root).unwrap(),0);
}

#[test]
fn runtime_files_are_validated_quota_counted_and_integrity_guarded() {
    let (home,request,settings,runtime)=fixture();
    let files=vec![("prompt.txt".to_string(),b"prompt".to_vec()),("home-seed/.gemini/config/hooks.json".to_string(),b"{}".to_vec())];
    let prepared=prepare_with_files(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),&files,Duration::from_secs(5)).unwrap();
    let mount=prepared.create.get_args().map(|v|v.to_string_lossy().into_owned()).find(|v|v.contains("dst=/dudu-runtime,")).unwrap();
    let config=PathBuf::from(mount.split("src=").nth(1).unwrap().split(',').next().unwrap());
    assert_eq!(std::fs::read(config.join("prompt.txt")).unwrap(),b"prompt");
    assert_eq!(std::fs::read(config.join("home-seed/.gemini/config/hooks.json")).unwrap(),b"{}");
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(config.join("home-seed/.gemini")).unwrap().permissions().mode()&0o777,0o700);
        assert_eq!(std::fs::metadata(config.join("prompt.txt")).unwrap().permissions().mode()&0o777,0o600);}
    std::fs::write(config.join("prompt.txt"),b"tampered").unwrap();
    assert!(prepared.guards.iter().try_for_each(IntegrityGuard::verify).is_err(),"runtime files are covered by the integrity guard");
    drop(prepared);
    let deep=format!("{}x","d/".repeat(8));
    for bad in ["../escape","/abs","a//b","./x","a/../b","gemini.json","empty-mcp.json","sp ace","a\\b","",deep.as_str()] {
        let files=vec![(bad.to_string(),b"x".to_vec())];
        assert!(prepare_with_files(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),&files,Duration::from_secs(5)).is_err(),"{bad:?}");
    }
    let clash=vec![("a".to_string(),b"x".to_vec()),("a/b".to_string(),b"y".to_vec())];
    assert!(prepare_with_files(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),&clash,Duration::from_secs(5)).is_err());
    let dup=vec![("a".to_string(),b"x".to_vec()),("a".to_string(),b"y".to_vec())];
    assert!(prepare_with_files(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&BTreeMap::new(),&dup,Duration::from_secs(5)).is_err());
    let used=tree_bytes(&home.path().join("discovery")).unwrap();
    let big=vec![("prompt.txt".to_string(),vec![b'p';64*1024])];
    assert!(prepare_with_files(home.path(),&request,&settings,QuotaLimits {max_run_bytes:1024*1024,max_total_bytes:used+32*1024},&runtime,&[],&BTreeMap::new(),&big,Duration::from_secs(5)).is_err(),"runtime files count against the quota");
}

/// Real Docker, zero providers: home seed, credential document and child
/// environment as the supervisor prepares them for the CLI.
#[tokio::test]
#[ignore = "requires local Docker and DUDU_DISCOVERY_ATTEMPT_IMAGE pinned Python image"]
async fn real_container_supervisor_seeds_home_and_hands_off_the_credential_document() {
    let image=std::env::var("DUDU_DISCOVERY_ATTEMPT_IMAGE").expect("set pinned Python image digest");
    let (home,request,settings,mut runtime)=fixture();runtime.image=image.clone();runtime.executable=PathBuf::from("/usr/bin/env");
    let probe=r#"import os,stat,json,sys
h="/tmp/dudu-private/home"
def mode(p): return oct(stat.S_IMODE(os.lstat(p).st_mode))
out={"env":sorted(k for k in os.environ if k.startswith("DUDU_CREDENTIAL")),
 "doc":open(h+"/.codex/auth.json").read(),"doc_mode":mode(h+"/.codex/auth.json"),
 "seed":open(h+"/.gemini/config/hooks.json").read(),"seed_mode":mode(h+"/.gemini/config/hooks.json"),
 "seed_dir_mode":mode(h+"/.gemini"),"grok":os.path.isdir(h+"/.grok"),"codex":mode(h+"/.codex")}
u=os.umask(0);os.umask(u);out["umask"]=oct(u)
import shutil,subprocess
shutil.copy("/bin/true",h+"/helper");os.chmod(h+"/helper",0o700)
out["home_exec"]=subprocess.run([h+"/helper"]).returncode
print(json.dumps({"type":"result","result":json.dumps(out)}),flush=True)"#;
    let env=BTreeMap::from([("DUDU_CREDENTIAL_DOC".to_string(),"{\"auth_mode\":\"probe\"}".to_string()),("DUDU_CREDENTIAL_DEST".to_string(),".codex/auth.json".to_string())]);
    let files=vec![("home-seed/.gemini/config/hooks.json".to_string(),b"{\"seed\":1}".to_vec())];
    let prepared=prepare_with_files(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&["python3".into(),"-I".into(),"-S".into(),"-B".into(),"-c".into(),probe.into()],&env,&files,Duration::from_secs(30)).unwrap();
    let output=prepared.run(b"",Duration::from_secs(30),|_|true).await.unwrap();
    assert!(output.status.success(),"{}",output.stderr);
    let line:serde_json::Value=serde_json::from_str(output.stdout.lines().last().unwrap()).unwrap();
    let seen:serde_json::Value=serde_json::from_str(line["result"].as_str().unwrap()).unwrap();
    assert_eq!(seen["env"],serde_json::json!([]),"credential variables never reach the CLI");
    assert_eq!(seen["doc"],"{\"auth_mode\":\"probe\"}");assert_eq!(seen["doc_mode"],"0o600");
    assert_eq!(seen["seed"],"{\"seed\":1}");assert_eq!(seen["seed_mode"],"0o600");assert_eq!(seen["seed_dir_mode"],"0o700");
    assert_eq!(seen["grok"],true);assert_eq!(seen["codex"],"0o700");
    assert_eq!(seen["home_exec"],0,"a helper extracted into the private HOME must run");
    assert_ne!(seen["umask"],"0o77","the CLI runs with the container's own umask, not the supervisor's 077");
    // A traversing destination stops the container before the CLI starts.
    for dest in ["../escape.json","/tmp/abs.json","a/../../b"] {
        let (home,request,settings,mut runtime)=fixture();runtime.image=image.clone();runtime.executable=PathBuf::from("/usr/bin/env");
        let marker=request.node_dir.join("cli-started");
        let script=format!("open({:?},'w').write('x')",marker.to_str().unwrap());
        let env=BTreeMap::from([("DUDU_CREDENTIAL_DOC".to_string(),"{\"a\":1}".to_string()),("DUDU_CREDENTIAL_DEST".to_string(),dest.to_string())]);
        let prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&["python3".into(),"-I".into(),"-c".into(),script],&env,Duration::from_secs(30)).unwrap();
        let output=prepared.run(b"",Duration::from_secs(30),|_|true).await.unwrap();
        assert!(!output.status.success(),"{dest}");assert!(!marker.exists(),"{dest}: CLI must not start");
    }
}

#[test]
fn secrets_reach_docker_create_by_name_only_and_never_start_or_rm() {
    let (home,request,settings,runtime)=fixture();
    let env=BTreeMap::from([("HOME".to_string(),"/tmp/dudu-private/home".to_string()),
        ("OPENAI_API_KEY".to_string(),"sk-secret-value".to_string()),("CODEX_API_KEY".to_string(),"sk-secret-value".to_string()),
        ("DUDU_CREDENTIAL_DOC".to_string(),"{\"refresh\":\"doc-secret\"}".to_string()),("DUDU_CREDENTIAL_DEST".to_string(),".codex/auth.json".to_string())]);
    let prepared=prepare(home.path(),&request,&settings,QuotaLimits::default(),&runtime,&[],&env,Duration::from_secs(5)).unwrap();
    let args=prepared.create.get_args().map(|a|a.to_string_lossy().into_owned()).collect::<Vec<_>>();
    assert!(!args.iter().any(|a|a.contains("sk-secret-value") || a.contains("doc-secret")),"no secret value in the docker argv");
    let names=args.windows(2).filter(|p|p[0]=="--env").map(|p|p[1].as_str()).collect::<Vec<_>>();
    for secret in ["OPENAI_API_KEY","CODEX_API_KEY","DUDU_CREDENTIAL_DOC"] { assert!(names.contains(&secret),"{secret} passed by name"); }
    assert!(names.contains(&"HOME=/tmp/dudu-private/home"),"non-secret keys keep KEY=value");
    assert!(names.contains(&"DUDU_CREDENTIAL_DEST=.codex/auth.json"));
    let create_env=prepared.create.get_envs().filter_map(|(k,v)|Some((k.to_str()?.to_owned(),v?.to_str()?.to_owned()))).collect::<BTreeMap<_,_>>();
    assert_eq!(create_env["OPENAI_API_KEY"],"sk-secret-value");assert_eq!(create_env["DUDU_CREDENTIAL_DOC"],"{\"refresh\":\"doc-secret\"}");
    assert_ne!(create_env.get("HOME").map(String::as_str),Some("/tmp/dudu-private/home"),"the docker client keeps its own HOME");
    let later=client_env(&prepared.create);
    assert!(!later.iter().any(|(k,v)|super::super::attempt_adapter::is_secret_env(k.to_str().unwrap()) || v.to_string_lossy().contains("sk-secret-value") || v.to_string_lossy().contains("doc-secret")),"start/rm carry no secret");
}
