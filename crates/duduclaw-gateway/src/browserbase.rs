//! Browserbase cloud browser client for DuDuClaw's L5 alternative (Phase 5).
//!
//! Provides a REST API client for Browserbase session management and cost tracking.
//! Complements the MCP server (`@browserbasehq/mcp-server-browserbase`) with direct
//! session lifecycle control.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use zeroize::Zeroize;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn default_base_url() -> String {
    "https://api.browserbase.com".to_string()
}
fn default_browser_width() -> u32 {
    1024
}
fn default_browser_height() -> u32 {
    768
}
fn default_cost_per_hour_millicents() -> u64 {
    12000
}

// NOTE: Clone creates additional copies of api_key in memory.
// Consider using Arc<BrowserbaseConfig> to share instead of cloning.
#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserbaseConfig {
    #[serde(skip_serializing)]
    pub api_key: String,
    pub project_id: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_browser_width")]
    pub browser_width: u32,
    #[serde(default = "default_browser_height")]
    pub browser_height: u32,
    /// Cost per browser-hour in millicents (default: 12000 = $0.12/hour)
    #[serde(default = "default_cost_per_hour_millicents")]
    pub cost_per_hour_millicents: u64,
}

impl Drop for BrowserbaseConfig {
    fn drop(&mut self) {
        self.api_key.zeroize();
    }
}

impl std::fmt::Debug for BrowserbaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserbaseConfig")
            .field("api_key", &"[REDACTED]")
            .field("project_id", &self.project_id)
            .field("base_url", &self.base_url)
            .field("browser_width", &self.browser_width)
            .field("browser_height", &self.browser_height)
            .field("cost_per_hour_millicents", &self.cost_per_hour_millicents)
            .finish()
    }
}

impl Default for BrowserbaseConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            project_id: String::new(),
            base_url: default_base_url(),
            browser_width: default_browser_width(),
            browser_height: default_browser_height(),
            cost_per_hour_millicents: default_cost_per_hour_millicents(),
        }
    }
}

// ---------------------------------------------------------------------------
// Session types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSession {
    pub id: String,
    pub project_id: String,
    pub status: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_settings: Option<BrowserSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BrowserbaseError {
    ApiError(String),
    ParseError(String),
    ConfigError(String),
}

impl fmt::Display for BrowserbaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrowserbaseError::ApiError(msg) => write!(f, "Browserbase API error: {msg}"),
            BrowserbaseError::ParseError(msg) => write!(f, "Browserbase parse error: {msg}"),
            BrowserbaseError::ConfigError(msg) => write!(f, "Browserbase config error: {msg}"),
        }
    }
}

impl std::error::Error for BrowserbaseError {}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct BrowserbaseClient {
    config: BrowserbaseConfig,
    http: reqwest::Client,
}

impl BrowserbaseClient {
    pub fn new(config: BrowserbaseConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        Self { config, http }
    }

    /// Create a new browser session.
    pub async fn create_session(&self) -> Result<BrowserSession, BrowserbaseError> {
        let req = SessionCreateRequest {
            project_id: self.config.project_id.clone(),
            browser_settings: Some(BrowserSettings {
                viewport: Some(Viewport {
                    width: self.config.browser_width,
                    height: self.config.browser_height,
                }),
            }),
        };

        info!(
            project_id = %self.config.project_id,
            "Creating Browserbase session"
        );

        let resp = self
            .http
            .post(format!("{}/v1/sessions", self.config.base_url))
            .header("x-bb-api-key", &self.config.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| BrowserbaseError::ApiError(format!("request failed: {e}")))?;

        let status = resp.status().as_u16();
        if status != 200 && status != 201 {
            let body = resp.text().await.unwrap_or_default();
            warn!(status, body = %body, "Browserbase create_session failed");
            return Err(BrowserbaseError::ApiError(format!(
                "HTTP {status}: {body}"
            )));
        }

        let session = resp
            .json::<BrowserSession>()
            .await
            .map_err(|e| BrowserbaseError::ParseError(format!("invalid response: {e}")))?;

        info!(session_id = %session.id, "Browserbase session created");
        Ok(session)
    }

    /// Validate that a session_id is safe for URL path interpolation.
    fn validate_session_id(id: &str) -> Result<(), BrowserbaseError> {
        if id.is_empty() || id.len() > 64 {
            return Err(BrowserbaseError::ApiError("Invalid session_id length".to_string()));
        }
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(BrowserbaseError::ApiError("Invalid session_id characters".to_string()));
        }
        Ok(())
    }

    /// Close/stop a browser session.
    pub async fn close_session(&self, session_id: &str) -> Result<(), BrowserbaseError> {
        Self::validate_session_id(session_id)?;
        info!(session_id, "Closing Browserbase session");

        let resp = self
            .http
            .post(format!(
                "{}/v1/sessions/{session_id}/stop",
                self.config.base_url
            ))
            .header("x-bb-api-key", &self.config.api_key)
            .send()
            .await
            .map_err(|e| BrowserbaseError::ApiError(format!("request failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(session_id, body = %body, "Browserbase close_session failed");
            return Err(BrowserbaseError::ApiError(format!(
                "close failed: {body}"
            )));
        }

        info!(session_id, "Browserbase session closed");
        Ok(())
    }

    /// Get session details (including live_url for replay).
    pub async fn get_session(
        &self,
        session_id: &str,
    ) -> Result<BrowserSession, BrowserbaseError> {
        Self::validate_session_id(session_id)?;
        let resp = self
            .http
            .get(format!(
                "{}/v1/sessions/{session_id}",
                self.config.base_url
            ))
            .header("x-bb-api-key", &self.config.api_key)
            .send()
            .await
            .map_err(|e| BrowserbaseError::ApiError(format!("request failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BrowserbaseError::ApiError(format!(
                "get failed: {body}"
            )));
        }

        resp.json::<BrowserSession>()
            .await
            .map_err(|e| BrowserbaseError::ParseError(format!("invalid response: {e}")))
    }

    /// List recent sessions for the project.
    pub async fn list_sessions(
        &self,
        limit: u32,
    ) -> Result<Vec<BrowserSession>, BrowserbaseError> {
        let resp = self
            .http
            .get(format!(
                "{}/v1/sessions?limit={limit}",
                self.config.base_url
            ))
            .header("x-bb-api-key", &self.config.api_key)
            .send()
            .await
            .map_err(|e| BrowserbaseError::ApiError(format!("request failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BrowserbaseError::ApiError(format!(
                "list failed: {body}"
            )));
        }

        resp.json::<Vec<BrowserSession>>()
            .await
            .map_err(|e| BrowserbaseError::ParseError(format!("invalid response: {e}")))
    }

    /// Calculate cost for a session duration.
    pub fn calculate_cost_millicents(&self, duration_seconds: u64) -> u64 {
        let hours = duration_seconds as f64 / 3600.0;
        (hours * self.config.cost_per_hour_millicents as f64).ceil() as u64
    }

    /// Get the session live replay URL.
    pub fn replay_url(&self, session_id: &str) -> String {
        format!("https://www.browserbase.com/sessions/{session_id}")
    }
}

// ---------------------------------------------------------------------------
// Config loading helper
// ---------------------------------------------------------------------------

/// Load Browserbase config from the DuDuClaw home directory.
/// Looks for `browserbase_api_key` (or encrypted `browserbase_api_key_enc`) and
/// `browserbase_project_id` in config.toml.
pub fn load_config(home_dir: &Path) -> Result<BrowserbaseConfig, BrowserbaseError> {
    let config_path = home_dir.join("config.toml");
    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| BrowserbaseError::ConfigError(format!("failed to read config: {e}")))?;
    let table: toml::Table = content
        .parse()
        .map_err(|e| BrowserbaseError::ConfigError(format!("invalid TOML: {e}")))?;

    // Try encrypted field first (browserbase_api_key_enc), fall back to plaintext.
    let api_key: String = table
        .get("browserbase_api_key_enc")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .and_then(|enc_val| {
            let key = crate::config_crypto::load_keyfile_public(home_dir)?;
            let engine = duduclaw_security::crypto::CryptoEngine::new(&key).ok()?;
            engine.decrypt_string(enc_val).ok()
        })
        .or_else(|| {
            let plain = table.get("browserbase_api_key")?.as_str()?;
            if plain.is_empty() { return None; }
            warn!(
                "browserbase_api_key is stored as plaintext in config.toml; \
                 migrate to browserbase_api_key_enc for better security"
            );
            Some(plain.to_string())
        })
        .ok_or_else(|| {
            BrowserbaseError::ConfigError(
                "browserbase_api_key not found in config.toml".into(),
            )
        })?;

    let project_id = table
        .get("browserbase_project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            BrowserbaseError::ConfigError(
                "browserbase_project_id not found in config.toml".into(),
            )
        })?;

    let cfg = BrowserbaseConfig {
        api_key,
        project_id: project_id.to_string(),
        ..BrowserbaseConfig::default()
    };

    if !cfg.base_url.starts_with("https://") {
        warn!(base_url = %cfg.base_url, "Browserbase base_url should use HTTPS in production");
    }

    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_default() {
        let cfg = BrowserbaseConfig::default();
        assert_eq!(cfg.base_url, "https://api.browserbase.com");
        assert_eq!(cfg.browser_width, 1024);
        assert_eq!(cfg.cost_per_hour_millicents, 12000);
    }

    #[test]
    fn cost_calculation() {
        let client = BrowserbaseClient::new(BrowserbaseConfig::default());
        // 1 hour = 12000 millicents = $0.12
        assert_eq!(client.calculate_cost_millicents(3600), 12000);
        // 30 minutes = 6000 millicents
        assert_eq!(client.calculate_cost_millicents(1800), 6000);
        // 1 second rounds up
        assert_eq!(client.calculate_cost_millicents(1), 4); // ceil(1/3600 * 12000)
    }

    #[test]
    fn replay_url() {
        let client = BrowserbaseClient::new(BrowserbaseConfig::default());
        assert_eq!(
            client.replay_url("sess-123"),
            "https://www.browserbase.com/sessions/sess-123"
        );
    }

    #[test]
    fn error_display() {
        let e = BrowserbaseError::ApiError("test".into());
        assert_eq!(e.to_string(), "Browserbase API error: test");
    }
}
