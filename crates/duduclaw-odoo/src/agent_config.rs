//! RFC-21 §2: per-agent Odoo configuration overrides.
//!
//! Layers `agent.toml [odoo]` on top of the global `config.toml [odoo]` so
//! every agent can connect to Odoo as its own service account at its own
//! permission level. Cross-project data isolation must be enforced by Odoo's
//! ACL, not by SOUL.md self-restraint.
//!
//! ## `agent.toml` schema (all fields optional)
//!
//! ```toml
//! [odoo]
//! profile         = "alpha"            # arbitrary label; appears in audit log
//! username        = "agent_alpha_pm"   # service account
//! api_key_enc     = "<encrypted>"      # AES-256-GCM via duduclaw-security
//! allowed_models  = ["crm.lead", "sale.order"]
//! allowed_actions = ["read", "search", "write:crm.lead"]
//! company_ids     = [1, 2]
//! ```
//!
//! Without an `[odoo]` block in `agent.toml`, the agent falls back to the
//! global `config.toml [odoo]` exactly as before — no regression.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::OdooConfig;

/// Per-agent override block parsed from `agent.toml [odoo]`.
///
/// Every field is optional — present fields override the global default;
/// absent fields inherit. `profile` defaults to `"default"` so deployments
/// without overrides work unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentOdooConfig {
    /// Audit-log label for this connection. Falls back to `"default"` so
    /// agents without explicit `[odoo].profile` still produce a stable
    /// pool key.
    pub profile: Option<String>,
    /// Service-account username. When set, replaces `OdooConfig::username`.
    pub username: Option<String>,
    /// Encrypted API key. When set, replaces `OdooConfig::api_key_enc`.
    #[serde(skip_serializing)]
    pub api_key_enc: Option<String>,
    /// Encrypted password. When set, replaces `OdooConfig::password_enc`.
    #[serde(skip_serializing)]
    pub password_enc: Option<String>,
    /// Whitelist of Odoo models this agent may touch via *any* MCP tool.
    /// Empty / absent ⇒ no model restriction (defer to Odoo ACL).
    pub allowed_models: Vec<String>,
    /// Whitelist of action verbs this agent may perform.
    ///
    /// Granular forms: `"read"`, `"search"`, `"create"`, `"write"`,
    /// `"execute"`. Per-model qualification: `"write:crm.lead"` allows
    /// only `write` on `crm.lead`. Empty / absent ⇒ no action restriction.
    pub allowed_actions: Vec<String>,
    /// Multi-company scope. When non-empty, queries are narrowed to these
    /// `res.company` ids. Empty ⇒ inherit Odoo user's default companies.
    pub company_ids: Vec<i32>,
}

impl AgentOdooConfig {
    /// Stable pool key for this configuration: `profile` if set, else
    /// `"default"`. Used by [`OdooConfigResolver::pool_key_for`] and the
    /// connector pool in `duduclaw-cli`.
    pub fn profile_or_default(&self) -> &str {
        self.profile.as_deref().unwrap_or("default")
    }

    /// Whether this override is meaningfully populated (vs. an empty/all-
    /// default block from `#[serde(default)]`).
    pub fn is_empty(&self) -> bool {
        self.profile.is_none()
            && self.username.is_none()
            && self.api_key_enc.is_none()
            && self.password_enc.is_none()
            && self.allowed_models.is_empty()
            && self.allowed_actions.is_empty()
            && self.company_ids.is_empty()
    }

    /// Parse from a `agent.toml` table — looks for the `[odoo]` section.
    /// Returns `None` when the section is absent (the common case).
    pub fn from_agent_toml(table: &toml::Table) -> Option<Self> {
        let block = table.get("odoo")?;
        // Use try_into so a malformed [odoo] block surfaces as None; the
        // gateway logs the failure and falls back to the global config.
        let cfg: AgentOdooConfig = block.clone().try_into().ok()?;
        if cfg.is_empty() { None } else { Some(cfg) }
    }

    /// Verb part of an action token (`"write:crm.lead"` → `"write"`).
    fn verb_of(action: &str) -> &str {
        action.split(':').next().unwrap_or(action)
    }

    /// Model qualifier of an action token (`"write:crm.lead"` → `Some("crm.lead")`,
    /// `"read"` → `None`).
    fn model_of(action: &str) -> Option<&str> {
        action.split_once(':').map(|(_, m)| m)
    }

    /// Check whether `(action, model)` is permitted by `allowed_actions`.
    /// An empty `allowed_actions` is permissive — see field doc.
    pub fn permits(&self, action: &str, model: &str) -> bool {
        if self.allowed_actions.is_empty() {
            return true;
        }
        self.allowed_actions.iter().any(|tok| {
            let verb = Self::verb_of(tok);
            if verb != action {
                return false;
            }
            match Self::model_of(tok) {
                None => true,           // bare verb → all models
                Some(m) => m == model,  // qualified verb → specific model
            }
        })
    }

    /// Check whether `model` is permitted by `allowed_models`.
    pub fn permits_model(&self, model: &str) -> bool {
        self.allowed_models.is_empty() || self.allowed_models.iter().any(|m| m == model)
    }
}

/// Resolves `(global, per-agent)` into the right configuration to hand to
/// the connector pool.
///
/// This intentionally does **not** rebuild a fully-merged `OdooConfig`
/// because the encrypted credentials live in the global config and are
/// decrypted lazily; the resolver only carries the bits the pool needs to
/// look up the right slot.
#[derive(Debug, Default)]
pub struct OdooConfigResolver {
    global: OdooConfig,
    per_agent: HashMap<String, AgentOdooConfig>,
}

impl OdooConfigResolver {
    pub fn new(global: OdooConfig) -> Self {
        Self { global, per_agent: HashMap::new() }
    }

    /// Register an `agent.toml [odoo]` override for `agent_id`.
    pub fn upsert_agent(&mut self, agent_id: impl Into<String>, cfg: AgentOdooConfig) {
        self.per_agent.insert(agent_id.into(), cfg);
    }

    pub fn global(&self) -> &OdooConfig {
        &self.global
    }

    /// Replace the global `[odoo]` block. Does **not** clear per-agent
    /// overrides — operators that hot-reload `config.toml` typically want
    /// existing `agent.toml [odoo]` overrides preserved.
    pub fn set_global(&mut self, global: OdooConfig) {
        self.global = global;
    }

    /// Returns the per-agent override if present, else `None` (caller
    /// should fall back to global).
    pub fn for_agent(&self, agent_id: &str) -> Option<&AgentOdooConfig> {
        self.per_agent.get(agent_id)
    }

    /// Stable `(agent_id, profile)` pool key used by the connector pool.
    /// Agents without an override share the `"default"` profile; agents
    /// with an override but no explicit `profile` field also use `"default"`
    /// — they're isolated from each other by `agent_id` alone.
    pub fn pool_key_for(&self, agent_id: &str) -> (String, String) {
        let profile = self
            .per_agent
            .get(agent_id)
            .map(|c| c.profile_or_default())
            .unwrap_or("default")
            .to_string();
        (agent_id.to_string(), profile)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_block_is_detected() {
        assert!(AgentOdooConfig::default().is_empty());
        assert!(!AgentOdooConfig {
            profile: Some("a".into()),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn profile_or_default_falls_back() {
        let bare = AgentOdooConfig::default();
        assert_eq!(bare.profile_or_default(), "default");
        let named = AgentOdooConfig { profile: Some("alpha".into()), ..Default::default() };
        assert_eq!(named.profile_or_default(), "alpha");
    }

    #[test]
    fn permits_with_empty_allowed_actions_is_permissive() {
        let cfg = AgentOdooConfig::default();
        assert!(cfg.permits("read", "any.model"));
        assert!(cfg.permits("execute", "sale.order"));
    }

    #[test]
    fn permits_recognises_bare_verb_as_global() {
        let cfg = AgentOdooConfig {
            allowed_actions: vec!["read".into(), "search".into()],
            ..Default::default()
        };
        assert!(cfg.permits("read", "crm.lead"));
        assert!(cfg.permits("search", "sale.order"));
        assert!(!cfg.permits("write", "crm.lead"));
        assert!(!cfg.permits("execute", "sale.order"));
    }

    #[test]
    fn permits_recognises_qualified_verb() {
        let cfg = AgentOdooConfig {
            allowed_actions: vec!["read".into(), "write:crm.lead".into()],
            ..Default::default()
        };
        // bare `read` → all models
        assert!(cfg.permits("read", "crm.lead"));
        assert!(cfg.permits("read", "sale.order"));
        // qualified `write` → only crm.lead
        assert!(cfg.permits("write", "crm.lead"));
        assert!(!cfg.permits("write", "sale.order"));
    }

    #[test]
    fn permits_model_with_empty_list_is_permissive() {
        let cfg = AgentOdooConfig::default();
        assert!(cfg.permits_model("anything"));
    }

    #[test]
    fn permits_model_filters_when_listed() {
        let cfg = AgentOdooConfig {
            allowed_models: vec!["crm.lead".into(), "sale.order".into()],
            ..Default::default()
        };
        assert!(cfg.permits_model("crm.lead"));
        assert!(cfg.permits_model("sale.order"));
        assert!(!cfg.permits_model("res.partner"));
    }

    #[test]
    fn from_agent_toml_returns_none_when_no_block() {
        let raw = "[agent]\nname = \"foo\"\n";
        let table: toml::Table = raw.parse().unwrap();
        assert!(AgentOdooConfig::from_agent_toml(&table).is_none());
    }

    #[test]
    fn from_agent_toml_parses_minimal_block() {
        let raw = "\
            [odoo]\n\
            profile = \"alpha\"\n\
            username = \"alpha_user\"\n\
        ";
        let table: toml::Table = raw.parse().unwrap();
        let cfg = AgentOdooConfig::from_agent_toml(&table).expect("present");
        assert_eq!(cfg.profile.as_deref(), Some("alpha"));
        assert_eq!(cfg.username.as_deref(), Some("alpha_user"));
        assert!(cfg.allowed_models.is_empty());
    }

    #[test]
    fn from_agent_toml_parses_full_block() {
        let raw = "\
            [odoo]\n\
            profile = \"alpha\"\n\
            username = \"alpha_user\"\n\
            allowed_models = [\"crm.lead\", \"sale.order\"]\n\
            allowed_actions = [\"read\", \"write:crm.lead\"]\n\
            company_ids = [1, 2]\n\
        ";
        let table: toml::Table = raw.parse().unwrap();
        let cfg = AgentOdooConfig::from_agent_toml(&table).expect("present");
        assert_eq!(cfg.allowed_models.len(), 2);
        assert_eq!(cfg.allowed_actions.len(), 2);
        assert_eq!(cfg.company_ids, vec![1, 2]);
    }

    #[test]
    fn from_agent_toml_returns_none_for_empty_block() {
        // Operator wrote `[odoo]` then nothing — should be treated as absent
        // so we don't pin a vacuous override.
        let raw = "[odoo]\n";
        let table: toml::Table = raw.parse().unwrap();
        assert!(AgentOdooConfig::from_agent_toml(&table).is_none());
    }

    #[test]
    fn from_agent_toml_returns_none_for_malformed_block() {
        // `allowed_models` should be a list of strings.
        let raw = "[odoo]\nallowed_models = 42\n";
        let table: toml::Table = raw.parse().unwrap();
        assert!(AgentOdooConfig::from_agent_toml(&table).is_none());
    }

    #[test]
    fn resolver_returns_none_for_unregistered_agent() {
        let resolver = OdooConfigResolver::new(OdooConfig::default());
        assert!(resolver.for_agent("ghost").is_none());
    }

    #[test]
    fn resolver_pool_key_uses_default_profile_when_unregistered() {
        let resolver = OdooConfigResolver::new(OdooConfig::default());
        assert_eq!(
            resolver.pool_key_for("agnes"),
            ("agnes".to_string(), "default".to_string())
        );
    }

    #[test]
    fn resolver_pool_key_uses_explicit_profile_when_set() {
        let mut resolver = OdooConfigResolver::new(OdooConfig::default());
        resolver.upsert_agent(
            "alpha-pm",
            AgentOdooConfig { profile: Some("alpha".into()), ..Default::default() },
        );
        resolver.upsert_agent(
            "beta-pm",
            AgentOdooConfig::default(), // override but no profile
        );
        assert_eq!(
            resolver.pool_key_for("alpha-pm"),
            ("alpha-pm".to_string(), "alpha".to_string())
        );
        assert_eq!(
            resolver.pool_key_for("beta-pm"),
            ("beta-pm".to_string(), "default".to_string())
        );
    }
}
