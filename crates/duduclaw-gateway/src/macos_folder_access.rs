//! RM-155 — macOS folder access (TCC) after an update.
//!
//! ## The problem
//!
//! macOS asks again for "Files and Folders" consent whenever the responsible
//! binary changes identity — today that is every update of the unsigned
//! gateway, because the launchd LaunchAgent runs the gateway binary directly
//! (`duduclaw_core::autostart`), which makes the gateway the *responsible
//! process* for every CLI and `mcp-server` it spawns. When nobody is at the
//! Mac to click **Allow**, an AI employee's read of `~/Documents/...` either
//! fails with `EPERM` or — in a GUI login session while the consent prompt is
//! on screen — simply **blocks**, and the channel reply runs into its 30
//! minute timeout with a generic "處理逾時" message that tells nobody anything.
//!
//! ## What is known (Apple docs / developer forums)
//!
//! * A TCC refusal surfaces as `EPERM` (errno 1), distinct from a POSIX mode
//!   refusal (`EACCES`, errno 13).
//! * While the consent prompt is showing, the access may block instead of
//!   failing.
//! * `access(2)` does not consult TCC — only a real open/read does.
//! * There is no API to *query* TCC state without triggering it.
//!
//! ## What this module does
//!
//! * [`is_tcc_protected`] / [`classify`] — pure, compiled and tested on every
//!   platform.
//! * A **probe**: a dedicated `std::thread` doing `read_dir` + the first entry,
//!   waited on for [`PROBE_TIMEOUT`]. Deliberately *not* `spawn_blocking`: a
//!   probe stuck on a consent prompt may never return, and parking it in the
//!   blocking pool would eat a slot shared by everything else. At most one
//!   probe is in flight per path; a stuck thread is detached and its path
//!   stays [`AccessState::PendingPrompt`] until the thread finishes (which is
//!   exactly when someone clicked Allow/Don't Allow — the thread then asks for
//!   an immediate re-probe).
//! * A per-`DUDUCLAW_HOME` process-wide [`Registry`] holding the last state of
//!   every protected probe target, read by `system.status`, the doctor card
//!   and the channel-reply timeout message.
//! * [`run_watcher`] — the gateway-start probe pass (covers the restart after
//!   an update), re-run on [`request_reprobe`] (dashboard saved
//!   `[files] allowed_roots`, a protected dispatch `work_dir`, a late probe
//!   completion) and every [`REPROBE_INTERVAL`] while anything is degraded.
//!   The watcher is only spawned on macOS (see `server.rs`).
//! * [`run_guarded`] — the same "dedicated thread + bounded wait" shape for
//!   the employee file tools (`duduclaw-cli/src/mcp_files.rs`), through a
//!   shared [`OpLimiter`]: at most [`MAX_FILE_OP_THREADS`] threads alive and
//!   one per path, so stuck consent prompts cannot leak threads (over the
//!   limit the call answers "pending" without spawning), plus the zh-TW +
//!   English error texts they return.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

/// How long a background probe waits before calling the path "pending".
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Re-probe cadence while any target is not granted.
pub const REPROBE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How long an employee file-tool filesystem call may take before the tool
/// gives up and explains why (a consent prompt can block forever).
pub const FILE_OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Public guide linked from the banner, the doctor card and every message.
pub const DOCS_URL: &str =
    "https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/macos-folder-access.md";

/// The Settings path, spelled the way macOS shows it in each language.
pub const SETTINGS_PATH_ZH: &str = "系統設定 › 隱私權與安全性 › 檔案和資料夾";
pub const SETTINGS_PATH_EN: &str = "System Settings › Privacy & Security › Files and Folders";
pub const SETTINGS_PATH_JA: &str = "システム設定 › プライバシーとセキュリティ › ファイルとフォルダ";

/// errno values (identical on macOS and Linux).
const EPERM: i32 = 1;
const EACCES: i32 = 13;

// ── Pure classification ─────────────────────────────────────────────────────

/// What one probe (or one file operation) told us about a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessState {
    /// The read completed.
    Granted,
    /// `EPERM` — macOS privacy (TCC) refused the read.
    Denied,
    /// `EACCES` — ordinary POSIX permissions refused the read. Not a TCC
    /// problem; reported for completeness, never as the TCC banner.
    PosixDenied,
    /// The read did not return within the wait — macOS is (most likely)
    /// showing a consent prompt nobody has answered yet.
    PendingPrompt,
    /// Any other failure (missing path, I/O error, probe thread lost). Says
    /// nothing about TCC.
    Unavailable,
}

impl AccessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::PosixDenied => "posix_denied",
            Self::PendingPrompt => "pending_prompt",
            Self::Unavailable => "unavailable",
        }
    }

    /// The two states that mean "macOS privacy is in the way".
    pub fn is_tcc_degraded(self) -> bool {
        matches!(self, Self::Denied | Self::PendingPrompt)
    }
}

/// The result of an operation run under a bounded wait.
#[derive(Debug)]
pub enum ProbeOutcome<T> {
    Completed(io::Result<T>),
    TimedOut,
    /// Not started: an earlier operation on the same path is still blocked,
    /// or [`MAX_FILE_OP_THREADS`] operations are already outstanding. No
    /// thread was spawned.
    Pending,
}

/// Classify a finished-or-timed-out operation.
pub fn classify<T>(outcome: &ProbeOutcome<T>) -> AccessState {
    match outcome {
        ProbeOutcome::TimedOut | ProbeOutcome::Pending => AccessState::PendingPrompt,
        ProbeOutcome::Completed(Ok(_)) => AccessState::Granted,
        ProbeOutcome::Completed(Err(e)) => classify_error(e),
    }
}

/// Classify an I/O error. `EPERM` is checked before the generic
/// `PermissionDenied` kind because Rust maps both errnos onto that kind.
pub fn classify_error(e: &io::Error) -> AccessState {
    if cfg!(unix) {
        match e.raw_os_error() {
            Some(EPERM) => return AccessState::Denied,
            Some(EACCES) => return AccessState::PosixDenied,
            _ => {}
        }
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        AccessState::PosixDenied
    } else {
        AccessState::Unavailable
    }
}

/// The worst TCC state across a set: any pending prompt wins (someone can fix
/// it right now by clicking), then any denial, else granted. POSIX refusals
/// and unavailable paths do not degrade the TCC summary.
pub fn overall(states: impl IntoIterator<Item = AccessState>) -> AccessState {
    let mut out = AccessState::Granted;
    for s in states {
        match s {
            AccessState::PendingPrompt => return AccessState::PendingPrompt,
            AccessState::Denied => out = AccessState::Denied,
            _ => {}
        }
    }
    out
}

/// Lexically normalized `Normal` components (`.` dropped, `..` pops). No
/// filesystem access — touching a protected path is exactly what must not
/// happen here.
fn normal_components(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Case-insensitive component equality (the default APFS volume is
/// case-insensitive, so `~/documents` is the same protected folder).
fn same_component(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether `path` sits in a location macOS guards with a Files-and-Folders
/// (TCC) consent: `~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive
/// (`~/Library/Mobile Documents`), File Provider storage
/// (`~/Library/CloudStorage`), removable/network volumes (`/Volumes/*`) and
/// automounted network shares (`/Network/*`).
///
/// Pure and lexical: symlinks are not resolved (resolving would touch the
/// filesystem). `home` is the macOS user's home, not `DUDUCLAW_HOME`.
pub fn is_tcc_protected(path: &Path, home: &Path) -> bool {
    if !path.has_root() {
        return false;
    }
    let comps = normal_components(path);
    if let Some(first) = comps.first() {
        if comps.len() >= 2 && (same_component(first, "Volumes") || same_component(first, "Network")) {
            return true;
        }
    }
    let home_comps = normal_components(home);
    if home_comps.is_empty() || !home.has_root() || comps.len() <= home_comps.len() {
        return false;
    }
    if !comps.iter().zip(&home_comps).all(|(a, b)| same_component(a, b)) {
        return false;
    }
    let rest = &comps[home_comps.len()..];
    let first = rest[0].as_str();
    if ["Desktop", "Documents", "Downloads"].iter().any(|p| same_component(first, p)) {
        return true;
    }
    same_component(first, "Library")
        && rest.get(1).is_some_and(|second| {
            same_component(second, "Mobile Documents") || same_component(second, "CloudStorage")
        })
}

/// `~/…` for paths under the user's home (no account name in UI copy), the
/// plain path otherwise.
pub fn path_label(path: &Path, user_home: Option<&Path>) -> String {
    if let Some(home) = user_home {
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// The macOS user's home directory.
pub fn user_home() -> Option<PathBuf> {
    dirs::home_dir()
}

// ── Bounded operations ──────────────────────────────────────────────────────

/// Most file-operation threads alive at once per process — running, or stuck
/// on a consent prompt nobody answers. A stuck call cannot be cancelled, so
/// without this cap every retried tool call would leak one more thread.
pub const MAX_FILE_OP_THREADS: usize = 16;
/// Most probe threads alive at once (one per distinct path, see
/// [`Registry::probe_one`]).
pub const MAX_PROBE_THREADS: usize = 16;

/// Bounded runner for filesystem calls that may block forever: at most `cap`
/// outstanding threads, at most one per path. Over either limit the call
/// returns [`ProbeOutcome::Pending`] without spawning anything.
pub struct OpLimiter {
    cap: usize,
    state: Mutex<LimiterState>,
}

#[derive(Default)]
struct LimiterState {
    live: usize,
    paths: BTreeSet<PathBuf>,
}

/// Releases a limiter slot when the operation finishes — including by panic,
/// and also when the thread could not be spawned.
struct LimiterSlot {
    limiter: &'static OpLimiter,
    key: PathBuf,
}

impl Drop for LimiterSlot {
    fn drop(&mut self) {
        let mut s = self.limiter.lock();
        s.live = s.live.saturating_sub(1);
        s.paths.remove(&self.key);
    }
}

impl OpLimiter {
    pub const fn new(cap: usize) -> Self {
        Self { cap, state: Mutex::new(LimiterState { live: 0, paths: BTreeSet::new() }) }
    }

    fn lock(&self) -> MutexGuard<'_, LimiterState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Threads currently outstanding (running or stuck).
    pub fn outstanding(&self) -> usize {
        self.lock().live
    }

    /// Run `op` on a dedicated thread (de-duplicated by `key`) and wait at
    /// most `timeout`. On timeout the thread keeps its slot until the call
    /// returns (someone answered the prompt), and its result is dropped.
    pub fn run<T, F>(&'static self, key: &Path, timeout: Duration, op: F) -> ProbeOutcome<T>
    where
        T: Send + 'static,
        F: FnOnce() -> io::Result<T> + Send + 'static,
    {
        {
            let mut s = self.lock();
            if s.live >= self.cap || s.paths.contains(key) {
                return ProbeOutcome::Pending;
            }
            s.live += 1;
            s.paths.insert(key.to_path_buf());
        }
        let slot = LimiterSlot { limiter: self, key: key.to_path_buf() };
        let (tx, rx) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("duduclaw-fs-guard".into())
            .spawn(move || {
                let result = op();
                // Free the slot BEFORE the caller can see the result, so an
                // immediate follow-up call on the same path is not "pending".
                drop(slot);
                let _ = tx.send(result);
            });
        if let Err(e) = spawned {
            return ProbeOutcome::Completed(Err(e));
        }
        match rx.recv_timeout(timeout) {
            Ok(result) => ProbeOutcome::Completed(result),
            Err(mpsc::RecvTimeoutError::Timeout) => ProbeOutcome::TimedOut,
            Err(mpsc::RecvTimeoutError::Disconnected) => ProbeOutcome::Completed(Err(io::Error::other(
                "filesystem operation thread ended without a result",
            ))),
        }
    }
}

/// The process-wide limiter for the employee file tools.
static FILE_OPS: OpLimiter = OpLimiter::new(MAX_FILE_OP_THREADS);

/// Run one employee file-tool call through the shared [`OpLimiter`]: bounded
/// threads, one in flight per `key` path, at most `timeout` of waiting. Used
/// by the employee file tools, which are synchronous.
pub fn run_guarded<T, F>(key: &Path, timeout: Duration, op: F) -> ProbeOutcome<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    FILE_OPS.run(key, timeout, op)
}

/// The probe operation: open the directory and read its first entry. Opening
/// is what makes macOS consult TCC; `access(2)`/`stat` would not.
pub type ProbeOp = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

pub fn default_probe_op() -> ProbeOp {
    Arc::new(|path: &Path| {
        let mut entries = std::fs::read_dir(path)?;
        if let Some(first) = entries.next() {
            first?;
        }
        Ok(())
    })
}

// ── Registry ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Inner {
    states: BTreeMap<PathBuf, AccessState>,
    in_flight: HashSet<PathBuf>,
    /// Protected work dirs seen at dispatch time (`note_work_dir`).
    extra_targets: BTreeSet<PathBuf>,
    /// The TCC summary admins were last told about (`None` = never told;
    /// treated as granted so a healthy first pass sends nothing).
    last_notified: Option<AccessState>,
}

/// Probe state for one `DUDUCLAW_HOME`.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<Inner>,
    reprobe: tokio::sync::Notify,
}

/// What one probe pass changed.
#[derive(Debug, Clone, PartialEq)]
pub struct PassReport {
    /// Any per-path state (or the target set) differs from the last pass.
    pub changed: bool,
    pub before: AccessState,
    pub after: AccessState,
    /// `Some(state)` when the TCC summary differs from what admins were last
    /// told — the only time a channel notice goes out.
    pub notify: Option<AccessState>,
    /// The non-granted paths after this pass.
    pub degraded: Vec<(PathBuf, AccessState)>,
}

/// Removes a path from the in-flight set when the probe thread finishes —
/// including by panic.
struct InFlightGuard {
    registry: Arc<Registry>,
    path: PathBuf,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.registry.lock().in_flight.remove(&self.path);
    }
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ask the watcher for a probe pass now.
    pub fn request_reprobe(&self) {
        self.reprobe.notify_one();
    }

    pub fn overall(&self) -> AccessState {
        overall(self.lock().states.values().copied())
    }

    pub fn states(&self) -> BTreeMap<PathBuf, AccessState> {
        self.lock().states.clone()
    }

    fn extra_targets(&self) -> BTreeSet<PathBuf> {
        self.lock().extra_targets.clone()
    }

    /// Probe one path on a dedicated thread. A path whose previous probe is
    /// still stuck is reported `PendingPrompt` without starting another.
    pub async fn probe_one(
        self: &Arc<Self>,
        path: PathBuf,
        op: ProbeOp,
        timeout: Duration,
    ) -> AccessState {
        // Bound to a statement so the guard is gone before any await. Over
        // the global cap nothing is spawned either (stuck probes on many
        // distinct paths must not grow without bound).
        let started = {
            let mut g = self.lock();
            g.in_flight.len() < MAX_PROBE_THREADS && g.in_flight.insert(path.clone())
        };
        if !started {
            return AccessState::PendingPrompt;
        }
        let (tx, rx) = tokio::sync::oneshot::channel::<io::Result<()>>();
        let guard = InFlightGuard {
            registry: Arc::clone(self),
            path: path.clone(),
        };
        let notify_late = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("duduclaw-tcc-probe".into())
            .spawn(move || {
                // The guard drops (clearing the in-flight mark) before the
                // send, so a caller woken by the result never sees it stale.
                let result = {
                    let in_flight = guard;
                    op(&in_flight.path)
                };
                // The receiver is gone ⇒ the wait timed out and this path was
                // reported pending. The answer has now arrived (someone
                // clicked), so ask for a fresh pass instead of waiting for
                // the 10-minute timer.
                if tx.send(result).is_err() {
                    notify_late.request_reprobe();
                }
            });
        if let Err(e) = spawned {
            self.lock().in_flight.remove(&path);
            tracing::warn!(error = %e, "macos-folder-access: could not start probe thread");
            return AccessState::Unavailable;
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => classify(&ProbeOutcome::Completed(result)),
            Ok(Err(_)) => AccessState::Unavailable,
            Err(_) => AccessState::PendingPrompt,
        }
    }

    /// Probe every target concurrently and record the new states.
    pub async fn probe_targets(
        self: &Arc<Self>,
        targets: &[PathBuf],
        op: &ProbeOp,
        timeout: Duration,
    ) -> PassReport {
        let mut set = tokio::task::JoinSet::new();
        for target in targets {
            let registry = Arc::clone(self);
            let op = Arc::clone(op);
            let target = target.clone();
            set.spawn(async move {
                let state = registry.probe_one(target.clone(), op, timeout).await;
                (target, state)
            });
        }
        let mut fresh = BTreeMap::new();
        while let Some(joined) = set.join_next().await {
            if let Ok((path, state)) = joined {
                fresh.insert(path, state);
            }
        }
        self.apply(fresh)
    }

    fn apply(&self, fresh: BTreeMap<PathBuf, AccessState>) -> PassReport {
        let mut g = self.lock();
        let before = overall(g.states.values().copied());
        let changed = g.states != fresh;
        g.states = fresh;
        let after = overall(g.states.values().copied());
        let told = g.last_notified.unwrap_or(AccessState::Granted);
        let notify = (after != told).then_some(after);
        g.last_notified = Some(after);
        let degraded = g
            .states
            .iter()
            .filter(|(_, s)| **s != AccessState::Granted)
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        PassReport {
            changed,
            before,
            after,
            notify,
            degraded,
        }
    }

    /// `system.status.folder_access` — `None` unless TCC is in the way.
    pub fn status_json(&self, user_home: Option<&Path>) -> Option<Value> {
        let states = self.states();
        let summary = overall(states.values().copied());
        if !summary.is_tcc_degraded() {
            return None;
        }
        let paths: Vec<Value> = states
            .iter()
            .filter(|(_, s)| **s != AccessState::Granted)
            .map(|(p, s)| json!({ "path_label": path_label(p, user_home), "state": s.as_str() }))
            .collect();
        Some(json!({ "state": summary.as_str(), "paths": paths, "docs_url": DOCS_URL }))
    }

    /// The `macos_folder_access` doctor card, or `None` when no protected
    /// location is in use (nothing to say).
    pub fn doctor_check(&self, user_home: Option<&Path>) -> Option<Value> {
        let states = self.states();
        if states.is_empty() {
            return None;
        }
        let summary = overall(states.values().copied());
        let list = |want: AccessState| -> Vec<String> {
            states
                .iter()
                .filter(|(_, s)| **s == want)
                .map(|(p, _)| path_label(p, user_home))
                .collect()
        };
        let pending = list(AccessState::PendingPrompt);
        let denied = list(AccessState::Denied);
        let posix = list(AccessState::PosixDenied);
        let (status, message) = if summary == AccessState::Granted && posix.is_empty() {
            (
                "pass",
                format!(
                    "DuDuClaw 可以讀取所有位於 macOS 保護位置的資料夾（{} 個）。",
                    states.len()
                ),
            )
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !pending.is_empty() {
                parts.push(format!(
                    "macOS 正在等待有人在這台 Mac 上按「允許」：{}。",
                    pending.join("、")
                ));
            }
            if !denied.is_empty() {
                parts.push(format!(
                    "macOS 拒絕了 DuDuClaw 讀取：{}。請到「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應的資料夾。",
                    denied.join("、")
                ));
            }
            if !posix.is_empty() {
                parts.push(format!(
                    "檔案權限不足（不是 macOS 隱私設定造成）：{}。請檢查資料夾擁有者與讀取權限。",
                    posix.join("、")
                ));
            }
            parts.push(format!("在授權之前，AI 員工讀取這些資料夾裡的檔案會失敗或卡住。說明：{DOCS_URL}"));
            ("warn", parts.join(" "))
        };
        Some(json!({
            "name": "macos_folder_access",
            "status": status,
            "message": message,
            "can_repair": false,
        }))
    }

    /// Remember a protected work dir and ask for a pass when it is new.
    pub fn note_target(&self, path: PathBuf) {
        let inserted = self.lock().extra_targets.insert(path);
        if inserted {
            self.request_reprobe();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_states_for_test(&self, states: BTreeMap<PathBuf, AccessState>) {
        self.lock().states = states;
    }
}

fn registries() -> &'static Mutex<HashMap<PathBuf, Arc<Registry>>> {
    static REGISTRIES: OnceLock<Mutex<HashMap<PathBuf, Arc<Registry>>>> = OnceLock::new();
    REGISTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The process-wide registry for one `DUDUCLAW_HOME` (created on first use).
pub fn registry_for(home: &Path) -> Arc<Registry> {
    let mut map = registries().lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(map.entry(home.to_path_buf()).or_default())
}

fn existing_registry(home: &Path) -> Option<Arc<Registry>> {
    registries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(home)
        .cloned()
}

/// Ask for a probe pass now (dashboard saved `[files] allowed_roots`, …).
/// Harmless when no watcher runs (non-macOS): the permit is simply kept.
pub fn request_reprobe(home: &Path) {
    registry_for(home).request_reprobe();
}

/// A dispatch ran in `work_dir`; probe it if it is a protected location.
pub fn note_work_dir(home: &Path, work_dir: &Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let Some(user_home) = user_home() else { return };
    if is_tcc_protected(work_dir, &user_home) {
        registry_for(home).note_target(work_dir.to_path_buf());
    }
}

/// `system.status.folder_access` for this home, or `None` when healthy.
pub fn status_json(home: &Path) -> Option<Value> {
    existing_registry(home)?.status_json(user_home().as_deref())
}

/// The doctor card for this home.
pub fn doctor_check(home: &Path) -> Option<Value> {
    existing_registry(home)?.doctor_check(user_home().as_deref())
}

/// The current TCC summary for this home (`Granted` when never probed).
pub fn current_state(home: &Path) -> AccessState {
    existing_registry(home)
        .map(|r| r.overall())
        .unwrap_or(AccessState::Granted)
}

// ── Probe targets ───────────────────────────────────────────────────────────

/// `config.toml [files] allowed_roots`, absolute entries only — the same
/// reading `duduclaw-cli/src/mcp_files.rs::configured_roots` applies.
pub fn configured_file_roots(home: &Path) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read_to_string(home.join("config.toml")) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return Vec::new();
    };
    doc.get("files")
        .and_then(|v| v.get("allowed_roots"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// The protected locations worth probing: `DUDUCLAW_HOME` itself, every
/// `[files] allowed_roots` entry and every protected dispatch work dir seen
/// so far. Unprotected ones are dropped — probing them proves nothing.
pub fn collect_targets(
    duduclaw_home: &Path,
    user_home: &Path,
    extra: &BTreeSet<PathBuf>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let candidates = std::iter::once(duduclaw_home.to_path_buf())
        .chain(configured_file_roots(duduclaw_home))
        .chain(extra.iter().cloned());
    for c in candidates {
        if is_tcc_protected(&c, user_home) && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

// ── Watcher ─────────────────────────────────────────────────────────────────

/// Probe at start, on request and every [`REPROBE_INTERVAL`] while degraded.
/// `on_pass` runs whenever a pass changed something or a notice is due.
pub async fn run_watcher<F, Fut>(home: PathBuf, mut on_pass: F)
where
    F: FnMut(PassReport) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let Some(user_home) = user_home() else {
        return;
    };
    let registry = registry_for(&home);
    let op = default_probe_op();
    loop {
        let targets = collect_targets(&home, &user_home, &registry.extra_targets());
        let report = registry.probe_targets(&targets, &op, PROBE_TIMEOUT).await;
        if report.changed || report.notify.is_some() {
            tracing::info!(
                from = report.before.as_str(),
                to = report.after.as_str(),
                degraded = report.degraded.len(),
                "macos-folder-access: probe pass changed state"
            );
            on_pass(report).await;
        }
        if registry.overall() == AccessState::Granted {
            registry.reprobe.notified().await;
        } else {
            tokio::select! {
                _ = registry.reprobe.notified() => {}
                _ = tokio::time::sleep(REPROBE_INTERVAL) => {}
            }
        }
        // Coalesce a burst (several late probe completions, a config save).
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ── Admin notices ───────────────────────────────────────────────────────────

/// zh-TW channel notice for a TCC summary transition. `None` for states that
/// never warrant one.
pub fn admin_notice_text(state: AccessState, labels: &[String]) -> Option<String> {
    let prefix = crate::decision_notify::reason_prefix(crate::decision_action::DecisionSource::FolderAccess);
    let list = if labels.is_empty() {
        String::new()
    } else {
        format!(
            "\n{}",
            labels.iter().map(|l| format!("• {l}")).collect::<Vec<_>>().join("\n")
        )
    };
    match state {
        AccessState::PendingPrompt => Some(format!(
            "{prefix}\nmacOS 正在等待有人在這台 Mac 上按「允許」，讓 DuDuClaw 讀取下列資料夾：{list}\n\
             請到這台 Mac 前按「允許」；若已錯過提示，請到「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應資料夾。\n\
             在授權之前，AI 員工讀取這些資料夾的檔案會失敗或卡住。\n說明：{DOCS_URL}"
        )),
        AccessState::Denied => Some(format!(
            "{prefix}\nmacOS 拒絕了 DuDuClaw 讀取下列資料夾：{list}\n\
             請在這台 Mac 打開「{SETTINGS_PATH_ZH}」，為 duduclaw 開啟對應資料夾。\n\
             在授權之前，AI 員工讀取這些資料夾的檔案會失敗。\n說明：{DOCS_URL}"
        )),
        AccessState::Granted => Some(format!(
            "{prefix}\n✅ macOS 資料夾存取已恢復，AI 員工可以再次讀取受保護資料夾裡的檔案。"
        )),
        AccessState::PosixDenied | AccessState::Unavailable => None,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const NOTICE_NO_BUTTON_HINT: &str = "這則通知不需要按鈕操作；請在這台 Mac 上處理，或打開儀表板查看狀態。";

/// Verified channel identities of every Active Admin.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn admin_links(home: &Path) -> Vec<(String, String)> {
    use duduclaw_auth::models::{UserRole, UserStatus};
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return Vec::new();
    };
    let Ok(users) = db.list_users() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in users {
        if u.status != UserStatus::Active || u.role != UserRole::Admin {
            continue;
        }
        if let Ok(idents) = db.verified_channels_for_user(&u.id) {
            for i in idents {
                out.push((i.channel, i.channel_user_id));
            }
        }
    }
    out
}

/// Push the transition notice to admins' linked channels. Called only when
/// [`PassReport::notify`] is set, so the same state is never repeated.
/// Quiet hours apply through the card source's notify level.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) async fn notify_admins(home: &Path, report: &PassReport) {
    let Some(state) = report.notify else { return };
    let user_home = user_home();
    let labels: Vec<String> = report
        .degraded
        .iter()
        .filter(|(_, s)| s.is_tcc_degraded())
        .map(|(p, _)| path_label(p, user_home.as_deref()))
        .collect();
    let Some(body) = admin_notice_text(state, &labels) else { return };
    let targets = admin_links(home);
    if targets.is_empty() {
        tracing::info!("macos-folder-access: no admin with a verified channel; dashboard only");
        return;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let link = crate::deep_link::deep_link(home, crate::deep_link::DeepLinkKind::System, "");
    let card = crate::notify_push::NotifyCard {
        source: crate::decision_action::DecisionSource::FolderAccess,
        decision_id: &id,
        body: &body,
        link: link.as_deref(),
        no_button_hint: NOTICE_NO_BUTTON_HINT,
    };
    let receipt = crate::notify_push::push(
        home,
        &card,
        &crate::notify_push::NotifyDest::LinkedUsers { targets },
    )
    .await;
    tracing::info!(
        state = state.as_str(),
        attempted = receipt.attempted,
        delivered = receipt.delivered.is_some(),
        "macos-folder-access: admin notice pushed"
    );
}

// ── User-facing texts ───────────────────────────────────────────────────────

/// Employee file-tool error for a failed filesystem call. `protected` is
/// "macOS, and the path is in a TCC-protected location". `generic` is the
/// caller's own wording for every other failure.
pub fn file_error_text(raw: &str, err: &io::Error, protected: bool, generic: &str) -> String {
    if protected && classify_error(err) == AccessState::Denied {
        return format!(
            "無法讀取「{raw}」：macOS 尚未允許 DuDuClaw 存取這個資料夾（隱私權的「檔案和資料夾」權限）。\
             請在這台 Mac 打開「{SETTINGS_PATH_ZH}」，為 duduclaw 開啟對應的資料夾（例如「文件」「桌面」「下載項目」或外接磁碟），然後再試一次。\n\
             Cannot read \"{raw}\": macOS has not allowed DuDuClaw to access this folder (the Files and Folders privacy permission). \
             On this Mac, open {SETTINGS_PATH_EN}, turn on the matching folder for duduclaw (Documents, Desktop, Downloads or the external volume), then try again.\n\
             {DOCS_URL}"
        );
    }
    generic.to_string()
}

/// Employee file-tool error when a filesystem call did not return in time.
pub fn file_timeout_text(raw: &str, protected: bool, timeout: Duration) -> String {
    if protected {
        return format!(
            "讀取「{raw}」逾時：macOS 正在等待有人在這台 Mac 上按「允許」，授權 DuDuClaw 存取這個資料夾。\
             請到這台 Mac 前按「允許」後再試；若沒有看到提示，請到「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應的資料夾。\n\
             Reading \"{raw}\" timed out: macOS is waiting for someone on this Mac to click Allow so DuDuClaw can access this folder. \
             Click Allow on this Mac and try again; if no prompt is visible, enable duduclaw under {SETTINGS_PATH_EN}.\n\
             {DOCS_URL}"
        );
    }
    let secs = timeout.as_secs().max(1);
    format!(
        "讀取「{raw}」逾時（{secs} 秒內沒有回應），檔案所在的磁碟可能沒有回應。\n\
         Reading \"{raw}\" timed out after {secs} s; the disk holding this file may not be responding."
    )
}

/// Employee file-tool error when the call was not even started: an earlier
/// call on the same path is still blocked, or too many calls are.
pub fn file_pending_text(raw: &str, protected: bool) -> String {
    if protected {
        return format!(
            "尚未讀取「{raw}」：先前對這個位置的讀取仍在等待 macOS 授權（有人需要在這台 Mac 上按「允許」）。\
             請到這台 Mac 前按「允許」後再試；若沒有看到提示，請到「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應的資料夾。\n\
             \"{raw}\" was not read: an earlier read of this location is still waiting for macOS permission (someone needs to click Allow on this Mac). \
             Click Allow on this Mac and try again; if no prompt is visible, enable duduclaw under {SETTINGS_PATH_EN}.\n\
             {DOCS_URL}"
        );
    }
    format!(
        "尚未讀取「{raw}」：先前的檔案讀取仍沒有回應，請稍後再試。\n\
         \"{raw}\" was not read: earlier file reads are still not responding; try again later."
    )
}

/// The channel-reply timeout explanation in `language`, for a TCC summary.
/// `None` when the state is not a TCC problem.
pub fn timeout_text_for_state(state: AccessState, agent: &str, language: Option<&str>) -> Option<String> {
    let pending = match state {
        AccessState::PendingPrompt => true,
        AccessState::Denied => false,
        _ => return None,
    };
    let text = match (language.unwrap_or("zh-TW"), pending) {
        ("en", true) => format!(
            "{agent} timed out because macOS is waiting for someone on this Mac to click Allow so DuDuClaw can read a protected folder. \
             Click Allow on this Mac (or enable duduclaw under {SETTINGS_PATH_EN}), then send the message again."
        ),
        ("en", false) => format!(
            "{agent} timed out because macOS is not allowing DuDuClaw to read a protected folder. \
             On this Mac, enable duduclaw under {SETTINGS_PATH_EN}, then send the message again."
        ),
        ("ja" | "ja-JP", true) => format!(
            "{agent} の応答が時間切れになりました。macOS が保護されたフォルダへのアクセス許可を待っています。\
             この Mac で「許可」をクリックするか、「{SETTINGS_PATH_JA}」で duduclaw をオンにしてから、もう一度送信してください。"
        ),
        ("ja" | "ja-JP", false) => format!(
            "{agent} の応答が時間切れになりました。macOS が保護されたフォルダへのアクセスを許可していません。\
             この Mac の「{SETTINGS_PATH_JA}」で duduclaw をオンにしてから、もう一度送信してください。"
        ),
        (_, true) => format!(
            "{agent} 這次處理逾時：macOS 正在等待有人在這台 Mac 上按「允許」，讓 DuDuClaw 讀取受保護的資料夾。\
             請到這台 Mac 前按「允許」（或到「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應資料夾），再重新送出訊息。"
        ),
        (_, false) => format!(
            "{agent} 這次處理逾時：macOS 沒有允許 DuDuClaw 讀取受保護的資料夾。\
             請在這台 Mac 打開「{SETTINGS_PATH_ZH}」為 duduclaw 開啟對應資料夾，再重新送出訊息。"
        ),
    };
    Some(text)
}

/// [`timeout_text_for_state`] for this home's current state.
pub fn channel_timeout_explanation(home: &Path, agent: &str, language: Option<&str>) -> Option<String> {
    timeout_text_for_state(current_state(home), agent, language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn home() -> PathBuf {
        PathBuf::from("/Users/amy")
    }

    // ── path detection ──

    #[test]
    fn protected_home_folders_are_detected() {
        let h = home();
        for p in [
            "/Users/amy/Desktop",
            "/Users/amy/Documents",
            "/Users/amy/Documents/clients/a.csv",
            "/Users/amy/Downloads/x",
            "/Users/amy/Library/Mobile Documents/com~apple~CloudDocs/x",
            "/Users/amy/Library/CloudStorage/OneDrive-Personal/x",
            "/Users/amy/documents/case-insensitive",
            "/Users/amy/./Documents/dot",
            "/Volumes/USB/data",
            "/Volumes/share",
            "/Network/Servers/nas/x",
        ] {
            assert!(is_tcc_protected(Path::new(p), &h), "{p} must be protected");
        }
    }

    #[test]
    fn ordinary_locations_are_not_protected() {
        let h = home();
        for p in [
            "/Users/amy",
            "/Users/amy/.duduclaw",
            "/Users/amy/DuDuClaw-files/a.csv",
            "/Users/amy/Library/Application Support/x",
            "/Users/amy/Library",
            "/Users/amy/Documentsx",
            "/Users/bob/Documents",
            "/Volumes",
            "/srv/exports",
            "/tmp/x",
            "relative/Documents",
            "/Users/amy/Documents/../DuDuClaw-files",
        ] {
            assert!(!is_tcc_protected(Path::new(p), &h), "{p} must not be protected");
        }
    }

    #[test]
    fn path_label_hides_the_account_name() {
        let h = home();
        assert_eq!(path_label(Path::new("/Users/amy/Documents/x"), Some(&h)), "~/Documents/x");
        assert_eq!(path_label(Path::new("/Volumes/USB"), Some(&h)), "/Volumes/USB");
        assert_eq!(path_label(Path::new("/Users/amy"), Some(&h)), "~");
    }

    // ── classify ──

    #[cfg(unix)]
    #[test]
    fn eperm_is_tcc_and_eacces_is_posix() {
        let eperm: ProbeOutcome<()> = ProbeOutcome::Completed(Err(io::Error::from_raw_os_error(EPERM)));
        let eacces: ProbeOutcome<()> = ProbeOutcome::Completed(Err(io::Error::from_raw_os_error(EACCES)));
        assert_eq!(classify(&eperm), AccessState::Denied);
        assert_eq!(classify(&eacces), AccessState::PosixDenied);
    }

    #[test]
    fn classify_covers_success_timeout_and_other_errors() {
        assert_eq!(classify(&ProbeOutcome::Completed(Ok(()))), AccessState::Granted);
        assert_eq!(classify::<()>(&ProbeOutcome::TimedOut), AccessState::PendingPrompt);
        let missing: ProbeOutcome<()> =
            ProbeOutcome::Completed(Err(io::Error::from(io::ErrorKind::NotFound)));
        assert_eq!(classify(&missing), AccessState::Unavailable);
        let kind_only: ProbeOutcome<()> =
            ProbeOutcome::Completed(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
        assert_eq!(classify(&kind_only), AccessState::PosixDenied);
    }

    #[test]
    fn overall_prefers_pending_then_denied() {
        use AccessState::*;
        assert_eq!(overall([Granted, PosixDenied, Unavailable]), Granted);
        assert_eq!(overall([Granted, Denied]), Denied);
        assert_eq!(overall([Denied, PendingPrompt, Granted]), PendingPrompt);
        assert_eq!(overall(std::iter::empty()), Granted);
    }

    // ── bounded operations ──

    fn limiter(cap: usize) -> &'static OpLimiter {
        Box::leak(Box::new(OpLimiter::new(cap)))
    }

    #[test]
    fn run_guarded_returns_a_fast_result() {
        let out = run_guarded(Path::new("/tmp/fast"), Duration::from_secs(2), || Ok(7));
        assert!(matches!(out, ProbeOutcome::Completed(Ok(7))));
        let l = limiter(2);
        for _ in 0..5 {
            // Sequential calls on one path never see their own finished slot.
            let out = l.run(Path::new("/a"), Duration::from_secs(2), || Ok(1));
            assert!(matches!(out, ProbeOutcome::Completed(Ok(1))));
        }
        assert_eq!(l.outstanding(), 0);
    }

    #[test]
    fn run_guarded_reports_a_sleeping_op_as_timed_out() {
        let out = limiter(2).run(Path::new("/a"), Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        });
        assert!(matches!(out, ProbeOutcome::TimedOut));
        assert_eq!(classify(&out), AccessState::PendingPrompt);
    }

    #[test]
    fn stuck_ops_are_deduplicated_per_path_and_capped_globally() {
        let l = limiter(2);
        let started = Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let stuck = |started: &Arc<AtomicUsize>, rx: &Arc<Mutex<mpsc::Receiver<()>>>| {
            let (started, rx) = (Arc::clone(started), Arc::clone(rx));
            move || {
                started.fetch_add(1, Ordering::SeqCst);
                let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
                Ok::<(), io::Error>(())
            }
        };
        let short = Duration::from_millis(30);
        assert!(matches!(l.run(Path::new("/a"), short, stuck(&started, &release_rx)), ProbeOutcome::TimedOut));
        // Same path while the first is stuck: pending, no new thread.
        assert!(matches!(l.run(Path::new("/a"), short, stuck(&started, &release_rx)), ProbeOutcome::Pending));
        assert!(matches!(l.run(Path::new("/b"), short, stuck(&started, &release_rx)), ProbeOutcome::TimedOut));
        // Global cap reached: even a fresh path is pending, nothing spawned.
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let out = l.run(Path::new("/c"), short, move || { r.fetch_add(1, Ordering::SeqCst); Ok(()) });
        assert!(matches!(out, ProbeOutcome::Pending));
        assert_eq!(classify(&out), AccessState::PendingPrompt);
        assert_eq!(ran.load(Ordering::SeqCst), 0);
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert_eq!(l.outstanding(), 2);
        // Answering the prompts frees the slots.
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while l.outstanding() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(l.outstanding(), 0);
        assert!(matches!(l.run(Path::new("/a"), Duration::from_secs(2), || Ok(())), ProbeOutcome::Completed(Ok(()))));
    }

    #[test]
    fn a_panicking_op_releases_its_slot() {
        let l = limiter(1);
        let out = l.run::<(), _>(Path::new("/p"), Duration::from_secs(2), || panic!("boom"));
        assert!(matches!(out, ProbeOutcome::Completed(Err(_))));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while l.outstanding() > 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(l.outstanding(), 0);
    }

    #[tokio::test]
    async fn probe_threads_are_capped_across_distinct_paths() {
        let reg = Arc::new(Registry::default());
        let sleepy: ProbeOp = Arc::new(|_| {
            std::thread::sleep(Duration::from_millis(400));
            Ok(())
        });
        for i in 0..MAX_PROBE_THREADS {
            let state = reg.probe_one(PathBuf::from(format!("/Volumes/p{i}")), Arc::clone(&sleepy), Duration::from_millis(10)).await;
            assert_eq!(state, AccessState::PendingPrompt);
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let counted: ProbeOp = Arc::new(move |_| { c.fetch_add(1, Ordering::SeqCst); Ok(()) });
        let over = reg.probe_one(PathBuf::from("/Volumes/over"), counted, Duration::from_secs(1)).await;
        assert_eq!(over, AccessState::PendingPrompt);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no thread over the cap");
        assert!(reg.lock().in_flight.len() <= MAX_PROBE_THREADS);
    }

    #[tokio::test]
    async fn probe_one_classifies_with_an_injected_op() {
        let reg = Arc::new(Registry::default());
        let ok: ProbeOp = Arc::new(|_| Ok(()));
        assert_eq!(
            reg.probe_one(PathBuf::from("/Volumes/a"), ok, Duration::from_secs(2)).await,
            AccessState::Granted
        );
        #[cfg(unix)]
        {
            let eperm: ProbeOp = Arc::new(|_| Err(io::Error::from_raw_os_error(EPERM)));
            assert_eq!(
                reg.probe_one(PathBuf::from("/Volumes/b"), eperm, Duration::from_secs(2)).await,
                AccessState::Denied
            );
        }
    }

    #[tokio::test]
    async fn a_stuck_probe_stays_pending_and_never_doubles_up() {
        let reg = Arc::new(Registry::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let sleepy: ProbeOp = Arc::new(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(800));
            Ok(())
        });
        let p = PathBuf::from("/Volumes/stuck");
        let first = reg.probe_one(p.clone(), Arc::clone(&sleepy), Duration::from_millis(50)).await;
        assert_eq!(first, AccessState::PendingPrompt);
        // Still in flight: reported pending without a second thread.
        let second = reg.probe_one(p.clone(), Arc::clone(&sleepy), Duration::from_millis(50)).await;
        assert_eq!(second, AccessState::PendingPrompt);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "at most one in-flight probe per path");
        // The late completion clears the in-flight mark and asks for a pass.
        tokio::time::timeout(Duration::from_secs(3), reg.reprobe.notified())
            .await
            .expect("late completion must request a re-probe");
        assert!(!reg.lock().in_flight.contains(&p));
    }

    #[tokio::test]
    async fn notices_fire_only_on_summary_change() {
        let reg = Arc::new(Registry::default());
        let targets = vec![PathBuf::from("/Volumes/a")];
        let granted: ProbeOp = Arc::new(|_| Ok(()));
        let first = reg.probe_targets(&targets, &granted, Duration::from_secs(2)).await;
        assert_eq!(first.notify, None, "a healthy first pass says nothing");
        assert!(first.changed);

        #[cfg(unix)]
        {
            let denied: ProbeOp = Arc::new(|_| Err(io::Error::from_raw_os_error(EPERM)));
            let r = reg.probe_targets(&targets, &denied, Duration::from_secs(2)).await;
            assert_eq!(r.notify, Some(AccessState::Denied));
            let again = reg.probe_targets(&targets, &denied, Duration::from_secs(2)).await;
            assert_eq!(again.notify, None, "never repeat the same state");
            assert!(!again.changed);
            let back = reg.probe_targets(&targets, &granted, Duration::from_secs(2)).await;
            assert_eq!(back.notify, Some(AccessState::Granted));
        }
    }

    // ── surfaces ──

    #[test]
    fn status_json_only_when_degraded() {
        let reg = Registry::default();
        let h = home();
        assert!(reg.status_json(Some(&h)).is_none());
        reg.set_states_for_test(BTreeMap::from([
            (PathBuf::from("/Users/amy/Documents/x"), AccessState::PendingPrompt),
            (PathBuf::from("/Volumes/USB"), AccessState::Granted),
        ]));
        let v = reg.status_json(Some(&h)).unwrap();
        assert_eq!(v["state"], "pending_prompt");
        assert_eq!(v["paths"].as_array().unwrap().len(), 1);
        assert_eq!(v["paths"][0]["path_label"], "~/Documents/x");
        assert_eq!(v["paths"][0]["state"], "pending_prompt");
        reg.set_states_for_test(BTreeMap::from([(PathBuf::from("/Volumes/USB"), AccessState::PosixDenied)]));
        assert!(reg.status_json(Some(&h)).is_none(), "POSIX refusals are not the TCC banner");
    }

    #[test]
    fn doctor_card_passes_warns_or_is_absent() {
        let reg = Registry::default();
        assert!(reg.doctor_check(None).is_none());
        reg.set_states_for_test(BTreeMap::from([(PathBuf::from("/Volumes/USB"), AccessState::Granted)]));
        assert_eq!(reg.doctor_check(None).unwrap()["status"], "pass");
        reg.set_states_for_test(BTreeMap::from([(PathBuf::from("/Volumes/USB"), AccessState::Denied)]));
        let card = reg.doctor_check(None).unwrap();
        assert_eq!(card["name"], "macos_folder_access");
        assert_eq!(card["status"], "warn");
        assert!(card["message"].as_str().unwrap().contains(SETTINGS_PATH_ZH));
    }

    // Unix-only: `[files] allowed_roots` drops entries that are not absolute
    // on the host, and `/Users/...` is not absolute on Windows.
    #[cfg(unix)]
    #[test]
    fn collect_targets_keeps_only_protected_locations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[files]\nallowed_roots = [\"/Users/amy/Documents/clients\", \"/srv/exports\", \"rel\"]\n",
        )
        .unwrap();
        let extra = BTreeSet::from([PathBuf::from("/Volumes/NAS/team"), PathBuf::from("/opt/work")]);
        let t = collect_targets(dir.path(), &home(), &extra);
        assert_eq!(
            t,
            vec![PathBuf::from("/Users/amy/Documents/clients"), PathBuf::from("/Volumes/NAS/team")]
        );
    }

    #[test]
    fn file_texts_name_the_permission_and_the_steps() {
        #[cfg(unix)]
        {
            let eperm = io::Error::from_raw_os_error(EPERM);
            let t = file_error_text("~/Documents/a.csv", &eperm, true, "generic");
            assert!(t.contains(SETTINGS_PATH_ZH) && t.contains(SETTINGS_PATH_EN), "{t}");
            assert!(t.contains("macOS"));
            assert_eq!(file_error_text("x", &eperm, false, "generic"), "generic");
        }
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(file_error_text("x", &missing, true, "generic"), "generic");
        let t = file_timeout_text("~/Documents/a.csv", true, FILE_OP_TIMEOUT);
        assert!(t.contains("click Allow") && t.contains("按「允許」"), "{t}");
        let plain = file_timeout_text("/srv/a.csv", false, FILE_OP_TIMEOUT);
        assert!(!plain.contains("macOS") && plain.contains("10"), "{plain}");
    }

    #[test]
    fn channel_timeout_text_only_for_tcc_states() {
        assert!(timeout_text_for_state(AccessState::Granted, "A", None).is_none());
        assert!(timeout_text_for_state(AccessState::PosixDenied, "A", None).is_none());
        for lang in [None, Some("en"), Some("ja-JP")] {
            let t = timeout_text_for_state(AccessState::PendingPrompt, "A", lang).unwrap();
            assert!(t.contains("macOS"), "{t}");
            assert!(timeout_text_for_state(AccessState::Denied, "A", lang).unwrap().contains("duduclaw"));
        }
        // An unprobed home is treated as healthy.
        let dir = tempfile::tempdir().unwrap();
        assert!(channel_timeout_explanation(dir.path(), "A", None).is_none());
    }

    #[test]
    fn admin_notice_texts() {
        let labels = vec!["~/Documents".to_string()];
        let pending = admin_notice_text(AccessState::PendingPrompt, &labels).unwrap();
        assert!(pending.contains("~/Documents") && pending.contains("允許"));
        assert!(admin_notice_text(AccessState::Denied, &labels).unwrap().contains(SETTINGS_PATH_ZH));
        assert!(admin_notice_text(AccessState::Granted, &[]).unwrap().contains("恢復"));
        assert!(admin_notice_text(AccessState::PosixDenied, &labels).is_none());
    }
}
