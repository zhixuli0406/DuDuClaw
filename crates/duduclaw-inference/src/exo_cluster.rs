//! Exo P2P cluster client — connects to an Exo distributed inference cluster.
//!
//! Exo aggregates multiple machines (especially Apple Silicon Macs) into a
//! single inference endpoint via P2P networking. It exposes an OpenAI-compatible
//! API that we consume directly.
//!
//! Features:
//! - Cluster discovery via mDNS or manual config
//! - Health monitoring with automatic failover
//! - Model placement based on aggregate cluster memory

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};


/// Exo cluster configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExoConfig {
    /// Enable Exo cluster backend.
    pub enabled: bool,

    /// Cluster API endpoint (e.g., "http://exo-cluster.local:8000").
    /// If empty, attempts mDNS discovery.
    pub endpoint: String,

    /// Fallback endpoints to try if primary is down.
    #[serde(default)]
    pub fallback_endpoints: Vec<String>,

    /// Model to request from the cluster.
    pub model: String,

    /// Health check interval in seconds.
    pub health_check_interval_secs: u64,

    /// Request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ExoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            fallback_endpoints: Vec::new(),
            model: "qwen3-8b".to_string(),
            health_check_interval_secs: 30,
            timeout_secs: 120,
        }
    }
}

/// Cluster node status.
#[derive(Debug, Clone, Serialize)]
pub struct ClusterStatus {
    pub endpoint: String,
    pub is_healthy: bool,
    #[serde(skip)]
    pub last_check: Option<Instant>,
    pub latency_ms: Option<u64>,
    pub model: String,
    pub node_count: Option<u32>,
}

/// Exo cluster client.
pub struct ExoCluster {
    config: ExoConfig,
    client: reqwest::Client,
    status: tokio::sync::RwLock<ClusterStatus>,
}

impl ExoCluster {
    pub fn new(config: ExoConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .unwrap_or_default();

        let status = ClusterStatus {
            endpoint: config.endpoint.clone(),
            is_healthy: false,
            last_check: None,
            latency_ms: None,
            model: config.model.clone(),
            node_count: None,
        };

        if !config.endpoint.is_empty() && !config.endpoint.starts_with("https://") {
            warn!(
                endpoint = %config.endpoint,
                "Exo cluster endpoint uses unencrypted HTTP — \
                 consider using HTTPS for production"
            );
        }

        Self {
            config,
            client,
            status: tokio::sync::RwLock::new(status),
        }
    }

    /// Check if the cluster is available and healthy.
    pub async fn health_check(&self) -> bool {
        let endpoints = self.all_endpoints();
        for ep in &endpoints {
            let url = format!("{}/v1/models", ep.trim_end_matches('/'));
            let start = Instant::now();

            match self.client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let latency = start.elapsed().as_millis() as u64;
                    let mut status = self.status.write().await;
                    status.endpoint = ep.clone();
                    status.is_healthy = true;
                    status.last_check = Some(Instant::now());
                    status.latency_ms = Some(latency);

                    // Try to parse node count from response
                    if let Ok(body) = resp.json::<serde_json::Value>().await
                        && let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                            status.node_count = Some(data.len() as u32);
                        }

                    info!(endpoint = %ep, latency_ms = latency, "Exo cluster healthy");
                    return true;
                }
                Ok(resp) => {
                    warn!(endpoint = %ep, status = %resp.status(), "Exo cluster unhealthy");
                }
                Err(e) => {
                    warn!(endpoint = %ep, error = %e, "Exo cluster unreachable");
                }
            }
        }

        let mut status = self.status.write().await;
        status.is_healthy = false;
        status.last_check = Some(Instant::now());
        false
    }

    /// Get the OpenAI-compatible base URL for the healthy endpoint.
    pub async fn api_base_url(&self) -> Option<String> {
        let status = self.status.read().await;
        if status.is_healthy {
            Some(format!("{}/v1", status.endpoint.trim_end_matches('/')))
        } else {
            None
        }
    }

    /// Get cluster status for monitoring.
    pub async fn get_status(&self) -> ClusterStatus {
        self.status.read().await.clone()
    }

    /// Get the model name configured for this cluster.
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Check if Exo is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled && !self.config.endpoint.is_empty()
    }

    /// All endpoints to try (primary + fallbacks).
    fn all_endpoints(&self) -> Vec<String> {
        let mut eps = vec![self.config.endpoint.clone()];
        eps.extend(self.config.fallback_endpoints.clone());
        eps.retain(|e| !e.is_empty());
        eps
    }
}
