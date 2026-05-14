//! `RedactionManager` — top-level façade for callers (gateway, channel,
//! dashboard).
//!
//! Owns the shared [`VaultStore`], the compiled [`RuleEngine`], the
//! [`AuditSink`], and per-agent key bytes. Hands out per-session
//! [`RedactionPipeline`] instances on demand. Also exposes egress
//! evaluation so tool dispatchers can call `decide_tool_call` without
//! materialising a pipeline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::audit::{AuditSink, JsonlAuditSink, NullAuditSink};
use crate::config::{Profile, RedactionConfig, SourcePolicy};
use crate::egress::{EgressDecision, EgressEvaluator};
use crate::engine::RuleEngine;
use crate::error::{RedactionError, Result};
use crate::pipeline::RedactionPipeline;
use crate::profiles;
use crate::rules::RuleSpec;
use crate::vault::{VaultStore, key};

/// Paths the manager needs on disk.
#[derive(Debug, Clone)]
pub struct ManagerPaths {
    /// Directory holding per-agent key files (`<agent>.key`).
    pub key_dir: PathBuf,
    /// Path to the SQLite vault file.
    pub vault_db: PathBuf,
    /// Path to the JSONL audit log file (None = log to tracing only).
    pub audit_log: Option<PathBuf>,
    /// Path to the force-override flag file.
    pub override_flag: PathBuf,
}

impl ManagerPaths {
    /// Sensible defaults rooted at `<duduclaw_home>/redaction/`.
    pub fn under_home(home: &std::path::Path) -> Self {
        let base = home.join("redaction");
        Self {
            key_dir: base.join("keys"),
            vault_db: base.join("vault.db"),
            audit_log: Some(base.join("audit.jsonl")),
            override_flag: base.join("override.flag"),
        }
    }
}

/// Top-level service object.
pub struct RedactionManager {
    config: RedactionConfig,
    engine: Arc<RuleEngine>,
    vault: Arc<VaultStore>,
    audit: Arc<dyn AuditSink>,
    paths: ManagerPaths,
    /// Cached per-agent key bytes — loaded once per agent.
    agent_keys: Mutex<HashMap<String, [u8; 32]>>,
    egress: Arc<EgressEvaluator>,
}

impl RedactionManager {
    /// Build a manager from a [`RedactionConfig`] and on-disk paths.
    ///
    /// `extra_specs` is appended to the rules resolved from profiles +
    /// inline `[redaction.rules.*]` entries. The gateway typically loads
    /// these once at startup and shares the resulting `Arc<RedactionManager>`.
    pub fn open(config: RedactionConfig, paths: ManagerPaths) -> Result<Self> {
        config.validate()?;

        // Materialise rule specs from profiles + inline rules.
        let mut specs: Vec<RuleSpec> = Vec::new();
        for profile_name in &config.profiles {
            let Some(profile) = profiles::load_builtin(profile_name)? else {
                // Custom profile: try `<key_dir>/../profiles/<name>.toml`.
                let custom_path = paths
                    .key_dir
                    .parent()
                    .unwrap_or(&paths.key_dir)
                    .join("profiles")
                    .join(format!("{profile_name}.toml"));
                if !custom_path.exists() {
                    return Err(RedactionError::config(format!(
                        "redaction profile '{profile_name}' not found (neither built-in nor at {})",
                        custom_path.display()
                    )));
                }
                let custom = Profile::from_path(custom_path)?;
                specs.extend(custom.into_specs());
                continue;
            };
            specs.extend(profile.into_specs());
        }
        // Inline rules override profile rules on id collision: walk last so
        // they end up later in the specs vec — same-id earlier specs are
        // shadowed by the engine's later compile-and-overwrite by id.
        for (id, mut spec) in config.rules.clone() {
            spec.id = id;
            // Drop any earlier spec with the same id.
            specs.retain(|s| s.id != spec.id);
            specs.push(spec);
        }

        let engine = Arc::new(RuleEngine::from_specs(specs)?);
        let vault = Arc::new(VaultStore::open(&paths.vault_db, &paths.key_dir)?);
        let audit: Arc<dyn AuditSink> = match &paths.audit_log {
            Some(p) => Arc::new(JsonlAuditSink::new(p.clone())),
            None => Arc::new(NullAuditSink),
        };
        let egress = Arc::new(EgressEvaluator::new(config.tool_egress.clone()));

        Ok(Self {
            config,
            engine,
            vault,
            audit,
            paths,
            agent_keys: Mutex::new(HashMap::new()),
            egress,
        })
    }

    /// Whether redaction is enabled at this manager's config layer. Multi-layer
    /// resolution lives in `toggle::compute_effective_enabled`.
    pub fn config_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn paths(&self) -> &ManagerPaths {
        &self.paths
    }

    pub fn vault(&self) -> &Arc<VaultStore> {
        &self.vault
    }

    pub fn audit_sink(&self) -> &Arc<dyn AuditSink> {
        &self.audit
    }

    pub fn engine(&self) -> &Arc<RuleEngine> {
        &self.engine
    }

    pub fn source_policy(&self) -> &SourcePolicy {
        &self.config.sources
    }

    pub fn vault_ttl_hours(&self) -> i64 {
        self.config.vault_ttl_hours
    }

    pub fn purge_after_expire_days(&self) -> u32 {
        self.config.purge_after_expire_days
    }

    /// Load (or generate) the agent's 32-byte key, caching the result.
    pub fn agent_key(&self, agent_id: &str) -> Result<[u8; 32]> {
        {
            let cache = self
                .agent_keys
                .lock()
                .map_err(|e| RedactionError::vault(e.to_string()))?;
            if let Some(k) = cache.get(agent_id) {
                return Ok(*k);
            }
        }
        let k = key::load_or_generate(agent_id, &self.paths.key_dir)?;
        let mut cache = self
            .agent_keys
            .lock()
            .map_err(|e| RedactionError::vault(e.to_string()))?;
        cache.insert(agent_id.to_string(), k);
        Ok(k)
    }

    /// Mint a fresh [`RedactionPipeline`] for a `(agent_id, session_id)` pair.
    ///
    /// Pipelines are cheap value types (they hold `Arc`s to the shared
    /// engine / vault / audit) so callers can construct one per turn
    /// without worry.
    pub fn pipeline(
        &self,
        agent_id: &str,
        session_id: Option<String>,
    ) -> Result<RedactionPipeline> {
        let key_bytes = self.agent_key(agent_id)?;
        Ok(RedactionPipeline::new(
            self.engine.clone(),
            self.vault.clone(),
            self.audit.clone(),
            agent_id,
            session_id,
            &key_bytes,
            self.config.sources.clone(),
            self.config.vault_ttl_hours,
        ))
    }

    /// One-shot egress decision — caller hands us the tool name + arg
    /// JSON, we look up the rule + restore against the vault.
    pub fn decide_tool_call(
        &self,
        tool_name: &str,
        args: &Value,
        agent_id: &str,
        session_id: Option<&str>,
    ) -> Result<EgressDecision> {
        self.egress
            .decide(tool_name, args, agent_id, session_id, &self.vault, &*self.audit)
    }
}

impl std::fmt::Debug for RedactionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedactionManager")
            .field("enabled", &self.config.enabled)
            .field("paths", &self.paths)
            .field("rules", &self.engine.rule_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RedactionConfig, ToolEgressRule, RestoreArgsMode};
    use crate::source::{Caller, RestoreTarget, Source};
    use tempfile::TempDir;

    fn paths(tmp: &TempDir) -> ManagerPaths {
        ManagerPaths::under_home(tmp.path())
    }

    fn cfg_with(profile: &str) -> RedactionConfig {
        let mut c = RedactionConfig::default();
        c.enabled = true;
        c.profiles = vec![profile.into()];
        c
    }

    #[test]
    fn opens_with_builtin_profile() {
        let tmp = TempDir::new().unwrap();
        let m = RedactionManager::open(cfg_with("general"), paths(&tmp)).unwrap();
        assert!(m.engine.rule_count() > 0);
    }

    #[test]
    fn unknown_profile_errors() {
        let tmp = TempDir::new().unwrap();
        let err = RedactionManager::open(cfg_with("does_not_exist"), paths(&tmp)).unwrap_err();
        assert!(matches!(err, RedactionError::Config(_)));
    }

    #[test]
    fn end_to_end_round_trip_through_manager() {
        let tmp = TempDir::new().unwrap();
        let m = RedactionManager::open(cfg_with("general"), paths(&tmp)).unwrap();
        let pipe = m.pipeline("agnes", Some("s1".into())).unwrap();

        let red = pipe
            .redact(
                "contact alice@acme.com",
                &Source::ToolResult { tool_name: "odoo.search".into() },
            )
            .unwrap();
        assert!(red.redacted_text.contains("<REDACT:"));

        let restored = pipe
            .restore(&red.redacted_text, &Caller::owner("agnes"), RestoreTarget::UserChannel)
            .unwrap();
        assert!(restored.contains("alice@acme.com"));
    }

    #[test]
    fn agent_key_is_cached() {
        let tmp = TempDir::new().unwrap();
        let m = RedactionManager::open(cfg_with("general"), paths(&tmp)).unwrap();
        let k1 = m.agent_key("agnes").unwrap();
        let k2 = m.agent_key("agnes").unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn decide_tool_call_denies_unknown_tools() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = cfg_with("general");
        // No tool_egress entry → default deny.
        let m = RedactionManager::open(cfg, paths(&tmp)).unwrap();
        let pipe = m.pipeline("agnes", Some("s1".into())).unwrap();
        let red = pipe
            .redact(
                "alice@acme.com",
                &Source::ToolResult { tool_name: "x".into() },
            )
            .unwrap();
        let tok = red.tokens_written[0].as_str().to_string();
        let dec = m
            .decide_tool_call(
                "web_fetch",
                &serde_json::json!({"url": tok}),
                "agnes",
                Some("s1"),
            )
            .unwrap();
        assert!(matches!(dec, EgressDecision::Deny { .. }));
    }

    #[test]
    fn decide_tool_call_allows_whitelisted_with_restore() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = cfg_with("general");
        cfg.tool_egress.insert(
            "send_email".into(),
            ToolEgressRule {
                restore_args: RestoreArgsMode::Restore,
                audit_reveal: false,
            },
        );
        let m = RedactionManager::open(cfg, paths(&tmp)).unwrap();
        let pipe = m.pipeline("agnes", Some("s1".into())).unwrap();
        let red = pipe
            .redact(
                "alice@acme.com",
                &Source::ToolResult { tool_name: "x".into() },
            )
            .unwrap();
        let tok = red.tokens_written[0].as_str().to_string();
        let dec = m
            .decide_tool_call(
                "send_email",
                &serde_json::json!({"to": tok}),
                "agnes",
                Some("s1"),
            )
            .unwrap();
        match dec {
            EgressDecision::Allow { args, .. } => {
                assert_eq!(args["to"], serde_json::Value::String("alice@acme.com".into()));
            }
            other => panic!("expected Allow, got {other:?}"),
        }
    }

    #[test]
    fn inline_rules_override_profile_rules_by_id() {
        let tmp = TempDir::new().unwrap();
        let mut cfg = cfg_with("general");
        // Override the built-in `email` rule with a stricter pattern.
        use crate::rules::{RestoreScope, RuleKind, RuleSpec};
        cfg.rules.insert(
            "email".into(),
            RuleSpec {
                id: "email".into(),
                category: "EMAIL_STRICT".into(),
                restore_scope: RestoreScope::Owner,
                priority: 80,
                cross_session_stable: false,
                apply_to_system_prompt: false,
                kind: RuleKind::Regex { pattern: r"strict@example\.com".into() },
            },
        );
        let m = RedactionManager::open(cfg, paths(&tmp)).unwrap();
        let pipe = m.pipeline("a", Some("s".into())).unwrap();
        // Non-strict email should NOT match anymore (we replaced the rule).
        let red = pipe
            .redact(
                "alice@acme.com",
                &Source::ToolResult { tool_name: "x".into() },
            )
            .unwrap();
        assert!(
            !red.redacted_text.contains("<REDACT:EMAIL"),
            "expected the original 'email' rule to be shadowed: {}",
            red.redacted_text
        );
    }
}
