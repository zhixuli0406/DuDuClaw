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
    /// Local model configuration (optional — enables local inference for this agent)
    #[serde(default)]
    pub local: Option<LocalModelConfig>,
}

/// Configuration for a local LLM model (per-agent).
///
/// Each agent can independently choose to use a local model, Claude API, or both.
/// When `prefer_local = true`, the agent tries the local model first and falls back
/// to Claude Code SDK if local inference fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LocalModelConfig {
    /// Model file path or id (e.g., "qwen3-8b-q4_k_m" or full path to .gguf)
    pub model: String,
    /// Backend type: "llama_cpp", "openai_compat", "mistral_rs"
    #[serde(default = "default_local_backend")]
    pub backend: String,
    /// Context window size
    #[serde(default = "default_local_context")]
    pub context_length: u32,
    /// Number of GPU layers to offload (-1 = all)
    #[serde(default = "default_local_gpu_layers")]
    pub gpu_layers: i32,
    /// Whether to prefer local model over Claude API when available.
    /// If true: try local → fallback to API. If false: always use API.
    #[serde(default)]
    pub prefer_local: bool,
    /// Use the confidence router to decide per-query whether to use local or API.
    /// Overrides prefer_local for complex queries that need Claude-level reasoning.
    #[serde(default)]
    pub use_router: bool,
}

fn default_local_backend() -> String {
    "llama_cpp".to_string()
}

fn default_local_context() -> u32 {
    4096
}

fn default_local_gpu_layers() -> i32 {
    -1
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

    // ── Prediction-driven evolution (Phase 1) ──

    /// When true, evolution is driven by prediction errors instead of fixed timers.
    /// Heartbeat scheduler still runs for bus polling and silence checking.
    #[serde(default)]
    pub prediction_driven: bool,

    /// Enable GVU (Generator-Verifier-Updater) loop for evolution proposals (Phase 2).
    #[serde(default)]
    pub gvu_enabled: bool,

    /// Enable cognitive memory layer with episodic/semantic separation (Phase 3).
    #[serde(default)]
    pub cognitive_memory: bool,

    /// Maximum hours of silence before a forced Meso reflection is triggered.
    /// Only applies when `prediction_driven` is true.
    #[serde(default = "default_max_silence_hours")]
    pub max_silence_hours: f64,

    /// Maximum GVU generation attempts per evolution cycle (default 3).
    #[serde(default = "default_max_gvu_generations")]
    pub max_gvu_generations: u32,

    /// Observation period in hours after a SOUL.md change (default 24).
    #[serde(default = "default_observation_period_hours")]
    pub observation_period_hours: f64,

    // ── Skill lifecycle ──

    /// Token budget for skills in system prompt (default 2500).
    #[serde(default = "default_skill_token_budget")]
    pub skill_token_budget: u32,

    /// Maximum concurrently active skills per agent (default 5).
    #[serde(default = "default_max_active_skills")]
    pub max_active_skills: usize,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            micro_reflection: true,
            meso_reflection: true,
            macro_reflection: true,
            skill_auto_activate: false,
            skill_security_scan: true,
            external_factors: Default::default(),
            prediction_driven: false,
            gvu_enabled: false,
            cognitive_memory: false,
            max_silence_hours: 12.0,
            max_gvu_generations: 3,
            observation_period_hours: 24.0,
            skill_token_budget: 2500,
            max_active_skills: 5,
        }
    }
}

fn default_max_silence_hours() -> f64 {
    12.0
}

fn default_max_gvu_generations() -> u32 {
    3
}

fn default_observation_period_hours() -> f64 {
    24.0
}

fn default_skill_token_budget() -> u32 {
    2500
}

fn default_max_active_skills() -> usize {
    5
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

/// Cognitive memory layer classification (Phase 3 — CoALA inspired).
///
/// Episodic: specific experiences — conversation summaries, reflection conclusions.
/// Semantic: generalised knowledge — user preferences, domain rules, principles.
/// Procedural: reserved for future use (skills, SOUL.md are tracked separately).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayer {
    Episodic,
    Semantic,
    Procedural,
}

impl Default for MemoryLayer {
    fn default() -> Self {
        Self::Episodic
    }
}

impl MemoryLayer {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "semantic" => Self::Semantic,
            "procedural" => Self::Procedural,
            _ => Self::Episodic,
        }
    }
}

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

    // ── Cognitive memory fields (Phase 3) ──

    /// Which cognitive layer this memory belongs to.
    #[serde(default)]
    pub layer: MemoryLayer,

    /// Importance score (0.0–10.0). Higher = more important, less likely to decay.
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Number of times this memory has been retrieved.
    #[serde(default)]
    pub access_count: u32,

    /// Last time this memory was accessed via search.
    #[serde(default)]
    pub last_accessed: Option<DateTime<Utc>>,

    /// What event produced this memory (e.g., "micro_reflection", "user_feedback").
    #[serde(default)]
    pub source_event: String,
}

fn default_importance() -> f64 {
    5.0
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
