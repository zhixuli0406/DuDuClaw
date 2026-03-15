use duduclaw_core::error::Result;
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use std::time::Duration;
use tracing::info;

/// Higher-level lifecycle management for agent containers.
///
/// Wraps a [`ContainerRuntime`] and provides convenient multi-step operations
/// such as "create + start" and "stop + remove".
pub struct ContainerLifecycle<R: ContainerRuntime> {
    runtime: R,
}

impl<R: ContainerRuntime> ContainerLifecycle<R> {
    pub fn new(runtime: R) -> Self {
        Self { runtime }
    }

    /// Create and immediately start an agent container.
    pub async fn run_agent_container(&self, config: ContainerConfig) -> Result<ContainerId> {
        let id = self.runtime.create(config).await?;
        self.runtime.start(&id).await?;
        info!(id = %id.0, "Agent container is running");
        Ok(id)
    }

    /// Stop a running container then remove it.
    pub async fn stop_and_cleanup(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        self.runtime.stop(id, timeout).await?;
        self.runtime.remove(id).await?;
        info!(id = %id.0, "Agent container stopped and removed");
        Ok(())
    }

    /// Retrieve logs from a container.
    pub async fn logs(&self, id: &ContainerId) -> Result<String> {
        self.runtime.logs(id).await
    }

    /// Check runtime health.
    pub async fn health(&self) -> Result<RuntimeHealth> {
        self.runtime.health_check().await
    }
}
