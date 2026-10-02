//! Docker side of the task sandbox: the per-task directory, the docker
//! availability probes, and one hardened container per task.
//!
//! The container shape mirrors a Discovery attempt
//! (`discovery/attempt_container.rs::prepare_with_files` + `run`): read-only
//! root, non-root `--user`, `--cap-drop ALL`, `no-new-privileges`, memory /
//! pids / cpu ceilings, an exec-capable `/tmp` tmpfs holding the private
//! HOME, the trusted supervisor as PID 1 with an absolute deadline, secrets
//! passed by variable NAME only, `--rm` plus a bounded `docker rm --force` on
//! every path, and `--pull never`. It does not call Discovery's `prepare`
//! (that one is bound to a Discovery run's quota, snapshot and maintenance
//! model).
//!
//! What the AI can see of the host:
//! - the workspace: a size-capped tmpfs at [`WORKSPACE`], owned by the run
//!   uid/gid and discarded with the container (nothing is written to the
//!   host disk);
//! - `/dudu-runtime`: the host-generated, read-only CLI configuration;
//! - `/agent/<entry>`: only the [`AGENT_ALLOWLIST`] entries of the agent
//!   directory, each mounted on its own and read-only. Never the directory
//!   itself: `.mcp.json` carries the agent's MCP key and identity token, and
//!   the container has a network.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::discovery::attempt_adapter::is_secret_env;
use crate::discovery::attempt_container::{SUPERVISOR, safe_mount, valid_runtime_file};
use crate::discovery::process::{self, ProcessOutput};

/// Label carried by every task-sandbox container (leftover search key).
/// Sub-labels: `.run` (run id), `.agent`, `.home` ([`home_label`]) and
/// `.deadline` (unix seconds after which the supervisor has stopped the CLI).
pub const LABEL: &str = "com.duduclaw.task-sandbox";

/// In-container path of the per-task workspace (a tmpfs).
pub const WORKSPACE: &str = "/workspace";

/// Entries of the agent directory a sandboxed task may read, mounted
/// individually and read-only at `/agent/<name>`: `(name, is_directory)`.
/// Everything else (`.mcp.json`, `.claude/`, `state/`, `agent.toml`,
/// databases, `.env*`, …) is unreachable from the container.
pub const AGENT_ALLOWLIST: &[(&str, bool)] = &[
    ("SOUL.md", false),
    ("IDENTITY.md", false),
    ("CLAUDE.md", false),
    ("AGENTS.md", false),
    ("GEMINI.md", false),
    ("CONTRACT.toml", false),
    ("SKILLS", true),
    ("wiki", true),
];

/// One read-only `/agent/<name>` bind mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentMount {
    /// Canonical host path, inside the canonical agent directory.
    pub source: PathBuf,
    pub name: &'static str,
}

/// The allowlisted entries of `agent_dir` (already canonical) that exist.
/// An entry is mounted only when it is not a symlink, has the expected
/// kind, is not a hard-linked file, and resolves directly inside the agent
/// directory; anything else is skipped (never followed) with a warning, so
/// a skipped entry can only hide data, never expose more. Paths that
/// docker could not parse fail the task.
pub(super) fn agent_mounts(agent_dir: &Path) -> Result<Vec<AgentMount>, String> {
    let mut out = Vec::new();
    for (name, is_dir) in AGENT_ALLOWLIST {
        let path = agent_dir.join(name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return Err(format!("the agent entry {name} cannot be inspected")),
        };
        let kind_ok = if *is_dir { meta.is_dir() } else { meta.is_file() };
        #[cfg(unix)]
        let hard_linked = {
            use std::os::unix::fs::MetadataExt;
            !is_dir && meta.nlink() > 1
        };
        #[cfg(not(unix))]
        let hard_linked = false;
        if meta.file_type().is_symlink() || !kind_ok || hard_linked {
            tracing::warn!(entry = name, "task sandbox: agent entry is a link or of the wrong kind; not mounted");
            continue;
        }
        let Ok(source) = path.canonicalize() else {
            return Err(format!("the agent entry {name} cannot be resolved"));
        };
        if source.parent() != Some(agent_dir) {
            tracing::warn!(entry = name, "task sandbox: agent entry resolves outside the agent directory; not mounted");
            continue;
        }
        safe_mount(&source).map_err(|_| "a mount path contains a comma or a line break".to_string())?;
        out.push(AgentMount { source, name });
    }
    Ok(out)
}

/// Stable, non-reversible label for one gateway home: the first 32 hex
/// digits of SHA-256 over the canonical home path. Lets the boot sweep touch
/// only this home's containers on a shared Docker daemon.
pub fn home_label(home: &Path) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(home.as_os_str().as_encoded_bytes());
    hex::encode(digest)[..32].to_string()
}

#[cfg(test)]
thread_local! {
    /// Test-only container client for runner tests on this thread.
    pub(crate) static DOCKER_PROGRAM: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn docker_program() -> OsString {
    #[cfg(test)]
    if let Some(program) = DOCKER_PROGRAM.with(|p| p.borrow().clone()) {
        return program.into_os_string();
    }
    "docker".into()
}

/// The docker client's own environment: the agent-CLI spawn allowlist only
/// (no gateway secrets). Secret values are added to the `create` call alone.
fn client_command() -> Command {
    let mut command = Command::new(docker_program());
    command.env_clear().envs(duduclaw_core::spawn_env::agent_cli_spawn_env_pairs());
    command
}

async fn probe(args: &[&str], timeout: Duration) -> Option<ProcessOutput> {
    let mut command = client_command();
    command.args(args);
    process::run(command, b"", timeout, 64 * 1024, |_| true).await.ok()
}

/// Run one bounded docker client command (10 s, 1 MiB of output); its stdout
/// when it exited successfully, `None` otherwise.
pub(super) async fn client_probe(args: &[&str]) -> Option<String> {
    let mut command = client_command();
    command.args(args);
    process::run(command, b"", Duration::from_secs(10), 1024 * 1024, |_| true)
        .await
        .ok()
        .filter(|o| o.status.success() && !o.timed_out && !o.output_truncated)
        .map(|o| o.stdout)
}

/// `docker version` answers with a server version: the daemon is reachable.
pub async fn docker_reachable() -> bool {
    probe(&["version", "--format", "{{.Server.Version}}"], Duration::from_secs(10))
        .await
        .is_some_and(|o| o.status.success() && !o.timed_out && !o.stdout.trim().is_empty())
}

/// `docker image inspect` finds the image locally. Never pulls.
pub async fn image_present(image: &str) -> bool {
    if !super::settings::valid_image(image) {
        return false;
    }
    probe(&["image", "inspect", "--format", "{{.Id}}", image], Duration::from_secs(10))
        .await
        .is_some_and(|o| o.status.success() && !o.timed_out && !o.stdout.trim().is_empty())
}

/// `<home>/sandbox/runs/<uuid>/config`, owner-only. Removed on drop. The
/// workspace is not here: it is a tmpfs inside the container.
pub(super) struct TaskDir {
    pub run_id: String,
    root: PathBuf,
    pub config: PathBuf,
    removed: bool,
}

impl TaskDir {
    pub fn create(home: &Path) -> io::Result<Self> {
        use crate::discovery::workspace::{canonical_real_directory, create_private_directory};
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let runs = home.join("sandbox").join("runs");
        create_private_directory(&runs)?;
        let runs = canonical_real_directory(&runs)?;
        let root = runs.join(&run_id);
        // Fresh: an existing directory of the same name is never reused.
        if std::fs::symlink_metadata(&root).is_ok() {
            return Err(io::Error::other("task sandbox directory already exists"));
        }
        let mut dir = Self { run_id, config: root.join("config"), root, removed: false };
        create_private_directory(&dir.config)?;
        dir.config = canonical_real_directory(&dir.config)?;
        Ok(dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Delete the whole per-task tree ([`remove_run_dir`]).
    pub fn remove(&mut self) -> io::Result<()> {
        if self.removed {
            return Ok(());
        }
        let result = remove_run_dir(&self.root);
        if result.is_ok() {
            self.removed = true;
        }
        result
    }
}

/// Delete one `<home>/sandbox/runs/<id>` tree. A tree with unreadable parts is
/// reopened first; links are never followed.
pub(super) fn remove_run_dir(root: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            reopen_tree(root);
            match std::fs::remove_dir_all(root) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }
}

impl Drop for TaskDir {
    fn drop(&mut self) {
        if let Err(error) = self.remove() {
            tracing::warn!(dir = %self.root.display(), %error, "task sandbox directory could not be removed");
        }
    }
}

fn reopen_tree(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else { return };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return;
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                reopen_tree(&entry.path());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Write the read-only `/dudu-runtime` files: the empty MCP config, the
/// Gemini system settings, and the family's own files (validated names,
/// no links followed, created fresh with mode 0600).
pub(super) fn write_config(config: &Path, max_turns: u32, files: &[(String, Vec<u8>)]) -> io::Result<()> {
    let gemini = crate::discovery::attempt_adapter::gemini_settings(max_turns).to_string();
    let fixed = [
        ("empty-mcp.json".to_string(), crate::discovery::agent_spawn::EMPTY_MCP_CONFIG.as_bytes().to_vec()),
        ("gemini.json".to_string(), gemini.into_bytes()),
    ];
    for (path, _) in files {
        if !valid_runtime_file(path) {
            return Err(io::Error::other("invalid sandbox runtime file name"));
        }
    }
    for (path, bytes) in fixed.iter().chain(files.iter()) {
        let target = config.join(path);
        let mut parent = config.to_path_buf();
        for part in Path::new(path).parent().into_iter().flat_map(Path::components) {
            parent.push(part);
            match std::fs::symlink_metadata(&parent) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => return Err(io::Error::other("sandbox runtime path is not a directory")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    crate::discovery::workspace::create_private_directory(&parent)?
                }
                Err(e) => return Err(e),
            }
        }
        write_new(&target, bytes)?;
    }
    Ok(())
}

fn write_new(target: &Path, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(target)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, bytes);
        Err(io::Error::other("task sandbox requires a unix host"))
    }
}

/// Everything one container needs. Host paths are already canonical.
pub(super) struct ContainerPlan<'a> {
    pub image: &'a str,
    pub executable: &'a Path,
    pub argv: &'a [String],
    pub env: &'a BTreeMap<String, String>,
    /// The allowlisted agent entries ([`agent_mounts`]).
    pub agent_files: &'a [AgentMount],
    pub config_dir: &'a Path,
    pub memory_bytes: u64,
    pub pids: u32,
    pub cpu_millis: u32,
    pub tmp_bytes: u64,
    pub workspace_bytes: u64,
    /// [`home_label`] of the gateway home.
    pub home_label: &'a str,
    pub uid: u32,
    pub gid: u32,
    pub run_id: &'a str,
    pub agent_id: &'a str,
    pub timeout: Duration,
}

/// The `docker create` command and the container name. Refuses uid 0 and
/// gid 0, zero limits, bad env names and any mount path with `,` / line breaks.
pub(super) fn build_create(plan: &ContainerPlan<'_>) -> Result<(Command, String), String> {
    let refused = |what: &str| Err(format!("sandbox container refused: {what}"));
    if plan.uid == 0 || plan.gid == 0 {
        return refused("the gateway runs as root (uid or gid 0)");
    }
    if plan.memory_bytes == 0 || plan.pids == 0 || plan.cpu_millis == 0 || plan.tmp_bytes == 0
        || plan.workspace_bytes == 0 || plan.timeout.is_zero()
    {
        return refused("a resource limit is zero");
    }
    if !super::settings::valid_image(plan.image) || !super::settings::valid_executable(plan.executable) {
        return refused("invalid image or executable");
    }
    let mount = |p: &Path| safe_mount(p).map_err(|_| "a mount path contains a comma or a line break".to_string());
    let config_dir = mount(plan.config_dir)?;
    let executable = plan.executable.to_str().ok_or_else(|| "invalid executable".to_string())?;
    let label_value = |v: &str| v.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'));
    if !label_value(plan.run_id) || !label_value(plan.agent_id) || !label_value(plan.home_label) || plan.home_label.is_empty() {
        return refused("invalid label value");
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs_f64();
    let deadline = now + plan.timeout.as_secs_f64();
    let name = format!("dudu-task-{}", plan.run_id);
    let mut create = client_command();
    create.args([
        "create", "--interactive", "--rm", "--pull", "never", "--name", &name, "--read-only",
        "--user", &format!("{}:{}", plan.uid, plan.gid),
        // The AI CLI has to reach its model provider: default bridge.
        "--network", "bridge",
        "--memory", &plan.memory_bytes.to_string(), "--memory-swap", &plan.memory_bytes.to_string(),
        "--pids-limit", &plan.pids.to_string(),
        "--cpus", &format!("{:.3}", f64::from(plan.cpu_millis) / 1000.0),
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges:true",
        // `exec`: HOME/TMPDIR live here and the CLIs extract helper binaries
        // into them (Discovery live finding L13). nosuid,nodev stay.
        "--tmpfs", &format!("/tmp:rw,exec,nosuid,nodev,size={},mode=1777", plan.tmp_bytes),
        // The workspace is discarded with the task, so it never touches the
        // host disk: a capped tmpfs owned by the run user (charged to the
        // memory cgroup — settings require tmp + workspace <= memory).
        "--tmpfs", &format!(
            "{WORKSPACE}:rw,exec,nosuid,nodev,size={},uid={},gid={},mode=0700",
            plan.workspace_bytes, plan.uid, plan.gid
        ),
        "--workdir", WORKSPACE,
    ]);
    for label in [
        format!("{LABEL}=1"),
        format!("{LABEL}.run={}", plan.run_id),
        format!("{LABEL}.agent={}", plan.agent_id),
        format!("{LABEL}.home={}", plan.home_label),
        format!("{LABEL}.deadline={}", deadline.ceil() as u64),
    ] {
        create.args(["--label", &label]);
    }
    let mut mounts = vec![format!("type=bind,src={config_dir},dst=/dudu-runtime,readonly,bind-propagation=rprivate")];
    for entry in plan.agent_files {
        if !AGENT_ALLOWLIST.iter().any(|(name, _)| *name == entry.name) {
            return refused("an agent entry outside the allowlist");
        }
        let source = mount(&entry.source)?;
        mounts.push(format!("type=bind,src={source},dst=/agent/{},readonly,bind-propagation=rprivate", entry.name));
    }
    for mount in mounts {
        create.args(["--mount", &mount]);
    }
    for (key, value) in plan.env {
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') || value.contains('\0') {
            return refused("invalid environment variable");
        }
        // Secrets by name only: docker copies the value from its own
        // environment, so it never appears in the host process list.
        if is_secret_env(key) {
            create.arg("--env").arg(key);
            create.env(key, value);
        } else {
            create.arg("--env").arg(format!("{key}={value}"));
        }
    }
    create
        .args(["--entrypoint", "python3", plan.image, "-I", "-S", "-B", "-c", SUPERVISOR, &format!("{deadline:.6}")])
        .arg(executable)
        .args(plan.argv);
    Ok((create, name))
}

/// Why a launch failed before or around the CLI run.
#[derive(Debug)]
pub(super) enum LaunchError {
    /// `docker create` did not confirm a container.
    Create(String),
    /// The `docker start` transport failed.
    Transport(String),
    /// `docker rm --force` was not confirmed.
    Cleanup(String),
}

/// Removes the container on every path. The containers carry `--rm`, so a
/// container that already exited may be gone before `docker rm --force`
/// runs; removal is confirmed when the forced remove succeeds or when no
/// container of that exact name is listed afterwards.
struct Cleanup {
    name: String,
    result: Option<Result<(), String>>,
}

/// `docker rm --force <name>` and, only when that reports failure, an exact
/// name listing that proves the container is gone. Both commands are built
/// on the caller's thread (the test client is thread-local) and run on a
/// blocking thread, never on an async worker.
struct RemoveCommands {
    remove: Command,
    confirm: Command,
}

impl RemoveCommands {
    fn new(name: &str) -> Self {
        let mut remove = client_command();
        remove.args(["rm", "--force", name]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut confirm = client_command();
        confirm
            .args(["ps", "--all", "--quiet", "--no-trunc", "--filter", &format!("name=^/{name}$")])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for command in [&mut remove, &mut confirm] {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            #[cfg(not(unix))]
            let _ = command;
        }
        Self { remove, confirm }
    }

    /// Blocking: at most ~5 s for the remove plus ~5 s for the confirmation.
    fn run(mut self) -> Result<(), String> {
        let removed = bounded_status(&mut self.remove, Duration::from_secs(5));
        match removed {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(why) => return Err(why),
        }
        // Not confirmed by the remove itself: an `--rm` container that
        // already vanished is fine, anything still listed is not.
        let child = self.confirm.spawn().map_err(|_| "sandbox cleanup could not start".to_owned())?;
        let output = bounded_output(child, Duration::from_secs(5))?;
        if output.trim().is_empty() {
            Ok(())
        } else {
            Err("sandbox container removal failed".into())
        }
    }
}

fn bounded_status(command: &mut Command, limit: Duration) -> Result<bool, String> {
    let mut child = command.spawn().map_err(|_| "sandbox cleanup could not start".to_owned())?;
    let until = Instant::now() + limit;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => break Err("sandbox container removal unconfirmed".to_owned()),
        }
    };
    #[cfg(unix)]
    {
        let _ = duduclaw_core::platform::kill_process_group(child.id());
    }
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

fn bounded_output(mut child: std::process::Child, limit: Duration) -> Result<String, String> {
    use std::io::Read;
    let mut stdout = child.stdout.take().ok_or_else(|| "sandbox cleanup output unavailable".to_owned())?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(64 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let until = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            _ => break Err("sandbox container removal unconfirmed".to_owned()),
        }
    };
    #[cfg(unix)]
    {
        let _ = duduclaw_core::platform::kill_process_group(child.id());
    }
    let _ = child.kill();
    let _ = child.wait();
    let bytes = reader.join().map_err(|_| "sandbox cleanup output unavailable".to_owned())?;
    match status {
        Ok(status) if status.success() => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        _ => Err("sandbox container removal unconfirmed".into()),
    }
}

impl Cleanup {
    async fn stop(&mut self) -> Result<(), String> {
        if let Some(result) = &self.result {
            return result.clone();
        }
        let commands = RemoveCommands::new(&self.name);
        let result = tokio::task::spawn_blocking(move || commands.run())
            .await
            .unwrap_or_else(|_| Err("sandbox cleanup task failed".into()));
        self.result = Some(result.clone());
        result
    }
}

impl Drop for Cleanup {
    /// The launch future was dropped (cancelled) before cleanup ran: remove
    /// the container on a blocking thread, never by sleeping on this one.
    fn drop(&mut self) {
        if self.result.is_some() {
            return;
        }
        let commands = RemoveCommands::new(&self.name);
        let name = self.name.clone();
        let run = move || {
            if let Err(error) = commands.run() {
                tracing::warn!(container = %name, %error, "task sandbox container could not be removed after cancellation");
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(run);
            }
            Err(_) => run(),
        }
    }
}

/// Create, start (attached, stdin = `payload`) and always remove the container.
pub(super) async fn launch(
    create: Command,
    name: String,
    payload: &[u8],
    timeout: Duration,
    on_event: impl FnMut(&[u8]) -> bool + Send,
) -> Result<ProcessOutput, LaunchError> {
    let started = Instant::now();
    let mut cleanup = Cleanup { name, result: None };
    let created = process::run(create, b"", timeout, 1024, |_| true).await;
    let created = match created {
        Ok(output) => output,
        Err(error) => {
            cleanup.stop().await.map_err(LaunchError::Cleanup)?;
            return Err(LaunchError::Create(error.to_string()));
        }
    };
    let id = created.stdout.trim().to_string();
    if !created.status.success() || created.timed_out || created.output_truncated || id.len() != 64
        || !id.bytes().all(|c| c.is_ascii_hexdigit())
    {
        cleanup.stop().await.map_err(LaunchError::Cleanup)?;
        return Err(LaunchError::Create(created.stderr));
    }
    let remaining = timeout.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        cleanup.stop().await.map_err(LaunchError::Cleanup)?;
        return Err(LaunchError::Transport("the deadline passed before the container started".into()));
    }
    let mut start = client_command();
    start.args(["start", "--attach", "--interactive", &id]);
    let output = process::run(start, payload, remaining, 256 * 1024, on_event).await;
    cleanup.stop().await.map_err(LaunchError::Cleanup)?;
    output.map_err(|e| LaunchError::Transport(e.to_string()))
}
