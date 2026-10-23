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

use duduclaw_odoo::{AgentOdooConfig, OdooConfig, OdooConfigResolver, OdooConnector};
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
    /// Effective `(url, db)` the cached connector was opened against. A cached
    /// connector is reused only while the agent's current effective endpoint
    /// still equals this, so a changed server never reuses the old session.
    endpoint: Option<(String, String)>,
}

impl PoolSlot {
    /// The cached connector, only when it targets `endpoint`.
    fn connector_for(&self, endpoint: &(String, String)) -> Option<Arc<OdooConnector>> {
        match (&self.connector, &self.endpoint) {
            (Some(conn), Some(ep)) if ep == endpoint => Some(conn.clone()),
            _ => None,
        }
    }
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
    /// Credentials doctrine P2 (zero-restart rotation): every cached
    /// connector is dropped here. A per-agent override only replaces the
    /// fields it sets — `merge_credentials` falls back to the global
    /// `api_key_enc` / `password_enc` for anything an override leaves unset —
    /// so *any* pool slot may be resting on the credential that was just
    /// replaced, not only the caller's own. Leaving other agents' connectors
    /// cached would mean "rotate the Odoo password" silently keeps every
    /// other agent authenticated on the old session until something else
    /// disconnects them, which is exactly the "changed the credential, still
    /// need a restart" gap this line exists to close. The next call per
    /// agent re-authenticates and picks up whatever `resolve` now decrypts
    /// to — no explicit [`disconnect`] / [`disconnect_all`] call required by
    /// the caller.
    pub async fn set_global(&self, global: OdooConfig) {
        self.resolver.write().await.set_global(global);
        self.disconnect_all().await;
    }

    /// Drop every cached connector. Useful after a global config edit when
    /// the operator wants every agent's next call to re-authenticate.
    pub async fn disconnect_all(&self) {
        self.pool.write().await.clear();
    }

    /// Register an `agent.toml [odoo]` override for `agent_id`.
    ///
    /// Credentials doctrine P2: an override may change this agent's own
    /// credential fields, so its cached connector (if any) is dropped —
    /// the next call re-authenticates against the config just registered
    /// rather than continuing on a session opened under the old one. The
    /// pool key is captured *before* the resolver is updated: `profile` is
    /// itself part of the override and can change too, and if it does,
    /// `pool_key_for` would resolve to a different key afterwards — dropping
    /// post-update would silently orphan the connector under the old key
    /// forever instead of freeing it. A no-op when nothing was connected yet
    /// (the common case: registration usually runs at gateway startup before
    /// the first `odoo_*` call).
    pub async fn register_agent(&self, agent_id: impl Into<String>, cfg: AgentOdooConfig) {
        let id = agent_id.into();
        info!(
            agent_id = %id,
            profile = %cfg.profile_or_default(),
            "Odoo per-agent override registered"
        );
        let old_key = self.pool_key(&id).await;
        self.resolver.write().await.upsert_agent(id, cfg);
        self.drop_slot(&old_key).await;
    }

    /// Forget `agent_id`'s override (its `agent.toml [odoo]` block was removed
    /// or became unusable) and drop its cached connector, so a stale override
    /// registered earlier cannot keep steering the agent to its old server.
    pub async fn unregister_agent(&self, agent_id: &str) {
        let old_key = self.pool_key(agent_id).await;
        self.resolver.write().await.remove_agent(agent_id);
        self.drop_slot(&old_key).await;
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

    /// `config.toml [odoo] features_*` gate for `model` (global switches;
    /// `Err(module)` when that module is switched off).
    pub async fn feature_gate(&self, model: &str) -> Result<(), &'static str> {
        self.resolver.read().await.global().feature_allows_model(model)
    }

    /// Effective `unblock_models` opt-out for `agent_id`. When the agent has an
    /// `agent.toml [odoo]` override block, that block's `unblock_models` is
    /// authoritative (even if empty — a deliberate "none"); otherwise the
    /// global `config.toml [odoo].unblock_models` baseline applies.
    pub async fn unblock_models(&self, agent_id: &str) -> Vec<String> {
        let resolver = self.resolver.read().await;
        match resolver.for_agent(agent_id) {
            Some(cfg) => cfg.unblock_models.clone(),
            None => resolver.global().unblock_models.clone(),
        }
    }

    /// Effective `(url, db)` for an agent: global overlaid with its override
    /// through the same `apply_to` merge the connection itself uses.
    fn effective_endpoint(
        global: &OdooConfig,
        override_cfg: Option<&AgentOdooConfig>,
    ) -> Result<(String, String), String> {
        let mut config = global.clone();
        if let Some(o) = override_cfg {
            o.apply_to(&mut config)?;
        }
        Ok((config.url, config.db))
    }

    /// Decrypted credentials for the given agent. Falls back to the global
    /// `[odoo]` block when no per-agent override sets the relevant fields.
    /// `decrypt` is invoked lazily so a miss in either pool slot never
    /// surfaces a decryption error to handlers that don't actually need a
    /// credential (e.g. `odoo_status` on an unconnected pool).
    async fn merge_credentials<F, Fut>(
        global: &OdooConfig,
        override_cfg: Option<&AgentOdooConfig>,
        resolve: F,
    ) -> Result<ResolvedCredentials, String>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        // One merge function shared with `odoo.agent_test` and the decision
        // export (`AgentOdooConfig::apply_to`): url / db / username / secrets,
        // with url + db validated (fail closed).
        let mut config = global.clone();
        if let Some(o) = override_cfg {
            o.apply_to(&mut config)?;
        }

        // Pick which credential source to resolve. Prefer api_key over
        // password. The `*_enc` field normally holds AES ciphertext but may
        // hold a `secret://...` Vault reference — resolution is delegated to
        // the (async) `resolve` closure, which handles both shapes.
        let enc = if !config.api_key_enc.is_empty() {
            config.api_key_enc.clone()
        } else if !config.password_enc.is_empty() {
            config.password_enc.clone()
        } else {
            return Err(
                "no Odoo credential configured (api_key_enc / password_enc both empty)".into(),
            );
        };

        let credential = resolve(enc).await?;
        Ok(ResolvedCredentials { config, credential })
    }

    /// Get-or-connect the connector for `agent_id`. The decrypt closure is
    /// invoked at most once per slot — the cached `Arc<OdooConnector>` is
    /// returned by every subsequent call until [`disconnect`] is invoked.
    pub async fn get_or_connect<F, Fut>(
        &self,
        agent_id: &str,
        resolve: F,
    ) -> Result<Arc<OdooConnector>, String>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        let key = self.pool_key(agent_id).await;
        let endpoint = {
            let resolver = self.resolver.read().await;
            Self::effective_endpoint(resolver.global(), resolver.for_agent(agent_id))?
        };

        // Outer fast path — read lock, see if a connector for the current
        // effective endpoint is already cached.
        {
            let pool = self.pool.read().await;
            if let Some(slot) = pool.get(&key) {
                let guard = slot.lock().await;
                if let Some(conn) = guard.connector_for(&endpoint) {
                    return Ok(conn);
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
        if let Some(conn) = guard.connector_for(&endpoint) {
            // Lost the race — another task connected first.
            return Ok(conn);
        }
        // Stale connector for a previous endpoint: discard before reconnecting.
        guard.connector = None;
        guard.endpoint = None;

        let resolver = self.resolver.read().await;
        let global = resolver.global().clone();
        let agent_override = resolver.for_agent(agent_id).cloned();
        drop(resolver);

        let creds = Self::merge_credentials(&global, agent_override.as_ref(), resolve).await?;
        let mut conn = OdooConnector::connect(&creds.config, &creds.credential).await?;
        // M18: apply the agent's cross-company scoping so every subsequent call
        // carries `allowed_company_ids` / `company_id`. Without this the builder
        // was never invoked and per-agent company isolation had no effect.
        if let Some(cfg) = agent_override.as_ref()
            && !cfg.company_ids.is_empty()
        {
            let ids: Vec<i64> = cfg.company_ids.iter().map(|&id| id as i64).collect();
            conn = conn.with_company_ids(ids);
        }
        let arc = Arc::new(conn);
        guard.connector = Some(arc.clone());
        guard.endpoint = Some((creds.config.url.clone(), creds.config.db.clone()));
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
        self.drop_slot(&key).await;
    }

    /// Drop the cached connector at an already-resolved pool key. Shared by
    /// [`disconnect`] (resolves the key itself) and [`register_agent`] (needs
    /// the *pre-update* key — see its doc comment).
    async fn drop_slot(&self, key: &(String, String)) {
        if let Some(slot) = self.pool.write().await.remove(key) {
            // Hold the slot lock briefly so any in-flight call sees the
            // explicit disconnect rather than a stale Arc.
            let _g = slot.lock().await;
            warn!(
                agent_id = %key.0,
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
            AgentOdooConfig {
                profile: Some("alpha".into()),
                ..Default::default()
            },
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

    // ── WP-8A: credentials doctrine P2 — zero-restart rotation ───────────

    /// `set_global` must drop EVERY cached slot, not just the caller's own —
    /// a per-agent override only replaces the fields it sets, so any slot
    /// may still be resting on the global credential that was just rotated
    /// (`merge_credentials` falls back to it). A real connector needs live
    /// network I/O to establish, so slots are seeded directly here — the
    /// property under test is "does `set_global` clear the pool map", not
    /// "can we actually connect".
    #[tokio::test(flavor = "current_thread")]
    async fn set_global_drops_every_cached_slot_not_only_the_callers() {
        let pool = OdooConnectorPool::default();
        {
            let mut inner = pool.pool.write().await;
            inner.insert(
                ("agent-a".into(), "default".into()),
                Arc::new(Mutex::new(PoolSlot::default())),
            );
            inner.insert(
                ("agent-b".into(), "default".into()),
                Arc::new(Mutex::new(PoolSlot::default())),
            );
        }
        assert_eq!(
            pool.slots().await.len(),
            2,
            "precondition: two seeded slots"
        );

        pool.set_global(OdooConfig::default()).await;

        assert!(
            pool.slots().await.is_empty(),
            "set_global must drop every cached slot so every agent re-authenticates \
             with the new credential on its next call"
        );
    }

    /// `register_agent` must drop the slot cached under the *pre-update* key.
    /// The pool key is derived from the (possibly just-changed) `profile`, so
    /// resolving the key AFTER applying the override would resolve to a
    /// different key than the one actually cached and silently orphan the
    /// old slot forever instead of freeing it.
    #[tokio::test(flavor = "current_thread")]
    async fn register_agent_drops_the_pre_update_slot_even_when_profile_changes() {
        let pool = OdooConnectorPool::default();
        let old_key = pool.pool_key("alpha-pm").await;
        assert_eq!(old_key, ("alpha-pm".to_string(), "default".to_string()));
        {
            let mut inner = pool.pool.write().await;
            inner.insert(old_key.clone(), Arc::new(Mutex::new(PoolSlot::default())));
        }
        assert_eq!(pool.slots().await.len(), 1, "precondition: one seeded slot");

        // The override changes `profile`, which changes the pool key.
        pool.register_agent(
            "alpha-pm",
            AgentOdooConfig {
                profile: Some("alpha".into()),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            pool.pool_key("alpha-pm").await,
            ("alpha-pm".to_string(), "alpha".to_string()),
            "sanity check: the key really did change"
        );
        assert!(
            pool.slots().await.is_empty(),
            "the slot cached under the pre-update key must be dropped, not orphaned"
        );
    }

    /// Same-profile override: the key is unchanged, so this exercises the
    /// ordinary (non-orphaning) path — the one slot under the one key is
    /// still dropped.
    #[tokio::test(flavor = "current_thread")]
    async fn register_agent_drops_its_slot_when_profile_is_unchanged() {
        let pool = OdooConnectorPool::default();
        let key = pool.pool_key("beta-pm").await;
        {
            let mut inner = pool.pool.write().await;
            inner.insert(key.clone(), Arc::new(Mutex::new(PoolSlot::default())));
        }

        pool.register_agent(
            "beta-pm",
            AgentOdooConfig {
                allowed_models: vec!["res.partner".into()],
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            pool.pool_key("beta-pm").await,
            key,
            "profile unchanged ⇒ same key"
        );
        assert!(pool.slots().await.is_empty());
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

        let creds = OdooConnectorPool::merge_credentials(&global, None, |enc: String| async move {
            Ok(format!("dec({enc})"))
        })
        .await
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

        let creds = OdooConnectorPool::merge_credentials(
            &global,
            Some(&agent_cfg),
            |enc: String| async move { Ok(format!("dec({enc})")) },
        )
        .await
        .unwrap();
        assert_eq!(creds.credential, "dec(ENC_ALPHA)");
        assert_eq!(creds.config.username, "alpha_user");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_errors_when_no_credential_present() {
        let global = OdooConfig::default(); // empty
        let err = OdooConnectorPool::merge_credentials(&global, None, |_: String| async {
            Ok(String::new())
        })
        .await
        .unwrap_err();
        assert!(err.contains("no Odoo credential"), "got: {err}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_passes_secret_reference_to_async_resolver() {
        // A `secret://` reference must reach the async resolver verbatim — the
        // pool itself does not interpret it, the resolver does.
        let mut global = OdooConfig::default();
        global.api_key_enc = "secret://vault/odoo-api-key".into();

        let creds =
            OdooConnectorPool::merge_credentials(&global, None, |cred: String| async move {
                assert!(cred.starts_with("secret://"));
                Ok(format!("resolved({cred})"))
            })
            .await
            .unwrap();
        assert_eq!(creds.credential, "resolved(secret://vault/odoo-api-key)");
    }

    // ── B1: per-agent url / db override ──────────────────────────────────

    /// Minimal fake Odoo JSON-RPC endpoint on 127.0.0.1 (plain HTTP is
    /// allowed for strict localhost). Every request gets `result: 7`; the
    /// returned counter is the number of requests served.
    async fn fake_odoo() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let counter = counter.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let _ = sock.read(&mut buf).await;
                    counter.fetch_add(1, Ordering::SeqCst);
                    let body = r#"{"jsonrpc":"2.0","id":1,"result":7}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (url, hits)
    }

    fn global_with(url: &str) -> OdooConfig {
        OdooConfig {
            url: url.into(),
            db: "global_db".into(),
            username: "u".into(),
            api_key_enc: "ENC".into(),
            ..OdooConfig::default()
        }
    }

    async fn dec(enc: String) -> Result<String, String> {
        Ok(format!("dec({enc})"))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_applies_url_and_db_override() {
        let global = global_with("https://global.example.com");
        let over = AgentOdooConfig {
            url: Some("https://alpha.example.com".into()),
            db: Some("alpha_db".into()),
            ..Default::default()
        };
        let creds = OdooConnectorPool::merge_credentials(&global, Some(&over), dec)
            .await
            .unwrap();
        assert_eq!(creds.config.url, "https://alpha.example.com");
        assert_eq!(creds.config.db, "alpha_db");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn merge_credentials_refuses_ssrf_url_override() {
        let global = global_with("https://global.example.com");
        for bad in ["https://169.254.169.254", "http://erp.example.com", "https://192.168.1.9"] {
            let over = AgentOdooConfig { url: Some(bad.into()), ..Default::default() };
            let err = OdooConnectorPool::merge_credentials(&global, Some(&over), dec)
                .await
                .unwrap_err();
            assert!(err.contains("Odoo 網址"), "{bad}: {err}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_override_connects_to_its_own_server_not_global() {
        let (global_url, global_hits) = fake_odoo().await;
        let (agent_url, agent_hits) = fake_odoo().await;
        let pool = OdooConnectorPool::new(global_with(&global_url));
        pool.register_agent(
            "alpha",
            AgentOdooConfig { url: Some(agent_url.clone()), db: Some("alpha_db".into()), ..Default::default() },
        )
        .await;
        let conn = pool.get_or_connect("alpha", dec).await.expect("connect");
        assert_eq!(conn.status().url, agent_url);
        assert_eq!(conn.status().db, "alpha_db");
        assert!(agent_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        assert_eq!(global_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        // An agent without an override still uses the global server.
        let other = pool.get_or_connect("beta", dec).await.expect("connect");
        assert_eq!(other.status().url, global_url);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn changed_url_never_reuses_cached_connector() {
        let (url_a, _) = fake_odoo().await;
        let (url_b, hits_b) = fake_odoo().await;
        let pool = OdooConnectorPool::new(global_with(&url_a));
        pool.register_agent(
            "alpha",
            AgentOdooConfig { url: Some(url_a.clone()), ..Default::default() },
        )
        .await;
        let first = pool.get_or_connect("alpha", dec).await.unwrap();
        assert_eq!(first.status().url, url_a);
        // Same endpoint ⇒ cached connector is reused.
        let again = pool.get_or_connect("alpha", dec).await.unwrap();
        assert!(Arc::ptr_eq(&first, &again));

        // Change only the URL, keeping the profile (so the pool key is
        // unchanged): the slot must not hand back the old session.
        pool.register_agent(
            "alpha",
            AgentOdooConfig { url: Some(url_b.clone()), ..Default::default() },
        )
        .await;
        let moved = pool.get_or_connect("alpha", dec).await.unwrap();
        assert_eq!(moved.status().url, url_b);
        assert!(hits_b.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_slot_is_replaced_even_without_re_registration() {
        // Defence in depth: swap the resolver's global url under a live slot
        // via a path that does not clear the pool (`register_agent` clears,
        // so go through the resolver directly).
        let (url_a, _) = fake_odoo().await;
        let (url_b, _) = fake_odoo().await;
        let pool = OdooConnectorPool::new(global_with(&url_a));
        let first = pool.get_or_connect("alpha", dec).await.unwrap();
        assert_eq!(first.status().url, url_a);
        pool.resolver.write().await.set_global(global_with(&url_b));
        let second = pool.get_or_connect("alpha", dec).await.unwrap();
        assert_eq!(second.status().url, url_b);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unsafe_override_url_fails_closed_even_with_cached_connector() {
        let (url_a, _) = fake_odoo().await;
        let pool = OdooConnectorPool::new(global_with(&url_a));
        pool.get_or_connect("alpha", dec).await.unwrap();
        pool.register_agent(
            "alpha",
            AgentOdooConfig { url: Some("https://169.254.169.254".into()), ..Default::default() },
        )
        .await;
        let err = pool.get_or_connect("alpha", dec).await.err().expect("must refuse");
        assert!(err.contains("Odoo 網址"), "{err}");
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
