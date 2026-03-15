use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use std::time::Duration;
#[cfg(target_os = "windows")]
use tracing::info;

/// WSL2 Direct runtime for Windows.
///
/// Executes containers through WSL2 without Docker Desktop by
/// forwarding docker commands via `wsl.exe -d <distro> -- docker ...`.
#[allow(dead_code)]
pub struct Wsl2Runtime {
    distro: String,
    wsl_binary: std::path::PathBuf,
}

impl Wsl2Runtime {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let distro =
            Self::detect_best_distro().unwrap_or_else(|| "Ubuntu-24.04".to_string());
        Self {
            distro,
            wsl_binary: std::path::PathBuf::from(r"C:\Windows\System32\wsl.exe"),
        }
    }

    /// Detect the best WSL2 distro available on this machine.
    ///
    /// Prefers a distro running WSL version 2. Returns `None` on
    /// non-Windows platforms.
    fn detect_best_distro() -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            let output = std::process::Command::new("wsl")
                .args(["-l", "-v"])
                .output()
                .ok()?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Parse the list output, skipping the header line.
            // Format: "* Ubuntu-24.04  Running  2"
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[parts.len() - 1] == "2" {
                    let name = parts[0].trim_start_matches('*').trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    /// Returns `true` when running on Windows and `wsl.exe` exists.
    pub fn is_available() -> bool {
        #[cfg(target_os = "windows")]
        {
            std::path::Path::new(r"C:\Windows\System32\wsl.exe").exists()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    /// Execute a command inside the configured WSL2 distro and return stdout.
    #[cfg(target_os = "windows")]
    async fn wsl_exec(&self, args: &[&str]) -> Result<String> {
        let output = tokio::process::Command::new(&self.wsl_binary)
            .args(["-d", &self.distro, "--"])
            .args(args)
            .output()
            .await
            .map_err(|e| DuDuClawError::Container(format!("WSL exec failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DuDuClawError::Container(format!(
                "WSL command failed: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

#[async_trait]
impl ContainerRuntime for Wsl2Runtime {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        #[cfg(target_os = "windows")]
        {
            let container_name = format!("duduclaw-{}", uuid::Uuid::new_v4());

            let mut args = vec!["docker", "create", "--name", &container_name];

            // Add bind mounts, converting Windows paths to WSL paths
            let mount_strings: Vec<String> = config
                .additional_mounts
                .iter()
                .map(|m| {
                    let mode = if m.readonly { "ro" } else { "rw" };
                    format!("{}:{}:{}", m.host, m.container, mode)
                })
                .collect();

            for mount_str in &mount_strings {
                args.push("-v");
                args.push(mount_str);
            }

            if config.readonly_project {
                args.push("--read-only");
            }

            args.push("duduclaw-agent:latest");

            let output = self.wsl_exec(&args).await?;
            info!(name = %container_name, "WSL2 container created");
            Ok(ContainerId(output.trim().to_string()))
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = config;
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            self.wsl_exec(&["docker", "start", &id.0]).await?;
            info!(id = %id.0, "WSL2 container started");
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            let timeout_secs = timeout.as_secs().to_string();
            self.wsl_exec(&["docker", "stop", "-t", &timeout_secs, &id.0])
                .await?;
            info!(id = %id.0, "WSL2 container stopped");
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (id, timeout);
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            self.wsl_exec(&["docker", "rm", "-f", &id.0]).await?;
            info!(id = %id.0, "WSL2 container removed");
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        #[cfg(target_os = "windows")]
        {
            self.wsl_exec(&["docker", "logs", &id.0]).await
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        #[cfg(target_os = "windows")]
        {
            match self
                .wsl_exec(&["docker", "info", "--format", "{{.ServerVersion}}"])
                .await
            {
                Ok(version) => Ok(RuntimeHealth {
                    healthy: true,
                    message: format!("WSL2 Docker {}", version.trim()),
                    uptime_seconds: 0,
                }),
                Err(e) => Ok(RuntimeHealth {
                    healthy: false,
                    message: format!("WSL2 Docker unavailable: {}", e),
                    uptime_seconds: 0,
                }),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(RuntimeHealth {
                healthy: false,
                message: "WSL2 runtime only available on Windows".into(),
                uptime_seconds: 0,
            })
        }
    }
}
