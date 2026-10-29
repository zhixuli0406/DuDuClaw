//! Persistent operator control of autonomous work, never of human conversation.
//!
//! A small, versioned JSON file is shared by gateway and agent schedulers without
//! opening a gateway-owned SQLite database (or introducing a dependency cycle).
//! Writers hold an exclusive admission lease, fsync a temporary file and
//! atomically replace the state. Handoffs hold shared leases. Readers see one
//! complete revision. The marker
//! distinguishes a new installation from a deleted state file: after first use,
//! missing state fails closed too. Downgrades do not enforce this new gate.
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const STATE_FILE: &str = "autonomy_pause.json";
pub const INIT_FILE: &str = "autonomy_pause.initialized";
pub const ADMISSION_LOCK_FILE: &str = "autonomy_pause.admission.lock";
const ADMISSION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
pub const MAX_REASON_CHARS: usize = 1000;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PENDING_CHANGE_RECEIPTS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyPath {
    GoalLoop,
    GoalRound,
    Dispatcher,
    Cron,
    Heartbeat,
    Responsibility,
    Autopilot,
    AssignSchedule,
    Workflow,
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PauseEntry {
    pub reason: String,
    pub who: String,
    pub at: String,
}

/// A typed scope keeps an employee named "all" distinct from a global change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ChangeScope {
    All,
    Agent(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeMethod {
    Pause,
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeReceipt {
    pub change_id: String,
    pub method: ChangeMethod,
    pub scope: ChangeScope,
    pub actor: String,
    pub request_digest: String,
    pub applied_at: String,
    pub completion_recorded: bool,
}

impl ChangeReceipt {
    pub fn agent_id(&self) -> Option<&str> {
        match &self.scope {
            ChangeScope::All => None,
            ChangeScope::Agent(id) => Some(id),
        }
    }

    /// Canonical request: compact JSON tuple [method, typed scope, actor, reason].
    /// The effective (length-limited) pause reason is included; generated times
    /// and transport-only/unknown parameters are not request fields.
    pub fn new(agent: Option<&str>, entry: Option<&PauseEntry>, actor: &str) -> io::Result<Self> {
        let method = if entry.is_some() { ChangeMethod::Pause } else { ChangeMethod::Resume };
        let scope = agent.map_or(ChangeScope::All, |id| ChangeScope::Agent(id.to_owned()));
        let canonical = serde_json::to_vec(&(method, &scope, actor, entry.map(|e| e.reason.as_str())))?;
        let receipt = Self {
            change_id: uuid::Uuid::new_v4().to_string(),
            method,
            scope,
            actor: actor.to_owned(),
            request_digest: format!("{:x}", Sha256::digest(canonical)),
            applied_at: chrono::Utc::now().to_rfc3339(),
            completion_recorded: false,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    fn validate(&self) -> io::Result<()> {
        let timestamp_ok = self.applied_at.len() <= 64
            && chrono::DateTime::parse_from_rfc3339(&self.applied_at)
                .is_ok_and(|at| (0..=253402300799).contains(&at.timestamp()));
        if self.change_id.len() != 36 || uuid::Uuid::parse_str(&self.change_id).is_err()
            || self.agent_id().is_some_and(|id| !crate::is_valid_agent_id(id))
            || self.actor.is_empty() || self.actor.len() > 256
            || self.actor.chars().any(char::is_control)
            || self.request_digest.len() != 64
            || !self.request_digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !timestamp_ok
        {
            return Err(invalid("invalid autonomy change receipt"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PauseState {
    pub global: Option<PauseEntry>,
    pub agents: BTreeMap<String, PauseEntry>,
    pub state_error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskState {
    version: u32,
    // Sole authority for completion, committed in the same rename as the change.
    // Completed entries may be pruned; pending entries must never be discarded.
    #[serde(default)]
    receipts: Vec<ChangeReceipt>,
    global: Option<PauseEntry>,
    agents: BTreeMap<String, PauseEntry>,
}

impl Default for DiskState {
    fn default() -> Self {
        Self {
            version: 1,
            receipts: Vec::new(),
            global: None,
            agents: BTreeMap::new(),
        }
    }
}

impl From<DiskState> for PauseState {
    fn from(value: DiskState) -> Self {
        Self {
            global: value.global,
            agents: value.agents,
            state_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PauseInfo {
    pub path: AutonomyPath,
    pub reason: String,
    pub entry: Option<PauseEntry>,
    pub state_error: Option<String>,
}

impl std::fmt::Display for PauseInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "autonomy paused ({:?}): {}", self.path, self.reason)
    }
}
impl std::error::Error for PauseInfo {}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(unix)]
fn validate_unix_owner_and_links(metadata: &std::fs::Metadata, euid: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != euid {
        return Err(invalid("autonomy file is not owned by the current user"));
    }
    if metadata.nlink() != 1 {
        return Err(invalid("autonomy file must have exactly one link"));
    }
    Ok(())
}

// Nonblocking/no-follow opens prevent a corrupt special file from hanging a
// Tokio worker; fstat closes the metadata-to-open replacement window.
fn open_regular(path: &Path, create: bool) -> io::Result<std::fs::File> {
    open_regular_with_metadata(path, create, std::fs::File::metadata)
}

// Inject only fstat for deterministic tests of replacement between open/chmod
// and validation. Production always inspects the opened descriptor.
fn open_regular_with_metadata(
    path: &Path,
    create: bool,
    mut metadata_of: impl FnMut(&std::fs::File) -> io::Result<std::fs::Metadata>,
) -> io::Result<std::fs::File> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && !metadata.file_type().is_file()
    {
        return Err(invalid("autonomy state is not a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    #[cfg(not(unix))]
    {
        let file = options.open(path)?;
        if !metadata_of(&file)?.is_file() {
            return Err(invalid("autonomy state is not a regular file"));
        }
        Ok(file)
    }
    #[cfg(unix)]
    {
        // Bound churn from concurrent replacement; exhaustion still fails closed.
        for _ in 0..8 {
            let file = options.open(path)?;
            let metadata = metadata_of(&file)?;
            if !metadata.is_file() {
                return Err(invalid("autonomy state is not a regular file"));
            }
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                // SAFETY: geteuid has no pointer arguments or preconditions.
                let euid = unsafe { libc::geteuid() };
                // Atomic replacement unlinks the previous inode. Never accept that
                // detached snapshot: reopen the current state and validate it afresh.
                // Markers/locks are not replaced by our writer and must stay strict.
                if !create && path.file_name().is_some_and(|name| name == STATE_FILE)
                    && metadata.uid() == euid && metadata.nlink() == 0
                {
                    continue;
                }
                validate_unix_owner_and_links(&metadata, euid)?;
                if metadata.mode() & 0o077 != 0 {
                    // File::set_permissions uses fchmod on this descriptor, not a path
                    // that could have been replaced since open. Never chmod a foreign
                    // or multiply-linked inode. Any tightening failure refuses access.
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                let secured = metadata_of(&file)?;
                if !create && path.file_name().is_some_and(|name| name == STATE_FILE)
                    && secured.uid() == euid && secured.nlink() == 0
                {
                    continue;
                }
                validate_unix_owner_and_links(&secured, euid)?;
                if secured.mode() & 0o077 != 0 {
                    return Err(invalid("autonomy file permissions are not private"));
                }
            }
            return Ok(file);
        }
        Err(invalid("autonomy state changed repeatedly while opening"))
    }
}

fn read_state(home: &Path) -> io::Result<DiskState> {
    // A nonexistent/unreadable home is not an unused installation.
    if !std::fs::metadata(home)?.is_dir() {
        return Err(invalid("autonomy home is not a directory"));
    }
    // Validate an existing marker even when the state file is present. Missing
    // markers retain the existing first-use semantics; invalid markers deny.
    match open_regular(&home.join(INIT_FILE), false) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let path = home.join(STATE_FILE);
    let file = match open_regular(&path, false) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // symlink_metadata distinguishes dangling links from no file.
            match std::fs::symlink_metadata(&path) {
                Ok(_) => return Err(invalid("autonomy state is a dangling link")),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            return match std::fs::symlink_metadata(home.join(INIT_FILE)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DiskState::default()),
                Err(e) => Err(e),
                Ok(_) => Err(invalid("initialized autonomy state is missing")),
            };
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(invalid("autonomy state is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("autonomy state exceeds size limit"));
    }
    // Include a digest in errors: changes to equally malformed JSON still log
    // once per change, without logging user reasons or file contents.
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let state: DiskState = serde_json::from_slice(&bytes).map_err(|e| {
        invalid(format!(
            "invalid autonomy state ({digest}), {:?} at {}:{}",
            e.classify(),
            e.line(),
            e.column()
        ))
    })?;
    if state.version != 1 || state.agents.keys().any(|id| !crate::is_valid_agent_id(id)) {
        return Err(invalid(format!("unsupported autonomy state ({digest})")));
    }
    if state.receipts.len() > MAX_PENDING_CHANGE_RECEIPTS {
        return Err(invalid("too many autonomy change receipts"));
    }
    let mut ids = std::collections::HashSet::new();
    for receipt in &state.receipts {
        receipt.validate()?;
        if !ids.insert(&receipt.change_id) {
            return Err(invalid("duplicate autonomy change receipt"));
        }
    }
    for entry in state.global.iter().chain(state.agents.values()) {
        if entry.reason.chars().count() > MAX_REASON_CHARS
            || chrono::DateTime::parse_from_rfc3339(&entry.at).is_err()
        {
            return Err(invalid(format!("invalid autonomy entry ({digest})")));
        }
    }
    Ok(state)
}

/// Read fresh state. An error is observable and means all autonomous work stops.
pub fn status(home: &Path) -> PauseState {
    let result = read_state(home);
    let error = result.as_ref().err().map(ToString::to_string);
    static ERRORS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    let mut seen = ERRORS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(error) = &error {
        if seen.get(home) != Some(error) {
            tracing::error!(home = %home.display(), error = %error, "autonomy state unreadable; all autonomous work paused");
            seen.insert(home.to_path_buf(), error.clone());
        }
    } else {
        seen.remove(home);
    }
    match result {
        Ok(state) => state.into(),
        Err(_) => PauseState {
            state_error: error,
            ..PauseState::default()
        },
    }
}

fn denied(
    path: AutonomyPath,
    reason: String,
    entry: Option<PauseEntry>,
    state_error: Option<String>,
) -> Box<PauseInfo> {
    Box::new(PauseInfo {
        path,
        reason,
        entry,
        state_error,
    })
}

fn check_state(state: &PauseState, path: AutonomyPath) -> Result<(), Box<PauseInfo>> {
    if let Some(error) = &state.state_error {
        return Err(denied(
            path,
            "state_unreadable".into(),
            None,
            Some(error.clone()),
        ));
    }
    if let Some(entry) = &state.global {
        return Err(denied(
            path,
            "global_pause".into(),
            Some(entry.clone()),
            None,
        ));
    }
    Ok(())
}

/// For autonomous system actions that have no employee identity.
pub fn check_global(home: &Path, path: AutonomyPath) -> Result<(), Box<PauseInfo>> {
    check_state(&status(home), path)
}

/// Resolve the caller and its authoritative ephemeral ancestors. The returned
/// order is caller first; a regular employee is always the sole identity.
/// Workspace mirrors never substitute for the host-owned org store.
pub fn effective_identities(
    home: &Path,
    agent_id: &str,
    path: AutonomyPath,
) -> Result<Vec<String>, Box<PauseInfo>> {
    if !crate::is_valid_agent_id(agent_id) {
        return Err(denied(path, "invalid_agent_id".into(), None, None));
    }
    if !agent_id.starts_with("eph-") {
        return Ok(vec![agent_id.to_owned()]);
    }
    let failure = || denied(path, "ephemeral_parent_invalid".into(), None, None);
    let org = crate::org_store::load(home);
    let root = home.join("agents/.ephemeral").canonicalize().map_err(|_| failure())?;
    let mut current = agent_id.to_owned();
    let mut identities = Vec::new();
    // Match the gateway's existing bound, including the final employee.
    for _ in 0..32 {
        if identities.contains(&current) {
            return Err(failure());
        }
        identities.push(current.clone());
        if !current.starts_with("eph-") {
            return Ok(identities);
        }
        let dir = root.join(&current).canonicalize().map_err(|_| failure())?;
        if dir == root || !dir.starts_with(&root) || !dir.join("agent.toml").is_file() {
            return Err(failure());
        }
        // Require the whole sidecar shape, as gateway EphemeralMeta does.
        #[derive(Deserialize)]
        struct ParentMetadata {
            parent: String,
            #[serde(rename = "tier")]
            _tier: String,
            #[serde(rename = "created_at")]
            _created_at: String,
            #[serde(rename = "expires_at")]
            _expires_at: String,
        }
        let text = std::fs::read_to_string(dir.join("ephemeral.toml"))
            .map_err(|_| failure())?;
        let metadata: ParentMetadata = toml::from_str(&text).map_err(|_| failure())?;
        let authority = org.get(&current).ok_or_else(failure)?;
        if !crate::is_valid_agent_id(&metadata.parent)
            || authority.reports_to != metadata.parent
        {
            return Err(failure());
        }
        current = metadata.parent;
    }
    Err(failure())
}

/// The shared admission check; callers re-check at their actual handoff boundary.
/// Lifecycle status is read live, not from the registry's potentially stale cache.
/// Direct human conversation must not call this function.
pub fn check(home: &Path, agent_id: &str, path: AutonomyPath) -> Result<(), Box<PauseInfo>> {
    let state = status(home);
    check_state(&state, path)?;
    for identity in effective_identities(home, agent_id, path)? {
        check_identity(home, &identity, path, &state)?;
    }
    Ok(())
}

fn check_identity(
    home: &Path,
    agent_id: &str,
    path: AutonomyPath,
    state: &PauseState,
) -> Result<(), Box<PauseInfo>> {
    if let Some(entry) = state.agents.get(agent_id) {
        return Err(denied(
            path,
            "agent_pause".into(),
            Some(entry.clone()),
            None,
        ));
    }
    let failure = |reason: String| denied(path, reason, None, None);
    // Ephemeral identities always use their scaffold. A normal employee path
    // with the same id must not shadow the lifecycle of the checked scaffold.
    let config = if agent_id.starts_with("eph-") {
        let root = home.join("agents/.ephemeral").canonicalize()
            .map_err(|_| failure("ephemeral_parent_invalid".into()))?;
        let dir = root.join(agent_id).canonicalize()
            .map_err(|_| failure("ephemeral_parent_invalid".into()))?;
        if dir == root || !dir.starts_with(&root) {
            return Err(failure("ephemeral_parent_invalid".into()));
        }
        dir.join("agent.toml")
    } else {
        home.join("agents").join(agent_id).join("agent.toml")
    };
    operational(&config).map_err(failure)?;
    let resolved = crate::preset::agent_resolved_path(home, agent_id);
    match std::fs::symlink_metadata(&resolved) {
        Ok(_) => operational(&resolved).map_err(failure),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(failure(format!("agent_status_unreadable: {e}"))),
    }
}

fn operational(path: &Path) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Config {
        agent: Agent,
    }
    #[derive(Deserialize)]
    struct Agent {
        status: crate::types::AgentStatus,
    }
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            "agent_missing".to_string()
        } else {
            format!("agent_status_unreadable: {e}")
        }
    })?;
    let config: Config = toml::from_str(&text)
        .map_err(|_| "agent_status_unreadable: invalid agent config".to_string())?;
    if config.agent.status.is_operational() {
        Ok(())
    } else {
        Err("agent_not_operational".into())
    }
}

/// An fd-owned cross-process admission lease. No thread-bound mutex is held
/// across awaits. Drop immediately after the process spawn or request handoff.
#[derive(Debug)]
pub struct AdmissionGuard {
    _file: std::fs::File,
    holder: Option<u64>,
}

type LocalHolders = HashMap<u64, (PathBuf, AutonomyPath)>;
static LOCAL_HOLDERS: OnceLock<Mutex<LocalHolders>> = OnceLock::new();
static NEXT_HOLDER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if let Some(id) = self.holder
            && let Ok(mut holders) = LOCAL_HOLDERS.get_or_init(Default::default).lock()
        {
            holders.remove(&id);
        }
    }
}

fn admission_timeout(home: &Path) -> io::Error {
    let mut paths = BTreeMap::<String, usize>::new();
    let holders = LOCAL_HOLDERS.get_or_init(Default::default).lock();
    if let Ok(holders) = holders {
        for (holder_home, path) in holders.values() {
            if holder_home == home {
                *paths.entry(format!("{path:?}")).or_default() += 1;
            }
        }
    }
    // Advisory diagnostics only: do not claim visibility into other processes.
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "admission_wait_timeout: local_holders={}, local_paths={paths:?}, other_process_holders=unknown",
            paths.values().sum::<usize>(),
        ),
    )
}

fn lock_admission(
    home: &Path,
    exclusive: bool,
    wait: std::time::Duration,
) -> io::Result<AdmissionGuard> {
    let file = open_regular(&home.join(ADMISSION_LOCK_FILE), true)?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        let result = if exclusive {
            fs2::FileExt::try_lock_exclusive(&file)
        } else {
            fs2::FileExt::try_lock_shared(&file)
        };
        match result {
            Ok(()) => {
                return Ok(AdmissionGuard {
                    _file: file,
                    holder: None,
                });
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {}
            Err(error) => return Err(error),
        }
        if std::time::Instant::now() >= deadline {
            return Err(admission_timeout(home));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Drain admissions before changing lifecycle status or removing an employee.
/// Never acquire this while holding a shared admission. Blocking callers only.
pub fn exclusive_barrier(home: &Path) -> io::Result<AdmissionGuard> {
    exclusive_barrier_with_wait(home, ADMISSION_WAIT)
}

fn exclusive_barrier_with_wait(home: &Path, wait: std::time::Duration) -> io::Result<AdmissionGuard> {
    lock_admission(home, true, wait).map_err(|error| {
        io::Error::new(error.kind(), format!("等待背景工作結束失敗，未變更員工狀態或移除員工：{error}"))
    })
}

/// Hold the same exclusive lease used by pause/resume across a lifecycle write.
pub fn with_exclusive_barrier<T>(home: &Path, change: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let _lease = exclusive_barrier(home)?;
    change()
}

/// Acquire off the async worker; retain the returned lease until the write ends.
pub async fn exclusive_barrier_async(home: &Path) -> io::Result<AdmissionGuard> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || exclusive_barrier(&home))
        .await.map_err(|error| io::Error::other(error.to_string()))?
}

/// Inspect the exact candidate bytes under the profile file lock. Missing or
/// invalid status is non-operational, just as it is at admission time.
pub fn lifecycle_barrier_required(before: &str, after: &str) -> bool {
    fn status(text: &str) -> Option<toml::Value> {
        text.parse::<toml::Table>().ok()?.get("agent")?.get("status").cloned()
    }
    let before = status(before);
    let after = status(after);
    before != after && after.as_ref().and_then(toml::Value::as_str) != Some("active")
}

async fn admit_scope(
    home: &Path,
    agent: Option<&str>,
    path: AutonomyPath,
) -> Result<AdmissionGuard, Box<PauseInfo>> {
    let home = home.to_path_buf();
    let agent = agent.map(str::to_owned);
    tokio::task::spawn_blocking(move || admit_scope_blocking(&home, agent.as_deref(), path))
    .await
    .map_err(|error| {
        denied(
            path,
            "admission_unavailable".into(),
            None,
            Some(error.to_string()),
        )
    })?
}

fn admit_scope_blocking(
    home: &Path,
    agent: Option<&str>,
    path: AutonomyPath,
) -> Result<AdmissionGuard, Box<PauseInfo>> {
    let mut guard = lock_admission(home, false, ADMISSION_WAIT).map_err(|error| {
        denied(
            path,
            if error.kind() == io::ErrorKind::TimedOut {
                "admission_wait_timeout".into()
            } else {
                "admission_unavailable".into()
            },
            None,
            Some(error.to_string()),
        )
    })?;
    match agent {
        Some(agent) => check(home, agent, path)?,
        None => check_global(home, path)?,
    }
    let id = NEXT_HOLDER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut holders) = LOCAL_HOLDERS.get_or_init(Default::default).lock() {
        holders.insert(id, (home.to_path_buf(), path));
        guard.holder = Some(id);
    }
    Ok(guard)
}

/// Synchronous admission for callers already executing on a blocking worker.
/// Shares all identity resolution, checks and lease semantics with `admit`.
/// Do not call this on an async runtime worker thread.
pub fn admit_blocking(
    home: &Path,
    agent_id: &str,
    path: AutonomyPath,
) -> Result<AdmissionGuard, Box<PauseInfo>> {
    admit_scope_blocking(home, Some(agent_id), path)
}

/// Take one bounded home-wide shared lease and check every effective identity
/// while holding it. The same lease excludes pauses of the caller, any parent,
/// and the global scope; taking separate locks per identity is unnecessary.
pub async fn admit(
    home: &Path,
    agent_id: &str,
    path: AutonomyPath,
) -> Result<AdmissionGuard, Box<PauseInfo>> {
    admit_scope(home, Some(agent_id), path).await
}

/// Admission for system work without an employee identity.
pub async fn admit_global(home: &Path, path: AutonomyPath) -> Result<AdmissionGuard, Box<PauseInfo>> {
    admit_scope(home, None, path).await
}

/// Rename is the commit point. A later fsync/readback failure must never be
/// reported as though the requested pause/resume did not take effect.
#[derive(Debug)]
pub enum MutationOutcome {
    NotApplied(io::Error),
    Applied(PauseState),
    AppliedButDurabilityUnknown(PauseState, io::Error),
}

impl MutationOutcome {
    fn into_applied_result(self) -> io::Result<PauseState> {
        match self {
            Self::NotApplied(error) => Err(error),
            Self::Applied(state) => Ok(state),
            Self::AppliedButDurabilityUnknown(state, error) => {
                tracing::error!(%error, "autonomy state applied; durability unknown");
                Ok(state)
            }
        }
    }
}

/// Production mutation entry: blocking filesystem work and bounded exclusive
/// acquisition never occupy a Tokio worker. The lease spans the rename/readback.
pub async fn mutate_async(
    home: &Path,
    agent_id: Option<&str>,
    entry: Option<PauseEntry>,
) -> MutationOutcome {
    let home = home.to_path_buf();
    let agent_id = agent_id.map(str::to_owned);
    match tokio::task::spawn_blocking(move || mutate(&home, agent_id.as_deref(), entry)).await {
        Ok(outcome) => outcome,
        Err(error) => MutationOutcome::NotApplied(io::Error::other(error.to_string())),
    }
}

/// Read only the bounded state file; audit history is never receipt authority.
pub fn change_receipts(home: &Path) -> io::Result<Vec<ChangeReceipt>> {
    read_state(home).map(|state| state.receipts)
}

/// Record successful completion without changing the current pause scope.
pub fn mark_completion_recorded(home: &Path, change_id: &str) -> io::Result<()> {
    mark_completion_with_durability(home, change_id, || sync_and_read_state(home))
}

fn sync_and_read_state(home: &Path) -> io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(home)?.sync_all()?;
    read_state(home).map(|_| ())
}

fn mark_completion_with_durability(
    home: &Path,
    change_id: &str,
    durability: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let _lease = lock_admission(home, true, ADMISSION_WAIT)?;
    let mut state = read_state(home)?;
    let index = state.receipts.iter().position(|r| r.change_id == change_id)
        .ok_or_else(|| invalid("autonomy change receipt missing"))?;
    if state.receipts[index].completion_recorded { return Ok(()); }
    state.receipts[index].completion_recorded = true;
    let mut renamed = false;
    let result = write_state(home, &state, || renamed = true, durability);
    if let Err(error) = result {
        if renamed {
            // Keep the exclusive lease: do not overwrite a concurrent mutation
            // when restoring pending after a post-rename durability failure.
            state.receipts[index].completion_recorded = false;
            if let Err(rollback) = write_state(home, &state, || {}, || sync_and_read_state(home)) {
                tracing::error!(%error, %rollback, %change_id,
                    "autonomy completion acknowledgement and pending restore failed");
                return Err(io::Error::other("autonomy completion durability unknown; pending restore failed"));
            }
        }
        return Err(error);
    }
    Ok(())
}

/// Gateway mutation with a self-contained durable receipt.
pub async fn mutate_tracked_async(
    home: &Path,
    entry: Option<PauseEntry>,
    receipt: ChangeReceipt,
) -> MutationOutcome {
    let home = home.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        let agent = receipt.agent_id().map(str::to_owned);
        mutate_with_receipt(&home, agent.as_deref(), entry, Some(receipt), ADMISSION_WAIT, || {
            #[cfg(unix)]
            std::fs::File::open(&home)?.sync_all()?;
            read_state(&home).map(|_| ())
        })
    }).await {
        Ok(outcome) => outcome,
        Err(error) => MutationOutcome::NotApplied(io::Error::other(error.to_string())),
    }
}

pub fn set_pause(
    home: &Path,
    agent_id: Option<&str>,
    reason: &str,
    who: &str,
) -> io::Result<PauseState> {
    mutate(
        home,
        agent_id,
        Some(PauseEntry {
            reason: crate::truncate_chars(reason, MAX_REASON_CHARS).to_string(),
            who: who.to_string(),
            at: chrono::Utc::now().to_rfc3339(),
        }),
    )
    .into_applied_result()
}

pub fn resume(home: &Path, agent_id: Option<&str>) -> io::Result<PauseState> {
    mutate(home, agent_id, None).into_applied_result()
}

fn mutate(home: &Path, agent_id: Option<&str>, entry: Option<PauseEntry>) -> MutationOutcome {
    mutate_with_durability(home, agent_id, entry, ADMISSION_WAIT, || {
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
        read_state(home).map(|_| ())
    })
}

// The injected post-rename step makes commit-point failures testable without
// process-global failpoints that could interfere with other tests.
fn mutate_with_durability(
    home: &Path,
    agent_id: Option<&str>,
    entry: Option<PauseEntry>,
    wait: std::time::Duration,
    durability: impl FnOnce() -> io::Result<()>,
) -> MutationOutcome {
    mutate_with_receipt(home, agent_id, entry, None, wait, durability)
}

fn mutate_with_receipt(
    home: &Path,
    agent_id: Option<&str>,
    entry: Option<PauseEntry>,
    receipt: Option<ChangeReceipt>,
    wait: std::time::Duration,
    durability: impl FnOnce() -> io::Result<()>,
) -> MutationOutcome {
    let mut landed = None;
    let result = (|| -> io::Result<()> {
        if agent_id.is_some_and(|id| !crate::is_valid_agent_id(id)) {
            return Err(invalid("invalid agent id"));
        }
        // This exclusive lease is also the writer serialization lock. Do not
        // nest the old unbounded with_file_lock inside a bounded admission wait.
        let _lease = lock_admission(home, true, wait)?;
        let mut state = read_state(home)?;
        if let Some(mut receipt) = receipt {
            receipt.validate()?;
            let expected = ChangeReceipt::new(agent_id, entry.as_ref(), &receipt.actor)?;
            if receipt.completion_recorded || receipt.method != expected.method
                || receipt.scope != expected.scope || receipt.request_digest != expected.request_digest
                || entry.as_ref().is_some_and(|e| e.who != receipt.actor)
                || state.receipts.iter().any(|r| r.change_id == receipt.change_id)
            {
                return Err(invalid("inconsistent autonomy change receipt"));
            }
            if state.receipts.len() >= MAX_PENDING_CHANGE_RECEIPTS {
                let oldest = state.receipts.iter().enumerate()
                    .filter(|(_, r)| r.completion_recorded)
                    .min_by_key(|(_, r)| chrono::DateTime::parse_from_rfc3339(&r.applied_at).ok())
                    .map(|(index, _)| index)
                    .ok_or_else(|| invalid("autonomy completion receipts require reconciliation"))?;
                state.receipts.remove(oldest);
            }
            receipt.applied_at = chrono::Utc::now().to_rfc3339();
            state.receipts.push(receipt);
        }
        match agent_id {
            Some(id) => match entry {
                Some(entry) => {
                    state.agents.insert(id.into(), entry);
                }
                None => {
                    state.agents.remove(id);
                }
            },
            None => state.global = entry,
        }
        write_state(home, &state, || landed = Some(PauseState {
            global: state.global.clone(), agents: state.agents.clone(), state_error: None,
        }), durability)
    })();
    match (result, landed) {
        (Ok(()), Some(state)) => MutationOutcome::Applied(state),
        (Err(error), Some(state)) => MutationOutcome::AppliedButDurabilityUnknown(state, error),
        (Err(error), None) => MutationOutcome::NotApplied(error),
        (Ok(()), None) => MutationOutcome::NotApplied(invalid("autonomy state was not committed")),
    }
}

// Both mutation and completion acknowledgement share the locked atomic writer.
// The caller holds the exclusive admission lease throughout this operation.
fn write_state(
    home: &Path,
    state: &DiskState,
    landed: impl FnOnce(),
    durability: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("autonomy state exceeds size limit"));
    }
    let marker = open_regular(&home.join(INIT_FILE), true)?;
    marker.sync_all()?;
    let temp = home.join(format!("autonomy_pause.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, home.join(STATE_FILE))?;
        landed();
        durability()
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temp); }
    result
}

#[cfg(test)]
mod tests;
