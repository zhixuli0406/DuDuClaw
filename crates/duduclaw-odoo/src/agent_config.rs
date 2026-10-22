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
//! url             = "https://erp.example.com"  # own Odoo server (admin-only, SSRF-checked)
//! db              = "alpha_prod"       # database on that server
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

/// Error text when an agent's `[odoo]` settings cannot be read.
pub const ODOO_UNREADABLE: &str =
    "這位員工的 Odoo 設定讀不到，請管理員檢查 agent.toml 的 [odoo] 區塊";

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
    /// Odoo server URL. When set, replaces `OdooConfig::url`. Validated by
    /// [`crate::url_guard::is_safe_odoo_url`] in [`Self::apply_to`]; a URL
    /// that fails the check refuses the connection (fail closed) instead of
    /// falling back to the global server.
    pub url: Option<String>,
    /// Odoo database name. When set, replaces `OdooConfig::db`.
    pub db: Option<String>,
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
    /// Opt-out list for the built-in security block list. Default-blocked
    /// models named here (e.g. `res.partner`) become reachable via the generic
    /// `odoo_search` / `odoo_execute` tools — still subject to `allowed_models`
    /// / `allowed_actions` and, for system metadata tables, read-only
    /// enforcement. Empty ⇒ the default block list is fully in force.
    pub unblock_models: Vec<String>,
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
            && self.url.is_none()
            && self.db.is_none()
            && self.username.is_none()
            && self.api_key_enc.is_none()
            && self.password_enc.is_none()
            && self.allowed_models.is_empty()
            && self.allowed_actions.is_empty()
            && self.company_ids.is_empty()
            && self.unblock_models.is_empty()
    }

    /// Parse from raw `agent.toml` text — looks for the `[odoo]` section.
    /// Returns `None` when the file is unparseable or the section is absent
    /// (the common case), and also when the block is present but malformed:
    /// the gateway logs the failure and falls back to the global config.
    ///
    /// **Why this is not `duduclaw_core::agent_toml::AgentTomlSections`.**
    /// That is the workspace's single typed `agent.toml` parse point, and
    /// `[odoo]` belongs on it — but `duduclaw-odoo` *depends on*
    /// `duduclaw-core`, so putting [`AgentOdooConfig`] there would invert the
    /// dependency. What is portable is the discipline: this is a total,
    /// section-isolated serde parse (a wrong-typed unrelated section cannot
    /// make it fail), not a hand-rolled `toml::Value` walk, and it reads the
    /// text rather than requiring the caller to hand over a parsed table.
    pub fn from_agent_toml(raw: &str) -> Option<Self> {
        /// Section-isolating wrapper: only `[odoo]` is described, and it is
        /// `Option` so an absent **or** malformed block yields `None` without
        /// disturbing anything else in the file.
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            odoo: Option<AgentOdooConfig>,
        }

        // `toml::from_str` on the wrapper would still fail if `[odoo]` itself
        // is malformed, so parse the block through a two-step: the whole file
        // first (any valid TOML), then the section.
        let wrap: Wrap = match toml::from_str::<Wrap>(raw) {
            Ok(w) => w,
            // Either the file is not valid TOML at all, or `[odoo]` is
            // malformed — both were `None` before.
            Err(_) => return None,
        };
        let cfg = wrap.odoo?;
        if cfg.is_empty() { None } else { Some(cfg) }
    }

    /// Strict parse of an already-parsed `agent.toml` table, for every path
    /// that connects to Odoo (fail closed).
    ///
    /// * no `[odoo]` block, or a vacuous one ⇒ `Ok(None)` (use the global config);
    /// * a block that does not parse ⇒ `Err` (refuse to connect — a half-read
    ///   override must not silently send the agent to the global server).
    pub fn from_table(table: &toml::Table) -> Result<Option<Self>, String> {
        let Some(section) = table.get("odoo") else {
            return Ok(None);
        };
        let cfg: Self = section.clone().try_into().map_err(|_| ODOO_UNREADABLE.to_string())?;
        Ok(if cfg.is_empty() { None } else { Some(cfg) })
    }

    /// Strict parse from raw `agent.toml` text; see [`Self::from_table`]. A
    /// file that is not valid TOML is also an `Err`, because it is then
    /// unknowable whether an `[odoo]` override was meant to apply.
    pub fn parse_agent_toml(raw: &str) -> Result<Option<Self>, String> {
        let table: toml::Table = raw.parse().map_err(|_| ODOO_UNREADABLE.to_string())?;
        Self::from_table(&table)
    }

    /// The single merge of this override over the global `[odoo]` block.
    ///
    /// Every place that dials Odoo on behalf of an agent (connector pool,
    /// decision export, `odoo.agent_test`) goes through this function, so a
    /// connectivity test can never pass for a config the runtime would not
    /// use. Empty `url` / `db` count as unset. `url` / `db` are validated here and
    /// a failure is an `Err` (fail closed): a malformed agent override must
    /// not silently connect to the global server.
    pub fn apply_to(&self, config: &mut OdooConfig) -> Result<(), String> {
        fn set(v: &Option<String>) -> Option<&str> {
            v.as_deref().map(str::trim).filter(|s| !s.is_empty())
        }
        if let Some(url) = set(&self.url) {
            if !crate::url_guard::is_safe_odoo_url(url) {
                return Err("agent [odoo].url must use HTTPS (http:// only allowed for localhost/127.0.0.1) and must not target a private/reserved host".into());
            }
            config.url = url.to_string();
        }
        if let Some(db) = set(&self.db) {
            if !crate::url_guard::is_valid_odoo_db(db) {
                return Err("agent [odoo].db must be at most 63 characters of [a-zA-Z0-9_-]".into());
            }
            config.db = db.to_string();
        }
        // Credentials keep their historical "present ⇒ replace" semantics.
        if let Some(u) = &self.username {
            config.username = u.clone();
        }
        if let Some(k) = &self.api_key_enc {
            config.api_key_enc = k.clone();
        }
        if let Some(p) = &self.password_enc {
            config.password_enc = p.clone();
        }
        Ok(())
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

    /// Forget the override for `agent_id` (its `agent.toml [odoo]` was removed).
    pub fn remove_agent(&mut self, agent_id: &str) {
        self.per_agent.remove(agent_id);
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

    // ── R5: `[odoo]` direction, pinned ───────────────────────────────────
    //
    // absent file text / malformed TOML / absent `[odoo]` / malformed
    // `[odoo]` / an all-default (vacuous) block ⇒ None ⇒ the agent inherits
    // the GLOBAL `config.toml [odoo]`. Never a partially-applied override:
    // half-applied ERP credentials are worse than none, because the agent
    // would connect as the wrong service account rather than failing loudly.

    #[test]
    fn default_direction_odoo_block_falls_back_to_global_on_anything_unusable() {
        for raw in [
            "",                                   // empty file
            "[agent]\nname = \"foo\"\n",          // no [odoo]
            "[odoo]\n",                           // vacuous block
            "odoo = 1\n",                         // wrong-typed section
            "[odoo]\nallowed_models = 42\n",      // malformed block
            "[odoo]\ncompany_ids = [\"one\"]\n",  // wrong element type
            "not toml [[[",                       // malformed file
        ] {
            assert!(
                AgentOdooConfig::from_agent_toml(raw).is_none(),
                "must fall back to global for {raw:?}"
            );
        }
    }

    #[test]
    fn default_direction_unrelated_sections_never_affect_the_odoo_block() {
        // The parse is section-isolated: unknown sections are ignored, so an
        // unrelated key (even a wrong-typed one, as long as the file is valid
        // TOML) cannot take the override down.
        let raw = "[agent]\nname = 42\n[whatever]\nx = [1, \"two\"]\n\
                   [odoo]\nprofile = \"alpha\"\n";
        let cfg = AgentOdooConfig::from_agent_toml(raw).expect("override survives");
        assert_eq!(cfg.profile.as_deref(), Some("alpha"));
    }

    #[test]
    fn from_agent_toml_returns_none_when_no_block() {
        let raw = "[agent]\nname = \"foo\"\n";
        assert!(AgentOdooConfig::from_agent_toml(raw).is_none());
    }

    #[test]
    fn from_agent_toml_parses_minimal_block() {
        let raw = "\
            [odoo]\n\
            profile = \"alpha\"\n\
            username = \"alpha_user\"\n\
        ";
        let cfg = AgentOdooConfig::from_agent_toml(raw).expect("present");
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
        let cfg = AgentOdooConfig::from_agent_toml(raw).expect("present");
        assert_eq!(cfg.allowed_models.len(), 2);
        assert_eq!(cfg.allowed_actions.len(), 2);
        assert_eq!(cfg.company_ids, vec![1, 2]);
    }

    #[test]
    fn from_agent_toml_returns_none_for_empty_block() {
        // Operator wrote `[odoo]` then nothing — should be treated as absent
        // so we don't pin a vacuous override.
        let raw = "[odoo]\n";
        assert!(AgentOdooConfig::from_agent_toml(raw).is_none());
    }

    #[test]
    fn from_agent_toml_returns_none_for_malformed_block() {
        // `allowed_models` should be a list of strings.
        let raw = "[odoo]\nallowed_models = 42\n";
        assert!(AgentOdooConfig::from_agent_toml(raw).is_none());
    }

    // ── url / db override (B1) ───────────────────────────────────────────

    fn global_cfg() -> OdooConfig {
        OdooConfig {
            url: "https://global.example.com".into(),
            db: "global_db".into(),
            username: "global_user".into(),
            ..OdooConfig::default()
        }
    }

    #[test]
    fn apply_to_overrides_url_and_db_over_global() {
        let over = AgentOdooConfig {
            url: Some("https://alpha.example.com".into()),
            db: Some("alpha_db".into()),
            ..Default::default()
        };
        let mut cfg = global_cfg();
        over.apply_to(&mut cfg).expect("valid override");
        assert_eq!(cfg.url, "https://alpha.example.com");
        assert_eq!(cfg.db, "alpha_db");
        assert_eq!(cfg.username, "global_user", "unset fields inherit");
    }

    #[test]
    fn apply_to_without_url_db_inherits_global() {
        let over = AgentOdooConfig { username: Some("u".into()), ..Default::default() };
        let mut cfg = global_cfg();
        over.apply_to(&mut cfg).unwrap();
        assert_eq!(cfg.url, "https://global.example.com");
        assert_eq!(cfg.db, "global_db");
        assert_eq!(cfg.username, "u");
    }

    #[test]
    fn apply_to_refuses_unsafe_url_and_leaves_config_untouched_on_url() {
        for bad in [
            "http://erp.example.com",
            "https://169.254.169.254/",
            "https://10.0.0.5",
            "https://user@erp.example.com",
            "file:///etc/passwd",
        ] {
            let over = AgentOdooConfig { url: Some(bad.into()), ..Default::default() };
            let mut cfg = global_cfg();
            assert!(over.apply_to(&mut cfg).is_err(), "{bad} must be refused");
            assert_eq!(cfg.url, "https://global.example.com");
        }
    }

    #[test]
    fn apply_to_refuses_bad_db_name() {
        let over = AgentOdooConfig { db: Some("a;drop".into()), ..Default::default() };
        assert!(over.apply_to(&mut global_cfg()).is_err());
    }

    #[test]
    fn url_db_alone_make_a_non_empty_override_and_parse() {
        let raw = "[odoo]\nurl = \"https://alpha.example.com\"\ndb = \"alpha_db\"\n";
        let cfg = AgentOdooConfig::from_agent_toml(raw).expect("url/db-only override survives");
        assert_eq!(cfg.url.as_deref(), Some("https://alpha.example.com"));
        assert_eq!(cfg.db.as_deref(), Some("alpha_db"));
    }

    #[test]
    fn from_table_matches_from_agent_toml() {
        // Test-vs-runtime parity: the RPC path parses a table, the runtime
        // parses text; both must yield the same merged config.
        let raw = "[odoo]\nurl = \"https://alpha.example.com\"\ndb = \"alpha_db\"\nusername = \"a\"\n";
        let table: toml::Table = raw.parse().unwrap();
        let (mut a, mut b) = (global_cfg(), global_cfg());
        AgentOdooConfig::from_table(&table).unwrap().unwrap().apply_to(&mut a).unwrap();
        AgentOdooConfig::parse_agent_toml(raw).unwrap().unwrap().apply_to(&mut b).unwrap();
        assert_eq!((a.url, a.db, a.username), (b.url, b.db, b.username));
    }

    #[test]
    fn strict_parse_absent_block_means_global() {
        for raw in ["", "[agent]\nname = \"x\"\n", "[odoo]\n"] {
            assert!(AgentOdooConfig::parse_agent_toml(raw).unwrap().is_none(), "{raw:?}");
        }
    }

    #[test]
    fn strict_parse_unreadable_block_is_refused_not_global() {
        for raw in [
            "[odoo]\nallowed_models = 42\n",
            "[odoo]\nurl = 5\n",
            "odoo = 1\n",
            "not toml [[[",
        ] {
            let err = AgentOdooConfig::parse_agent_toml(raw).unwrap_err();
            assert!(err.contains("Odoo 設定讀不到"), "{raw:?}: {err}");
        }
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
