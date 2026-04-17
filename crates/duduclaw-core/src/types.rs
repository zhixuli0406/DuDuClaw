use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Agent configuration types
// ---------------------------------------------------------------------------

/// Role an agent plays in the system.
///
/// Serialised as kebab-case in `agent.toml`. Single-word variants look
/// identical to the old lowercase encoding (`main`, `worker`, `qa`, …) so
/// existing agent configs keep parsing. Multi-word variants use kebab-case
/// (e.g. `team-leader`, `product-manager`) which matches typical job-title
/// writing conventions.
///
/// When adding a new variant, also update the string-to-enum map in
/// [`crates/duduclaw-cli/src/mcp.rs`](../../../duduclaw-cli/src/mcp.rs)'s
/// `create_agent` MCP handler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRole {
    /// The top-level user-facing agent (only one per home directory).
    Main,
    /// Generic specialist — fallback when nothing more specific fits.
    Specialist,
    /// Low-privilege worker — used by the RBAC layer for leaf sub-agents.
    Worker,
    /// Software engineer / implementer (frontend, backend, devops, ML, …).
    #[serde(alias = "engineer")]
    Developer,
    /// Quality assurance — runs review / testing / red-team workflows.
    #[serde(alias = "quality-assurance", alias = "quality")]
    Qa,
    /// Planning / coordination — used for generic planners that don't
    /// cleanly fit `TeamLeader` or `ProductManager`.
    Planner,
    /// Team Leader — coordinates a sub-team, integrates reports, assigns
    /// work. Typically has `reports_to = ""` or a parent org.
    #[serde(alias = "tl", alias = "lead", alias = "teamlead")]
    TeamLeader,
    /// Product Manager — drives research and feature proposals for a
    /// specific project / domain.
    #[serde(alias = "pm")]
    ProductManager,
}

impl AgentRole {
    /// The canonical kebab-case string used in `agent.toml` and on the
    /// wire. This is the inverse of [`std::str::FromStr::from_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Specialist => "specialist",
            Self::Worker => "worker",
            Self::Developer => "developer",
            Self::Qa => "qa",
            Self::Planner => "planner",
            Self::TeamLeader => "team-leader",
            Self::ProductManager => "product-manager",
        }
    }

    /// Comma-separated list of all valid role strings, suitable for
    /// embedding in error messages.
    pub fn valid_values_help() -> &'static str {
        "main, specialist, worker, developer, qa, planner, team-leader, product-manager"
    }
}

impl std::str::FromStr for AgentRole {
    type Err = String;

    /// Parse an `AgentRole` from its canonical kebab-case encoding, with
    /// lenient matching on common aliases so old configs and natural-
    /// language inputs (`"team leader"`, `"product_manager"`, …) keep
    /// working.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Normalise separators + case so `team_leader`, `team leader`,
        // `Team-Leader`, etc. all land on the same variant.
        let normalised: String = s
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c == '_' || c == ' ' { '-' } else { c })
            .collect();

        Ok(match normalised.as_str() {
            "main" => Self::Main,
            "specialist" => Self::Specialist,
            "worker" => Self::Worker,
            "developer" | "engineer" => Self::Developer,
            "qa" | "quality-assurance" | "quality" => Self::Qa,
            "planner" => Self::Planner,
            "team-leader" | "teamleader" | "tl" | "lead" => Self::TeamLeader,
            "product-manager" | "productmanager" | "pm" => Self::ProductManager,
            _ => {
                return Err(format!(
                    "invalid role '{s}'. valid values: {}",
                    Self::valid_values_help()
                ))
            }
        })
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
    /// API mode for cloud calls: "cli" (default, via claude binary), "direct" (HTTP API),
    /// or "auto" (CLI first for zero-cost OAuth, fallback to Direct API when rate-limited).
    #[serde(default = "default_api_mode")]
    pub api_mode: String,
}

fn default_api_mode() -> String {
    "cli".to_string()
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
    /// Run agent tasks in an isolated git worktree (L0 lightweight isolation).
    /// Cheaper than container sandbox — creates a separate working directory
    /// so concurrent agents don't step on each other's files.
    #[serde(default)]
    pub worktree_enabled: bool,
    /// Automatically merge worktree branch back after successful task completion.
    #[serde(default = "default_true")]
    pub worktree_auto_merge: bool,
    /// Remove worktree after task completion (or after merge).
    #[serde(default = "default_true")]
    pub worktree_cleanup_on_exit: bool,
    /// Non-git-tracked files to copy into the worktree (e.g. `.env`, `.env.local`).
    #[serde(default)]
    pub worktree_copy_files: Vec<String>,
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

/// Capabilities controlling access to high-risk Claude Code native tools.
///
/// Each capability defaults to `false` (deny-by-default). Enabling a capability
/// removes it from the `--disallowedTools` list passed to the Claude CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilitiesConfig {
    /// Allow Claude Code's `computer_use` tool (screenshot + mouse + keyboard).
    /// WARNING: operates on the host display — only enable for attended local use.
    #[serde(default)]
    pub computer_use: bool,

    /// Allow running browser automation commands (playwright, puppeteer) via Bash.
    #[serde(default)]
    pub browser_via_bash: bool,

    /// Explicit tool allowlist. If non-empty, ONLY these tools are permitted.
    /// Takes precedence over individual capability flags.
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Explicit tool denylist. Tools listed here are always blocked,
    /// even if allowed by other flags. Evaluated after `allowed_tools`.
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Wiki visibility control — which agents can read this agent's wiki.
    /// `["*"]` = all agents (default, backward compatible).
    /// `[]` = no one except self (fully private).
    /// `["agnes", "bob"]` = only these agents can read.
    #[serde(default = "default_wiki_visible_to")]
    pub wiki_visible_to: Vec<String>,
}

fn default_wiki_visible_to() -> Vec<String> {
    vec!["*".to_string()]
}

impl Default for CapabilitiesConfig {
    fn default() -> Self {
        Self {
            computer_use: false,
            browser_via_bash: false,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            wiki_visible_to: default_wiki_visible_to(),
        }
    }
}

/// Programmatic Tool Calling (PTC) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PtcConfig {
    /// Enable PTC for this agent.
    pub enabled: bool,
    /// MCP tools the script may call via RPC.
    pub allowed_tools: Vec<String>,
    /// Max output tokens from script stdout.
    pub max_output_tokens: usize,
    /// Script execution timeout in seconds.
    pub timeout_seconds: u32,
}

impl Default for PtcConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_tools: vec![
                "web_search".to_string(),
                "memory_search".to_string(),
                "memory_store".to_string(),
                "send_message".to_string(),
                "send_to_agent".to_string(),
            ],
            max_output_tokens: 4096,
            timeout_seconds: 30,
        }
    }
}

impl CapabilitiesConfig {
    /// Compute the list of tools that should be disallowed for Claude CLI.
    ///
    /// Logic:
    /// 1. If `denied_tools` is non-empty, those are always blocked.
    /// 2. Individual capability flags control built-in high-risk tools.
    /// 3. Returns a deduplicated, sorted Vec suitable for `--disallowedTools`.
    pub fn disallowed_tools(&self) -> Vec<String> {
        let mut denied: Vec<String> = self.denied_tools.clone();

        // Deny-by-default high-risk tools unless explicitly enabled
        if !self.computer_use {
            denied.push("computer".to_string());
        }

        // Deduplicate and sort for deterministic CLI args
        denied.sort();
        denied.dedup();
        denied
    }
}

/// Evolution / self-improvement configuration.
///
/// Evolution is driven exclusively by the prediction engine (error-based triggering)
/// and the GVU self-play loop (Generator → Verifier → Updater).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EvolutionConfig {
    pub skill_auto_activate: bool,
    pub skill_security_scan: bool,
    /// External factors to include in reflections.
    #[serde(default)]
    pub external_factors: ExternalFactorsConfig,

    /// Enable GVU (Generator-Verifier-Updater) loop for evolution proposals.
    #[serde(default = "default_true")]
    pub gvu_enabled: bool,

    /// Enable cognitive memory layer with episodic/semantic separation.
    #[serde(default = "default_true")]
    pub cognitive_memory: bool,

    /// Maximum hours of silence before the heartbeat silence-breaker fires.
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

    // ── Skill auto-synthesis (P0) ──

    /// Enable automatic skill synthesis from episodic memory when repeated
    /// domain gaps are detected.
    #[serde(default)]
    pub skill_synthesis_enabled: bool,

    /// Number of repeated gap detections required before triggering synthesis.
    #[serde(default = "default_skill_synthesis_threshold")]
    pub skill_synthesis_threshold: u32,

    /// Cooldown hours after synthesizing a skill for the same topic.
    #[serde(default = "default_skill_synthesis_cooldown_hours")]
    pub skill_synthesis_cooldown_hours: u64,

    /// TTL (in conversations) for sandboxed trial skills before evaluation.
    #[serde(default = "default_skill_trial_ttl")]
    pub skill_trial_ttl: u32,

    // ── Cross-agent skill migration (P2) ──

    /// Enable automatic skill graduation to global scope when lift is proven.
    #[serde(default)]
    pub skill_graduation_enabled: bool,

    /// Minimum lift required for skill graduation.
    #[serde(default = "default_skill_graduation_min_lift")]
    pub skill_graduation_min_lift: f64,

    /// Enable cross-agent skill recommendations for new agents.
    #[serde(default)]
    pub skill_recommendation_enabled: bool,

    /// Minimum combined score for auto-activating recommended skills.
    #[serde(default = "default_skill_recommendation_threshold")]
    pub skill_recommendation_threshold: f64,

    // ── Curiosity-driven exploration (P4) ──

    /// Enable curiosity-driven proactive exploration of underexplored domains.
    #[serde(default)]
    pub curiosity_enabled: bool,

    /// Curiosity score threshold for triggering exploration.
    #[serde(default = "default_curiosity_threshold")]
    pub curiosity_threshold: f64,

    /// Maximum exploration actions per day (cost control).
    #[serde(default = "default_curiosity_max_daily")]
    pub curiosity_max_daily: u32,

    // ── Behavior monitoring (P1) ──

    /// Enable behavioral drift detection after skill activation.
    #[serde(default)]
    pub skill_behavior_monitor_enabled: bool,

    /// Drift magnitude threshold for flagging anomalous behavior (0.0–1.0).
    #[serde(default = "default_skill_behavior_drift_threshold")]
    pub skill_behavior_drift_threshold: f64,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            skill_auto_activate: false,
            skill_security_scan: true,
            external_factors: Default::default(),
            gvu_enabled: true,
            cognitive_memory: true,
            max_silence_hours: 12.0,
            max_gvu_generations: 3,
            observation_period_hours: 24.0,
            skill_token_budget: 2500,
            max_active_skills: 5,
            // Skill auto-synthesis
            skill_synthesis_enabled: false,
            skill_synthesis_threshold: 3,
            skill_synthesis_cooldown_hours: 24,
            skill_trial_ttl: 20,
            // Cross-agent migration
            skill_graduation_enabled: false,
            skill_graduation_min_lift: 0.1,
            skill_recommendation_enabled: false,
            skill_recommendation_threshold: 0.3,
            // Curiosity
            curiosity_enabled: false,
            curiosity_threshold: 0.6,
            curiosity_max_daily: 3,
            // Behavior monitoring
            skill_behavior_monitor_enabled: false,
            skill_behavior_drift_threshold: 0.3,
        }
    }
}

fn default_true() -> bool {
    true
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

fn default_skill_synthesis_threshold() -> u32 {
    3
}

fn default_skill_synthesis_cooldown_hours() -> u64 {
    24
}

fn default_skill_trial_ttl() -> u32 {
    20
}

fn default_skill_graduation_min_lift() -> f64 {
    0.1
}

fn default_skill_recommendation_threshold() -> f64 {
    0.3
}

fn default_curiosity_threshold() -> f64 {
    0.6
}

fn default_curiosity_max_daily() -> u32 {
    3
}

fn default_skill_behavior_drift_threshold() -> f64 {
    0.3
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

/// Per-agent channel configuration (e.g., dedicated Discord bot token).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ChannelsConfig {
    pub discord: Option<DiscordChannelConfig>,
    pub telegram: Option<TelegramChannelConfig>,
    pub line: Option<LineChannelConfig>,
    pub slack: Option<SlackChannelConfig>,
    pub whatsapp: Option<WhatsAppChannelConfig>,
    pub feishu: Option<FeishuChannelConfig>,
}

/// Per-agent Discord channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct DiscordChannelConfig {
    /// Plain-text bot token (or encrypted via `bot_token_enc`).
    pub bot_token: String,
    /// AES-256-GCM encrypted bot token (base64).
    pub bot_token_enc: Option<String>,
}


/// Per-agent Telegram channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct TelegramChannelConfig {
    pub bot_token: String,
    pub bot_token_enc: Option<String>,
}


/// Per-agent LINE channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct LineChannelConfig {
    pub channel_token: String,
    pub channel_token_enc: Option<String>,
    pub channel_secret: String,
    pub channel_secret_enc: Option<String>,
}


/// Per-agent Slack channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct SlackChannelConfig {
    pub app_token: String,
    pub app_token_enc: Option<String>,
    pub bot_token: String,
    pub bot_token_enc: Option<String>,
}


/// Per-agent WhatsApp channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct WhatsAppChannelConfig {
    pub access_token: String,
    pub access_token_enc: Option<String>,
    pub verify_token: String,
    pub phone_number_id: String,
    pub app_secret: String,
    pub app_secret_enc: Option<String>,
}


/// Per-agent Feishu (Lark) channel settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct FeishuChannelConfig {
    pub app_id: String,
    pub app_id_enc: Option<String>,
    pub app_secret: String,
    pub app_secret_enc: Option<String>,
    pub verification_token: String,
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
    /// High-risk tool capabilities (computer_use, browser, etc.)
    /// Defaults to all-denied if omitted from agent.toml.
    #[serde(default)]
    pub capabilities: CapabilitiesConfig,
    /// Proactive behavior configuration (PROACTIVE.md execution + notification).
    #[serde(default)]
    pub proactive: ProactiveConfig,
    /// Per-agent channel configuration (e.g., dedicated Discord bot token).
    #[serde(default)]
    pub channels: Option<ChannelsConfig>,
    /// Cultural context for adjusting behavioural signal interpretation.
    /// Defaults to zh-TW high-context settings.
    ///
    /// ```toml
    /// [cultural_context]
    /// locale = "zh-TW"
    /// high_context = true
    /// short_reply_threshold = 15
    /// silence_as_agreement_weight = 0.7
    /// indirect_disagreement_weight = 0.3
    /// ```
    #[serde(default)]
    pub cultural_context: CulturalContextConfig,
    /// Programmatic Tool Calling configuration.
    #[serde(default)]
    pub ptc: PtcConfig,
    /// Emotion-based sticker/reaction auto-sending configuration.
    /// Disabled by default — enable per-agent in `agent.toml [sticker]`.
    #[serde(default)]
    pub sticker: StickerConfig,
    /// MemGPT 3-layer memory configuration.
    /// Disabled by default — enable per-agent in `agent.toml [memory]`.
    #[serde(default)]
    pub memory: MemoryConfig,
}

/// MemGPT-style 3-layer memory configuration.
///
/// ```toml
/// [memory]
/// enabled = true
/// core_tokens = 2000
/// recall_tokens = 3000
/// archival_tokens = 1500
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct MemoryConfig {
    /// Whether the 3-layer memory system is enabled.
    pub enabled: bool,
    /// Max tokens for Core Memory (L1).
    pub core_tokens: u32,
    /// Max tokens for Recall Memory (L2).
    pub recall_tokens: u32,
    /// Max tokens for Archival Memory (L3).
    pub archival_tokens: u32,
    /// How many recent recall entries to auto-inject.
    pub recall_auto_inject: u32,
    /// How many archival entries to auto-retrieve.
    pub archival_auto_retrieve: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            core_tokens: 2000,
            recall_tokens: 3000,
            archival_tokens: 1500,
            recall_auto_inject: 10,
            archival_auto_retrieve: 5,
        }
    }
}

/// Cultural context for adjusting behavioural signal interpretation.
///
/// High-context cultures (East Asian) use indirect communication patterns.
/// Based on CHI 2024 "Cross-Cultural Perceptions of AI Conversational Agents"
/// and ScienceDirect 2025 "Culturally Responsive AI Chatbots".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct CulturalContextConfig {
    /// IANA locale (e.g., "zh-TW", "en-US").
    pub locale: String,
    /// High-context culture: silence/short replies may mean agreement.
    pub high_context: bool,
    /// Character count below which a reply is considered "short".
    pub short_reply_threshold: usize,
    /// Weight for silence-as-agreement interpretation (0.0-1.0).
    pub silence_as_agreement_weight: f64,
    /// Weight for indirect disagreement signals (0.0-1.0).
    pub indirect_disagreement_weight: f64,
}

impl Default for CulturalContextConfig {
    fn default() -> Self {
        Self {
            locale: "zh-TW".into(),
            high_context: true,
            short_reply_threshold: 15,
            silence_as_agreement_weight: 0.7,
            indirect_disagreement_weight: 0.3,
        }
    }
}

/// Expressiveness level for sticker sending frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Expressiveness {
    /// 0.5x probability multiplier — very sparse stickers.
    Minimal,
    /// 1.0x probability multiplier — balanced (default).
    #[default]
    Moderate,
    /// 2.0x probability multiplier — more frequent stickers.
    Expressive,
}


impl Expressiveness {
    /// Probability multiplier for this expressiveness level.
    pub fn multiplier(self) -> f32 {
        match self {
            Self::Minimal => 0.5,
            Self::Moderate => 1.0,
            Self::Expressive => 2.0,
        }
    }
}

/// Emotion-based sticker auto-sending configuration.
///
/// ```toml
/// [sticker]
/// enabled = true
/// probability = 0.3
/// intensity_threshold = 0.7
/// cooldown_messages = 5
/// expressiveness = "moderate"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct StickerConfig {
    /// Enable emotion-based sticker sending for this agent.
    pub enabled: bool,
    /// Base probability of sending a sticker when emotion is detected (0.0-1.0).
    pub probability: f32,
    /// Minimum emotion intensity to trigger sticker (0.0-1.0).
    pub intensity_threshold: f32,
    /// Minimum messages between stickers in the same session.
    pub cooldown_messages: u32,
    /// How expressive this agent is (multiplies probability).
    pub expressiveness: Expressiveness,
}

impl Default for StickerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            probability: 0.3,
            intensity_threshold: 0.7,
            cooldown_messages: 5,
            expressiveness: Expressiveness::Moderate,
        }
    }
}

impl StickerConfig {
    /// Clamp values to valid ranges after deserialization.
    pub fn sanitize(&mut self) {
        self.probability = self.probability.clamp(0.0, 1.0);
        self.intensity_threshold = self.intensity_threshold.clamp(0.0, 1.0);
    }
}

/// Proactive agent configuration — scheduled checks + user notification.
///
/// ```toml
/// [proactive]
/// enabled = true
/// check_interval = "*/30 * * * *"   # cron: every 30 min
/// quiet_hours_start = 23
/// quiet_hours_end = 8
/// max_messages_per_hour = 3
/// token_budget_per_check = 2000
/// notify_channel = "telegram"
/// notify_chat_id = "123456789"
/// timezone = "Asia/Taipei"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ProactiveConfig {
    /// Enable proactive checks for this agent.
    pub enabled: bool,
    /// Cron expression for check interval (default: every 30 minutes).
    pub check_interval: String,
    /// Quiet hours start (0-23, local timezone). No proactive messages during quiet hours.
    pub quiet_hours_start: u8,
    /// Quiet hours end (0-23, local timezone).
    pub quiet_hours_end: u8,
    /// Maximum proactive messages per hour (rate limit).
    pub max_messages_per_hour: u32,
    /// Token budget per proactive check cycle.
    pub token_budget_per_check: u32,
    /// Channel to send proactive notifications to.
    pub notify_channel: String,
    /// Chat/group ID to send notifications to.
    pub notify_chat_id: String,
    /// IANA timezone for quiet hours (e.g., "Asia/Taipei").
    pub timezone: String,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            check_interval: "*/30 * * * *".into(),
            quiet_hours_start: 23,
            quiet_hours_end: 8,
            max_messages_per_hour: 3,
            token_budget_per_check: 2000,
            notify_channel: String::new(),
            notify_chat_id: String::new(),
            timezone: "Asia/Taipei".into(),
        }
    }
}

impl ProactiveConfig {
    /// Clamp values to valid ranges after deserialization.
    pub fn sanitize(&mut self) {
        if self.quiet_hours_start > 23 {
            tracing::warn!(
                value = self.quiet_hours_start,
                "quiet_hours_start out of range (0-23), clamping to 23"
            );
            self.quiet_hours_start = 23;
        }
        if self.quiet_hours_end > 23 {
            tracing::warn!(
                value = self.quiet_hours_end,
                "quiet_hours_end out of range (0-23), clamping to 23"
            );
            self.quiet_hours_end = 23;
        }
    }
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
#[derive(Default)]
pub enum MemoryLayer {
    #[default]
    Episodic,
    Semantic,
    Procedural,
}


impl MemoryLayer {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    pub fn parse(s: &str) -> Self {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn agent_role_roundtrip_via_serde_json() {
        for role in [
            AgentRole::Main,
            AgentRole::Specialist,
            AgentRole::Worker,
            AgentRole::Developer,
            AgentRole::Qa,
            AgentRole::Planner,
            AgentRole::TeamLeader,
            AgentRole::ProductManager,
        ] {
            let encoded = serde_json::to_string(&role).unwrap();
            let decoded: AgentRole = serde_json::from_str(&encoded).unwrap();
            assert_eq!(role, decoded, "roundtrip failed for {role:?}");
        }
    }

    #[test]
    fn agent_role_kebab_case_wire_format() {
        assert_eq!(serde_json::to_string(&AgentRole::TeamLeader).unwrap(), "\"team-leader\"");
        assert_eq!(serde_json::to_string(&AgentRole::ProductManager).unwrap(), "\"product-manager\"");
        // Single-word variants stay identical to the old lowercase encoding.
        assert_eq!(serde_json::to_string(&AgentRole::Main).unwrap(), "\"main\"");
        assert_eq!(serde_json::to_string(&AgentRole::Qa).unwrap(), "\"qa\"");
    }

    #[test]
    fn agent_role_serde_aliases_accepted() {
        let cases = [
            ("\"engineer\"",           AgentRole::Developer),
            ("\"quality-assurance\"",  AgentRole::Qa),
            ("\"tl\"",                 AgentRole::TeamLeader),
            ("\"pm\"",                 AgentRole::ProductManager),
        ];
        for (input, expected) in cases {
            let decoded: AgentRole = serde_json::from_str(input)
                .unwrap_or_else(|e| panic!("serde alias failed for {input}: {e}"));
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn agent_role_from_str_lenient_normalisation() {
        let cases = [
            ("team-leader",      AgentRole::TeamLeader),
            ("team_leader",      AgentRole::TeamLeader),
            ("Team Leader",      AgentRole::TeamLeader),
            ("  TEAM-LEADER  ",  AgentRole::TeamLeader),
            ("product_manager",  AgentRole::ProductManager),
            ("engineer",         AgentRole::Developer),
            ("quality",          AgentRole::Qa),
            ("main",             AgentRole::Main),
        ];
        for (input, expected) in cases {
            assert_eq!(
                AgentRole::from_str(input).unwrap(),
                expected,
                "from_str({input:?})"
            );
        }
    }

    #[test]
    fn agent_role_from_str_rejects_garbage() {
        assert!(AgentRole::from_str("xyz").is_err());
        assert!(AgentRole::from_str("").is_err());
    }

    #[test]
    fn agent_role_display_roundtrip() {
        for role in [AgentRole::TeamLeader, AgentRole::ProductManager, AgentRole::Qa] {
            let s = role.to_string();
            assert_eq!(AgentRole::from_str(&s).unwrap(), role);
        }
    }
}
