//! InferenceManager — multi-mode auto-switching state machine.
//!
//! Manages the inference lifecycle across multiple backends with automatic
//! failover:
//!
//! ```text
//! Priority order:
//!   1. llamafile (if running — portable, zero-install)
//!   2. Direct backend (an in-process `InferenceBackend`)
//!   3. OpenAI-compat (external server)
//!   4. Cloud API (Claude — fallback, highest quality)
//! ```
//!
//! The Exo P2P cluster mode was removed on 2026-09-29
//! (`wiki/reports/feature-audit-2026-09-29.md` T3-S4): pointing
//! `[openai_compat] base_url` at an Exo endpoint reaches the same cluster.
//!
//! The manager periodically health-checks all backends and auto-switches
//! to the best available one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::info;

use crate::config::InferenceConfig;
use crate::error::Result;
use crate::llamafile::LlamafileManager;

/// Which mode the manager is currently using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceMode {
    /// llamafile local server
    Llamafile,
    /// Direct backend (an in-process `InferenceBackend`)
    DirectBackend,
    /// External OpenAI-compatible server
    OpenAiCompat,
    /// No local inference available — cloud API only
    CloudOnly,
}

impl std::fmt::Display for InferenceMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llamafile => write!(f, "llamafile"),
            Self::DirectBackend => write!(f, "direct"),
            Self::OpenAiCompat => write!(f, "openai-compat"),
            Self::CloudOnly => write!(f, "cloud-only"),
        }
    }
}

/// Manager status for monitoring.
#[derive(Debug, Clone, Serialize)]
pub struct ManagerStatus {
    pub current_mode: InferenceMode,
    pub llamafile_available: bool,
}

/// Multi-mode inference manager with automatic failover.
pub struct InferenceManager {
    current_mode: RwLock<InferenceMode>,
    llamafile: Option<Arc<LlamafileManager>>,
    last_health_check: RwLock<Option<Instant>>,
    health_check_interval: Duration,
}

impl InferenceManager {
    /// Create a new manager from config.
    pub fn new(config: &InferenceConfig) -> Self {
        let llamafile = config.llamafile.as_ref()
            .filter(|c| c.enabled)
            .map(|c| Arc::new(LlamafileManager::new(c.clone())));

        Self {
            current_mode: RwLock::new(InferenceMode::CloudOnly),
            llamafile,
            last_health_check: RwLock::new(None),
            health_check_interval: Duration::from_secs(30),
        }
    }

    /// Initialize: health-check all backends and select the best mode.
    pub async fn init(&self) -> Result<InferenceMode> {
        let mode = self.select_best_mode().await;
        *self.current_mode.write().await = mode;
        *self.last_health_check.write().await = Some(Instant::now());
        info!(mode = %mode, "InferenceManager initialized");
        Ok(mode)
    }

    /// Get the current inference mode.
    pub async fn current_mode(&self) -> InferenceMode {
        // Re-check if interval elapsed
        let should_recheck = {
            let last = self.last_health_check.read().await;
            last.is_none_or(|t| t.elapsed() > self.health_check_interval)
        };

        if should_recheck {
            let mode = self.select_best_mode().await;
            *self.current_mode.write().await = mode;
            *self.last_health_check.write().await = Some(Instant::now());
        }

        *self.current_mode.read().await
    }

    /// Get the OpenAI-compatible base URL for the current best backend.
    ///
    /// Returns `None` if only Cloud API is available.
    pub async fn get_api_base_url(&self) -> Option<String> {
        let mode = self.current_mode().await;

        match mode {
            InferenceMode::Llamafile => {
                if let Some(ref lf) = self.llamafile {
                    return Some(lf.api_base_url());
                }
                None
            }
            InferenceMode::DirectBackend | InferenceMode::OpenAiCompat => {
                // Handled by the InferenceEngine directly
                None
            }
            InferenceMode::CloudOnly => None,
        }
    }

    /// Get the model name for the current mode.
    ///
    /// Always `None` since the Exo P2P mode was removed (2026-09-29,
    /// `wiki/reports/feature-audit-2026-09-29.md` T3-S4): the remaining modes
    /// all report their model through the backend, not the manager. Kept so
    /// `InferenceEngine::init` keeps its "manager-provided model" shape for
    /// whatever mode comes next.
    pub async fn get_model(&self) -> Option<String> {
        None
    }

    /// Get full manager status.
    pub async fn status(&self) -> ManagerStatus {
        let llamafile_available = match &self.llamafile {
            Some(lf) => lf.is_healthy().await,
            None => false,
        };

        ManagerStatus {
            current_mode: *self.current_mode.read().await,
            llamafile_available,
        }
    }

    /// Start the llamafile server if configured.
    pub async fn start_llamafile(&self) -> Result<()> {
        if let Some(ref lf) = self.llamafile {
            lf.start(None).await?;
        }
        Ok(())
    }

    /// Stop the llamafile server.
    pub async fn stop_llamafile(&self) {
        if let Some(ref lf) = self.llamafile {
            lf.stop().await;
        }
    }

    /// List available llamafiles.
    pub async fn list_llamafiles(&self) -> Vec<String> {
        if let Some(ref lf) = self.llamafile {
            lf.list_files().await
        } else {
            Vec::new()
        }
    }

    /// Select the best available mode based on health checks.
    async fn select_best_mode(&self) -> InferenceMode {
        // 1. Try llamafile (portable, already running)
        if let Some(ref lf) = self.llamafile
            && lf.is_enabled() {
                if lf.is_healthy().await {
                    return InferenceMode::Llamafile;
                }
                // Try to start it
                if lf.start(None).await.is_ok() {
                    return InferenceMode::Llamafile;
                }
            }

        // 2. Direct backend is handled by InferenceEngine
        // We return CloudOnly and let the engine decide
        InferenceMode::CloudOnly
    }
}
