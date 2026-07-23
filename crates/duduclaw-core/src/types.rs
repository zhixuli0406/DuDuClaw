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
    /// Off-boarded but fully recoverable (WP4): heartbeat/evolution halted and
    /// hidden from the LIVE roster, but no data is deleted — `unarchive` restores.
    Archived,
    /// Soft-deleted (WP4): hidden from every list/route, but the agent directory
    /// and memory are retained on disk. Distinct from `Terminated` (a runtime
    /// end-state) — `Deleted` is an explicit off-board removal.
    Deleted,
}

impl AgentStatus {
    /// Central predicate (WP4 / F2): whether an agent may be *acted on* —
    /// spawned, delegated to, or listed in a team roster. Only `Active` agents
    /// are operational; `Archived` / `Deleted` (and the runtime end-states
    /// `Paused` / `Terminated`) are not. This is the single source of truth so
    /// spawn / delegate / roster-assembly paths cannot drift from each other.
    ///
    /// Fail-closed by construction: any status that is not explicitly `Active`
    /// is non-operational.
    pub fn is_operational(&self) -> bool {
        matches!(self, AgentStatus::Active)
    }

    /// Whether an agent with this status should appear in a listing.
    /// `Deleted` is never listed; `Archived` is listed only when the caller
    /// explicitly asks for archived agents (`include_archived`). Every other
    /// status is listed (so operators still see Paused / Terminated agents).
    pub fn is_listable(&self, include_archived: bool) -> bool {
        match self {
            AgentStatus::Deleted => false,
            AgentStatus::Archived => include_archived,
            _ => true,
        }
    }
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
    /// Lightweight "utility" model for cheap internal tasks (session compression,
    /// key-fact extraction, GVU evolution, summarization, skill synthesis).
    /// Defaults to claude-haiku-4-5. (RFC-25 Phase 0 — replaces scattered literals.)
    #[serde(default = "default_utility_model")]
    pub utility: String,
}

fn default_api_mode() -> String {
    "cli".to_string()
}

/// Single source of truth for the default lightweight "utility" model.
///
/// Both the typed [`ModelConfig::utility`] serde default and the gateway's
/// lightweight `agent.toml` reader (`runtime_config`, which re-exports this)
/// resolve to this one literal (RFC-25 L6 — no duplicated string).
pub const DEFAULT_UTILITY_MODEL: &str = "claude-haiku-4-5";

fn default_utility_model() -> String {
    DEFAULT_UTILITY_MODEL.to_string()
}

/// Single source of truth for the default "preferred" chat model, used when a
/// reply is built with no resolved agent (and therefore no `[model] preferred`).
/// Matches the `agents.create` scaffold default so display and execution agree.
pub const DEFAULT_PREFERRED_MODEL: &str = "claude-sonnet-4-6";

/// Which agent runtime backend executes a prompt (RFC-25 multi-runtime).
///
/// Used as the `RuntimeRegistry` key and parsed from `agent.toml [runtime] provider`.
/// Defaults to [`RuntimeType::Claude`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeType {
    #[default]
    Claude,
    Codex,
    Gemini,
    /// Google Antigravity CLI (`agy`) — the 2026-06-18 successor to the
    /// personal-tier Gemini CLI. Same model lineage, distinct binary/flags.
    Antigravity,
    /// xAI Grok CLI ("Grok Build", beta 2026-05) — terminal coding agent driving
    /// `grok-build-0.1` behind a SuperGrok / X Premium+ subscription. MCP-native,
    /// `-p` headless mode. R4 phase 1 wired CLI detection + headless spawn;
    /// phase 2 (v1.41) added the dashboard one-click SuperGrok device-code
    /// login (`grok login --device-code`, see `cli_auth.rs`).
    Grok,
    #[serde(rename = "openai_compat")]
    OpenAiCompat,
}

impl RuntimeType {
    /// Stable lowercase identifier (matches `agent.toml` values).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Antigravity => "antigravity",
            Self::Grok => "grok",
            Self::OpenAiCompat => "openai_compat",
        }
    }

    /// Parse from a config string; unknown values fall back to
    /// [`RuntimeType::Claude`].
    ///
    /// L18 fix: an unknown provider (typically a typo such as `"claudee"` or
    /// `"openai_compatible"`) used to be silently coerced to `Claude`, masking
    /// the misconfiguration. We now emit a `tracing::warn!` before defaulting so
    /// operators can spot the bad value. The signature is unchanged to keep the
    /// existing `.map(RuntimeType::parse)` callers working.
    pub fn parse(s: &str) -> Self {
        let normalized = s.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "gemini" => Self::Gemini,
            "antigravity" | "agy" => Self::Antigravity,
            "grok" | "grok-cli" => Self::Grok,
            "openai_compat" | "openai" | "openai-compat" => Self::OpenAiCompat,
            other => {
                tracing::warn!(
                    provider = %other,
                    "unknown runtime provider in config; defaulting to Claude"
                );
                Self::Claude
            }
        }
    }
}

/// Product *form factor* of a DuDuClaw deployment, orthogonal to the license
/// tier (which controls which `commercial/` modules unlock) and to the
/// CE/Pro source split.
///
/// `EditionProfile` only changes **defaults and UI presentation** — it never
/// gates a core feature (design rule "畫法 A"). A [`Personal`] deployment hides
/// the multi-seat / audit-management surfaces by default; an [`Enterprise`]
/// deployment shows them. Both run the exact same Apache-2.0 core.
///
/// The `Personal` profile is also the *unit of tenancy* for managed ("代管")
/// personal hosting — a managed personal instance is the same artifact a user
/// could self-host.
///
/// Resolution precedence (see [`EditionProfile::resolve`]):
/// `DUDUCLAW_EDITION` env  >  `agent.toml [edition] profile`  >  license tier
/// >  default ([`Personal`]).
///
/// [`Personal`]: EditionProfile::Personal
/// [`Enterprise`]: EditionProfile::Enterprise
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EditionProfile {
    /// Single-owner, zero-config, personal-assistant defaults. The default,
    /// and the tenancy unit for managed personal hosting.
    #[default]
    Personal,
    /// Multi-seat / compliance / multi-tenant management surfaces enabled.
    Enterprise,
}

impl EditionProfile {
    /// Stable lowercase identifier (matches config + `system.status` JSON).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Enterprise => "enterprise",
        }
    }

    /// `true` for the single-owner personal form factor.
    pub fn is_personal(&self) -> bool {
        matches!(self, Self::Personal)
    }

    /// Parse from a config string. Unknown / empty values **fail closed** to
    /// [`EditionProfile::Personal`] (the least-privileged default — no
    /// enterprise management surfaces) after emitting a `tracing::warn!`.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "personal" | "personal_edition" | "individual" => Self::Personal,
            "enterprise" | "enterprise_edition" => Self::Enterprise,
            "" => Self::Personal,
            other => {
                tracing::warn!(
                    edition = %other,
                    "unknown edition profile in config; defaulting to Personal"
                );
                Self::Personal
            }
        }
    }

    /// Derive the *default* edition implied by a license tier's TOML key.
    ///
    /// Decoupled from `duduclaw-license` (takes the key as `&str`) so
    /// `duduclaw-core` carries no license dependency. The Enterprise set MUST
    /// stay in sync with the tiers whose `dashboard_enterprise = true` in
    /// `crates/duduclaw-license/features.toml`: Business / OEM / Partner /
    /// Self-Host Pro (the self-host line's enterprise plan). Everything else
    /// (open-source, hobby, solo, studio, personal-pro-self-host) implies
    /// [`Personal`].
    ///
    /// Accepts both snake_case TOML keys and kebab-case CLI tier values.
    ///
    /// [`Enterprise`]: EditionProfile::Enterprise
    /// [`Personal`]: EditionProfile::Personal
    pub fn from_tier_key(tier_key: &str) -> Self {
        match tier_key
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .as_str()
        {
            "business" | "enterprise" | "oem" | "partner" | "self_host_pro" => Self::Enterprise,
            _ => Self::Personal,
        }
    }

    /// Resolve the active edition using the documented precedence:
    /// env override > explicit config > license tier > default.
    ///
    /// - `env`: value of `DUDUCLAW_EDITION` (`None` if unset).
    /// - `config`: value of `agent.toml [edition] profile` (`None` if unset).
    /// - `tier_key`: the active license tier's TOML key (`None` for open-source).
    pub fn resolve(env: Option<&str>, config: Option<&str>, tier_key: Option<&str>) -> Self {
        if let Some(e) = env.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::parse(e);
        }
        if let Some(c) = config.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::parse(c);
        }
        if let Some(t) = tier_key.map(str::trim).filter(|s| !s.is_empty()) {
            return Self::from_tier_key(t);
        }
        Self::Personal
    }

    /// Convenience wrapper reading `DUDUCLAW_EDITION` from the process env as
    /// the override layer.
    pub fn resolve_from_env(config: Option<&str>, tier_key: Option<&str>) -> Self {
        let env = std::env::var("DUDUCLAW_EDITION").ok();
        Self::resolve(env.as_deref(), config, tier_key)
    }

    /// Default agent cap for the Personal edition: `0` = **unlimited**.
    ///
    /// Decision 2026-07-16 (B+C): the self-host / open-core promise ("never
    /// limit self-host") wins — the Personal edition ships UNCAPPED by
    /// default. Upgrade desire is driven by the enterprise capability gates
    /// (departments, approvals, multi-account, white-label) plus a soft
    /// dashboard hint above [`PERSONAL_RECOMMENDED_AGENTS`] — not by a hard
    /// block. Managed/hosted deployments that DO want a hard cap set
    /// `DUDUCLAW_PERSONAL_MAX_AGENTS`. The Enterprise edition is never
    /// subject to this — it uses the license tier's `max_agents`
    /// (see `license_runtime`).
    ///
    /// [`PERSONAL_RECOMMENDED_AGENTS`]: Self::PERSONAL_RECOMMENDED_AGENTS
    pub const PERSONAL_MAX_AGENTS_DEFAULT: usize = 0;

    /// Soft threshold above which the dashboard shows a gentle upgrade hint
    /// on the Personal edition. Informational only — nothing is blocked.
    pub const PERSONAL_RECOMMENDED_AGENTS: usize = 3;

    /// The effective Personal-edition agent cap. Reads
    /// `DUDUCLAW_PERSONAL_MAX_AGENTS` (a non-negative integer; `0` =
    /// unlimited) as an operator override, else
    /// [`PERSONAL_MAX_AGENTS_DEFAULT`] (unlimited). Only meaningful when the
    /// active edition is [`Personal`].
    ///
    /// [`Personal`]: EditionProfile::Personal
    pub fn personal_max_agents() -> usize {
        std::env::var("DUDUCLAW_PERSONAL_MAX_AGENTS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(Self::PERSONAL_MAX_AGENTS_DEFAULT)
    }
}

#[cfg(test)]
mod edition_cap_tests {
    use super::EditionProfile;

    #[test]
    fn personal_cap_default_is_unlimited() {
        // B+C decision (2026-07-16): the self-host promise wins — no hard cap
        // by default (`0` = unlimited, the features.toml convention); the
        // dashboard shows a soft hint above the recommended size instead. We
        // assert the constants rather than mutating global env in a parallel
        // test run.
        assert_eq!(EditionProfile::PERSONAL_MAX_AGENTS_DEFAULT, 0);
        assert_eq!(EditionProfile::PERSONAL_RECOMMENDED_AGENTS, 3);
    }

    #[test]
    fn enterprise_editions_are_not_personal() {
        for k in ["business", "enterprise", "oem", "partner", "self_host_pro"] {
            assert!(!EditionProfile::from_tier_key(k).is_personal(), "{k}");
        }
        for k in ["opensource", "hobby", "solo", "studio", "personal_pro_self_host", ""] {
            assert!(EditionProfile::from_tier_key(k).is_personal(), "{k}");
        }
    }
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
    // Default-empty: template agent.toml files (free templates/ and premium
    // packs/kits) ship `[container]` sections without this key, and a missing
    // mount list can only mean "no extra mounts". Before this default, any
    // template-deployed agent whose `[container]` section omitted the key
    // failed the registry's typed parse and was silently skipped on scan.
    #[serde(default)]
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
    /// Command + args to run inside the container (empty = use the image default).
    ///
    /// HC5: the PTC container path sets this to the user-script invocation so the
    /// script actually executes inside the sandbox (instead of the image default).
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Environment variables to inject into the container, as `(key, value)` pairs.
    ///
    /// HC5: used to inject `DUDUCLAW_PTC_SOCKET` so in-container scripts can reach
    /// the host RPC bridge over the bind-mounted UDS socket.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

/// Heartbeat / scheduled-task configuration.
///
/// `cron` expressions are evaluated in the timezone named by `cron_timezone`.
/// When `cron_timezone` is empty (the default), cron falls back to UTC —
/// backward-compatible with pre-v1.8.23 behaviour. Set it to an IANA name
/// (e.g. `"Asia/Taipei"`) to write cron expressions in your local wall clock
/// and let the scheduler do the UTC conversion. Both 5-field (`min hour dom
/// mon dow`) and 6-field (`sec min hour dom mon dow`) forms are accepted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HeartbeatConfig {
    pub enabled: bool,
    pub interval_seconds: u64,
    pub max_concurrent_runs: u32,
    /// Cron expression evaluated in `cron_timezone` (or UTC if that is empty).
    /// Empty string disables cron and falls back to `interval_seconds`.
    /// Example: with `cron_timezone = "Asia/Taipei"`, `"0 9 * * *"` fires
    /// at 09:00 Taipei time daily.
    pub cron: String,
    /// IANA timezone name for interpreting `cron` (e.g. `"Asia/Taipei"`,
    /// `"America/New_York"`). Empty = UTC (legacy behaviour pre-v1.8.23).
    /// Invalid names log a warning at load time and fall back to UTC.
    #[serde(default)]
    pub cron_timezone: String,
}

/// Budget limits and warnings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BudgetConfig {
    pub monthly_limit_cents: u64,
    pub warn_threshold_percent: u8,
    pub hard_stop: bool,
    /// Hard daily spend cap in cents (0 = no daily cap). When exceeded and
    /// [`hard_stop`](Self::hard_stop) is true, the budget circuit breaker blocks
    /// new LLM calls for this agent until the rolling 24h spend falls back under
    /// the cap. Complements the calendar-agnostic `monthly_limit_cents`.
    /// `#[serde(default)]` keeps pre-existing `[budget]` sections valid.
    #[serde(default)]
    pub daily_cap_cents: u64,
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

    /// Computer use execution mode.
    /// - `container` (default): L5a — run inside Docker/Apple Container with Xvfb.
    /// - `native`: L5b — directly control the host desktop (enigo + OS screen capture).
    /// - `auto`: choose based on agent trust level and task requirements.
    #[serde(default)]
    pub computer_use_mode: ComputerUseMode,

    /// Computer use sub-configuration (session limits, app whitelist, etc.).
    #[serde(default)]
    pub computer_use_config: ComputerUseCapConfig,

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

    /// Parameter-level static tool policy (Progent-style tool+arg matcher).
    /// Consumed by the PolicyKernel reference monitor (`duduclaw-security`).
    /// Empty (default) → the kernel abstains and other layers (scope check,
    /// injection scan, `denied_tools`) still apply — backward compatible.
    /// Non-empty → strict allowlist semantics: `forbid` rules win over `allow`,
    /// and a tool call matching no `allow` rule is denied (fail-closed, I5).
    #[serde(default)]
    pub policy: Vec<ToolPolicy>,

    /// Opt-in native OS process sandbox (`duduclaw-sandbox`): when `true`, the
    /// spawned agent CLI subprocess is confined by a native OS primitive
    /// (macOS Seatbelt / Linux Landlock) derived from [`Self::sandbox_level`],
    /// on top of any CLI-flag sandboxing. Default `false`. When enabled and the
    /// primitive cannot confine (unsupported OS / kernel), the spawn is refused
    /// rather than run unconfined (fail-closed, I5).
    #[serde(default)]
    pub native_sandbox: bool,

    /// Opt-in OS-native integration (Phase 1 of the OS-native agent track).
    /// Master switch: when `false` (default), the agent's filesystem watcher is
    /// not started and the `os_notify` / `os_watch_status` / `os_open` MCP tools
    /// are denied at the dispatch gate. Filesystem watching additionally requires
    /// a non-empty `[os_watch] paths` list in the agent's `agent.toml`.
    #[serde(default)]
    pub os_native: bool,
}

/// Effect of a [`ToolPolicy`] rule.
///
/// Precedence when multiple rules match one call (most restrictive wins):
/// `Forbid` > `Ask` > `Allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    /// Permit the call when this rule matches.
    Allow,
    /// Block the call when this rule matches (checked before `Ask`/`Allow`).
    Forbid,
    /// Escalate the call to a human approval (ApprovalBroker) before it runs.
    Ask,
}

/// Comparison operator for an [`ArgCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgOp {
    /// Exact string equality against the stringified argument value.
    Equals,
    /// Substring containment (operator's explicit choice; not a security
    /// allowlist match — use `equals` for identity decisions).
    Contains,
    /// Prefix match against the stringified argument value.
    StartsWith,
}

/// A single argument condition within a [`ToolPolicy`]. All conditions in a
/// rule's `when` list must match (logical AND) for the rule to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ArgCondition {
    /// Top-level key within the tool's `arguments` object.
    pub arg: String,
    /// Comparison operator.
    pub op: ArgOp,
    /// Value to compare the (stringified) argument against.
    pub value: String,
}

/// A parameter-level tool policy rule (Progent-style tool+arg matcher).
///
/// Example `agent.toml`:
/// ```toml
/// [[capabilities.policy]]
/// tool = "shell_exec"
/// effect = "forbid"
/// when = [{ arg = "command", op = "contains", value = "rm -rf" }]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPolicy {
    /// Canonical (`fs_write` / `shell_exec` / `mcp_call`) or runtime tool name
    /// this rule applies to. `"*"` matches any tool.
    pub tool: String,
    /// Whether a match allows or forbids the call.
    pub effect: PolicyEffect,
    /// Argument conditions (logical AND). Empty → matches any arguments.
    #[serde(default)]
    pub when: Vec<ArgCondition>,
}

/// Computer use execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseMode {
    /// L5a: run inside an isolated container with Xvfb virtual display.
    Container,
    /// L5b: directly control the host desktop (requires explicit trust).
    Native,
    /// Auto-select based on agent trust level and task requirements.
    Auto,
}

impl Default for ComputerUseMode {
    fn default() -> Self {
        Self::Container
    }
}

/// Sub-configuration for computer use sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ComputerUseCapConfig {
    /// Allowed applications — empty means all allowed.
    pub allowed_apps: Vec<String>,
    /// Blocked action types (e.g., "delete_file", "terminal").
    pub blocked_actions: Vec<String>,
    /// Maximum session duration in minutes.
    pub max_session_minutes: u32,
    /// Maximum actions per session.
    pub max_actions: u32,
    /// Virtual display width (container mode).
    pub display_width: u32,
    /// Virtual display height (container mode).
    pub display_height: u32,
    /// Automatically confirm trusted operations (in allowed_apps whitelist).
    pub auto_confirm_trusted: bool,
}

impl Default for ComputerUseCapConfig {
    fn default() -> Self {
        Self {
            allowed_apps: Vec::new(),
            blocked_actions: vec![
                "delete_file".to_string(),
                "terminal".to_string(),
                "system_preferences".to_string(),
            ],
            max_session_minutes: 10,
            max_actions: 50,
            display_width: 1280,
            display_height: 800,
            auto_confirm_trusted: false,
        }
    }
}

fn default_wiki_visible_to() -> Vec<String> {
    vec!["*".to_string()]
}

impl Default for CapabilitiesConfig {
    fn default() -> Self {
        Self {
            computer_use: false,
            computer_use_mode: ComputerUseMode::default(),
            computer_use_config: ComputerUseCapConfig::default(),
            browser_via_bash: false,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            wiki_visible_to: default_wiki_visible_to(),
            policy: Vec::new(),
            native_sandbox: false,
            os_native: false,
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

    /// Compute the explicit tool allowlist for Claude CLI (`--allowedTools`).
    ///
    /// HS12 (2026-06 deep review): `allowed_tools` was previously parsed but
    /// never enforced — an operator who set `allowed_tools = ["Read"]` expecting
    /// a read-only sub-agent still got full Write/Edit/Bash. When this returns a
    /// non-empty list, spawn sites MUST pass it as `--allowedTools`, which puts
    /// Claude Code into allowlist mode (only the listed tools are usable). An
    /// empty list means "no explicit allowlist" — spawn sites keep their default
    /// behavior. Returns a deduplicated, sorted Vec for deterministic CLI args.
    pub fn allowed_tools(&self) -> Vec<String> {
        let mut allowed = self.allowed_tools.clone();
        allowed.sort();
        allowed.dedup();
        allowed
    }

    /// Whether this config carries per-tool restrictions (an explicit allowlist
    /// or denylist). Runtimes that can only enforce a coarse sandbox mode
    /// (Codex / Gemini) use this to warn operators that enforcement is
    /// best-effort — the per-tool granularity does not survive the mapping.
    pub fn has_tool_restrictions(&self) -> bool {
        !self.allowed_tools.is_empty() || !self.denied_tools.is_empty()
    }

    /// Whether write-capable tools (Bash / Write / Edit / MultiEdit /
    /// NotebookEdit) are permitted by this config.
    ///
    /// Token-anchored matching only (never substring, per the 2026-06 review
    /// conventions): an entry matches when its base name — the part before an
    /// optional `(` qualifier, e.g. `Bash(git:*)` → `Bash` — equals the tool
    /// name case-insensitively.
    ///
    /// - Allowlist mode (`allowed_tools` non-empty): a write tool must appear
    ///   in the allowlist (bare or qualified) and not be bare-denied.
    /// - Denylist mode: write tools are allowed unless EVERY write tool is
    ///   bare-denied (a qualified deny such as `Bash(rm:*)` does not count as
    ///   fully denying Bash).
    pub fn write_tools_allowed(&self) -> bool {
        const WRITE_TOOLS: [&str; 5] = ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"];
        fn base(entry: &str) -> &str {
            entry.split('(').next().unwrap_or(entry).trim()
        }
        let bare_denied =
            |tool: &str| self.denied_tools.iter().any(|d| d.trim().eq_ignore_ascii_case(tool));
        if !self.allowed_tools.is_empty() {
            WRITE_TOOLS.iter().any(|tool| {
                !bare_denied(tool)
                    && self
                        .allowed_tools
                        .iter()
                        .any(|a| base(a).eq_ignore_ascii_case(tool))
            })
        } else {
            WRITE_TOOLS.iter().any(|tool| !bare_denied(tool))
        }
    }

    /// Map this capability config to the coarse [`SandboxLevel`] used by
    /// runtimes whose CLIs expose a sandbox mode instead of per-tool lists
    /// (Codex `--sandbox`, Gemini `--sandbox`).
    ///
    /// - `computer_use = true` (explicit full-desktop grant) → [`SandboxLevel::FullAccess`]
    /// - no write-capable tools allowed AND no `browser_via_bash` → [`SandboxLevel::ReadOnly`]
    /// - otherwise → [`SandboxLevel::WorkspaceWrite`] (deny-by-default middle ground)
    pub fn sandbox_level(&self) -> SandboxLevel {
        if self.computer_use {
            return SandboxLevel::FullAccess;
        }
        if !self.browser_via_bash && !self.write_tools_allowed() {
            return SandboxLevel::ReadOnly;
        }
        SandboxLevel::WorkspaceWrite
    }
}

/// Coarse sandbox level for CLI runtimes that cannot enforce per-tool
/// allow/deny lists (Codex / Gemini). Derived from [`CapabilitiesConfig`] via
/// [`CapabilitiesConfig::sandbox_level`] / [`sandbox_level_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxLevel {
    /// Model may read but not mutate the workspace.
    ReadOnly,
    /// Model may mutate the workspace only (default).
    WorkspaceWrite,
    /// Full host access — ONLY when capabilities explicitly grant `computer_use`.
    FullAccess,
}

impl SandboxLevel {
    /// Codex CLI `--sandbox` value.
    pub fn as_codex_flag(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "danger-full-access",
        }
    }
}

/// [`SandboxLevel`] for an optional capabilities config. `None` (capability-less
/// legacy callers) keeps the historical workspace-write behaviour — the old
/// `--full-auto` / default approval modes implied workspace-scoped writes.
pub fn sandbox_level_for(caps: Option<&CapabilitiesConfig>) -> SandboxLevel {
    caps.map(CapabilitiesConfig::sandbox_level)
        .unwrap_or(SandboxLevel::WorkspaceWrite)
}

/// Evolution / self-improvement configuration.
///
/// Evolution is driven exclusively by the prediction engine (error-based triggering)
/// and the GVU self-play loop (Generator → Verifier → Updater).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct EvolutionConfig {
    /// Master kill-switch for ALL autonomous evolution paths on this agent.
    ///
    /// When `false`, every self-improvement path is inert regardless of the
    /// individual `*_enabled` toggles below: GVU reflection, heartbeat
    /// silence-breaker, forced reflection, sub-agent prediction, skill
    /// synthesis / graduation / recommendation, and curiosity exploration.
    /// Defaults to `true` so agents predating this field keep their current
    /// behavior (backward compatible). This is the single switch the operator
    /// flips to "freeze" an agent's autonomy; user-authored autopilot rules are
    /// deliberately NOT governed by it (see `docs/guides/evolution-switches.md`).
    #[serde(default = "default_true")]
    pub enabled: bool,
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

    // ── Stagnation detection (P0 config, P1 suppress action) ──

    /// Configuration for the signal-stagnation detector.
    ///
    /// P0: only `log_only` action is active.
    /// P1: `suppress` action will be wired up here without schema changes.
    #[serde(default)]
    pub stagnation_detection: StagnationDetectionConfig,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
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
            // Stagnation detection
            stagnation_detection: StagnationDetectionConfig::default(),
        }
    }
}

impl EvolutionConfig {
    /// True iff the master switch is on AND at least one evolution path is
    /// individually enabled. The master switch (`enabled`) has veto power:
    /// when it is `false` this returns `false` even if every sub-toggle is on.
    pub fn is_any_evolution_enabled(&self) -> bool {
        self.enabled
            && (self.gvu_enabled
                || self.skill_synthesis_enabled
                || self.skill_graduation_enabled
                || self.skill_recommendation_enabled
                || self.curiosity_enabled
                || self.skill_auto_activate
                || self.skill_behavior_monitor_enabled)
    }
}

/// Read `[evolution] enabled` (the master kill-switch) from an agent's
/// `agent.toml`, for callsites that don't hold a parsed [`EvolutionConfig`].
///
/// Unlike [`crate`]'s stricter per-feature reads, this defaults to **`true`**:
/// a missing file, malformed TOML, absent `[evolution]` section, or absent
/// `enabled` key all mean "not explicitly frozen" ⇒ evolution allowed. Only an
/// explicit `enabled = false` freezes the agent. This preserves the behavior of
/// every agent that predates the master switch.
pub fn evolution_master_enabled(agent_dir: &std::path::Path) -> bool {
    let path = agent_dir.join("agent.toml");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return true;
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return true;
    };
    value
        .get("evolution")
        .and_then(|e| e.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
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

// ── Stagnation detection defaults ─────────────────────────────────────────────

fn default_stagnation_window_seconds() -> u64 {
    21600 // 6 hours
}

fn default_stagnation_trigger_threshold() -> u32 {
    3
}

fn default_stagnation_action() -> StagnationAction {
    StagnationAction::LogOnly
}

// ── StagnationDetectionConfig ─────────────────────────────────────────────────

/// Which action to take when stagnation is detected.
///
/// P0 supports only `log_only`.
/// P1 will add `suppress` — the variant is defined here so the config schema
/// is stable and no TOML migration is needed when P1 ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagnationAction {
    /// Record the stagnation event in the audit log but take no further action.
    /// This is the only active mode in P0.
    LogOnly,
    /// Suppress the triggering signal (P1 reserved — not yet wired up).
    ///
    /// ⚠️ Setting this in P0 has no effect; the runtime will treat it as
    /// `log_only` until P1 is merged.
    Suppress,
}

impl Default for StagnationAction {
    fn default() -> Self {
        Self::LogOnly
    }
}

impl std::fmt::Display for StagnationAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogOnly => f.write_str("log_only"),
            Self::Suppress => f.write_str("suppress"),
        }
    }
}

/// Stagnation-detection configuration for the evolution engine.
///
/// Detects when the same type of signal fires too often within a short window,
/// which typically indicates a feedback loop or misconfigured threshold.
///
/// Agnes-approved defaults (Sprint N P0):
/// - `window_seconds = 21600` (6 h)
/// - `trigger_threshold = 3`
/// - `action = log_only`
///
/// ## TOML example
/// ```toml
/// [evolution.stagnation_detection]
/// enabled = true
/// window_seconds = 21600
/// trigger_threshold = 3
/// action = "log_only"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StagnationDetectionConfig {
    /// Master switch. When `false` the detector is fully disabled.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Sliding-window length in seconds (default 21600 = 6 h).
    ///
    /// Valid range: 60 – 604800 (1 min – 7 days).
    #[serde(default = "default_stagnation_window_seconds")]
    pub window_seconds: u64,

    /// Number of signal firings within `window_seconds` that constitutes
    /// stagnation (default 3).
    ///
    /// Valid range: 1 – 1000.
    #[serde(default = "default_stagnation_trigger_threshold")]
    pub trigger_threshold: u32,

    /// What to do when stagnation is detected.
    ///
    /// P0: only `log_only` has effect. `suppress` is accepted by the parser
    /// for forward-compatibility but behaves as `log_only` until P1.
    #[serde(default = "default_stagnation_action")]
    pub action: StagnationAction,
}

impl Default for StagnationDetectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_seconds: default_stagnation_window_seconds(),
            trigger_threshold: default_stagnation_trigger_threshold(),
            action: default_stagnation_action(),
        }
    }
}

impl StagnationDetectionConfig {
    /// Validate field ranges and return a descriptive error if invalid.
    pub fn validate(&self) -> Result<(), String> {
        if self.window_seconds < 60 || self.window_seconds > 604_800 {
            return Err(format!(
                "stagnation_detection.window_seconds must be 60–604800, got {}",
                self.window_seconds
            ));
        }
        if self.trigger_threshold == 0 || self.trigger_threshold > 1000 {
            return Err(format!(
                "stagnation_detection.trigger_threshold must be 1–1000, got {}",
                self.trigger_threshold
            ));
        }
        Ok(())
    }
}

// ── Tests — StagnationDetectionConfig ────────────────────────────────────────

#[cfg(test)]
mod sandbox_level_tests {
    use super::*;

    fn caps(
        computer_use: bool,
        browser_via_bash: bool,
        allowed: &[&str],
        denied: &[&str],
    ) -> CapabilitiesConfig {
        CapabilitiesConfig {
            computer_use,
            browser_via_bash,
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn default_caps_are_workspace_write() {
        assert_eq!(
            CapabilitiesConfig::default().sandbox_level(),
            SandboxLevel::WorkspaceWrite
        );
    }

    #[test]
    fn none_caps_keep_legacy_workspace_write() {
        assert_eq!(sandbox_level_for(None), SandboxLevel::WorkspaceWrite);
    }

    #[test]
    fn computer_use_grant_is_full_access() {
        assert_eq!(
            caps(true, false, &[], &[]).sandbox_level(),
            SandboxLevel::FullAccess
        );
    }

    #[test]
    fn read_only_allowlist_maps_to_read_only() {
        assert_eq!(
            caps(false, false, &["Read", "Grep"], &[]).sandbox_level(),
            SandboxLevel::ReadOnly
        );
    }

    #[test]
    fn write_tools_matching_is_token_anchored_not_substring() {
        // `Bash(git:*)` counts as a (scoped) Bash grant…
        assert!(caps(false, false, &["Bash(git:*)"], &[]).write_tools_allowed());
        // …but a tool merely *containing* a write-tool name must not.
        assert!(!caps(false, false, &["NotWriteTool"], &[]).write_tools_allowed());
        assert!(!caps(false, false, &["Bashful"], &[]).write_tools_allowed());
    }

    #[test]
    fn denylist_mode_requires_all_write_tools_bare_denied() {
        assert!(caps(false, false, &[], &["Bash"]).write_tools_allowed());
        assert!(
            !caps(
                false,
                false,
                &[],
                &["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit"]
            )
            .write_tools_allowed()
        );
        // Qualified deny does not fully deny the base tool.
        assert!(
            caps(
                false,
                false,
                &[],
                &["Bash(rm:*)", "Write", "Edit", "MultiEdit", "NotebookEdit"]
            )
            .write_tools_allowed()
        );
    }

    #[test]
    fn browser_via_bash_prevents_read_only() {
        assert_eq!(
            caps(false, true, &["Read"], &[]).sandbox_level(),
            SandboxLevel::WorkspaceWrite
        );
    }

    #[test]
    fn codex_flag_values() {
        assert_eq!(SandboxLevel::ReadOnly.as_codex_flag(), "read-only");
        assert_eq!(SandboxLevel::WorkspaceWrite.as_codex_flag(), "workspace-write");
        assert_eq!(SandboxLevel::FullAccess.as_codex_flag(), "danger-full-access");
    }
}

#[cfg(test)]
mod stagnation_tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let cfg = StagnationDetectionConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.window_seconds, 21600);
        assert_eq!(cfg.trigger_threshold, 3);
        assert_eq!(cfg.action, StagnationAction::LogOnly);
    }

    #[test]
    fn test_default_validates() {
        assert!(StagnationDetectionConfig::default().validate().is_ok());
    }

    #[test]
    fn test_window_too_small_fails() {
        let mut cfg = StagnationDetectionConfig::default();
        cfg.window_seconds = 30;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("window_seconds"), "got: {err}");
    }

    #[test]
    fn test_window_too_large_fails() {
        let mut cfg = StagnationDetectionConfig::default();
        cfg.window_seconds = 700_000;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_threshold_zero_fails() {
        let mut cfg = StagnationDetectionConfig::default();
        cfg.trigger_threshold = 0;
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("trigger_threshold"), "got: {err}");
    }

    #[test]
    fn test_suppress_action_deserialises() {
        // P1-reserved value must parse without error.
        let toml_str = r#"
            enabled = true
            window_seconds = 21600
            trigger_threshold = 3
            action = "suppress"
        "#;
        let cfg: StagnationDetectionConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.action, StagnationAction::Suppress);
        // Validation passes regardless of action.
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_deserialise_from_toml_with_defaults() {
        // Minimal TOML — all optional fields should fall back to defaults.
        let cfg: StagnationDetectionConfig = toml::from_str("").expect("empty TOML");
        assert_eq!(cfg.window_seconds, 21600);
        assert_eq!(cfg.trigger_threshold, 3);
        assert_eq!(cfg.action, StagnationAction::LogOnly);
    }

    #[test]
    fn test_stagnation_action_display() {
        assert_eq!(StagnationAction::LogOnly.to_string(), "log_only");
        assert_eq!(StagnationAction::Suppress.to_string(), "suppress");
    }

    #[test]
    fn test_evolution_config_has_stagnation_detection() {
        let cfg = EvolutionConfig::default();
        // stagnation_detection field must be present with defaults.
        assert!(cfg.stagnation_detection.enabled);
        assert_eq!(cfg.stagnation_detection.window_seconds, 21600);
    }

    #[test]
    fn test_evolution_config_stagnation_overridable_via_toml() {
        let toml_str = r#"
            skill_auto_activate = false
            skill_security_scan = true
            [stagnation_detection]
            enabled = false
            window_seconds = 3600
            trigger_threshold = 5
            action = "log_only"
        "#;
        let cfg: EvolutionConfig = toml::from_str(toml_str).expect("parse");
        assert!(!cfg.stagnation_detection.enabled);
        assert_eq!(cfg.stagnation_detection.window_seconds, 3600);
        assert_eq!(cfg.stagnation_detection.trigger_threshold, 5);
    }
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
    pub googlechat: Option<GoogleChatChannelConfig>,
    pub teams: Option<TeamsChannelConfig>,
    pub wecom: Option<WeComChannelConfig>,
    pub dingtalk: Option<DingTalkChannelConfig>,
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
    /// RFC-22 Decision 3-D (Phase 3 W3): bind specific Discord
    /// thread/channel/guild IDs to this agent so messages routed there
    /// flow directly to the bound agent (rather than always landing on
    /// the root agent and then being delegated).  Empty list means
    /// "no binding — fall back to default routing" (backwards-compat).
    pub bindings: Vec<ChannelBinding>,
}

/// RFC-22 Decision 3-D: a single channel/thread/guild → agent binding.
///
/// Stored under `[[channels.discord.bindings]]` (or future telegram/line)
/// in `agent.toml`.  At resolution time the agent registry walks all agents
/// and matches the incoming Discord `session_id` shape:
///
/// - `discord:thread:<thread_id>`   matches `kind = "thread"`, `id = thread_id`
/// - `discord:<channel_id>`         matches `kind = "channel"`, `id = channel_id`
/// - matching guild requires looking up parent guild from Discord context
///   (not yet implemented in this pass — `kind = "guild"` is reserved)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChannelBinding {
    /// One of `"thread"`, `"channel"`, `"guild"`.  Unknown kinds are
    /// treated as no-match (fail-closed).
    pub kind: String,
    /// The Discord snowflake ID for the bound entity.
    pub id: String,
    /// Operator-facing description (purely informational; surfaces in
    /// dashboard / CLI listings).
    #[serde(default)]
    pub description: String,
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
///
/// Backward compatible with the single-OA layout: the top-level
/// `channel_token`/`channel_secret` fields still work. WP7 adds
/// `[[channels.line.accounts]]` so one gateway can host several LINE Official
/// Accounts (DuduCloud B2C), each bound to an agent with its own credit rate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct LineChannelConfig {
    pub channel_token: String,
    pub channel_token_enc: Option<String>,
    pub channel_secret: String,
    pub channel_secret_enc: Option<String>,
    /// WP7 multi-OA. When non-empty, each entry is an independent Official
    /// Account. When empty, the top-level single-OA fields are used (legacy).
    #[serde(default)]
    pub accounts: Vec<LineAccount>,
}

/// One LINE Official Account in a multi-OA deployment (WP7).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "snake_case")]
pub struct LineAccount {
    /// Operator-facing label; also the credit-account namespace key.
    pub name: String,
    pub channel_token: String,
    pub channel_token_enc: Option<String>,
    pub channel_secret: String,
    pub channel_secret_enc: Option<String>,
    /// Agent this OA's conversations route to.
    pub agent_id: String,
    /// Points charged per 1K output tokens for credit metering. 0 ⇒ metering off.
    #[serde(default)]
    pub credit_rate: f64,
}

impl LineChannelConfig {
    /// Resolve the effective account list. When `accounts` is empty, synthesize
    /// a single `"default"` account from the legacy top-level fields so old
    /// configs behave byte-identically.
    pub fn resolve_accounts(&self) -> Vec<LineAccount> {
        if !self.accounts.is_empty() {
            return self.accounts.clone();
        }
        vec![LineAccount {
            name: "default".to_string(),
            channel_token: self.channel_token.clone(),
            channel_token_enc: self.channel_token_enc.clone(),
            channel_secret: self.channel_secret.clone(),
            channel_secret_enc: self.channel_secret_enc.clone(),
            agent_id: String::new(),
            credit_rate: 0.0,
        }]
    }
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


/// Per-agent WeCom (企業微信) channel settings — self-built app (自建應用).
///
/// Inbound callbacks hit the global `POST /webhook/wecom` endpoint; the
/// callback Token + EncodingAESKey authenticate/decrypt them (fail-closed).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct WeComChannelConfig {
    /// Enterprise ID (corpid) — also the receiveid the crypto envelope must carry.
    pub corp_id: String,
    /// Self-built app secret (corpsecret) for gettoken.
    pub corp_secret: String,
    pub corp_secret_enc: Option<String>,
    /// Self-built app AgentId.
    pub agent_id: String,
    /// Callback verification Token (msg_signature key).
    pub callback_token: String,
    pub callback_token_enc: Option<String>,
    /// 43-char EncodingAESKey for the AES-256-CBC callback envelope.
    pub encoding_aes_key: String,
    pub encoding_aes_key_enc: Option<String>,
}


/// Per-agent DingTalk (釘釘) channel settings — enterprise internal robot.
///
/// Inbound callbacks hit the global `POST /webhook/dingtalk` endpoint,
/// verified via the HMAC-SHA256 `sign` header keyed by `app_secret`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct DingTalkChannelConfig {
    /// Robot AppKey / Client ID.
    pub app_key: String,
    /// Robot AppSecret — the callback signature key.
    pub app_secret: String,
    pub app_secret_enc: Option<String>,
}


/// Per-agent Google Chat channel settings.
///
/// The Chat app is configured in the Google Cloud console with an HTTP
/// endpoint URL pointing at `POST /webhook/googlechat`. Inbound requests
/// carry a JWT issued by `chat@system.gserviceaccount.com` whose audience
/// is the Cloud **project number**. Outbound (async) sends authenticate
/// with a service-account key (scope `chat.bot`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct GoogleChatChannelConfig {
    /// Google Cloud project number (JWT audience for inbound verification).
    pub project_number: String,
    /// Service-account JSON key (full JSON content, encrypted at rest).
    pub service_account_json: String,
    pub service_account_json_enc: Option<String>,
}


/// Per-agent Microsoft Teams channel settings.
///
/// Requires an Azure Bot resource whose messaging endpoint points at
/// `POST /webhook/teams`. Single-tenant registrations (the current Azure
/// default) must set `tenant_id`; multi-tenant bots may leave it empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
#[derive(Default)]
pub struct TeamsChannelConfig {
    /// Microsoft App ID (Entra application / bot ID).
    pub app_id: String,
    /// Client secret for the app registration.
    pub app_password: String,
    pub app_password_enc: Option<String>,
    /// Entra tenant ID (required for single-tenant bots; empty = multi-tenant).
    pub tenant_id: String,
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
    /// WP7 — department this agent belongs to (company → department → personal
    /// knowledge/skill layering). Empty/absent = no department: the agent sees
    /// no `departments/*` shared-wiki page or department skill, exactly as
    /// before WP7 (backward compatible). Validated against
    /// [`crate::department::is_valid_department`] wherever it selects a path.
    #[serde(default)]
    pub department: String,
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
    /// Legacy memory configuration (retained for agent.toml backwards compatibility).
    /// The MemGPT 3-layer memory system was removed — these fields are no longer consumed.
    /// Session continuity is now handled by native multi-turn session management.
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Night Engine (N1–N4 idle-time compute suite). Disabled by default;
    /// opt in per agent via `[night_engine] enabled = true`.
    #[serde(default)]
    pub night_engine: NightEngineConfig,
    /// System prompt assembly mode (#11 Active Retrieval, 2026-05-12).
    /// Default `Full` preserves v1.12.x behaviour; opt-in `Minimal` switches
    /// to Anthropic Skills-style "index + MCP on demand" — wiki/skill
    /// content is fetched at tool-call time instead of injected upfront.
    /// See `commercial/docs/TODO-runtime-health-fixes-202605.md #11`.
    #[serde(default)]
    pub prompt: PromptConfig,
}

/// How the system prompt is assembled.
///
/// `Full` (default) — v1.12.x behaviour: inject SOUL/IDENTITY/CONTRACT,
/// pre-load wiki L0+L1, all skills, team roster, pinned tasks. Caches well
/// when nothing changes but the prefix gets large for knowledge-rich agents.
///
/// `Minimal` — Anthropic Skills-style: only stable core (SOUL/IDENTITY/
/// CONTRACT) + a short MCP tool index. Agents fetch wiki / skill bodies
/// on demand via `wiki_search` / `wiki_read` / `skill_lookup`. Designed
/// for agents that hit the 200 K cliff because conversation history +
/// inlined wiki together overflow the cache window.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    #[default]
    Full,
    Minimal,
}

/// System prompt assembly knobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct PromptConfig {
    /// Assembly mode — see [`PromptMode`].
    pub mode: PromptMode,
    /// Soft budget for the SOUL core slice in Minimal mode, in kilobytes.
    /// The trimmer keeps the first `minimal_core_kb` × 1024 bytes of SOUL.md
    /// (no smarter slicing yet — first N bytes is usually persona +
    /// principles, which is what we want). Default 5 KB ≈ 1.5 K tokens.
    pub minimal_core_kb: u32,
    /// **#15 (2026-05-12)** — opt in to Claude CLI's `--bare` mode for
    /// the agent's subprocess invocations.
    ///
    /// When `true`:
    /// - Cron / dispatcher Claude CLI calls add `--bare --system-prompt
    ///   <gateway-built>` to the spawn args, preventing CLAUDE.md
    ///   auto-discovery from leaking into the prompt.
    /// - Auth switches from OAuth/keychain to `ANTHROPIC_API_KEY` env;
    ///   the AccountRotator must surface an API key for this agent, or
    ///   the spawn fails fast with an actionable error.
    ///
    /// The default is `false` because `--bare` is a behavioural shift
    /// (loses OAuth, skips hooks). Operators opt in per-agent after
    /// verifying their AccountRotator has an API key fallback.
    ///
    /// See [#15 in commercial/docs/TODO-runtime-health-fixes-202605.md].
    pub cli_bare_mode: bool,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            mode: PromptMode::Full,
            minimal_core_kb: 5,
            cli_bare_mode: false,
        }
    }
}

/// Legacy memory configuration — no longer consumed at runtime.
///
/// Retained so existing `agent.toml` files with a `[memory]` section
/// can still be deserialized without errors. All fields are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct MemoryConfig {
    pub enabled: bool,
    pub core_tokens: u32,
    pub recall_tokens: u32,
    pub archival_tokens: u32,
    pub recall_auto_inject: u32,
    pub archival_auto_retrieve: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            core_tokens: 0,
            recall_tokens: 0,
            archival_tokens: 0,
            recall_auto_inject: 0,
            archival_auto_retrieve: 0,
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
/// check_interval = "*/30 * * * *"   # cron: every 30 min (UTC — see [heartbeat] cron note)
/// quiet_hours_start = 23
/// quiet_hours_end = 8
/// max_messages_per_hour = 3
/// token_budget_per_check = 2000
/// notify_channel = "telegram"
/// notify_chat_id = "123456789"
/// timezone = "Asia/Taipei"          # affects quiet_hours only
/// max_turns = 8                     # Claude CLI --max-turns for proactive runs
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
    /// NOTE: only affects `quiet_hours_*` evaluation. The `check_interval`
    /// cron expression is always evaluated in UTC (same as `[heartbeat] cron`).
    pub timezone: String,
    /// Claude CLI `--max-turns` for a proactive check. Needs enough headroom
    /// for MCP tool calls (e.g. querying Notion, Gmail) and summarisation.
    /// Default 8; bump higher for checks that chain many tool calls.
    pub max_turns: u32,
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
            max_turns: 8,
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
        if self.max_turns == 0 {
            tracing::warn!(
                "proactive.max_turns is 0, bumping to default (8)"
            );
            self.max_turns = 8;
        }
        if self.max_turns > 64 {
            tracing::warn!(
                value = self.max_turns,
                "proactive.max_turns unusually high, clamping to 64"
            );
            self.max_turns = 64;
        }
    }
}

/// Night Engine configuration (N1–N4 idle-time compute suite).
///
/// The Night Engine layers four paper-grounded idle-time capabilities on top of
/// the existing heartbeat scheduler + evolution engine — "the AI employee tidies
/// its memory and pre-reads tomorrow's work while it sleeps":
///
/// - **N1 Sleep-time compute** (arXiv:2504.13171) — pre-reason over active
///   context during idle windows; results land in a per-agent night cache.
/// - **N2 Proactive prefetch** (ProAct, arXiv:2605.25971) — predict the user's
///   next need from history + memory and gather evidence ahead of time.
/// - **N3 Schema induction** (DCPM, arXiv:2606.09483) — a nightly System-2 pass
///   that induces recurring schemas from episodic memory (deterministic).
/// - **N4 Recurrence-gated consolidation + trust verification** (RecMem
///   arXiv:2605.16045 + TRUSTMEM arXiv:2606.25161) — only semantically recurring
///   knowledge triggers consolidation; the result passes a deterministic
///   coverage/preservation/faithfulness gate before it is written (rollback on
///   failure).
///
/// Disabled by default — opt in per agent via `agent.toml [night_engine]`, or
/// set a global default in `config.toml [night_engine]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct NightEngineConfig {
    /// Master switch. Default `false` — the whole suite is inert unless opted in.
    pub enabled: bool,
    /// Minutes of no user interaction before an agent is considered idle and a
    /// night pass may run. Default 90.
    pub idle_threshold_minutes: u64,
    /// Hard budget cap per night pass, in cents. Once a pass' estimated spend
    /// reaches this, remaining LLM-backed sub-passes (N1/N2) are skipped.
    /// Deterministic passes (N3/N4) never spend and run regardless. Default 20.
    pub max_pass_cost_cents: u64,
    /// Circuit breaker: maximum night passes per agent per rolling 24h. Guards
    /// against runaway idle loops. Default 8.
    pub max_passes_per_day: u32,
    /// N1 Sleep-time compute sub-pass toggle. Default `true` (still gated by
    /// `enabled`). Requires an LLM path.
    pub sleep_time: bool,
    /// N2 Proactive prefetch sub-pass toggle. Default `true`. Requires an LLM path.
    pub prefetch: bool,
    /// N3 Schema induction sub-pass toggle. Default `true`. Deterministic, no LLM.
    pub schema_induction: bool,
    /// N4 Recurrence-gated consolidation sub-pass toggle. Default `true`.
    /// Deterministic verification, no LLM required.
    pub recurrence_consolidation: bool,
    /// N3: minimum number of episodic occurrences a pattern needs before it is
    /// promoted to a schema entry. Default 3.
    pub schema_min_support: u32,
    /// N4: minimum semantic recurrence count before consolidation is triggered
    /// (the RecMem recurrence gate). Default 3.
    pub recurrence_threshold: u32,
    /// N1/N2: how many recent memories / turns to consider as context per pass.
    /// Default 40.
    pub context_window: u32,
}

impl Default for NightEngineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_threshold_minutes: 90,
            max_pass_cost_cents: 20,
            max_passes_per_day: 8,
            sleep_time: true,
            prefetch: true,
            schema_induction: true,
            recurrence_consolidation: true,
            schema_min_support: 3,
            recurrence_threshold: 3,
            context_window: 40,
        }
    }
}

impl NightEngineConfig {
    /// Clamp values to sane ranges after deserialization.
    pub fn sanitize(&mut self) {
        if self.idle_threshold_minutes == 0 {
            self.idle_threshold_minutes = 90;
        }
        if self.max_passes_per_day == 0 {
            self.max_passes_per_day = 1;
        }
        self.schema_min_support = self.schema_min_support.max(2);
        self.recurrence_threshold = self.recurrence_threshold.max(2);
        self.context_window = self.context_window.clamp(5, 500);
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

/// Result of waiting for a container to exit (HC5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ContainerExit {
    /// Process exit code reported by the container.
    pub exit_code: i64,
    /// Combined stdout/stderr captured from the container logs.
    pub logs: String,
}

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

    // ── R4: Grok runtime type ──────────────────────────────────────

    #[test]
    fn runtime_type_grok_parse_and_display_roundtrip() {
        // Config-string parse (case/alias-insensitive).
        assert_eq!(RuntimeType::parse("grok"), RuntimeType::Grok);
        assert_eq!(RuntimeType::parse("GROK"), RuntimeType::Grok);
        assert_eq!(RuntimeType::parse("grok-cli"), RuntimeType::Grok);
        // Stable identifier.
        assert_eq!(RuntimeType::Grok.as_str(), "grok");
        // as_str ↔ parse round-trip for every variant.
        for rt in [
            RuntimeType::Claude,
            RuntimeType::Codex,
            RuntimeType::Gemini,
            RuntimeType::Antigravity,
            RuntimeType::Grok,
            RuntimeType::OpenAiCompat,
        ] {
            assert_eq!(RuntimeType::parse(rt.as_str()), rt, "round-trip {rt:?}");
        }
    }

    #[test]
    fn runtime_type_grok_serde_roundtrip() {
        let json = serde_json::to_string(&RuntimeType::Grok).unwrap();
        assert_eq!(json, r#""grok""#);
        let back: RuntimeType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RuntimeType::Grok);
    }

    // ── WP1 evolution master kill-switch ───────────────────────────

    #[test]
    fn evolution_config_default_has_master_enabled() {
        // Backward compat: an agent predating the master switch defaults to on.
        assert!(EvolutionConfig::default().enabled);
    }

    #[test]
    fn is_any_evolution_enabled_master_off_vetoes_gvu_on() {
        // T1.5 ③: master off wins even if gvu_enabled is true.
        let mut cfg = EvolutionConfig::default();
        cfg.enabled = false;
        cfg.gvu_enabled = true;
        assert!(!cfg.is_any_evolution_enabled());
    }

    #[test]
    fn is_any_evolution_enabled_master_on_requires_a_subtoggle() {
        let mut cfg = EvolutionConfig::default();
        cfg.enabled = true;
        cfg.gvu_enabled = false;
        cfg.skill_synthesis_enabled = false;
        cfg.skill_graduation_enabled = false;
        cfg.skill_recommendation_enabled = false;
        cfg.curiosity_enabled = false;
        cfg.skill_auto_activate = false;
        cfg.skill_behavior_monitor_enabled = false;
        assert!(!cfg.is_any_evolution_enabled());
        cfg.gvu_enabled = true;
        assert!(cfg.is_any_evolution_enabled());
    }

    #[test]
    fn evolution_config_missing_section_defaults_master_on() {
        // T1.5 ④: absent [evolution] parses byte-identical to defaults ⇒ on.
        let cfg: EvolutionConfig = toml::from_str("").unwrap_or_default();
        assert!(cfg.enabled);
    }

    #[test]
    fn evolution_master_enabled_reads_explicit_false() {
        let tmp = std::env::temp_dir()
            .join(format!("evo-master-off-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("agent.toml"), "[evolution]\nenabled = false\n").unwrap();
        assert!(!evolution_master_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn evolution_master_enabled_defaults_true_when_absent_or_missing() {
        // Missing file → true (not frozen). Present file without the key → true.
        let tmp = std::env::temp_dir()
            .join(format!("evo-master-default-{}", uuid::Uuid::new_v4()));
        assert!(evolution_master_enabled(&tmp)); // no dir/file yet
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("agent.toml"), "[agent]\nname = \"x\"\n").unwrap();
        assert!(evolution_master_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn capabilities_without_policy_parses_backward_compat() {
        // A pre-P1-3 agent.toml [capabilities] block with no `policy` key must
        // still parse, with `policy` defaulting to empty.
        let toml_src = r#"
            computer_use = false
            browser_via_bash = true
            allowed_tools = ["Read", "Grep"]
        "#;
        let caps: CapabilitiesConfig = toml::from_str(toml_src).unwrap();
        assert!(caps.policy.is_empty());
        assert!(caps.browser_via_bash);
    }

    #[test]
    fn capabilities_policy_parses_to_tool_policy_vec() {
        let toml_src = r#"
            [[policy]]
            tool = "shell_exec"
            effect = "forbid"
            when = [{ arg = "command", op = "contains", value = "rm -rf" }]

            [[policy]]
            tool = "mcp_call"
            effect = "ask"

            [[policy]]
            tool = "*"
            effect = "allow"
        "#;
        let caps: CapabilitiesConfig = toml::from_str(toml_src).unwrap();
        assert_eq!(caps.policy.len(), 3);
        assert_eq!(caps.policy[0].tool, "shell_exec");
        assert_eq!(caps.policy[0].effect, PolicyEffect::Forbid);
        assert_eq!(caps.policy[0].when.len(), 1);
        assert_eq!(caps.policy[0].when[0].op, ArgOp::Contains);
        assert_eq!(caps.policy[1].effect, PolicyEffect::Ask);
        assert!(caps.policy[1].when.is_empty());
        assert_eq!(caps.policy[2].effect, PolicyEffect::Allow);
    }

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

    // ── EditionProfile ────────────────────────────────────────────────

    #[test]
    fn edition_profile_default_is_personal() {
        assert_eq!(EditionProfile::default(), EditionProfile::Personal);
        assert!(EditionProfile::default().is_personal());
    }

    #[test]
    fn edition_profile_as_str_roundtrip() {
        for ed in [EditionProfile::Personal, EditionProfile::Enterprise] {
            assert_eq!(EditionProfile::parse(ed.as_str()), ed);
        }
        assert_eq!(EditionProfile::Personal.as_str(), "personal");
        assert_eq!(EditionProfile::Enterprise.as_str(), "enterprise");
    }

    #[test]
    fn edition_profile_parse_aliases_and_case() {
        assert_eq!(EditionProfile::parse("PERSONAL"), EditionProfile::Personal);
        assert_eq!(EditionProfile::parse("  Enterprise  "), EditionProfile::Enterprise);
        assert_eq!(EditionProfile::parse("individual"), EditionProfile::Personal);
        assert_eq!(EditionProfile::parse("enterprise_edition"), EditionProfile::Enterprise);
    }

    #[test]
    fn edition_profile_unknown_and_empty_fail_closed_to_personal() {
        assert_eq!(EditionProfile::parse("megacorp"), EditionProfile::Personal);
        assert_eq!(EditionProfile::parse(""), EditionProfile::Personal);
        assert_eq!(EditionProfile::parse("   "), EditionProfile::Personal);
    }

    #[test]
    fn edition_profile_from_tier_key() {
        for k in [
            "business",
            "enterprise",
            "oem",
            "OEM",
            " Business ",
            // Self-host line enterprise tiers (dashboard_enterprise = true in
            // features.toml): both TOML snake_case and CLI kebab-case forms.
            "self_host_pro",
            "self-host-pro",
            "partner",
        ] {
            assert_eq!(EditionProfile::from_tier_key(k), EditionProfile::Enterprise, "{k}");
        }
        for k in [
            "opensource",
            "hobby",
            "solo",
            "studio",
            "personal_pro_self_host",
            "personal-pro-self-host",
            "",
        ] {
            assert_eq!(EditionProfile::from_tier_key(k), EditionProfile::Personal, "{k}");
        }
    }

    #[test]
    fn edition_profile_resolve_precedence() {
        // env wins over everything
        assert_eq!(
            EditionProfile::resolve(Some("enterprise"), Some("personal"), Some("solo")),
            EditionProfile::Enterprise
        );
        // config wins over tier when env absent
        assert_eq!(
            EditionProfile::resolve(None, Some("enterprise"), Some("solo")),
            EditionProfile::Enterprise
        );
        // tier used when env + config absent
        assert_eq!(
            EditionProfile::resolve(None, None, Some("business")),
            EditionProfile::Enterprise
        );
        assert_eq!(
            EditionProfile::resolve(None, None, Some("studio")),
            EditionProfile::Personal
        );
        // nothing set → default Personal
        assert_eq!(EditionProfile::resolve(None, None, None), EditionProfile::Personal);
        // empty strings are treated as unset and fall through
        assert_eq!(
            EditionProfile::resolve(Some("  "), Some(""), Some("business")),
            EditionProfile::Enterprise
        );
    }

    #[test]
    fn edition_profile_serde_roundtrip() {
        for ed in [EditionProfile::Personal, EditionProfile::Enterprise] {
            let json = serde_json::to_string(&ed).unwrap();
            let back: EditionProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ed);
        }
        // lowercase wire format
        assert_eq!(serde_json::to_string(&EditionProfile::Personal).unwrap(), "\"personal\"");
    }
}
