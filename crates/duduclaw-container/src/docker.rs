use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    StopContainerOptions,
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

#[async_trait]
impl ContainerRuntime for DockerRuntime {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        let container_name = format!("duduclaw-{}", uuid::Uuid::new_v4());

        // Build bind mounts from additional_mounts and readonly_project
        let mut binds: Vec<String> = Vec::new();
        for mount in &config.additional_mounts {
            let mode = if mount.readonly { "ro" } else { "rw" };
            binds.push(format!("{}:{}:{}", mount.host, mount.container, mode));
        }

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

        let host_config = bollard::models::HostConfig {
            binds: if binds.is_empty() {
                None
            } else {
                Some(binds)
            },
            readonly_rootfs: if config.readonly_project {
                Some(true)
            } else {
                None
            },
            ..Default::default()
        };

        let container_config = Config {
            image: Some(DEFAULT_IMAGE.to_string()),
            labels: Some(labels),
            stop_timeout: Some(stop_timeout_secs),
            host_config: Some(host_config),
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
