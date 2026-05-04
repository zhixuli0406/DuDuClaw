//! odoo_pool.rs — RFC-21 §2: per-agent Odoo connector pool.
//!
//! Replaces the v1.10.1 global `Arc<RwLock<Option<OdooConnector>>>` with a
//! pool keyed by `(agent_id, profile)` so every agent can connect as its own
//! service account. The pool also carries an [`OdooConfigResolver`] holding
//! the global `[odoo]` block plus any per-agent `agent.toml [odoo]`
//! overrides discovered at gateway startup.
//!
//! ## Pool key
//!
//! The pool key is `(agent_id, profile)`:
//! - **Without** an override: every agent shares profile `"default"`, so the
//!   pool collapses to a single slot keyed by `(agent_id, "default")`.
//!   Two different agents *still* get separate connector slots — they may
//!   share the same global credentials but the pool key is distinct, which
//!   makes the audit trail and connector lifecycle per-agent.
//! - **With** an override: profile is whatever the operator set in
//!   `agent.toml [odoo].profile`, and credentials come from the override
//!   (`username` / `api_key_enc` / `password_enc`).
//!
//! ## Locking
//!
//! - `pool` uses an outer `RwLock<HashMap>` for membership reads, plus
//!   per-slot `tokio::sync::Mutex` for first-use connect serialisation.
//!   That keeps the connect race tight without holding the outer write
//!   lock during the (slow) HTTP authentication round-trip.
//! - `resolver` uses a plain `RwLock` because it's read-heavy — operators
//!   call `register_agent` once at startup, every odoo_* call only reads.

use std::collections::HashMap;
use std::sync::Arc;

use duduclaw_odoo::{
    AgentOdooConfig, OdooConfig, OdooConfigResolver, OdooConnector,
};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

/// Resolved credentials ready to hand to [`OdooConnector::connect`].
#[derive(Debug, Clone)]
struct ResolvedCredentials {
    config: OdooConfig,
    credential: String,
}

/// One slot in the pool. Wrapped in a `Mutex` so concurrent first-use
/// connect attempts serialise cleanly.
#[derive(Default)]
struct PoolSlot {
    connector: Option<Arc<OdooConnector>>,
}

/// Per-agent Odoo connector pool. Cheap to clone — backed by `Arc`.
pub struct OdooConnectorPool {
    pool: RwLock<HashMap<(String, String), Arc<Mutex<PoolSlot>>>>,
    resolver: RwLock<OdooConfigResolver>,
}

impl Default for OdooConnectorPool {
    fn default() -> Self {
        Self::new(OdooConfig::default())
    }
}

impl OdooConnectorPool {
    pub fn new(global: OdooConfig) -> Self {
        Self {
            pool: RwLock::new(HashMap::new()),
            resolver: RwLock::new(OdooConfigResolver::new(global)),
        }
    }

    /// Replace the global `[odoo]` config (e.g. on `odoo_connect` after a
    /// `config.toml` edit). Per-agent overrides are preserved — operators
    /// that hot-reload the global block typically want existing `agent.toml
    /// [odoo]` overrides to stay registered.
    ///
    /// Cached connectors are *not* dropped here — they will continue to
    /// serve requests against whatever they were authed for. Call
    /// [`disconnect`] (or [`disconnect_all`]) explicitly if you want a
    /// fresh handshake.
    pub async fn set_global(&self, global: OdooConfig) {
        self.resolver.write().await.set_global(global);
    }

    /// Drop every cached connector. Useful after a global config edit when
    /// the operator wants every agent's next call to re-authenticate.
    pub async fn disconnect_all(&self) {
        self.pool.write().await.clear();
    }

    /// Register an `agent.toml [odoo]` override for `agent_id`.
    pub async fn register_agent(&self, agent_id: impl Into<String>, cfg: AgentOdooConfig) {
        let id = agent_id.into();
        info!(
            agent_id = %id,
            profile = %cfg.profile_or_default(),
            "Odoo per-agent override registered"
        );
        self.resolver.write().await.upsert_agent(id, cfg);
    }

    /// Stable pool key for `agent_id`.
    pub async fn pool_key(&self, agent_id: &str) -> (String, String) {
        self.resolver.read().await.pool_key_for(agent_id)
    }

    /// Snapshot of the per-agent override (or `None` if absent). Cheap
    /// clone of the small `AgentOdooConfig` struct.
    pub async fn agent_override(&self, agent_id: &str) -> Option<AgentOdooConfig> {
        self.resolver.read().await.for_agent(agent_id).cloned()
    }

    /// Decrypted credentials for the given agent. Falls back to the global
    /// `[odoo]` block when no per-agent override sets the relevant fields.
    /// `decrypt` is invoked lazily so a miss in either pool slot never
    /// surfaces a decryption error to handlers that don't actually need a
    /// credential (e.g. `odoo_status` on an unconnected pool).
    fn merge_credentials<F>(
        global: &OdooConfig,
        override_cfg: Option<&AgentOdooConfig>,
        decrypt: F,
    ) -> Result<ResolvedCredentials, String>
    where
        F: Fn(&str) -> Result<String, String>,
    {
        // Username merge.
        let mut config = global.clone();
        if let Some(o) = override_cfg {
            if let Some(u) = &o.username {
                config.username = u.clone();
            }
            if let Some(k) = &o.api_key_enc {
                config.api_key_enc = k.clone();
            }
            if let Some(p) = &o.password_enc {
                config.password_enc = p.clone();
            }
        }

        // Pick which encrypted blob to decrypt. Prefer api_key over password.
        let enc = if !config.api_key_enc.is_empty() {
            config.api_key_enc.clone()
        } else if !config.password_enc.is_empty() {
            config.password_enc.clone()
        } else {
            return Err(
                "no Odoo credential configured (api_key_enc / password_enc both empty)".into(),
            );
        };

        let credential = decrypt(&enc)?;
        Ok(ResolvedCredentials { config, credential })
    }

    /// Get-or-connect the connector for `agent_id`. The decrypt closure is
    /// invoked at most once per slot — the cached `Arc<OdooConnector>` is
    /// returned by every subsequent call until [`disconnect`] is invoked.
    pub async fn get_or_connect<F>(
        &self,
        agent_id: &str,
        decrypt: F,
    ) -> Result<Arc<OdooConnector>, String>
    where
        F: Fn(&str) -> Result<String, String>,
    {
        let key = self.pool_key(agent_id).await;

        // Outer fast path — read lock, see if a connector is already cached.
        {
            let pool = self.pool.read().await;
            if let Some(slot) = pool.get(&key) {
                let guard = slot.lock().await;
                if let Some(conn) = &guard.connector {
                    return Ok(conn.clone());
                }
            }
        }

        // Slow path — acquire the slot, holding only the inner Mutex during
        // the connect call so other agents can still progress.
        let slot = {
            let mut pool = self.pool.write().await;
            pool.entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(PoolSlot::default())))
                .clone()
        };

        let mut guard = slot.lock().await;
        if let Some(conn) = &guard.connector {
            // Lost the race — another task connected first.
            return Ok(conn.clone());
        }

        let resolver = self.resolver.read().await;
        let global = resolver.global().clone();
        let agent_override = resolver.for_agent(agent_id).cloned();
        drop(resolver);

        let creds = Self::merge_credentials(&global, agent_override.as_ref(), &decrypt)?;
        let conn = OdooConnector::connect(&creds.config, &creds.credential).await?;
        let arc = Arc::new(conn);
        guard.connector = Some(arc.clone());
        info!(
            agent_id = %agent_id,
            profile = %key.1,
            "Odoo connector established"
        );
        Ok(arc)
    }

    /// Probe whether an `agent_id`'s slot already holds a live connector.
    pub async fn is_connected(&self, agent_id: &str) -> bool {
        let key = self.pool_key(agent_id).await;
        let pool = self.pool.read().await;
        if let Some(slot) = pool.get(&key) {
            slot.lock().await.connector.is_some()
        } else {
            false
        }
    }

    /// Drop the cached connector for `agent_id`. The next call will
    /// re-authenticate.
    pub async fn disconnect(&self, agent_id: &str) {
        let key = self.pool_key(agent_id).await;
        if let Some(slot) = self.pool.write().await.remove(&key) {
            // Hold the slot lock briefly so any in-flight call sees the
            // explicit disconnect rather than a stale Arc.
            let _g = slot.lock().await;
            warn!(
                agent_id = %agent_id,
                profile = %key.1,
                "Odoo connector disconnected"
            );
        }
    }

    /// Returns a debug-friendly listing of currently-connected slots.
    pub async fn slots(&self) -> Vec<(String, String, bool)> {
        let pool = self.pool.read().await;
        let mut out = Vec::with_capacity(pool.len());
        for (key, slot) in pool.iter() {
            let connected = slot.lock().await.connector.is_some();
            out.push((key.0.clone(), key.1.clone(), connected));
        }
        out
    }
}

// ── Action / model permission helpers ────────────────────────────────────────

/// RFC-21 §2 acceptance: defence-in-depth. Even if the operator mis-provisions
/// the Odoo service account, DuDuClaw refuses calls that the agent's
/// `agent.toml [odoo].allowed_actions` / `allowed_models` whitelists do not
/// cover.
///
/// `verb` is one of `"read" | "search" | "create" | "write" | "execute"`.
pub fn check_action_permission(
    cfg: Option<&AgentOdooConfig>,
    verb: &str,
    model: &str,
) -> Result<(), String> {
    let cfg = match cfg {
        Some(c) => c,
        None => return Ok(()), // no override ⇒ no extra restriction
    };
    if !cfg.permits_model(model) {
        return Err(format!(
            "model '{model}' is not in this agent's allowed_models list"
        ));
    }
    if !cfg.permits(verb, model) {
        return Err(format!(
            "action '{verb}' on model '{model}' is not in this agent's allowed_actions list"
        ));
    }
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_odoo::AgentOdooConfig;

    #[tokio::test(flavor = "current_thread")]
    async fn pool_key_uses_default_for_unregistered_agent() {
        let pool = OdooConnectorPool::default();
        assert_eq!(
            pool.pool_key("agnes").await,
            ("agnes".to_string(), "default".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_key_uses_explicit_profile_when_registered() {
        let pool = OdooConnectorPool::default();
        pool.register_agent(
            "alpha-pm",
            AgentOdooConfig { profile: Some("alpha".into()), ..Default::default() },
        )
        .await;
        assert_eq!(
            pool.pool_key("alpha-pm").await,
            ("alpha-pm".to_string(), "alpha".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_override_returns_registered_config() {
        let pool = OdooConnectorPool::default();
        pool.register_agent(
            "alpha-pm",
            AgentOdooConfig {
                profile: Some("alpha".into()),
                allowed_models: vec!["crm.lead".into()],
                ..Default::default()
            },
        )
        .await;
        let cfg = pool.agent_override("alpha-pm").await.expect("present");
        assert_eq!(cfg.profile.as_deref(), Some("alpha"));
        assert_eq!(cfg.allowed_models, vec!["crm.lead".to_string()]);
        assert!(pool.agent_override("nope").await.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn is_connected_starts_false_for_every_agent() {
        let pool = OdooConnectorPool::default();
        assert!(!pool.is_connected("agnes").await);
        assert!(!pool.is_connected("alpha-pm").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disconnect_is_idempotent_when_not_connected() {
        let pool = OdooConnectorPool::default();
        pool.disconnect("never-connected").await; // must not panic
        assert!(!pool.is_connected("never-connected").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_falls_back_to_global() {
        let mut global = OdooConfig::default();
        global.api_key_enc = "ENC_GLOBAL".into();

        let creds =
            OdooConnectorPool::merge_credentials(&global, None, |enc| Ok(format!("dec({enc})")))
                .unwrap();
        assert_eq!(creds.credential, "dec(ENC_GLOBAL)");
        assert_eq!(creds.config.api_key_enc, "ENC_GLOBAL");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_prefers_per_agent_override() {
        let mut global = OdooConfig::default();
        global.api_key_enc = "ENC_GLOBAL".into();
        global.username = "global_user".into();

        let agent_cfg = AgentOdooConfig {
            username: Some("alpha_user".into()),
            api_key_enc: Some("ENC_ALPHA".into()),
            ..Default::default()
        };

        let creds = OdooConnectorPool::merge_credentials(&global, Some(&agent_cfg), |enc| {
            Ok(format!("dec({enc})"))
        })
        .unwrap();
        assert_eq!(creds.credential, "dec(ENC_ALPHA)");
        assert_eq!(creds.config.username, "alpha_user");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_errors_when_no_credential_present() {
        let global = OdooConfig::default(); // empty
        let err = OdooConnectorPool::merge_credentials(&global, None, |_| Ok(String::new()))
            .unwrap_err();
        assert!(err.contains("no Odoo credential"), "got: {err}");
    }

    #[test]
    fn check_action_permission_with_no_override_is_permissive() {
        assert!(check_action_permission(None, "write", "anything").is_ok());
    }

    #[test]
    fn check_action_permission_blocks_disallowed_model() {
        let cfg = AgentOdooConfig {
            allowed_models: vec!["crm.lead".into()],
            ..Default::default()
        };
        let err = check_action_permission(Some(&cfg), "read", "res.partner").unwrap_err();
        assert!(err.contains("allowed_models"));
    }

    #[test]
    fn check_action_permission_blocks_disallowed_verb() {
        let cfg = AgentOdooConfig {
            allowed_actions: vec!["read".into(), "search".into()],
            ..Default::default()
        };
        let err = check_action_permission(Some(&cfg), "write", "crm.lead").unwrap_err();
        assert!(err.contains("allowed_actions"));
    }

    #[test]
    fn check_action_permission_passes_qualified_verb() {
        let cfg = AgentOdooConfig {
            allowed_actions: vec!["write:crm.lead".into()],
            ..Default::default()
        };
        assert!(check_action_permission(Some(&cfg), "write", "crm.lead").is_ok());
        assert!(check_action_permission(Some(&cfg), "write", "sale.order").is_err());
    }
}
