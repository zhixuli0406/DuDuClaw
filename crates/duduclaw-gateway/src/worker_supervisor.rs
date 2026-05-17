//! Phase 7 — supervisor for an out-of-process `duduclaw-cli-worker` subprocess.
//!
//! When `[runtime] worker_managed = true` is set in the agent's config,
//! the gateway boots a child `duduclaw-cli-worker` process and routes all
//! PtyPool invocations through it via HTTP+JSON-RPC. The supervisor:
//!
//! 1. Resolves the worker binary (`which duduclaw-cli-worker` or sibling
//!    of the gateway binary).
//! 2. Spawns it with `--bind` (loopback ephemeral) + `--home-dir` (the
//!    gateway's home) + `DUDUCLAW_WORKER_TOKEN` env var.
//! 3. Polls `GET /healthz` until it returns 200 or a boot timeout fires.
//! 4. Runs a background health-check loop (30 s tick). After N (default 3)
//!    consecutive misses it kills the child and respawns.
//! 5. On gateway shutdown it sends SIGTERM and reaps.
//!
//! The supervisor is **best-effort**: any failure to spawn the worker
//! lands the gateway in a degraded state where `acquire_and_invoke` returns
//! `Err(...)` until the next restart attempt succeeds. Callers (the
//! channel_reply / dispatcher wedges) already fall back to legacy
//! `tokio::process::Command` when PTY routing errors, so a missing worker
//! is recoverable rather than fatal.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use duduclaw_cli_worker::{TokenStore, WorkerClient};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Default values for the supervisor knobs. All overridable via config.
pub const DEFAULT_BIND: &str = "127.0.0.1:9876";
pub const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_HEALTH_INTERVAL_SECS: u64 = 30;
pub const DEFAULT_MAX_HEALTH_MISSES: u32 = 3;
pub const DEFAULT_RESTART_BACKOFF_MS: u64 = 1_000;

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub home_dir: PathBuf,
    /// Worker bind address — MUST be loopback.
    pub bind: SocketAddr,
    /// Optional override path for the `duduclaw-cli-worker` binary.
    /// When None, [`resolve_worker_bin`] is consulted.
    pub binary_override: Option<PathBuf>,
    pub boot_timeout: Duration,
    pub health_interval: Duration,
    pub max_health_misses: u32,
    pub restart_backoff: Duration,
}

impl SupervisorConfig {
    pub fn new(home_dir: PathBuf) -> Self {
        Self {
            home_dir,
            bind: DEFAULT_BIND.parse().expect("static literal must parse"),
            binary_override: None,
            boot_timeout: Duration::from_secs(DEFAULT_BOOT_TIMEOUT_SECS),
            health_interval: Duration::from_secs(DEFAULT_HEALTH_INTERVAL_SECS),
            max_health_misses: DEFAULT_MAX_HEALTH_MISSES,
            restart_backoff: Duration::from_millis(DEFAULT_RESTART_BACKOFF_MS),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SupervisorError {
    #[error("could not resolve `duduclaw-cli-worker` binary on PATH")]
    BinaryNotFound,
    #[error("worker token store: {0}")]
    TokenStore(String),
    #[error("worker subprocess spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("worker boot timed out after {0:?}")]
    BootTimeout(Duration),
}

/// Live supervisor handle. Cloning shares one supervisor across the
/// gateway; `shutdown` is idempotent across clones.
#[derive(Clone)]
pub struct WorkerSupervisorHandle {
    inner: Arc<SupervisorInner>,
}

struct SupervisorInner {
    config: SupervisorConfig,
    token: String,
    client: WorkerClient,
    child: Mutex<Option<Child>>,
    shutdown: CancellationToken,
    binary: PathBuf,
}

impl WorkerSupervisorHandle {
    /// Spawn the worker subprocess + wait for its healthz endpoint to
    /// reply 200 within the boot timeout. Returns a handle whose
    /// background health-check task is already running.
    pub async fn spawn(config: SupervisorConfig) -> Result<Self, SupervisorError> {
        let binary = match &config.binary_override {
            Some(p) => p.clone(),
            None => resolve_worker_bin().ok_or(SupervisorError::BinaryNotFound)?,
        };
        let token_store = TokenStore::new(&config.home_dir);
        let token = token_store
            .load_or_generate()
            .map_err(|e| SupervisorError::TokenStore(e.to_string()))?;

        let child = spawn_child(&binary, &config, &token).await?;
        let base_url = format!("http://{}", config.bind);
        let client = WorkerClient::new(&base_url, &token).map_err(|e| {
            SupervisorError::TokenStore(format!("WorkerClient::new: {e}"))
        })?;

        // Wait for the worker's /healthz to come back before declaring the
        // boot complete. We poll every 200 ms for `boot_timeout`.
        //
        // Initial boot has no `inner` yet (the SupervisorInner is built
        // below), so the shutdown token isn't observable here. That's
        // fine — at this point there's no detached background task
        // whose lifetime would be affected by a slow boot.
        wait_for_healthy(&client, config.boot_timeout, None).await?;

        info!(
            bind = %config.bind,
            binary = %binary.display(),
            "worker_supervisor: spawned + healthy"
        );

        let inner = Arc::new(SupervisorInner {
            config: config.clone(),
            token,
            client: client.clone(),
            child: Mutex::new(Some(child)),
            shutdown: CancellationToken::new(),
            binary,
        });

        let handle = Self {
            inner: inner.clone(),
        };
        tokio::spawn(health_check_loop(inner));
        Ok(handle)
    }

    pub fn client(&self) -> WorkerClient {
        self.inner.client.clone()
    }

    pub fn bind(&self) -> SocketAddr {
        self.inner.config.bind
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// Trigger graceful shutdown of the supervisor + child. Idempotent.
    pub async fn shutdown(&self) {
        if self.inner.shutdown.is_cancelled() {
            return;
        }
        self.inner.shutdown.cancel();
        let mut guard = self.inner.child.lock().await;
        if let Some(mut child) = guard.take() {
            kill_child_gracefully(&mut child).await;
        }
        info!("worker_supervisor: shutdown complete");
    }
}

async fn spawn_child(
    binary: &Path,
    config: &SupervisorConfig,
    token: &str,
) -> Result<Child, SupervisorError> {
    let mut cmd = tokio::process::Command::new(binary);
    // **Round 3 security fix (MED-M5)**: drop the parent env entirely
    // and pass only an allowlisted subset. Previously the worker
    // subprocess inherited every env var of the gateway (including
    // `ANTHROPIC_API_KEY`, `DUDUCLAW_DISABLE_PTY_POOL`, debug
    // tooling, etc.) — that's a broader-than-necessary blast radius
    // and a footgun if a future code change makes the worker
    // sensitive to a gateway-only env var.
    //
    // Allowlisted passthrough:
    // - `RUST_LOG` so operators can debug the worker with the same
    //   filter as the gateway.
    // - `PATH` because the worker resolves `claude` via PATH.
    // - `HOME` because `dirs::home_dir()` reads it on Unix.
    // - Locale vars so claude's TUI renders properly (`LANG`,
    //   `LC_ALL`, `LC_CTYPE`).
    // - `TERM` and `TERMINFO` so portable-pty can size the child's
    //   terminal.
    // - macOS keychain access requires `__CF_USER_TEXT_ENCODING`
    //   (Cocoa init) — pass that too on macOS.
    cmd.env_clear();
    for key in [
        "PATH",
        "HOME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TERMINFO",
        "USER",
        "LOGNAME",
        "__CF_USER_TEXT_ENCODING",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // **Round 4 fix (MED-C3)**: on Windows, claude CLI locates its
    // OAuth credentials + settings via `%APPDATA%\claude\`. Without
    // those env vars the spawned worker (and the CLI it spawns) will
    // fail to authenticate and the pool will churn rebuilding
    // doomed sessions. `HOME` covers Unix only.
    #[cfg(windows)]
    for key in ["USERPROFILE", "APPDATA", "LOCALAPPDATA", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    // **Round 4 fix (MED-3)**: cap `RUST_LOG` at `debug` for the
    // worker. A misconfigured (or attacker-controlled) gateway env
    // setting `RUST_LOG=trace` would otherwise cause the worker to
    // log every PTY byte through `forward_stream` into the gateway's
    // tracing pipeline — a trivial disk-fill DoS vector.
    if let Ok(v) = std::env::var("RUST_LOG") {
        let capped = cap_rust_log_verbosity(&v);
        cmd.env("RUST_LOG", capped);
    }
    cmd.arg("--bind")
        .arg(config.bind.to_string())
        .arg("--home-dir")
        .arg(&config.home_dir)
        .env("DUDUCLAW_WORKER_TOKEN", token)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;

    // Drain stdout/stderr in the background and forward to tracing so the
    // worker's log lines surface in the gateway's combined log stream.
    if let Some(stdout) = child.stdout.take() {
        tokio::spawn(forward_stream("worker.stdout", stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(forward_stream("worker.stderr", stderr));
    }

    Ok(child)
}

async fn forward_stream<R>(label: &'static str, reader: R)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                // **Round 4 fix (HIGH-3)**: sanitize control characters
                // before passing into the tracing macro. A worker
                // process that emits raw `\n`/`\r`/ESC sequences could
                // otherwise inject fake log records into a downstream
                // JSON-line log collector (Loki, Datadog), bypassing the
                // structured-log integrity guarantees that operators
                // rely on for incident review.
                let safe = sanitize_worker_log_line(&line);
                debug!(
                    target: "duduclaw_gateway::worker_supervisor",
                    stream = label,
                    line = %safe,
                    "worker stream"
                );
            }
            Ok(None) => return,
            Err(e) => {
                warn!(stream = label, error = %e, "worker stream read failed");
                return;
            }
        }
    }
}

/// **Round 4 fix (HIGH-3)**: replace newline + ESC + other control
/// characters with `?` before logging. Keeps tabs (`\t`, 0x09) because
/// some legitimate log lines tab-indent JSON. Caps the displayed length
/// so a multi-MB single line can't be turned into one tracing record.
pub(crate) fn sanitize_worker_log_line(line: &str) -> String {
    const MAX_LEN: usize = 4096;
    let truncated: String = line.chars().take(MAX_LEN).collect();
    truncated
        .chars()
        .map(|c| {
            if c == '\t' || (!c.is_control() && c != '\u{7f}') {
                c
            } else {
                '?'
            }
        })
        .collect()
}

/// **Round 4 fix (MED-3)**: cap `RUST_LOG` directive so the worker
/// can't be set to `trace` (which floods stdout/stderr through
/// `forward_stream`). Operates by string substitution because
/// `tracing_subscriber::EnvFilter` doesn't expose its grammar.
pub(crate) fn cap_rust_log_verbosity(filter: &str) -> String {
    filter.replace("trace", "debug").replace("TRACE", "DEBUG")
}

async fn wait_for_healthy(
    client: &WorkerClient,
    timeout: Duration,
    shutdown: Option<&CancellationToken>,
) -> Result<(), SupervisorError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut tick = tokio::time::interval(Duration::from_millis(200));
    tick.tick().await; // skip immediate first tick
    loop {
        // **Round 4 fix (MED-C1)**: honour the supervisor's shutdown
        // token. Without this, a gateway shutdown that lands during a
        // worker restart leaves this loop spinning until the boot
        // timeout expires (default 30 s) holding the `health_check_loop`
        // task open and delaying Tokio runtime shutdown.
        if let Some(token) = shutdown {
            if token.is_cancelled() {
                debug!("worker_supervisor: wait_for_healthy cancelled by shutdown");
                return Err(SupervisorError::BootTimeout(timeout));
            }
        }
        if tokio::time::Instant::now() > deadline {
            return Err(SupervisorError::BootTimeout(timeout));
        }
        match client.healthz(Duration::from_secs(2)).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                debug!(error = %e, "worker_supervisor: healthz pending, retrying");
                if let Some(token) = shutdown {
                    tokio::select! {
                        _ = token.cancelled() => {
                            return Err(SupervisorError::BootTimeout(timeout));
                        }
                        _ = tick.tick() => {}
                    }
                } else {
                    tick.tick().await;
                }
            }
        }
    }
}

async fn health_check_loop(inner: Arc<SupervisorInner>) {
    let mut tick = tokio::time::interval(inner.config.health_interval);
    tick.tick().await; // discard immediate first tick
    let mut misses: u32 = 0;
    // **Review fix (MEDIUM)**: bound consecutive restart attempts +
    // exponential backoff. Previously a broken worker binary could
    // be fork-bombed every ~60s indefinitely; now after
    // `MAX_CONSECUTIVE_RESTARTS` failures we sleep
    // `RESTART_BACKOFF_FAILURE_SECS` before trying again, giving
    // operators time to notice + fix the root cause.
    const MAX_CONSECUTIVE_RESTARTS: u32 = 5;
    const RESTART_BACKOFF_FAILURE_SECS: u64 = 300;
    let mut consecutive_restart_failures: u32 = 0;
    loop {
        tokio::select! {
            _ = inner.shutdown.cancelled() => {
                debug!("worker_supervisor: health loop exiting (shutdown)");
                return;
            }
            _ = tick.tick() => {}
        }

        match inner.client.healthz(Duration::from_secs(3)).await {
            Ok(()) => {
                if misses > 0 || consecutive_restart_failures > 0 {
                    info!(
                        prev_misses = misses,
                        prev_restart_failures = consecutive_restart_failures,
                        "worker_supervisor: healthz recovered"
                    );
                }
                misses = 0;
                consecutive_restart_failures = 0;
            }
            Err(e) => {
                misses += 1;
                crate::metrics::global_metrics().worker_health_miss();
                warn!(misses, max = inner.config.max_health_misses, error = %e, "worker_supervisor: healthz failed");
                if misses >= inner.config.max_health_misses {
                    if let Err(restart_err) = restart_child(&inner).await {
                        consecutive_restart_failures = consecutive_restart_failures.saturating_add(1);
                        error!(
                            error = %restart_err,
                            consecutive_failures = consecutive_restart_failures,
                            "worker_supervisor: restart failed"
                        );
                        if consecutive_restart_failures >= MAX_CONSECUTIVE_RESTARTS {
                            error!(
                                consecutive_failures = consecutive_restart_failures,
                                backoff_secs = RESTART_BACKOFF_FAILURE_SECS,
                                "worker_supervisor: too many consecutive restart failures — backing off"
                            );
                            // Long sleep, cancellable via shutdown token.
                            tokio::select! {
                                _ = inner.shutdown.cancelled() => return,
                                _ = tokio::time::sleep(Duration::from_secs(RESTART_BACKOFF_FAILURE_SECS)) => {}
                            }
                            // **Round 2 review fix (HIGH-5)**: reset
                            // BOTH `consecutive_restart_failures` AND
                            // `misses` after the backoff sleep. The
                            // previous code only reset the failure
                            // count, leaving `misses >=
                            // max_health_misses`, which caused the
                            // next tick to immediately fire another
                            // restart without first probing healthz.
                            // Resetting `misses` here lets the next
                            // tick re-evaluate the worker's actual
                            // state via a fresh healthz probe before
                            // deciding to restart again.
                            consecutive_restart_failures = 0;
                            misses = 0;
                        }
                    } else {
                        crate::metrics::global_metrics().worker_restart();
                        misses = 0;
                        consecutive_restart_failures = 0;
                    }
                }
            }
        }
    }
}

async fn restart_child(inner: &SupervisorInner) -> Result<(), SupervisorError> {
    warn!("worker_supervisor: restarting child");
    {
        let mut guard = inner.child.lock().await;
        if let Some(mut child) = guard.take() {
            kill_child_gracefully(&mut child).await;
        }
    }
    tokio::time::sleep(inner.config.restart_backoff).await;
    let new_child = spawn_child(&inner.binary, &inner.config, &inner.token).await?;
    // Round 4 (MED-C1): observe `inner.shutdown` during restart polls
    // so gateway shutdown isn't blocked by a slow-booting worker.
    wait_for_healthy(&inner.client, inner.config.boot_timeout, Some(&inner.shutdown)).await?;
    let mut guard = inner.child.lock().await;
    *guard = Some(new_child);
    info!("worker_supervisor: child restarted");
    Ok(())
}

async fn kill_child_gracefully(child: &mut Child) {
    // Try SIGTERM first (portable-pty / Drop will SIGKILL after grace).
    // tokio's Child::kill sends SIGKILL on Unix and TerminateProcess on
    // Windows — that's already the hard step. For the gentle step, send
    // SIGTERM via id().
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            // Give the worker up to 3 s to clean up.
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        }
    }
    // Final hard kill (Windows: TerminateProcess; Unix: SIGKILL).
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Look for the `duduclaw-cli-worker` binary. Preference order:
/// 1. Sibling of the current gateway binary (`<exe_dir>/duduclaw-cli-worker[.exe]`).
/// 2. `PATH` lookup (with a `warn!` so operators notice the
///    hijack-prone fallback).
///
/// Round 4 deferred-cleanup (LOW-3): the PATH-walk fallback is
/// hijack-prone — a writable directory in front of the system bin
/// dirs (`$HOME/bin`, an `npm` global, an MDM-managed shim path)
/// can shadow the real worker. The sibling lookup is the only
/// strictly trustworthy path, so we now emit a `warn!` whenever the
/// fallback fires so operators are nudged toward
/// `binary_override` in `config.toml [runtime] worker_bin =
/// "/abs/path"`.
pub fn resolve_worker_bin() -> Option<PathBuf> {
    let bin_name = if cfg!(windows) {
        "duduclaw-cli-worker.exe"
    } else {
        "duduclaw-cli-worker"
    };
    if let Ok(self_exe) = std::env::current_exe() {
        if let Some(dir) = self_exe.parent() {
            let candidate = dir.join(bin_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // PATH walk — supply-chain risk: warn so operators can decide
    // whether to pin `binary_override`.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(bin_name);
        if candidate.exists() {
            warn!(
                resolved = %candidate.display(),
                "worker_supervisor: `duduclaw-cli-worker` not found beside the gateway binary; falling back to PATH. \
This is hijack-prone — set `[runtime] worker_bin = \"/abs/path\"` in config.toml to lock in the binary location."
            );
            return Some(candidate);
        }
    }
    None
}

/// Read `[runtime] worker_managed = true` from `<home>/config.toml`. Default
/// `false` keeps the in-process PtyPool path.
pub fn read_worker_managed_flag(home_dir: &Path) -> bool {
    read_runtime_flag(home_dir, "worker_managed").unwrap_or(false)
}

/// Read the optional override bind for the worker.
pub fn read_worker_bind(home_dir: &Path) -> Option<SocketAddr> {
    let path = home_dir.join("config.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: toml::Value = text.parse().ok()?;
    let bind_str = value
        .get("runtime")?
        .get("worker_bind")?
        .as_str()?;
    bind_str.parse().ok()
}

/// Round 4 deferred-cleanup (LOW-3): read `[runtime] worker_bin =
/// "/abs/path"` to let operators pin the worker binary against PATH
/// hijack. Empty / relative paths are rejected (relative paths are
/// almost certainly an operator error and would re-introduce the
/// hijack risk via `cwd`).
pub fn read_worker_binary_override(home_dir: &Path) -> Option<PathBuf> {
    let path = home_dir.join("config.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: toml::Value = text.parse().ok()?;
    let raw = value
        .get("runtime")?
        .get("worker_bin")?
        .as_str()?;
    let candidate = PathBuf::from(raw);
    if !candidate.is_absolute() {
        warn!(
            value = raw,
            "worker_supervisor: ignoring `[runtime] worker_bin` because it is not absolute"
        );
        return None;
    }
    if !candidate.exists() {
        warn!(
            value = raw,
            "worker_supervisor: ignoring `[runtime] worker_bin` because it does not exist"
        );
        return None;
    }
    Some(candidate)
}

fn read_runtime_flag(home_dir: &Path, key: &str) -> Option<bool> {
    let path = home_dir.join("config.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let value: toml::Value = text.parse().ok()?;
    value.get("runtime")?.get(key)?.as_bool()
}

/// Combined "spawn supervisor if `worker_managed = true`" entry point used
/// by `server.rs` at gateway boot. The returned `Option` is `None` when
/// the flag is off — callers should leave `pty_runtime` in InProcess mode.
pub async fn spawn_if_enabled(
    home_dir: &Path,
) -> Result<Option<WorkerSupervisorHandle>, SupervisorError> {
    // Phase 8 emergency rollback: `DUDUCLAW_DISABLE_PTY_POOL=1` overrides
    // config-file opt-in and forces every agent back to the legacy
    // fresh-spawn path. Skipping the supervisor here means
    // `pty_runtime::MANAGED_WORKER` stays unset, which combined with
    // `runtime_mode_for_agent` short-circuiting to FreshSpawn produces a
    // consistent "PTY pool disabled" state across the gateway.
    if crate::pty_runtime::is_pty_pool_disabled_globally() {
        info!("worker_supervisor: DUDUCLAW_DISABLE_PTY_POOL=1 — skipping spawn");
        return Ok(None);
    }
    if !read_worker_managed_flag(home_dir) {
        return Ok(None);
    }
    let mut config = SupervisorConfig::new(home_dir.to_path_buf());
    if let Some(bind) = read_worker_bind(home_dir) {
        if !bind.ip().is_loopback() {
            warn!(
                bind = %bind,
                "worker_supervisor: refusing non-loopback worker_bind, falling back to default"
            );
        } else {
            config.bind = bind;
        }
    }
    if let Some(override_path) = read_worker_binary_override(home_dir) {
        info!(
            path = %override_path.display(),
            "worker_supervisor: using configured `[runtime] worker_bin` (PATH lookup skipped)"
        );
        config.binary_override = Some(override_path);
    }
    Ok(Some(WorkerSupervisorHandle::spawn(config).await?))
}

// Silence unused-warning on `tokio::sync::oneshot` (kept for future
// expansion to "request immediate health check" channels).
#[allow(dead_code)]
fn _unused_imports() -> oneshot::Sender<()> {
    let (tx, _rx) = oneshot::channel();
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn supervisor_config_defaults_are_safe() {
        let cfg = SupervisorConfig::new(PathBuf::from("/tmp/x"));
        assert_eq!(cfg.bind.to_string(), DEFAULT_BIND);
        assert!(cfg.bind.ip().is_loopback());
        assert_eq!(cfg.max_health_misses, DEFAULT_MAX_HEALTH_MISSES);
    }

    #[test]
    fn read_worker_managed_returns_false_for_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(!read_worker_managed_flag(dir.path()));
    }

    #[test]
    fn read_worker_managed_returns_true_when_set() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\nworker_managed = true\n",
        )
        .unwrap();
        assert!(read_worker_managed_flag(dir.path()));
    }

    #[test]
    fn read_worker_managed_returns_false_for_explicit_false() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\nworker_managed = false\n",
        )
        .unwrap();
        assert!(!read_worker_managed_flag(dir.path()));
    }

    #[test]
    fn read_worker_bind_parses_valid_loopback() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\nworker_bind = \"127.0.0.1:19876\"\n",
        )
        .unwrap();
        let bind = read_worker_bind(dir.path()).expect("must parse");
        assert_eq!(bind.port(), 19876);
        assert!(bind.ip().is_loopback());
    }

    #[test]
    fn read_worker_bind_returns_none_for_missing_key() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[other]\nfoo = 1\n").unwrap();
        assert!(read_worker_bind(dir.path()).is_none());
    }

    #[test]
    fn resolve_worker_bin_returns_none_for_unset_path() {
        // In a clean env without the binary on PATH and no sibling, this
        // returns None. We can't fully assert that here (PATH may have it
        // when running tests), but we can at least exercise the function.
        let _ = resolve_worker_bin();
    }

    // Round 4 deferred-cleanup (LOW-3) — worker_bin override.

    #[test]
    fn read_worker_binary_override_returns_none_when_missing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[runtime]\nworker_managed = true\n")
            .unwrap();
        assert!(read_worker_binary_override(dir.path()).is_none());
    }

    #[test]
    fn read_worker_binary_override_ignores_relative_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\nworker_bin = \"./worker\"\n",
        )
        .unwrap();
        assert!(
            read_worker_binary_override(dir.path()).is_none(),
            "relative paths must be rejected to avoid PATH-hijack regression"
        );
    }

    #[test]
    fn read_worker_binary_override_ignores_nonexistent_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[runtime]\nworker_bin = \"/this/path/does/not/exist/duduclaw-cli-worker\"\n",
        )
        .unwrap();
        assert!(read_worker_binary_override(dir.path()).is_none());
    }

    #[test]
    fn read_worker_binary_override_accepts_existing_absolute_path() {
        let dir = TempDir::new().unwrap();
        let bin_path = dir.path().join("worker-bin");
        std::fs::write(&bin_path, b"#!/bin/sh\necho ok\n").unwrap();
        let config_text = format!(
            "[runtime]\nworker_bin = \"{}\"\n",
            bin_path.to_string_lossy()
        );
        std::fs::write(dir.path().join("config.toml"), config_text).unwrap();
        let got = read_worker_binary_override(dir.path()).expect("must accept");
        assert_eq!(got, bin_path);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_if_enabled_returns_none_when_flag_off() {
        let dir = TempDir::new().unwrap();
        // No config.toml exists.
        let result = spawn_if_enabled(dir.path()).await.expect("must succeed");
        assert!(result.is_none());
    }

    // **Round 4 (HIGH-3)** — log line sanitiser.

    #[test]
    fn sanitize_replaces_newlines_and_escapes() {
        let raw = "good line\n[FAKE] gateway: pwned\r\x1b[31mred";
        let s = sanitize_worker_log_line(raw);
        assert!(!s.contains('\n'));
        assert!(!s.contains('\r'));
        assert!(!s.contains('\x1b'));
        assert!(s.contains("good line"));
        assert!(s.contains("[FAKE] gateway: pwned")); // text preserved, control chars only replaced
    }

    #[test]
    fn sanitize_preserves_tabs_and_printables() {
        let raw = "key\tvalue with spaces 12345 中文 émoji";
        let s = sanitize_worker_log_line(raw);
        assert_eq!(s, raw);
    }

    #[test]
    fn sanitize_caps_excessive_length() {
        let raw: String = "A".repeat(10_000);
        let s = sanitize_worker_log_line(&raw);
        assert!(s.len() <= 4096);
    }

    // **Round 4 (MED-3)** — RUST_LOG verbosity cap.

    #[test]
    fn cap_rust_log_downgrades_trace_to_debug() {
        assert_eq!(cap_rust_log_verbosity("trace"), "debug");
        assert_eq!(cap_rust_log_verbosity("TRACE"), "DEBUG");
        assert_eq!(
            cap_rust_log_verbosity("duduclaw_cli_runtime=trace,info"),
            "duduclaw_cli_runtime=debug,info"
        );
    }

    #[test]
    fn cap_rust_log_leaves_other_levels_alone() {
        assert_eq!(cap_rust_log_verbosity("info"), "info");
        assert_eq!(cap_rust_log_verbosity("warn,foo::bar=debug"), "warn,foo::bar=debug");
    }

    // **Round 4 (MED-C1)** — wait_for_healthy honours shutdown signal.

    #[tokio::test]
    async fn wait_for_healthy_returns_when_shutdown_cancelled() {
        // Construct a WorkerClient pointing at a port nothing listens on
        // — every healthz attempt will fail, ordinarily looping for the
        // full boot_timeout. Cancelling the token must short-circuit.
        let client = WorkerClient::new("http://127.0.0.1:1", "tok").unwrap();
        let token = CancellationToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            t2.cancel();
        });
        let start = tokio::time::Instant::now();
        let result = wait_for_healthy(&client, Duration::from_secs(30), Some(&token)).await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "must report failure when shutdown fires");
        assert!(
            elapsed < Duration::from_secs(2),
            "must exit promptly after cancel, took {elapsed:?}"
        );
    }
}
