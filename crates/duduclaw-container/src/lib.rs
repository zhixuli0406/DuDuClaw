pub mod apple;
pub mod docker;
pub mod lifecycle;
#[cfg(target_os = "windows")]
pub mod wsl2;
pub mod wsl_path;

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
    #[cfg(target_os = "windows")]
    Wsl2(wsl2::Wsl2Runtime),
}

/// Upper bound of one script-sandbox run, whatever the caller asked for.
pub const MAX_SCRIPT_RUNTIME: Duration = Duration::from_secs(600);

impl RuntimeBackend {
    /// Detect and return the best available container runtime.
    ///
    /// Priority: WSL2 (Windows) > Docker (all). The Apple Container backend
    /// is never chosen: it cannot create containers yet (its `create` fails
    /// closed, deep-review C6), so preferring it on a Mac that also has a
    /// working Docker would make the sandbox unavailable for nothing.
    ///
    /// Runs [`docker::default_image`] — the platform's published image for
    /// this version. Callers that honour `config.toml [container.sandbox]
    /// image` use [`Self::detect_with_image`].
    pub fn detect() -> Result<Self> {
        Self::detect_with_image(&docker::default_image())
    }

    /// [`Self::detect`] with an explicit image (already resolved by the
    /// caller from `[container.sandbox] image`, default
    /// `duduclaw_core::sandbox_image::platform_image`).
    pub fn detect_with_image(image: &str) -> Result<Self> {
        #[cfg(target_os = "windows")]
        if wsl2::Wsl2Runtime::is_available() {
            return Ok(RuntimeBackend::Wsl2(wsl2::Wsl2Runtime::with_image(image)?));
        }

        Ok(RuntimeBackend::Docker(docker::DockerRuntime::with_image(image)?))
    }

    /// Whether the sandbox image is present locally (never pulls). The Apple
    /// backend has no image and never creates containers: `false`.
    pub async fn image_present(&self) -> Result<bool> {
        match self {
            RuntimeBackend::Docker(rt) => rt.image_present().await,
            RuntimeBackend::Apple(_) => Ok(false),
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.image_present().await,
        }
    }

    /// A guard that force-removes container `id` when dropped, unless
    /// [`RemoveOnDrop::disarm`] ran first. Covers a caller whose future is
    /// cancelled between `create` and the final `remove`.
    pub fn remove_on_drop(&self, id: &ContainerId) -> RemoveOnDrop {
        let target = match self {
            RuntimeBackend::Docker(rt) => RemoveTarget::Docker(rt.client()),
            RuntimeBackend::Apple(_) => RemoveTarget::Nothing,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => RemoveTarget::Wsl2(rt.remover()),
        };
        RemoveOnDrop { id: id.0.clone(), target: Some(target) }
    }

    /// Create, start, wait (at most `timeout`, never more than
    /// [`MAX_SCRIPT_RUNTIME`]) and remove one container. The container is
    /// force-removed on every path, including when this future is dropped.
    pub async fn run_once(
        &self,
        config: ContainerConfig,
        timeout: Duration,
    ) -> std::result::Result<ContainerExit, RunFailure> {
        let id = self.create(config).await.map_err(RunFailure::Create)?;
        let mut guard = self.remove_on_drop(&id);
        if let Err(e) = self.start(&id).await {
            return Err(RunFailure::Start(e));
        }
        let waited = tokio::time::timeout(timeout.min(MAX_SCRIPT_RUNTIME), self.wait(&id)).await;
        let _ = self.stop(&id, Duration::from_secs(5)).await;
        if self.remove(&id).await.is_ok() {
            guard.disarm();
        }
        match waited {
            Ok(Ok(exit)) => Ok(exit),
            Ok(Err(e)) => Err(RunFailure::Wait(e)),
            Err(_) => Err(RunFailure::TimedOut),
        }
    }
}

/// Why [`RuntimeBackend::run_once`] produced no exit.
#[derive(Debug)]
pub enum RunFailure {
    /// The container could not be created (no isolation was applied).
    Create(duduclaw_core::error::DuDuClawError),
    /// It was created but would not start.
    Start(duduclaw_core::error::DuDuClawError),
    /// It ran but its exit could not be read.
    Wait(duduclaw_core::error::DuDuClawError),
    /// It exceeded its deadline and was removed.
    TimedOut,
}

enum RemoveTarget {
    Docker(bollard::Docker),
    #[cfg(target_os = "windows")]
    Wsl2(wsl2::Remover),
    Nothing,
}

/// See [`RuntimeBackend::remove_on_drop`].
pub struct RemoveOnDrop {
    id: String,
    target: Option<RemoveTarget>,
}

impl RemoveOnDrop {
    /// The container was removed normally; dropping does nothing.
    pub fn disarm(&mut self) {
        self.target = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let Some(target) = self.target.take() else { return };
        let id = std::mem::take(&mut self.id);
        match target {
            RemoveTarget::Docker(client) => {
                let cli_id = id.clone();
                let remove = async move {
                    let options = bollard::container::RemoveContainerOptions { force: true, ..Default::default() };
                    if let Err(e) = client.remove_container(&id, Some(options)).await {
                        tracing::warn!(container = %id, error = %e, "script sandbox container could not be removed");
                    }
                };
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(remove);
                    }
                    Err(_) => {
                        // No runtime to drive the client: fall back to the CLI.
                        let _ = std::process::Command::new("docker")
                            .args(["rm", "--force", &cli_id])
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                }
            }
            #[cfg(target_os = "windows")]
            RemoveTarget::Wsl2(remover) => remover.remove(&id),
            RemoveTarget::Nothing => {}
        }
    }
}

#[async_trait]
impl ContainerRuntime for RuntimeBackend {
    async fn create(&self, config: ContainerConfig) -> Result<ContainerId> {
        match self {
            RuntimeBackend::Docker(rt) => rt.create(config).await,
            RuntimeBackend::Apple(rt) => rt.create(config).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.create(config).await,
        }
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.start(id).await,
            RuntimeBackend::Apple(rt) => rt.start(id).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.start(id).await,
        }
    }

    async fn stop(&self, id: &ContainerId, timeout: Duration) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.stop(id, timeout).await,
            RuntimeBackend::Apple(rt) => rt.stop(id, timeout).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.stop(id, timeout).await,
        }
    }

    async fn remove(&self, id: &ContainerId) -> Result<()> {
        match self {
            RuntimeBackend::Docker(rt) => rt.remove(id).await,
            RuntimeBackend::Apple(rt) => rt.remove(id).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.remove(id).await,
        }
    }

    async fn logs(&self, id: &ContainerId) -> Result<String> {
        match self {
            RuntimeBackend::Docker(rt) => rt.logs(id).await,
            RuntimeBackend::Apple(rt) => rt.logs(id).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.logs(id).await,
        }
    }

    async fn wait(&self, id: &ContainerId) -> Result<ContainerExit> {
        match self {
            RuntimeBackend::Docker(rt) => rt.wait(id).await,
            RuntimeBackend::Apple(rt) => rt.wait(id).await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.wait(id).await,
        }
    }

    async fn health_check(&self) -> Result<RuntimeHealth> {
        match self {
            RuntimeBackend::Docker(rt) => rt.health_check().await,
            RuntimeBackend::Apple(rt) => rt.health_check().await,
            #[cfg(target_os = "windows")]
            RuntimeBackend::Wsl2(rt) => rt.health_check().await,
        }
    }
}
