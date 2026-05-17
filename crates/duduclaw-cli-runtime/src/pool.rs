//! `PtyPool` — caches long-lived [`PtySession`]s keyed by `(agent_id, cli_kind,
//! bare_mode)`, enforces per-agent concurrency via [`tokio::sync::Semaphore`],
//! and evicts idle sessions on a background tick.
//!
//! Phase 2 implementation lives here. Wiring into gateway happens in Phase 3.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::error::{PoolError, SessionError};
use crate::session::{CliKind, PtySession};
use crate::supervisor::Supervisor;

/// Composite key identifying a poolable session slot.
///
/// `bare_mode` is part of the key because a `--bare` Claude session is *not*
/// interchangeable with a non-bare one (different auth, different ambient
/// context loading semantics — see TODO #15 in TODO-runtime-health-fixes).
///
/// **Phase 3.D.2**: `account_id` is added so multi-account OAuth rotation
/// segregates sessions per account. When `None` (default, back-compat),
/// all callers share one pool slot per `(agent_id, cli_kind, bare_mode)`.
/// When `Some("alice@example.com")`, each account gets its own session,
/// preserving Account → Conversation context continuity that the
/// rotator's account selection assumes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentKey {
    pub agent_id: String,
    pub cli_kind: CliKind,
    pub bare_mode: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// **Review fix**: per-agent model selection. Was silently dropped in
    /// the PTY OAuth path — sessions used the CLI's default model
    /// regardless of the agent's `[model] preferred` setting. Adding it
    /// to the cache key means two invocations with different models for
    /// the same agent get distinct sessions (correct: a session boot
    /// includes `--model X`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl AgentKey {
    /// Back-compat constructor — account_id + model stay unset.
    pub fn new(agent_id: impl Into<String>, cli_kind: CliKind, bare_mode: bool) -> Self {
        Self {
            agent_id: agent_id.into(),
            cli_kind,
            bare_mode,
            account_id: None,
            model: None,
        }
    }

    /// Phase 3.D.2 — constructor with explicit account scope.
    pub fn with_account(
        agent_id: impl Into<String>,
        cli_kind: CliKind,
        bare_mode: bool,
        account_id: Option<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            cli_kind,
            bare_mode,
            account_id,
            model: None,
        }
    }

    /// **Review fix**: full builder including model.
    pub fn with_account_and_model(
        agent_id: impl Into<String>,
        cli_kind: CliKind,
        bare_mode: bool,
        account_id: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            cli_kind,
            bare_mode,
            account_id,
            model,
        }
    }

    fn cache_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.agent_id,
            self.cli_kind.as_str(),
            if self.bare_mode { "bare" } else { "std" },
            self.account_id.as_deref().unwrap_or("default"),
            self.model.as_deref().unwrap_or("default"),
        )
    }

    /// Round 4 fix (CRIT-1): redacted variant used for tracing. The raw
    /// `cache_key` embeds `account_id`, which in the gateway is set to
    /// `oauth-<first12chars-of-OAuth-token>` (see
    /// `channel_reply::account_id_from_env_vars`). Writing that to disk
    /// via `info!` / `warn!` / `debug!` would persist OAuth token
    /// prefixes in long-lived log files. `log_key` substitutes a short
    /// non-reversible fingerprint of the account_id while keeping the
    /// agent / kind / model fields untouched (those are operationally
    /// useful and not sensitive).
    fn log_key(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.agent_id,
            self.cli_kind.as_str(),
            if self.bare_mode { "bare" } else { "std" },
            self.account_id
                .as_deref()
                .map(redact_account_id)
                .unwrap_or_else(|| "default".to_string()),
            self.model.as_deref().unwrap_or("default"),
        )
    }
}

/// Round 4 fix (CRIT-1): convert a (possibly secret-bearing) account_id
/// into an 8-char non-reversible tag for log lines. We can't pull in
/// `sha2` here without growing the crate's dependency footprint, so we
/// use FNV-1a 64-bit then truncate. FNV is not cryptographic; the goal
/// is **not** to resist deliberate reversal — the goal is to avoid
/// trivially-readable OAuth token prefixes ending up in disk logs. For
/// correlation across log lines we only need same-input-same-output.
fn redact_account_id(account_id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in account_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("acct-{:08x}", (hash as u32))
}

/// Round 4 fix (CRIT-1): redact a previously-serialised `cache_key`
/// string. `tick_eviction` iterates `sessions` by inserted key (the
/// raw cache_key) and only has the String, not the original
/// `AgentKey`. Format is `<agent>:<kind>:<bare|std>:<account>:<model>`
/// — exactly 4 colons; if anything else, fall back to a generic tag
/// rather than letting an unrecognised key shape through (fail-safe).
fn redact_cache_key_str(cache_key: &str) -> String {
    let parts: Vec<&str> = cache_key.splitn(5, ':').collect();
    if parts.len() != 5 {
        return "<malformed-cache-key>".to_string();
    }
    let account = if parts[3] == "default" {
        "default".to_string()
    } else {
        redact_account_id(parts[3])
    };
    format!("{}:{}:{}:{}:{}", parts[0], parts[1], parts[2], account, parts[4])
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Max concurrent in-flight invokes per agent. CLI binaries don't support
    /// reentrant invocation on a single TUI, so the default is 1.
    pub max_per_agent: usize,
    /// Idle timeout — sessions untouched for this duration are evicted.
    pub idle_timeout: Duration,
    /// How often the background eviction task tries to clean up.
    pub eviction_interval: Duration,
    /// Default invoke deadline if the caller doesn't override.
    pub default_invoke_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_per_agent: 1,
            idle_timeout: Duration::from_secs(10 * 60),
            eviction_interval: Duration::from_secs(60),
            default_invoke_timeout: Duration::from_secs(300),
        }
    }
}

/// A spawn-factory closure. Decoupled from the pool so the gateway can decide
/// how to build the SpawnOpts (e.g. resolve `which claude` per account, layer
/// in `--resume <id>`, inject env from the rotator).
///
/// The factory receives the `AgentKey` so it can pick account + binary + flags.
pub type SpawnFactory = Arc<
    dyn Fn(AgentKey) -> futures_compat::SpawnFuture + Send + Sync + 'static,
>;

/// Trait-object-friendly future return type for the factory. We don't pull
/// `futures` into the dep tree — just `Pin<Box<dyn Future + Send>>`.
pub mod futures_compat {
    use std::future::Future;
    use std::pin::Pin;

    use crate::error::SessionError;
    use crate::session::PtySession;
    use std::sync::Arc;

    pub type SpawnFuture =
        Pin<Box<dyn Future<Output = Result<Arc<PtySession>, SessionError>> + Send + 'static>>;
}

pub struct PtyPool {
    sessions: DashMap<String, Arc<PtySession>>,
    semaphores: DashMap<String, Arc<Semaphore>>,
    factory: SpawnFactory,
    config: PoolConfig,
    supervisor: Arc<Supervisor>,
    shutdown: CancellationToken,
}

impl PtyPool {
    pub fn new(factory: SpawnFactory, config: PoolConfig) -> Arc<Self> {
        let pool = Arc::new(Self {
            sessions: DashMap::new(),
            semaphores: DashMap::new(),
            factory,
            config,
            supervisor: Arc::new(Supervisor::default()),
            shutdown: CancellationToken::new(),
        });
        // Phase 2: kick off the eviction task. Held by the pool's lifetime.
        let weak = Arc::downgrade(&pool);
        let interval = pool.config.eviction_interval;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.tick().await; // discard first immediate tick
            loop {
                tick.tick().await;
                let Some(pool) = weak.upgrade() else { break };
                if pool.shutdown.is_cancelled() {
                    break;
                }
                pool.tick_eviction().await;
            }
            debug!("PtyPool eviction task exited");
        });
        pool
    }

    /// Returns the number of currently-cached sessions (for diagnostics / tests).
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// **Review fix (CRITICAL #1)**: non-spawning existence check. Used by
    /// `handle_shutdown_session` in the worker to avoid spawning a fresh
    /// session just to immediately destroy it — which was a token-cost
    /// and availability trap.
    ///
    /// **Round 2 caveat**: a `true` return is a point-in-time observation.
    /// The session may be evicted by the background tick before the next
    /// instruction. Returning `true` does NOT guarantee the session is
    /// healthy; for atomic "remove if present", use
    /// [`PtyPool::remove_if_present`].
    pub fn contains_key(&self, key: &AgentKey) -> bool {
        self.sessions.contains_key(&key.cache_key())
    }

    /// **Round 2 review fix (HIGH-1)**: atomically remove a cached
    /// session without going through `acquire` (which spawns on
    /// cache-miss). Closes the TOCTOU race where
    /// `contains_key=true` → background eviction removes → `acquire`
    /// spawns a fresh session just to immediately destroy it.
    ///
    /// Returns `true` when a session was actually present and shut
    /// down; `false` when nothing was cached. Cooperative with the
    /// background eviction tick — both use `DashMap::remove` so only
    /// one wins, and the loser observes `false`.
    pub async fn remove_if_present(&self, key: &AgentKey) -> bool {
        let cache_key = key.cache_key();
        if let Some((_, sess)) = self.sessions.remove(&cache_key) {
            self.supervisor.untrack(&cache_key);
            sess.shutdown().await;
            true
        } else {
            false
        }
    }

    /// Acquire (or spawn) a session for `key`. The returned [`PooledSession`]
    /// holds a semaphore permit; dropping it releases capacity back.
    pub async fn acquire(self: &Arc<Self>, key: AgentKey) -> Result<PooledSession, PoolError> {
        if self.shutdown.is_cancelled() {
            return Err(PoolError::ShuttingDown);
        }
        let cache_key = key.cache_key();
        let sem = self
            .semaphores
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(self.config.max_per_agent)))
            .clone();
        let permit = sem
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Exhausted(key.agent_id.clone()))?;

        // Try existing session first.
        if let Some(existing) = self.sessions.get(&cache_key).map(|v| v.value().clone()) {
            if existing.is_healthy() {
                debug!(key = %key.log_key(), "pool: reusing session");
                return Ok(PooledSession {
                    session: existing,
                    permit,
                    pool: Arc::downgrade(self),
                    cache_key,
                });
            }
            // Evict unhealthy + fall through to spawn.
            warn!(key = %key.log_key(), "pool: existing session unhealthy — replacing");
            if let Some((_, sess)) = self.sessions.remove(&cache_key) {
                self.supervisor.untrack(&cache_key);
                sess.shutdown().await;
            }
        }

        // Spawn new.
        let fut = (self.factory)(key.clone());
        let new_session = fut.await.map_err(PoolError::from)?;
        self.sessions
            .insert(cache_key.clone(), new_session.clone());
        self.supervisor.track(cache_key.clone(), new_session.clone());
        info!(key = %key.log_key(), pid = new_session.pid().unwrap_or(0), "pool: spawned new session");

        Ok(PooledSession {
            session: new_session,
            permit,
            pool: Arc::downgrade(self),
            cache_key,
        })
    }

    async fn tick_eviction(&self) {
        let cutoff = Instant::now().checked_sub(self.config.idle_timeout);
        let Some(cutoff) = cutoff else { return };

        let mut victims = Vec::new();
        for entry in self.sessions.iter() {
            let session = entry.value();
            if !session.is_healthy() {
                victims.push((entry.key().clone(), "unhealthy"));
            } else if session.last_used() < cutoff {
                victims.push((entry.key().clone(), "idle"));
            }
        }
        for (key, reason) in victims {
            if let Some((_, sess)) = self.sessions.remove(&key) {
                self.supervisor.untrack(&key);
                info!(key = %redact_cache_key_str(&key), reason, "pool: evicting session");
                sess.shutdown().await;
            }
        }
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let keys: Vec<String> = self.sessions.iter().map(|e| e.key().clone()).collect();
        for key in keys {
            if let Some((_, sess)) = self.sessions.remove(&key) {
                self.supervisor.untrack(&key);
                sess.shutdown().await;
            }
        }
    }
}

/// RAII guard returned from [`PtyPool::acquire`]. While alive, holds one
/// semaphore permit for the agent slot.
pub struct PooledSession {
    session: Arc<PtySession>,
    #[allow(dead_code)] // held for its Drop side-effect
    permit: OwnedSemaphorePermit,
    pool: std::sync::Weak<PtyPool>,
    cache_key: String,
}

impl PooledSession {
    pub fn session(&self) -> &PtySession {
        &self.session
    }

    pub fn arc(&self) -> Arc<PtySession> {
        self.session.clone()
    }

    pub fn cache_key(&self) -> &str {
        &self.cache_key
    }

    /// Convenience pass-through.
    pub async fn invoke(
        &self,
        prompt: &str,
        deadline: Option<Duration>,
    ) -> Result<String, SessionError> {
        self.session.invoke(prompt, deadline).await
    }

    /// Mark the session unhealthy so the pool evicts on next acquire. Use this
    /// when the caller observed a hard failure (e.g. CLI auth expired).
    pub fn invalidate(&self) {
        // We don't have a direct "mark unhealthy" flag on PtySession (Phase 1
        // relies on child-exit detection). Phase 2+ may add an explicit flag.
        // For now, callers can drop the session via `pool.shutdown_one(key)`
        // — TODO Phase 2.
        if let Some(pool) = self.pool.upgrade() {
            if let Some((_, sess)) = pool.sessions.remove(&self.cache_key) {
                pool.supervisor.untrack(&self.cache_key);
                tokio::spawn(async move { sess.shutdown().await });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SpawnOpts;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn redact_account_id_is_deterministic() {
        let a = redact_account_id("oauth-abc123def456");
        let b = redact_account_id("oauth-abc123def456");
        assert_eq!(a, b, "same input must produce same tag for log correlation");
        assert!(a.starts_with("acct-"));
        assert_eq!(a.len(), "acct-".len() + 8);
    }

    #[test]
    fn redact_account_id_does_not_leak_prefix() {
        let token_prefix = "oauth-abc123def456";
        let tag = redact_account_id(token_prefix);
        assert!(!tag.contains("abc123"), "tag must not echo input bytes");
        assert!(!tag.contains("def456"));
        assert!(!tag.starts_with("oauth-"));
    }

    #[test]
    fn log_key_redacts_account_but_keeps_other_fields() {
        let key = AgentKey::with_account_and_model(
            "alice",
            CliKind::Claude,
            false,
            Some("oauth-abcdef123456".to_string()),
            Some("claude-opus-4-7".to_string()),
        );
        let log_k = key.log_key();
        assert!(log_k.starts_with("alice:"));
        assert!(log_k.contains(":std:"));
        assert!(log_k.contains(":claude-opus-4-7"));
        assert!(!log_k.contains("oauth-abcdef"), "OAuth prefix must not appear in log: {log_k}");
        assert!(log_k.contains("acct-"));
    }

    #[test]
    fn log_key_preserves_default_when_account_absent() {
        let key = AgentKey::new("bob", CliKind::Claude, false);
        assert!(key.log_key().contains(":default:"));
    }

    #[test]
    fn redact_cache_key_str_handles_well_formed_keys() {
        let raw = "alice:claude:std:oauth-secret123:claude-opus-4-7";
        let redacted = redact_cache_key_str(raw);
        assert!(redacted.starts_with("alice:claude:std:acct-"));
        assert!(redacted.ends_with(":claude-opus-4-7"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn redact_cache_key_str_handles_default_account() {
        let raw = "alice:claude:std:default:claude-opus-4-7";
        let redacted = redact_cache_key_str(raw);
        assert_eq!(redacted, raw, "no redaction needed when account is the default sentinel");
    }

    #[test]
    fn redact_cache_key_str_fails_safe_for_malformed() {
        assert_eq!(redact_cache_key_str("not:a:valid"), "<malformed-cache-key>");
        assert_eq!(redact_cache_key_str(""), "<malformed-cache-key>");
    }

    fn cat_program() -> (String, Vec<String>) {
        #[cfg(unix)]
        {
            ("cat".to_string(), vec![])
        }
        #[cfg(windows)]
        {
            (
                "findstr".to_string(),
                vec!["/N".to_string(), "^".to_string()],
            )
        }
    }

    fn factory_for_cat() -> SpawnFactory {
        Arc::new(move |key: AgentKey| {
            Box::pin(async move {
                let (program, args) = cat_program();
                PtySession::spawn(SpawnOpts {
                    agent_id: key.agent_id.clone(),
                    cli_kind: key.cli_kind,
                    program,
                    extra_args: args,
                    env: HashMap::new(),
                    cwd: None,
                    session_id: None,
                    boot_timeout: Duration::from_millis(500),
                    default_invoke_timeout: Duration::from_secs(2),
                    rows: 24,
                    cols: 200,
                    interactive: false,
                    pre_trusted: false,
                })
                .await
            })
        })
    }

    #[tokio::test]
    async fn acquire_then_release_keeps_session_cached() {
        let pool = PtyPool::new(factory_for_cat(), PoolConfig::default());
        let key = AgentKey::new("agent-a", CliKind::Claude, false);
        {
            let lease = pool.acquire(key.clone()).await.expect("acquire 1");
            assert_eq!(lease.cache_key(), &key.cache_key());
            drop(lease);
        }
        assert_eq!(pool.session_count(), 1);
        // Second acquire should reuse the same session.
        let second = pool.acquire(key.clone()).await.expect("acquire 2");
        assert_eq!(second.cache_key(), &key.cache_key());
        drop(second);
        assert_eq!(pool.session_count(), 1);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_acquire_serialises_per_agent() {
        let pool = PtyPool::new(factory_for_cat(), PoolConfig::default());
        let counter = Arc::new(AtomicUsize::new(0));
        let pool2 = pool.clone();
        let counter2 = counter.clone();
        let h1 = tokio::spawn(async move {
            let lease = pool2
                .acquire(AgentKey::new("agent-b", CliKind::Claude, false))
                .await
                .unwrap();
            counter2.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(150)).await;
            drop(lease);
        });
        // Tiny delay so h1 wins acquire race.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let pool3 = pool.clone();
        let counter3 = counter.clone();
        let h2 = tokio::spawn(async move {
            let lease = pool3
                .acquire(AgentKey::new("agent-b", CliKind::Claude, false))
                .await
                .unwrap();
            // Must observe h1 already incremented before we got our permit.
            assert!(counter3.load(Ordering::SeqCst) >= 1);
            drop(lease);
        });
        h1.await.unwrap();
        h2.await.unwrap();
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn idle_eviction_drops_old_sessions() {
        let config = PoolConfig {
            idle_timeout: Duration::from_millis(50),
            eviction_interval: Duration::from_millis(80),
            ..PoolConfig::default()
        };
        let pool = PtyPool::new(factory_for_cat(), config);
        let key = AgentKey::new("agent-c", CliKind::Claude, false);
        {
            let _lease = pool.acquire(key.clone()).await.unwrap();
        }
        assert_eq!(pool.session_count(), 1);
        // Wait long enough for idle eviction tick.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(pool.session_count(), 0, "session should have been evicted");
        pool.shutdown().await;
    }
}
