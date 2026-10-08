//! The container operations a tool-driven session needs, behind a trait so
//! the session manager's checks (ownership, limits, risk, lifecycle) are
//! unit-tested without Docker.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use super::live_view::{DockerLiveView, LiveViewAccess};
use crate::computer_use::{ComputerAction, ComputerUseError};
use crate::computer_use_orchestrator::{
    ComputerUseConfig, ComputerUseOrchestrator, MaskedScreenshot, NavigateOutcome,
    OrchestratorControl,
};

/// One session's container. Production: [`OrchestratorBackend`].
#[async_trait]
pub(crate) trait SessionBackend: Send + Sync {
    /// Start the container and wait for its display.
    async fn start(&mut self) -> Result<(), ComputerUseError>;
    /// Register the control handle in the global registry (5-session cap,
    /// chat emergency-stop words).
    async fn register(&mut self, session_id: &str) -> Result<(), ComputerUseError>;
    /// Stop and remove the container and leave the registry. Idempotent.
    async fn stop(&mut self);
    /// A masked screenshot (base64 PNG); fully masked when detection fails,
    /// with the reason in [`MaskedScreenshot::full_mask`].
    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError>;
    /// The focused window's title, or why it could not be read.
    async fn window_title(&self) -> Result<String, String>;
    /// Execute one already-validated action.
    async fn execute(&self, action: &ComputerAction) -> Result<(), ComputerUseError>;
    /// Open an already-validated `https://` URL in the container's browser.
    async fn navigate(&self, _url: &str) -> Result<NavigateOutcome, ComputerUseError> {
        Err(ComputerUseError::ApiError("navigation not supported".into()))
    }
    /// Pause the container (keep-alive). Default: unsupported.
    async fn freeze(&self) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::ApiError("pause not supported".into()))
    }
    /// Resume a paused container. Default: unsupported.
    async fn thaw(&self) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::ApiError("resume not supported".into()))
    }
    /// The visible page's text for the injection scan. Default: unreadable
    /// (the caller then fails closed).
    async fn page_text(&self) -> Result<String, ComputerUseError> {
        Err(ComputerUseError::ApiError("page text not supported".into()))
    }
    /// Container access for dashboard viewers, once the container runs.
    fn live_view(&self) -> Option<Arc<dyn LiveViewAccess>> {
        None
    }
    /// The control flags shared with the registry.
    fn control(&self) -> Arc<OrchestratorControl>;
}

/// Builds a backend for `(agent_id, home, config)`.
pub(crate) type BackendFactory =
    Arc<dyn Fn(&str, &Path, ComputerUseConfig) -> Box<dyn SessionBackend> + Send + Sync>;

/// The production factory: a [`ComputerUseOrchestrator`] per session.
pub(crate) fn orchestrator_factory() -> BackendFactory {
    Arc::new(|agent_id: &str, home: &Path, config: ComputerUseConfig| {
        Box::new(OrchestratorBackend {
            inner: ComputerUseOrchestrator::new(agent_id.to_string(), home.to_path_buf(), config),
        }) as Box<dyn SessionBackend>
    })
}

/// [`SessionBackend`] over the existing orchestrator primitives. No model
/// session is attached: tool-driven sessions need no Anthropic key.
pub(crate) struct OrchestratorBackend {
    inner: ComputerUseOrchestrator,
}

#[async_trait]
impl SessionBackend for OrchestratorBackend {
    async fn start(&mut self) -> Result<(), ComputerUseError> {
        self.inner.start_container().await
    }

    async fn register(&mut self, session_id: &str) -> Result<(), ComputerUseError> {
        self.inner.register(session_id).await
    }

    async fn stop(&mut self) {
        self.inner.stop_session().await;
        self.inner.unregister().await;
    }

    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        self.inner.capture_masked_screenshot_detailed().await
    }

    async fn window_title(&self) -> Result<String, String> {
        self.inner.read_active_window_title().await
    }

    async fn execute(&self, action: &ComputerAction) -> Result<(), ComputerUseError> {
        self.inner.execute_action(action).await
    }

    async fn navigate(&self, url: &str) -> Result<NavigateOutcome, ComputerUseError> {
        self.inner.navigate(url).await
    }

    async fn freeze(&self) -> Result<(), ComputerUseError> {
        self.inner.freeze().await
    }

    async fn thaw(&self) -> Result<(), ComputerUseError> {
        self.inner.thaw().await
    }

    async fn page_text(&self) -> Result<String, ComputerUseError> {
        self.inner.read_page_text().await
    }

    fn live_view(&self) -> Option<Arc<dyn LiveViewAccess>> {
        self.inner.container_name().map(|name| {
            Arc::new(DockerLiveView {
                container: name.to_string(),
            }) as Arc<dyn LiveViewAccess>
        })
    }

    fn control(&self) -> Arc<OrchestratorControl> {
        self.inner.control_handle()
    }
}

/// What a session holds after [`super::ComputerUseSessions`] took its real
/// backend away to stop it on a detached task: every operation fails, `stop`
/// does nothing. Only ever seen by code that still holds an ended session.
pub(crate) struct EndedBackend {
    pub(crate) control: Arc<OrchestratorControl>,
}

#[async_trait]
impl SessionBackend for EndedBackend {
    async fn start(&mut self) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::ApiError("session ended".into()))
    }

    async fn register(&mut self, _session_id: &str) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::ApiError("session ended".into()))
    }

    async fn stop(&mut self) {}

    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        Err(ComputerUseError::ApiError("session ended".into()))
    }

    async fn window_title(&self) -> Result<String, String> {
        Err("session ended".into())
    }

    async fn execute(&self, _action: &ComputerAction) -> Result<(), ComputerUseError> {
        Err(ComputerUseError::ApiError("session ended".into()))
    }

    fn control(&self) -> Arc<OrchestratorControl> {
        Arc::clone(&self.control)
    }
}
