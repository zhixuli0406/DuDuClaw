//! The single typed parse point for `agent.toml` sections that used to be read
//! with hand-rolled `toml::Value` accessors.
//!
//! # Why
//!
//! `agent.toml` carried two mutually invisible schemas: the typed
//! [`crate::types::AgentConfig`] (loaded once by `AgentRegistry`) and a
//! scattered set of raw-TOML "shadow readers" that re-read the file on every
//! call. The shadow readers were the problem: they read the **file**, so any
//! future assembly layer that resolves configuration before handing it
//! downstream (preset / kit overlays) would be silently bypassed by every one
//! of them — a config where half the gates see the resolved values and half
//! see the raw file is the hardest class of bug to diagnose.
//!
//! This module collapses those readers onto one typed projection,
//! [`AgentTomlSections`], which shares its section structs with `AgentConfig`.
//! One schema, two entry points:
//!
//! | | [`AgentConfig`] | [`AgentTomlSections`] |
//! |---|---|---|
//! | used by | `AgentRegistry::load_agent`, dashboard round-trip | the migrated per-call readers |
//! | strictness | strict (a missing `[agent]` is a hard error) | total (always parses) |
//! | shares section types | ✔ | ✔ |
//!
//! # Two invariants this migration must not break
//!
//! **No cache.** The shadow readers re-read the file on every call, so a live
//! `agent.toml` edit took effect immediately without a registry rescan. This
//! module deliberately keeps that: it reads on every call. The registry's own
//! `AgentConfig` cache has no mtime invalidation (there is no file watcher and
//! no periodic rescan — only an explicit `update_agent_toml_with` triggers
//! one), so routing these readers through it would have converted an immediate
//! read into a stale one. Preserving read-per-call keeps the change purely
//! structural, and costs exactly what it cost before.
//!
//! **No default-direction drift.** Every accessor reproduces its predecessor's
//! missing-key behavior exactly, including the directions that contradict each
//! other. See the `default_direction_*` tests here and in the migrated modules.

use std::path::Path;

use serde::Deserialize;

use crate::lenient::{TomlFlag, TomlNumber, Tri};
use crate::types::{
    CapabilitiesConfig, ForkSection, GuardrailsSection, MemoryConfig, NoiseBandSection,
    OsWatchSection, RuntimeSection,
};
#[cfg(test)]
use crate::types::PolicyEffect;

/// The `[model]` keys the former shadow readers consumed.
///
/// A deliberately narrow projection rather than [`crate::types::ModelConfig`]:
/// `ModelConfig` has three required fields (`preferred` / `fallback` /
/// `account_pool`), so it cannot participate in a total ("always parses")
/// deserialization. Giving those fields serde defaults would instead loosen
/// `AgentConfig` itself — a `[model]` table missing `preferred` would start
/// loading with an empty model string rather than being rejected — which is a
/// behavior change well outside this refactor's remit.
///
/// The keys below are the same ones typed on `ModelConfig`;
/// `model_view_matches_typed_model_config` locks the two against drift.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ModelSectionView {
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub preferred: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub utility: Option<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub fallbacks: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub standard: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub delegation_routing: Option<bool>,
}

/// The `[agent]` identity keys the former shadow readers consumed.
///
/// A narrow projection rather than [`crate::types::AgentInfo`] for the same
/// reason as [`ModelSectionView`]: `AgentInfo`'s fields are required, so it
/// cannot participate in a total deserialization, and giving them serde
/// defaults would loosen `AgentConfig` itself.
///
/// Carried as `Option<AgentSectionView>` on [`AgentTomlSections`] because the
/// **presence of the `[agent]` table is load-bearing** for two readers:
/// `export_to::read_agent` and `budget::agent_display_name` both bail out
/// entirely when `table.get("agent").and_then(|v| v.as_table())` is `None`.
/// Collapsing "no `[agent]` table" into "an all-empty one" would turn a
/// skipped export into an export of a blank agent.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct AgentSectionView {
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub name: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub display_name: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub role: Option<String>,
    /// Raw lifecycle status. Kept as `String` (not
    /// [`crate::types::AgentStatus`]) because the reader it replaces mapped an
    /// unrecognised value to `None` (indeterminate) rather than erroring.
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub status: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub reports_to: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub icon: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub trigger: Option<String>,
}

/// The `[budget]` keys `gateway::budget` consumed.
///
/// Narrow projection of [`crate::types::BudgetConfig`] (whose fields are
/// required, so it cannot deserialize totally) with one extra property the
/// typed struct does not have: every field is **three-state**. `budget`'s
/// reader logs a loud `warn!` for a present-but-wrong-typed key ("a config
/// typo must not silently disable a cost control") and stays silent for an
/// absent one, so [`crate::lenient::Tri`] — not `Option` — is what preserves
/// it. The int-vs-float distinction is preserved for the same reason: an
/// integer is clamped, a float is rounded.
///
/// All arithmetic (clamping, rounding, the `min(100)` on the warn threshold)
/// deliberately stays in the accessor.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BudgetSectionView {
    #[serde(deserialize_with = "crate::lenient::tri")]
    pub monthly_limit_cents: Tri<TomlNumber>,
    #[serde(deserialize_with = "crate::lenient::tri")]
    pub warn_threshold_percent: Tri<TomlNumber>,
    #[serde(deserialize_with = "crate::lenient::tri")]
    pub daily_cap_cents: Tri<TomlNumber>,
    /// `hard_stop` tolerates an integer (`1`/`0`) with a warning, so it needs
    /// [`TomlFlag`] rather than a plain `bool`.
    #[serde(deserialize_with = "crate::lenient::tri")]
    pub hard_stop: Tri<TomlFlag>,
}

/// The `[evolution]` keys the four GVU/AEE shadow readers consumed.
///
/// Narrow projection of [`crate::types::EvolutionConfig`] (required fields ⇒
/// no total parse). `evolution_view_matches_typed_evolution_config` locks the
/// two against drift the same way `ModelSectionView` is locked.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct EvolutionSectionView {
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub legacy_soul_evolution: Option<bool>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub strategy: Option<String>,
    /// Integer literals ARE accepted here — the opposite of `[fork]`'s budget
    /// quirk. See [`crate::lenient::opt_number_lossy`].
    #[serde(deserialize_with = "crate::lenient::opt_number_lossy")]
    pub aee_settle_hours: Option<f64>,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub noise_band: NoiseBandSection,
}

/// `agent.toml [skills]` — the curated recommendation list.
///
/// A section [`crate::types::AgentConfig`] does not model at all, so this view
/// has no typed twin to drift from. Read by
/// `duduclaw_agent::skill_recommend`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct SkillsSectionView {
    /// Missing / non-array ⇒ empty. Entry validation (`hub/slug` shape,
    /// path-traversal rejection) stays in the accessor — it is a safety
    /// policy, not a file-format rule.
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub recommended: Vec<String>,
}

/// `agent.toml [mcp]` — per-agent external MCP servers.
///
/// Another section `AgentConfig` does not model. Read by
/// `gateway::mcp_external`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct McpSectionView {
    #[serde(deserialize_with = "crate::lenient::lenient_vec")]
    pub external: Vec<ExternalMcpEntryView>,
}

/// One `[[mcp.external]]` entry, as raw as the reader it replaces.
///
/// Every semantic decision (preset resolution, the exactly-one-transport
/// rule, `env://` / `secret://` credential resolution, the fail-closed skip on
/// a wrong-typed tool filter) stays in `gateway::mcp_external` — this type
/// only stops that module from walking a `toml::Value` by hand.
///
/// The two tool-filter fields are [`Tri`], not `Vec<String>`: they are the one
/// place where "absent" and "present but wrong type" must NOT converge, since
/// converging them would turn a typo into a silently permissive allowlist.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ExternalMcpEntryView {
    /// Missing / wrong-typed ⇒ `None` ⇒ the accessor's `true` (opt-out flag).
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub enabled: Option<bool>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub preset: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub name: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub command: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub url: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub bearer_token: Option<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub args: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::string_map")]
    pub env: Vec<(String, String)>,
    #[serde(deserialize_with = "crate::lenient::string_map")]
    pub headers: Vec<(String, String)>,
    #[serde(deserialize_with = "crate::lenient::tri_string_vec")]
    pub allowed_tools: Tri<Vec<String>>,
    #[serde(deserialize_with = "crate::lenient::tri_string_vec")]
    pub denied_tools: Tri<Vec<String>>,
}

/// `agent.toml [goal_intent]` — per-agent override for the channel-side goal
/// intent router (P0, `commercial/docs/DESIGN-goal-intent-router-2026-08.md`).
///
/// Every field is optional: an absent key falls back to the global
/// `config.toml [goal_intent]` value, which itself falls back to the hard
/// default. The merge (global → per-agent, per-field) lives in
/// `duduclaw_gateway::goal_intent::GoalIntentConfig::resolve` — this type is
/// only the typed, no-shadow-reader parse point for the agent.toml side,
/// same discipline as every other section in this file.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct GoalIntentSectionView {
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub enabled: Option<bool>,
    /// `"auto"` / `"local"` / `"reply_tag"` / `"off"`. Unrecognized values are
    /// handled by the caller (falls back to `auto`), not here — this is a raw
    /// projection, same convention as `resume_on_restart` elsewhere.
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub mode: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub t_goal: Option<i64>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub t_gray: Option<i64>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub cooldown_minutes: Option<i64>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub daily_cap: Option<i64>,
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub suggest_ttl_minutes: Option<i64>,
}

/// Every `agent.toml` section the migrated readers touch, in one tolerant
/// parse.
///
/// **This deserialization never fails.** Each section goes through
/// [`crate::lenient::or_default`] and each field through the matching lenient
/// helper, so a malformed section, a wrong-typed key, or a missing table
/// degrades to that field's documented default — exactly what the
/// `value.get(..).and_then(..)` chains did. Only a file that is not valid TOML
/// at all yields `None` from [`load`], and that was already the shadow
/// readers' behavior.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct AgentTomlSections {
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub runtime: RuntimeSection,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub guardrails: GuardrailsSection,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub os_watch: OsWatchSection,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub fork: ForkSection,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub capabilities: CapabilitiesConfig,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub memory: MemoryConfig,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub model: ModelSectionView,
    /// `None` distinguishes "no `[agent]` table" from "an empty one" — see
    /// [`AgentSectionView`].
    #[serde(deserialize_with = "crate::lenient::opt")]
    pub agent: Option<AgentSectionView>,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub budget: BudgetSectionView,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub evolution: EvolutionSectionView,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub skills: SkillsSectionView,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub mcp: McpSectionView,
    #[serde(deserialize_with = "crate::lenient::or_default")]
    pub goal_intent: GoalIntentSectionView,
}

/// Parse the sections out of an `agent.toml` string.
///
/// Invalid TOML ⇒ all-defaults (the shadow readers' universal fail-safe).
pub fn parse(text: &str) -> AgentTomlSections {
    toml::from_str(text).unwrap_or_default()
}

/// Load the sections from `<agent_dir>/agent.toml` — or, when the agent has
/// a resolved preset binding, from the materialized `agent.resolved.toml`
/// artifact instead (WP-6F, agent presets P1).
///
/// # Why this is the preset integration point
///
/// This function is the **one** place all twelve formerly-shadow readers
/// (capability grants, guardrails, MCP external servers, GVU noise-band,
/// planner, research, skill recommendations, …) converge on. Before preset
/// support existed, the module docs above already called this out as the
/// risk: "any future assembly layer that resolves configuration before
/// handing it downstream (preset / kit overlays) would be silently bypassed
/// by every one of [the shadow readers]". Redirecting *here* — rather than
/// touching all twelve call sites — means every one of them sees
/// preset-resolved values automatically, with zero changes to any of those
/// modules (several of which are off-limits to unrelated concurrent work).
///
/// # Byte-identical fallback (R1.2)
///
/// An agent with no preset binding never has a `agent.resolved.toml` (see
/// `duduclaw-agent::registry::load_agent`, which only writes one on a
/// successful [`crate::preset::PresetResolution::Applied`] and deletes any
/// stale one otherwise) — so this reads exactly `agent.toml`, exactly as
/// before this module existed.
///
/// Reads on every call by design; see the module docs.
pub fn load(agent_dir: &Path) -> AgentTomlSections {
    if let Some(resolved) = resolved_override_path(agent_dir) {
        if let Ok(text) = std::fs::read_to_string(&resolved) {
            return parse(&text);
        }
    }
    match std::fs::read_to_string(agent_dir.join("agent.toml")) {
        Ok(text) => parse(&text),
        Err(_) => AgentTomlSections::default(),
    }
}

/// Env kill-switch for the minimal-context spawn optimization (WP-7A). Set to
/// `0`/`false`/`no`/`off` to disable globally, `1`/`true`/`yes`/`on` to
/// force-enable; unset defers to per-agent `[runtime] minimal_context`, then the
/// default (ON). An unrecognized value is ignored (falls through to the
/// per-agent / default resolution).
pub const ENV_MINIMAL_CONTEXT: &str = "DUDUCLAW_MINIMAL_CONTEXT";

/// Resolve whether minimal-context spawn flags (`--setting-sources
/// project,local` + a curated `--tools`) apply for the agent at `agent_dir`.
///
/// Precedence: env kill-switch ([`ENV_MINIMAL_CONTEXT`]) > `agent.toml
/// [runtime] minimal_context` > default (`true`). A `None` `agent_dir`
/// (agent-less system callers — GVU/utility, dashboard widgets) skips the
/// per-agent read and uses the env/default only.
///
/// Reads `agent.toml` on every call by design (immediate hot-reload; matches
/// [`load`]'s no-cache contract). The read is fail-safe: an absent/malformed
/// file yields `None` for the field → the default (ON).
pub fn resolve_minimal_context(agent_dir: Option<&Path>) -> bool {
    if let Ok(v) = std::env::var(ENV_MINIMAL_CONTEXT) {
        match v.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => return false,
            "1" | "true" | "yes" | "on" => return true,
            _ => {}
        }
    }
    if let Some(dir) = agent_dir {
        if let Some(v) = load(dir).runtime.minimal_context {
            return v;
        }
    }
    true
}

/// `<home>/agent_resolved/<agent_id>.toml` for the agent at `agent_dir`, or
/// `None` when `agent_dir` is not the standard `<home>/agents/<id>` layout
/// (ephemeral scaffolds, test fixtures) — see
/// `crate::preset::agent_home_dir` for why that case is deliberately
/// unsupported rather than guessed.
fn resolved_override_path(agent_dir: &Path) -> Option<std::path::PathBuf> {
    let home = crate::preset::agent_home_dir(agent_dir)?;
    let agent_id = agent_dir.file_name()?.to_str()?;
    Some(crate::preset::agent_resolved_path(&home, agent_id))
}

/// Load the sections for `<home_dir>/agents/<agent_id>/agent.toml`.
///
/// Convenience for the MCP-server callers that hold a home dir + agent id
/// rather than an agent dir.
pub fn load_for_agent(home_dir: &Path, agent_id: &str) -> AgentTomlSections {
    load(&home_dir.join("agents").join(agent_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sections `AgentConfig` requires, with no migrated keys in them.
    /// Kept minimal-but-valid so the tests below exercise the migrated
    /// sections rather than the pre-existing schema.
    const BASE: &str = r#"
[agent]
name = "a"
display_name = "A"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = ""

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
max_gvu_generations = 0
observation_period_hours = 24.0
skill_token_budget = 500
max_active_skills = 2
"#;

    /// `BASE` plus a `[model]` section with no migrated keys.
    fn minimal() -> String {
        format!("{BASE}\n[model]\npreferred = \"m\"\nfallback = \"f\"\naccount_pool = []\n")
    }

    /// `BASE` plus every migrated section, populated.
    fn full() -> String {
        format!("{BASE}{EXTRAS}")
    }

    const EXTRAS: &str = r#"
[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = []
utility = "u-model"
fallbacks = ["openai/gpt-5.4", "  ", "compat:x/y"]
standard = "mid"
delegation_routing = true

[runtime]
provider = "codex"
fallback = "gemini"
pty_pool_enabled = true

[guardrails]
enabled = true
redact_pii = true

[os_watch]
paths = ["~/x"]
debounce_ms = 900
goal_template = "do {path}"

[fork]
enabled = true
max_branches = 9

[capabilities]
os_native = true
computer_use_mode = "native"
scoped_tools = ["a", "b"]
grant_ttl_secs = 42
autonomy_level = "operator"

[[capabilities.policy]]
tool = "shell_exec"
effect = "forbid"

[memory]
decision_continuity = true
decision_ttl_days = 3
"#;

    #[test]
    fn full_config_parses_every_migrated_section() {
        let s = parse(&full());
        assert_eq!(s.runtime.provider.as_deref(), Some("codex"));
        assert_eq!(s.runtime.fallback.as_deref(), Some("gemini"));
        assert_eq!(s.runtime.pty_pool_enabled, Some(true));
        assert!(s.guardrails.enabled && s.guardrails.redact_pii);
        assert_eq!(s.os_watch.paths, vec!["~/x".to_string()]);
        assert_eq!(s.os_watch.debounce_ms, Some(900));
        assert_eq!(s.fork.max_branches, Some(9));
        assert_eq!(s.capabilities.grant_ttl_secs, Some(42));
        assert_eq!(s.capabilities.autonomy_level.as_deref(), Some("operator"));
        assert_eq!(s.memory.decision_ttl_days, Some(3));
        assert_eq!(s.model.utility.as_deref(), Some("u-model"));
    }

    /// Drift guard: the narrow [`ModelSectionView`] and the typed
    /// [`crate::types::ModelConfig`] must read the same `[model]` keys with the
    /// same results. If someone adds a key to one and forgets the other, the
    /// migrated readers and the registry would disagree about the same file —
    /// the exact two-schema split this module exists to end.
    #[test]
    fn model_view_matches_typed_model_config() {
        let text = full();
        let full_cfg: crate::types::AgentConfig = toml::from_str(&text).unwrap();
        let view = parse(&text).model;
        assert_eq!(view.preferred.as_deref(), Some(full_cfg.model.preferred.as_str()));
        assert_eq!(view.utility.as_deref(), Some(full_cfg.model.utility.as_str()));
        assert_eq!(view.fallbacks, full_cfg.model.fallbacks);
        assert_eq!(view.standard, full_cfg.model.standard);
        assert_eq!(view.delegation_routing, full_cfg.model.delegation_routing);
    }

    /// The same file must resolve identically through the strict registry path
    /// and the tolerant per-call path — otherwise the two schemas are still
    /// two schemas.
    #[test]
    fn strict_and_tolerant_paths_agree_on_every_section() {
        let text = full();
        let strict: crate::types::AgentConfig = toml::from_str(&text).unwrap();
        let loose = parse(&text);
        assert_eq!(strict.runtime, loose.runtime);
        assert_eq!(strict.guardrails, loose.guardrails);
        assert_eq!(strict.os_watch, loose.os_watch);
        assert_eq!(strict.fork, loose.fork);
        assert_eq!(strict.capabilities.scoped_tools, loose.capabilities.scoped_tools);
        assert_eq!(strict.memory.decision_ttl_days, loose.memory.decision_ttl_days);

        // The formerly-typed half of `[capabilities]` must survive the
        // tolerant path too — including the nested enum-bearing `policy`
        // array-of-tables, which is what the lenient buffering has to carry
        // through unchanged. `[capabilities]` straddling both schemas was R2's
        // sharpest example; this is the assertion that it no longer does.
        assert!(loose.capabilities.os_native);
        assert_eq!(
            loose.capabilities.computer_use_mode,
            strict.capabilities.computer_use_mode
        );
        // `ToolPolicy` has no `PartialEq`, so compare field-wise rather than
        // deriving one just for a test.
        assert_eq!(loose.capabilities.policy.len(), strict.capabilities.policy.len());
        assert_eq!(loose.capabilities.policy.len(), 1);
        assert_eq!(loose.capabilities.policy[0].tool, "shell_exec");
        assert_eq!(loose.capabilities.policy[0].effect, PolicyEffect::Forbid);
        assert!(loose.capabilities.policy[0].when.is_empty());
    }

    /// WP-10A: `[capabilities] git_credentials` reads through the shared
    /// [`CapabilitiesConfig`] type on both entry points — the strict
    /// `AgentConfig` registry path and this module's tolerant per-call
    /// `AgentTomlSections` path — with no separate shadow reader. Absent ⇒
    /// `false` on both; an explicit `true` parses through both.
    #[test]
    fn git_credentials_defaults_false_and_parses_true_on_both_paths() {
        let absent = parse("[capabilities]\nos_native = true\n");
        assert!(!absent.capabilities.git_credentials);

        // `full()` already builds a fully valid `agent.toml` (required
        // `[agent]`/`[model]`/... sections) — inject the new key into its
        // existing `[capabilities]` table rather than hand-rolling a second
        // minimal document that would have to track every required field.
        let text = full().replace(
            "[capabilities]\nos_native = true",
            "[capabilities]\nos_native = true\ngit_credentials = true",
        );
        let strict: crate::types::AgentConfig = toml::from_str(&text).unwrap();
        let loose = parse(&text);
        assert!(strict.capabilities.git_credentials);
        assert!(loose.capabilities.git_credentials);
    }

    #[test]
    fn empty_and_invalid_input_both_yield_defaults() {
        let empty = parse("");
        let broken = parse("this is not toml {{{");
        assert_eq!(empty.runtime, RuntimeSection::default());
        assert_eq!(broken.runtime, RuntimeSection::default());
        assert_eq!(broken.guardrails, GuardrailsSection::default());
        assert_eq!(broken.fork, ForkSection::default());
        assert!(broken.capabilities.scoped_tools.is_empty());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = std::env::temp_dir().join("duduclaw-agent-toml-absent-test");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join("agent.toml"));
        let s = load(&dir);
        assert_eq!(s.runtime, RuntimeSection::default());
        assert_eq!(s.guardrails, GuardrailsSection::default());
    }

    /// WP-7A: minimal-context resolution. Default ON; a per-agent explicit
    /// `false` opts out; `None` agent_dir uses the default. The env kill-switch
    /// is process-global, so this test only exercises the agent.toml/default
    /// paths and skips them when the env var happens to be set in the runner.
    #[test]
    fn resolve_minimal_context_agent_toml_and_default() {
        if std::env::var(ENV_MINIMAL_CONTEXT).is_ok() {
            return; // env override is authoritative; skip the toml-path asserts
        }
        // No agent_dir → default ON.
        assert!(resolve_minimal_context(None));

        let dir = tempfile::tempdir().unwrap();
        // Empty agent.toml → field absent → default ON.
        std::fs::write(dir.path().join("agent.toml"), "[runtime]\nprovider = \"claude\"\n").unwrap();
        assert!(resolve_minimal_context(Some(dir.path())));

        // Explicit opt-out.
        std::fs::write(
            dir.path().join("agent.toml"),
            "[runtime]\nminimal_context = false\n",
        )
        .unwrap();
        assert!(!resolve_minimal_context(Some(dir.path())));

        // Explicit opt-in.
        std::fs::write(
            dir.path().join("agent.toml"),
            "[runtime]\nminimal_context = true\n",
        )
        .unwrap();
        assert!(resolve_minimal_context(Some(dir.path())));
    }

    /// WP-6F (agent presets P1): when `agent.resolved.toml` exists next to a
    /// standard `<home>/agents/<id>/agent.toml`, `load` must prefer it — this
    /// is the single choke point every shadow reader is redirected through.
    #[test]
    fn load_prefers_the_resolved_artifact_when_present() {
        // `tempfile::tempdir()` (not a fixed `std::env::temp_dir()` path):
        // a hardcoded name here would risk colliding with a concurrently
        // running test under cargo's default parallel test execution — this
        // module's own `missing_file_yields_defaults` predates that lesson,
        // but every new test in this codebase should get a private dir.
        let home_tmp = tempfile::tempdir().unwrap();
        let home = home_tmp.path();
        let agent_dir = home.join("agents").join("bob");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[runtime]\nprovider = \"claude\"\n").unwrap();

        // No resolved artifact yet ⇒ reads agent.toml (R1.2 fallback).
        assert_eq!(load(&agent_dir).runtime.provider.as_deref(), Some("claude"));

        let resolved_path = crate::preset::agent_resolved_path(home, "bob");
        std::fs::create_dir_all(resolved_path.parent().unwrap()).unwrap();
        std::fs::write(&resolved_path, "[runtime]\nprovider = \"codex\"\n").unwrap();

        assert_eq!(
            load(&agent_dir).runtime.provider.as_deref(),
            Some("codex"),
            "must prefer the resolved artifact once it exists"
        );
    }

    /// Non-standard layouts (ephemeral scaffolds, ad-hoc dirs) must not guess
    /// a resolved path — falls straight through to the raw `agent.toml`.
    #[test]
    fn load_ignores_resolved_lookup_for_non_standard_layouts() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("agent.toml"), "[runtime]\nprovider = \"gemini\"\n").unwrap();
        assert_eq!(load(dir).runtime.provider.as_deref(), Some("gemini"));
    }

    /// A wrong-typed key must not fail the parse — before this migration such
    /// a key was silently ignored, and turning it into a hard error would make
    /// the agent disappear from the registry entirely.
    #[test]
    fn wrong_typed_keys_never_fail_the_parse() {
        let s = parse(
            r#"
[runtime]
provider = 42

[guardrails]
enabled = "yes"

[capabilities]
scoped_tools = "not-an-array"

[fork]
max_branches = "many"
"#,
        );
        assert_eq!(s.runtime.provider, None);
        assert!(!s.guardrails.enabled);
        assert!(s.capabilities.scoped_tools.is_empty());
        assert_eq!(s.fork.max_branches, None);
    }

    /// The same tolerance must hold for `AgentConfig` itself, since it now
    /// owns these sections: a `[runtime] provider = 42` typo used to be
    /// ignored while the agent loaded fine, and must stay that way.
    #[test]
    fn wrong_typed_new_section_keys_do_not_break_agent_config() {
        let text = full().replace("provider = \"codex\"", "provider = 42");
        let cfg: crate::types::AgentConfig =
            toml::from_str(&text).expect("a typo in a migrated section must not lose the agent");
        assert_eq!(cfg.runtime.provider, None);
    }

    /// Round-trip shape guard: a config that never wrote the migrated sections
    /// must not gain them when `AgentConfig` is serialized back over the file
    /// (which `agent_update` does). Materializing a default is how a
    /// "missing ⇒ X" gate silently becomes "explicit ⇒ X" somewhere else.
    #[test]
    fn absent_sections_are_not_materialized_on_rewrite() {
        let cfg: crate::types::AgentConfig = toml::from_str(&minimal()).unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        for section in ["[runtime]", "[guardrails]", "[os_watch]", "[fork]"] {
            assert!(
                !out.contains(section),
                "{section} must not be materialized on rewrite:\n{out}"
            );
        }
        for key in [
            "scoped_tools",
            "grant_ttl_secs",
            "approval_required_tools",
            "irreversible_tools",
            "maybe_irreversible_tools",
            "autonomy_level",
            "fallbacks",
            "standard",
            "delegation_routing",
            "decision_continuity",
            "decision_ttl_days",
        ] {
            assert!(!out.contains(key), "{key} must not be materialized:\n{out}");
        }
    }

    // ── 2nd-pass views: drift guards + presence semantics ────────────────

    /// Drift guard, same role as `model_view_matches_typed_model_config`: the
    /// narrow [`EvolutionSectionView`] and the typed
    /// [`crate::types::EvolutionConfig`] must read the same `[evolution]` keys
    /// with the same results, including the two OPPOSITE numeric quirks
    /// (`aee_settle_hours` accepts an integer, `noise_band.*` does not).
    #[test]
    fn evolution_view_matches_typed_evolution_config() {
        let text = format!(
            "{BASE}\n[model]\npreferred = \"m\"\nfallback = \"f\"\naccount_pool = []\n\
             [evolution.extra_ignored]\nx = 1\n"
        )
        .replace(
            "skill_token_budget = 500",
            "skill_token_budget = 500\n\
             legacy_soul_evolution = true\n\
             strategy = \"innovate\"\n\
             aee_settle_hours = 48\n",
        )
            + "\n[evolution.noise_band]\ncases = 0.02\njudge = 3\n";

        let full_cfg: crate::types::AgentConfig = toml::from_str(&text).unwrap();
        let view = parse(&text).evolution;
        assert_eq!(view.legacy_soul_evolution, full_cfg.evolution.legacy_soul_evolution);
        assert_eq!(view.strategy, full_cfg.evolution.strategy);
        assert_eq!(view.aee_settle_hours, full_cfg.evolution.aee_settle_hours);
        assert_eq!(view.noise_band, full_cfg.evolution.noise_band);

        // And the quirks themselves, spelled out.
        assert_eq!(view.aee_settle_hours, Some(48.0), "integer literal accepted");
        assert_eq!(view.noise_band.cases, Some(0.02));
        assert_eq!(
            view.noise_band.judge, None,
            "an integer literal is IGNORED for noise_band (as_float-only quirk)"
        );
    }

    /// The `[agent]` section's *presence* is load-bearing for two readers
    /// (`export_to::read_agent`, `budget::agent_display_name`), so the view
    /// must distinguish "no table" from "an empty table". A wrong-typed
    /// section reads as absent, matching `as_table()` returning `None`.
    #[test]
    fn agent_section_presence_is_distinguishable_from_emptiness() {
        assert!(parse("").agent.is_none(), "no table ⇒ None");
        assert!(
            parse("agent = \"scalar\"\n").agent.is_none(),
            "wrong-typed section reads as absent, like as_table() → None"
        );
        assert!(parse("this is not toml {{{").agent.is_none());

        let empty = parse("[agent]\n").agent.expect("an empty table is still a table");
        assert_eq!(empty, AgentSectionView::default());

        let full = parse(
            "[agent]\nname = \"a\"\ndisplay_name = \"A\"\nrole = \"specialist\"\n\
             status = \"active\"\nreports_to = \"boss\"\nicon = \"x\"\ntrigger = \"@a\"\n",
        )
        .agent
        .unwrap();
        assert_eq!(full.name.as_deref(), Some("a"));
        assert_eq!(full.status.as_deref(), Some("active"));
        assert_eq!(full.reports_to.as_deref(), Some("boss"));

        // A wrong-typed key inside the table degrades to None, never fatal.
        let typo = parse("[agent]\nname = 42\ndisplay_name = \"A\"\n").agent.unwrap();
        assert_eq!(typo.name, None);
        assert_eq!(typo.display_name.as_deref(), Some("A"));
    }

    /// `[budget]`'s view is three-state because the accessor warns only for
    /// the wrong-typed case, and keeps int/float apart because the accessor
    /// clamps one and rounds the other.
    #[test]
    fn budget_view_keeps_absent_wrong_typed_int_and_float_apart() {
        let absent = parse("").budget;
        assert_eq!(absent.monthly_limit_cents, Tri::Absent);
        assert_eq!(absent.hard_stop, Tri::Absent);

        let b = parse(
            "[budget]\nmonthly_limit_cents = 5000\ndaily_cap_cents = 100.0\n\
             warn_threshold_percent = \"80\"\nhard_stop = 1\n",
        )
        .budget;
        assert_eq!(b.monthly_limit_cents, Tri::Value(TomlNumber::Int(5000)));
        assert_eq!(b.daily_cap_cents, Tri::Value(TomlNumber::Float(100.0)));
        assert_eq!(b.warn_threshold_percent, Tri::WrongType);
        assert_eq!(b.hard_stop, Tri::Value(TomlFlag::Int(1)));

        // A scalar `budget` and an absent one converge on all-Absent, which is
        // what makes the accessor's single "inert" result correct for both.
        assert_eq!(parse("budget = 1\n").budget, BudgetSectionView::default());
    }

    /// `[[mcp.external]]`'s tool filters are the one place where absent and
    /// wrong-typed must NOT converge — see `ExternalMcpEntryView`.
    #[test]
    fn mcp_external_view_flags_wrong_typed_tool_filters() {
        assert!(parse("").mcp.external.is_empty());
        assert!(parse("[mcp]\nexternal = \"nope\"\n").mcp.external.is_empty());

        let e = &parse(
            "[[mcp.external]]\nname = \"s\"\ncommand = \"npx\"\n\
             args = [\"-y\", 3, \"pkg\"]\n\
             env = { A = \"1\", B = 2 }\n\
             allowed_tools = [\"t\", 9]\n\
             denied_tools = \"oops\"\n",
        )
        .mcp
        .external[0];
        assert_eq!(e.name.as_deref(), Some("s"));
        assert_eq!(e.enabled, None, "absent ⇒ None ⇒ the accessor's `true`");
        assert_eq!(e.args, vec!["-y".to_string(), "pkg".to_string()]);
        assert_eq!(e.env, vec![("A".to_string(), "1".to_string())]);
        assert_eq!(
            e.allowed_tools,
            Tri::Value(vec!["t".to_string()]),
            "a mixed array is filtered, not flagged"
        );
        assert_eq!(
            e.denied_tools,
            Tri::WrongType,
            "a scalar where an array belongs must stay distinguishable"
        );

        // A non-table element degrades to an all-default entry rather than
        // failing the whole list and losing its well-formed siblings.
        let entries = parse("[mcp]\nexternal = [\"scalar\", { name = \"ok\" }]\n").mcp.external;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], ExternalMcpEntryView::default());
        assert_eq!(entries[1].name.as_deref(), Some("ok"));
    }

    /// `[skills]` has no typed twin on `AgentConfig`, so this only pins the
    /// leniency: absent / non-array / mixed-array all behave as the raw walk
    /// did.
    #[test]
    fn skills_view_is_lenient_about_everything() {
        assert!(parse("").skills.recommended.is_empty());
        assert!(parse("[skills]\n").skills.recommended.is_empty());
        assert!(parse("skills = 1\n").skills.recommended.is_empty());
        assert!(parse("[skills]\nrecommended = \"x\"\n").skills.recommended.is_empty());
        assert_eq!(
            parse("[skills]\nrecommended = [\"a\", 2, \"b\"]\n").skills.recommended,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    /// `allowed_tools` / `denied_tools` moved onto `lenient::string_vec` so
    /// the tolerant and strict paths agree — and, more importantly, so one
    /// stray non-string element can no longer fail `AgentConfig` outright and
    /// delete the agent from the registry.
    #[test]
    fn tool_allow_deny_lists_drop_bad_elements_instead_of_losing_the_agent() {
        let text = format!(
            "{}\n[capabilities]\nallowed_tools = [\"Bash\", 7]\ndenied_tools = \"WebFetch\"\n",
            minimal()
        );
        let loose = parse(&text);
        assert_eq!(loose.capabilities.allowed_tools, vec!["Bash".to_string()]);
        assert!(loose.capabilities.denied_tools.is_empty(), "non-array ⇒ empty");

        let strict: crate::types::AgentConfig =
            toml::from_str(&text).expect("a stray element must not lose the agent");
        assert_eq!(strict.capabilities.allowed_tools, loose.capabilities.allowed_tools);
        assert_eq!(strict.capabilities.denied_tools, loose.capabilities.denied_tools);
    }

    /// `auto_approve_install` is fail-CLOSED while its section-mates are
    /// fail-safe-empty. Pin the asymmetry at the schema level too.
    #[test]
    fn auto_approve_install_is_none_unless_explicitly_written() {
        assert_eq!(parse("").capabilities.auto_approve_install, None);
        assert_eq!(parse("[capabilities]\n").capabilities.auto_approve_install, None);
        assert_eq!(
            parse("[capabilities]\nauto_approve_install = \"true\"\n")
                .capabilities
                .auto_approve_install,
            None,
            "wrong type ⇒ None ⇒ the accessor's false ⇒ gate stays on"
        );
        assert_eq!(
            parse("[capabilities]\nauto_approve_install = true\n")
                .capabilities
                .auto_approve_install,
            Some(true)
        );
    }

    /// The 2nd-pass keys must not be materialized on rewrite either — same
    /// contract as `absent_sections_are_not_materialized_on_rewrite`.
    #[test]
    fn second_pass_keys_are_not_materialized_on_rewrite() {
        let cfg: crate::types::AgentConfig = toml::from_str(&minimal()).unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        for key in [
            "auto_approve_install",
            "legacy_soul_evolution",
            "aee_settle_hours",
            "noise_band",
        ] {
            assert!(!out.contains(key), "{key} must not be materialized:\n{out}");
        }
    }

    /// The converse: a config that DID write these sections must survive the
    /// same round-trip. Before this migration `agent_update` re-serialized
    /// `AgentConfig` over `agent.toml` and dropped every untyped section — a
    /// `[runtime] provider = "codex"` agent could be silently reset to Claude
    /// by an unrelated icon edit.
    #[test]
    fn present_sections_survive_the_agent_config_rewrite() {
        let cfg: crate::types::AgentConfig = toml::from_str(&full()).unwrap();
        let out = toml::to_string_pretty(&cfg).unwrap();
        let back: crate::types::AgentConfig = toml::from_str(&out).unwrap();
        assert_eq!(back.runtime.provider.as_deref(), Some("codex"));
        assert_eq!(back.guardrails, cfg.guardrails);
        assert_eq!(back.os_watch, cfg.os_watch);
        assert_eq!(back.fork, cfg.fork);
        assert_eq!(back.capabilities.scoped_tools, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(back.memory.decision_ttl_days, Some(3));
        assert_eq!(back.model.standard.as_deref(), Some("mid"));
    }
}
