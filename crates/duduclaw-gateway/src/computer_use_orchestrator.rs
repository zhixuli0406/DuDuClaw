//! L5 computer-use container primitives for the `computer_*` tool sessions
//! (`computer_use_sessions`): container lifecycle, masked screenshots,
//! xdotool actions, navigation, the global session registry and the shared
//! checks (CONTRACT.toml `must_not`, the threat-level kill switch).
//!
//! Sessions run only in the container. The gateway-run, chat-triggered loop
//! that used to drive this orchestrator through the Anthropic Messages API,
//! and the native mode that drove the host desktop, were removed; neither
//! ever completed a session in a released build.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::computer_use::{
    ComputerAction, ComputerUseError, MaskingConfig, RegionDetectFailure, mask_screenshot_regions,
};

// ---------------------------------------------------------------------------
// Global session registry (for MCP tool → orchestrator wiring)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// Global registry of active computer use sessions, keyed by session_id.
///
/// Holds only the control flags, so the chat emergency-stop words can stop
/// every session and the cap of [`MAX_CONCURRENT_SESSIONS`] covers every
/// tool-driven session (`computer_use_sessions`).
static SESSION_REGISTRY: std::sync::OnceLock<
    tokio::sync::Mutex<HashMap<String, Arc<OrchestratorControl>>>,
> = std::sync::OnceLock::new();

fn session_registry() -> &'static tokio::sync::Mutex<HashMap<String, Arc<OrchestratorControl>>> {
    SESSION_REGISTRY.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Maximum concurrent computer use sessions (prevents DoS).
const MAX_CONCURRENT_SESSIONS: usize = 5;

/// `docker run --pids-limit` for the computer-use container.
///
/// The limit counts threads, not processes. Chromium on `about:blank` already
/// runs 86-92 threads; the old value of 100 meant a page spawning ~20 web
/// workers hit the cap (`pthread_create: Resource temporarily unavailable`),
/// after which the DOM helper timed out and every screenshot was fully masked.
/// 512 leaves headroom for real pages while still bounding a fork bomb.
const CONTAINER_PIDS_LIMIT: u32 = 512;

/// Upper bound on `docker run -d` (the image is local and never pulled: the
/// argv carries `--pull never` and `start_container` checks presence first, but
/// a wedged daemon must not hang the session start).
const DOCKER_RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound on one readiness probe inside `wait_for_display`.
const DISPLAY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on each `docker exec` used for screenshot capture / readback.
const SCREENSHOT_EXEC_TIMEOUT: Duration = Duration::from_secs(15);
/// Upper bound on one xdotool action (`type` of long text is the slow case).
const ACTION_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on the active-window-title probe.
const WINDOW_TITLE_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on one `duduclaw-navigate` exec (the helper itself gives up
/// on the load event after 15 s).
const NAVIGATE_EXEC_TIMEOUT: Duration = Duration::from_secs(20);
/// Upper bound on each best-effort cleanup call (`docker stop -t 5` / `rm -f`).
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Run `docker <args>` with a hard wall-clock limit.
///
/// The child is spawned with `kill_on_drop(true)`, so when the timeout fires
/// and the `output()` future is dropped, the `docker` client process is killed
/// rather than left running. A timeout is returned as an `Err` exactly like a
/// spawn failure, so every caller's existing failure path applies unchanged.
pub(crate) async fn docker_output<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    limit: Duration,
    what: &str,
) -> Result<std::process::Output, ComputerUseError> {
    let fut = tokio::process::Command::new("docker")
        .args(args)
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(limit, fut).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(ComputerUseError::ApiError(format!("{what} failed: {e}"))),
        Err(_) => Err(ComputerUseError::ApiError(format!(
            "{what} timed out after {}s",
            limit.as_secs()
        ))),
    }
}

/// [`docker_output`] with `input` written to the child's stdin (for
/// `docker exec -i`), then stdin closed. Same timeout and kill-on-drop
/// behaviour; the write itself is inside the time limit.
pub(crate) async fn docker_output_with_stdin<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    input: &[u8],
    limit: Duration,
    what: &str,
) -> Result<std::process::Output, ComputerUseError> {
    use tokio::io::AsyncWriteExt;
    let run = async {
        let mut child = tokio::process::Command::new("docker")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
        }
        child.wait_with_output().await
    };
    match tokio::time::timeout(limit, run).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(ComputerUseError::ApiError(format!("{what} failed: {e}"))),
        Err(_) => Err(ComputerUseError::ApiError(format!(
            "{what} timed out after {}s",
            limit.as_secs()
        ))),
    }
}

/// Where `scrot` writes the screenshot the gateway then reads back: inside
/// `/tmp/duduclaw-root`, the root-only scratch directory (mode 0700, created
/// by the entrypoint on the `/tmp` tmpfs before any process runs as the
/// `sandbox` user). Root-side temp files go there, never directly in the
/// world-writable `/tmp`, so the browser user cannot pre-create or race them.
pub(crate) const CONTAINER_SCREENSHOT_PATH: &str = "/tmp/duduclaw-root/screen.png";

/// Window titles that indicate a credential / secret context. Shared by
/// `capture_masked_screenshot` and risk assessment so they always agree.
const SENSITIVE_WINDOW_MARKERS: &[&str] = &[
    "1password",
    "bitwarden",
    "lastpass",
    "keepass",
    "keychain",
    "密碼",
    "password",
    "credential",
    "ssh",
    "gpg",
    "pgp",
];

/// D12: does this action enter input (text/keystrokes) that could land in a
/// sensitive field? `Type` and `Key` write characters; clicks/moves do not.
pub(crate) fn action_targets_input(action: &ComputerAction) -> bool {
    matches!(
        action,
        ComputerAction::Type { .. } | ComputerAction::Key { .. }
    )
}

/// Turn a finished title probe into the title, or why it is unusable.
fn title_from_output(output: &std::process::Output) -> Result<String, String> {
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(code) => format!("title probe exited with status {code}"),
            None => "title probe was terminated by a signal".to_string(),
        });
    }
    match std::str::from_utf8(&output.stdout) {
        Ok(text) => Ok(text.trim().to_string()),
        Err(_) => Err("title probe printed non-UTF-8 output".to_string()),
    }
}

/// What the window-title check decides for a screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleVerdict {
    /// Read, and no credential marker: the screenshot goes out unchanged.
    Clear,
    /// Read, and a credential marker matched: mask everything.
    Sensitive,
    /// Not readable: mask everything (fail closed).
    Unreadable,
}

/// Why a screenshot came back fully masked. A closed set: the caller gets
/// the code, never the helper's or the probe's raw output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullMaskReason {
    /// Sensitive-region detection failed for any reason other than
    /// [`Self::SeveralPages`] (browser not answering, timeout, unexpected
    /// output, viewport not where expected).
    HelperFailed,
    /// Several browser pages are visible at once (e.g. after `ctrl+n`), so
    /// the helper cannot tell which one is on screen.
    SeveralPages,
    /// The focused window's title carries a credential marker.
    TitleSensitive,
    /// The focused window's title could not be read.
    TitleUnreadable,
}

impl FullMaskReason {
    /// The stable code carried in the screenshot response and audit row.
    pub fn code(self) -> &'static str {
        match self {
            Self::HelperFailed => "helper_failed",
            Self::SeveralPages => "several_pages",
            Self::TitleSensitive => "title_sensitive",
            Self::TitleUnreadable => "title_unreadable",
        }
    }
}

/// A masked screenshot and whether masking covered all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskedScreenshot {
    /// Base64 PNG, masked.
    pub png_base64: String,
    /// `Some` when the whole picture was masked (fail closed), with why.
    pub full_mask: Option<FullMaskReason>,
}

impl MaskedScreenshot {
    /// Was the whole picture masked?
    pub fn fully_masked(&self) -> bool {
        self.full_mask.is_some()
    }
}

fn title_verdict(title: &Result<String, String>) -> TitleVerdict {
    match title {
        Ok(t) if window_is_sensitive(Some(t)) => TitleVerdict::Sensitive,
        Ok(_) => TitleVerdict::Clear,
        Err(_) => TitleVerdict::Unreadable,
    }
}

/// D12: is the focused window a known credential / secret context?
pub(crate) fn window_is_sensitive(title: Option<&str>) -> bool {
    match title {
        Some(t) => {
            let lower = t.to_lowercase();
            SENSITIVE_WINDOW_MARKERS.iter().any(|m| lower.contains(m))
        }
        None => false,
    }
}

/// Register an orchestrator's control handle in the global registry.
///
/// Returns `Err` if the maximum concurrent session limit is reached.
pub async fn register_session(
    session_id: &str,
    control: Arc<OrchestratorControl>,
) -> Result<(), ComputerUseError> {
    let mut registry = session_registry().lock().await;
    if registry.len() >= MAX_CONCURRENT_SESSIONS {
        return Err(ComputerUseError::ApiError(format!(
            "Maximum concurrent sessions ({MAX_CONCURRENT_SESSIONS}) reached"
        )));
    }
    registry.insert(session_id.to_string(), control);
    Ok(())
}

/// Remove a session from the registry.
pub async fn unregister_session(session_id: &str) {
    session_registry().lock().await.remove(session_id);
}

/// Look up an active session's control handle.
pub async fn get_session_control(session_id: &str) -> Option<Arc<OrchestratorControl>> {
    session_registry().lock().await.get(session_id).cloned()
}

/// List all active session IDs.
pub async fn list_sessions() -> Vec<String> {
    session_registry().lock().await.keys().cloned().collect()
}

/// Label carrying the home hash on every computer-use container, so the
/// orphan sweep only ever touches containers of its own home.
pub const HOME_LABEL: &str = "com.duduclaw.computer-use.home";

/// Label carrying the container's hard deadline (unix seconds).
pub const DEADLINE_LABEL: &str = "com.duduclaw.computer-use.deadline";

/// The value of [`HOME_LABEL`] for `home`: the same 32-hex digest the task
/// sandbox uses, over the canonical path when it resolves (so two spellings
/// of one home agree).
pub fn computer_use_home_label(home: &std::path::Path) -> String {
    let canonical = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    crate::task_sandbox::container::home_label(&canonical)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Computer use session configuration (from agent.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputerUseConfig {
    /// Maximum minutes per session.
    pub max_session_minutes: u32,
    /// Maximum actions per session.
    pub max_actions: u32,
    /// Virtual display width.
    pub display_width: u32,
    /// Virtual display height.
    pub display_height: u32,
    /// Automatically confirm trusted operations without channel confirmation.
    pub auto_confirm_trusted: bool,
    /// Allowed applications (empty = all allowed).
    pub allowed_apps: Vec<String>,
    /// Blocked action types.
    pub blocked_actions: Vec<String>,
    /// Container image to use. Tool-driven sessions set it from
    /// `crate::computer_use_image::load` (`config.toml [computer_use] image`,
    /// else the versioned published image); the default is that versioned
    /// image, never `:latest`.
    pub container_image: String,
    /// Network access mode for the container.
    pub network_access: bool,
    /// Allowed domains (only when network_access=true).
    pub allowed_domains: Vec<String>,
    /// Gateway-resolved navigation hosts (tool-driven sessions only). When
    /// non-empty the container is started in the pinned-egress mode (see
    /// [`build_docker_run_args`]) and `network_access` / `allowed_domains`
    /// are ignored. Never read from or written to configuration.
    #[serde(skip)]
    pub pinned_hosts: Vec<PinnedHost>,
    /// CONTRACT.toml `must_not` rules — actions matching these are blocked.
    pub contract_must_not: Vec<String>,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        Self {
            max_session_minutes: 10,
            max_actions: 50,
            display_width: 1280,
            display_height: 800,
            auto_confirm_trusted: false,
            allowed_apps: Vec::new(),
            blocked_actions: vec![
                "delete_file".to_string(),
                "terminal".to_string(),
                "system_preferences".to_string(),
            ],
            container_image: crate::computer_use_image::default_image(),
            network_access: false,
            allowed_domains: Vec::new(),
            pinned_hosts: Vec::new(),
            contract_must_not: Vec::new(),
        }
    }
}

/// One navigation host the gateway resolved and vetted for a tool-driven
/// session: the container gets `--add-host <host>:<ip>` and may open TCP 443
/// to `ip` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedHost {
    /// Lowercased exact hostname (already through
    /// `duduclaw_core::types::normalize_navigation_host`).
    pub host: String,
    /// The first public IPv4 address the host resolved to.
    pub ip: std::net::Ipv4Addr,
}

/// What `duduclaw-navigate` reported (one JSON line on stdout).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NavigateOutcome {
    pub ok: bool,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Parse the helper's stdout: the last non-empty line must be the JSON
/// object. Anything else is `None` (the caller treats it as a failure).
pub(crate) fn parse_navigate_output(stdout: &[u8]) -> Option<NavigateOutcome> {
    let text = std::str::from_utf8(stdout).ok()?;
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?;
    serde_json::from_str(line.trim()).ok()
}

// ---------------------------------------------------------------------------
// Orchestrator state
// ---------------------------------------------------------------------------

/// Shared flags a session is controlled through (emergency stop, pause).
#[derive(Debug)]
pub struct OrchestratorControl {
    /// Set to true to pause the session (only screenshots and stop run).
    pub paused: AtomicBool,
    /// Set to true to end the session.
    pub stopped: AtomicBool,
}

impl OrchestratorControl {
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }
}

/// One computer-use container session: lifecycle, masked screenshots,
/// actions and navigation. Policy (gates, approvals, limits) lives in
/// `computer_use_sessions`.
pub struct ComputerUseOrchestrator {
    /// Container identifier (set after start_container).
    container_id: Option<String>,
    /// Configuration.
    config: ComputerUseConfig,
    /// Agent identifier.
    agent_id: String,
    /// Home directory for audit logs.
    home_dir: PathBuf,
    /// Shared control flags (accessed by channel commands).
    pub control: Arc<OrchestratorControl>,
    /// Screenshot masking config.
    masking: MaskingConfig,
    /// The id this orchestrator's control handle is registered under in the
    /// global registry (set by [`Self::register`]).
    registered_as: Option<String>,
}

/// SEC: Ensure the container is cleaned up even on panic/task cancellation.
impl Drop for ComputerUseOrchestrator {
    fn drop(&mut self) {
        // Leave the global registry too, so a dropped orchestrator never
        // keeps a slot of the 5-session cap (or shows up to the emergency
        // stop) after its container is gone.
        if let Some(id) = self.registered_as.take() {
            match session_registry().try_lock() {
                Ok(mut registry) => {
                    registry.remove(&id);
                }
                Err(_) => {
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            unregister_session(&id).await;
                        });
                    }
                }
            }
        }
        if let Some(ref name) = self.container_id {
            let name = name.clone();
            warn!(container = %name, "Orchestrator dropped with active container — force cleanup");
            // Use try_current to avoid panic if Tokio runtime is already shut down.
            // If runtime is gone, fall back to blocking std::process::Command.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = tokio::process::Command::new("docker")
                        .args(["rm", "-f", &name])
                        .output()
                        .await;
                });
            } else {
                // Runtime already shut down — use blocking cleanup
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &name])
                    .output();
            }
        }
    }
}

impl ComputerUseOrchestrator {
    pub fn new(agent_id: String, home_dir: PathBuf, config: ComputerUseConfig) -> Self {
        Self {
            container_id: None,
            config,
            agent_id,
            home_dir,
            control: Arc::new(OrchestratorControl::new()),
            masking: MaskingConfig::default(),
            registered_as: None,
        }
    }

    /// Register this orchestrator's control handle in the global registry
    /// under `session_id` (cap of [`MAX_CONCURRENT_SESSIONS`]). `Drop` and
    /// [`Self::unregister`] remove it again.
    pub async fn register(&mut self, session_id: &str) -> Result<(), ComputerUseError> {
        register_session(session_id, self.control_handle()).await?;
        self.registered_as = Some(session_id.to_string());
        Ok(())
    }

    /// Leave the global registry (no-op when not registered).
    pub async fn unregister(&mut self) {
        if let Some(id) = self.registered_as.take() {
            unregister_session(&id).await;
        }
    }

    /// The display size this orchestrator was configured with.
    pub fn display_size(&self) -> (u32, u32) {
        (self.config.display_width, self.config.display_height)
    }


    /// Open `url` in the container's browser through `duduclaw-navigate`.
    /// The caller has validated the URL against the session's pinned hosts;
    /// the helper refuses anything but `https://` again. The URL travels on
    /// the exec's stdin (`docker exec -i`), never in its argv, so the query
    /// string is not visible in the process list while the helper runs.
    /// `Err` when the helper could not be run or printed no parseable line.
    pub async fn navigate(&self, url: &str) -> Result<NavigateOutcome, ComputerUseError> {
        let container = self
            .container_id
            .as_deref()
            .ok_or_else(|| ComputerUseError::ApiError("No active container".to_string()))?;
        let output = docker_output_with_stdin(
            &["exec", "-i", container, "duduclaw-navigate"],
            format!("{url}\n").as_bytes(),
            NAVIGATE_EXEC_TIMEOUT,
            "Navigate",
        )
        .await?;
        parse_navigate_output(&output.stdout).ok_or_else(|| {
            ComputerUseError::ApiError("duduclaw-navigate printed no result line".to_string())
        })
    }

    /// Get a shareable handle to the control flags.
    pub fn control_handle(&self) -> Arc<OrchestratorControl> {
        Arc::clone(&self.control)
    }

    // ── Container lifecycle ───────────────────────────────────

    /// Start the container (`--network=none` unless a valid egress
    /// allowlist is configured) and wait for its virtual display.
    pub async fn start_container(&mut self) -> Result<(), ComputerUseError> {
        info!(agent = %self.agent_id, "Starting computer use session");

        // SEC: Validate container image name to prevent docker flag injection
        validate_image_name(&self.config.container_image)?;

        // Never pull: the image must already be in the local store. A missing
        // image (or a Docker that does not answer) is reported to the caller
        // as an operator-safe message naming the image and the remedy.
        let presence = crate::computer_use_image::image_presence(&self.config.container_image).await;
        if let Some(message) =
            crate::computer_use_image::presence_error(&self.config.container_image, presence)
        {
            warn!(image = %self.config.container_image, ?presence, "computer-use image unavailable");
            return Err(ComputerUseError::Unavailable(message));
        }

        let container_name = format!("duduclaw-cu-{}", uuid::Uuid::new_v4().as_simple());

        // P0-4: validate the egress allowlist up front (I10). Entries failing
        // canonicalization (control bytes, `%`, CRLF, IP-literals, URL
        // structure, bad globs) are dropped rather than smuggled into
        // `ALLOWED_DOMAINS`.
        let valid_domains = valid_egress_domains(&self.config.allowed_domains);
        for d in &self.config.allowed_domains {
            if !valid_domains.contains(d) {
                tracing::warn!(domain = %d, "dropping invalid egress allowlist entry (fail-closed)");
            }
        }

        let labels = ContainerLabels {
            home: computer_use_home_label(&self.home_dir),
            deadline_unix: unix_now()
                .saturating_add(u64::from(self.config.max_session_minutes).saturating_mul(60)),
        };
        let args = build_docker_run_args(&container_name, &self.config, &valid_domains, &labels);

        let output = docker_output(&args, DOCKER_RUN_TIMEOUT, "Container start").await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComputerUseError::ApiError(format!(
                "Container start failed: {stderr}"
            )));
        }

        self.container_id = Some(container_name.clone());

        // Wait for Xvfb to be ready (poll with timeout)
        self.wait_for_display(&container_name).await?;

        info!(
            agent = %self.agent_id,
            container = %container_name,
            "Computer use session started"
        );
        Ok(())
    }

    /// Wait for the virtual display to become ready.
    async fn wait_for_display(&self, container_name: &str) -> Result<(), ComputerUseError> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if Instant::now() > deadline {
                return Err(ComputerUseError::ApiError(
                    "Timed out waiting for virtual display".to_string(),
                ));
            }

            let check = docker_output(
                &["exec", container_name, "xdotool", "getactivewindow"],
                DISPLAY_PROBE_TIMEOUT,
                "Display probe",
            )
            .await;

            match check {
                Ok(output) if output.status.success() => {
                    info!(container = %container_name, "Virtual display is ready");
                    return Ok(());
                }
                _ => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// Stop and remove the container.
    pub async fn stop_session(&mut self) {
        if let Some(ref name) = self.container_id {
            info!(container = %name, "Stopping computer use session");
            // Best-effort cleanup (each step bounded; `rm -f` still runs if
            // `stop` hangs or times out).
            let _ = docker_output(&["stop", "-t", "5", name], CLEANUP_TIMEOUT, "Container stop")
                .await;
            let _ = docker_output(&["rm", "-f", name], CLEANUP_TIMEOUT, "Container remove").await;
        }
        self.container_id = None;
    }

    // ── Screenshot capture ────────────────────────────────────

    /// Capture a screenshot via `scrot` inside the container (unmasked).
    pub async fn capture_screenshot(&self) -> Result<String, ComputerUseError> {
        let container = self
            .container_id
            .as_deref()
            .ok_or_else(|| ComputerUseError::ApiError("No active container".to_string()))?;

        let capture = docker_output(
            &["exec", container, "scrot", "-o", CONTAINER_SCREENSHOT_PATH],
            SCREENSHOT_EXEC_TIMEOUT,
            "Screenshot capture",
        )
        .await?;

        if !capture.status.success() {
            return Err(ComputerUseError::ApiError(
                "scrot failed inside container".to_string(),
            ));
        }

        let read = docker_output(
            &["exec", container, "cat", CONTAINER_SCREENSHOT_PATH],
            SCREENSHOT_EXEC_TIMEOUT,
            "Screenshot read",
        )
        .await?;

        if !read.status.success() {
            return Err(ComputerUseError::ApiError(
                "Failed to read screenshot from container".to_string(),
            ));
        }

        Ok(base64::engine::general_purpose::STANDARD.encode(&read.stdout))
    }

    /// Capture a screenshot with sensitive-region masking applied (DOM-based
    /// detection inside the container, then the window-title rule), plus
    /// whether the whole screenshot was masked and the closed reason code.
    pub async fn capture_masked_screenshot_detailed(
        &self,
    ) -> Result<MaskedScreenshot, ComputerUseError> {
        let b64 = self.capture_screenshot().await?;

        // L5a: Detect sensitive regions via DOM queries inside the container.
        let regions = match self.container_id {
            Some(ref container) => Some(
                crate::computer_use::detect_sensitive_regions(container, &self.masking.patterns)
                    .await,
            ),
            None => None,
        };

        // Check active window title for known sensitive apps (HS5): a password
        // manager / credential window masks the whole screenshot.
        let title = self.read_active_window_title().await;
        self.apply_masks(b64, regions, &title)
    }

    /// Both masking rules, in order: the DOM regions (when detection ran),
    /// then the window-title rule — always, even after regions were masked,
    /// so a credential window or an unreadable title masks the whole
    /// screenshot whatever the DOM said.
    ///
    /// HS5 fix (fail closed): a detection error masks the entire screen; the
    /// raw screenshot is never shipped. A title that cannot be read (command
    /// error, timeout, non-zero exit, non-UTF-8 output) also masks the whole
    /// screenshot. An empty title that was read successfully is a readable,
    /// non-sensitive title.
    ///
    /// A detection failure is reported as [`FullMaskReason::SeveralPages`]
    /// or [`FullMaskReason::HelperFailed`] and the title is then not
    /// consulted; otherwise a title verdict decides.
    fn apply_masks(
        &self,
        b64: String,
        regions: Option<Result<Vec<[u32; 4]>, RegionDetectFailure>>,
        title: &Result<String, String>,
    ) -> Result<MaskedScreenshot, ComputerUseError> {
        let b64 = match regions {
            Some(Ok(regions)) if !regions.is_empty() => {
                mask_screenshot_regions(&b64, &regions, self.masking.fill_color)?
            }
            Some(Ok(_)) | None => b64,
            Some(Err(e)) => {
                let reason = match e {
                    RegionDetectFailure::SeveralPages => FullMaskReason::SeveralPages,
                    RegionDetectFailure::Failed(_) => FullMaskReason::HelperFailed,
                };
                warn!(
                    error = %e,
                    reason = reason.code(),
                    "Sensitive region detection failed — masking full screenshot (fail closed)"
                );
                return Ok(MaskedScreenshot {
                    png_base64: self.mask_full_screen(&b64)?,
                    full_mask: Some(reason),
                });
            }
        };
        self.apply_window_title_rule(b64, title)
    }

    /// The window-title half of [`Self::capture_masked_screenshot_detailed`]: mask the
    /// whole screenshot when the title is sensitive or could not be read,
    /// pass it through otherwise.
    fn apply_window_title_rule(
        &self,
        b64: String,
        title: &Result<String, String>,
    ) -> Result<MaskedScreenshot, ComputerUseError> {
        let reason = match title_verdict(title) {
            TitleVerdict::Clear => {
                return Ok(MaskedScreenshot { png_base64: b64, full_mask: None });
            }
            TitleVerdict::Sensitive => {
                if let Ok(t) = title {
                    warn!(window = %t, "Sensitive window detected — masking full screenshot");
                }
                FullMaskReason::TitleSensitive
            }
            TitleVerdict::Unreadable => {
                if let Err(reason) = title {
                    warn!(
                        reason = %reason,
                        "Active window title could not be read — masking full screenshot (fail closed)"
                    );
                }
                FullMaskReason::TitleUnreadable
            }
        };
        Ok(MaskedScreenshot {
            png_base64: self.mask_full_screen(&b64)?,
            full_mask: Some(reason),
        })
    }

    /// Mask the entire screenshot. Used as the fail-closed safety measure when
    /// sensitive-region detection fails or a known credential window is focused.
    fn mask_full_screen(&self, b64: &str) -> Result<String, ComputerUseError> {
        let regions = vec![[
            0_u32,
            0,
            self.config.display_width,
            self.config.display_height,
        ]];
        mask_screenshot_regions(b64, &regions, self.masking.fill_color)
    }

    // ── Action execution ──────────────────────────────────────

    /// Execute a single `ComputerAction` in the container (docker exec +
    /// xdotool). `Screenshot` is a no-op here (captured separately); `Wait`
    /// sleeps.
    pub async fn execute_action(&self, action: &ComputerAction) -> Result<(), ComputerUseError> {
        match action {
            ComputerAction::Screenshot => return Ok(()), // handled separately
            ComputerAction::Wait { duration } => {
                tokio::time::sleep(Duration::from_secs(u64::from(*duration))).await;
                return Ok(());
            }
            _ => {}
        }

        let container = self
            .container_id
            .as_deref()
            .ok_or_else(|| ComputerUseError::ApiError("No active container".to_string()))?;

        let args = action_to_docker_args(container, action);

        let output = docker_output(&args, ACTION_EXEC_TIMEOUT, "Action execution").await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(action = ?action, stderr = %stderr, "xdotool command failed");
            return Err(ComputerUseError::ApiError(format!(
                "xdotool failed: {stderr}"
            )));
        }
        Ok(())
    }

    /// Read the active window title, or the reason it could not be read:
    /// `docker exec xdotool getactivewindow getwindowname`, bounded by
    /// [`WINDOW_TITLE_TIMEOUT`]. `Err` covers no container, a spawn error, a
    /// timeout, a non-zero exit and non-UTF-8 output; a successful read of
    /// an empty title is `Ok("")`.
    pub async fn read_active_window_title(&self) -> Result<String, String> {
        let container = self
            .container_id
            .as_deref()
            .ok_or_else(|| "no active container".to_string())?;
        let output = docker_output(
            &[
                "exec",
                container,
                "xdotool",
                "getactivewindow",
                "getwindowname",
            ],
            WINDOW_TITLE_TIMEOUT,
            "Active window probe",
        )
        .await
        .map_err(|e| e.to_string())?;
        title_from_output(&output)
    }
}

/// The operator kill switch `<home>/threat_level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreatLevel {
    Green,
    Yellow,
    Red,
}

/// One read of `<home>/threat_level`, before deciding (F4 L6, F5-C L1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreatRead {
    /// The file does not exist: the documented "no kill switch" state.
    Absent,
    /// `GREEN` / `YELLOW` / `RED`, case-insensitive, surrounding whitespace
    /// and a leading UTF-8 BOM ignored.
    Level(ThreatLevel),
    /// Exists but unreadable, not UTF-8, empty, or holding anything else.
    /// May be an operator mid-write; re-read before treating it as RED.
    Unclear,
}

/// Classify one read (pure).
pub(crate) fn classify_threat_read(read: std::io::Result<String>) -> ThreatRead {
    match read {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ThreatRead::Absent,
        Err(_) => ThreatRead::Unclear,
        Ok(raw) => {
            let text = raw.trim_start_matches('\u{FEFF}').trim();
            match text.to_ascii_uppercase().as_str() {
                "GREEN" => ThreatRead::Level(ThreatLevel::Green),
                "YELLOW" => ThreatRead::Level(ThreatLevel::Yellow),
                "RED" => ThreatRead::Level(ThreatLevel::Red),
                _ => ThreatRead::Unclear,
            }
        }
    }
}

/// Interpret one read with no retry (coding convention 4, fail closed):
/// absent ⇒ `Green`; a recognised level ⇒ that level; anything unclear ⇒
/// `Red`.
pub(crate) fn threat_level_from_read(read: std::io::Result<String>) -> ThreatLevel {
    match classify_threat_read(read) {
        ThreatRead::Absent => ThreatLevel::Green,
        ThreatRead::Level(level) => level,
        ThreatRead::Unclear => ThreatLevel::Red,
    }
}

/// Re-reads after an unclear read: an operator writing the file with
/// `echo GREEN > threat_level` truncates first, so a read can land between
/// the truncate and the write (F5-C, review F4-L1).
const THREAT_LEVEL_RETRIES: usize = 2;
const THREAT_LEVEL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

fn settle_unclear(path: &std::path::Path) -> ThreatLevel {
    tracing::warn!(
        path = %path.display(),
        "threat_level exists but is unreadable, empty or not GREEN/YELLOW/RED; treating it as RED. \
         Write the file atomically (temp file + rename)"
    );
    ThreatLevel::Red
}

/// Read `<home>/threat_level`: absent ⇒ `Green`; unclear ⇒ re-read up to
/// twice 50 ms apart, still unclear ⇒ `Red`.
pub(crate) async fn read_threat_level(home_dir: &std::path::Path) -> ThreatLevel {
    let path = home_dir.join("threat_level");
    for attempt in 0..=THREAT_LEVEL_RETRIES {
        match classify_threat_read(tokio::fs::read_to_string(&path).await) {
            ThreatRead::Absent => return ThreatLevel::Green,
            ThreatRead::Level(level) => return level,
            ThreatRead::Unclear if attempt < THREAT_LEVEL_RETRIES => {
                tokio::time::sleep(THREAT_LEVEL_RETRY_DELAY).await;
            }
            ThreatRead::Unclear => {}
        }
    }
    settle_unclear(&path)
}

/// Synchronous twin of [`read_threat_level`] for the last gate before an
/// action, which must not await. The re-read sleeps block this thread for at
/// most 100 ms, and only when the file is unclear.
pub(crate) fn read_threat_level_sync(home_dir: &std::path::Path) -> ThreatLevel {
    let path = home_dir.join("threat_level");
    for attempt in 0..=THREAT_LEVEL_RETRIES {
        match classify_threat_read(std::fs::read_to_string(&path)) {
            ThreatRead::Absent => return ThreatLevel::Green,
            ThreatRead::Level(level) => return level,
            ThreatRead::Unclear if attempt < THREAT_LEVEL_RETRIES => {
                std::thread::sleep(THREAT_LEVEL_RETRY_DELAY);
            }
            ThreatRead::Unclear => {}
        }
    }
    settle_unclear(&path)
}

/// Whether `action` (plus optional model reasoning) violates one of the
/// CONTRACT.toml `must_not` rules (tool-driven sessions).
///
/// Rules are free-text strings (e.g., "不得開啟終端機", "不得修改系統設定").
/// We check both the action's semantic description and the model reasoning.
pub(crate) fn contract_must_not_violated(
    rules: &[String],
    action: &ComputerAction,
    model_reasoning: &Option<String>,
) -> bool {
    if rules.is_empty() {
        return false;
    }
    // Build a semantic description of the action (not Rust Debug format)
    // so CONTRACT.toml rules written in natural language can match.
    contract_must_not_matches(rules, &action_to_semantic_string(action), model_reasoning)
}

/// [`contract_must_not_violated`] over an already-built semantic description
/// (the tool-driven `navigate` op, which is not a [`ComputerAction`]).
pub(crate) fn contract_must_not_matches(
    rules: &[String],
    action_str: &str,
    model_reasoning: &Option<String>,
) -> bool {
    if rules.is_empty() {
        return false;
    }
    let reasoning = model_reasoning.as_deref().unwrap_or("").to_lowercase();

    for rule in rules {
        let rule_lower = rule.to_lowercase();
        // Extract significant keywords (4+ chars to reduce false positives).
        // Short functional words like "不得", "開啟" are too generic.
        let keywords: Vec<&str> = rule_lower
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|w| w.len() >= 4 || w.chars().any(|c| c > '\u{2E7F}')) // 4+ bytes or CJK
            .filter(|w| !matches!(*w, "不得" | "不要" | "禁止" | "should" | "must" | "shall"))
            .collect();
        if keywords.is_empty() {
            continue; // rule has no matchable keywords
        }
        // Require ALL significant keywords to match (AND logic, not ANY)
        let matched = keywords
            .iter()
            .all(|kw| action_str.contains(kw) || reasoning.contains(kw));
        if matched {
            warn!(
                rule = %rule,
                action = %action_str,
                "CONTRACT.toml must_not rule triggered"
            );
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Action → docker exec args translation
// ---------------------------------------------------------------------------

/// Convert a `ComputerAction` to a human-readable semantic string for
/// CONTRACT.toml rule matching. Uses natural language terms so rules like
/// "不得開啟終端機" can match against action descriptions.
pub(crate) fn action_to_semantic_string(action: &ComputerAction) -> String {
    match action {
        ComputerAction::LeftClick { coordinate: [x, y] } => {
            format!("left click at {x},{y}")
        }
        ComputerAction::RightClick { coordinate: [x, y] } => {
            format!("right click at {x},{y}")
        }
        ComputerAction::DoubleClick { coordinate: [x, y] } => {
            format!("double click at {x},{y}")
        }
        ComputerAction::Type { text } => {
            format!("type text input: {}", text.to_lowercase())
        }
        ComputerAction::Key { text } => {
            format!("key press: {}", text.to_lowercase())
        }
        ComputerAction::Scroll {
            coordinate: [x, y],
            direction,
            amount,
        } => {
            format!("scroll {direction} {amount} at {x},{y}")
        }
        ComputerAction::MouseMove { coordinate: [x, y] } => {
            format!("mouse move to {x},{y}")
        }
        ComputerAction::Wait { duration } => {
            format!("wait {duration} seconds")
        }
        ComputerAction::Screenshot => "screenshot capture".to_string(),
        ComputerAction::Zoom { .. } => "zoom".to_string(),
    }
}

/// [`validate_image_name`] as a predicate (used by the `[computer_use] image`
/// override parser so a value it accepts is one `start_container` accepts).
pub(crate) fn image_name_is_safe(name: &str) -> bool {
    validate_image_name(name).is_ok()
}

/// Validate container image name against injection. `@` is allowed for
/// digest references (`repo@sha256:…`); it has no meaning in an argv.
fn validate_image_name(name: &str) -> Result<(), ComputerUseError> {
    if name.is_empty() || name.len() > 256 {
        return Err(ComputerUseError::ApiError(
            "Invalid image name length".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._/-:@".contains(c))
    {
        return Err(ComputerUseError::ApiError(format!(
            "Invalid container image name (illegal chars): {name}"
        )));
    }
    if name.contains("--") || name.contains(' ') || name.contains("..") || name.starts_with('/') {
        return Err(ComputerUseError::ApiError(format!(
            "Container image name contains forbidden pattern: {name}"
        )));
    }
    Ok(())
}

/// Filter an egress allowlist down to canonically-valid host entries (P0-4 / I10).
///
/// Invalid entries (control bytes, `%`, CRLF, IP-literals, URL structure, bad
/// globs) are dropped — this is the assembly-time gate before `ALLOWED_DOMAINS`
/// is handed to the container.
fn valid_egress_domains(raw: &[String]) -> Vec<String> {
    raw.iter()
        .filter(|d| duduclaw_core::is_valid_egress_host(d))
        .cloned()
        .collect()
}

/// Fail-closed network decision (P0-4 / I5): isolate the container whenever
/// isolation is requested OR network is enabled but no *valid* domain survived.
/// An empty (or all-invalid) allowlist must never mean "unrestricted egress".
fn should_isolate_network(network_access: bool, valid_domains: &[String]) -> bool {
    !network_access || valid_domains.is_empty()
}

/// Build the `docker run` argv (without the leading `docker`) for a
/// computer-use container. Pure: `valid_domains` must already have passed
/// `valid_egress_domains`.
/// The two labels every computer-use container carries, read back by the
/// orphan sweep (`computer_use_sessions::sweep`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContainerLabels {
    /// [`computer_use_home_label`] of the owning home.
    pub home: String,
    /// Unix seconds after which the container has outlived its session.
    pub deadline_unix: u64,
}

fn build_docker_run_args(
    container_name: &str,
    config: &ComputerUseConfig,
    valid_domains: &[String],
    labels: &ContainerLabels,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        // Never pull, whatever the daemon's default: a missing image fails
        // the start instead of fetching something from a registry.
        "--pull".to_string(),
        "never".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "--read-only".to_string(),
        "--tmpfs".to_string(),
        "/tmp:size=256m".to_string(),
        "--label".to_string(),
        "managed-by=duduclaw".to_string(),
        "--label".to_string(),
        format!("{HOME_LABEL}={}", labels.home),
        "--label".to_string(),
        format!("{DEADLINE_LABEL}={}", labels.deadline_unix),
    ];

    // No privilege gain after start: setuid/setgid binaries and file
    // capabilities stop working inside the container. The entrypoint only
    // ever drops privileges (`setpriv`), which this does not affect.
    args.extend(["--security-opt".to_string(), "no-new-privileges".to_string()]);

    // Resource limits (prevent single container from exhausting host)
    args.extend(["--cpus".to_string(), "1".to_string()]);
    args.extend(["--memory".to_string(), "512m".to_string()]);
    args.extend(["--pids-limit".to_string(), CONTAINER_PIDS_LIMIT.to_string()]);

    // Pinned egress (tool-driven sessions with a navigation allowlist): the
    // gateway resolved and vetted every host; the container may reach only
    // those addresses on TCP 443 (`domain-filter.sh` ALLOWED_IPS mode) and
    // resolves names through `/etc/hosts` only. Entries that are not exact
    // hostnames are skipped (defence in depth; the caller normalized them).
    let pinned: Vec<&PinnedHost> = config
        .pinned_hosts
        .iter()
        .filter(|p| {
            duduclaw_core::types::normalize_navigation_host(&p.host).as_deref() == Some(p.host.as_str())
        })
        .collect();
    if !pinned.is_empty() {
        // The default bridge explicitly, whatever the daemon's default
        // network: on a user-defined network Docker's embedded DNS answers
        // on 127.0.0.11 (domain-filter.sh also refuses it).
        args.extend(["--network".to_string(), "bridge".to_string()]);
        args.push("--cap-add=NET_ADMIN".to_string());
        let mut ips: Vec<String> = Vec::new();
        for p in &pinned {
            args.extend(["--add-host".to_string(), format!("{}:{}", p.host, p.ip)]);
            let ip = p.ip.to_string();
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
        args.extend([
            "-e".to_string(),
            format!("DISPLAY_SIZE={}x{}", config.display_width, config.display_height),
            "-e".to_string(),
            "DISPLAY=:99".to_string(),
            "-e".to_string(),
            format!("ALLOWED_IPS={}", ips.join(",")),
        ]);
        args.push(config.container_image.clone());
        return args;
    }

    // Network isolation — fail-closed (I5): see `should_isolate_network`.
    let isolate = should_isolate_network(config.network_access, valid_domains);
    if isolate {
        args.push("--network=none".to_string());
    } else {
        // Filtered egress: the container's `domain-filter.sh` installs a
        // default-deny iptables policy plus per-domain allow rules, which
        // needs NET_ADMIN (Docker does not grant it by default). Without it the
        // script refuses to start (fail closed). Granted ONLY in this case —
        // the `--network=none` path never gets the capability.
        args.push("--cap-add=NET_ADMIN".to_string());
    }

    // Environment variables for the entrypoint
    args.extend([
        "-e".to_string(),
        format!("DISPLAY_SIZE={}x{}", config.display_width, config.display_height),
        "-e".to_string(),
        "DISPLAY=:99".to_string(),
    ]);

    // Domain filtering (only when network is allowed AND a valid allowlist
    // survived validation). When empty, the `--network=none` branch above
    // already denied all egress, so no env var is needed.
    if !isolate {
        args.extend([
            "-e".to_string(),
            format!("ALLOWED_DOMAINS={}", valid_domains.join(",")),
        ]);
    }

    args.push(config.container_image.clone());
    args
}

/// Validate xdotool key string — only alphanumeric, `+`, `_`, `-` allowed.
pub(crate) fn validate_xdotool_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-_".contains(c))
        && !key.starts_with('-')
}

/// Convert a `ComputerAction` to docker exec + xdotool arguments.
fn action_to_docker_args(container: &str, action: &ComputerAction) -> Vec<String> {
    let base = vec!["exec".to_string(), container.to_string()];

    match action {
        ComputerAction::LeftClick { coordinate: [x, y] } => {
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                x.to_string(),
                y.to_string(),
                "click".to_string(),
                "1".to_string(),
            ]);
            args
        }
        ComputerAction::RightClick { coordinate: [x, y] } => {
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                x.to_string(),
                y.to_string(),
                "click".to_string(),
                "3".to_string(),
            ]);
            args
        }
        ComputerAction::DoubleClick { coordinate: [x, y] } => {
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                x.to_string(),
                y.to_string(),
                "click".to_string(),
                "--repeat".to_string(),
                "2".to_string(),
                "1".to_string(),
            ]);
            args
        }
        ComputerAction::Type { text } => {
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "type".to_string(),
                "--clearmodifiers".to_string(),
                "--".to_string(),
                text.clone(),
            ]);
            args
        }
        ComputerAction::Key { text } => {
            let mut args = base;
            // SEC: validate key string and add "--" to prevent xdotool flag injection
            let safe_key = if validate_xdotool_key(text) {
                text.clone()
            } else {
                warn!(key = %text, "Invalid xdotool key string, sanitizing");
                "Escape".to_string() // safe fallback
            };
            args.extend([
                "xdotool".to_string(),
                "key".to_string(),
                "--clearmodifiers".to_string(),
                "--".to_string(),
                safe_key,
            ]);
            args
        }
        ComputerAction::Scroll {
            coordinate: [x, y],
            direction,
            amount,
        } => {
            // xdotool click button 4=up, 5=down
            let button = if direction == "up" { "4" } else { "5" };
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                x.to_string(),
                y.to_string(),
                "click".to_string(),
                "--repeat".to_string(),
                amount.to_string(),
                button.to_string(),
            ]);
            args
        }
        ComputerAction::MouseMove { coordinate: [x, y] } => {
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                x.to_string(),
                y.to_string(),
            ]);
            args
        }
        // Screenshot and Wait are handled specially in execute_action
        ComputerAction::Screenshot | ComputerAction::Wait { .. } => base,
        ComputerAction::Zoom { coordinate } => {
            // Zoom is not directly supported by xdotool — simulate via keyboard
            let cx = (coordinate[0] + coordinate[2]) / 2;
            let cy = (coordinate[1] + coordinate[3]) / 2;
            let mut args = base;
            args.extend([
                "xdotool".to_string(),
                "mousemove".to_string(),
                cx.to_string(),
                cy.to_string(),
                "key".to_string(),
                "--".to_string(),
                "ctrl+plus".to_string(),
            ]);
            args
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn action_to_docker_args_left_click() {
        let args = action_to_docker_args(
            "test-container",
            &ComputerAction::LeftClick {
                coordinate: [100, 200],
            },
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "test-container",
                "xdotool",
                "mousemove",
                "100",
                "200",
                "click",
                "1",
            ]
        );
    }

    #[test]
    fn action_to_docker_args_type_text() {
        let args = action_to_docker_args(
            "c1",
            &ComputerAction::Type {
                text: "hello".to_string(),
            },
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "c1",
                "xdotool",
                "type",
                "--clearmodifiers",
                "--",
                "hello",
            ]
        );
    }

    #[test]
    fn action_to_docker_args_key() {
        let args = action_to_docker_args(
            "c1",
            &ComputerAction::Key {
                text: "ctrl+s".to_string(),
            },
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "c1",
                "xdotool",
                "key",
                "--clearmodifiers",
                "--",
                "ctrl+s",
            ]
        );
    }

    #[test]
    fn action_to_docker_args_scroll_down() {
        let args = action_to_docker_args(
            "c1",
            &ComputerAction::Scroll {
                coordinate: [50, 100],
                direction: "down".to_string(),
                amount: 3,
            },
        );
        assert_eq!(
            args,
            vec![
                "exec",
                "c1",
                "xdotool",
                "mousemove",
                "50",
                "100",
                "click",
                "--repeat",
                "3",
                "5",
            ]
        );
    }

    #[test]
    fn action_to_docker_args_scroll_up() {
        let args = action_to_docker_args(
            "c1",
            &ComputerAction::Scroll {
                coordinate: [50, 100],
                direction: "up".to_string(),
                amount: 2,
            },
        );
        assert!(args.contains(&"4".to_string())); // button 4 = up
    }

    #[test]
    fn action_to_docker_args_double_click() {
        let args = action_to_docker_args(
            "c1",
            &ComputerAction::DoubleClick {
                coordinate: [300, 400],
            },
        );
        assert!(args.contains(&"--repeat".to_string()));
        assert!(args.contains(&"2".to_string()));
    }

    #[test]
    fn default_config_values() {
        let cfg = ComputerUseConfig::default();
        assert_eq!(cfg.max_session_minutes, 10);
        assert_eq!(cfg.max_actions, 50);
        assert_eq!(cfg.display_width, 1280);
        assert_eq!(cfg.display_height, 800);
        assert!(!cfg.network_access);
        // Security defaults
        assert!(!cfg.auto_confirm_trusted); // deny-by-default
    }

    #[test]
    fn validate_image_name_accepts_valid() {
        assert!(validate_image_name("duduclaw-computer-use:latest").is_ok());
        assert!(validate_image_name("my-registry.com/image:v1.0").is_ok());
    }

    #[test]
    fn validate_image_name_rejects_injection() {
        assert!(validate_image_name("ubuntu --privileged").is_err());
        assert!(validate_image_name("image; rm -rf /").is_err());
        assert!(validate_image_name("").is_err());
    }

    // ── P0-4: egress allowlist validation + fail-closed network decision ──────

    #[test]
    fn valid_egress_domains_filters_invalid() {
        let raw = vec![
            "example.com".to_string(),
            "*.gov.tw".to_string(),
            "127.0.0.1".to_string(),     // IP-literal → dropped
            "evil.com/path".to_string(), // URL structure → dropped
            "exa%2ecom".to_string(),     // percent-encoding → dropped
        ];
        let ok = valid_egress_domains(&raw);
        assert_eq!(ok, vec!["example.com".to_string(), "*.gov.tw".to_string()]);
    }

    #[test]
    fn isolate_network_when_disabled() {
        assert!(should_isolate_network(false, &["example.com".to_string()]));
    }

    #[test]
    fn isolate_network_when_allowlist_empty_even_if_enabled() {
        // The fail-open case the P0-4 fix closes: network on, no valid domain.
        assert!(should_isolate_network(true, &[]));
        // All-invalid entries collapse to empty → still isolate.
        let valid = valid_egress_domains(&["999.999.999.999".to_string(), "a b".to_string()]);
        assert!(should_isolate_network(true, &valid));
    }

    #[test]
    fn allow_network_when_valid_domains_present() {
        assert!(!should_isolate_network(true, &["example.com".to_string()]));
    }

    // ── docker run argv: NET_ADMIN only on the filtered-egress path ──────────

    #[test]
    fn docker_run_args_no_network_has_no_net_admin() {
        let cfg = ComputerUseConfig::default();
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &[], &test_labels());
        assert!(args.contains(&"--network=none".to_string()));
        assert!(!args.iter().any(|a| a.contains("NET_ADMIN") || a == "--cap-add"));
        assert!(!args.iter().any(|a| a.starts_with("ALLOWED_DOMAINS=")));
        let pids = args.iter().position(|a| a == "--pids-limit").unwrap();
        assert_eq!(args[pids + 1], "512");
        assert_eq!(args.last().unwrap(), &cfg.container_image);
        assert_never_pulls(&args);
    }

    #[test]
    fn docker_run_args_network_on_but_no_valid_domain_stays_isolated() {
        let cfg = ComputerUseConfig {
            network_access: true,
            allowed_domains: vec!["127.0.0.1".to_string()],
            ..Default::default()
        };
        let valid = valid_egress_domains(&cfg.allowed_domains);
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &valid, &test_labels());
        assert!(args.contains(&"--network=none".to_string()));
        assert!(!args.iter().any(|a| a.contains("NET_ADMIN")));
    }

    #[test]
    fn docker_run_args_filtered_network_gets_net_admin() {
        let cfg = ComputerUseConfig {
            network_access: true,
            allowed_domains: vec!["example.com".to_string(), "*.gov.tw".to_string()],
            ..Default::default()
        };
        let valid = valid_egress_domains(&cfg.allowed_domains);
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &valid, &test_labels());
        assert!(!args.contains(&"--network=none".to_string()));
        assert_eq!(args.iter().filter(|a| a.contains("NET_ADMIN")).count(), 1);
        assert!(args.contains(&"--cap-add=NET_ADMIN".to_string()));
        assert!(args.contains(&"ALLOWED_DOMAINS=example.com,*.gov.tw".to_string()));
        assert_eq!(args.last().unwrap(), &cfg.container_image);
        assert_never_pulls(&args);
    }

    // ── docker run argv: pinned navigation hosts (tool-driven sessions) ──────

    fn pinned(host: &str, ip: [u8; 4]) -> PinnedHost {
        PinnedHost { host: host.into(), ip: std::net::Ipv4Addr::from(ip) }
    }

    #[test]
    fn docker_run_args_pinned_hosts_mode() {
        let cfg = ComputerUseConfig {
            pinned_hosts: vec![
                pinned("example.com", [93, 184, 215, 14]),
                pinned("docs.example.com", [93, 184, 215, 14]),
                pinned("other.example.org", [1, 1, 1, 1]),
            ],
            ..Default::default()
        };
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &[], &test_labels());
        assert!(!args.contains(&"--network=none".to_string()));
        // S2: the default bridge, named explicitly (no embedded DNS there).
        assert!(args.windows(2).any(|w| w[0] == "--network" && w[1] == "bridge"), "{args:?}");
        assert_eq!(args.iter().filter(|a| a.starts_with("--network")).count(), 1);
        assert_eq!(args.iter().filter(|a| *a == "--cap-add=NET_ADMIN").count(), 1);
        let add_hosts: Vec<&String> =
            args.windows(2).filter(|w| w[0] == "--add-host").map(|w| &w[1]).collect();
        assert_eq!(
            add_hosts,
            vec!["example.com:93.184.215.14", "docs.example.com:93.184.215.14", "other.example.org:1.1.1.1"]
        );
        // De-duplicated IP list, and never the in-container-resolution mode.
        assert!(args.contains(&"ALLOWED_IPS=93.184.215.14,1.1.1.1".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("ALLOWED_DOMAINS=")));
        assert!(args.contains(&"DISPLAY_SIZE=1280x800".to_string()));
        assert_eq!(args.last().unwrap(), &cfg.container_image);
        assert_never_pulls(&args);
    }

    #[test]
    fn docker_run_args_pinned_mode_wins_over_the_domain_mode() {
        let cfg = ComputerUseConfig {
            network_access: true,
            allowed_domains: vec!["evil.example".into()],
            pinned_hosts: vec![pinned("example.com", [93, 184, 215, 14])],
            ..Default::default()
        };
        let valid = valid_egress_domains(&cfg.allowed_domains);
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &valid, &test_labels());
        assert!(!args.iter().any(|a| a.starts_with("ALLOWED_DOMAINS=")));
        assert!(args.contains(&"ALLOWED_IPS=93.184.215.14".to_string()));
    }

    #[test]
    fn docker_run_args_invalid_pinned_hosts_fall_back_to_no_network() {
        let cfg = ComputerUseConfig {
            pinned_hosts: vec![
                pinned("*.example.com", [93, 184, 215, 14]),
                pinned("Example.com", [93, 184, 215, 14]),
                pinned("example.com:1", [93, 184, 215, 14]),
            ],
            ..Default::default()
        };
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &[], &test_labels());
        assert!(args.contains(&"--network=none".to_string()));
        assert!(!args.iter().any(|a| a.contains("NET_ADMIN") || a == "--add-host"));
        assert!(!args.iter().any(|a| a.starts_with("ALLOWED_IPS=")));
    }

    #[test]
    fn navigate_output_takes_the_last_json_line() {
        let ok = parse_navigate_output(b"noise\n{\"ok\":true,\"host\":\"example.com\",\"error\":null}\n\n").unwrap();
        assert_eq!(ok, NavigateOutcome { ok: true, host: Some("example.com".into()), error: None });
        let bad = parse_navigate_output(b"{\"ok\":false,\"host\":null,\"error\":\"net::ERR_NAME_NOT_RESOLVED\"}").unwrap();
        assert!(!bad.ok);
        assert_eq!(bad.error.as_deref(), Some("net::ERR_NAME_NOT_RESOLVED"));
        assert!(parse_navigate_output(b"").is_none());
        assert!(parse_navigate_output(b"<html>").is_none());
        assert!(parse_navigate_output(&[0xff, 0xfe]).is_none());
    }

    fn test_labels() -> ContainerLabels {
        ContainerLabels { home: "0123456789abcdef0123456789abcdef".into(), deadline_unix: 1_900_000_000 }
    }

    #[test]
    fn docker_run_args_carry_home_and_deadline_labels_before_the_image() {
        let cfg = ComputerUseConfig::default();
        let args = build_docker_run_args("duduclaw-cu-test", &cfg, &[], &test_labels());
        let home = format!("{HOME_LABEL}=0123456789abcdef0123456789abcdef");
        let deadline = format!("{DEADLINE_LABEL}=1900000000");
        for label in [&home, &deadline] {
            let at = args.iter().position(|a| a == label).expect("label present");
            assert_eq!(args[at - 1], "--label");
            assert!(at < args.len() - 1, "labels must precede the image");
        }
    }

    #[test]
    fn home_label_is_stable_and_hex() {
        let tmp = tempfile::tempdir().unwrap();
        let a = computer_use_home_label(tmp.path());
        assert_eq!(a, computer_use_home_label(tmp.path()));
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    /// `--pull never` sits among the options (before the image positional).
    fn assert_never_pulls(args: &[String]) {
        let pull = args.iter().position(|a| a == "--pull").expect("--pull present");
        assert_eq!(args[pull + 1], "never");
        assert!(pull + 1 < args.len() - 1, "--pull never must precede the image");
        assert_eq!(args.iter().filter(|a| a.as_str() == "--pull").count(), 1);
        // S6: every computer-use container runs with no-new-privileges.
        assert!(
            args.windows(2).any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"),
            "--security-opt no-new-privileges missing: {args:?}"
        );
    }

    #[test]
    fn default_image_is_the_versioned_published_one() {
        let cfg = ComputerUseConfig::default();
        assert_eq!(cfg.container_image, crate::computer_use_image::default_image());
        assert!(!cfg.container_image.ends_with(":latest"));
        assert!(validate_image_name(&cfg.container_image).is_ok());
    }

    #[test]
    fn validate_image_name_accepts_digest_references() {
        assert!(validate_image_name(
            "ghcr.io/x/cu@sha256:be45dfabcca9ecb7c783fd06b08a98bc33ab37255684cf14bed46327f8d98776"
        )
        .is_ok());
    }

    // ── Window-title rule (fail closed) ───────────────────────────────────────

    fn white_png_b64(w: u32, h: u32) -> String {
        use image::ImageEncoder;
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(&buf)
    }

    fn fully_masked(b64: &str) -> bool {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
        img.pixels().all(|p| *p == image::Rgba([0, 0, 0, 255]))
    }

    fn orchestrator() -> ComputerUseOrchestrator {
        ComputerUseOrchestrator::new(
            "test-agent".into(),
            std::env::temp_dir(),
            ComputerUseConfig::default(),
        )
    }

    #[test]
    fn title_read_error_masks_the_whole_screenshot() {
        let orch = orchestrator();
        let shot = white_png_b64(8, 6);
        for reason in [
            "Active window probe timed out after 5s",
            "title probe exited with status 1",
            "title probe printed non-UTF-8 output",
        ] {
            let out = orch
                .apply_window_title_rule(shot.clone(), &Err(reason.to_string()))
                .unwrap();
            assert!(fully_masked(&out.png_base64), "{reason}");
            assert_eq!(out.full_mask, Some(FullMaskReason::TitleUnreadable), "{reason}");
        }
    }

    #[test]
    fn readable_non_sensitive_title_leaves_the_screenshot_unchanged() {
        let orch = orchestrator();
        let shot = white_png_b64(8, 6);
        for title in ["Example Domain - Chromium", ""] {
            let out = orch.apply_window_title_rule(shot.clone(), &Ok(title.to_string())).unwrap();
            assert_eq!(out, MaskedScreenshot { png_base64: shot.clone(), full_mask: None }, "{title:?}");
            assert!(!out.fully_masked());
        }
    }

    #[test]
    fn sensitive_title_masks_the_whole_screenshot() {
        let orch = orchestrator();
        let shot = white_png_b64(8, 6);
        let out = orch
            .apply_window_title_rule(shot, &Ok("Bitwarden — Chromium".to_string()))
            .unwrap();
        assert!(fully_masked(&out.png_base64));
        assert_eq!(out.full_mask, Some(FullMaskReason::TitleSensitive));
    }

    /// The title rule runs after DOM regions were masked too: a credential
    /// window or an unreadable title still masks everything.
    #[test]
    fn title_rule_applies_after_dom_regions_were_masked() {
        let orch = orchestrator();
        let shot = white_png_b64(8, 6);
        let regions = || Some(Ok(vec![[0_u32, 0, 2, 2]]));
        let sensitive = orch
            .apply_masks(shot.clone(), regions(), &Ok("Bitwarden — Chromium".to_string()))
            .unwrap();
        assert!(fully_masked(&sensitive.png_base64));
        assert_eq!(sensitive.full_mask, Some(FullMaskReason::TitleSensitive));
        let unreadable = orch
            .apply_masks(shot.clone(), regions(), &Err("probe timed out".to_string()))
            .unwrap();
        assert!(fully_masked(&unreadable.png_base64));
        assert_eq!(unreadable.full_mask, Some(FullMaskReason::TitleUnreadable));
        // A clear title keeps the partial mask only, and says so.
        let partial = orch
            .apply_masks(shot.clone(), regions(), &Ok("Example - Chromium".to_string()))
            .unwrap();
        assert!(!fully_masked(&partial.png_base64));
        assert_ne!(partial.png_base64, shot);
        assert_eq!(partial.full_mask, None);
        // A detection error masks everything; no detection leaves the title rule.
        let failed = orch
            .apply_masks(shot.clone(), Some(Err(RegionDetectFailure::Failed("x".into()))), &Ok(String::new()))
            .unwrap();
        assert!(fully_masked(&failed.png_base64));
        assert_eq!(failed.full_mask, Some(FullMaskReason::HelperFailed));
        assert_eq!(
            orch.apply_masks(shot.clone(), None, &Ok(String::new())).unwrap(),
            MaskedScreenshot { png_base64: shot.clone(), full_mask: None }
        );
    }

    /// Every full-mask path maps to exactly one closed code; a detection
    /// failure wins over the title (the title is not consulted then).
    #[test]
    fn each_full_mask_path_carries_its_reason_code() {
        let orch = orchestrator();
        let shot = white_png_b64(8, 6);
        let cases: [(Option<Result<Vec<[u32; 4]>, RegionDetectFailure>>, Result<String, String>, &str); 5] = [
            (Some(Err(RegionDetectFailure::SeveralPages)), Ok("Example".into()), "several_pages"),
            (Some(Err(RegionDetectFailure::SeveralPages)), Ok("Bitwarden".into()), "several_pages"),
            (Some(Err(RegionDetectFailure::Failed("timed out".into()))), Err("x".into()), "helper_failed"),
            (Some(Ok(Vec::new())), Ok("ssh key — Chromium".into()), "title_sensitive"),
            (None, Err("title probe exited with status 1".into()), "title_unreadable"),
        ];
        for (regions, title, code) in cases {
            let out = orch.apply_masks(shot.clone(), regions, &title).unwrap();
            assert!(out.fully_masked() && fully_masked(&out.png_base64), "{code}");
            assert_eq!(out.full_mask.map(FullMaskReason::code), Some(code));
        }
    }

    #[cfg(unix)]
    #[test]
    fn title_from_output_classifies_exit_and_encoding() {
        use std::os::unix::process::ExitStatusExt;
        let out = |code: i32, stdout: &[u8]| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
        };
        assert_eq!(title_from_output(&out(0, b"  Page title \n")), Ok("Page title".to_string()));
        assert_eq!(title_from_output(&out(0, b"\n")), Ok(String::new()));
        assert!(title_from_output(&out(1, b"Page")).is_err());
        assert!(title_from_output(&out(0, &[0xff, 0xfe, b'a'])).is_err());
        assert_eq!(title_verdict(&Ok(String::new())), TitleVerdict::Clear);
        assert_eq!(title_verdict(&Err("x".into())), TitleVerdict::Unreadable);
    }

    #[tokio::test]
    async fn container_mode_without_a_container_is_unreadable_not_clear() {
        let orch = orchestrator();
        assert!(orch.read_active_window_title().await.is_err());
    }

    /// Real Docker, opt-in. Run with
    /// `DUDU_COMPUTER_USE_IMAGE=duduclaw-computer-use:latest cargo test -p duduclaw-gateway --lib
    /// --no-default-features -- --ignored real_docker_computer_use_session`.
    ///
    /// Starts a container through `start_container` (presence check, the real
    /// `build_docker_run_args` argv with `--pull never` and `--network=none`,
    /// display readiness), runs the DOM helper, reads the window title,
    /// takes a masked screenshot through `capture_masked_screenshot_detailed`
    /// and removes the container before asserting. No model is involved.
    #[tokio::test]
    #[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
    async fn real_docker_computer_use_session() {
        let image = std::env::var("DUDU_COMPUTER_USE_IMAGE")
            .expect("set DUDU_COMPUTER_USE_IMAGE, e.g. duduclaw-computer-use:latest");

        // A missing image fails the start with the operator-safe text.
        let mut absent = ComputerUseOrchestrator::new(
            "real-docker-test".into(),
            std::env::temp_dir(),
            ComputerUseConfig {
                container_image: "duduclaw-real-docker-test/absent:never".into(),
                ..Default::default()
            },
        );
        let absent_err = absent.start_container().await;

        let mut orch = ComputerUseOrchestrator::new(
            "real-docker-test".into(),
            std::env::temp_dir(),
            ComputerUseConfig { container_image: image.clone(), ..Default::default() },
        );
        let started = orch.start_container().await;
        let container = orch.container_id.clone();
        let (dom, title, shot) = if started.is_ok() {
            let name = container.clone().unwrap();
            let dom = docker_output(
                &["exec", name.as_str(), "duduclaw-eval-dom", "JSON.stringify([])"],
                Duration::from_secs(30),
                "DOM helper",
            )
            .await;
            let title = orch.read_active_window_title().await;
            let shot = orch.capture_masked_screenshot_detailed().await.map(|s| s.png_base64);
            // S7: the URL goes over stdin; without network it fails cleanly.
            let nav = orch.navigate("https://example.com/?q=1").await;
            // The argv form is refused by the helper.
            let argv_nav = docker_output(
                &["exec", name.as_str(), "duduclaw-navigate", "https://example.com/"],
                Duration::from_secs(30),
                "argv navigate",
            )
            .await;
            (Some(dom), Some(title), Some((shot, nav, argv_nav)))
        } else {
            (None, None, None)
        };
        orch.stop_session().await;

        match absent_err {
            Err(ComputerUseError::Unavailable(m)) => {
                assert!(m.contains("docker pull duduclaw-real-docker-test/absent:never"), "{m}")
            }
            other => panic!("absent image must be Unavailable, got {other:?}"),
        }
        started.expect("session start");
        let dom = dom.unwrap().expect("docker exec duduclaw-eval-dom");
        assert!(dom.status.success(), "DOM helper exit: {:?}", dom.status);
        assert_eq!(String::from_utf8_lossy(&dom.stdout).trim(), "[]");
        let title = title.unwrap().expect("window title readable");
        assert!(!window_is_sensitive(Some(&title)), "{title}");
        let (shot, nav, argv_nav) = shot.unwrap();
        let nav = nav.expect("navigate helper ran and printed one JSON line");
        assert!(!nav.ok, "no network: navigation must fail, got {nav:?}");
        assert!(nav.error.as_deref().is_some_and(|e| e != "usage" && e != "bad_url"), "{nav:?}");
        let argv_nav = argv_nav.expect("docker exec duduclaw-navigate <url>");
        assert_eq!(argv_nav.status.code(), Some(2), "argv form must be refused");
        assert_eq!(
            parse_navigate_output(&argv_nav.stdout).and_then(|o| o.error),
            Some("usage".to_string())
        );
        let shot = shot.expect("masked screenshot");
        let bytes = base64::engine::general_purpose::STANDARD.decode(&shot).unwrap();
        let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (1280, 800));
        // DOM detection and the title read both succeeded, so nothing forced
        // a full-screen mask.
        assert!(!fully_masked(&shot), "screenshot came back fully masked");
        let gone = docker_output(
            &["ps", "-a", "-q", "--filter", &format!("name={}", container.unwrap())],
            Duration::from_secs(15),
            "ps",
        )
        .await
        .unwrap();
        assert!(gone.stdout.trim_ascii().is_empty(), "container was not removed");
    }

    /// Real Docker, opt-in. Run with
    /// `DUDU_COMPUTER_USE_IMAGE=duduclaw-computer-use:latest cargo test -p duduclaw-gateway --lib
    /// --no-default-features -- --ignored real_docker_pointer_actions`.
    ///
    /// Pointer actions at the pointer's current position used to wait
    /// ~18 s each (`xdotool mousemove --sync` waits for a move event that
    /// never comes). Two identical clicks and two identical scrolls must
    /// each finish well under 5 s, and a click after a move still lands at
    /// its own coordinate: a capturing click listener (installed through the
    /// DOM helper, in its isolated world) records `clientX/clientY` on the
    /// page, read back through the helper.
    #[tokio::test]
    #[ignore = "needs Docker and a local computer-use image (DUDU_COMPUTER_USE_IMAGE)"]
    async fn real_docker_pointer_actions_at_one_spot_are_fast_and_land() {
        let image = std::env::var("DUDU_COMPUTER_USE_IMAGE")
            .expect("set DUDU_COMPUTER_USE_IMAGE, e.g. duduclaw-computer-use:latest");
        let mut orch = ComputerUseOrchestrator::new(
            "real-docker-pointer-test".into(),
            std::env::temp_dir(),
            ComputerUseConfig { container_image: image, ..Default::default() },
        );
        let started = orch.start_container().await;
        let mut timings: Vec<(String, Duration)> = Vec::new();
        let mut outcome: Result<Option<String>, String> = Ok(None);
        if started.is_ok() {
            let name = orch.container_id.clone().unwrap();
            let eval = |js: &'static str| {
                let name = name.clone();
                async move {
                    let out = docker_output(
                        &["exec", name.as_str(), "duduclaw-eval-dom", js],
                        Duration::from_secs(30),
                        "DOM helper",
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    if !out.status.success() {
                        return Err(format!(
                            "helper exit {:?}: {}",
                            out.status.code(),
                            String::from_utf8_lossy(&out.stderr).trim()
                        ));
                    }
                    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
                }
            };
            outcome = async {
                eval(
                    "(document.addEventListener('click', e => document.documentElement\
                     .setAttribute('data-dd-click', e.clientX + ',' + e.clientY), true), 'ok')",
                )
                .await?;
                let at = [300_u32, 300];
                let actions = [
                    ("click 1", ComputerAction::LeftClick { coordinate: at }),
                    ("click 2 (same spot)", ComputerAction::LeftClick { coordinate: at }),
                    (
                        "scroll 1",
                        ComputerAction::Scroll { coordinate: at, direction: "down".into(), amount: 2 },
                    ),
                    (
                        "scroll 2 (same spot)",
                        ComputerAction::Scroll { coordinate: at, direction: "down".into(), amount: 2 },
                    ),
                ];
                for (label, action) in actions {
                    let t = std::time::Instant::now();
                    orch.execute_action(&action).await.map_err(|e| format!("{label}: {e}"))?;
                    timings.push((label.to_string(), t.elapsed()));
                }
                // A click somewhere else after the pointer sat at 300,300.
                orch.execute_action(&ComputerAction::LeftClick { coordinate: [517, 211] })
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::time::sleep(Duration::from_millis(300)).await;
                eval("document.documentElement.getAttribute('data-dd-click')").await.map(Some)
            }
            .await;
        }
        orch.stop_session().await;

        started.expect("session start");
        let recorded = outcome.expect("pointer actions ran");
        for (label, took) in &timings {
            assert!(*took < Duration::from_secs(5), "{label} took {took:?}");
        }
        assert_eq!(timings.len(), 4, "{timings:?}");
        assert_eq!(recorded.as_deref(), Some("517,211"), "click landed elsewhere; timings {timings:?}");
    }

    #[test]
    fn validate_xdotool_key_accepts_valid() {
        assert!(validate_xdotool_key("ctrl+s"));
        assert!(validate_xdotool_key("Return"));
        assert!(validate_xdotool_key("alt+Tab"));
        assert!(validate_xdotool_key("F12"));
    }

    #[test]
    fn validate_xdotool_key_rejects_injection() {
        assert!(!validate_xdotool_key("--window 0 ctrl+c"));
        assert!(!validate_xdotool_key("-flag"));
        assert!(!validate_xdotool_key(""));
    }

    #[test]
    fn action_to_semantic_string_type() {
        let s = action_to_semantic_string(&ComputerAction::Type {
            text: "Hello World".to_string(),
        });
        assert!(s.contains("type"));
        assert!(s.contains("hello world")); // lowercased
    }

    #[test]
    fn action_to_semantic_string_key() {
        let s = action_to_semantic_string(&ComputerAction::Key {
            text: "ctrl+s".to_string(),
        });
        assert!(s.contains("key press"));
        assert!(s.contains("ctrl+s"));
    }

    #[tokio::test]
    async fn session_registry_max_limit() {
        // Register MAX sessions
        for i in 0..MAX_CONCURRENT_SESSIONS {
            let ctl = Arc::new(OrchestratorControl::new());
            register_session(&format!("test-sess-{i}"), ctl)
                .await
                .unwrap();
        }
        // Next one should fail
        let ctl = Arc::new(OrchestratorControl::new());
        assert!(register_session("overflow", ctl).await.is_err());
        // Cleanup
        for i in 0..MAX_CONCURRENT_SESSIONS {
            unregister_session(&format!("test-sess-{i}")).await;
        }
    }

    #[test]
    fn orchestrator_control_defaults() {
        let ctl = OrchestratorControl::new();
        assert!(!ctl.paused.load(Ordering::Relaxed));
        assert!(!ctl.stopped.load(Ordering::Relaxed));
    }
}
