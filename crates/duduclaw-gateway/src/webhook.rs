//! Generic webhook endpoint — receives HTTP POST from external systems and
//! routes them to the target agent via `bus_queue.jsonl`.
//!
//! URL: `POST /webhook/{agent_id}`
//! Optional HMAC-SHA256 signature verification via `X-Hub-Signature-256` header.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use axum::{
    Router,
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::RwLock;
use tracing::{info, warn};

use duduclaw_agent::registry::AgentRegistry;

type HmacSha256 = Hmac<Sha256>;

/// Cached webhook secret with TTL to reflect config changes without restart.
struct CachedSecret {
    value: Option<String>,
    cached_at: std::time::Instant,
}

/// TTL for cached webhook secrets — re-read from disk after this duration.
const SECRET_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// In-memory cache for webhook secrets to avoid a disk read on every request (R4-M4).
/// Entries expire after SECRET_CACHE_TTL to reflect config changes (R5 fix).
static WEBHOOK_SECRET_CACHE: OnceLock<RwLock<HashMap<String, CachedSecret>>> = OnceLock::new();

fn webhook_secret_cache() -> &'static RwLock<HashMap<String, CachedSecret>> {
    WEBHOOK_SECRET_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Cached wrapper around `get_webhook_secret`.
///
/// Returns the cached value if fresh (< 60s); falls back to a disk read on miss or expiry.
async fn get_webhook_secret_cached(home_dir: &std::path::Path, agent_id: &str) -> Option<String> {
    let cache = webhook_secret_cache();

    // Check cache first (read lock)
    {
        let read = cache.read().await;
        if let Some(cached) = read.get(agent_id) {
            if cached.cached_at.elapsed() < SECRET_CACHE_TTL {
                return cached.value.clone();
            }
        }
    }

    // Cache miss or expired — read from disk and update
    let secret = get_webhook_secret(home_dir, agent_id).await;
    {
        let mut write = cache.write().await;
        write.insert(agent_id.to_string(), CachedSecret {
            value: secret.clone(),
            cached_at: std::time::Instant::now(),
        });
    }
    secret
}

/// Shared state for webhook handler.
pub struct WebhookState {
    pub home_dir: PathBuf,
    pub registry: Arc<RwLock<AgentRegistry>>,
    /// Rate limiter: agent_id → (count, window_start)
    rate_limits: tokio::sync::Mutex<std::collections::HashMap<String, (u32, chrono::DateTime<Utc>)>>,
}

impl WebhookState {
    pub fn new(home_dir: PathBuf, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        Self {
            home_dir,
            registry,
            rate_limits: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Check rate limit: 60 requests per agent per minute.
    async fn check_rate_limit(&self, agent_id: &str) -> bool {
        let mut limits = self.rate_limits.lock().await;
        let now = Utc::now();
        let entry = limits
            .entry(agent_id.to_string())
            .or_insert((0, now));

        // Reset window if older than 60 seconds
        if (now - entry.1).num_seconds() > 60 {
            *entry = (0, now);
        }

        entry.0 += 1;
        entry.0 <= 60
    }
}

/// Build the webhook router.
pub fn webhook_router(state: Arc<WebhookState>) -> Router {
    Router::new()
        .route("/webhook/{agent_id}", post(handle_webhook))
        .with_state(state)
}

async fn handle_webhook(
    State(state): State<Arc<WebhookState>>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Validate agent_id format (prevent path traversal)
    if !duduclaw_core::is_valid_agent_id(&agent_id) {
        warn!(agent_id, "Webhook: invalid agent_id format");
        return StatusCode::BAD_REQUEST.into_response();
    }

    // Validate agent exists
    {
        let reg = state.registry.read().await;
        if reg.get(&agent_id).is_none() {
            warn!(agent_id, "Webhook: unknown agent");
            return StatusCode::NOT_FOUND.into_response();
        }
    }

    // Rate limit
    if !state.check_rate_limit(&agent_id).await {
        warn!(agent_id, "Webhook: rate limited");
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // HMAC-SHA256 signature verification — secret is mandatory; reject if not configured
    let secret = get_webhook_secret_cached(&state.home_dir, &agent_id).await;
    if secret.is_none() {
        warn!(agent_id = %agent_id, "Webhook rejected: no secret configured");
        return (StatusCode::FORBIDDEN, "Webhook secret not configured").into_response();
    }
    if let Some(secret) = secret {
        let sig_str = match headers.get("x-hub-signature-256") {
            Some(h) => h.to_str().unwrap_or(""),
            None => {
                warn!(agent_id, "Webhook: missing required X-Hub-Signature-256 header");
                return StatusCode::UNAUTHORIZED.into_response();
            }
        };
        if !verify_signature(&body, &secret, sig_str) {
            warn!(agent_id, "Webhook: signature verification failed");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    // Parse body as JSON (or use raw string)
    let payload = match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(json) => serde_json::to_string(&json).unwrap_or_default(),
        Err(_) => String::from_utf8_lossy(&body).to_string(),
    };

    // Write to bus_queue.jsonl
    let message = serde_json::json!({
        "type": "agent_message",
        "message_id": uuid::Uuid::new_v4().to_string(),
        "agent_id": agent_id,
        "payload": format!("[Webhook event]\n{payload}"),
        "timestamp": Utc::now().to_rfc3339(),
    });

    let queue_path = state.home_dir.join("bus_queue.jsonl");
    let line = serde_json::to_string(&message).unwrap_or_default();

    // Write with flock for safe concurrent access (R3-H2)
    if let Err(e) = crate::dispatcher::append_line(&queue_path, &line).await {
        warn!(agent_id, error = %e, "Webhook: failed to write to queue");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    info!(agent_id, payload_bytes = body.len(), "Webhook event dispatched");

    // Immediate notification: if payload contains "notify" field, queue a proactive notification
    // alongside the bus queue entry. This enables real-time alerts for critical events.
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
        if let Some(raw_msg) = json.get("notify").and_then(|v| v.as_str()) {
            // Sanitize: length limit + source label + strip control chars
            let notify_msg: String = raw_msg.chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(500)
                .collect();
            let notify_msg = format!("[Webhook] {notify_msg}");

            // Read agent's proactive config for notify_channel/chat_id
            let agent_dir = state.home_dir.join("agents").join(&agent_id);
            let config_path = agent_dir.join("agent.toml");
            if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
                    if config.proactive.enabled
                        && !config.proactive.notify_channel.is_empty()
                        && !config.proactive.notify_chat_id.is_empty()
                    {
                        let notification = serde_json::json!({
                            "type": "proactive_notification",
                            "agent_id": agent_id,
                            "channel": config.proactive.notify_channel,
                            "chat_id": config.proactive.notify_chat_id,
                            "message": notify_msg,
                            "timestamp": Utc::now().to_rfc3339(),
                            "source": "webhook",
                        });
                        let notif_line = serde_json::to_string(&notification).unwrap_or_default();
                        let _ = crate::dispatcher::append_line(&queue_path, &notif_line).await;
                        info!(agent_id, "Webhook: immediate notification queued");
                    }
                }
            }
        }
    }

    StatusCode::ACCEPTED.into_response()
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Verify HMAC-SHA256 signature (GitHub/Meta/Slack webhook format).
fn verify_signature(body: &[u8], secret: &str, signature: &str) -> bool {
    let expected = signature.strip_prefix("sha256=").unwrap_or(signature);
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(computed.as_bytes(), expected.as_bytes())
}

/// Look up webhook secret for an agent from config.
async fn get_webhook_secret(home_dir: &std::path::Path, agent_id: &str) -> Option<String> {
    let config_path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let content = tokio::fs::read_to_string(&config_path).await.ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("webhook")
        .and_then(|w| w.get("secret"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}
