//! L5 Computer Use orchestrator — drives the screenshot→vision→action loop
//! inside a container sandbox, reporting progress back to the messaging channel.
//!
//! This module bridges `computer_use.rs` (API client) with `duduclaw-container`
//! (container lifecycle) and `channel_sender.rs` (channel-native feedback).
//!
//! All authorization and monitoring happens via the messaging channel — no
//! Dashboard required.  The user can `/pause`, `/stop`, `/screenshot`, or
//! send a natural-language stop word from their phone.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::channel_sender::ChannelSender;
use crate::computer_use::{
    mask_screenshot_regions, ComputerAction, ComputerUseError, ComputerUseSession, MaskingConfig,
    Message,
};
use crate::risk_detector::{self, ActionContext, RiskLevel};
use crate::screenshot_audit::{AuditEntry, BrowserAuditLog};

use duduclaw_core::types::ComputerUseMode;

// ---------------------------------------------------------------------------
// Global session registry (for MCP tool → orchestrator wiring)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// Global registry of active computer use sessions, keyed by session_id.
///
/// This allows MCP tools (`computer_click`, etc.) to route commands to the
/// correct running orchestrator without passing it through the call stack.
static SESSION_REGISTRY: std::sync::OnceLock<
    tokio::sync::Mutex<HashMap<String, Arc<OrchestratorControl>>>,
> = std::sync::OnceLock::new();

fn session_registry() -> &'static tokio::sync::Mutex<HashMap<String, Arc<OrchestratorControl>>> {
    SESSION_REGISTRY.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// Maximum concurrent computer use sessions (prevents DoS).
const MAX_CONCURRENT_SESSIONS: usize = 5;

/// Register an orchestrator's control handle in the global registry.
///
/// Returns `Err` if the maximum concurrent session limit is reached.
pub async fn register_session(session_id: &str, control: Arc<OrchestratorControl>) -> Result<(), ComputerUseError> {
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
    /// Screenshot interval: when to send screenshots to channel.
    pub screenshot_interval: ScreenshotInterval,
    /// Automatically confirm trusted operations without channel confirmation.
    pub auto_confirm_trusted: bool,
    /// Allowed applications (empty = all allowed).
    pub allowed_apps: Vec<String>,
    /// Blocked action types.
    pub blocked_actions: Vec<String>,
    /// Consecutive failure threshold before auto-pause.
    pub max_consecutive_failures: u32,
    /// Container image to use.
    pub container_image: String,
    /// Network access mode for the container.
    pub network_access: bool,
    /// Allowed domains (only when network_access=true).
    pub allowed_domains: Vec<String>,
    /// Execution mode: Container (L5a) or Native (L5b).
    pub execution_mode: ComputerUseMode,
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
            screenshot_interval: ScreenshotInterval::AfterOnly,
            auto_confirm_trusted: false,
            allowed_apps: Vec::new(),
            blocked_actions: vec![
                "delete_file".to_string(),
                "terminal".to_string(),
                "system_preferences".to_string(),
            ],
            max_consecutive_failures: 3,
            container_image: "duduclaw-computer-use:latest".to_string(),
            network_access: false,
            allowed_domains: Vec::new(),
            execution_mode: ComputerUseMode::Container,
            contract_must_not: Vec::new(),
        }
    }
}

/// When to send screenshots to the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotInterval {
    /// Send screenshot before and after each action.
    BeforeAndAfter,
    /// Send screenshot only after each action.
    AfterOnly,
    /// Only send when explicitly requested via /screenshot.
    Manual,
}

impl Default for ScreenshotInterval {
    fn default() -> Self {
        Self::AfterOnly
    }
}

// ---------------------------------------------------------------------------
// Orchestrator state
// ---------------------------------------------------------------------------

/// Shared flags for controlling the orchestrator from channel commands.
#[derive(Debug)]
pub struct OrchestratorControl {
    /// Set to true to pause the loop (waits until cleared).
    pub paused: AtomicBool,
    /// Set to true to stop the loop entirely.
    pub stopped: AtomicBool,
    /// Current consecutive failure count.
    pub consecutive_failures: AtomicU32,
}

impl OrchestratorControl {
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            consecutive_failures: AtomicU32::new(0),
        }
    }
}

/// The main Computer Use orchestrator.
///
/// Drives: container lifecycle → screenshot capture → Claude Vision API →
/// action execution → channel feedback loop.
pub struct ComputerUseOrchestrator {
    /// Container identifier (set after start_session).
    container_id: Option<String>,
    /// Claude API session for computer use.
    session: Option<ComputerUseSession>,
    /// Conversation history for multi-turn.
    conversation: Vec<Message>,
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
    /// Session start time.
    started_at: Option<Instant>,
}

/// SEC: Ensure the container is cleaned up even on panic/task cancellation.
impl Drop for ComputerUseOrchestrator {
    fn drop(&mut self) {
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
    pub fn new(
        agent_id: String,
        home_dir: PathBuf,
        config: ComputerUseConfig,
    ) -> Self {
        Self {
            container_id: None,
            session: None,
            conversation: Vec::new(),
            config,
            agent_id,
            home_dir,
            control: Arc::new(OrchestratorControl::new()),
            masking: MaskingConfig::default(),
            started_at: None,
        }
    }

    /// Get a shareable handle to the control flags.
    pub fn control_handle(&self) -> Arc<OrchestratorControl> {
        Arc::clone(&self.control)
    }

    // ── Container lifecycle ───────────────────────────────────

    /// Start a computer use container and wait for the virtual display.
    pub async fn start_session(&mut self, api_key: &str, model: &str) -> Result<(), ComputerUseError> {
        info!(agent = %self.agent_id, "Starting computer use session");

        // SEC: Validate container image name to prevent docker flag injection
        validate_image_name(&self.config.container_image)?;

        let container_name = format!("duduclaw-cu-{}", uuid::Uuid::new_v4().as_simple());

        // Build docker run command
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            container_name.clone(),
            "--read-only".to_string(),
            "--tmpfs".to_string(),
            "/tmp:size=256m".to_string(),
            "--label".to_string(),
            "managed-by=duduclaw".to_string(),
        ];

        // Resource limits (prevent single container from exhausting host)
        args.extend(["--cpus".to_string(), "1".to_string()]);
        args.extend(["--memory".to_string(), "512m".to_string()]);
        args.extend(["--pids-limit".to_string(), "100".to_string()]);

        // Network isolation
        if !self.config.network_access {
            args.push("--network=none".to_string());
        }

        // Environment variables for the entrypoint
        args.extend([
            "-e".to_string(),
            format!("DISPLAY_SIZE={}x{}", self.config.display_width, self.config.display_height),
            "-e".to_string(),
            "DISPLAY=:99".to_string(),
        ]);

        // Domain filtering (if network is allowed)
        if self.config.network_access && !self.config.allowed_domains.is_empty() {
            args.extend([
                "-e".to_string(),
                format!("ALLOWED_DOMAINS={}", self.config.allowed_domains.join(",")),
            ]);
        }

        args.push(self.config.container_image.clone());

        let output = tokio::process::Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("Failed to start container: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ComputerUseError::ApiError(format!(
                "Container start failed: {stderr}"
            )));
        }

        self.container_id = Some(container_name.clone());

        // Wait for Xvfb to be ready (poll with timeout)
        self.wait_for_display(&container_name).await?;

        // Create the Claude API session
        self.session = Some(ComputerUseSession::with_config(
            api_key.to_string(),
            model.to_string(),
            self.config.display_width,
            self.config.display_height,
            self.config.max_actions,
            Some(self.masking.clone()),
        ));
        self.started_at = Some(Instant::now());

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

            let check = tokio::process::Command::new("docker")
                .args(["exec", container_name, "xdotool", "getactivewindow"])
                .output()
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
            // Best-effort cleanup
            let _ = tokio::process::Command::new("docker")
                .args(["stop", "-t", "5", name])
                .output()
                .await;
            let _ = tokio::process::Command::new("docker")
                .args(["rm", "-f", name])
                .output()
                .await;
        }
        self.container_id = None;
        self.session = None;
        self.started_at = None;
    }

    // ── Screenshot capture ────────────────────────────────────

    /// Capture a screenshot, dispatching to container (L5a) or native (L5b).
    pub async fn capture_screenshot(&self) -> Result<String, ComputerUseError> {
        match self.config.execution_mode {
            ComputerUseMode::Native => self.capture_screenshot_native().await,
            _ => self.capture_screenshot_container().await,
        }
    }

    /// L5a: capture screenshot via `scrot` inside the container.
    async fn capture_screenshot_container(&self) -> Result<String, ComputerUseError> {
        let container = self.container_id.as_deref().ok_or_else(|| {
            ComputerUseError::ApiError("No active container".to_string())
        })?;

        let capture = tokio::process::Command::new("docker")
            .args(["exec", container, "scrot", "-o", "/tmp/screen.png"])
            .output()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("Screenshot capture failed: {e}")))?;

        if !capture.status.success() {
            return Err(ComputerUseError::ApiError(
                "scrot failed inside container".to_string(),
            ));
        }

        let read = tokio::process::Command::new("docker")
            .args(["exec", container, "cat", "/tmp/screen.png"])
            .output()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("Screenshot read failed: {e}")))?;

        if !read.status.success() {
            return Err(ComputerUseError::ApiError(
                "Failed to read screenshot from container".to_string(),
            ));
        }

        Ok(base64::engine::general_purpose::STANDARD.encode(&read.stdout))
    }

    /// L5b: capture screenshot from the host display via `xcap`.
    async fn capture_screenshot_native(&self) -> Result<String, ComputerUseError> {
        // xcap must run on the main thread (macOS AppKit requirement),
        // so we use spawn_blocking.
        let result = tokio::task::spawn_blocking(|| -> Result<String, String> {
            let monitors = xcap::Monitor::all()
                .map_err(|e| format!("No monitors: {e}"))?;
            let monitor = monitors
                .iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .or(monitors.first())
                .ok_or_else(|| "No monitor found".to_string())?;
            let img = monitor
                .capture_image()
                .map_err(|e| format!("Capture failed: {e}"))?;

            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            image::ImageEncoder::write_image(
                encoder,
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("PNG encode: {e}"))?;

            Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
        })
        .await
        .map_err(|e| ComputerUseError::ApiError(format!("spawn_blocking: {e}")))?;

        result.map_err(|e| ComputerUseError::ApiError(e))
    }

    /// Capture screenshot with sensitive region masking applied.
    ///
    /// Works for both L5a (container — DOM-based detection) and L5b (native —
    /// heuristic detection of common password/credential windows).
    pub async fn capture_masked_screenshot(&self) -> Result<String, ComputerUseError> {
        let b64 = self.capture_screenshot().await?;

        // L5a: Detect sensitive regions via DOM queries inside the container
        if let Some(ref container) = self.container_id {
            let regions = crate::computer_use::detect_sensitive_regions(
                container,
                &self.masking.patterns,
            )
            .await
            .unwrap_or_default();

            if !regions.is_empty() {
                return mask_screenshot_regions(&b64, &regions, self.masking.fill_color);
            }
        }

        // L5b (native): Check active window title for known sensitive apps
        if self.config.execution_mode == ComputerUseMode::Native {
            if let Some(ref title) = self.get_active_window_title().await {
                let lower = title.to_lowercase();
                let sensitive_windows = [
                    "1password", "bitwarden", "lastpass", "keepass",
                    "keychain", "密碼", "password", "credential",
                    "ssh", "gpg", "pgp",
                ];
                if sensitive_windows.iter().any(|w| lower.contains(w)) {
                    warn!(window = %title, "Sensitive window detected — masking full screenshot");
                    // Mask the entire screenshot as a safety measure
                    let regions = vec![[0_u32, 0, self.config.display_width, self.config.display_height]];
                    return mask_screenshot_regions(&b64, &regions, self.masking.fill_color);
                }
            }
        }

        Ok(b64)
    }

    // ── Action execution ──────────────────────────────────────

    /// Execute a single `ComputerAction`, dispatching L5a (container) or L5b (native).
    pub async fn execute_action(
        &self,
        action: &ComputerAction,
    ) -> Result<(), ComputerUseError> {
        // Common actions handled the same in both modes
        match action {
            ComputerAction::Screenshot => return Ok(()), // handled separately
            ComputerAction::Wait { duration } => {
                tokio::time::sleep(Duration::from_secs(u64::from(*duration))).await;
                return Ok(());
            }
            _ => {}
        }

        match self.config.execution_mode {
            ComputerUseMode::Native => self.execute_action_native(action).await,
            _ => self.execute_action_container(action).await,
        }
    }

    /// L5a: execute via docker exec + xdotool.
    async fn execute_action_container(
        &self,
        action: &ComputerAction,
    ) -> Result<(), ComputerUseError> {
        let container = self.container_id.as_deref().ok_or_else(|| {
            ComputerUseError::ApiError("No active container".to_string())
        })?;

        let args = action_to_docker_args(container, action);

        let output = tokio::process::Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| ComputerUseError::ApiError(format!("Action execution failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(action = ?action, stderr = %stderr, "xdotool command failed");
            return Err(ComputerUseError::ApiError(format!("xdotool failed: {stderr}")));
        }
        Ok(())
    }

    /// L5b: execute on host via `enigo` (mouse/keyboard) through spawn_blocking.
    async fn execute_action_native(
        &self,
        action: &ComputerAction,
    ) -> Result<(), ComputerUseError> {
        let action = action.clone();
        tokio::task::spawn_blocking(move || {
            use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};

            let mut enigo = Enigo::new(&Settings::default())
                .map_err(|e| ComputerUseError::ApiError(format!("enigo init: {e}")))?;

            match action {
                ComputerAction::LeftClick { coordinate: [x, y] } => {
                    enigo.move_mouse(x as i32, y as i32, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                    enigo.button(Button::Left, Direction::Click)
                        .map_err(|e| ComputerUseError::ApiError(format!("click: {e}")))?;
                }
                ComputerAction::RightClick { coordinate: [x, y] } => {
                    enigo.move_mouse(x as i32, y as i32, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                    enigo.button(Button::Right, Direction::Click)
                        .map_err(|e| ComputerUseError::ApiError(format!("click: {e}")))?;
                }
                ComputerAction::DoubleClick { coordinate: [x, y] } => {
                    enigo.move_mouse(x as i32, y as i32, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                    enigo.button(Button::Left, Direction::Click)
                        .map_err(|e| ComputerUseError::ApiError(format!("click1: {e}")))?;
                    enigo.button(Button::Left, Direction::Click)
                        .map_err(|e| ComputerUseError::ApiError(format!("click2: {e}")))?;
                }
                ComputerAction::Type { ref text } => {
                    enigo.text(text)
                        .map_err(|e| ComputerUseError::ApiError(format!("type: {e}")))?;
                }
                ComputerAction::Key { ref text } => {
                    // Parse key combo and execute
                    let parts: Vec<&str> = text.split('+').collect();
                    for part in &parts[..parts.len().saturating_sub(1)] {
                        if let Some(k) = duduclaw_desktop::controller::parse_enigo_key(part.trim()) {
                            let _ = enigo.key(k, Direction::Press);
                        }
                    }
                    if let Some(last) = parts.last() {
                        if let Some(k) = duduclaw_desktop::controller::parse_enigo_key(last.trim()) {
                            let _ = enigo.key(k, Direction::Click);
                        }
                    }
                    for part in parts[..parts.len().saturating_sub(1)].iter().rev() {
                        if let Some(k) = duduclaw_desktop::controller::parse_enigo_key(part.trim()) {
                            let _ = enigo.key(k, Direction::Release);
                        }
                    }
                }
                ComputerAction::Scroll { coordinate: [x, y], ref direction, amount } => {
                    enigo.move_mouse(x as i32, y as i32, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                    let clicks = if direction == "up" { amount as i32 } else { -(amount as i32) };
                    enigo.scroll(clicks, Axis::Vertical)
                        .map_err(|e| ComputerUseError::ApiError(format!("scroll: {e}")))?;
                }
                ComputerAction::MouseMove { coordinate: [x, y] } => {
                    enigo.move_mouse(x as i32, y as i32, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                }
                ComputerAction::Zoom { coordinate } => {
                    let cx = ((coordinate[0] + coordinate[2]) / 2) as i32;
                    let cy = ((coordinate[1] + coordinate[3]) / 2) as i32;
                    enigo.move_mouse(cx, cy, Coordinate::Abs)
                        .map_err(|e| ComputerUseError::ApiError(format!("mouse_move: {e}")))?;
                    // Simulate ctrl+plus for zoom
                    let _ = enigo.key(enigo::Key::Control, Direction::Press);
                    let _ = enigo.key(enigo::Key::Unicode('+'), Direction::Click);
                    let _ = enigo.key(enigo::Key::Control, Direction::Release);
                }
                _ => {} // Screenshot/Wait already handled above
            }
            Ok(())
        })
        .await
        .map_err(|e| ComputerUseError::ApiError(format!("spawn_blocking: {e}")))?
    }

    /// Query the active window title (for risk detection).
    ///
    /// L5a: `docker exec xdotool getactivewindow getwindowname`
    /// L5b: `xcap::Window` or platform-specific API
    pub async fn get_active_window_title(&self) -> Option<String> {
        match self.config.execution_mode {
            ComputerUseMode::Native => {
                // Use xdotool on Linux, AppleScript on macOS
                #[cfg(target_os = "macos")]
                {
                    let output = tokio::process::Command::new("osascript")
                        .args(["-e", r#"tell application "System Events" to get name of first process whose frontmost is true"#])
                        .output()
                        .await
                        .ok()?;
                    if output.status.success() {
                        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                    } else {
                        None
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let output = tokio::process::Command::new("xdotool")
                        .args(["getactivewindow", "getwindowname"])
                        .output()
                        .await
                        .ok()?;
                    if output.status.success() {
                        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                    } else {
                        None
                    }
                }
            }
            _ => {
                // Container mode: query inside the container
                let container = self.container_id.as_deref()?;
                let output = tokio::process::Command::new("docker")
                    .args(["exec", container, "xdotool", "getactivewindow", "getwindowname"])
                    .output()
                    .await
                    .ok()?;
                if output.status.success() {
                    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
                } else {
                    None
                }
            }
        }
    }

    // ── Main loop ─────────────────────────────────────────────

    /// Run the full computer use loop: screenshot → vision → action → repeat.
    ///
    /// Reports progress to the channel via `sender`. Returns the final text
    /// response from the model.
    pub async fn run_loop(
        &mut self,
        task: &str,
        sender: &dyn ChannelSender,
    ) -> Result<String, ComputerUseError> {
        let audit = BrowserAuditLog::new(&self.home_dir, 7);
        let mut final_text = String::new();

        sender
            .send_text(&format!("🖥️ 開始電腦操作：{task}"))
            .await
            .ok();

        loop {
            // ── Check threat level ──
            if self.check_threat_level(sender).await {
                break;
            }

            // ── Check control flags ──
            if self.control.stopped.load(Ordering::Acquire) {
                sender.send_text("🛑 電腦操作已停止").await.ok();
                break;
            }

            // Pause loop — only enters if paused flag is set
            let was_paused = self.control.paused.load(Ordering::Acquire);
            while self.control.paused.load(Ordering::Acquire) {
                if self.control.stopped.load(Ordering::Acquire) {
                    sender.send_text("🛑 電腦操作已停止").await.ok();
                    return Ok(final_text);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            // Only reset failure counter when actually resuming from a paused state
            if was_paused {
                self.control.consecutive_failures.store(0, Ordering::Release);
            }

            // ── Check session timeout ──
            if let Some(started) = self.started_at {
                let elapsed = started.elapsed();
                let limit = Duration::from_secs(u64::from(self.config.max_session_minutes) * 60);
                if elapsed >= limit {
                    let msg = format!(
                        "⏱ 電腦操作 session 已達 {} 分鐘上限，已自動結束",
                        self.config.max_session_minutes
                    );
                    sender.send_text(&msg).await.ok();
                    break;
                }
            }

            // ── Capture screenshot ──
            let screenshot_b64 = match self.capture_masked_screenshot().await {
                Ok(s) => s,
                Err(e) => {
                    self.bump_failure();
                    if self.should_auto_pause() {
                        sender
                            .send_text("⚠️ 連續截圖失敗，已自動暫停。發送 /resume 繼續")
                            .await
                            .ok();
                        self.control.paused.store(true, Ordering::Release);
                        continue;
                    }
                    warn!(error = %e, "Screenshot capture failed, retrying");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            // Send before-screenshot to channel if configured
            if self.config.screenshot_interval == ScreenshotInterval::BeforeAndAfter {
                self.send_screenshot_to_channel(&screenshot_b64, "📸 操作前", sender)
                    .await;
            }

            // ── Call Claude Vision API ──
            let session = self.session.as_mut().ok_or_else(|| {
                ComputerUseError::ApiError("No active session".to_string())
            })?;

            let result = session
                .execute_step(&screenshot_b64, task, &self.conversation)
                .await;

            match result {
                Ok(cu_result) => {
                    // Reset failure counter on success
                    self.control.consecutive_failures.store(0, Ordering::Release);

                    // Collect text response
                    if let Some(ref text) = cu_result.text_response {
                        final_text = text.clone();
                        sender.send_text(text).await.ok();
                    }

                    // No actions = model is done
                    if cu_result.actions.is_empty() {
                        info!(agent = %self.agent_id, "Model finished — no more actions");
                        break;
                    }

                    // Execute each action with risk assessment
                    for action in &cu_result.actions {
                        // ── Risk assessment before execution ──
                        let window_title = self.get_active_window_title().await;
                        let action_ctx = ActionContext {
                            action: action.clone(),
                            model_reasoning: cu_result.text_response.clone(),
                            targets_sensitive_input: false, // detected from DOM in capture
                            active_window_title: window_title,
                        };
                        let risk = risk_detector::assess_risk(&action_ctx, &self.config);

                        // Also check CONTRACT.toml must_not rules
                        let contract_blocked = self.check_contract_must_not(action, &cu_result.text_response);

                        if contract_blocked || risk == RiskLevel::Blocked {
                            let reason = if contract_blocked { "CONTRACT.toml 約束" } else { "安全規則" };
                            sender
                                .send_text(&format!("🚫 操作被 {reason} 阻擋：{action:?}"))
                                .await
                                .ok();
                            continue;
                        }

                        if risk == RiskLevel::High && !self.config.auto_confirm_trusted {
                            // Ask for channel confirmation
                            let confirmed = sender
                                .request_confirmation(
                                    &format!("⚠️ 高風險操作：{action:?}\n要繼續嗎？"),
                                    None,
                                    60,
                                )
                                .await
                                .unwrap_or(false);
                            if !confirmed {
                                sender.send_text("已跳過此操作").await.ok();
                                continue;
                            }
                        }

                        // ── Audit log ──
                        let tier = if self.config.execution_mode == ComputerUseMode::Native {
                            "L5b"
                        } else {
                            "L5a"
                        };
                        let audit_entry = AuditEntry {
                            timestamp: chrono::Utc::now(),
                            agent_id: self.agent_id.clone(),
                            tier: tier.to_string(),
                            action: format!("{action:?}"),
                            url: None,
                            domain: None,
                            screenshot_path: None,
                            details: serde_json::to_value(action).unwrap_or_default(),
                        };
                        audit.log_action(&audit_entry).ok();

                        // ── Execute ──
                        match action {
                            ComputerAction::Screenshot => {
                                let ss = self.capture_masked_screenshot().await?;
                                self.send_screenshot_to_channel(&ss, "📸", sender).await;
                            }
                            _ => {
                                if let Err(e) = self.execute_action(action).await {
                                    self.bump_failure();
                                    warn!(error = %e, action = ?action, "Action failed");

                                    if self.should_auto_pause() {
                                        sender
                                            .send_text(
                                                "⚠️ 連續操作失敗，已自動暫停。發送 /resume 繼續",
                                            )
                                            .await
                                            .ok();
                                        self.control.paused.store(true, Ordering::Release);
                                    }
                                    continue;
                                }

                                // Brief delay after action for UI to settle
                                tokio::time::sleep(Duration::from_millis(300)).await;

                                if risk == RiskLevel::Medium {
                                    // Medium risk: notify channel with screenshot
                                    if let Ok(ss) = self.capture_masked_screenshot().await {
                                        self.send_screenshot_to_channel(&ss, "⚠️ 中風險操作已執行", sender).await;
                                    }
                                }
                            }
                        }
                    }

                    // Send after-screenshot to channel
                    if self.config.screenshot_interval != ScreenshotInterval::Manual {
                        if let Ok(ss) = self.capture_masked_screenshot().await {
                            self.send_screenshot_to_channel(&ss, "📸 操作後", sender).await;

                            // Save to audit
                            if let Ok(png_bytes) =
                                base64::engine::general_purpose::STANDARD.decode(&ss)
                            {
                                audit.save_screenshot(&self.agent_id, &png_bytes).ok();
                            }
                        }
                    }
                }
                Err(ComputerUseError::MaxActionsExceeded) => {
                    sender
                        .send_text(&format!(
                            "⚠️ 已達到最大操作次數 ({})，session 結束",
                            self.config.max_actions
                        ))
                        .await
                        .ok();
                    break;
                }
                Err(e) => {
                    self.bump_failure();
                    error!(error = %e, "Claude Vision API error");

                    if self.should_auto_pause() {
                        sender
                            .send_text("⚠️ API 連續錯誤，已自動暫停。發送 /resume 繼續")
                            .await
                            .ok();
                        self.control.paused.store(true, Ordering::Release);
                    }
                }
            }
        }

        // Cleanup
        self.stop_session().await;

        if final_text.is_empty() {
            final_text = "✅ 電腦操作已完成".to_string();
        }

        Ok(final_text)
    }

    // ── Helpers ───────────────────────────────────────────────

    async fn send_screenshot_to_channel(
        &self,
        b64: &str,
        caption: &str,
        sender: &dyn ChannelSender,
    ) {
        if let Ok(png_bytes) = base64::engine::general_purpose::STANDARD.decode(b64) {
            sender.send_photo(&png_bytes, caption).await.ok();
        }
    }

    fn bump_failure(&self) {
        self.control
            .consecutive_failures
            .fetch_add(1, Ordering::Release);
    }

    fn should_auto_pause(&self) -> bool {
        self.control.consecutive_failures.load(Ordering::Acquire)
            >= self.config.max_consecutive_failures
    }

    /// Check if an action violates CONTRACT.toml `must_not` rules.
    ///
    /// Rules are free-text strings (e.g., "不得開啟終端機", "不得修改系統設定").
    /// We check both the action debug representation and the model reasoning.
    fn check_contract_must_not(
        &self,
        action: &ComputerAction,
        model_reasoning: &Option<String>,
    ) -> bool {
        if self.config.contract_must_not.is_empty() {
            return false;
        }
        // Build a semantic description of the action (not Rust Debug format)
        // so CONTRACT.toml rules written in natural language can match.
        let action_str = action_to_semantic_string(action);
        let reasoning = model_reasoning
            .as_deref()
            .unwrap_or("")
            .to_lowercase();

        for rule in &self.config.contract_must_not {
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
            let matched = keywords.iter().all(|kw| {
                action_str.contains(kw) || reasoning.contains(kw)
            });
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

    /// Check the threat level and pause/stop if needed.
    ///
    /// Called at the top of each loop iteration. Reads the threat level
    /// from `~/.duduclaw/threat_level` (same file used by bash-gate.sh).
    pub async fn check_threat_level(&self, sender: &dyn ChannelSender) -> bool {
        let threat_path = self.home_dir.join("threat_level");
        let level = tokio::fs::read_to_string(&threat_path)
            .await
            .unwrap_or_else(|_| "GREEN".to_string());
        let level = level.trim().to_uppercase();

        match level.as_str() {
            "RED" => {
                sender.send_text("🔴 威脅等級 RED — 電腦操作已緊急終止").await.ok();
                self.control.stopped.store(true, Ordering::Release);
                true
            }
            "YELLOW" => {
                if !self.control.paused.load(Ordering::Acquire) {
                    sender.send_text("🟡 威脅等級 YELLOW — 電腦操作已暫停").await.ok();
                    self.control.paused.store(true, Ordering::Release);
                }
                false
            }
            _ => false, // GREEN = normal
        }
    }
}

// ---------------------------------------------------------------------------
// Action → docker exec args translation
// ---------------------------------------------------------------------------

/// Convert a `ComputerAction` to a human-readable semantic string for
/// CONTRACT.toml rule matching. Uses natural language terms so rules like
/// "不得開啟終端機" can match against action descriptions.
fn action_to_semantic_string(action: &ComputerAction) -> String {
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
        ComputerAction::Scroll { coordinate: [x, y], direction, amount } => {
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

/// Validate container image name against injection.
fn validate_image_name(name: &str) -> Result<(), ComputerUseError> {
    if name.is_empty() || name.len() > 256 {
        return Err(ComputerUseError::ApiError("Invalid image name length".into()));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || "._/-:".contains(c)) {
        return Err(ComputerUseError::ApiError(
            format!("Invalid container image name (illegal chars): {name}"),
        ));
    }
    if name.contains("--") || name.contains(' ') || name.contains("..") || name.starts_with('/') {
        return Err(ComputerUseError::ApiError(
            format!("Container image name contains forbidden pattern: {name}"),
        ));
    }
    Ok(())
}

/// Validate xdotool key string — only alphanumeric, `+`, `_`, `-` allowed.
fn validate_xdotool_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.chars().all(|c| c.is_ascii_alphanumeric() || "+-_".contains(c))
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
                "--sync".to_string(),
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
                "--sync".to_string(),
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
                "--sync".to_string(),
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
                "--sync".to_string(),
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
                "--sync".to_string(),
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
                "--sync".to_string(),
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

    #[test]
    fn action_to_docker_args_left_click() {
        let args = action_to_docker_args("test-container", &ComputerAction::LeftClick {
            coordinate: [100, 200],
        });
        assert_eq!(args, vec![
            "exec", "test-container",
            "xdotool", "mousemove", "--sync", "100", "200", "click", "1",
        ]);
    }

    #[test]
    fn action_to_docker_args_type_text() {
        let args = action_to_docker_args("c1", &ComputerAction::Type {
            text: "hello".to_string(),
        });
        assert_eq!(args, vec![
            "exec", "c1",
            "xdotool", "type", "--clearmodifiers", "--", "hello",
        ]);
    }

    #[test]
    fn action_to_docker_args_key() {
        let args = action_to_docker_args("c1", &ComputerAction::Key {
            text: "ctrl+s".to_string(),
        });
        assert_eq!(args, vec![
            "exec", "c1",
            "xdotool", "key", "--clearmodifiers", "--", "ctrl+s",
        ]);
    }

    #[test]
    fn action_to_docker_args_scroll_down() {
        let args = action_to_docker_args("c1", &ComputerAction::Scroll {
            coordinate: [50, 100],
            direction: "down".to_string(),
            amount: 3,
        });
        assert_eq!(args, vec![
            "exec", "c1",
            "xdotool", "mousemove", "--sync", "50", "100",
            "click", "--repeat", "3", "5",
        ]);
    }

    #[test]
    fn action_to_docker_args_scroll_up() {
        let args = action_to_docker_args("c1", &ComputerAction::Scroll {
            coordinate: [50, 100],
            direction: "up".to_string(),
            amount: 2,
        });
        assert!(args.contains(&"4".to_string())); // button 4 = up
    }

    #[test]
    fn action_to_docker_args_double_click() {
        let args = action_to_docker_args("c1", &ComputerAction::DoubleClick {
            coordinate: [300, 400],
        });
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
        assert_eq!(cfg.max_consecutive_failures, 3);
        // Security defaults
        assert!(!cfg.auto_confirm_trusted); // deny-by-default
        assert_eq!(cfg.screenshot_interval, ScreenshotInterval::AfterOnly);
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
            register_session(&format!("test-sess-{i}"), ctl).await.unwrap();
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
        assert_eq!(ctl.consecutive_failures.load(Ordering::Acquire), 0);
    }
}
