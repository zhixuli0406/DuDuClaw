//! Channel user → AI-employee (agent) binding for shared-bot deployments (WP9).
//!
//! Motivation: a company can run ONE shared Telegram bot (the global
//! `config.toml` token) instead of one bot token per employee. Each employee
//! scans a QR / opens a deep-link (`https://t.me/<bot>?start=<token>`) that the
//! dashboard minted for a specific agent; the `/start` payload binds their
//! Telegram user id to that agent, and every later message from that user is
//! routed to their bound agent. This sidesteps Telegram's multi-bot account
//! lockout risk while still giving per-employee identity.
//!
//! ## Two persisted structures (one file)
//!
//! - **Bindings**: `(channel, external_user_id) → agent_id`, durable. Looked up
//!   on every inbound message from the shared bot.
//! - **Bind tokens**: one-time, TTL-bounded, use-capped grants keyed by the
//!   SHA-256 digest of the plaintext token (plaintext exists only in the
//!   dashboard response / the deep-link). Redeeming a token writes a binding.
//!
//! ## Cross-process state
//!
//! Token generation happens in the dashboard RPC handler; redemption happens
//! on the Telegram poll loop; both may run in the same gateway process but the
//! store is written the same fail-closed way as [`crate::access_control`]:
//! reloaded before every operation and rewritten atomically (temp + rename)
//! under an advisory file lock (coding convention #3). Missing / corrupt file
//! ⇒ empty state (no bindings, no tokens) — fail-closed, never a panic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Default time-to-live for a freshly minted bind token.
pub const DEFAULT_TTL_MINUTES: i64 = 15;
/// Default number of redemptions allowed per bind token.
pub const DEFAULT_MAX_USES: u32 = 1;
/// Hard cap on redemptions to keep a single token from becoming a broadcast link.
pub const MAX_USES_LIMIT: u32 = 100;
/// Hard cap on TTL (24h) to keep a token from living indefinitely.
pub const MAX_TTL_MINUTES: i64 = 24 * 60;

/// A pending one-time bind token (persisted; plaintext never stored).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingBind {
    /// SHA-256 hex digest of the plaintext token (also the map key).
    token_sha256: String,
    /// Channel this token is valid for (exact match — never cross-channel).
    channel: String,
    /// Target agent id the redeeming user gets bound to.
    agent_id: String,
    expires_at: DateTime<Utc>,
    max_uses: u32,
    used: u32,
}

impl PendingBind {
    fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
    fn is_exhausted(&self) -> bool {
        self.used >= self.max_uses
    }
}

/// On-disk state: durable bindings + pending one-time tokens.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct BindState {
    /// `channel → (external_user_id → agent_id)`.
    #[serde(default)]
    bindings: HashMap<String, HashMap<String, String>>,
    /// Pending bind tokens keyed by SHA-256 digest of the plaintext.
    #[serde(default)]
    tokens: HashMap<String, PendingBind>,
    #[serde(default)]
    migration_receipts: HashMap<String, MigrationBindingReceipt>,
}

#[derive(Debug,Clone,Serialize,Deserialize,PartialEq,Eq)]
#[serde(deny_unknown_fields)]
struct MigrationBindingReceipt {digest:String,channel:String,user:String,agent:String}

/// Why a bind-token redemption failed (kept distinct for logging; the channel
/// surface collapses all of these to one friendly zh-TW rejection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindRedeemError {
    /// No such token (unknown / already consumed-and-pruned / wrong string).
    Invalid,
    /// Token exists but its TTL has elapsed.
    Expired,
    /// Token exists but its use budget is spent.
    Exhausted,
    /// Token exists but was minted for a different channel.
    ChannelMismatch,
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// Persistent store of channel-user → agent bindings and one-time bind tokens.
pub struct AgentBindingStore {
    state: Arc<RwLock<BindState>>,
    persist_path: Option<PathBuf>,
}

impl AgentBindingStore {
    /// In-memory only (tests).
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(BindState::default())),
            persist_path: None,
        }
    }

    /// File-backed store shared across gateway subsystems / processes.
    pub fn with_persistence(persist_path: PathBuf) -> Self {
        Self {
            state: Arc::new(RwLock::new(load_state(&persist_path))),
            persist_path: Some(persist_path),
        }
    }

    async fn reload(&self) {
        if let Some(path) = &self.persist_path {
            *self.state.write().await = load_state(path);
        }
    }

    /// Every ordinary mutation reloads and commits under the same lock as
    /// migration. No stale in-memory snapshot may replace another writer.
    async fn mutate<T>(&self, action:impl FnOnce(&mut BindState)->T)->std::io::Result<T> {
        if let Some(path)=&self.persist_path {
            let (value,state)=duduclaw_core::with_file_lock(path,|| {
                let mut state=read_durable_state(path)?;
                let value=action(&mut state);
                write_durable_state(path,&state)?;
                Ok((value,state))
            })?;
            *self.state.write().await=state;
            Ok(value)
        } else {
            let mut state=self.state.write().await;
            Ok(action(&mut state))
        }
    }

    /// Mint a one-time bind token for `agent_id` on `channel`. Returns the
    /// plaintext token (32 hex chars — URL/deep-link safe). `ttl_minutes` and
    /// `max_uses` are clamped to sane bounds; `0` falls back to the defaults.
    pub async fn generate_bind_token(
        &self,
        channel: &str,
        agent_id: &str,
        ttl_minutes: i64,
        max_uses: u32,
    ) -> String {
        let ttl = if ttl_minutes <= 0 {
            DEFAULT_TTL_MINUTES
        } else {
            ttl_minutes.min(MAX_TTL_MINUTES)
        };
        let uses = if max_uses == 0 {
            DEFAULT_MAX_USES
        } else {
            max_uses.min(MAX_USES_LIMIT)
        };

        // 128-bit random token, hex-encoded → matches Telegram's deep-link
        // start-payload charset ([A-Za-z0-9_-], ≤64 chars).
        let token = format!("{:032x}", uuid::Uuid::new_v4().as_u128());
        let digest = sha256_hex(&token);
        let saved=self.mutate(|state| {
            prune_expired(state);
            state.tokens.insert(
                digest.clone(),
                PendingBind {
                    token_sha256: digest,
                    channel: channel.to_string(),
                    agent_id: agent_id.to_string(),
                    expires_at: Utc::now() + chrono::Duration::minutes(ttl),
                    max_uses: uses,
                    used: 0,
                },
            );
        }).await;
        if let Err(error)=saved {warn!(%error,"bind token persistence failed");return String::new();}
        info!(
            channel,
            agent_id,
            ttl_minutes = ttl,
            max_uses = uses,
            "bind token generated"
        );
        token
    }

    /// Redeem a plaintext bind token for `user_id` on `channel`. On success the
    /// binding `(channel, user_id) → agent_id` is written and the token's use
    /// count is incremented (removed when exhausted). Fail-closed: unknown /
    /// expired / exhausted / cross-channel tokens are rejected and never bind.
    pub async fn redeem_bind_token(
        &self,
        channel: &str,
        token: &str,
        user_id: &str,
    ) -> Result<String, BindRedeemError> {
        if token.is_empty() || user_id.is_empty() {
            return Err(BindRedeemError::Invalid);
        }
        let digest = sha256_hex(token);
        let now = Utc::now();

        let outcome = self.mutate(|state| {
            let Some(pending) = state.tokens.get(&digest).cloned() else {
                return Err(BindRedeemError::Invalid);
            };
            // Exact channel match — a telegram token can never bind a slack user.
            if pending.channel != channel {
                return Err(BindRedeemError::ChannelMismatch);
            }
            if pending.is_expired(now) {
                state.tokens.remove(&digest);
                Err(BindRedeemError::Expired)
            } else if pending.is_exhausted() {
                state.tokens.remove(&digest);
                Err(BindRedeemError::Exhausted)
            } else {
                // Consume one use; write the binding.
                if let Some(p) = state.tokens.get_mut(&digest) {
                    p.used += 1;
                    if p.is_exhausted() {
                        state.tokens.remove(&digest);
                    }
                }
                // A fresh external redemption replaces migration provenance,
                // even when it requests the same employee.
                state.migration_receipts.retain(|_,r|r.channel!=channel||r.user!=user_id);
                state
                    .bindings
                    .entry(channel.to_string())
                    .or_default()
                    .insert(user_id.to_string(), pending.agent_id.clone());
                Ok(pending.agent_id)
            }
        }).await.unwrap_or(Err(BindRedeemError::Invalid));
        if let Ok(agent) = &outcome {
            info!(channel, user_id, agent, "channel user bound to agent");
        }
        outcome
    }

    /// Look up the agent a channel user is bound to. Exact key match; `None`
    /// when unbound (the shared bot then falls back to its default behavior).
    pub async fn resolve_bound_agent(&self, channel: &str, user_id: &str) -> Option<String> {
        if user_id.is_empty() {
            return None;
        }
        self.reload().await;
        let state = self.state.read().await;
        state.bindings.get(channel)?.get(user_id).cloned()
    }

    /// Remove a binding (operator revoke / employee handoff). Returns true when
    /// a binding was actually removed.
    pub async fn unbind(&self, channel: &str, user_id: &str) -> bool {
        let removed=self.mutate(|state| {
            state.migration_receipts.retain(|_,r|r.channel!=channel||r.user!=user_id);
            state.bindings.get_mut(channel).is_some_and(|m|m.remove(user_id).is_some())
        }).await.unwrap_or(false);
        if removed {info!(channel,user_id,"channel user unbound");}
        removed
    }
}

impl AgentBindingStore {
    /// Same durable binding map as token redemption, with its operation receipt
    /// in the same atomic file replacement. Caller must hold a live Admin decision.
    pub(crate) fn migration_bind(&self,operation:&str,digest:&str,channel:&str,user:&str,agent:&str)->Result<(),String> {
        let receipt=MigrationBindingReceipt {digest:digest.into(),channel:channel.into(),user:user.into(),agent:agent.into()};
        self.migration_transaction(|state| {
            if let Some(old)=state.migration_receipts.get(operation) {
                if old==&receipt {
                    match state.bindings.get(channel).and_then(|m|m.get(user)) {
                        Some(a) if a==agent=>return Ok(()),
                        None=>{state.bindings.entry(channel.into()).or_default().insert(user.into(),agent.into());return Ok(());},
                        _=>{},
                    }
                }
                return Err("migration binding conflict".into());
            }
            if state.bindings.get(channel).is_some_and(|m|m.contains_key(user)) {return Err("migration binding already exists".into());}
            state.bindings.entry(channel.into()).or_default().insert(user.into(),agent.into());
            state.migration_receipts.insert(operation.into(),receipt);Ok(())
        })
    }
    pub(crate) fn migration_readback(&self,operation:&str,digest:&str,channel:&str,user:&str,agent:&str)->Result<(),String> {
        self.migration_transaction(|state| {
            let receipt=state.migration_receipts.get(operation).ok_or("migration binding receipt missing")?;
            if receipt.digest!=digest || receipt.channel!=channel || receipt.user!=user || receipt.agent!=agent
                || state.bindings.get(channel).and_then(|m|m.get(user)).map(String::as_str)!=Some(agent) {return Err("migration binding changed".into());}Ok(())
        })
    }
    pub(crate) fn migration_unbind(&self,operation:&str,digest:&str)->Result<(),String> {
        self.migration_transaction(|state| {
            let Some(receipt)=state.migration_receipts.get(operation).cloned() else {return Err("migration binding receipt unavailable".into());};
            if receipt.digest!=digest {return Err("migration binding digest changed".into());}
            let map=state.bindings.get_mut(&receipt.channel).ok_or("migration binding changed")?;
            if map.get(&receipt.user).is_some_and(|a|a!=&receipt.agent) {return Err("migration binding changed".into());}
            map.remove(&receipt.user);
            // Retain a receipt tombstone for idempotent disable/retry; a new
            // external redemption explicitly removes it before granting access.
            Ok(())
        })
    }
    fn migration_transaction(&self,action:impl FnOnce(&mut BindState)->Result<(),String>)->Result<(),String> {
        let path=self.persist_path.as_ref().ok_or("migration requires durable bindings")?;
        duduclaw_core::with_file_lock(path,|| {
            let mut state=read_durable_state(path)?;
            action(&mut state).map_err(|_|std::io::Error::other("migration binding refused"))?;
            write_durable_state(path,&state)
        }).map_err(|_|"migration binding transaction failed".into())
    }
}

impl Default for AgentBindingStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop expired tokens in place (called on generate to bound file growth).
fn prune_expired(state: &mut BindState) {
    let now = Utc::now();
    state.tokens.retain(|_, p| !p.is_expired(now));
}

/// Load state from disk. Missing / corrupt ⇒ empty (fail-closed).
fn load_state(path: &std::path::Path) -> BindState {
    let Ok(data) = std::fs::read_to_string(path) else {
        return BindState::default();
    };
    match serde_json::from_str::<BindState>(&data) {
        Ok(mut state) => {
            prune_expired(&mut state);
            state
        }
        Err(_) => {
            warn!(
                ?path,
                "agent_binding state file is corrupt — starting empty"
            );
            BindState::default()
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_round_trip_bind() {
        let store = AgentBindingStore::new();
        let token = store
            .generate_bind_token("telegram", "sales-bot", 15, 1)
            .await;
        // Before redeeming, the user is unbound.
        assert_eq!(store.resolve_bound_agent("telegram", "u123").await, None);
        // Redeem → bound.
        assert_eq!(
            store.redeem_bind_token("telegram", &token, "u123").await,
            Ok("sales-bot".to_string())
        );
        assert_eq!(
            store.resolve_bound_agent("telegram", "u123").await,
            Some("sales-bot".to_string())
        );
    }

    #[tokio::test]
    async fn test_token_single_use_by_default() {
        let store = AgentBindingStore::new();
        let token = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        assert!(
            store
                .redeem_bind_token("telegram", &token, "u1")
                .await
                .is_ok()
        );
        // Second redemption (even by a different user) is rejected — one-time.
        assert_eq!(
            store.redeem_bind_token("telegram", &token, "u2").await,
            Err(BindRedeemError::Invalid)
        );
        // The second user never got bound.
        assert_eq!(store.resolve_bound_agent("telegram", "u2").await, None);
    }

    #[tokio::test]
    async fn test_multi_use_token() {
        let store = AgentBindingStore::new();
        let token = store
            .generate_bind_token("telegram", "team-bot", 15, 3)
            .await;
        assert!(
            store
                .redeem_bind_token("telegram", &token, "a")
                .await
                .is_ok()
        );
        assert!(
            store
                .redeem_bind_token("telegram", &token, "b")
                .await
                .is_ok()
        );
        assert!(
            store
                .redeem_bind_token("telegram", &token, "c")
                .await
                .is_ok()
        );
        // 4th exceeds max_uses → rejected.
        assert_eq!(
            store.redeem_bind_token("telegram", &token, "d").await,
            Err(BindRedeemError::Invalid)
        );
        assert_eq!(
            store.resolve_bound_agent("telegram", "a").await,
            Some("team-bot".to_string())
        );
        assert_eq!(store.resolve_bound_agent("telegram", "d").await, None);
    }

    #[tokio::test]
    async fn test_expired_token_rejected() {
        let store = AgentBindingStore::new();
        // Negative-clamped TTL falls back to default, so inject an already
        // expired token directly to exercise the expiry branch.
        let token = "deadbeef";
        {
            let mut state = store.state.write().await;
            state.tokens.insert(
                sha256_hex(token),
                PendingBind {
                    token_sha256: sha256_hex(token),
                    channel: "telegram".into(),
                    agent_id: "late-bot".into(),
                    expires_at: Utc::now() - chrono::Duration::minutes(1),
                    max_uses: 1,
                    used: 0,
                },
            );
        }
        assert_eq!(
            store.redeem_bind_token("telegram", token, "u1").await,
            Err(BindRedeemError::Expired)
        );
        assert_eq!(store.resolve_bound_agent("telegram", "u1").await, None);
    }

    #[tokio::test]
    async fn test_channel_mismatch_rejected() {
        let store = AgentBindingStore::new();
        let token = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        // A telegram token must not bind a slack user.
        assert_eq!(
            store.redeem_bind_token("slack", &token, "u1").await,
            Err(BindRedeemError::ChannelMismatch)
        );
        assert_eq!(store.resolve_bound_agent("slack", "u1").await, None);
    }

    #[tokio::test]
    async fn test_multi_agent_isolation() {
        let store = AgentBindingStore::new();
        let tok_x = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        let tok_y = store
            .generate_bind_token("telegram", "agent-y", 15, 1)
            .await;
        assert!(
            store
                .redeem_bind_token("telegram", &tok_x, "userA")
                .await
                .is_ok()
        );
        assert!(
            store
                .redeem_bind_token("telegram", &tok_y, "userB")
                .await
                .is_ok()
        );
        // Each user resolves to their own agent — no cross-talk.
        assert_eq!(
            store.resolve_bound_agent("telegram", "userA").await,
            Some("agent-x".to_string())
        );
        assert_eq!(
            store.resolve_bound_agent("telegram", "userB").await,
            Some("agent-y".to_string())
        );
    }

    #[tokio::test]
    async fn test_rebind_overwrites() {
        let store = AgentBindingStore::new();
        let tok_x = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        assert!(
            store
                .redeem_bind_token("telegram", &tok_x, "u1")
                .await
                .is_ok()
        );
        let tok_y = store
            .generate_bind_token("telegram", "agent-y", 15, 1)
            .await;
        assert!(
            store
                .redeem_bind_token("telegram", &tok_y, "u1")
                .await
                .is_ok()
        );
        // Re-binding the same user replaces the target agent.
        assert_eq!(
            store.resolve_bound_agent("telegram", "u1").await,
            Some("agent-y".to_string())
        );
    }

    #[tokio::test]
    async fn test_empty_inputs_rejected() {
        let store = AgentBindingStore::new();
        assert_eq!(
            store.redeem_bind_token("telegram", "", "u1").await,
            Err(BindRedeemError::Invalid)
        );
        let token = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        assert_eq!(
            store.redeem_bind_token("telegram", &token, "").await,
            Err(BindRedeemError::Invalid)
        );
    }

    #[tokio::test]
    async fn test_cross_process_generate_then_redeem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent_bindings.json");

        // Dashboard side mints the token…
        let dashboard = AgentBindingStore::with_persistence(path.clone());
        let token = dashboard
            .generate_bind_token("telegram", "sales-bot", 15, 1)
            .await;

        // …the gateway poll loop (a distinct instance) redeems it.
        let gateway = AgentBindingStore::with_persistence(path.clone());
        assert_eq!(
            gateway.redeem_bind_token("telegram", &token, "u777").await,
            Ok("sales-bot".to_string())
        );

        // A third instance (≈ restart) still sees the durable binding.
        let after_restart = AgentBindingStore::with_persistence(path);
        assert_eq!(
            after_restart.resolve_bound_agent("telegram", "u777").await,
            Some("sales-bot".to_string())
        );
    }

    #[tokio::test]
    async fn test_use_count_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent_bindings.json");
        let a = AgentBindingStore::with_persistence(path.clone());
        let token = a.generate_bind_token("telegram", "agent-x", 15, 1).await;
        assert!(a.redeem_bind_token("telegram", &token, "u1").await.is_ok());
        // A fresh instance must see the token already consumed (no reuse).
        let b = AgentBindingStore::with_persistence(path);
        assert_eq!(
            b.redeem_bind_token("telegram", &token, "u2").await,
            Err(BindRedeemError::Invalid)
        );
    }

    #[tokio::test]
    async fn test_unbind() {
        let store = AgentBindingStore::new();
        let token = store
            .generate_bind_token("telegram", "agent-x", 15, 1)
            .await;
        store
            .redeem_bind_token("telegram", &token, "u1")
            .await
            .unwrap();
        assert!(store.unbind("telegram", "u1").await);
        assert_eq!(store.resolve_bound_agent("telegram", "u1").await, None);
        // Unbinding a non-existent binding is a no-op false.
        assert!(!store.unbind("telegram", "u1").await);
    }

    #[tokio::test]
    async fn test_max_uses_clamped() {
        let store = AgentBindingStore::new();
        // Request an absurd use count; it must be clamped to the hard limit.
        let token = store
            .generate_bind_token("telegram", "agent-x", 15, 10_000)
            .await;
        let digest = sha256_hex(&token);
        let state = store.state.read().await;
        assert_eq!(state.tokens.get(&digest).unwrap().max_uses, MAX_USES_LIMIT);
    }
}

#[cfg(test)] mod migration_tests {
    use super::*;
    #[tokio::test] async fn migration_authority_channel_normal_mutation_reloads_under_shared_lock() {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("agent_bindings.json");
        let ordinary=AgentBindingStore::with_persistence(path.clone());
        let migration=AgentBindingStore::with_persistence(path);
        // The ordinary store cached empty state before migration committed.
        migration.migration_bind("op","digest","telegram","123","sales").unwrap();
        let token=ordinary.generate_bind_token("telegram","support",15,1).await;
        assert!(!token.is_empty());
        migration.migration_readback("op","digest","telegram","123","sales").unwrap();
        ordinary.redeem_bind_token("telegram",&token,"456").await.unwrap();
        migration.migration_readback("op","digest","telegram","123","sales").unwrap();
        assert_eq!(migration.resolve_bound_agent("telegram","456").await.as_deref(),Some("support"));
        assert!(ordinary.unbind("telegram","456").await);
        migration.migration_readback("op","digest","telegram","123","sales").unwrap();
    }
    #[tokio::test] async fn migration_authority_channel_binding_receipt_retry_and_fresh_redemption_fence() {
        let temp=tempfile::tempdir().unwrap();let store=AgentBindingStore::with_persistence(temp.path().join("agent_bindings.json"));
        assert_eq!(store.resolve_bound_agent("telegram","123").await,None);
        store.migration_bind("operation","digest","telegram","123","sales").unwrap();store.migration_bind("operation","digest","telegram","123","sales").unwrap();
        store.migration_readback("operation","digest","telegram","123","sales").unwrap();assert_eq!(store.resolve_bound_agent("telegram","123").await.as_deref(),Some("sales"));
        store.migration_unbind("operation","digest").unwrap();store.migration_unbind("operation","digest").unwrap();assert_eq!(store.resolve_bound_agent("telegram","123").await,None);
        store.migration_bind("operation","digest","telegram","123","sales").unwrap();
        let token=store.generate_bind_token("telegram","sales",15,1).await;
        store.redeem_bind_token("telegram",&token,"123").await.unwrap();
        assert!(store.migration_unbind("operation","digest").is_err());assert_eq!(store.resolve_bound_agent("telegram","123").await.as_deref(),Some("sales"));
    }
}

fn read_durable_state(path:&std::path::Path)->std::io::Result<BindState> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file()&&m.len()<=1024*1024=>serde_json::from_str(&crate::handlers::config_commit::read_regular_config(path)?).map_err(|_|std::io::Error::other("binding state invalid")),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>Ok(BindState::default()),
        _=>Err(std::io::Error::other("binding state unavailable")),
    }
}
fn write_durable_state(path:&std::path::Path,state:&BindState)->std::io::Result<()> {
    let encoded=serde_json::to_string_pretty(state).map_err(|_|std::io::Error::other("binding encoding failed"))?;
    crate::handlers::config_commit::replace_owner_only(path,&encoded)?;
    std::fs::File::open(path.parent().ok_or_else(||std::io::Error::other("binding path"))?).and_then(|f|f.sync_all())
}

// ── F6h (isolation_label_stores, S9): global channel routes to one label ──

/// Routes to `agent` (exact label): durable channel-user bindings plus
/// unexpired bind tokens. Missing file ⇒ 0; unreadable or corrupt ⇒ `Err`
/// (never read as zero).
pub(crate) fn label_route_count(path:&std::path::Path,agent:&str)->std::io::Result<u64> {
    let mut state=read_durable_state(path)?;
    prune_expired(&mut state);
    let bound=state.bindings.values().flat_map(|users|users.values()).filter(|bound|bound.as_str()==agent).count();
    let tokens=state.tokens.values().filter(|token|token.agent_id==agent).count();
    Ok((bound+tokens) as u64)
}

/// Strip every route to `agent` (exact label) under the store lock: bindings
/// and bind tokens. Returns the stripped names (`binding:<channel>:<user>`,
/// `token:<channel>`; never a token digest). A missing file is never created.
pub(crate) fn strip_label_routes(path:&std::path::Path,agent:&str)->std::io::Result<Vec<String>> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(Vec::new()),
        _=>{}
    }
    duduclaw_core::with_file_lock(path,|| {
        let mut state=read_durable_state(path)?;
        let mut names=Vec::new();
        for (channel,users) in state.bindings.iter_mut() {
            users.retain(|user,bound| {
                let keep=bound.as_str()!=agent;
                if !keep { names.push(format!("binding:{channel}:{user}")); }
                keep
            });
        }
        state.bindings.retain(|_,users|!users.is_empty());
        state.tokens.retain(|_,token| {
            let keep=token.agent_id!=agent;
            if !keep { names.push(format!("token:{}",token.channel)); }
            keep
        });
        if !names.is_empty() { write_durable_state(path,&state)?; }
        names.sort();
        Ok(names)
    })
}
