//! llamafile subprocess manager — portable single-binary LLM inference.
//!
//! llamafile bundles model weights + inference engine + runtime into a single
//! executable that runs on macOS, Linux, Windows, *BSD with zero installation.
//!
//! This module manages the lifecycle of a llamafile server process:
//! - Auto-start on first inference request
//! - Health monitoring and auto-restart
//! - Graceful shutdown on engine teardown
//! - OpenAI-compatible API on localhost

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::error::{InferenceError, Result};

/// llamafile configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlamafileConfig {
    /// Enable llamafile backend.
    pub enabled: bool,

    /// Directory containing .llamafile executables.
    pub dir: String,

    /// Default llamafile to use (filename without path).
    pub default_file: Option<String>,

    /// Port for the llamafile HTTP server.
    pub port: u16,

    /// Host to bind (default: 127.0.0.1).
    pub host: String,

    /// Number of GPU layers to offload (-1 = all, 0 = CPU only).
    pub gpu_layers: i32,

    /// Context size.
    pub context_size: u32,

    /// Additional CLI arguments.
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl Default for LlamafileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: "~/.duduclaw/llamafiles".to_string(),
            default_file: None,
            port: 8079,
            host: "127.0.0.1".to_string(),
            gpu_layers: -1,
            context_size: 4096,
            extra_args: Vec::new(),
        }
    }
}

/// llamafile server state.
#[derive(Debug, Clone, PartialEq)]
enum ServerState {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

/// llamafile subprocess manager.
pub struct LlamafileManager {
    config: LlamafileConfig,
    state: RwLock<ServerState>,
    child: RwLock<Option<tokio::process::Child>>,
    client: reqwest::Client,
}

impl LlamafileManager {
    pub fn new(config: LlamafileConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            config,
            state: RwLock::new(ServerState::Stopped),
            child: RwLock::new(None),
            client,
        }
    }

    /// Resolve the llamafiles directory, expanding `~`.
    fn resolve_dir(&self) -> PathBuf {
        crate::util::expand_tilde(&self.config.dir)
    }

    /// List available llamafile executables.
    pub async fn list_files(&self) -> Vec<String> {
        let dir = self.resolve_dir();
        let mut files = Vec::new();

        if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".llamafile") || name.ends_with(".llamafile.exe") {
                    files.push(name);
                }
            }
        }
        files
    }

    /// Start the llamafile server.
    pub async fn start(&self, llamafile_name: Option<&str>) -> Result<()> {
        {
            let state = self.state.read().await;
            if *state == ServerState::Running {
                return Ok(());
            }
        }

        *self.state.write().await = ServerState::Starting;

        let file_name = llamafile_name
            .map(|s| s.to_string())
            .or_else(|| self.config.default_file.clone())
            .ok_or_else(|| InferenceError::Config("No llamafile specified".to_string()))?;

        // Reject path traversal
        if file_name.contains('/') || file_name.contains('\\') || file_name.contains("..") {
            let msg = format!("Invalid llamafile name: {file_name}");
            *self.state.write().await = ServerState::Failed(msg.clone());
            return Err(InferenceError::Config(msg));
        }

        let file_path = self.resolve_dir().join(&file_name);
        if !file_path.exists() {
            let msg = format!("llamafile not found: {}", file_path.display());
            *self.state.write().await = ServerState::Failed(msg.clone());
            return Err(InferenceError::ModelNotFound { path: msg });
        }

        // Ensure executable permission on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&file_path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = std::fs::set_permissions(&file_path, perms);
            }
        }

        info!(
            file = %file_name,
            port = self.config.port,
            gpu_layers = self.config.gpu_layers,
            "Starting llamafile server"
        );

        let mut cmd = tokio::process::Command::new(&file_path);
        cmd.args([
            "--server",
            "--host", &self.config.host,
            "--port", &self.config.port.to_string(),
            "-ngl", &self.config.gpu_layers.to_string(),
            "-c", &self.config.context_size.to_string(),
        ]);

        // Filter extra_args — block flags that override security-critical settings
        const BLOCKED_ARGS: &[&str] = &["--host", "--port", "--api-key", "-H", "-p", "--log-disable"];
        for arg in &self.config.extra_args {
            let lower = arg.to_lowercase();
            let is_blocked = BLOCKED_ARGS.iter().any(|b| {
                lower == *b || lower.starts_with(&format!("{b}="))
            });
            if is_blocked {
                warn!(arg = %arg, "Blocked extra_arg that overrides security-critical setting");
                continue;
            }
            cmd.arg(arg);
        }

        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| {
            let msg = format!("Failed to spawn llamafile: {e}");
            InferenceError::GenerationFailed(msg)
        })?;

        *self.child.write().await = Some(child);

        // Wait for server to become ready (poll health endpoint)
        let ready = self.wait_for_ready(Duration::from_secs(30)).await;
        if ready {
            *self.state.write().await = ServerState::Running;
            info!(port = self.config.port, "llamafile server ready");
            Ok(())
        } else {
            let msg = "llamafile server failed to start within 30s".to_string();
            *self.state.write().await = ServerState::Failed(msg.clone());
            self.stop().await;
            Err(InferenceError::GenerationFailed(msg))
        }
    }

    /// Stop the llamafile server.
    pub async fn stop(&self) {
        let mut child_guard = self.child.write().await;
        if let Some(ref mut child) = *child_guard {
            let _ = child.kill().await;
            let _ = child.wait().await; // Prevent zombie
            info!("llamafile server stopped");
        }
        *child_guard = None;
        *self.state.write().await = ServerState::Stopped;
    }

    /// Get the OpenAI-compatible base URL.
    pub fn api_base_url(&self) -> String {
        format!("http://{}:{}/v1", self.config.host, self.config.port)
    }

    /// Check if the server is running and healthy.
    pub async fn is_healthy(&self) -> bool {
        let state = self.state.read().await;
        if *state != ServerState::Running {
            return false;
        }

        let url = format!("http://{}:{}/health", self.config.host, self.config.port);
        matches!(self.client.get(&url).send().await, Ok(r) if r.status().is_success())
    }

    /// Check if llamafile is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Wait for the server to become ready.
    async fn wait_for_ready(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        let health_url = format!("http://{}:{}/health", self.config.host, self.config.port);

        while start.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Check if process is still alive
            {
                let mut child_guard = self.child.write().await;
                if let Some(ref mut child) = *child_guard {
                    if let Ok(Some(status)) = child.try_wait() {
                        warn!(exit_code = ?status.code(), "llamafile exited prematurely");
                        return false;
                    }
                }
            }

            if let Ok(resp) = self.client.get(&health_url).send().await {
                if resp.status().is_success() {
                    return true;
                }
            }
        }
        false
    }
}

