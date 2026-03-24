use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Agent configuration types
// ---------------------------------------------------------------------------

/// Role an agent plays in the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Main,
    Specialist,
    Worker,
}

/// Current lifecycle status of an agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Paused,
    Terminated,
}

/// LLM model selection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelConfig {
    pub preferred: String,
    pub fallback: String,
    pub account_pool: Vec<String>,
}

/// Mount point mapping between host and container.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MountConfig {
    pub host: String,
    pub container: String,
    pub readonly: bool,
}

/// Container runtime configuration for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContainerConfig {
    pub timeout_ms: u64,
    pub max_concurrent: u32,
    pub readonly_project: bool,
    pub additional_mounts: Vec<MountConfig>,
    /// Run agent tasks inside a sandboxed container (Docker / Apple Container).
    #[serde(default)]
    pub sandbox_enabled: bool,
    /// Allow network access inside the sandbox (default: false = offline).
    #[serde(default)]
    pub network_access: bool,
}

/// Heartbeat / scheduled-task configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HeartbeatConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub max_concurrent_runs: u32,
    pub cron: String,
}

/// Budget limits and warnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BudgetConfig {
    pub monthly_limit_cents: u64,
    pub warn_threshold_percent: u8,
    pub hard_stop: bool,
}

/// Permission flags that constrain what an agent is allowed to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PermissionsConfig {
    pub can_create_agents: bool,
    pub can_send_cross_agent: bool,
    pub can_modify_own_skills: bool,
    pub can_modify_own_soul: bool,
    pub can_schedule_tasks: bool,
    pub allowed_channels: Vec<String>,
}

/// Evolution / self-improvement configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EvolutionConfig {
    pub micro_reflection: bool,
    pub meso_reflection: bool,
    pub macro_reflection: bool,
    pub skill_auto_activate: bool,
    pub skill_security_scan: bool,
    /// External factors to include in reflections.
    #[serde(default)]
    pub external_factors: ExternalFactorsConfig,
}

/// Configuration for external factors that feed into the evolution engine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExternalFactorsConfig {
    /// Include user feedback signals (thumbs up/down, corrections).
    #[serde(default)]
    pub user_feedback: bool,
    /// Include security events (injection attempts, SOUL drift) in reflection.
    #[serde(default)]
    pub security_events: bool,
    /// Include channel activity metrics (response times, error rates).
    #[serde(default)]
    pub channel_metrics: bool,
    /// Include Odoo business context (pipeline changes, KPIs) in reflection.
    #[serde(default)]
    pub business_context: bool,
    /// Include peer agent performance signals (cross-agent learning).
    #[serde(default)]
    pub peer_signals: bool,
}

/// Top-level agent identity (the `[agent]` table in agent.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentInfo {
    pub name: String,
    pub display_name: String,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub trigger: String,
    pub reports_to: String,
    pub icon: String,
}

/// Full agent configuration file (`agent.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AgentConfig {
    pub agent: AgentInfo,
    pub model: ModelConfig,
    pub container: ContainerConfig,
    pub heartbeat: HeartbeatConfig,
    pub budget: BudgetConfig,
    pub permissions: PermissionsConfig,
    pub evolution: EvolutionConfig,
}

// ---------------------------------------------------------------------------
// Messaging types
// ---------------------------------------------------------------------------

/// Direction / purpose of a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Incoming,
    Outgoing,
    Internal,
    Delegate,
    DelegateResponse,
}

/// A single message flowing through the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Message {
    pub id: String,
    pub message_type: MessageType,
    pub channel: String,
    pub chat_id: String,
    pub sender: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub agent_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Memory types
// ---------------------------------------------------------------------------

/// A stored memory entry for an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemoryEntry {
    pub id: String,
    pub agent_id: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub tags: Vec<String>,
    pub embedding: Option<Vec<f32>>,
}

/// A time window used for filtering / summarisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Container / runtime types
// ---------------------------------------------------------------------------

/// Opaque identifier for a running container.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContainerId(pub String);

/// Health status returned by a container runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RuntimeHealth {
    pub healthy: bool,
    pub message: String,
    pub uptime_seconds: u64,
}

// ---------------------------------------------------------------------------
// Doctor / diagnostic types
// ---------------------------------------------------------------------------

/// Outcome of a single doctor check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// A single diagnostic check result produced by `duduclaw doctor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub can_repair: bool,
    pub repair_hint: Option<String>,
}
