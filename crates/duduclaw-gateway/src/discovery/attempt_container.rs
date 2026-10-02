//! One-shot containers with private read snapshots and namespace-wide teardown.
use std::{collections::BTreeMap, io, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use super::{config::{AttemptRuntimeConfig, AttemptSettings}, contracts::{AttemptInfraError, AttemptRequest}, workspace::{IntegrityGuard, canonical_real_directory, directory_sha256, tree_bytes}};

/// Authoritative host configuration, never deserialized from a task body.
#[derive(Debug,Clone,Copy)]
pub struct QuotaLimits { pub max_run_bytes:u64, pub max_total_bytes:u64 }
impl Default for QuotaLimits {
    fn default()->Self { Self {max_run_bytes:512*1024*1024,max_total_bytes:2*1024*1024*1024} }
}
/// Only the host producer allocates this private, run-associated root. The
/// container receives its own aN/ws only, never the parent history/home.
pub fn create_policy_development_workspace(home:&Path,run_id:&str)->io::Result<tempfile::TempDir> {
    super::maintenance::scope_labels(home,run_id,"policy").map_err(io::Error::other)?;
    let home=canonical_real_directory(home)?;
    let parent=home.join("discovery/policy-development").join(run_id);
    super::workspace::create_private_directory(&parent)?;
    private_tempdir(&parent,"session-")
}

fn private_tempdir(parent:&Path,prefix:&str)->io::Result<tempfile::TempDir> {
    #[cfg(not(unix))]
    {let _=(parent,prefix);Err(io::Error::other("private temporary ACL unavailable"))}
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        let private=tempfile::Builder::new().prefix(prefix)
            .permissions(std::fs::Permissions::from_mode(0o700)).tempdir_in(parent)?;
        super::workspace::create_private_directory(private.path())?;
        Ok(private)
    }
}
/// Host-only generated bundles and strict copies share workspace's quota lock.
/// `estimated_bytes` covers bytes copied by the closure; actual usage is checked
/// again before publication. The caller retains the private directory lease.
pub fn allocate_controlled_snapshot<T>(home:&Path,run_id:&str,quota:QuotaLimits,deadline:Instant,
    estimated_bytes:u64,build:impl FnOnce(&Path)->Result<T,AttemptInfraError>,
)->Result<(tempfile::TempDir,T),AttemptInfraError> {
    allocate_associated_snapshot(home,run_id,quota,deadline,estimated_bytes,"attempt-snapshots",build)
}
pub(crate) fn allocate_associated_snapshot<T>(home:&Path,run_id:&str,quota:QuotaLimits,deadline:Instant,
    estimated_bytes:u64,association:&str,build:impl FnOnce(&Path)->Result<T,AttemptInfraError>,
)->Result<(tempfile::TempDir,T),AttemptInfraError> {
    if Instant::now()>=deadline {return Err(AttemptInfraError::BudgetExhausted);}
    if !matches!(association,"attempt-snapshots"|"retry-seeds") {return refused();}
    super::maintenance::scope_labels(home,run_id,"attempt").map_err(failure)?;
    let home=canonical_real_directory(home).map_err(failure)?;
    let root=home.join("discovery");super::workspace::create_private_directory(&root).map_err(failure)?;
    let _lock=quota_lock(&root,deadline).map_err(failure)?;
    check_quota(&root,run_id,quota,estimated_bytes).map_err(failure)?;
    let parent=root.join(association).join(run_id);super::workspace::create_private_directory(&parent).map_err(failure)?;
    let private=private_tempdir(&parent,"call-").map_err(failure)?;
    let value=build(private.path())?;
    check_quota(&root,run_id,quota,0).map_err(failure)?;
    if Instant::now()>=deadline {return Err(AttemptInfraError::BudgetExhausted);}
    Ok((private,value))
}

pub(crate) const SUPERVISOR: &str = include_str!("python/attempt_supervisor.py");
#[cfg(test)]
thread_local! {
    /// Test-only container client for runner tests on this thread.
    pub(crate) static DOCKER_PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}
fn docker_program()->std::ffi::OsString {
    #[cfg(test)]
    if let Some(program)=DOCKER_PROGRAM.with(|p|p.borrow().clone()) { return program.into_os_string(); }
    "docker".into()
}
pub struct PreparedAttemptContainer {
    pub create: Command,
    _private: tempfile::TempDir,
    guards: Vec<IntegrityGuard>,
    home: PathBuf,
    run_id: String,
    name: String,
    role: &'static str,
    deadline: Instant,
}
fn refused<T>() -> Result<T, AttemptInfraError> { Err(AttemptInfraError::IsolationUnavailable) }
fn failure(error: impl std::fmt::Display) -> AttemptInfraError { AttemptInfraError::Spawn(error.to_string()) }
pub(crate) fn safe_mount(path: &Path) -> Result<String, AttemptInfraError> {
    let value=path.to_str().ok_or(AttemptInfraError::IsolationUnavailable)?;
    if value.bytes().any(|c| matches!(c,b','|b'\n'|b'\r'|0)) { return refused(); }
    Ok(value.into())
}
fn pinned(image:&str)->bool {
    let digest=image.rsplit_once("@sha256:").map(|(_,d)|d).or_else(||image.strip_prefix("sha256:"));
    !image.bytes().any(|c|c.is_ascii_whitespace() || c==0)
        && digest.is_some_and(|d|d.len()==64 && d.bytes().all(|c|c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

/// Shares the exact sidecar used by workspace copy and retention, with a
/// bounded wait that is part of the attempt's wall deadline.
pub(crate) fn quota_lock(root:&Path, deadline:Instant)->io::Result<std::fs::File> {
    #[cfg(not(unix))]
    { let _=(root,deadline);Err(io::Error::other("quota locking unavailable")) }
    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd,unix::fs::{OpenOptionsExt,MetadataExt}};
        let file=std::fs::OpenOptions::new().create(true).read(true).write(true)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC)
            .open(root.join(".quota.lock.lock"))?;
        let meta=file.metadata()?;
        if !meta.is_file() || meta.nlink()!=1 || meta.uid()!=unsafe{libc::geteuid()} {
            return Err(io::Error::other("invalid quota lock authority"));
        }
        let until=deadline.min(Instant::now()+Duration::from_millis(500));
        loop {
            if Instant::now()>=until {return Err(io::Error::new(io::ErrorKind::TimedOut,"quota lock deadline"));}
            if unsafe{libc::flock(file.as_raw_fd(),libc::LOCK_EX|libc::LOCK_NB)}==0 {return Ok(file);}
            let error=io::Error::last_os_error();
            if error.kind()!=io::ErrorKind::WouldBlock && error.kind()!=io::ErrorKind::Interrupted {return Err(error);}
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
fn association_bytes(path:&Path)->io::Result<u64> {
    let meta=match std::fs::symlink_metadata(path) {Ok(meta)=>meta,Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok(0),Err(e)=>return Err(e)};
    if meta.file_type().is_symlink(){return Err(io::Error::other("linked quota association"));}
    if meta.is_dir(){tree_bytes(path)}else if meta.is_file(){Ok(meta.len())}else{Err(io::Error::other("special quota association"))}
}
pub(crate) fn check_quota(root:&Path,run_id:&str,limits:QuotaLimits,additional:u64)->io::Result<()> {
    let mut run=0u64;
    for kind in ["runs","artifacts","policy-development","attempt-snapshots","retry-seeds"] {
        run=run.checked_add(association_bytes(&root.join(kind).join(run_id))?).ok_or_else(||io::Error::other("quota overflow"))?;
    }
    run=run.checked_add(association_bytes(&root.join("reports").join(format!("{run_id}.json")))?).ok_or_else(||io::Error::other("quota overflow"))?;
    if limits.max_run_bytes==0 || limits.max_total_bytes==0
        || run.checked_add(additional).is_none_or(|size|size>limits.max_run_bytes)
        || tree_bytes(root)?.checked_add(additional).is_none_or(|size|size>limits.max_total_bytes) {
        return Err(io::Error::other("discovery snapshot aggregate quota exceeded"));
    }
    Ok(())
}
pub(crate) fn request_role(home:&Path,run:&Path,node:&Path,request:&AttemptRequest)->Result<&'static str,AttemptInfraError> {
    super::maintenance::scope_labels(home,&request.run_id,"attempt").map_err(failure)?;
    if run==home.join("discovery/runs").join(&request.run_id) {return Ok("attempt");}
    let parent=home.join("discovery/policy-development").join(&request.run_id);
    if run.parent()!=Some(parent.as_path()) || !run.file_name().and_then(|name|name.to_str()).is_some_and(|name|name.starts_with("session-"))
        || !request.read_workspaces.is_empty() {return refused();}
    let Some((round,revision))=request.cell_id.strip_prefix("policy-r").and_then(|id|id.split_once("-v")) else{return refused();};
    if round.parse::<u32>().ok().is_none_or(|n|n==0) || revision.parse::<u32>().ok().is_none_or(|n|n==0)
        || node!=run.join(format!("a{revision}/ws")) {return refused();}
    // Validation never repairs permissions on an existing caller path.
    super::workspace::create_private_directory(&parent).map_err(failure)?;
    super::workspace::create_private_directory(run).map_err(failure)?;
    super::workspace::create_private_directory(node).map_err(failure)?;
    Ok("policy")
}

pub fn prepare(home:&Path, request:&AttemptRequest, settings:&AttemptSettings, quota:QuotaLimits,
    runtime:&AttemptRuntimeConfig, argv:&[String], environment:&BTreeMap<String,String>, timeout:Duration,
) -> Result<PreparedAttemptContainer, AttemptInfraError> {
    prepare_with_files(home,request,settings,quota,runtime,argv,environment,&[],timeout)
}

/// Fixed names the host always writes into `/dudu-runtime`.
const RESERVED_RUNTIME_FILES: &[&str] = &["empty-mcp.json","gemini.json"];
/// Host-generated `/dudu-runtime` file paths: relative, `[A-Za-z0-9._-]`
/// components separated by `/`, no empty, `.` or `..` component, at most
/// eight levels, never one of the reserved names. Anything else is refused.
pub(crate) fn valid_runtime_file(path:&str)->bool {
    !path.is_empty() && path.len()<=256 && !RESERVED_RUNTIME_FILES.contains(&path)
        && path.split('/').count()<=8
        && path.split('/').all(|part| !part.is_empty() && part!="." && part!=".."
            && part.bytes().all(|c|c.is_ascii_alphanumeric() || matches!(c,b'.'|b'_'|b'-')))
}

/// [`prepare`] plus host-generated files for the read-only `/dudu-runtime`
/// directory. They count against the run quota and are covered by the same
/// integrity guard as the fixed configuration files.
#[allow(clippy::too_many_arguments)]
pub fn prepare_with_files(home:&Path, request:&AttemptRequest, settings:&AttemptSettings, quota:QuotaLimits,
    runtime:&AttemptRuntimeConfig, argv:&[String], environment:&BTreeMap<String,String>,
    files:&[(String,Vec<u8>)], timeout:Duration,
) -> Result<PreparedAttemptContainer, AttemptInfraError> {
    let mut names=std::collections::BTreeSet::new();
    for (path,_) in files {
        // Neither a parent of another file nor a duplicate.
        if !valid_runtime_file(path) || !names.insert(path.as_str()) { return refused(); }
    }
    if names.iter().any(|a|names.iter().any(|b|b.len()>a.len() && b.starts_with(a) && b.as_bytes()[a.len()]==b'/')) { return refused(); }
    let extra=files.iter().try_fold(0u64,|sum,(_,bytes)|sum.checked_add(bytes.len() as u64)).ok_or(AttemptInfraError::IsolationUnavailable)?;
    let deadline=Instant::now().checked_add(timeout).ok_or(AttemptInfraError::IsolationUnavailable)?;
    let absolute_deadline=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(failure)?.as_secs_f64()+timeout.as_secs_f64();
    super::maintenance::check_clean(home).map_err(failure)?;
    let home=canonical_real_directory(home).map_err(failure)?;
    let run=canonical_real_directory(&request.run_dir).map_err(failure)?;
    let node=canonical_real_directory(&request.node_dir).map_err(failure)?;
    let role=request_role(&home,&run,&node,request)?;
    if !node.starts_with(&run)
        || node.file_name()!=Some(std::ffi::OsStr::new("ws")) || timeout.is_zero()
        || !pinned(&runtime.image) || settings.memory_bytes==0 || settings.pids==0
        || settings.cpu_millis==0 || settings.tmp_bytes==0 || settings.max_snapshot_bytes==0
        || !runtime.executable.is_absolute()
        || !["/opt","/usr","/bin","/app"].iter().any(|root|runtime.executable.starts_with(root))
        || runtime.executable.starts_with(&home)
        || runtime.executable.components().any(|c|matches!(c,std::path::Component::ParentDir)) { return refused(); }
    #[cfg(unix)]
    let (uid,gid)=unsafe { (libc::geteuid(),libc::getegid()) };
    #[cfg(not(unix))]
    let (uid,gid)=(0u32,0u32);
    if uid==0 { return refused(); }
    let discovery=home.join("discovery");
    super::workspace::create_private_directory(&discovery).map_err(failure)?;
    let _quota_guard=quota_lock(&discovery,deadline).map_err(failure)?;
    let mut sources=Vec::new();let mut seen=std::collections::BTreeSet::new();let mut total=0u64;
    for original in &request.read_workspaces {
        let source=canonical_real_directory(original).map_err(failure)?;
        if !source.starts_with(&run) || source.file_name()!=Some(std::ffi::OsStr::new("ws"))
            || source==node || source.starts_with(&node) || node.starts_with(&source) { return refused(); }
        if !seen.insert(source.clone()) {continue;}
        let guard=IntegrityGuard::capture(&source).map_err(failure)?;
        total=total.checked_add(tree_bytes(&source).map_err(failure)?).ok_or(AttemptInfraError::IsolationUnavailable)?;
        if total>settings.max_snapshot_bytes {return refused();}
        sources.push((source,guard));
        if Instant::now()>=deadline {return Err(AttemptInfraError::BudgetExhausted);}
    }
    let gemini=super::attempt_adapter::gemini_settings(request.max_turns).to_string();
    let expected=total.checked_add(super::agent_spawn::EMPTY_MCP_CONFIG.len() as u64)
        .and_then(|n|n.checked_add(gemini.len() as u64)).and_then(|n|n.checked_add(extra)).ok_or(AttemptInfraError::IsolationUnavailable)?;
    check_quota(&discovery,&request.run_id,quota,expected).map_err(failure)?;
    let snapshot_parent=discovery.join("attempt-snapshots").join(&request.run_id);
    super::workspace::create_private_directory(&snapshot_parent).map_err(failure)?;
    let private=private_tempdir(&snapshot_parent,"call-").map_err(failure)?;
    let private_root=private.path().canonicalize().map_err(failure)?;
    let mut guards=Vec::new();let mut mounts=Vec::new();
    mounts.push(format!("type=bind,src={},dst={},bind-propagation=rprivate",safe_mount(&node)?,safe_mount(&node)?));
    for (index,(source,before)) in sources.into_iter().enumerate() {
        let copy=private_root.join(format!("read-{}",index+1));
        duduclaw_fork::CopyPolicy::with_excludes(std::iter::empty::<&str>()).copy_tree(&source,&copy).map_err(failure)?;
        before.verify().map_err(failure)?;
        if directory_sha256(&source).map_err(failure)?!=directory_sha256(&copy).map_err(failure)?
            || tree_bytes(&private_root).map_err(failure)?>settings.max_snapshot_bytes {return refused();}
        check_quota(&discovery,&request.run_id,quota,0).map_err(failure)?;
        if Instant::now()>=deadline {return Err(AttemptInfraError::BudgetExhausted);}
        guards.push(before);guards.push(IntegrityGuard::capture(&copy).map_err(failure)?);
        mounts.push(format!("type=bind,src={},dst={},readonly,bind-propagation=rprivate",safe_mount(&copy)?,safe_mount(&source)?));
    }
    // The agent cannot rewrite trusted system settings through its shell.
    let config=private_root.join("config");super::workspace::create_private_directory(&config).map_err(failure)?;
    std::fs::write(config.join("empty-mcp.json"),super::agent_spawn::EMPTY_MCP_CONFIG).map_err(failure)?;
    std::fs::write(config.join("gemini.json"),gemini).map_err(failure)?;
    for (path,bytes) in files {
        let target=config.join(path);
        let mut parent=config.clone();
        for part in Path::new(path).parent().into_iter().flat_map(Path::components) {
            parent.push(part);
            match std::fs::symlink_metadata(&parent) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => return refused(),
                Err(error) if error.kind()==io::ErrorKind::NotFound => super::workspace::create_private_directory(&parent).map_err(failure)?,
                Err(error) => return Err(failure(error)),
            }
        }
        #[cfg(unix)]
        {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(&target).map_err(failure)?;
            file.write_all(bytes).map_err(failure)?;
        }
        #[cfg(not(unix))]
        { let _=(target,bytes);return refused(); }
    }
    mounts.push(format!("type=bind,src={},dst=/dudu-runtime,readonly,bind-propagation=rprivate",safe_mount(&config)?));
    guards.push(IntegrityGuard::capture(&config).map_err(failure)?);
    check_quota(&discovery,&request.run_id,quota,0).map_err(failure)?;
    let name=format!("dudu-attempt-{}",uuid::Uuid::new_v4().simple());
    let mut create=Command::new(docker_program());
    create.env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs());
    // `--pull never`: the image presence check and the create must agree;
    // a missing image fails here instead of being fetched in between.
    create.args(["create","--interactive","--pull","never","--name",&name,"--read-only","--user",&format!("{uid}:{gid}"),
        "--memory",&settings.memory_bytes.to_string(),"--memory-swap",&settings.memory_bytes.to_string(),
        "--pids-limit",&settings.pids.to_string(),"--cpus",&format!("{:.3}",settings.cpu_millis as f64/1000.0),
        "--cap-drop","ALL","--security-opt","no-new-privileges:true","--tmpfs",
        // `exec`: Docker defaults --tmpfs to noexec, but HOME/TMPDIR live here
        // and CLIs extract helper binaries into them (grok tools, Bun native
        // modules, agy). The node bind mount is already exec-capable, so this
        // grants nothing new; nosuid,nodev stay.
        &format!("/tmp:rw,exec,nosuid,nodev,size={},mode=1777",settings.tmp_bytes),"--workdir",&safe_mount(&node)?]);
    for label in super::maintenance::scope_labels(&home,&request.run_id,role).map_err(failure)? { create.args(["--label",&label]); }
    for mount in mounts { create.args(["--mount",&mount]); }
    // Values are argv elements, never parsed by a host shell or logged.
    // Secrets go by name only: docker copies the value from its own
    // environment, so it never appears in the host process list.
    for (key,value) in environment {
        if !key.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'_') || key.is_empty() || value.contains('\0') { return refused(); }
        if super::attempt_adapter::is_secret_env(key) {
            create.arg("--env").arg(key);create.env(key,value);
        } else {
            create.arg("--env").arg(format!("{key}={value}"));
        }
    }
    if Instant::now()>=deadline {return Err(AttemptInfraError::BudgetExhausted);}
    create.args(["--entrypoint","python3",&runtime.image,"-I","-S","-B","-c",SUPERVISOR,&format!("{absolute_deadline:.6}")])
        .arg(&runtime.executable).args(argv);
    Ok(PreparedAttemptContainer {create,_private:private,guards,home,run_id:request.run_id.clone(),name,role,deadline})
}

/// Environment for the later `docker start` / `docker rm` clients: the
/// create command's own environment minus every secret value.
fn client_env(create:&Command)->Vec<(std::ffi::OsString,std::ffi::OsString)> {
    create.get_envs().filter(|(k,_)|!k.to_str().is_some_and(super::attempt_adapter::is_secret_env))
        .filter_map(|(k,v)|v.map(|v|(k.into(),v.into()))).collect()
}
struct Cleanup { program:std::ffi::OsString, env:Vec<(std::ffi::OsString,std::ffi::OsString)>, name:String, home:PathBuf, run:String, role:&'static str, result:Option<Result<(),String>> }
impl Cleanup {
    fn command(&self)->Command { let mut c=Command::new(&self.program);c.env_clear().envs(self.env.iter().cloned());c }
    fn stop(&mut self)->Result<(),String> {
        if let Some(result)=&self.result { return result.clone(); }
        let mut command=self.command();command.args(["rm","--force",&self.name]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)] { use std::os::unix::process::CommandExt;command.process_group(0); }
        let result:Result<(),String>=(|| {
            let mut child=command.spawn().map_err(|_|"attempt cleanup launch failed".to_owned())?;
            let until=Instant::now()+Duration::from_secs(5);
            let result=loop { match child.try_wait() {
                Ok(Some(status))=>break if status.success(){Ok(())}else{Err("attempt cleanup failed".into())},
                Ok(None) if Instant::now()<until=>std::thread::sleep(Duration::from_millis(10)),
                _=>break Err("attempt cleanup unconfirmed".into()),
            }};
            #[cfg(unix)] { let _=duduclaw_core::platform::kill_process_group(child.id()); }
            if result.is_err(){let _=child.kill();}let _=child.wait();result
        })();
        if let Err(reason)=&result { let _=super::maintenance::record_cleanup_failure(&self.home,&self.run,self.role,reason); }
        self.result=Some(result.clone());result
    }
}
impl Drop for Cleanup { fn drop(&mut self){let _=self.stop();} }
impl PreparedAttemptContainer {
    pub async fn run(self,payload:&[u8],timeout:Duration,on_event:impl FnMut(&[u8])->bool+Send)
        ->Result<super::process::ProcessOutput,AttemptInfraError> {
        let timeout=timeout.min(self.deadline.saturating_duration_since(Instant::now()));
        if timeout.is_zero() {return Err(AttemptInfraError::BudgetExhausted);}
        let started=Instant::now();
        let mut cleanup=Cleanup {program:self.create.get_program().into(),
            env:client_env(&self.create),
            name:self.name.clone(),home:self.home.clone(),run:self.run_id.clone(),role:self.role,result:None};
        let created=super::process::run(self.create,b"",timeout,1024, |_|true).await.map_err(failure)?;
        let id=created.stdout.trim();
        if !created.status.success() || created.timed_out || created.output_truncated || id.len()!=64 || !id.bytes().all(|c|c.is_ascii_hexdigit()) {
            cleanup.stop().map_err(AttemptInfraError::CleanupFailed)?;
            return Err(AttemptInfraError::Spawn("attempt create was not confirmed".into()));
        }
        let remaining=timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() { cleanup.stop().map_err(AttemptInfraError::CleanupFailed)?;return Err(AttemptInfraError::BudgetExhausted); }
        let mut start=cleanup.command();start.args(["start","--attach","--interactive",id]);
        let output=super::process::run(start,payload,remaining,256*1024,on_event).await;
        cleanup.stop().map_err(AttemptInfraError::CleanupFailed)?;
        self.guards.iter().try_for_each(IntegrityGuard::verify).map_err(failure)?;
        super::maintenance::check_clean(&self.home).map_err(AttemptInfraError::CleanupFailed)?;
        output.map_err(failure)
    }
}
#[cfg(test)]
#[path="tests_attempt_container.rs"]
mod tests;
