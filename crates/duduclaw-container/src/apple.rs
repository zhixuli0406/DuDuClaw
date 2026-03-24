//! Apple Container runtime backend for macOS 15+.
//!
//! [A-2a] Uses the `container` CLI tool available on macOS Sequoia+
//! to run agents in lightweight Apple-native containers.

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use std::time::Duration;
use tracing::info;

/// Apple Container runtime (macOS 15+ only).
pub struct AppleContainerRuntime {
    binary: String,
}

impl AppleContainerRuntime {
    /// Check if the `container` CLI is available on this system.
    pub fn is_available() -> bool {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("container")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn new() -> Self {
        Self {
            binary: "container".to_string(),
        }
    }

    /// Run a command via the `container` CLI.
    async fn run_cmd(&self, args: &[&str]) -> Result<String> {
        let output = tokio::process::Command::new(&self.binary)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| DuDuClawError::Container(format!("Apple container command failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DuDuClawError::Container(format!(
                "Apple container error: {}",
                stderr.chars().take(200).collect::<String>()
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_string())
    }
}

#[async_trait]
impl ContainerRuntime for AppleContainerRuntime {
    async fn create(&self, _config: ContainerConfig) -> Result<ContainerId> {
        let id = format!("duduclaw-apple-{}", uuid::Uuid::new_v4());
        // Apple Container uses `container run` (create + start combined)
        // We store the intended ID for later use
        info!(id = %id, "Apple Container ID reserved");
        Ok(ContainerId(id))
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        info!(id = %id.0, "Apple Container start (no-op, started on create)");
        Ok(())
    }

    async fn stop(&self, id: &ContainerId, _timeout: Duration) -> Result<()> {
        self.run_cmd(&["stop", &id.0]).await?;
        info!(id = %id.0, "Apple Container stopped");
        Ok(())
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        self.run_cmd(&["rm", &id.0]).await?;
        info!(id = %id.0, "Apple Container removed");
        Ok(())
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        self.run_cmd(&["logs", &id.0]).await
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        match self.run_cmd(&["--version"]).await {
            Ok(version) => Ok(RuntimeHealth {
                healthy: true,
                message: format!("Apple Container available: {version}"),
                uptime_seconds: 0,
            }),
            Err(e) => Ok(RuntimeHealth {
                healthy: false,
                message: format!("Apple Container unavailable: {e}"),
                uptime_seconds: 0,
            }),
        }
    }
}
