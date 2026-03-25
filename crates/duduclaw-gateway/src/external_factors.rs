//! External factors collector for the evolution engine.
//!
//! Gathers signals from outside the agent's own conversation history
//! to enrich reflection with real-world context.

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;

use duduclaw_core::types::ExternalFactorsConfig;

/// Collected external context for a single reflection cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalContext {
    /// User feedback signals since last reflection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_feedback: Vec<FeedbackSignal>,
    /// Security events relevant to this agent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security_events: Vec<SecurityEvent>,
    /// Channel performance metrics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_metrics: Option<ChannelMetrics>,
    /// Business context from Odoo or external systems.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub business_context: Vec<BusinessSignal>,
    /// Peer agent observations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub peer_signals: Vec<PeerSignal>,
}

impl ExternalContext {
    pub fn empty() -> Self {
        Self {
            user_feedback: Vec::new(),
            security_events: Vec::new(),
            channel_metrics: None,
            business_context: Vec::new(),
            peer_signals: Vec::new(),
        }
    }

    /// Check if there's anything meaningful to include.
    pub fn has_content(&self) -> bool {
        !self.user_feedback.is_empty()
            || !self.security_events.is_empty()
            || self.channel_metrics.is_some()
            || !self.business_context.is_empty()
            || !self.peer_signals.is_empty()
    }

    /// Convert to a text prompt addendum for the evolution engine.
    pub fn to_prompt(&self) -> String {
        if !self.has_content() {
            return String::new();
        }

        let mut sections = Vec::new();

        if !self.user_feedback.is_empty() {
            let mut lines = vec!["## User Feedback Signals".to_string()];
            for fb in &self.user_feedback {
                let icon = match fb.signal_type.as_str() {
                    "positive" => "+",
                    "negative" => "-",
                    "correction" => "~",
                    _ => "?",
                };
                lines.push(format!("[{icon}] {}: {}", fb.channel, fb.detail));
            }
            sections.push(lines.join("\n"));
        }

        if !self.security_events.is_empty() {
            let mut lines = vec!["## Security Events".to_string()];
            for evt in &self.security_events {
                lines.push(format!("[{}] {}: {}", evt.severity, evt.event_type, evt.summary));
            }
            sections.push(lines.join("\n"));
        }

        if let Some(metrics) = &self.channel_metrics {
            sections.push(format!(
                "## Channel Metrics\n\
                 Messages handled: {}\n\
                 Avg response time: {:.1}s\n\
                 Error rate: {:.1}%\n\
                 Active channels: {}",
                metrics.messages_handled,
                metrics.avg_response_time_ms as f64 / 1000.0,
                metrics.error_rate_pct,
                metrics.active_channels,
            ));
        }

        if !self.business_context.is_empty() {
            let mut lines = vec!["## Business Context".to_string()];
            for sig in &self.business_context {
                lines.push(format!("[{}] {}: {}", sig.source, sig.metric, sig.value));
            }
            sections.push(lines.join("\n"));
        }

        if !self.peer_signals.is_empty() {
            let mut lines = vec!["## Peer Agent Observations".to_string()];
            for peer in &self.peer_signals {
                lines.push(format!(
                    "{} ({}): {} tasks, {:.0}% success",
                    peer.agent_id, peer.role, peer.tasks_completed, peer.success_rate * 100.0
                ));
            }
            sections.push(lines.join("\n"));
        }

        format!("\n---\n# External Context\n{}", sections.join("\n\n"))
    }
}

// ── Signal types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackSignal {
    pub signal_type: String, // positive, negative, correction
    pub channel: String,
    pub detail: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub event_type: String,
    pub severity: String,
    pub summary: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMetrics {
    pub messages_handled: u64,
    pub avg_response_time_ms: u64,
    pub error_rate_pct: f64,
    pub active_channels: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusinessSignal {
    pub source: String, // odoo, webhook, manual
    pub metric: String,
    pub value: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSignal {
    pub agent_id: String,
    pub role: String,
    pub tasks_completed: u32,
    pub success_rate: f64,
}

// ── Collector ───────────────────────────────────────────────

/// Collect external factors for an agent's reflection.
pub async fn collect_external_factors(
    home_dir: &Path,
    agent_id: &str,
    config: &ExternalFactorsConfig,
) -> ExternalContext {
    let mut ctx = ExternalContext::empty();

    if config.user_feedback {
        ctx.user_feedback = collect_user_feedback(home_dir, agent_id).await;
    }

    if config.security_events {
        ctx.security_events = collect_security_events(home_dir, agent_id).await;
    }

    if config.channel_metrics {
        ctx.channel_metrics = collect_channel_metrics(home_dir).await;
    }

    if config.business_context {
        ctx.business_context = collect_business_context(home_dir).await;
    }

    if config.peer_signals {
        ctx.peer_signals = collect_peer_signals(home_dir, agent_id).await;
    }

    if ctx.has_content() {
        info!(agent = agent_id, "External factors collected for reflection");
    }

    ctx
}

// ── Individual collectors ───────────────────────────────────

/// Maximum feedback.jsonl file size before truncation (5 MB).
const MAX_FEEDBACK_SIZE: u64 = 5 * 1024 * 1024;

/// Collect user feedback from bus_queue (thumbs up/down, corrections).
async fn collect_user_feedback(home_dir: &Path, agent_id: &str) -> Vec<FeedbackSignal> {
    let feedback_path = home_dir.join("feedback.jsonl");

    // Truncate if file exceeds size limit (BE-M3)
    if let Ok(meta) = tokio::fs::metadata(&feedback_path).await {
        if meta.len() > MAX_FEEDBACK_SIZE {
            tracing::warn!(
                size = meta.len(),
                "feedback.jsonl exceeds {}MB — truncating old entries",
                MAX_FEEDBACK_SIZE / 1024 / 1024
            );
            if let Ok(content) = tokio::fs::read_to_string(&feedback_path).await {
                let lines: Vec<&str> = content.lines().collect();
                // Keep only the last 1000 lines
                let keep = lines.len().saturating_sub(1000);
                let truncated = lines[keep..].join("\n");
                let _ = tokio::fs::write(&feedback_path, truncated).await;
            }
        }
    }

    let content = match tokio::fs::read_to_string(&feedback_path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let since = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            v.get("agent_id").and_then(|a| a.as_str()) == Some(agent_id)
                && v.get("timestamp")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t > since.as_str())
        })
        .map(|v| FeedbackSignal {
            signal_type: v["type"].as_str().unwrap_or("unknown").to_string(),
            channel: v["channel"].as_str().unwrap_or("").to_string(),
            detail: v["detail"].as_str().unwrap_or("").to_string(),
            timestamp: v["timestamp"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

/// Collect security events from the audit log.
async fn collect_security_events(home_dir: &Path, agent_id: &str) -> Vec<SecurityEvent> {
    let events = duduclaw_security::audit::read_recent_events(home_dir, 20);
    events
        .into_iter()
        .filter(|e| e.agent_id == agent_id || e.agent_id == "*")
        .map(|e| SecurityEvent {
            event_type: e.event_type,
            severity: format!("{:?}", e.severity),
            summary: e.details.to_string().chars().take(100).collect(),
            timestamp: e.timestamp,
        })
        .collect()
}

/// Collect channel activity metrics from session DB.
async fn collect_channel_metrics(home_dir: &Path) -> Option<ChannelMetrics> {
    // Read from a metrics file that channel bots write to
    let metrics_path = home_dir.join("channel_metrics.json");
    let content = tokio::fs::read_to_string(&metrics_path).await.ok()?;
    serde_json::from_str(&content).ok()
}

/// Collect business context from Odoo events or external webhooks.
async fn collect_business_context(home_dir: &Path) -> Vec<BusinessSignal> {
    let queue_path = home_dir.join("bus_queue.jsonl");
    let content = match tokio::fs::read_to_string(&queue_path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let since = (Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|v| {
            v.get("type").and_then(|t| t.as_str()) == Some("odoo_event")
                && v.get("timestamp")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t > since.as_str())
        })
        .map(|v| BusinessSignal {
            source: "odoo".to_string(),
            metric: v["event_type"].as_str().unwrap_or("").to_string(),
            value: v["data"].to_string().chars().take(100).collect(),
            timestamp: v["timestamp"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

/// Collect peer agent performance signals.
async fn collect_peer_signals(home_dir: &Path, self_agent_id: &str) -> Vec<PeerSignal> {
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut peers = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() { continue; }

        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        if name == self_agent_id || name.starts_with('_') { continue; }

        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await {
            if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content) {
                peers.push(PeerSignal {
                    agent_id: name,
                    role: format!("{:?}", config.agent.role),
                    tasks_completed: 0, // Would be populated from actual metrics
                    success_rate: 0.0,
                });
            }
        }
    }

    peers
}

// ── MCP tool: submit user feedback ──────────────────────────

/// Write a feedback signal to feedback.jsonl.
pub async fn submit_feedback(
    home_dir: &Path,
    agent_id: &str,
    signal_type: &str,
    channel: &str,
    detail: &str,
) -> Result<(), String> {
    let path = home_dir.join("feedback.jsonl");
    let entry = json!({
        "agent_id": agent_id,
        "type": signal_type,
        "channel": channel,
        "detail": detail,
        "timestamp": Utc::now().to_rfc3339(),
    });

    let line = serde_json::to_string(&entry).map_err(|e| format!("Serialize: {e}"))?;

    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{line}");
        }
    })
    .await
    .map_err(|e| format!("Write: {e}"))?;

    Ok(())
}
