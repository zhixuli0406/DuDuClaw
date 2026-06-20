use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions, WaitContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::Docker;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

const DEFAULT_IMAGE: &str = "duduclaw-agent:latest";

pub struct DockerRuntime {
    client: Docker,
}

impl DockerRuntime {
    pub fn new() -> Result<Self> {
        let client = Docker::connect_with_local_defaults().map_err(|e| {
            DuDuClawError::Container(format!("Failed to connect to Docker daemon: {e}"))
        })?;
        Ok(Self { client })
    }
}

/// Build the bind-mount strings (`host:container:mode`) for a container config.
fn build_binds(config: &ContainerConfig) -> Vec<String> {
    config
        .additional_mounts
        .iter()
        .map(|mount| {
            let mode = if mount.readonly { "ro" } else { "rw" };
            format!("{}:{}:{}", mount.host, mount.container, mode)
        })
        .collect()
}

/// Build the `HostConfig` that applies sandbox isolation for a container.
///
/// C6 / C6.6: this is the single source of truth for isolation so it can be
/// unit-tested without a Docker daemon. Isolation rules:
///  - network: `none` unless the agent explicitly opted into egress;
///  - tmpfs: a small writable `/tmp` so a readonly-rootfs container still has
///    scratch space;
///  - memory: a 2 GiB cap so a runaway task can't exhaust the host.
fn build_host_config(config: &ContainerConfig) -> bollard::models::HostConfig {
    let binds = build_binds(config);

    let network_mode = if config.network_access {
        None // Docker default bridge — egress allowed (explicit opt-in)
    } else {
        Some("none".to_string())
    };

    let mut tmpfs = HashMap::new();
    tmpfs.insert("/tmp".to_string(), "rw,noexec,nosuid,size=64m".to_string());

    bollard::models::HostConfig {
        binds: if binds.is_empty() { None } else { Some(binds) },
        readonly_rootfs: if config.readonly_project {
            Some(true)
        } else {
            None
        },
        network_mode,
        tmpfs: Some(tmpfs),
        // 2 GiB cap — bounds runaway tasks without breaking typical builds.
        memory: Some(2 * 1024 * 1024 * 1024),
        ..Default::default()
    }
}

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        let container_name = format!("duduclaw-{}", uuid::Uuid::new_v4());

        // Pull image if not present (best-effort)
        let pull_opts = CreateImageOptions {
            from_image: DEFAULT_IMAGE,
            ..Default::default()
        };
        let mut pull_stream = self.client.create_image(Some(pull_opts), None, None);
        while let Some(result) = pull_stream.next().await {
            match result {
                Ok(_) => {}
                Err(e) => {
                    warn!("Image pull failed (may already exist locally): {}", e);
                    break;
                }
            }
        }

        let mut labels = HashMap::new();
        labels.insert("managed-by".to_string(), "duduclaw".to_string());

        let stop_timeout_secs = (config.timeout_ms / 1000) as i64;

        // C6 fix: actually apply sandbox isolation (network=none / tmpfs / memory)
        // via the shared, unit-tested `build_host_config` helper. Previously only
        // binds + readonly_rootfs were set, so the container ran on Docker's
        // default bridge with full egress despite `network_access=false`.
        let host_config = build_host_config(&config);

        if config.network_access {
            warn!(
                name = %container_name,
                "sandbox container created WITH network egress (network_access=true)"
            );
        } else {
            info!(name = %container_name, "sandbox network isolation: none");
        }

        // HC5: run the requested command and inject env vars. `cmd` empty means
        // "use the image default"; `env` is appended as `K=V` strings. This is
        // what makes the PTC container path actually execute the user script and
        // see `DUDUCLAW_PTC_SOCKET`.
        let cmd = if config.cmd.is_empty() {
            None
        } else {
            Some(config.cmd.clone())
        };
        let env = if config.env.is_empty() {
            None
        } else {
            Some(
                config
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<String>>(),
            )
        };

        let container_config = Config {
            image: Some(DEFAULT_IMAGE.to_string()),
            labels: Some(labels),
            stop_timeout: Some(stop_timeout_secs),
            host_config: Some(host_config),
            cmd,
            env,
            ..Default::default()
        };

        let options = CreateContainerOptions {
            name: container_name.as_str(),
            platform: None,
        };

        let response = self
            .client
            .create_container(Some(options), container_config)
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to create container: {}", e)))?;

        info!(id = %response.id, name = %container_name, "Container created");
        Ok(ContainerId(response.id))
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        self.client
            .start_container(&id.0, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to start container: {}", e)))?;

        info!(id = %id.0, "Container started");
        Ok(())
    }

    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        let options = StopContainerOptions {
            t: timeout.as_secs() as i64,
        };

        self.client
            .stop_container(&id.0, Some(options))
            .await
            .map_err(|e| DuDuClawError::Container(format!("Failed to stop container: {}", e)))?;

        info!(id = %id.0, "Container stopped");
        Ok(())
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        let options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };

        self.client
            .remove_container(&id.0, Some(options))
            .await
            .map_err(|e| {
                DuDuClawError::Container(format!("Failed to remove container: {}", e))
            })?;

        info!(id = %id.0, "Container removed");
        Ok(())
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        let options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: false,
            ..Default::default()
        };

        let mut stream = self.client.logs(&id.0, Some(options));
        let mut output = String::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(chunk) => {
                    output.push_str(&chunk.to_string());
                }
                Err(e) => {
                    warn!(id = %id.0, error = %e, "Error reading container logs");
                    break;
                }
            }
        }

        Ok(output)
    }

    async fn wait(&self, id: &ContainerId) -> Result<ContainerExit> {
        // Wait for the container to exit, capturing the status code. The
        // wait_container stream yields one response on exit; bollard turns a
        // non-zero exit into a `DockerContainerWaitError` carrying the code.
        let mut stream = self
            .client
            .wait_container(&id.0, None::<WaitContainerOptions<String>>);

        let mut exit_code: i64 = 0;
        while let Some(result) = stream.next().await {
            match result {
                Ok(resp) => exit_code = resp.status_code,
                Err(bollard::errors::Error::DockerContainerWaitError { code, .. }) => {
                    exit_code = code;
                }
                Err(e) => {
                    return Err(DuDuClawError::Container(format!(
                        "Failed to wait for container: {e}"
                    )));
                }
            }
        }

        // Collect logs after exit so we get the full output.
        let logs = self.logs(id).await.unwrap_or_default();

        info!(id = %id.0, exit_code, "Container exited");
        Ok(ContainerExit { exit_code, logs })
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        match self.client.ping().await {
            Ok(_) => {
                // Get system info for uptime
                let info = self.client.info().await.map_err(|e| {
                    DuDuClawError::Container(format!("Failed to get Docker info: {}", e))
                })?;

                let containers_running = info.containers_running.unwrap_or(0) as u64;

                Ok(RuntimeHealth {
                    healthy: true,
                    message: format!(
                        "Docker daemon is healthy, {} containers running",
                        containers_running
                    ),
                    uptime_seconds: 0, // Docker API does not expose daemon uptime directly
                })
            }
            Err(e) => Ok(RuntimeHealth {
                healthy: false,
                message: format!("Docker daemon unreachable: {}", e),
                uptime_seconds: 0,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::types::MountConfig;

    /// Minimal `ContainerConfig` for isolation tests.
    fn base_config(network_access: bool) -> ContainerConfig {
        ContainerConfig {
            timeout_ms: 30_000,
            max_concurrent: 1,
            readonly_project: true,
            additional_mounts: vec![MountConfig {
                host: "/host/work".to_string(),
                container: "/workspace".to_string(),
                readonly: true,
            }],
            sandbox_enabled: true,
            network_access,
            worktree_enabled: false,
            worktree_auto_merge: true,
            worktree_cleanup_on_exit: true,
            worktree_copy_files: vec![],
            cmd: vec![],
            env: vec![],
        }
    }

    #[test]
    fn build_host_config_isolates_when_network_disabled() {
        // C6.6: network_access=false ⇒ --network=none + tmpfs + memory cap.
        let hc = build_host_config(&base_config(false));

        assert_eq!(
            hc.network_mode.as_deref(),
            Some("none"),
            "network must be `none` when network_access=false"
        );
        let tmpfs = hc.tmpfs.expect("tmpfs must be set");
        assert!(tmpfs.contains_key("/tmp"), "/tmp tmpfs mount must be present");
        assert_eq!(
            hc.memory,
            Some(2 * 1024 * 1024 * 1024),
            "memory cap must be applied"
        );
        assert_eq!(hc.readonly_rootfs, Some(true));
        // The single additional mount must be rendered as a bind string.
        let binds = hc.binds.expect("binds must be present");
        assert_eq!(binds, vec!["/host/work:/workspace:ro".to_string()]);
    }

    #[test]
    fn build_host_config_allows_egress_when_network_enabled() {
        // network_access=true ⇒ network_mode None (Docker default bridge),
        // but tmpfs + memory isolation still apply.
        let hc = build_host_config(&base_config(true));

        assert!(
            hc.network_mode.is_none(),
            "network_mode must be None (default bridge) when network_access=true"
        );
        assert!(hc.tmpfs.is_some(), "tmpfs still applied with egress");
        assert_eq!(hc.memory, Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn build_binds_renders_read_write_mode() {
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![MountConfig {
            host: "/run/duduclaw".to_string(),
            container: "/run/duduclaw".to_string(),
            readonly: false,
        }];
        let binds = build_binds(&cfg);
        assert_eq!(binds, vec!["/run/duduclaw:/run/duduclaw:rw".to_string()]);
    }

    /// Integration test — requires a running Docker daemon and the
    /// `duduclaw-agent:latest` image. Asserts that a `--network=none` container
    /// genuinely has no egress. Marked `#[ignore]` so CI without Docker skips it.
    ///
    /// Run manually with: `cargo test -p duduclaw-container -- --ignored network_none`
    #[tokio::test]
    #[ignore = "requires a Docker daemon + duduclaw-agent:latest image"]
    async fn network_none_blocks_egress() {
        let runtime = DockerRuntime::new().expect("Docker daemon must be reachable");

        // Try to reach an external host; with --network=none this must fail.
        let mut cfg = base_config(false);
        cfg.additional_mounts = vec![];
        cfg.cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            // exit 0 only if egress succeeds — we assert the opposite.
            "getent hosts example.com >/dev/null 2>&1 && echo EGRESS_OK || echo EGRESS_BLOCKED"
                .to_string(),
        ];

        let id = runtime.create(cfg).await.expect("create container");
        runtime.start(&id).await.expect("start container");
        let exit = runtime.wait(&id).await.expect("wait container");
        let _ = runtime.remove(&id).await;

        assert!(
            exit.logs.contains("EGRESS_BLOCKED"),
            "container with --network=none must not resolve external hosts; logs: {}",
            exit.logs
        );
    }
}
