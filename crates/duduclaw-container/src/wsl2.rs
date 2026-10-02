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
/// forwarding docker commands via `wsl.exe -d <distro> --exec docker ...`
/// ([`distro_exec_args`]): `--exec` runs `docker` directly, so no shell in
/// the distro interprets a path component containing spaces, `$` or `;`.
#[allow(dead_code)]
pub struct Wsl2Runtime {
    distro: String,
    wsl_binary: std::path::PathBuf,
    /// Image the containers are created from — the same resolver as the
    /// Docker backend (`crate::docker::default_image`, overridable).
    image: String,
}

impl Wsl2Runtime {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let distro =
            Self::detect_best_distro().unwrap_or_else(|| "Ubuntu-24.04".to_string());
        Self {
            distro,
            wsl_binary: std::path::PathBuf::from(r"C:\Windows\System32\wsl.exe"),
            image: crate::docker::default_image(),
        }
    }

    /// [`Self::new`] running `image` (validated, never pulled by this code —
    /// `docker create` inside the distro may still pull on its own; see
    /// [`Self::create`]).
    pub fn with_image(image: &str) -> Result<Self> {
        let image = image.trim();
        if !duduclaw_core::sandbox_image::valid_image(image) {
            return Err(DuDuClawError::Container(format!(
                "invalid sandbox image reference {image:?}"
            )));
        }
        Ok(Self {
            image: image.to_string(),
            ..Self::new()
        })
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

    /// Whether the image is present inside the distro (never pulls). A
    /// failing `docker image inspect` reads as "not present".
    pub async fn image_present(&self) -> Result<bool> {
        #[cfg(target_os = "windows")]
        {
            Ok(self.wsl_exec(&["docker", "image", "inspect", "--format", "{{.Id}}", &self.image]).await.is_ok())
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    /// What the remove-on-drop guard needs to run `docker rm -f` later.
    pub(crate) fn remover(&self) -> Remover {
        Remover { wsl_binary: self.wsl_binary.clone(), distro: self.distro.clone() }
    }

    /// Execute a command inside the configured WSL2 distro and return stdout.
    #[cfg(target_os = "windows")]
    async fn wsl_exec(&self, args: &[&str]) -> Result<String> {
        let output = tokio::process::Command::new(&self.wsl_binary)
            .args(distro_exec_args(&self.distro))
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

/// The `wsl.exe` arguments that run the following argv directly (no shell)
/// inside `distro`. Every distro command in this file goes through it.
pub(crate) fn distro_exec_args(distro: &str) -> [&str; 3] {
    ["-d", distro, "--exec"]
}

/// Fire-and-forget `docker rm -f` inside the distro (remove-on-drop guard).
pub(crate) struct Remover {
    wsl_binary: std::path::PathBuf,
    distro: String,
}

impl Remover {
    pub(crate) fn remove(&self, id: &str) {
        let _ = std::process::Command::new(&self.wsl_binary)
            .args(distro_exec_args(&self.distro))
            .args(["docker", "rm", "-f", id])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

#[async_trait]
impl ContainerRuntime for Wsl2Runtime {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        #[cfg(target_os = "windows")]
        {
            let container_name = format!("duduclaw-{}", uuid::Uuid::new_v4());

            // Same contract as the Docker backend: an explicit command (the
            // platform image's ENTRYPOINT is the gateway), never root, and no
            // pull — `--pull never` makes a missing image an error.
            let Some((program, rest)) = config.cmd.split_first() else {
                return Err(DuDuClawError::Container(
                    "script sandbox needs an explicit command (the image's default entrypoint is the gateway)"
                        .into(),
                ));
            };
            let user = duduclaw_core::sandbox_image::IMAGE_DEFAULT_USER;

            // Same ceilings as the Docker backend (`crate::docker::SCRIPT_*`).
            let memory = crate::docker::SCRIPT_MEMORY_BYTES.to_string();
            let pids = crate::docker::SCRIPT_PIDS_LIMIT.to_string();
            let cpus = format!("{:.3}", crate::docker::SCRIPT_NANO_CPUS as f64 / 1e9);
            let tmpfs = format!("/tmp:{}", crate::docker::SCRIPT_TMPFS_OPTIONS);
            let log_size = format!("max-size={}", crate::docker::SCRIPT_LOG_MAX_SIZE);
            let log_files = format!("max-file={}", crate::docker::SCRIPT_LOG_MAX_FILES);
            let mut args = vec![
                "docker", "create", "--name", &container_name, "--pull", "never",
                "--user", user, "--cap-drop", "ALL",
                "--security-opt", "no-new-privileges",
                "--memory", &memory, "--memory-swap", &memory, "--pids-limit", &pids, "--cpus", &cpus,
                "--tmpfs", &tmpfs,
                "--log-driver", "json-file", "--log-opt", &log_size, "--log-opt", &log_files,
                "--entrypoint", program.as_str(),
            ];

            // Bind mounts: the Windows host path is converted to the
            // distro's `/mnt/<drive>/…` view; anything that cannot be
            // converted (UNC, relative) refuses the container.
            let mut mount_strings: Vec<String> = Vec::new();
            for m in &config.additional_mounts {
                let mode = if m.readonly { "ro" } else { "rw" };
                let host = crate::wsl_path::windows_to_wsl_path(&m.host).map_err(DuDuClawError::Container)?;
                if m.container.contains([':', ',']) || !m.container.starts_with('/') {
                    return Err(DuDuClawError::Container(format!("invalid container mount path {:?}", m.container)));
                }
                mount_strings.push(format!("{host}:{}:{mode}", m.container));
            }

            for mount_str in &mount_strings {
                args.push("-v");
                args.push(mount_str);
            }

            if config.readonly_project {
                args.push("--read-only");
            }

            if !config.network_access {
                args.push("--network");
                args.push("none");
            }

            // HC5: inject the requested env vars (`-e K=V`); the PTC path
            // passes none (there is no RPC socket).
            let env_strings: Vec<String> = config
                .env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            for env_str in &env_strings {
                args.push("-e");
                args.push(env_str);
            }

            args.push(self.image.as_str());

            // HC5: append the command's arguments (after the image name) so
            // the script actually runs; the program itself is `--entrypoint`.
            for part in rest {
                args.push(part);
            }

            let output = self.wsl_exec(&args).await.map_err(|e| {
                DuDuClawError::Container(format!(
                    "{e} (if the sandbox image is missing inside the WSL distro, run `docker pull {}` there; it is never pulled automatically)",
                    self.image
                ))
            })?;
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
            // The log is capped on disk by `--log-opt`; the read is bounded too.
            let mut output = self.wsl_exec(&["docker", "logs", &id.0]).await?;
            if output.len() > crate::docker::SCRIPT_LOG_READ_MAX_BYTES {
                output = duduclaw_core::truncate_bytes(&output, crate::docker::SCRIPT_LOG_READ_MAX_BYTES).to_string();
                output.push_str("\n...[output truncated]");
            }
            Ok(output)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = id;
            Err(DuDuClawError::Container(
                "WSL2 runtime only available on Windows".into(),
            ))
        }
    }

    async fn wait(&self, id: &ContainerId) -> Result<ContainerExit> {
        #[cfg(target_os = "windows")]
        {
            // `docker wait` blocks until exit and prints the status code.
            let code_out = self.wsl_exec(&["docker", "wait", &id.0]).await?;
            let exit_code = code_out.trim().parse::<i64>().unwrap_or(-1);
            let logs = self.logs(id).await.unwrap_or_default();
            info!(id = %id.0, exit_code, "WSL2 container exited");
            Ok(ContainerExit { exit_code, logs })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every distro command runs its argv directly (`--exec`), never through
    /// the distro's shell (`--`).
    #[test]
    fn distro_commands_bypass_the_shell() {
        assert_eq!(distro_exec_args("Ubuntu-24.04"), ["-d", "Ubuntu-24.04", "--exec"]);
    }
}
