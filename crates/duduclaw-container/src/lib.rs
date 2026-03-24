pub mod apple;
pub mod docker;
pub mod lifecycle;
pub mod sandbox;
pub mod wsl2;

use async_trait::async_trait;
use duduclaw_core::error::Result;
use duduclaw_core::traits::ContainerRuntime;
use duduclaw_core::types::*;
use std::time::Duration;

/// Runtime backend selector.
///
/// Supports Docker (all platforms), Apple Container (macOS 15+),
/// and WSL2 (Windows only).
pub enum RuntimeBackend {
    Docker(docker::DockerRuntime),
    Apple(apple::AppleContainerRuntime),
    Wsl2(wsl2::Wsl2Runtime),
}

impl RuntimeBackend {
    /// Detect and return the best available container runtime.
    ///
    /// Priority: Apple Container (macOS) > WSL2 (Windows) > Docker (all).
    pub fn detect() -> Result<Self> {
        #[cfg(target_os = "macos")]
        if apple::AppleContainerRuntime::is_available() {
            return Ok(RuntimeBackend::Apple(apple::AppleContainerRuntime::new()));
        }

        #[cfg(target_os = "windows")]
        if wsl2::Wsl2Runtime::is_available() {
            return Ok(RuntimeBackend::Wsl2(wsl2::Wsl2Runtime::new()));
        }

        Ok(RuntimeBackend::Docker(docker::DockerRuntime::new()?))
    }
}

#[async_trait]
impl ContainerRuntime for RuntimeBackend {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        match self {
            RuntimeBackend::Docker(rt) => rt.create(config).await,
            RuntimeBackend::Apple(rt) => rt.create(config).await,
            RuntimeBackend::Wsl2(rt) => rt.create(config).await,
        }
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.start(id).await,
            RuntimeBackend::Apple(rt) => rt.start(id).await,
            RuntimeBackend::Wsl2(rt) => rt.start(id).await,
        }
    }

    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.stop(id, timeout).await,
            RuntimeBackend::Apple(rt) => rt.stop(id, timeout).await,
            RuntimeBackend::Wsl2(rt) => rt.stop(id, timeout).await,
        }
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.remove(id).await,
            RuntimeBackend::Apple(rt) => rt.remove(id).await,
            RuntimeBackend::Wsl2(rt) => rt.remove(id).await,
        }
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        match self {
            RuntimeBackend::Docker(rt) => rt.logs(id).await,
            RuntimeBackend::Apple(rt) => rt.logs(id).await,
            RuntimeBackend::Wsl2(rt) => rt.logs(id).await,
        }
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        match self {
            RuntimeBackend::Docker(rt) => rt.health_check().await,
            RuntimeBackend::Apple(rt) => rt.health_check().await,
            RuntimeBackend::Wsl2(rt) => rt.health_check().await,
        }
    }
}
