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
    /// work. Typically has `reports_to = ""` or a parent org. `front_desk` is
    /// the expert-pack roster name for the same thing (the team's 對外窗口 that
    /// workers report to) — accepted here so installed packs load.
    #[serde(
        alias = "tl",
        alias = "lead",
        alias = "teamlead",
        alias = "front_desk",
        alias = "front-desk"
    )]
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
            "team-leader" | "teamleader" | "tl" | "lead" | "front-desk" | "frontdesk" => {
                Self::TeamLeader
            }
            "product-manager" | "productmanager" | "pm" => Self::ProductManager,
            _ => {
                return Err(format!(
                    "invalid role '{s}'. valid values: {}",
                    Self::valid_values_help()
                ));
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

    // ── Formerly-untyped `[model]` keys (R2 unification) ────────────────
    //
    // Read by `gateway::runtime_config`'s raw-TOML accessors before this
    // migration. `skip_serializing_if` keeps the on-disk shape unchanged for
    // configs that never wrote them.
    /// Cross-provider Direct-API fallback chain (W3/G1), tried in order after
    /// `preferred`. Missing / non-array ⇒ empty ⇒ "no chain" (single-shot).
    /// Blank entries are dropped by the accessor, not here.
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,

    /// Optional mid tier for confidence-aware delegation routing (O1).
    /// Missing / blank ⇒ `None` ⇒ the Standard tier resolves to `preferred`.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard: Option<String>,

    /// Per-agent override of `config.toml [delegation] confidence_routing`
    /// (O1). Deliberately `Option`: the agent wins over the global flag **in
    /// both directions**, so "unset" and "explicitly false" are different
    /// states and must not be collapsed.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegation_routing: Option<bool>,
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

// ═══════════════════════════════════════════════════════════════════════
// `[team]` — Team-as-Agent role composition (P1/WP-1)
// ═══════════════════════════════════════════════════════════════════════
//
// Design: `commercial/docs/DESIGN-team-as-agent-2026-09.md` §1, §3.1, §3.11.
//
// An **employee** stays the only user-visible unit (agent directory, SOUL.md,
// channels, memory, playbook — unchanged). A **team** is a role grouping
// *inside* one employee: `planner` / `executor` / `verifier` / `utility`,
// each bound to its own `{runtime, model, effort}`.
//
// Three properties of this schema are load-bearing and are enforced by
// [`validate_team`], not by convention:
//
// 1. **`(role, runtime, model)` is a triple, never a bare model name.**
//    Goose's Lead/Worker shipped role configs that stored only the model
//    name; a `qwen-*` lead was then executed by the Claude backend
//    (goose#10731). Here a model whose family does not belong to its
//    declared runtime is a hard [`TeamConfigError::ModelRuntimeMismatch`] —
//    the platform never guesses a provider for a model id it cannot place.
// 2. **The verifier's model family must differ from the executor's**
//    (decision C, arXiv:2607.13918 — decorrelation is the lever, adding
//    another same-family judge is not). Same family ⇒ refuse to form the
//    team, do not warn-and-continue.
// 3. **Default Solo — decided by the gate, not by the switch.** Since v1.66
//    `enabled` defaults to `true` and the gate ([`crate::team_gate`])
//    defaults to `auto`; a team is formed per task, never per boot. The
//    ordinary task still runs Solo because the gate needs three of its four
//    signals, and an unconfigured `[team.roles]` still runs Solo because
//    [`cascade_unbound_roles`] gives executor and verifier the same employee
//    model, which the decorrelation rule refuses.
//
// Cascade: a role field missing in `agent.toml [team]` falls back to the same
// field of `config.toml [team]` ([`TeamConfig::merge`]), and a role that
// resolves to nothing at all falls back — in the *caller* — to the employee's
// `[model] preferred`. That last hop is deliberately outside this module: it
// needs the employee's `AgentConfig`, which this validation layer does not
// take.

/// One of the four fixed roles inside an employee's team.
///
/// Distinct from [`AgentRole`], which is an *employee's* org role
/// (`main` / `specialist` / `worker` / …). The two never mix: an employee has
/// exactly one [`AgentRole`] and, when a team is formed for a task, up to four
/// [`Role`] slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Decomposition and planning.
    Planner,
    /// Does the work. May be fanned out (see [`TeamConfig::executor_fanout`]).
    Executor,
    /// Independent acceptance. Must not share the executor's model family.
    Verifier,
    /// Summarisation / classification / chores. Does not occupy a spawn slot.
    Utility,
}

impl Role {
    /// Every role, in pipeline order. Iteration order of validation and of
    /// the `[team.roles.*]` table is this order, so error reporting is
    /// deterministic.
    pub const ALL: &'static [Role] =
        &[Role::Planner, Role::Executor, Role::Verifier, Role::Utility];

    /// Canonical lowercase identifier — the `[team.roles.<id>]` key, the
    /// `role_turns.jsonl` field value, and the inverse of
    /// [`std::str::FromStr::from_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Executor => "executor",
            Role::Verifier => "verifier",
            Role::Utility => "utility",
        }
    }

    /// Comma-separated list of every valid role id, for error messages.
    pub fn valid_values_help() -> &'static str {
        "planner, executor, verifier, utility"
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    /// Exact (case-insensitive, trimmed) match only. Unlike [`AgentRole`],
    /// this enum has **no aliases**: the four ids are new surface with no
    /// legacy spellings to accept, and a silently-accepted near-miss here
    /// would route a turn to the wrong role.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "planner" => Ok(Role::Planner),
            "executor" => Ok(Role::Executor),
            "verifier" => Ok(Role::Verifier),
            "utility" => Ok(Role::Utility),
            other => Err(format!(
                "unknown team role `{other}` (expected one of: {})",
                Role::valid_values_help()
            )),
        }
    }
}

// `Effort` is NOT defined here. The per-call reasoning-effort enum, its
// per-runtime ceiling/clamp and its verified CLI flag mapping live in
// [`crate::effort`] (P1/WP-3) — one crate must not carry two spellings of the
// same knob, and that module is the one that knows which runtimes accept
// which levels. This layer keeps `[team.roles.*] effort` as a raw string
// (see [`RoleSpec::effort`]) and parses it through
// [`crate::effort::Effort`] in [`validate_team`].
//
// Note on Gemini: its CLI has no effort flag at all. That is deliberately
// NOT a config error here — refusing the key would make the same `[team]`
// non-portable across runtimes for no safety gain. The spawn layer
// (WP-3, via `Effort::is_supported_by`) is where it becomes a no-op.

/// How the decomposability gate ([`crate::team_gate`]) behaves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamGateMode {
    /// Run the L0/L1 rules (design §3.2). The only mode intended for
    /// production.
    #[default]
    Auto,
    /// Never form a team. The kill switch.
    AlwaysSolo,
    /// Always form a team — **testing only**: it bypasses every hard
    /// exclusion, including the irreversible-tool one.
    AlwaysTeam,
}

impl TeamGateMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TeamGateMode::Auto => "auto",
            TeamGateMode::AlwaysSolo => "always_solo",
            TeamGateMode::AlwaysTeam => "always_team",
        }
    }

    pub fn valid_values_help() -> &'static str {
        "auto, always_solo, always_team"
    }
}

impl std::fmt::Display for TeamGateMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TeamGateMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(TeamGateMode::Auto),
            "always_solo" | "always-solo" => Ok(TeamGateMode::AlwaysSolo),
            "always_team" | "always-team" => Ok(TeamGateMode::AlwaysTeam),
            other => Err(format!(
                "unknown team gate mode `{other}` (expected one of: {})",
                TeamGateMode::valid_values_help()
            )),
        }
    }
}

/// One `[team.roles.<role>]` table, exactly as written.
///
/// Every field is `Option` so the global → per-employee cascade is
/// **field-wise**: an employee that overrides only `effort` keeps the global
/// `runtime` / `model`. Collapsing an absent key into a default here would
/// make "unset" indistinguishable from "explicitly the default", and the
/// merge would then overwrite the global value with a phantom one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct RoleSpec {
    /// Runtime id (or catalog alias, e.g. `agy`). Raw `String` — the
    /// allowlist and canonicalisation happen in [`validate_team`], which can
    /// report *which role* was wrong; a strict serde enum here would fail the
    /// whole `agent.toml` and take the employee out of the registry.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Model id within that runtime's family.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort. Raw `String` for the same reason as `runtime`;
    /// parsed into [`Effort`] by [`validate_team`].
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl RoleSpec {
    /// True when the role declares nothing at all — the caller then cascades
    /// to the employee's `[model] preferred`.
    pub fn is_empty(&self) -> bool {
        self.runtime.is_none() && self.model.is_none() && self.effort.is_none()
    }

    /// Field-wise override: every `Some` in `over` wins, every `None` keeps
    /// `self`.
    pub fn merged_with(&self, over: &RoleSpec) -> RoleSpec {
        RoleSpec {
            runtime: over.runtime.clone().or_else(|| self.runtime.clone()),
            model: over.model.clone().or_else(|| self.model.clone()),
            effort: over.effort.clone().or_else(|| self.effort.clone()),
        }
    }
}

/// The `[team.roles]` table — the four fixed slots.
///
/// A struct, not a map: the role set is closed, so an unknown
/// `[team.roles.reviewer]` table is ignored exactly like any other unknown
/// key rather than silently creating a fifth role nothing dispatches to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TeamRoles {
    #[serde(deserialize_with = "crate::lenient::or_default")]
    #[serde(skip_serializing_if = "RoleSpec::is_empty")]
    pub planner: RoleSpec,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    #[serde(skip_serializing_if = "RoleSpec::is_empty")]
    pub executor: RoleSpec,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    #[serde(skip_serializing_if = "RoleSpec::is_empty")]
    pub verifier: RoleSpec,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    #[serde(skip_serializing_if = "RoleSpec::is_empty")]
    pub utility: RoleSpec,
}

impl TeamRoles {
    pub fn get(&self, role: Role) -> &RoleSpec {
        match role {
            Role::Planner => &self.planner,
            Role::Executor => &self.executor,
            Role::Verifier => &self.verifier,
            Role::Utility => &self.utility,
        }
    }

    pub fn get_mut(&mut self, role: Role) -> &mut RoleSpec {
        match role {
            Role::Planner => &mut self.planner,
            Role::Executor => &mut self.executor,
            Role::Verifier => &mut self.verifier,
            Role::Utility => &mut self.utility,
        }
    }

    pub fn is_empty(&self) -> bool {
        Role::ALL.iter().all(|r| self.get(*r).is_empty())
    }
}

/// Default for [`TeamConfig::executor_fanout`] when unset.
pub const TEAM_EXECUTOR_FANOUT_DEFAULT: u8 = 1;
/// Hard ceiling for [`TeamConfig::executor_fanout`] (design §3.1). Copies
/// above this buy nothing measurable and multiply cost linearly.
pub const TEAM_EXECUTOR_FANOUT_MAX: u8 = 3;

/// `config.toml [team]` (global defaults) and `agent.toml [team]`
/// (per-employee overrides) — the same shape on both sides.
///
/// The three scalars are `Option` for the same reason [`RoleSpec`]'s fields
/// are: [`TeamConfig::merge`] is field-wise, so "unset" and "explicitly
/// false" must stay distinguishable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TeamConfig {
    /// Master switch. Unset ⇒ `true` since v1.66 — the decomposability gate,
    /// not this flag, is what keeps an ordinary task Solo.
    ///
    /// It used to default to `false`, which made teams doubly off (a flag AND
    /// a gate that leans Solo) and meant nothing ever exercised the path. The
    /// switch now says "teams may form", and [`crate::team_gate::decide`]
    /// still answers Solo for everything that does not clear its four
    /// signals. An explicit `enabled = false` remains a real kill switch, and
    /// a deployment that never wrote `[team.roles]` still runs Solo because
    /// its executor and verifier cascade onto the employee's own model and
    /// therefore share a family (see [`cascade_unbound_roles`]).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// How many executor copies work the same sub-task. Unset ⇒
    /// [`TEAM_EXECUTOR_FANOUT_DEFAULT`]; out of range ⇒ clamped into
    /// `1..=`[`TEAM_EXECUTOR_FANOUT_MAX`] with a note, never rejected (a
    /// fanout typo must not cost an operator their whole team).
    ///
    /// Stored as `i64`, the TOML integer width, so the clamp can *see* the
    /// out-of-range value it is correcting.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor_fanout: Option<i64>,
    /// `auto` / `always_solo` / `always_team`. Raw `String`; an unrecognised
    /// value degrades to [`TeamGateMode::Auto`] with a note (same convention
    /// as `[goal_loop] resume_on_restart` and `[goal_intent] mode`).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// `[team.roles.*]`.
    #[serde(deserialize_with = "crate::lenient::or_default")]
    #[serde(skip_serializing_if = "TeamRoles::is_empty")]
    pub roles: TeamRoles,
}

impl TeamConfig {
    /// True when nothing was written — lets [`AgentConfig`] skip serializing
    /// the whole section so an absent `[team]` stays absent on rewrite.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Resolved master switch. Unset ⇒ `true` (see [`TeamConfig::enabled`]).
    ///
    /// "Enabled" only means *a team may be considered*. Three independent
    /// things still have to hold before one forms: the spec must validate
    /// (executor and verifier must exist and must not share a model family),
    /// the decomposability gate must say Team, and the spawn budget must be
    /// able to pay for a minimal round.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Parse a `[team]` **table** value (i.e. the value of the `team` key),
    /// tolerantly: a non-table, or any wrong-typed key inside it, degrades to
    /// that field's default rather than failing.
    ///
    /// Takes `toml::Value` rather than text because the gateway already holds
    /// a parsed `config.toml` document.
    pub fn from_toml_value(value: &toml::Value) -> TeamConfig {
        value.clone().try_into().unwrap_or_default()
    }

    /// Parse the `[team]` section out of a whole `config.toml` document
    /// value. Missing section ⇒ all-defaults.
    pub fn from_config_toml(document: &toml::Value) -> TeamConfig {
        match document.get("team") {
            Some(v) => TeamConfig::from_toml_value(v),
            None => TeamConfig::default(),
        }
    }

    /// Field-wise cascade: `config.toml [team]` (`global`) underneath,
    /// `agent.toml [team]` (`agent`) on top. Every `Some` in `agent` wins;
    /// every `None` keeps the global value. Role tables merge per field
    /// ([`RoleSpec::merged_with`]), so overriding one role's `effort` does
    /// not erase the global `runtime`/`model` for that role.
    pub fn merge(global: &TeamConfig, agent: &TeamConfig) -> TeamConfig {
        let mut roles = TeamRoles::default();
        for role in Role::ALL {
            *roles.get_mut(*role) = global.roles.get(*role).merged_with(agent.roles.get(*role));
        }
        TeamConfig {
            enabled: agent.enabled.or(global.enabled),
            executor_fanout: agent.executor_fanout.or(global.executor_fanout),
            gate: agent.gate.clone().or_else(|| global.gate.clone()),
            roles,
        }
    }
}

/// Fill the two **required** roles from the employee's own
/// `[runtime] provider` / `[model] preferred` when `[team.roles]` left them
/// unbound, and report which ones were filled.
///
/// # Why this exists
///
/// [`validate_team`] requires an executor *and* a verifier: a team without
/// both is not a smaller team, it is Solo with extra steps. Before
/// `[team] enabled` defaulted to `true` that was the end of it — an
/// unconfigured `[team]` was simply disabled, so the missing halves never
/// came up. With the default flipped, "the operator wrote no roles" is the
/// normal case, and refusing it as a hard error would put a refusal row on
/// every goal task of every deployment.
///
/// So an unbound executor / verifier now inherits the employee's own brain,
/// which is exactly the cascade hop [`RoleSpec`] already documents for
/// planner and utility ("otherwise, for a role that named neither a runtime
/// nor a model, the employee's own `[model] preferred`"). The consequence is
/// deliberate and is the safety property the default-on flip rests on: two
/// roles cascading onto the same employee model share a model family, so
/// [`validate_team`] answers [`TeamConfigError::VerifierSameFamily`] and the
/// caller runs Solo. Turning teams on without naming a second vendor cannot
/// silently buy a correlated verifier.
///
/// **Planner and utility are deliberately not filled.** Their unbound state
/// already means "cascade to the employee's model" at spawn time, and
/// materialising it here would flip `ResolvedTeam::planner` from `None` to
/// `Some` — which is what decides whether a round runs a 規劃 stage at all.
/// A cascade must not add a stage nobody configured.
///
/// Pure and I/O-free: the caller reads `agent.toml` and passes the two
/// strings in. Empty / whitespace-only values count as absent.
pub fn cascade_unbound_roles(
    config: &TeamConfig,
    employee_runtime: Option<&str>,
    employee_model: Option<&str>,
) -> (TeamConfig, Vec<Role>) {
    let clean = |s: Option<&str>| -> Option<String> {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let runtime = clean(employee_runtime);
    let model = clean(employee_model);
    let mut out = config.clone();
    let mut cascaded = Vec::new();
    if runtime.is_none() && model.is_none() {
        return (out, cascaded);
    }
    // Only the two roles `validate_team` requires. See the doc comment.
    //
    // "Unbound" is `runtime.is_none() && model.is_none()` rather than
    // `RoleSpec::is_empty()`: a role that declared only an `effort` is just as
    // unbound, and its effort is a real preference that must survive the
    // cascade rather than being the reason it does not happen.
    for role in [Role::Executor, Role::Verifier] {
        let spec = out.roles.get_mut(role);
        if spec.runtime.is_some() || spec.model.is_some() {
            continue;
        }
        spec.runtime = runtime.clone();
        spec.model = model.clone();
        cascaded.push(role);
    }
    (out, cascaded)
}

/// Runtimes that may back a team role in the first batch (decision D).
///
/// Every one of these already registers the DuDuClaw MCP server natively, so
/// a role running on it gets the full tool surface. The remaining catalog
/// entries (`qwen`, `kimi`, `copilot`, `cursor`, `kiro`, `vibe`, `opencode`,
/// `openai_compat`) are **not** allowed yet — not because they cannot answer,
/// but because a role that silently loses its tools produces confident
/// tool-free narration, which the verifier cannot distinguish from work.
pub const TEAM_ROLE_RUNTIME_ALLOWLIST: &[&str] =
    &["claude", "codex", "gemini", "antigravity", "grok"];

/// `"low, medium, high, xhigh, max"` — built from [`crate::effort::Effort::ALL`]
/// so a new level never has to be remembered in an error message here.
fn effort_values_help() -> String {
    crate::effort::Effort::ALL
        .iter()
        .map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A non-fatal observation made while resolving a [`TeamConfig`].
///
/// Returned inside [`ResolvedTeam`] rather than logged: this crate is
/// I/O-free and the caller owns both the log and the audit sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamNote {
    /// `executor_fanout` was outside `1..=`[`TEAM_EXECUTOR_FANOUT_MAX`].
    ExecutorFanoutClamped { requested: i64, applied: u8 },
    /// `gate` held a value that is not a [`TeamGateMode`]; `auto` was used.
    UnknownGateMode { value: String },
    /// A role declared `effort` but neither `runtime` nor `model`, so it has
    /// no slot of its own and cascades to the employee's `[model] preferred`
    /// — which carries the employee's effort, not this one. The key is
    /// therefore inert. Surfaced rather than dropped: a config whose only
    /// written key does nothing is exactly the kind of thing an operator
    /// must be told about.
    EffortWithoutRoleBinding {
        role: Role,
        effort: crate::effort::Effort,
    },
    /// The role runs on a runtime the catalog marks deprecated (e.g. the
    /// Gemini CLI, R1 2026-10). The role still resolves and runs; the note
    /// carries the replacement and removal version for the caller's log.
    ///
    /// Never pushed into [`ResolvedTeam::notes`] — it is derived on demand by
    /// [`ResolvedTeam::deprecation_notes`], so a deprecation (a change in
    /// what the platform *says*, not in what a config *does*) leaves the
    /// resolved team byte-identical.
    DeprecatedRuntime { role: Role, runtime: &'static str },
}

impl TeamNote {
    /// Stable token for audit rows. Never derived from `Debug`.
    pub fn code(&self) -> &'static str {
        match self {
            TeamNote::ExecutorFanoutClamped { .. } => "executor_fanout_clamped",
            TeamNote::UnknownGateMode { .. } => "unknown_gate_mode",
            TeamNote::EffortWithoutRoleBinding { .. } => "effort_without_role_binding",
            TeamNote::DeprecatedRuntime { .. } => "deprecated_runtime",
        }
    }
}

impl std::fmt::Display for TeamNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamNote::ExecutorFanoutClamped { requested, applied } => write!(
                f,
                "executor_fanout {requested} is out of range 1..={TEAM_EXECUTOR_FANOUT_MAX}; using {applied}"
            ),
            TeamNote::UnknownGateMode { value } => write!(
                f,
                "unknown gate mode `{value}`; using auto (expected one of: {})",
                TeamGateMode::valid_values_help()
            ),
            TeamNote::EffortWithoutRoleBinding { role, effort } => write!(
                f,
                "[team.roles.{role}] effort `{effort}` is ignored: the role declares neither \
                 runtime nor model, so it cascades to the employee's own model"
            ),
            TeamNote::DeprecatedRuntime { role, runtime } => {
                match crate::runtime_catalog::spec_for(runtime).and_then(|s| s.deprecation) {
                    Some(dep) => write!(f, "[team.roles.{role}] {}", dep.notice(runtime)),
                    None => write!(f, "[team.roles.{role}] runtime `{runtime}` is deprecated"),
                }
            }
        }
    }
}

/// Why a [`TeamConfig`] cannot form a team. Closed enum — every variant
/// carries a stable [`TeamConfigError::code`] for the audit log.
///
/// Every variant is a **refusal**, not a downgrade: the caller's correct
/// response is to run the task Solo and surface the reason, never to form a
/// partial team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TeamConfigError {
    /// The role's runtime is not in [`TEAM_ROLE_RUNTIME_ALLOWLIST`] (or is
    /// not a runtime id at all).
    RuntimeNotAllowed { role: Role, runtime: String },
    /// The model does not belong to the declared runtime's family — or its
    /// family is unknown to the catalog and no runtime was declared, so there
    /// is nothing to bind it to. Never guessed (goose#10731).
    ModelRuntimeMismatch {
        role: Role,
        runtime: Option<String>,
        model: String,
    },
    /// Verifier and executor share a model family. Decision C: refuse to form
    /// the team (arXiv:2607.13918 — decorrelation is the whole mechanism).
    VerifierSameFamily { family: String },
    /// `effort` is not one of [`Effort::ALL`].
    InvalidEffort { role: Role, effort: String },
    /// A role that must resolve (executor, verifier) declared neither a
    /// runtime nor a model.
    Incomplete { role: Role },
}

impl TeamConfigError {
    /// Stable snake_case token for `role_turns.jsonl` / audit rows. Fixed
    /// strings, never `format!("{:?}", …).to_lowercase()` — that idiom has
    /// already produced one silently-wrong column in this workspace
    /// (`McpOnly` → `"mcponly"`).
    pub fn code(&self) -> &'static str {
        match self {
            TeamConfigError::RuntimeNotAllowed { .. } => "runtime_not_allowed",
            TeamConfigError::ModelRuntimeMismatch { .. } => "model_runtime_mismatch",
            TeamConfigError::VerifierSameFamily { .. } => "verifier_same_family",
            TeamConfigError::InvalidEffort { .. } => "invalid_effort",
            TeamConfigError::Incomplete { .. } => "team_incomplete",
        }
    }

    /// The role the error is about, when it is about one.
    pub fn role(&self) -> Option<Role> {
        match self {
            TeamConfigError::RuntimeNotAllowed { role, .. }
            | TeamConfigError::ModelRuntimeMismatch { role, .. }
            | TeamConfigError::InvalidEffort { role, .. }
            | TeamConfigError::Incomplete { role } => Some(*role),
            TeamConfigError::VerifierSameFamily { .. } => None,
        }
    }
}

impl std::fmt::Display for TeamConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TeamConfigError::RuntimeNotAllowed { role, runtime } => write!(
                f,
                "[team.roles.{role}] runtime `{runtime}` cannot back a team role (allowed: {})",
                TEAM_ROLE_RUNTIME_ALLOWLIST.join(", ")
            ),
            TeamConfigError::ModelRuntimeMismatch {
                role,
                runtime,
                model,
            } => match runtime {
                Some(rt) => write!(
                    f,
                    "[team.roles.{role}] model `{model}` does not belong to runtime `{rt}`"
                ),
                None => write!(
                    f,
                    "[team.roles.{role}] model `{model}` belongs to no known runtime family; \
                     declare `runtime` explicitly"
                ),
            },
            TeamConfigError::VerifierSameFamily { family } => write!(
                f,
                "verifier and executor both run model family `{family}`; a team needs an \
                 independent verifier family"
            ),
            TeamConfigError::InvalidEffort { role, effort } => write!(
                f,
                "[team.roles.{role}] effort `{effort}` is invalid (expected one of: {})",
                effort_values_help()
            ),
            TeamConfigError::Incomplete { role } => write!(
                f,
                "[team.roles.{role}] must declare a runtime or a model to form a team"
            ),
        }
    }
}

impl std::error::Error for TeamConfigError {}

/// One role slot after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRole {
    pub role: Role,
    /// Canonical catalog id (an alias such as `agy` is resolved to
    /// `antigravity`).
    pub runtime: &'static str,
    /// `None` ⇒ the caller cascades to the employee's `[model] preferred`.
    pub model: Option<String>,
    pub effort: Option<crate::effort::Effort>,
    /// Model-family key used by the verifier/executor decorrelation rule.
    /// See [`team_model_family`].
    pub family: &'static str,
}

/// A [`TeamConfig`] that passed [`validate_team`].
///
/// `planner` / `utility` may be `None` (the caller cascades those to the
/// employee's own model); `executor` and `verifier` are always `Some` — a
/// team without both never gets here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTeam {
    pub enabled: bool,
    pub gate: TeamGateMode,
    pub executor_fanout: u8,
    pub planner: Option<ResolvedRole>,
    pub executor: ResolvedRole,
    pub verifier: ResolvedRole,
    pub utility: Option<ResolvedRole>,
    /// Non-fatal observations (clamps, unknown enum values) for the caller to
    /// log or audit.
    pub notes: Vec<TeamNote>,
}

impl ResolvedTeam {
    pub fn role(&self, role: Role) -> Option<&ResolvedRole> {
        match role {
            Role::Planner => self.planner.as_ref(),
            Role::Executor => Some(&self.executor),
            Role::Verifier => Some(&self.verifier),
            Role::Utility => self.utility.as_ref(),
        }
    }

    /// One [`TeamNote::DeprecatedRuntime`] per bound role whose runtime the
    /// catalog marks deprecated, in planner → executor → verifier → utility
    /// order. Empty for a team with no deprecated runtime. Derived rather
    /// than stored so [`Self::notes`] is unchanged by a deprecation.
    pub fn deprecation_notes(&self) -> Vec<TeamNote> {
        [Role::Planner, Role::Executor, Role::Verifier, Role::Utility]
            .into_iter()
            .filter_map(|r| self.role(r))
            .filter(|rr| {
                crate::runtime_catalog::spec_for(rr.runtime)
                    .is_some_and(|s| s.deprecation.is_some())
            })
            .map(|rr| TeamNote::DeprecatedRuntime {
                role: rr.role,
                runtime: rr.runtime,
            })
            .collect()
    }
}

/// The model-family key a `(runtime, model)` pair belongs to, for the
/// verifier ≠ executor rule.
///
/// Derived from [`crate::runtime_catalog`], never hard-coded:
///
/// * with a model, the family is whichever catalog entry claims that model's
///   prefix (`runtime_for_model`);
/// * without a model, the family is whichever catalog entry claims the
///   runtime's *own* first declared prefix.
///
/// The second rule is what makes `antigravity` and `gemini` collapse onto one
/// family, which is correct and load-bearing: both serve `gemini-*` models,
/// so pairing them as executor and verifier would buy zero decorrelation
/// while looking, in the config, like two different vendors.
///
/// Falls back to the runtime id for a runtime that claims no family at all
/// (none of the allowlisted five do today; this keeps the function total).
pub fn team_model_family(
    runtime: &'static crate::runtime_catalog::RuntimeSpec,
    model: Option<&str>,
) -> &'static str {
    if let Some(m) = model {
        if let Some(spec) = crate::runtime_catalog::runtime_for_model(m) {
            return spec.id;
        }
    }
    match runtime.model_prefixes.first() {
        Some(prefix) => crate::runtime_catalog::runtime_for_model(prefix)
            .map(|s| s.id)
            .unwrap_or(runtime.id),
        None => runtime.id,
    }
}

/// Validate a merged [`TeamConfig`] and resolve it into a [`ResolvedTeam`].
///
/// Validation is **independent of `enabled`**: an operator editing a disabled
/// team still deserves to see that its spec is broken. Callers gate on
/// [`TeamConfig::is_enabled`] before forming a team; an all-empty config is
/// [`TeamConfigError::Incomplete`] by construction, which is the honest
/// answer to "can this config form a team?".
///
/// Roles are checked in [`Role::ALL`] order, so the first error a caller sees
/// is deterministic for a given config.
pub fn validate_team(config: &TeamConfig) -> Result<ResolvedTeam, TeamConfigError> {
    let mut notes = Vec::new();

    let gate = match config.gate.as_deref().map(str::trim) {
        None | Some("") => TeamGateMode::Auto,
        Some(raw) => raw.parse::<TeamGateMode>().unwrap_or_else(|_| {
            notes.push(TeamNote::UnknownGateMode {
                value: raw.to_string(),
            });
            TeamGateMode::Auto
        }),
    };

    let executor_fanout = match config.executor_fanout {
        None => TEAM_EXECUTOR_FANOUT_DEFAULT,
        Some(raw) => {
            let clamped = raw.clamp(1, TEAM_EXECUTOR_FANOUT_MAX as i64) as u8;
            if i64::from(clamped) != raw {
                notes.push(TeamNote::ExecutorFanoutClamped {
                    requested: raw,
                    applied: clamped,
                });
            }
            clamped
        }
    };

    // Resolved in `Role::ALL` order so the first error is deterministic.
    let planner = resolve_role(Role::Planner, &config.roles.planner, &mut notes)?;
    let executor = resolve_role(Role::Executor, &config.roles.executor, &mut notes)?;
    let verifier = resolve_role(Role::Verifier, &config.roles.verifier, &mut notes)?;
    let utility = resolve_role(Role::Utility, &config.roles.utility, &mut notes)?;

    // Executor and verifier must both exist — a team is a pipeline, and a
    // missing half is not a smaller team, it is Solo with extra steps.
    let executor = executor.ok_or(TeamConfigError::Incomplete {
        role: Role::Executor,
    })?;
    let verifier = verifier.ok_or(TeamConfigError::Incomplete {
        role: Role::Verifier,
    })?;

    if executor.family == verifier.family {
        return Err(TeamConfigError::VerifierSameFamily {
            family: executor.family.to_string(),
        });
    }

    Ok(ResolvedTeam {
        enabled: config.is_enabled(),
        gate,
        executor_fanout,
        planner,
        executor,
        verifier,
        utility,
        notes,
    })
}

/// Resolve one role. `Ok(None)` ⇒ the role declared nothing bindable (the
/// caller cascades to `[model] preferred`); that is allowed for planner and
/// utility and rejected later for executor and verifier.
fn resolve_role(
    role: Role,
    spec: &RoleSpec,
    notes: &mut Vec<TeamNote>,
) -> Result<Option<ResolvedRole>, TeamConfigError> {
    let runtime_raw = spec
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model_raw = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let effort_raw = spec
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Effort is validated even on an otherwise-empty role: a typo'd effort on
    // a role that cascades its model is still a typo, and staying silent
    // about it would ship a config whose only visible key does nothing.
    let effort = match effort_raw {
        Some(raw) => Some(raw.parse::<crate::effort::Effort>().map_err(|_| {
            TeamConfigError::InvalidEffort {
                role,
                effort: raw.to_string(),
            }
        })?),
        None => None,
    };

    let runtime_spec = match runtime_raw {
        Some(raw) => {
            let spec = crate::runtime_catalog::spec_for(raw)
                .filter(|s| TEAM_ROLE_RUNTIME_ALLOWLIST.contains(&s.id))
                .ok_or_else(|| TeamConfigError::RuntimeNotAllowed {
                    role,
                    runtime: raw.to_string(),
                })?;
            Some(spec)
        }
        None => None,
    };

    let (runtime_spec, model) = match (runtime_spec, model_raw) {
        // Neither declared — nothing to bind.
        (None, None) => {
            if let Some(effort) = effort {
                notes.push(TeamNote::EffortWithoutRoleBinding { role, effort });
            }
            return Ok(None);
        }
        // Model only: bind it to whichever runtime owns its family. A family
        // the catalog does not know is a hard error, never a guess.
        (None, Some(model)) => {
            let inferred = crate::runtime_catalog::runtime_for_model(model)
                .filter(|s| TEAM_ROLE_RUNTIME_ALLOWLIST.contains(&s.id))
                .ok_or_else(|| TeamConfigError::ModelRuntimeMismatch {
                    role,
                    runtime: None,
                    model: model.to_string(),
                })?;
            (inferred, Some(model.to_string()))
        }
        // Runtime only: the caller cascades the model.
        (Some(rt), None) => (rt, None),
        // Both: the model's family must be one this runtime serves.
        //
        // Checked against the runtime's OWN declared prefixes rather than
        // `runtime_for_model(model).id == rt.id`, because catalog order
        // breaks the `gemini` prefix tie in favour of the Gemini CLI — so the
        // strict identity check would reject the perfectly legal
        // `runtime = "antigravity", model = "gemini-3.7-flash"`.
        (Some(rt), Some(model)) => {
            let lower = model.to_ascii_lowercase();
            let bare = lower.rsplit('/').next().unwrap_or(&lower).to_string();
            let serves = rt.model_prefixes.iter().any(|p| bare.starts_with(*p));
            if !serves {
                return Err(TeamConfigError::ModelRuntimeMismatch {
                    role,
                    runtime: Some(rt.id.to_string()),
                    model: model.to_string(),
                });
            }
            (rt, Some(model.to_string()))
        }
    };

    let family = team_model_family(runtime_spec, model.as_deref());
    Ok(Some(ResolvedRole {
        role,
        runtime: runtime_spec.id,
        model,
        effort,
        family,
    }))
}

/// Declare [`RuntimeType`]'s variants and their catalog ids in ONE place.
///
/// The macro exists so a variant, its serde wire value, its `as_str()`
/// identifier and its [`crate::runtime_catalog`] key are physically impossible
/// to write inconsistently — the four used to be four separate hand-written
/// matches. `runtime_catalog` owns everything *about* a runtime (binary,
/// install channel, headless flags, auth); this enum is only the closed set of
/// keys the `RuntimeRegistry` and `agent.toml` use, and the paired
/// `every_runtime_type_has_a_catalog_spec` test proves the two never drift.
macro_rules! runtime_types {
    ($( $(#[$meta:meta])* $variant:ident => $id:literal ),+ $(,)?) => {
        /// Which agent runtime backend executes a prompt (RFC-25 multi-runtime).
        ///
        /// Used as the `RuntimeRegistry` key and parsed from `agent.toml
        /// [runtime] provider`. Defaults to [`RuntimeType::Claude`].
        /// Per-runtime facts live in [`crate::runtime_catalog::CATALOG`];
        /// reach them with [`RuntimeType::spec`].
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
        pub enum RuntimeType {
            $(
                $(#[$meta])*
                #[serde(rename = $id)]
                $variant,
            )+
        }

        impl RuntimeType {
            /// Every variant, in catalog order. `ALL[0]` is the default.
            pub const ALL: &'static [RuntimeType] = &[ $( RuntimeType::$variant, )+ ];

            /// Stable lowercase identifier (matches `agent.toml` values, the
            /// serde wire value, and the catalog id).
            pub fn as_str(&self) -> &'static str {
                match self { $( Self::$variant => $id, )+ }
            }

            /// Strict id → variant. `None` for anything not a canonical id —
            /// use [`Self::parse`] when aliases should be accepted, and never
            /// use either one where an unknown value must not resolve to a
            /// command (that is `runtime_catalog::spec_for`'s job).
            pub fn from_id(id: &str) -> Option<Self> {
                match id { $( $id => Some(Self::$variant), )+ _ => None }
            }
        }
    };
}

runtime_types! {
    #[default]
    Claude => "claude",
    Codex => "codex",
    Gemini => "gemini",
    /// Google Antigravity CLI (`agy`) — the 2026-06-18 successor to the
    /// personal-tier Gemini CLI. Same model lineage, distinct binary/flags.
    Antigravity => "antigravity",
    /// xAI Grok CLI ("Grok Build", beta 2026-05) — terminal coding agent driving
    /// `grok-build-0.1` behind a SuperGrok / X Premium+ subscription. MCP-native,
    /// `-p` headless mode. R4 phase 1 wired CLI detection + headless spawn;
    /// phase 2 (v1.41) added the dashboard one-click SuperGrok device-code
    /// login (`grok login --device-code`, see `cli_auth.rs`).
    Grok => "grok",
    /// Alibaba **Qwen Code** (`qwen`, npm `@qwen-code/qwen-code`). Driven
    /// through the generic print-mode runtime. The free OAuth tier was
    /// discontinued 2026-04-15 — API key / ModelStudio only.
    Qwen => "qwen",
    /// Moonshot AI **Kimi Code** (`kimi`, npm `@moonshot-ai/kimi-code`).
    /// Device-code login; shell API-key env vars are ignored by design.
    Kimi => "kimi",
    /// **GitHub Copilot CLI** (`copilot`, npm `@github/copilot`). GitHub OAuth
    /// device flow; plain-text headless output only (no JSON mode exists).
    Copilot => "copilot",
    /// AWS **Kiro CLI** (`kiro-cli`) — the 2026 successor to the Amazon Q
    /// Developer CLI.
    Kiro => "kiro",
    /// **Cursor CLI** (`cursor-agent`, installed by `cursor.com/install`).
    Cursor => "cursor",
    /// **Mistral Vibe** (`vibe`, PyPI `mistral-vibe`).
    Vibe => "vibe",
    /// **OpenCode** (`opencode`, sst/opencode) — a multi-provider shell driven
    /// headlessly with `opencode run`.
    OpenCode => "opencode",
    OpenAiCompat => "openai_compat",
}

impl RuntimeType {
    /// This runtime's catalog entry. Infallible: the
    /// `every_runtime_type_has_a_catalog_spec` test proves every variant has
    /// one, so a missing spec is a compile-time-adjacent bug, not a runtime
    /// condition callers should have to handle.
    pub fn spec(&self) -> &'static crate::runtime_catalog::RuntimeSpec {
        crate::runtime_catalog::spec_for(self.as_str())
            .expect("every RuntimeType has a runtime_catalog entry (enforced by test)")
    }

    /// Parse a runtime identifier. Canonical ids and the catalog's documented
    /// aliases (`agy`, `grok-cli`, `cursor-agent`, `openai`, …) both resolve —
    /// the alias list lives in [`crate::runtime_catalog`], so adding one no
    /// longer means editing this function.
    ///
    /// **`None` for anything else. There is no fallback.**
    ///
    /// This used to return `Self`, mapping an unknown string to `Claude` with a
    /// warning. With one runtime that was a typo-tolerance nicety; with twelve
    /// it is a correctness hole — a `auth.cli_login.start {runtime: "kimi"}`
    /// against a build that did not know `kimi` would silently run
    /// `claude setup-token` and hand the user someone else's login screen. Any
    /// caller that genuinely needs a default must now say so at the call site
    /// (see `runtime_config::parse_provider_or_default`, which logs the bad
    /// value first); a caller acting on a *request* must refuse.
    pub fn parse(s: &str) -> Option<Self> {
        crate::runtime_catalog::spec_for(s).and_then(|spec| Self::from_id(spec.id))
    }

    /// `claude|codex|gemini|…` — the accepted-values list for an error
    /// message, built from the catalog so it can never go stale.
    pub fn valid_values() -> String {
        crate::runtime_catalog::id_list_pipe()
    }

    /// This runtime's deprecation window, from the catalog (the single source
    /// of the version strings). `None` ⇒ not deprecated.
    pub fn deprecation(&self) -> Option<crate::runtime_catalog::RuntimeDeprecation> {
        self.spec().deprecation
    }

    /// `true` ⇒ the runtime still parses and runs, but is scheduled for
    /// removal. Never consulted by [`Self::parse`] — deprecation changes what
    /// callers *say*, not what a value resolves to.
    pub fn is_deprecated(&self) -> bool {
        self.deprecation().is_some()
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
/// `DUDUCLAW_EDITION` env  >  license tier  >  default ([`Personal`]).
/// `resolve` also takes an explicit override between the two (the gateway's
/// `GatewayConfig.edition`, meant for an embedding host); the shipped CLI
/// always passes `None`, and no `agent.toml` key feeds it.
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
    /// env override > explicit override > license tier > default.
    ///
    /// - `env`: value of `DUDUCLAW_EDITION` (`None` if unset).
    /// - `config`: an explicit override from the caller (the gateway passes
    ///   `GatewayConfig.edition`, which the shipped CLI leaves `None`; no
    ///   `agent.toml` key is read for it).
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
        for k in [
            "opensource",
            "hobby",
            "solo",
            "studio",
            "personal_pro_self_host",
            "",
        ] {
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
    /// Backend type. Only `"openai_compat"` (a local OpenAI-compatible
    /// server) can start; the in-process `"llama_cpp"` / `"mistral_rs"`
    /// backends were removed on 2026-09-29 and an existing file carrying one
    /// still parses (the dashboard shows it so the user can change it).
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
    "openai_compat".to_string()
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
    /// No reader (v1.68 audit). Defaulted so scaffolds need not write it.
    #[serde(default)]
    pub max_concurrent: u32,
    /// No reader (v1.68 audit). Defaulted so scaffolds need not write it.
    #[serde(default)]
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
    /// Command + args to run inside the container (empty = use the image default).
    ///
    /// HC5: the PTC container path sets this to the user-script invocation so the
    /// script actually executes inside the sandbox (instead of the image default).
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Environment variables to inject into the container, as `(key, value)` pairs.
    ///
    /// The PTC script sandbox passes none: scripts get no RPC socket or
    /// tool bridge.
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
    /// Allow the `computer_*` MCP tools (screenshot + mouse + keyboard in a
    /// gateway-owned container session).
    #[serde(default)]
    pub computer_use: bool,

    /// Computer use execution mode.
    /// - `container` (default): L5a — run inside a Docker container with Xvfb.
    /// - `native`: removed. Still parses; the `computer_*` tools refuse an
    ///   employee set to it (no fallback to a container) and `duduclaw
    ///   doctor` lists such employees.
    /// - `auto`: treated as `container`.
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
    ///
    /// Uses [`crate::lenient::string_vec`] like its four `*_tools` siblings
    /// below. Two reasons, both about a mixed array (`["Bash", 1]`):
    /// the raw reader this field now serves
    /// (`cli::expert::install::extract_agent_frontmatter`) used
    /// `filter_map(as_str)` and kept `["Bash"]`, and the strict `AgentConfig`
    /// path used to reject the whole file — meaning one stray element made
    /// the agent vanish from the registry entirely. The lenient helper makes
    /// both paths agree on "drop the bad element", which is also the only
    /// non-fatal option.
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    pub allowed_tools: Vec<String>,

    /// Explicit tool denylist. Tools listed here are always blocked,
    /// even if allowed by other flags. Evaluated after `allowed_tools`.
    ///
    /// Same [`crate::lenient::string_vec`] rationale as
    /// [`Self::allowed_tools`].
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
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

    /// Opt-in recording-to-skill capture (WP3.3). Master switch: when `false`
    /// (default), the `browser_record_start` / `browser_record_stop` /
    /// `desktop_record_start` / `desktop_record_stop` / `skill_from_recording`
    /// MCP tools are denied at the dispatch gate (fail-closed). Recordings
    /// capture live browser traffic / desktop screenshots and are privacy-
    /// sensitive, so this must be an explicit operator decision per agent.
    #[serde(default)]
    pub recording: bool,

    /// Opt-in per-agent authorization to hand the operator's SSH/GPG identity
    /// to this agent's spawned CLI subprocess (WP-10A, 2026-08). Master
    /// switch: when `false` (default), `duduclaw_core::spawn_env`'s WP-8B
    /// credential scrub applies unchanged — the child gets only the base
    /// allowlist (`PATH`/locale/proxy/…), so `git push` over SSH and a GPG
    /// commit signature both fail from inside a spawned agent CLI exactly as
    /// they did after the WP-8B env scrub shipped. When `true`,
    /// `duduclaw_core::spawn_env::GIT_CREDENTIALS_ENV_ALLOWLIST`
    /// (`SSH_AUTH_SOCK` / `SSH_AGENT_PID` / `GPG_TTY` / `GNUPGHOME`) is
    /// additionally carried through from the gateway's own environment, so
    /// `git`/`ssh`/`gpg` invoked by the agent can reach the operator's running
    /// `ssh-agent` / GPG keyring. This is a deliberately narrow, explicit
    /// grant of the operator's own push/signing identity — every spawn that
    /// actually carries one of these names is audit-logged (env var *names*
    /// only, never values; see
    /// `duduclaw_security::audit::log_git_credentials_granted`). This flag is
    /// not an org field (see `duduclaw_core::org_field_guard`), so it is
    /// changed through the normal `[capabilities]` write path
    /// (`agent_update` / dashboard) like every other capability here.
    #[serde(default)]
    pub git_credentials: bool,

    /// Opt-in system-operator designation: master switch for the `os_*`
    /// system-operation MCP tool face (device/system status, backup,
    /// power, update, factory reset, doctor). Default `false` — every
    /// agent, even one holding `Scope::Admin`, is denied these tools at
    /// the dispatch gate unless an operator explicitly sets this to
    /// `true` on that specific agent. Distinct from `os_native` (host
    /// automation for an ordinary agent's own machine footprint): this
    /// flag marks an agent as the machine's designated operator persona,
    /// a materially higher trust tier reserved for the small number of
    /// agents meant to run/restart/update/reset the box on a human's
    /// behalf. Also gates whether the conversational reply path routes a
    /// turn through the system-operation intent router before answering.
    #[serde(default)]
    pub system_operator: bool,

    /// Opt-in human-machine co-drive (人機共駕, CD-1,
    /// `commercial/docs/DESIGN-codrive-desktop-2026-08.md` §6 red line 1:
    /// "共駕能力預設關；開啟是 per-agent 明確授權"). Master switch for the
    /// `codrive_run` MCP tool — GUI-level mouse/keyboard injection into a
    /// shared desktop via the `duduclaw-comp` compositor's agent-injection
    /// socket. Default `false`: even an Admin-scoped agent is denied
    /// `codrive_run` at the dispatch gate unless an operator explicitly
    /// sets this to `true` on that specific agent — fail-closed, same
    /// deny-by-default shape as `system_operator` / `recording`.
    #[serde(default)]
    pub codrive: bool,

    /// Read-only SQL data sources this agent may query (WP-D,
    /// `DESIGN-redaction-field-rules-2026-09` §13.7). Each entry names a
    /// `config.toml [db_sources.<name>]` block.
    ///
    /// **Deny-by-default**: empty or absent means the `db_sources` /
    /// `db_tables` / `db_select` / `db_query` MCP tools are refused at the
    /// dispatch gate and hidden from `tools/list`, so a customer database is
    /// never reachable by an agent nobody granted it to. Granting a source
    /// here is not the whole authorization — the source's own
    /// `allowed_tables` still bounds what `db_select` / `db_tables` can see.
    ///
    /// Uses [`crate::lenient::string_vec`] like its `*_tools` siblings: one
    /// stray non-string element drops that element rather than making the
    /// agent fail to parse (and therefore vanish from the registry).
    /// `skip_serializing_if` keeps the on-disk shape unchanged for the
    /// overwhelming majority of agents that have no database grant.
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub db_sources: Vec<String>,

    // ── Formerly-untyped `[capabilities]` keys (R2 unification) ─────────
    //
    // These six lived in the same `[capabilities]` table as the fields above
    // but were read by raw `toml::Value` accessors in four different modules,
    // so `[capabilities]` alone straddled both schemas — the sharpest example
    // of the R2 split. They are typed here so an assembly layer sees one
    // section, and so the `agent_update` round-trip (which re-serializes
    // `AgentConfig` over the file) stops silently dropping them.
    //
    // Each is `skip_serializing_if`-guarded: a config that never wrote the key
    // still never gets it written back, so the on-disk shape is unchanged.
    /// Tools that require an active task-scoped grant (PORTICO).
    /// Missing ⇒ empty ⇒ nothing is scoped (documented fail-*safe*: a
    /// malformed config must not make every tool look scoped and brick the
    /// agent). Read by `gateway::capability_grants`.
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scoped_tools: Vec<String>,

    /// TTL for task-scoped grants. Missing / non-positive ⇒ the caller's
    /// `DEFAULT_GRANT_TTL_SECS`. Stored raw; the positivity filter stays in
    /// the accessor so the historical "0 means default, not zero" rule holds.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_ttl_secs: Option<i64>,

    /// Tools that must clear an ApprovalBroker request before running.
    /// Missing ⇒ empty ⇒ no approval friction. Read by `gateway::approval`
    /// (not migrated this round — see the remaining-shadow list).
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub approval_required_tools: Vec<String>,

    /// ActionGuard "always irreversible" tools. Missing ⇒ empty.
    /// Read by `gateway::approval` (not migrated this round).
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub irreversible_tools: Vec<String>,

    /// ActionGuard "maybe irreversible" tools (LLM judge, fail-closed).
    /// Missing ⇒ empty. Read by `gateway::approval` (not migrated this round).
    #[serde(default, deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub maybe_irreversible_tools: Vec<String>,

    /// Goal-loop autonomy level. Missing ⇒ the caller's conservative
    /// `Approver` default; an unrecognised string also ⇒ `Approver`. Stored as
    /// the raw string so that lenient parse survives here exactly as it did in
    /// the `toml::Value` reader. Read by `gateway::goal_loop`.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy_level: Option<String>,

    /// WP5 F1: operator opt-OUT of the install-class MCP approval gate.
    ///
    /// **Fail-closed:** only an explicit `true` disables the gate. Missing,
    /// wrong-typed, and `false` all leave it ON — so this stays `Option<bool>`
    /// rather than `bool`, and the accessor
    /// (`gateway::approval::auto_approve_install`) applies
    /// `unwrap_or(false)`. Read there.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_approve_install: Option<bool>,

    /// Per-effect / per-tool `allow` / `ask` / `block` rules
    /// (`action_rules = [{ effect = "send", verdict = "ask" }, { tool =
    /// "mail_send", verdict = "block" }]`), enforced by the MCP dispatch gate
    /// on top of the name lists above, take-the-stricter: a rule never
    /// removes an approval or a denial the name lists impose.
    ///
    /// Lives in `[capabilities]` on purpose: `org_field_guard` freezes the
    /// whole table against the employee's own writes. Parsing never fails the
    /// section; a malformed list or entry is kept verbatim and makes every
    /// side-effecting call at least `ask` (fail closed). See
    /// [`crate::tool_effect::ActionRules`].
    #[serde(default, skip_serializing_if = "crate::tool_effect::ActionRules::is_absent")]
    pub action_rules: crate::tool_effect::ActionRules,
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
    /// Removed (it drove the host desktop). Kept so existing `agent.toml`
    /// files parse; every computer-use entry point refuses it.
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
    /// Hosts a tool-driven computer-use session may open (`computer_navigate`).
    /// Exact hostnames only; read through [`Self::navigation_hosts`], which
    /// drops invalid entries. Empty (default) = no network at all. The
    /// chat-driven gateway loop ignores this list and always runs without
    /// network.
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub allowed_domains: Vec<String>,
    /// Durable computer-use workspaces (`computer_session_start` with
    /// `workspace`, the `computer_workspace_*` tools). Usable only when this
    /// AND `config.toml [computer_use.workspaces] enabled` are true. Missing
    /// or wrong-typed = false.
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub workspace: bool,
}

/// Most hosts a computer-use navigation allowlist may hold (after
/// de-duplication); later entries are dropped.
pub const COMPUTER_USE_MAX_ALLOWED_DOMAINS: usize = 20;

/// [`ComputerUseCapConfig::navigation_hosts`]: the usable hosts and the raw
/// entries that were dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationHosts {
    /// Lowercased, de-duplicated, valid hostnames (at most
    /// [`COMPUTER_USE_MAX_ALLOWED_DOMAINS`]), in configuration order.
    pub hosts: Vec<String>,
    /// Entries that were not usable (invalid shape, wildcard, IP literal,
    /// port/path-bearing, or over the cap).
    pub dropped: Vec<String>,
}

/// Normalize one navigation-allowlist entry: trimmed and lowercased, then
/// accepted only when it is an exact hostname per
/// [`crate::is_valid_egress_host`] (no wildcard) whose last label starts with
/// an ASCII letter (so numeric forms such as `1.2.3` or `0x7f000001`, which
/// resolvers read as IP addresses, are refused too).
pub fn normalize_navigation_host(entry: &str) -> Option<String> {
    let host = entry.trim().to_ascii_lowercase();
    if host.starts_with('*') || !crate::is_valid_egress_host(&host) {
        return None;
    }
    let last = host.rsplit('.').next()?;
    if !last.as_bytes().first().is_some_and(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    Some(host)
}

impl ComputerUseCapConfig {
    /// The usable navigation allowlist: every entry normalized with
    /// [`normalize_navigation_host`], duplicates removed, capped at
    /// [`COMPUTER_USE_MAX_ALLOWED_DOMAINS`]. Each dropped entry logs a
    /// warning (the entry itself is operator configuration, not a secret).
    pub fn navigation_hosts(&self) -> NavigationHosts {
        let mut out = NavigationHosts::default();
        for raw in &self.allowed_domains {
            match normalize_navigation_host(raw) {
                Some(host) if out.hosts.contains(&host) => {}
                Some(host) if out.hosts.len() < COMPUTER_USE_MAX_ALLOWED_DOMAINS => out.hosts.push(host),
                Some(_) => {
                    tracing::warn!(
                        entry = %crate::truncate_chars(raw, 80),
                        max = COMPUTER_USE_MAX_ALLOWED_DOMAINS,
                        "computer_use_config.allowed_domains: over the limit, entry ignored"
                    );
                    out.dropped.push(raw.clone());
                }
                None => {
                    tracing::warn!(
                        entry = %crate::truncate_chars(raw, 80),
                        "computer_use_config.allowed_domains: not an exact hostname (wildcard, IP address, port or path), entry ignored"
                    );
                    out.dropped.push(raw.clone());
                }
            }
        }
        out
    }
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
            allowed_domains: Vec::new(),
            workspace: false,
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
            recording: false,
            git_credentials: false,
            system_operator: false,
            codrive: false,
            db_sources: Vec::new(),
            scoped_tools: Vec::new(),
            grant_ttl_secs: None,
            approval_required_tools: Vec::new(),
            irreversible_tools: Vec::new(),
            maybe_irreversible_tools: Vec::new(),
            autonomy_level: None,
            auto_approve_install: None,
            action_rules: crate::tool_effect::ActionRules::default(),
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

/// Built-in Claude Code tools DuDuClaw agents may use on a **skip-permissions**
/// spawn path (channel reply / eval). This is the curated `--tools` set applied
/// under minimal-context mode when the agent has no explicit `allowed_tools`
/// allowlist: it drops the built-in tools DuDuClaw never uses in headless mode
/// (`Task` — DuDuClaw delegates via the `spawn_agent` MCP tool, not Claude's
/// native sub-agent tool; `ExitPlanMode` / `SlashCommand` — irrelevant under
/// `-p`) while preserving every file / shell / web / todo / notebook tool.
/// Sorted for deterministic CLI args.
pub const CURATED_BUILTIN_TOOLS: [&str; 13] = [
    "Bash",
    "BashOutput",
    "Edit",
    "Glob",
    "Grep",
    "KillShell",
    "MultiEdit",
    "NotebookEdit",
    "Read",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
];

/// Built-in tool subset auto-approved on the dispatcher path
/// (`claude_runner::prepare_claude_cmd`, `--permission-mode auto`). Mirrors the
/// built-in half of that path's `DEFAULT_ALLOWED_TOOLS` allowlist so `--tools`
/// never advertises a schema the allowlist would refuse to auto-approve (an
/// unusable schema is pure token waste there). Sorted.
pub const DISPATCH_DEFAULT_BUILTIN_TOOLS: [&str; 9] = [
    "Bash",
    "Edit",
    "Glob",
    "Grep",
    "Read",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
];

impl CapabilitiesConfig {
    /// Does this agent hold a grant for the `db_sources.<name>` block?
    ///
    /// Exact, trimmed, ASCII-case-insensitive equality — never a substring
    /// test (coding convention 2), so a grant for `crm` cannot reach
    /// `crm_payroll`. An empty grant list denies everything (deny-by-default).
    pub fn db_source_granted(&self, name: &str) -> bool {
        let want = name.trim();
        if want.is_empty() {
            return false;
        }
        self.db_sources
            .iter()
            .any(|g| g.trim().eq_ignore_ascii_case(want))
    }

    /// Curated `--tools` value for a minimal-context spawn.
    ///
    /// `--tools` controls which *built-in* Claude Code tool schemas are sent to
    /// the model — distinct from `--allowedTools`, which controls permission.
    /// Under minimal-context mode we send only the built-in tools the agent can
    /// actually use, dropping the rest of the ~21k-token built-in schema.
    ///
    /// - `allowed_tools` empty → `default_builtins` (the caller path's curated
    ///   set: [`CURATED_BUILTIN_TOOLS`] on skip-permissions paths,
    ///   [`DISPATCH_DEFAULT_BUILTIN_TOOLS`] on the allowlisted dispatcher path).
    /// - `allowed_tools` non-empty (allowlist mode) → the built-in entries of
    ///   the allowlist (`mcp__…` MCP patterns and a bare `*` are not built-in
    ///   tools), so `--tools` matches exactly what the allowlist permits.
    /// - Either way, `denied_tools` base-name matches are removed.
    ///
    /// This can only ever *narrow* the built-in surface, never widen it — an
    /// empty result means "no built-in tools" (`--tools ""`), correct for an
    /// MCP-only agent. Never-widening is the fail-safe direction for a tool
    /// gate: a misconfigured capability set drops tools, never grants extras.
    /// Base-name matching only (never substring, per coding convention 2): the
    /// part before an optional `(` qualifier, ASCII-case-insensitively.
    pub fn minimal_builtin_tools(&self, default_builtins: &[&str]) -> Vec<String> {
        fn base(entry: &str) -> &str {
            entry.split('(').next().unwrap_or(entry).trim()
        }
        let allowed = self.allowed_tools();
        let mut result: Vec<String> = if allowed.is_empty() {
            default_builtins.iter().map(|s| (*s).to_string()).collect()
        } else {
            allowed
                .iter()
                .filter(|t| !t.starts_with("mcp__"))
                .map(|t| base(t).to_string())
                .filter(|t| !t.is_empty() && t.as_str() != "*")
                .collect()
        };
        let denied: Vec<String> = self
            .disallowed_tools()
            .iter()
            .map(|t| base(t).to_ascii_lowercase())
            .collect();
        result.retain(|t| !denied.contains(&t.to_ascii_lowercase()));
        result.sort();
        result.dedup();
        result
    }

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
        let bare_denied = |tool: &str| {
            self.denied_tools
                .iter()
                .any(|d| d.trim().eq_ignore_ascii_case(tool))
        };
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
    /// No reader (v1.68 audit): the prediction-error activation path never
    /// consulted it. Kept, defaulted, so files that still carry it parse and
    /// scaffolds no longer have to write it.
    #[serde(default)]
    pub skill_auto_activate: bool,
    /// No reader (v1.68 audit): the skill scanner always runs. Kept,
    /// defaulted, for the same reason as `skill_auto_activate`.
    #[serde(default = "default_true")]
    pub skill_security_scan: bool,
    /// External factors to include in reflections.
    #[serde(default)]
    pub external_factors: ExternalFactorsConfig,

    /// Enable GVU (Generator-Verifier-Updater) loop for evolution proposals.
    ///
    /// **Default `false` (WP0.1, 2026-08-06 — fixes root cause R3).** Before
    /// this fix the struct default was `true` while the actual runtime gate
    /// (`gvu::trigger::agent_gvu_enabled` in duduclaw-gateway, which reads
    /// `agent.toml` directly rather than through this struct) defaulted a
    /// missing/absent key to `false`. The two defaults pointed opposite
    /// directions: dashboard code paths that render this struct's default
    /// showed GVU as "on", while the agent silently never ran it — 19 of 20
    /// production template agents were affected. Fail-closed opt-in wins:
    /// GVU only runs for agents whose `agent.toml` explicitly sets
    /// `gvu_enabled = true`. Template/scaffold generators must write the key
    /// explicitly (even when `false`) so the toggle is visible, not implicit.
    #[serde(default)]
    pub gvu_enabled: bool,

    /// DEPRECATED (D7, 2026-08-04): the cognitive memory layer is now a
    /// permanent part of the platform and can no longer be switched off.
    ///
    /// The field is retained **only** so existing `agent.toml` / `config.toml`
    /// files that still carry `[evolution] cognitive_memory = …` keep
    /// deserializing (serde compatibility). Every read path must go through
    /// [`EvolutionConfig::cognitive_memory_enabled`], which returns `true`
    /// unconditionally and logs one deprecation warning per process the first
    /// time a stored `false` is observed.
    #[serde(default = "default_true")]
    pub cognitive_memory: bool,

    /// Maximum hours of silence before the heartbeat silence-breaker fires.
    #[serde(default = "default_max_silence_hours")]
    pub max_silence_hours: f64,

    // ── Skill lifecycle ──
    /// Token budget for skills in system prompt (default 2500).
    #[serde(default = "default_skill_token_budget")]
    pub skill_token_budget: u32,

    /// Maximum concurrently active skills per agent (default 5).
    #[serde(default = "default_max_active_skills")]
    pub max_active_skills: usize,

    // ── Skill auto-synthesis (P0) ──
    //
    // H3 (2026-09-29): eight sibling keys in this block
    // (`skill_graduation_enabled`, `skill_recommendation_enabled`,
    // `skill_recommendation_threshold`, `curiosity_enabled`,
    // `curiosity_threshold`, `curiosity_max_daily`,
    // `skill_behavior_monitor_enabled`, `skill_behavior_drift_threshold`)
    // were REMOVED: the dashboard validated and wrote them, this struct typed
    // them, and the gateway had ZERO readers — a setting that is written,
    // displayed, and never takes effect is worse than no setting at all.
    // Unknown keys left behind in an existing `agent.toml` still deserialize
    // (this struct is not `deny_unknown_fields`), they simply do nothing,
    // which is what they already did.
    //
    // Every key that remains below has a named reader.
    /// Enable automatic skill synthesis from episodic memory when repeated
    /// domain gaps are detected. Read by
    /// `gateway::skill_lifecycle::synthesis_runner`.
    #[serde(default)]
    pub skill_synthesis_enabled: bool,

    /// Number of repeated gap detections required before a synthesis signal
    /// fires. Read per agent by `gateway::channel_reply` → `GapAccumulator`.
    #[serde(default = "default_skill_synthesis_threshold")]
    pub skill_synthesis_threshold: u32,

    /// Cooldown hours after a synthesis signal for the same topic. Read per
    /// agent by `gateway::channel_reply` → `GapAccumulator`.
    #[serde(default = "default_skill_synthesis_cooldown_hours")]
    pub skill_synthesis_cooldown_hours: u64,

    /// TTL (in conversations) for sandboxed trial skills before evaluation.
    /// Read by `gateway::skill_lifecycle::synthesis_runner`.
    #[serde(default = "default_skill_trial_ttl")]
    pub skill_trial_ttl: u32,

    /// Minimum lift required for skill graduation. Read per agent by
    /// `gateway::channel_reply` → `GraduationCriteria`.
    #[serde(default = "default_skill_graduation_min_lift")]
    pub skill_graduation_min_lift: f64,

    // ── Stagnation detection (P0 config, P1 suppress action) ──
    /// Configuration for the signal-stagnation detector.
    ///
    /// P0: only `log_only` action is active.
    /// P1: `suppress` action will be wired up here without schema changes.
    #[serde(default)]
    pub stagnation_detection: StagnationDetectionConfig,

    // ── Formerly-untyped `[evolution]` keys (R2 unification, 2nd pass) ───
    //
    // These four were read by raw `toml::Value` accessors in four different
    // gateway modules (`gvu::loop_`, `gvu::aee::intent`, `gvu::aee::run`,
    // `gvu::verifier_measure`) while `[evolution]` was otherwise typed here —
    // the same straddling-two-schemas shape `[capabilities]` had. Typing them
    // also stops the `agent_update` round-trip (which re-serializes
    // `AgentConfig` over `agent.toml`) from silently dropping them.
    //
    // Each is `skip_serializing_if`-guarded: a config that never wrote the key
    // still never gets it written back, so the on-disk shape is unchanged.
    /// AEE round-intent strategy (`balanced` / `innovate` / `harden` /
    /// `repair_only`). Stored raw: an unrecognised value must `warn!` and fall
    /// back to `balanced` at the accessor, which a strict serde enum here
    /// would turn into total agent loss. Read by `gateway::gvu::aee::intent`.
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,

    /// Hours a committed AEE round is observed before its entries settle.
    /// Missing / non-positive ⇒ the accessor's 24 h default; the clamp stays
    /// in the accessor.
    ///
    /// Uses [`crate::lenient::opt_number_lossy`] — NOT `opt_float_strict` —
    /// because this reader deliberately accepted an integer literal
    /// (`as_float().or_else(|| as_integer()…)`), the opposite of the `[fork]`
    /// budget quirk. Read by `gateway::gvu::aee::run`.
    #[serde(default, deserialize_with = "crate::lenient::opt_number_lossy")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aee_settle_hours: Option<f64>,

    /// `[evolution.noise_band]` — the AEE commit gate's per-dimension noise
    /// band. Read by `gateway::gvu::verifier_measure`.
    #[serde(default, skip_serializing_if = "NoiseBandSection::is_empty")]
    pub noise_band: NoiseBandSection,
}

/// `agent.toml [evolution.noise_band]` — AEE matches-or-improves tolerances.
///
/// Every key is individually optional and **float-only**: the raw reader used
/// `as_float()`, so `cases = 1` (an integer literal) was silently ignored and
/// kept the default. [`crate::lenient::opt_float_strict`] preserves that;
/// widening it here would quietly loosen a commit gate.
///
/// Clamping (`cases` to `NOISE_BAND_CASES_MAX`, the rest to `>= 0.0`) stays in
/// the accessor — it is a policy of the gate, not of the file format.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct NoiseBandSection {
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cases: Option<f64>,
    /// WP-4E: the *held-out* half of the case dimension, which the commit gate
    /// compares independently of the visible half. Absent ⇒ the accessor
    /// derives `cases / 2` (a held-out fence is deliberately stricter than the
    /// visible one); an explicit value is clamped to `<= cases` there, because
    /// a wider held-out band would re-open the masking hole this key exists to
    /// close.
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holdout: Option<f64>,
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<f64>,
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anti_sycophancy: Option<f64>,
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub novelty: Option<f64>,
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f64>,
}

impl NoiseBandSection {
    /// True when nothing was written — lets the enclosing section skip
    /// serializing it so an absent `[evolution.noise_band]` stays absent.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            skill_auto_activate: false,
            skill_security_scan: true,
            external_factors: Default::default(),
            // WP0.1 (2026-08-06): default false — see field doc on
            // `gvu_enabled` above for the R3 root-cause writeup.
            gvu_enabled: false,
            cognitive_memory: true,
            max_silence_hours: 12.0,
            skill_token_budget: 2500,
            max_active_skills: 5,
            // Skill lifecycle knobs with live readers (H3)
            skill_synthesis_enabled: false,
            skill_synthesis_threshold: 3,
            skill_trial_ttl: 20,
            skill_synthesis_cooldown_hours: 24,
            skill_graduation_min_lift: 0.1,
            // Stagnation detection
            stagnation_detection: StagnationDetectionConfig::default(),
            // R2 2nd pass: absent by default so the on-disk shape is
            // unchanged; each accessor owns its own missing-key direction.
            strategy: None,
            aee_settle_hours: None,
            noise_band: NoiseBandSection::default(),
        }
    }
}

impl EvolutionConfig {
    /// True iff the master switch is on AND at least one evolution path is
    /// individually enabled. The master switch (`enabled`) has veto power:
    /// when it is `false` this returns `false` even if every sub-toggle is on.
    pub fn is_any_evolution_enabled(&self) -> bool {
        self.enabled
            && (self.gvu_enabled || self.skill_auto_activate || self.skill_synthesis_enabled)
    }

    /// Whether the cognitive memory layer is active. **Always `true`** since
    /// D7 (2026-08-04): cognitive memory is a permanent platform capability,
    /// not a feature flag.
    ///
    /// The deprecated [`Self::cognitive_memory`] field is still parsed for
    /// backward compatibility with configs written before D7; an explicit
    /// `false` in such a config is ignored, and the first one observed in this
    /// process logs a single deprecation warning (further occurrences are
    /// silent — this is called on every reply path).
    pub fn cognitive_memory_enabled(&self) -> bool {
        if !self.cognitive_memory {
            COGNITIVE_MEMORY_DEPRECATION_WARNED.call_once(|| {
                tracing::warn!(
                    "`[evolution] cognitive_memory = false` is deprecated and ignored: \
                     cognitive memory is always on since 2026-08-04. \
                     Remove the key from your agent.toml / config.toml."
                );
            });
        }
        true
    }
}

/// One-shot guard so the `cognitive_memory` deprecation warning is logged at
/// most once per process, however many agents still carry the old key.
static COGNITIVE_MEMORY_DEPRECATION_WARNED: std::sync::Once = std::sync::Once::new();

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

fn default_skill_token_budget() -> u32 {
    2500
}

fn default_max_active_skills() -> usize {
    5
}

fn default_skill_synthesis_threshold() -> u32 {
    3
}

fn default_skill_trial_ttl() -> u32 {
    20
}

fn default_skill_synthesis_cooldown_hours() -> u64 {
    24
}

fn default_skill_graduation_min_lift() -> f64 {
    0.1
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
        assert_eq!(
            SandboxLevel::WorkspaceWrite.as_codex_flag(),
            "workspace-write"
        );
        assert_eq!(
            SandboxLevel::FullAccess.as_codex_flag(),
            "danger-full-access"
        );
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
/// Single Official Account per agent. The WP7 multi-OA layout
/// (`[[channels.line.accounts]]` + credit metering) was removed in 2026-09: the
/// per-account routing and the deduction gate were never wired, so the config
/// keys promised a behavior that did not exist. A stale `accounts` array in an
/// existing `config.toml` is ignored (this struct has no `deny_unknown_fields`),
/// never an error.
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

// ── agent.toml sections formerly read as raw `toml::Value` (R2) ─────────────
//
// Historically these sections existed ONLY as ad-hoc `toml::Value` accessors
// scattered across the gateway and CLI ("shadow readers"): the typed
// `AgentConfig` could not see them, and `AgentConfig` has no
// `deny_unknown_fields`, so the two schemas were mutually invisible. Any
// future single assembly point (preset / kit overlays) would have been
// bypassed by every shadow reader, and the `agent_update` MCP tool — which
// re-serializes `AgentConfig` over `agent.toml` — silently DROPPED these
// sections entirely.
//
// Typing them here fixes both. Two rules govern the migration:
//
// 1. **Missing-key defaults are reproduced exactly, never "corrected".**
//    Several of these directions contradict each other (`[guardrails]
//    enabled` ⇒ false but `block_secrets` ⇒ true; `[evolution] enabled` ⇒
//    true but `gvu_enabled` ⇒ false). That inconsistency is historical fact.
//    Changing it is a separate, deliberate decision — not a side effect of a
//    schema refactor. The `default_direction_*` tests lock each one down.
// 2. **Tolerance is preserved.** Every field uses `crate::lenient`, so a
//    wrong-typed key degrades to its default exactly as the `as_str()` /
//    `as_bool()` chains did, instead of failing the whole `AgentConfig` parse
//    and making the agent vanish from the registry.

/// `agent.toml [runtime]` — which backend executes this agent, plus PTY-pool
/// knobs.
///
/// Every field is `Option` on purpose: the dashboard's `agents.inspect` echoes
/// back only the keys actually present so the operator can tell "unset" from
/// an explicit `false` (the PTY-pool OAuth default-enable migration
/// materializes its toggle only when it was never written). Collapsing unset
/// into `false` here would silently opt agents out of that migration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct RuntimeSection {
    /// Raw provider string. Kept as `String` (not [`RuntimeType`]) because
    /// [`RuntimeType::parse`] is deliberately lenient — an unrecognised value
    /// warns and falls back to Claude rather than erroring, and a strict serde
    /// enum here would turn a provider typo into total agent loss.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Raw fallback provider string. Missing ⇒ `None` (no fallback).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Minimal-context spawn optimization (WP-7A). When applied, the spawned
    /// official CLI drops the operator's *user*-global settings and memory
    /// (`--setting-sources project,local` — keeps the agent's own
    /// `.claude/settings.json`, so the agent-file-guard hook still loads) and
    /// exposes only a curated built-in tool subset (`--tools`) instead of the
    /// full ~21k-token built-in schema. Measured fixed-overhead reduction:
    /// ~35.9k → ~11k tokens per spawn (local `claude -p` probe, 2026-08-16).
    /// `None` ⇒ the runtime default (ON); an explicit `false` opts a single
    /// agent out. Resolved via [`crate::agent_toml::resolve_minimal_context`],
    /// which also honors the `DUDUCLAW_MINIMAL_CONTEXT` env kill-switch.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal_context: Option<bool>,
}

impl RuntimeSection {
    /// True when nothing was written — lets `AgentConfig` skip serializing the
    /// whole section so an absent `[runtime]` stays absent on rewrite.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `agent.toml [guardrails]` — outbound reply scanning.
///
/// **Mixed default directions, preserved verbatim:** the master switch is
/// opt-in (`enabled` ⇒ `false`) but the two content scanners are opt-*out*
/// (`block_secrets` / `block_injection_echo` ⇒ `true`). The scanners only run
/// when the master switch is on, which is why "default true" is safe here —
/// but the asymmetry is real and is locked by test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct GuardrailsSection {
    /// Master switch. Missing ⇒ `false` (guardrails disabled entirely).
    pub enabled: bool,
    /// Missing ⇒ `true`.
    pub block_secrets: bool,
    /// Missing ⇒ `false`.
    pub redact_pii: bool,
    /// Missing ⇒ `true`.
    pub block_injection_echo: bool,
    /// Missing / non-array ⇒ empty.
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub deny_phrases: Vec<String>,
}

impl Default for GuardrailsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            block_secrets: true,
            redact_pii: false,
            block_injection_echo: true,
            deny_phrases: Vec::new(),
        }
    }
}

impl GuardrailsSection {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `agent.toml [os_watch]` — OS-native filesystem watching and its goal hook.
///
/// `paths` empty ⇒ **never watch**: the watcher is not started at all, no
/// matter what the other keys say. That early return is the section's real
/// master switch and is reproduced in the accessor, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct OsWatchSection {
    /// Missing / non-array ⇒ empty ⇒ nothing is ever watched.
    /// Tilde expansion happens in the accessor (it is a filesystem concern,
    /// not a schema one, and the dashboard edit form must see the raw value).
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Missing / non-array ⇒ empty.
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<String>,
    /// Missing ⇒ `None` ⇒ the watcher crate's `DEFAULT_DEBOUNCE_MS`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce_ms: Option<i64>,
    /// Missing ⇒ `None` ⇒ the watcher crate's `DEFAULT_MAX_EVENTS_PER_MIN`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_events_per_min: Option<i64>,
    /// Goal-loop kickoff template. Missing / blank ⇒ `None` ⇒ an OS file
    /// event never starts a goal.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_template: Option<String>,
    /// Optional acceptance-criteria template. Missing / blank ⇒ `None`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_acceptance: Option<String>,
    /// Frontmost-app poll interval, seconds. Read by `os_frontmost` (not
    /// migrated this round); typed here so `agent_update` stops dropping it.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmost_poll_secs: Option<i64>,
    /// Footprint distillation opt-in. Read by `footprint_distill` (not
    /// migrated this round). Missing ⇒ `None` ⇒ off (deny-by-default).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footprint: Option<bool>,
}

impl OsWatchSection {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// `agent.toml [fork]` — RFC-26 live forking (parallel branch exploration).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ForkSection {
    /// Missing ⇒ `false` (forking is opt-in per agent).
    pub enabled: bool,
    /// Missing / `< 1` ⇒ `4`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_branches: Option<i64>,
    /// Missing / `<= 0` ⇒ `0.50`.
    ///
    /// **Historical quirk, preserved:** the raw reader used `as_float()` only,
    /// so an integer literal (`default_budget_usd = 1`) was silently ignored
    /// and fell back to the default.
    ///
    /// [`crate::lenient::opt_float_strict`] — NOT `opt` — reproduces that:
    /// serde's own `f64` impl accepts and widens an integer, which would have
    /// doubled the effective ceiling of any config written that way. See
    /// `default_direction_fork_budget_rejects_integer_literal`.
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_budget_usd: Option<f64>,
    /// Missing / `<= 0` ⇒ `1.50`. Same integer-literal quirk as above.
    #[serde(deserialize_with = "crate::lenient::opt_float_strict")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_budget_usd: Option<f64>,
    /// Missing ⇒ `"auto_with_fallback"`. Unknown values are mapped by
    /// `parse_merge_mode` at use time, not rejected here.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_mode: Option<String>,
    /// Missing / blank ⇒ `None` (no verification command).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// Missing / `< 1` ⇒ `120`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_timeout_s: Option<i64>,
    /// Missing ⇒ `false`.
    pub fine_grained_judge: bool,
    /// Missing / unrecognised ⇒ `"heuristic"` (deterministic, zero LLM cost).
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<String>,
    /// Hours a retained (unresolved) branch workspace under `<home>/fork_ws/`
    /// survives before the sweep deletes it. Missing / `< 1` ⇒ `24`.
    #[serde(deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_workspace_ttl_hours: Option<i64>,
}

impl Default for ForkSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_branches: None,
            default_budget_usd: None,
            aggregate_budget_usd: None,
            merge_mode: None,
            test_command: None,
            test_timeout_s: None,
            fine_grained_judge: false,
            judge: None,
            retained_workspace_ttl_hours: None,
        }
    }
}

impl ForkSection {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
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

    // ── R2 unification: sections that used to be invisible here ─────────
    //
    // Each is skipped on serialize when untouched, so re-writing an
    // `agent.toml` that never had the section leaves the file shape
    // unchanged — while a config that DOES have it now survives the
    // `agent_update` round-trip instead of being silently dropped.
    /// `[runtime]` — see [`RuntimeSection`].
    #[serde(default, skip_serializing_if = "RuntimeSection::is_empty")]
    pub runtime: RuntimeSection,
    /// `[guardrails]` — see [`GuardrailsSection`].
    #[serde(default, skip_serializing_if = "GuardrailsSection::is_default")]
    pub guardrails: GuardrailsSection,
    /// `[os_watch]` — see [`OsWatchSection`].
    #[serde(default, skip_serializing_if = "OsWatchSection::is_empty")]
    pub os_watch: OsWatchSection,
    /// `[fork]` — see [`ForkSection`].
    #[serde(default, skip_serializing_if = "ForkSection::is_default")]
    pub fork: ForkSection,
    /// `[team]` — per-employee role composition overrides (Team-as-Agent
    /// P1/WP-1). Absent ⇒ the employee inherits `config.toml [team]`
    /// wholesale; see [`TeamConfig::merge`]. Typed here (rather than read
    /// only through [`crate::agent_toml::AgentTomlSections`]) so the
    /// `agent_update` round-trip cannot drop the section.
    #[serde(default, skip_serializing_if = "TeamConfig::is_default")]
    pub team: TeamConfig,
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

/// Per-agent `[memory]` section.
///
/// v1.68.0 removed the six MemGPT leftovers (`enabled`, `core_tokens`,
/// `recall_tokens`, `archival_tokens`, `recall_auto_inject`,
/// `archival_auto_retrieve`; MemGPT itself went in v1.8.1). Old `agent.toml`
/// files that still carry them deserialize fine (unknown keys are ignored)
/// and the keys are no longer written back.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct MemoryConfig {
    // ── Formerly-untyped `[memory]` keys (R2 unification) ───────────────
    //
    // These ARE consumed at runtime (RFC-24 decision continuity); they were
    // read by `gateway::runtime_config`'s raw-TOML accessors.
    // `skip_serializing_if` keeps the on-disk shape unchanged.
    /// RFC-24 decision continuity. Opt-in; missing ⇒ `false` (feature off).
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_continuity: Option<bool>,

    /// Days after which an unanswered open decision auto-expires (RFC-24
    /// §P3.2). Stored raw; the "non-positive ⇒ default 7" filter stays in the
    /// accessor, because TTL is always enforced (the ledger may not grow
    /// unbounded) and `0` therefore means "default", not "never expire".
    #[serde(default, deserialize_with = "crate::lenient::opt")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_ttl_days: Option<i64>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            decision_continuity: None,
            decision_ttl_days: None,
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
    /// Optional thread/topic ID within the chat (e.g. a Discord thread or a
    /// Telegram forum topic). Empty = post to the chat root.
    pub notify_thread_id: String,
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
            notify_thread_id: String::new(),
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
            tracing::warn!("proactive.max_turns is 0, bumping to default (8)");
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
/// Disabled by default — opt in per agent via `agent.toml [night_engine]`.
///
/// This comment used to claim a `config.toml [night_engine]` global default
/// existed as well. It does not: `night_engine::spawn_night_engine` reads only
/// `LoadedAgent.config.night_engine`, and `AgentRegistry::load` overlays
/// nothing global onto an agent's config. The one global knob for this feature
/// is `config.toml [night] llm_enabled`, which gates the LLM-backed sub-passes
/// (N1/N2) and is read by `gateway::night_llm::night_llm_enabled`.
/// Corrected 2026-09-29 while writing `docs/features/58-night-engine.md`.
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

    // ── WP-7A: minimal-context `--tools` curation ──────────────────────────

    /// Empty `allowed_tools` → the caller path's curated default set, verbatim.
    #[test]
    fn minimal_builtin_tools_empty_allowlist_uses_default() {
        let caps = CapabilitiesConfig::default();
        let got = caps.minimal_builtin_tools(&CURATED_BUILTIN_TOOLS);
        let mut want: Vec<String> = CURATED_BUILTIN_TOOLS.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want);
        // Never advertises Task / ExitPlanMode / SlashCommand.
        assert!(!got.iter().any(|t| t == "Task"));
    }

    /// Non-empty `allowed_tools` → only its built-in entries; MCP `mcp__…`
    /// patterns are dropped (they are not built-in tools), qualifiers stripped.
    #[test]
    fn minimal_builtin_tools_allowlist_keeps_only_builtins() {
        let caps = CapabilitiesConfig {
            allowed_tools: vec![
                "Read".into(),
                "Bash(git:*)".into(),
                "mcp__duduclaw__memory_search".into(),
                "*".into(),
            ],
            ..Default::default()
        };
        let got = caps.minimal_builtin_tools(&CURATED_BUILTIN_TOOLS);
        assert_eq!(got, vec!["Bash".to_string(), "Read".to_string()]);
    }

    /// `denied_tools` base-name matches are removed even from the default set.
    #[test]
    fn minimal_builtin_tools_denied_removed() {
        let caps = CapabilitiesConfig {
            denied_tools: vec!["Bash".into()],
            ..Default::default()
        };
        let got = caps.minimal_builtin_tools(&CURATED_BUILTIN_TOOLS);
        assert!(!got.iter().any(|t| t == "Bash"));
        assert!(got.iter().any(|t| t == "Read"));
    }

    /// An MCP-only allowlist yields an empty built-in set (`--tools ""`),
    /// never a fallback to the full default — narrowing only, never widening.
    #[test]
    fn minimal_builtin_tools_mcp_only_allowlist_is_empty() {
        let caps = CapabilitiesConfig {
            allowed_tools: vec!["mcp__duduclaw__tasks_list".into()],
            ..Default::default()
        };
        let got = caps.minimal_builtin_tools(&CURATED_BUILTIN_TOOLS);
        assert!(got.is_empty(), "MCP-only allowlist → no built-in tools, got {got:?}");
    }

    /// `[runtime] minimal_context` round-trips through the typed section.
    #[test]
    fn runtime_section_minimal_context_roundtrips() {
        let s: RuntimeSection = toml::from_str("minimal_context = false\n").unwrap();
        assert_eq!(s.minimal_context, Some(false));
        let absent: RuntimeSection = toml::from_str("provider = \"claude\"\n").unwrap();
        assert_eq!(absent.minimal_context, None);
    }

    /// `front_desk` (the expert-pack roster name) must load as TeamLeader —
    /// both via serde (existing agent.toml on disk) and FromStr (CLI/MCP
    /// inputs). Installed packs bricked at registry load without this.
    #[test]
    fn agent_role_front_desk_alias() {
        #[derive(serde::Deserialize)]
        struct Probe {
            role: AgentRole,
        }
        for raw in ["front_desk", "front-desk"] {
            let p: Probe = toml::from_str(&format!("role = \"{raw}\"")).unwrap();
            assert_eq!(p.role, AgentRole::TeamLeader, "serde alias {raw}");
        }
        assert_eq!(
            AgentRole::from_str("front_desk").unwrap(),
            AgentRole::TeamLeader
        );
        assert_eq!(
            AgentRole::from_str("Front Desk").unwrap(),
            AgentRole::TeamLeader
        );
    }

    // ── D7: cognitive memory is always on ──────────────────────────

    /// A pre-D7 config that still says `cognitive_memory = false` must keep
    /// deserializing (no hard error, no missing-field failure) and must be
    /// treated as ENABLED — the flag was removed, not honoured.
    #[test]
    fn legacy_cognitive_memory_false_parses_and_is_treated_as_on() {
        let raw = "\
            skill_auto_activate = true\n\
            skill_security_scan = true\n\
            gvu_enabled = false\n\
            cognitive_memory = false\n";
        let cfg: EvolutionConfig = toml::from_str(raw).expect("legacy config must still parse");
        // Raw field preserved (round-trips back to disk untouched)…
        assert!(
            !cfg.cognitive_memory,
            "raw deprecated field is preserved as parsed"
        );
        // …but the behavioural accessor ignores it.
        assert!(
            cfg.cognitive_memory_enabled(),
            "cognitive memory is always on since D7"
        );
        // Unrelated flags still parse normally.
        assert!(!cfg.gvu_enabled);
        assert!(cfg.enabled, "master switch defaults to true");
    }

    /// Absent key ⇒ default true, and the accessor agrees.
    #[test]
    fn absent_cognitive_memory_defaults_to_on() {
        let cfg: EvolutionConfig =
            toml::from_str("skill_auto_activate = false\nskill_security_scan = true\n").unwrap();
        assert!(cfg.cognitive_memory);
        assert!(cfg.cognitive_memory_enabled());
    }

    // ── R4: Grok runtime type ──────────────────────────────────────

    #[test]
    fn runtime_type_grok_parse_and_display_roundtrip() {
        // Config-string parse (case/alias-insensitive).
        assert_eq!(RuntimeType::parse("grok"), Some(RuntimeType::Grok));
        assert_eq!(RuntimeType::parse("GROK"), Some(RuntimeType::Grok));
        assert_eq!(RuntimeType::parse("grok-cli"), Some(RuntimeType::Grok));
        // Stable identifier.
        assert_eq!(RuntimeType::Grok.as_str(), "grok");
        // as_str ↔ parse round-trip for EVERY variant — driven by `ALL`, so a
        // new runtime is covered the moment it is declared.
        for rt in RuntimeType::ALL {
            assert_eq!(RuntimeType::parse(rt.as_str()), Some(*rt), "round-trip {rt:?}");
        }
    }

    #[test]
    fn runtime_type_grok_serde_roundtrip() {
        let json = serde_json::to_string(&RuntimeType::Grok).unwrap();
        assert_eq!(json, r#""grok""#);
        let back: RuntimeType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RuntimeType::Grok);
    }

    // ── WP-B: RuntimeType ↔ runtime_catalog coupling ───────────────

    /// The whole point of the catalog: a variant with no spec would be
    /// configurable but undetectable, uninstallable and unloggable-into —
    /// exactly the class of bug the six hand-written lists used to produce.
    #[test]
    fn every_runtime_type_has_a_catalog_spec() {
        for rt in RuntimeType::ALL {
            let spec = crate::runtime_catalog::spec_for(rt.as_str())
                .unwrap_or_else(|| panic!("RuntimeType::{rt:?} has no runtime_catalog entry"));
            assert_eq!(spec.id, rt.as_str());
            // …and `spec()` must not panic for any variant.
            assert_eq!(rt.spec().id, spec.id);
        }
    }

    /// …and the reverse: a catalog entry with no variant could never be
    /// selected as an agent's `[runtime] provider`.
    #[test]
    fn every_catalog_spec_has_a_runtime_type() {
        for spec in crate::runtime_catalog::CATALOG {
            assert!(
                RuntimeType::from_id(spec.id).is_some(),
                "catalog entry `{}` has no RuntimeType variant — it can be \
                 detected but never selected",
                spec.id
            );
        }
        assert_eq!(
            RuntimeType::ALL.len(),
            crate::runtime_catalog::CATALOG.len(),
            "the enum and the catalog must be the same set"
        );
    }

    /// Serde's wire value, `as_str()` and the catalog id are one string.
    /// A drift here silently rewrites every persisted `agent.toml`.
    #[test]
    fn serde_wire_value_equals_the_catalog_id() {
        for rt in RuntimeType::ALL {
            let json = serde_json::to_string(rt).unwrap();
            assert_eq!(json, format!("\"{}\"", rt.as_str()), "{rt:?}");
            let back: RuntimeType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *rt);
        }
        // The pre-WP-B wire values must be byte-identical (persisted configs).
        assert_eq!(RuntimeType::OpenAiCompat.as_str(), "openai_compat");
        assert_eq!(RuntimeType::Antigravity.as_str(), "antigravity");
    }

    #[test]
    fn runtime_type_parse_accepts_catalog_aliases() {
        assert_eq!(RuntimeType::parse("agy"), Some(RuntimeType::Antigravity));
        assert_eq!(RuntimeType::parse("openai"), Some(RuntimeType::OpenAiCompat));
        assert_eq!(
            RuntimeType::parse("openai-compat"),
            Some(RuntimeType::OpenAiCompat)
        );
        assert_eq!(RuntimeType::parse("qwen-code"), Some(RuntimeType::Qwen));
        assert_eq!(RuntimeType::parse("cursor-agent"), Some(RuntimeType::Cursor));
        assert_eq!(RuntimeType::parse("  KIMI  "), Some(RuntimeType::Kimi));
        // `from_id` is strict about ALIASES too (canonical ids only).
        assert!(RuntimeType::from_id("agy").is_none());
    }

    /// The hole this closes: `parse` used to map ANY unknown string to
    /// `Claude`. A request to log into a runtime this build does not know
    /// would then have run `claude setup-token` and shown the user the wrong
    /// vendor's login screen. Unknown MUST be `None` so the caller refuses.
    #[test]
    fn runtime_type_parse_rejects_unknown_ids_instead_of_defaulting() {
        for bad in [
            "",
            "   ",
            "claudee",              // typo
            "openai_compatible",    // near-miss
            "kimi2",               // a runtime this build does not have
            "not-a-runtime",
            "claude; rm -rf /",     // payload
            "../../etc/passwd",
            "сlaude",              // Cyrillic homoglyph
        ] {
            assert_eq!(
                RuntimeType::parse(bad),
                None,
                "`{bad}` must NOT resolve to a runtime"
            );
        }
        assert!(RuntimeType::parse(&"a".repeat(4096)).is_none());
        // …and the accepted-values list used in error messages stays in sync.
        let valid = RuntimeType::valid_values();
        for rt in RuntimeType::ALL {
            assert!(valid.contains(rt.as_str()), "{rt:?} missing from valid_values()");
        }
    }

    #[test]
    fn every_new_runtime_is_reachable() {
        // Pins the WP-B additions by name so a rename/removal is a test
        // failure rather than a silently missing backend.
        for (id, rt) in [
            ("qwen", RuntimeType::Qwen),
            ("kimi", RuntimeType::Kimi),
            ("copilot", RuntimeType::Copilot),
            ("kiro", RuntimeType::Kiro),
            ("cursor", RuntimeType::Cursor),
            ("vibe", RuntimeType::Vibe),
            ("opencode", RuntimeType::OpenCode),
        ] {
            assert_eq!(RuntimeType::from_id(id), Some(rt));
            assert_eq!(rt.as_str(), id);
            assert!(!rt.spec().binary.is_empty(), "{id} must name a binary");
        }
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
        cfg.skill_auto_activate = false;
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
        let tmp = std::env::temp_dir().join(format!("evo-master-off-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("agent.toml"), "[evolution]\nenabled = false\n").unwrap();
        assert!(!evolution_master_enabled(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn evolution_master_enabled_defaults_true_when_absent_or_missing() {
        // Missing file → true (not frozen). Present file without the key → true.
        let tmp = std::env::temp_dir().join(format!("evo-master-default-{}", uuid::Uuid::new_v4()));
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

    /// WP-D §13.7: `db_sources` is deny-by-default, exact-match, lenient about
    /// a stray non-string element, and invisible on the wire when empty.
    #[test]
    fn capabilities_db_sources_grant_semantics() {
        let none: CapabilitiesConfig = toml::from_str("computer_use = false").unwrap();
        assert!(none.db_sources.is_empty());
        assert!(!none.db_source_granted("crm"), "absent must deny");

        let caps: CapabilitiesConfig =
            toml::from_str("db_sources = [\"crm\", \"warehouse_pg\"]").unwrap();
        assert!(caps.db_source_granted("crm"));
        assert!(caps.db_source_granted(" CRM "), "trimmed, case-insensitive");
        assert!(caps.db_source_granted("warehouse_pg"));
        // Never a substring match (coding convention 2).
        assert!(!caps.db_source_granted("crm_payroll"));
        assert!(!caps.db_source_granted("cr"));
        assert!(!caps.db_source_granted(""));

        // One bad element drops that element, it does not sink the agent.
        let lenient: CapabilitiesConfig =
            toml::from_str("db_sources = [\"crm\", 7]").unwrap();
        assert_eq!(lenient.db_sources, vec!["crm".to_string()]);

        // Empty stays off the wire, so an agent.toml round-trip does not grow
        // a `db_sources = []` line it never had.
        let rendered = toml::to_string(&CapabilitiesConfig::default()).unwrap();
        assert!(!rendered.contains("db_sources"), "{rendered}");
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
        assert_eq!(
            serde_json::to_string(&AgentRole::TeamLeader).unwrap(),
            "\"team-leader\""
        );
        assert_eq!(
            serde_json::to_string(&AgentRole::ProductManager).unwrap(),
            "\"product-manager\""
        );
        // Single-word variants stay identical to the old lowercase encoding.
        assert_eq!(serde_json::to_string(&AgentRole::Main).unwrap(), "\"main\"");
        assert_eq!(serde_json::to_string(&AgentRole::Qa).unwrap(), "\"qa\"");
    }

    #[test]
    fn agent_role_serde_aliases_accepted() {
        let cases = [
            ("\"engineer\"", AgentRole::Developer),
            ("\"quality-assurance\"", AgentRole::Qa),
            ("\"tl\"", AgentRole::TeamLeader),
            ("\"pm\"", AgentRole::ProductManager),
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
            ("team-leader", AgentRole::TeamLeader),
            ("team_leader", AgentRole::TeamLeader),
            ("Team Leader", AgentRole::TeamLeader),
            ("  TEAM-LEADER  ", AgentRole::TeamLeader),
            ("product_manager", AgentRole::ProductManager),
            ("engineer", AgentRole::Developer),
            ("quality", AgentRole::Qa),
            ("main", AgentRole::Main),
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
        for role in [
            AgentRole::TeamLeader,
            AgentRole::ProductManager,
            AgentRole::Qa,
        ] {
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
        assert_eq!(
            EditionProfile::parse("  Enterprise  "),
            EditionProfile::Enterprise
        );
        assert_eq!(
            EditionProfile::parse("individual"),
            EditionProfile::Personal
        );
        assert_eq!(
            EditionProfile::parse("enterprise_edition"),
            EditionProfile::Enterprise
        );
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
            assert_eq!(
                EditionProfile::from_tier_key(k),
                EditionProfile::Enterprise,
                "{k}"
            );
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
            assert_eq!(
                EditionProfile::from_tier_key(k),
                EditionProfile::Personal,
                "{k}"
            );
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
        assert_eq!(
            EditionProfile::resolve(None, None, None),
            EditionProfile::Personal
        );
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
        assert_eq!(
            serde_json::to_string(&EditionProfile::Personal).unwrap(),
            "\"personal\""
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // `[team]` — Team-as-Agent schema, cascade and validation (P1/WP-1)
    // ═══════════════════════════════════════════════════════════════════

    /// The spec from the design doc §3.1, verbatim.
    const TEAM_TOML: &str = r#"
enabled = true
executor_fanout = 2
gate = "auto"

[roles.planner]
runtime = "claude"
model = "claude-fable-5-1"
effort = "high"

[roles.executor]
runtime = "codex"
model = "gpt-5.5"
effort = "medium"

[roles.verifier]
runtime = "gemini"
model = "gemini-3.7-flash"
effort = "low"

[roles.utility]
runtime = "claude"
model = "claude-haiku-4-5"
"#;

    fn team(toml_body: &str) -> TeamConfig {
        TeamConfig::from_toml_value(&toml_body.parse::<toml::Value>().expect("valid toml"))
    }

    // ── enums ───────────────────────────────────────────────────────────

    #[test]
    fn team_role_tokens_and_parsing() {
        assert_eq!(Role::ALL.len(), 4);
        for r in Role::ALL {
            assert_eq!(r.as_str().parse::<Role>().unwrap(), *r);
            assert_eq!(r.to_string(), r.as_str());
            let json = serde_json::to_string(r).unwrap();
            assert_eq!(json, format!("\"{}\"", r.as_str()));
            assert_eq!(serde_json::from_str::<Role>(&json).unwrap(), *r);
        }
        assert_eq!("  EXECUTOR ".parse::<Role>().unwrap(), Role::Executor);
        // No aliases: a near-miss must not silently route to a role.
        assert!("exec".parse::<Role>().is_err());
        assert!("plan".parse::<Role>().is_err());
        assert!("facade".parse::<Role>().is_err());
    }

    /// `[team]` does not own the effort enum — [`crate::effort`] does. This
    /// only pins the contract `validate_team` depends on.
    #[test]
    fn effort_parsing_comes_from_the_shared_effort_module() {
        use crate::effort::Effort;
        for e in Effort::ALL {
            assert_eq!(e.as_str().parse::<Effort>().unwrap(), *e);
        }
        assert_eq!("HIGH".parse::<Effort>().unwrap(), Effort::High);
        assert!("ultra".parse::<Effort>().is_err());
        assert!("".parse::<Effort>().is_err());
        assert_eq!(effort_values_help(), "low, medium, high, xhigh, max");
    }

    #[test]
    fn team_gate_mode_tokens_and_parsing() {
        assert_eq!(TeamGateMode::default(), TeamGateMode::Auto);
        for m in [
            TeamGateMode::Auto,
            TeamGateMode::AlwaysSolo,
            TeamGateMode::AlwaysTeam,
        ] {
            assert_eq!(m.as_str().parse::<TeamGateMode>().unwrap(), m);
            assert_eq!(
                serde_json::to_string(&m).unwrap(),
                format!("\"{}\"", m.as_str())
            );
        }
        assert_eq!(
            "always-solo".parse::<TeamGateMode>().unwrap(),
            TeamGateMode::AlwaysSolo
        );
        assert!("sometimes".parse::<TeamGateMode>().is_err());
    }

    // ── parsing ─────────────────────────────────────────────────────────

    #[test]
    fn team_section_parses_the_design_document_shape() {
        let c = team(TEAM_TOML);
        assert_eq!(c.enabled, Some(true));
        assert_eq!(c.executor_fanout, Some(2));
        assert_eq!(c.gate.as_deref(), Some("auto"));
        assert_eq!(c.roles.planner.runtime.as_deref(), Some("claude"));
        assert_eq!(c.roles.planner.model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(c.roles.planner.effort.as_deref(), Some("high"));
        assert_eq!(c.roles.executor.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(c.roles.verifier.runtime.as_deref(), Some("gemini"));
        assert_eq!(c.roles.utility.model.as_deref(), Some("claude-haiku-4-5"));
        assert!(c.roles.utility.effort.is_none());
    }

    #[test]
    fn team_section_is_lenient_like_every_other_agent_toml_section() {
        // Wrong-typed keys degrade to their defaults; nothing fails.
        let c = team(
            r#"
enabled = "yes"
executor_fanout = "two"
gate = 7
roles = "nope"
"#,
        );
        assert_eq!(c, TeamConfig::default());
        assert!(c.is_default());

        // A wrong-typed role table degrades to an empty role, keeping its
        // siblings.
        let c = team(
            r#"
[roles]
planner = "claude"
[roles.executor]
runtime = "codex"
"#,
        );
        assert!(c.roles.planner.is_empty());
        assert_eq!(c.roles.executor.runtime.as_deref(), Some("codex"));

        // An unknown fifth role is ignored, not invented.
        let c = team(
            r#"
[roles.reviewer]
runtime = "claude"
"#,
        );
        assert!(c.roles.is_empty());
    }

    #[test]
    fn team_config_from_a_whole_config_document() {
        let doc: toml::Value = r#"
[server]
port = 8080

[team]
enabled = true
[team.roles.executor]
runtime = "codex"
"#
        .parse()
        .unwrap();
        let c = TeamConfig::from_config_toml(&doc);
        assert_eq!(c.enabled, Some(true));
        assert_eq!(c.roles.executor.runtime.as_deref(), Some("codex"));

        // Missing section ⇒ all defaults, not an error.
        let doc: toml::Value = "[server]\nport = 1\n".parse().unwrap();
        assert!(TeamConfig::from_config_toml(&doc).is_default());
    }

    // ── cascade ─────────────────────────────────────────────────────────

    #[test]
    fn merge_is_field_wise_in_both_directions() {
        let global = team(TEAM_TOML);
        // The employee overrides ONE key of ONE role.
        let agent = team(
            r#"
[roles.executor]
effort = "high"
"#,
        );
        let merged = TeamConfig::merge(&global, &agent);
        // The overridden field wins…
        assert_eq!(merged.roles.executor.effort.as_deref(), Some("high"));
        // …and the rest of that role survives, which is the whole point.
        assert_eq!(merged.roles.executor.runtime.as_deref(), Some("codex"));
        assert_eq!(merged.roles.executor.model.as_deref(), Some("gpt-5.5"));
        // Untouched roles and scalars come through unchanged.
        assert_eq!(merged.roles.verifier, global.roles.verifier);
        assert_eq!(merged.enabled, Some(true));
        assert_eq!(merged.executor_fanout, Some(2));
    }

    #[test]
    fn merge_keeps_explicit_false_distinct_from_unset() {
        let global = TeamConfig {
            enabled: Some(true),
            ..TeamConfig::default()
        };
        // Unset on the employee ⇒ inherit `true`.
        assert_eq!(
            TeamConfig::merge(&global, &TeamConfig::default()).enabled,
            Some(true)
        );
        // Explicit `false` ⇒ the employee opts out.
        let off = TeamConfig {
            enabled: Some(false),
            ..TeamConfig::default()
        };
        assert_eq!(TeamConfig::merge(&global, &off).enabled, Some(false));
        assert!(!TeamConfig::merge(&global, &off).is_enabled());
        // Nothing set anywhere ⇒ the switch is on since v1.66. What keeps an
        // unconfigured deployment Solo is `cascade_unbound_roles` plus the
        // gate, not this flag — see `default_on_unconfigured_team_cannot_form`.
        assert!(TeamConfig::default().is_enabled());
    }

    /// X2: the master switch defaults **on**, and an explicit `false` is still
    /// a real kill switch that survives a `true` global.
    #[test]
    fn team_enabled_defaults_to_true_and_explicit_false_still_wins() {
        assert_eq!(TeamConfig::default().enabled, None, "unset stays unset");
        assert!(TeamConfig::default().is_enabled());
        let off = TeamConfig {
            enabled: Some(false),
            ..TeamConfig::default()
        };
        assert!(!off.is_enabled());
        let global_on = TeamConfig {
            enabled: Some(true),
            ..TeamConfig::default()
        };
        assert!(
            !TeamConfig::merge(&global_on, &off).is_enabled(),
            "an employee's explicit opt-out must beat a global opt-in"
        );
    }

    /// X2 safety condition: with the switch on by default and no
    /// `[team.roles]`, the cascade hands executor and verifier the *same*
    /// employee model, so the decorrelation rule refuses the spec and the
    /// caller runs Solo. Nothing about that is an error.
    #[test]
    fn default_on_unconfigured_team_cannot_form() {
        let empty = TeamConfig::default();
        assert!(empty.is_enabled(), "precondition: the switch is on");
        let (cascaded_cfg, cascaded) =
            cascade_unbound_roles(&empty, Some("claude"), Some("claude-sonnet-4-6"));
        assert_eq!(cascaded, vec![Role::Executor, Role::Verifier]);
        assert_eq!(
            validate_team(&cascaded_cfg).unwrap_err(),
            TeamConfigError::VerifierSameFamily {
                family: "claude".to_string()
            },
            "two roles on one employee model must not pass as a decorrelated team"
        );
    }

    /// The employee declaring nothing bindable leaves the roles unbound, and
    /// the spec is `Incomplete` — still Solo, still not a partial team.
    #[test]
    fn cascade_with_no_employee_brain_leaves_the_spec_incomplete() {
        let (cfg, cascaded) = cascade_unbound_roles(&TeamConfig::default(), None, Some("   "));
        assert!(cascaded.is_empty());
        assert_eq!(cfg, TeamConfig::default());
        assert_eq!(
            validate_team(&cfg).unwrap_err(),
            TeamConfigError::Incomplete {
                role: Role::Executor
            }
        );
    }

    /// A half-configured team is the case the cascade exists for: the operator
    /// pinned an executor, the verifier cascades onto the employee's own
    /// model, the families differ, and a real team forms.
    #[test]
    fn cascade_completes_a_half_configured_team() {
        let c = team(
            r#"
[roles.executor]
runtime = "codex"
model = "gpt-5.5"
"#,
        );
        let (cfg, cascaded) = cascade_unbound_roles(&c, Some("claude"), Some("claude-sonnet-4-6"));
        assert_eq!(cascaded, vec![Role::Verifier], "only the unbound role moves");
        let r = validate_team(&cfg).expect("codex executor + claude verifier decorrelates");
        assert_eq!(r.executor.runtime, "codex");
        assert_eq!(r.verifier.runtime, "claude");
        assert_eq!(r.verifier.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_ne!(r.executor.family, r.verifier.family);
        assert!(
            r.planner.is_none() && r.utility.is_none(),
            "the cascade must not materialise a 規劃 stage nobody configured"
        );
    }

    /// A role that declared only an `effort` is still unbound — and its effort
    /// survives the cascade rather than being the reason it is skipped.
    #[test]
    fn cascade_fills_an_effort_only_role_and_keeps_the_effort() {
        let c = team(
            r#"
[roles.executor]
effort = "high"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let (cfg, cascaded) = cascade_unbound_roles(&c, Some("claude"), Some("claude-sonnet-4-6"));
        assert_eq!(cascaded, vec![Role::Executor]);
        assert_eq!(cfg.roles.executor.effort.as_deref(), Some("high"));
        let r = validate_team(&cfg).expect("claude executor + gemini verifier decorrelates");
        assert_eq!(r.executor.effort, Some(crate::effort::Effort::High));
    }

    /// An explicitly bound role is never rewritten by the cascade.
    #[test]
    fn cascade_never_overwrites_a_bound_role() {
        let c = team(
            r#"
[roles.executor]
runtime = "codex"
model = "gpt-5.5"
[roles.verifier]
runtime = "gemini"
model = "gemini-3-pro-preview"
"#,
        );
        let (cfg, cascaded) = cascade_unbound_roles(&c, Some("claude"), Some("claude-opus-5"));
        assert!(cascaded.is_empty());
        assert_eq!(cfg, c, "a fully bound spec must pass through byte-identical");
    }

    // ── validation: happy paths ─────────────────────────────────────────

    #[test]
    fn validate_team_resolves_the_design_document_spec() {
        use crate::effort::Effort;
        let resolved = validate_team(&team(TEAM_TOML)).expect("the documented spec is valid");
        assert!(resolved.enabled);
        assert_eq!(resolved.gate, TeamGateMode::Auto);
        assert_eq!(resolved.executor_fanout, 2);
        assert!(resolved.notes.is_empty());

        let planner = resolved.planner.as_ref().unwrap();
        assert_eq!(planner.runtime, "claude");
        assert_eq!(planner.family, "claude");
        assert_eq!(planner.effort, Some(Effort::High));

        assert_eq!(resolved.executor.runtime, "codex");
        assert_eq!(resolved.executor.family, "codex");
        assert_eq!(resolved.executor.effort, Some(Effort::Medium));

        assert_eq!(resolved.verifier.runtime, "gemini");
        assert_eq!(resolved.verifier.family, "gemini");

        let utility = resolved.utility.as_ref().unwrap();
        assert_eq!(utility.model.as_deref(), Some("claude-haiku-4-5"));
        assert!(utility.effort.is_none());

        // The accessor agrees with the fields.
        assert_eq!(resolved.role(Role::Executor).unwrap().runtime, "codex");
        assert_eq!(resolved.role(Role::Planner).unwrap().runtime, "claude");
    }

    #[test]
    fn planner_and_utility_may_cascade_to_the_employees_own_model() {
        let c = team(
            r#"
[roles.executor]
runtime = "codex"
model = "gpt-5.5"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert!(r.planner.is_none(), "planner cascades to [model] preferred");
        assert!(r.utility.is_none());
        // A verifier with only a runtime still resolves; its model cascades.
        assert_eq!(r.verifier.runtime, "gemini");
        assert!(r.verifier.model.is_none());
        assert_eq!(r.verifier.family, "gemini");
    }

    #[test]
    fn a_runtime_alias_canonicalises() {
        let c = team(
            r#"
[roles.executor]
runtime = "claude"
model = "claude-sonnet-4-6"
[roles.verifier]
runtime = "agy"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.verifier.runtime, "antigravity");
    }

    #[test]
    fn a_model_without_a_runtime_binds_through_the_catalog_not_a_guess() {
        let c = team(
            r#"
[roles.executor]
model = "gpt-5.5"
[roles.verifier]
model = "claude-sonnet-4-6"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.executor.runtime, "codex");
        assert_eq!(r.verifier.runtime, "claude");

        // …but a family the catalog does not know is refused outright.
        let c = team(
            r#"
[roles.executor]
model = "deepseek-v3.2"
[roles.verifier]
runtime = "gemini"
"#,
        );
        assert_eq!(
            validate_team(&c).unwrap_err(),
            TeamConfigError::ModelRuntimeMismatch {
                role: Role::Executor,
                runtime: None,
                model: "deepseek-v3.2".to_string(),
            }
        );
    }

    #[test]
    fn antigravity_may_declare_a_gemini_model_despite_the_catalog_tie() {
        // `runtime_for_model("gemini-…")` resolves to the Gemini CLI because
        // catalog order breaks the tie — so a strict identity check would
        // reject this perfectly legal pairing.
        let c = team(
            r#"
[roles.executor]
runtime = "claude"
model = "claude-sonnet-4-6"
[roles.verifier]
runtime = "antigravity"
model = "gemini-3.1-pro"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.verifier.runtime, "antigravity");
        // …and its FAMILY still collapses onto gemini, which is what the
        // decorrelation rule must see.
        assert_eq!(r.verifier.family, "gemini");
    }

    // ── validation: refusals ────────────────────────────────────────────

    #[test]
    fn a_runtime_outside_the_first_batch_allowlist_is_refused() {
        for runtime in ["qwen", "openai_compat", "copilot", "cursor", "notarealcli"] {
            let c = team(&format!(
                r#"
[roles.executor]
runtime = "{runtime}"
[roles.verifier]
runtime = "gemini"
"#
            ));
            assert_eq!(
                validate_team(&c).unwrap_err(),
                TeamConfigError::RuntimeNotAllowed {
                    role: Role::Executor,
                    runtime: runtime.to_string(),
                },
                "{runtime} must not back a team role"
            );
        }
        // The allowlist is exactly decision D.
        assert_eq!(
            TEAM_ROLE_RUNTIME_ALLOWLIST,
            &["claude", "codex", "gemini", "antigravity", "grok"]
        );
    }

    #[test]
    fn a_model_from_another_family_is_refused_never_re_routed() {
        // goose#10731: a role model string not bound to its provider sent
        // `qwen-*` to the Claude backend.
        let c = team(
            r#"
[roles.executor]
runtime = "claude"
model = "gpt-5.5"
[roles.verifier]
runtime = "gemini"
"#,
        );
        assert_eq!(
            validate_team(&c).unwrap_err(),
            TeamConfigError::ModelRuntimeMismatch {
                role: Role::Executor,
                runtime: Some("claude".to_string()),
                model: "gpt-5.5".to_string(),
            }
        );
    }

    #[test]
    fn a_verifier_sharing_the_executors_family_refuses_to_form_a_team() {
        // Decision C: refuse, do not warn — decorrelation is the entire
        // mechanism (arXiv:2607.13918).
        let same_family = [
            // literally the same runtime
            (
                "claude",
                "claude-sonnet-4-6",
                "claude",
                "claude-haiku-4-5",
                "claude",
            ),
            // two runtimes, one family: antigravity also serves gemini-*
            (
                "gemini",
                "gemini-3.7-flash",
                "antigravity",
                "gemini-3.1-pro",
                "gemini",
            ),
        ];
        for (e_rt, e_model, v_rt, v_model, family) in same_family {
            let c = team(&format!(
                r#"
[roles.executor]
runtime = "{e_rt}"
model = "{e_model}"
[roles.verifier]
runtime = "{v_rt}"
model = "{v_model}"
"#
            ));
            assert_eq!(
                validate_team(&c).unwrap_err(),
                TeamConfigError::VerifierSameFamily {
                    family: family.to_string(),
                },
                "{e_rt}/{v_rt}"
            );
        }
    }

    #[test]
    fn an_executor_or_verifier_that_resolves_to_nothing_is_incomplete() {
        // Nothing at all.
        assert_eq!(
            validate_team(&TeamConfig::default()).unwrap_err(),
            TeamConfigError::Incomplete {
                role: Role::Executor
            }
        );
        // Executor present, verifier missing.
        let c = team(
            r#"
[roles.executor]
runtime = "codex"
"#,
        );
        assert_eq!(
            validate_team(&c).unwrap_err(),
            TeamConfigError::Incomplete {
                role: Role::Verifier
            }
        );
        // A blank string is not a declaration.
        let c = team(
            r#"
[roles.executor]
runtime = "   "
model = ""
[roles.verifier]
runtime = "gemini"
"#,
        );
        assert_eq!(
            validate_team(&c).unwrap_err(),
            TeamConfigError::Incomplete {
                role: Role::Executor
            }
        );
    }

    #[test]
    fn an_invalid_effort_is_refused_on_any_role() {
        let base = team(
            r#"
[roles.executor]
runtime = "codex"
model = "gpt-5.5"
[roles.verifier]
runtime = "gemini"
"#,
        );
        validate_team(&base).expect("the base spec is valid before the effort typo");
        for role in Role::ALL {
            let mut c = base.clone();
            c.roles.get_mut(*role).effort = Some("ultra".to_string());
            let err = validate_team(&c).unwrap_err();
            assert_eq!(err.code(), "invalid_effort", "{role}: {err}");
            assert_eq!(err.role(), Some(*role));
        }
    }

    #[test]
    fn validation_is_independent_of_the_enabled_switch() {
        // An operator editing a disabled team still gets told it is broken.
        let c = team(
            r#"
enabled = false
[roles.executor]
runtime = "claude"
[roles.verifier]
runtime = "claude"
"#,
        );
        assert_eq!(
            validate_team(&c).unwrap_err().code(),
            "verifier_same_family"
        );

        let c = team(
            r#"
enabled = false
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert!(!r.enabled, "a valid spec can still be switched off");
    }

    // ── notes (non-fatal) ───────────────────────────────────────────────

    #[test]
    fn executor_fanout_is_clamped_with_a_note_never_rejected() {
        let base = r#"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#;
        for (written, applied) in [(0i64, 1u8), (-4, 1), (1, 1), (3, 3), (9, 3), (999, 3)] {
            let c = team(&format!("executor_fanout = {written}\n{base}"));
            let r = validate_team(&c).unwrap();
            assert_eq!(r.executor_fanout, applied, "fanout {written}");
            let clamped = r
                .notes
                .iter()
                .any(|n| n.code() == "executor_fanout_clamped");
            assert_eq!(
                clamped,
                i64::from(applied) != written,
                "a note iff the value was corrected (fanout {written})"
            );
        }
        // Unset ⇒ the default, no note.
        let r = validate_team(&team(base)).unwrap();
        assert_eq!(r.executor_fanout, TEAM_EXECUTOR_FANOUT_DEFAULT);
        assert!(r.notes.is_empty());
    }

    #[test]
    fn an_unknown_gate_mode_degrades_to_auto_with_a_note() {
        let c = team(
            r#"
gate = "always-team-ish"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.gate, TeamGateMode::Auto);
        assert_eq!(
            r.notes,
            vec![TeamNote::UnknownGateMode {
                value: "always-team-ish".to_string()
            }]
        );
        assert!(!r.notes[0].to_string().is_empty());

        // A known mode carries no note.
        let c = team(
            r#"
gate = "always_solo"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.gate, TeamGateMode::AlwaysSolo);
        assert!(r.notes.is_empty());
    }

    #[test]
    fn an_effort_on_an_unbound_role_is_reported_not_silently_dropped() {
        use crate::effort::Effort;
        let c = team(
            r#"
[roles.planner]
effort = "high"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert!(r.planner.is_none());
        assert_eq!(
            r.notes,
            vec![TeamNote::EffortWithoutRoleBinding {
                role: Role::Planner,
                effort: Effort::High,
            }]
        );
    }

    // ── audit surface ───────────────────────────────────────────────────

    #[test]
    fn team_error_and_note_codes_are_unique_and_stable() {
        use crate::effort::Effort;
        let errors = [
            TeamConfigError::RuntimeNotAllowed {
                role: Role::Planner,
                runtime: "x".into(),
            },
            TeamConfigError::ModelRuntimeMismatch {
                role: Role::Planner,
                runtime: None,
                model: "x".into(),
            },
            TeamConfigError::VerifierSameFamily { family: "x".into() },
            TeamConfigError::InvalidEffort {
                role: Role::Planner,
                effort: "x".into(),
            },
            TeamConfigError::Incomplete {
                role: Role::Planner,
            },
        ];
        let mut codes: Vec<&str> = errors.iter().map(|e| e.code()).collect();
        let n = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), n);
        assert_eq!(
            codes,
            vec![
                "invalid_effort",
                "model_runtime_mismatch",
                "runtime_not_allowed",
                "team_incomplete",
                "verifier_same_family",
            ]
        );
        for e in &errors {
            assert!(!e.to_string().is_empty());
        }

        let notes = [
            TeamNote::ExecutorFanoutClamped {
                requested: 9,
                applied: 3,
            },
            TeamNote::UnknownGateMode { value: "x".into() },
            TeamNote::EffortWithoutRoleBinding {
                role: Role::Utility,
                effort: Effort::Low,
            },
        ];
        let mut ncodes: Vec<&str> = notes.iter().map(|n| n.code()).collect();
        let n = ncodes.len();
        ncodes.sort_unstable();
        ncodes.dedup();
        assert_eq!(ncodes.len(), n);
    }

    /// R1 (2026-10): a role on the deprecated Gemini CLI still validates and
    /// leaves `notes` untouched; the deprecation surfaces only through the
    /// derived `deprecation_notes()`.
    #[test]
    fn a_gemini_role_resolves_unchanged_and_yields_a_derived_deprecation_note() {
        let c = team(
            r#"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "gemini"
"#,
        );
        let r = validate_team(&c).unwrap();
        assert_eq!(r.verifier.runtime, "gemini");
        assert!(r.notes.is_empty(), "deprecation must not alter notes");
        let dn = r.deprecation_notes();
        assert_eq!(
            dn,
            vec![TeamNote::DeprecatedRuntime {
                role: Role::Verifier,
                runtime: "gemini"
            }]
        );
        assert_eq!(dn[0].code(), "deprecated_runtime");
        let text = dn[0].to_string();
        assert!(text.contains("antigravity") && text.contains("v1.72.0"), "{text}");

        let c = team(
            r#"
[roles.executor]
runtime = "codex"
[roles.verifier]
runtime = "antigravity"
"#,
        );
        assert!(validate_team(&c).unwrap().deprecation_notes().is_empty());
    }

    // ── agent.toml round-trip ───────────────────────────────────────────

    #[test]
    fn agent_config_round_trips_the_team_section_and_keeps_absence_absent() {
        // An `agent.toml` with no `[team]` must serialize back without one —
        // otherwise `agent_update` rewrites every agent file on first touch.
        let base = r#"
[agent]
name = "a"
display_name = "A"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = ""

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = []

[container]
timeout_ms = 60000
max_concurrent = 1
readonly_project = true

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 500
warn_threshold_percent = 80
hard_stop = false

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = true
gvu_enabled = false
max_silence_hours = 168.0
skill_token_budget = 500
max_active_skills = 2
"#;
        let cfg: AgentConfig = toml::from_str(base).expect("base parses");
        assert!(cfg.team.is_default());
        let out = toml::to_string(&cfg).unwrap();
        assert!(!out.contains("[team]"), "absent stays absent:\n{out}");

        // …and a written `[team]` survives the round-trip instead of being
        // silently dropped.
        let with_team = format!(
            "{base}\n[team]\nenabled = true\nexecutor_fanout = 2\n\
             [team.roles.executor]\nruntime = \"codex\"\nmodel = \"gpt-5.5\"\n\
             [team.roles.verifier]\nruntime = \"gemini\"\n"
        );
        let cfg: AgentConfig = toml::from_str(&with_team).expect("parses with [team]");
        assert_eq!(cfg.team.enabled, Some(true));
        let out = toml::to_string(&cfg).unwrap();
        let back: AgentConfig = toml::from_str(&out).expect("re-parses");
        assert_eq!(back.team, cfg.team);
        assert_eq!(validate_team(&back.team).unwrap().executor.runtime, "codex");
    }

    /// Regression (H1, 2026-09): the WP7 `[[channels.line.accounts]]` multi-OA
    /// block and its `credit_rate` key were removed together with the credit
    /// ledger. An existing `config.toml` that still carries them must keep
    /// parsing (unknown keys are ignored, never a hard error), and the legacy
    /// single-OA fields must survive untouched.
    #[test]
    fn line_channel_config_ignores_removed_multi_oa_accounts_block() {
        let legacy = r#"
channel_token = "legacy-token"
channel_secret = "legacy-secret"

[[accounts]]
name = "acme-support"
channel_token = "acme-token"
channel_secret = "acme-secret"
agent_id = "acme-agent"
credit_rate = 2.0
"#;
        let cfg: LineChannelConfig = toml::from_str(legacy).expect("legacy accounts block parses");
        assert_eq!(cfg.channel_token, "legacy-token");
        assert_eq!(cfg.channel_secret, "legacy-secret");
        // Round-trips without re-emitting the removed block.
        let out = toml::to_string(&cfg).unwrap();
        assert!(!out.contains("accounts"), "re-serialized: {out}");
        assert!(!out.contains("credit_rate"), "re-serialized: {out}");
    }

    // ── computer-use navigation allowlist ─────────────────────────────────

    #[test]
    fn navigation_hosts_normalize_dedup_and_drop_invalid() {
        let cfg = ComputerUseCapConfig {
            allowed_domains: vec![
                " Example.COM ".into(),
                "example.com".into(),
                "docs.example.com".into(),
                "*.example.com".into(),
                "93.184.216.34".into(),
                "1.2.3".into(),
                "0x7f000001".into(),
                "example.com:443".into(),
                "example.com/path".into(),
                "https://example.com".into(),
                "".into(),
                "user@example.com".into(),
                "[::1]".into(),
            ],
            ..Default::default()
        };
        let got = cfg.navigation_hosts();
        assert_eq!(got.hosts, vec!["example.com".to_string(), "docs.example.com".to_string()]);
        assert_eq!(got.dropped.len(), 10);
    }

    #[test]
    fn navigation_hosts_cap_at_twenty() {
        let cfg = ComputerUseCapConfig {
            allowed_domains: (0..25).map(|i| format!("h{i}.example.com")).collect(),
            ..Default::default()
        };
        let got = cfg.navigation_hosts();
        assert_eq!(got.hosts.len(), COMPUTER_USE_MAX_ALLOWED_DOMAINS);
        assert_eq!(got.hosts[0], "h0.example.com");
        assert_eq!(got.dropped.len(), 5);
    }

    #[test]
    fn computer_use_workspace_switch_defaults_off_and_tolerates_wrong_types() {
        let parse = |t: &str| toml::from_str::<CapabilitiesConfig>(t).unwrap();
        assert!(!parse("computer_use = true\n").computer_use_config.workspace);
        assert!(parse("[computer_use_config]\nworkspace = true\n").computer_use_config.workspace);
        let wrong = parse("[computer_use_config]\nworkspace = \"yes\"\nmax_actions = 9\n");
        assert!(!wrong.computer_use_config.workspace);
        assert_eq!(wrong.computer_use_config.max_actions, 9);
    }

    #[test]
    fn allowed_domains_reads_from_toml_and_tolerates_wrong_types() {
        let caps: CapabilitiesConfig = toml::from_str(
            "computer_use = true\n[computer_use_config]\nallowed_domains = [\"example.com\", 7]\n",
        )
        .unwrap();
        assert!(caps.computer_use);
        assert_eq!(caps.computer_use_config.allowed_domains, vec!["example.com".to_string()]);
        // A non-array value degrades to empty without losing the rest.
        let caps: CapabilitiesConfig = toml::from_str(
            "computer_use = true\n[computer_use_config]\nmax_actions = 9\nallowed_domains = \"example.com\"\n",
        )
        .unwrap();
        assert!(caps.computer_use);
        assert_eq!(caps.computer_use_config.max_actions, 9);
        assert!(caps.computer_use_config.allowed_domains.is_empty());
        // Absent = empty.
        assert!(ComputerUseCapConfig::default().navigation_hosts().hosts.is_empty());
    }
}
