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

use crate::types::{
    CapabilitiesConfig, ForkSection, GuardrailsSection, MemoryConfig, OsWatchSection,
    RuntimeSection,
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
}

/// Parse the sections out of an `agent.toml` string.
///
/// Invalid TOML ⇒ all-defaults (the shadow readers' universal fail-safe).
pub fn parse(text: &str) -> AgentTomlSections {
    toml::from_str(text).unwrap_or_default()
}

/// Load the sections from `<agent_dir>/agent.toml`.
///
/// A missing / unreadable / malformed file yields all-defaults — every shadow
/// reader this replaces degraded the same way, and none of them distinguished
/// "no file" from "no key".
///
/// Reads on every call by design; see the module docs.
pub fn load(agent_dir: &Path) -> AgentTomlSections {
    match std::fs::read_to_string(agent_dir.join("agent.toml")) {
        Ok(text) => parse(&text),
        Err(_) => AgentTomlSections::default(),
    }
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
