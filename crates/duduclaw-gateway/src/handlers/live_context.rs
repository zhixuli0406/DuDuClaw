//! Live identity for every RPC.
//!
//! A WebSocket keeps the `UserContext` it built at handshake for as long as
//! it stays open, and an access token lives 30 minutes. Before this, a role
//! change, an unbinding or a suspension did not reach an open connection
//! until the next login. `MethodHandler::dispatch` now rebuilds the context
//! from `users.db` on every call: the JWT only proves who the caller is
//! (`sub`); role, bindings, team grants and the forced-password-change flag
//! come from the database.
//!
//! Cost: one single-row read of `auth_meta.acl_generation` per RPC. Every
//! authorization-relevant write in `duduclaw-auth` bumps that counter in the
//! same transaction, so a cached context keyed by `(user_id, generation)` is
//! never stale; on a miss the full context is read from one snapshot.
//!
//! Failure handling (fail closed):
//! - user missing or not Active → the call is refused with
//!   [`INACTIVE_ACCOUNT_ERROR`] (the handshake's own wording);
//! - any read error → the call is refused with [`READ_FAILURE_ERROR`] (the
//!   handshake's wording for the same situation). Nothing from the token is
//!   reused: an unreadable store cannot prove the role, the bindings or that
//!   the forced-password-change flag is clear. Each failure is logged and
//!   counted ([`read_failure_stats`], shown by `system.doctor`);
//! - the generation cannot be read → the cache is bypassed, never trusted.
//!
//! Consistency with `review_evidence::audience::fresh_dashboard_context`:
//! both require an Active account and read role and bindings fresh. That
//! helper refuses a caller who still has to change their password; here the
//! live flag is carried on the context and the dispatch gate right after
//! restricts the caller to the self-service allowlist, so neither path lets
//! such a caller reach a protected RPC.
//!
//! Left as they were: the `system` principal (gateway admin token / no-account
//! local fallback, nothing in `users.db` can revoke it), pre-auth lock-screen
//! connections (no identity at all) and a handler with no user database wired
//! in (no JWT session can exist without one; in-process test harnesses).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use duduclaw_auth::{UserContext, UserDb};

use super::MethodHandler;
use crate::protocol::WsFrame;

/// Same text `server.rs::jwt_account_gate` uses at handshake.
pub(crate) const INACTIVE_ACCOUNT_ERROR: &str = "account is suspended or offboarded";

/// Same text `server.rs::authenticate_jwt` uses when the user store cannot
/// be read at handshake.
pub(crate) const READ_FAILURE_ERROR: &str = "authentication service unavailable";

/// Live sockets distinguish revoked credentials from a temporarily unreadable store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketValidity {
    Valid,
    Revoked,
    Unavailable,
}

impl SocketValidity {
    pub(crate) fn from_error(error: &str) -> Self {
        if error == INACTIVE_ACCOUNT_ERROR || error == "登入憑證已失效，請重新登入。" {
            Self::Revoked
        } else {
            Self::Unavailable
        }
    }
}

/// Only heartbeat failures consume the recovery window; traffic cannot shorten it.
#[derive(Default)]
pub(crate) struct SocketRecovery {
    failed_heartbeats: u8,
    paused: bool,
    closed: bool,
}

impl SocketRecovery {
    pub(crate) fn observe(&mut self, validity: SocketValidity, heartbeat: bool) -> bool {
        if self.closed { return false; }
        match validity {
            SocketValidity::Valid => { self.failed_heartbeats = 0; self.paused = false; }
            SocketValidity::Revoked => self.closed = true,
            SocketValidity::Unavailable => {
                self.paused = true;
                if heartbeat {
                    self.failed_heartbeats += 1;
                    self.closed = self.failed_heartbeats >= 3;
                }
            }
        }
        if self.closed {
            log_socket_close(if validity == SocketValidity::Revoked {
                "credential revoked or expired"
            } else { "identity unavailable for three heartbeats" });
        }
        !self.closed && !self.paused
    }

    pub(crate) fn closed(&self) -> bool { self.closed }
}

fn log_socket_close(reason: &str) {
    static LAST_LOG: AtomicU64 = AtomicU64::new(0);
    let now = chrono::Utc::now().timestamp().max(1) as u64;
    let last = LAST_LOG.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 30
        && LAST_LOG.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        tracing::warn!(reason, "closing authenticated socket");
    }
}

/// Most cached identities; beyond it stale generations are dropped first,
/// then everything.
const LIVE_CONTEXT_CACHE_CAP: usize = 4096;

/// `user_id → (acl_generation, context)`.
#[derive(Default)]
pub(crate) struct LiveContextCache {
    entries: Mutex<HashMap<String, (u64, UserContext, std::time::Instant)>>,
}

impl LiveContextCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u64, UserContext, std::time::Instant)>> {
        // A panicked holder leaves plain data behind; keep serving.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, user_id: &str, generation: u64) -> Option<UserContext> {
        self.lock()
            .get(user_id)
            .filter(|(g, _, _)| *g == generation)
            .map(|(_, ctx, _)| ctx.clone())
    }

    fn put(&self, ctx: &UserContext) {
        let mut entries = self.lock();
        if entries.len() >= LIVE_CONTEXT_CACHE_CAP && !entries.contains_key(&ctx.user_id) {
            let current = ctx.acl_generation;
            entries.retain(|_, (g, _, _)| *g == current);
            if entries.len() >= LIVE_CONTEXT_CACHE_CAP {
                entries.clear();
            }
        }
        entries.insert(ctx.user_id.clone(), (ctx.acl_generation, ctx.clone(), std::time::Instant::now()));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

/// Rebuild `jwt_ctx` from the database. `Err` for a missing or non-Active
/// account and for any read error (see the module docs).
pub(crate) fn resolve_live_context(
    db: &UserDb,
    cache: &LiveContextCache,
    jwt_ctx: &UserContext,
) -> Result<UserContext, String> {
    let generation = db.acl_generation();
    if let Ok(g) = generation {
        if let Some(hit) = cache.get(&jwt_ctx.user_id, g) {
            if !duduclaw_auth::mfa::credential_epoch_matches(jwt_ctx.credential_epoch, hit.credential_epoch) { return Err("登入憑證已失效，請重新登入。".into()); }
            return Ok(hit);
        }
    }
    match db.live_user_context(&jwt_ctx.user_id) {
        Ok(Some(live)) => {
            // Cache only under a generation read before the snapshot and
            // equal to it: a write in between makes the entry unreachable
            // instead of stale.
            if generation.as_ref().is_ok_and(|g| *g == live.acl_generation) {
                cache.put(&live);
            }
            if !duduclaw_auth::mfa::credential_epoch_matches(jwt_ctx.credential_epoch, live.credential_epoch) { return Err("登入憑證已失效，請重新登入。".into()); }
            Ok(live)
        }
        Ok(None) => Err(INACTIVE_ACCOUNT_ERROR.to_string()),
        Err(e) => {
            tracing::warn!(
                user = %duduclaw_core::truncate_chars(&jwt_ctx.user_id, 12),
                error = %e,
                "live ACL read failed — refusing the call"
            );
            record_read_failure();
            Err(READ_FAILURE_ERROR.to_string())
        }
    }
}

/// Socket messages reuse the same identity reader/cache as RPCs, with the
/// existing push-cache window. Heartbeats always force a generation check.
/// A message checks expiry independently and does not read SQLite on a cache hit.
pub(crate) fn resolve_socket_context(db: &UserDb, cache: &LiveContextCache,
    ctx: &UserContext, heartbeat: bool) -> Result<UserContext, String> {
    if !heartbeat {
        let entries = cache.lock();
        if let Some((_, live, at)) = entries.get(&ctx.user_id) {
            if at.elapsed() < std::time::Duration::from_secs(super::push_filter::PUSH_CACHE_SECS) {
                return if duduclaw_auth::mfa::credential_epoch_matches(ctx.credential_epoch, live.credential_epoch) {
                    Ok(live.clone())
                } else { Err("登入憑證已失效，請重新登入。".into()) };
            }
        }
    }
    let live = resolve_live_context(db, cache, ctx).inspect_err(|_| {
        // Do not let a failed forced read fall back to a previously valid hit.
        cache.lock().remove(&ctx.user_id);
    })?;
    cache.put(&live);
    Ok(live)
}

/// Live ACL reads that failed (and so refused a call) since the gateway started.
static READ_FAILURES: AtomicU64 = AtomicU64::new(0);
/// Unix seconds of the most recent failure (0 = never).
static LAST_FAILURE_UNIX: AtomicU64 = AtomicU64::new(0);

fn record_read_failure() {
    READ_FAILURES.fetch_add(1, Ordering::Relaxed);
    let now = chrono::Utc::now().timestamp().max(0) as u64;
    LAST_FAILURE_UNIX.store(now, Ordering::Relaxed);
}

/// `(failures since start, unix seconds of the last one or 0)`, reported by
/// `system.doctor` so a broken identity store is visible, not just logged.
pub(crate) fn read_failure_stats() -> (u64, u64) {
    (
        READ_FAILURES.load(Ordering::Relaxed),
        LAST_FAILURE_UNIX.load(Ordering::Relaxed),
    )
}

impl MethodHandler {
    pub(crate) async fn socket_context(&self, ctx: &UserContext, heartbeat: bool) -> Result<UserContext, SocketValidity> {
        if ctx.user_id == "system" {
            return Ok(crate::machine_employee::without_restricted_access(&self.home_dir, ctx.clone()));
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return Ok(crate::machine_employee::without_restricted_access(&self.home_dir, ctx.clone()));
        };
        let cache = self.live_ctx_cache.clone();
        let ctx = ctx.clone();
        match tokio::task::spawn_blocking(move || resolve_socket_context(&db, &cache, &ctx, heartbeat)).await {
            // Keep the shared cache raw; preset changes must narrow every return.
            Ok(Ok(live)) => Ok(crate::machine_employee::without_restricted_access(&self.home_dir, live)),
            Ok(Err(e)) => Err(SocketValidity::from_error(&e)),
            Err(_) => Err(SocketValidity::Unavailable),
        }
    }

    /// The context this RPC runs under. See the module docs.
    pub(crate) async fn live_context(&self, ctx: &UserContext) -> Result<UserContext, WsFrame> {
        if ctx.user_id == "system" {
            return Ok(crate::machine_employee::without_restricted_access(&self.home_dir, ctx.clone()));
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return Ok(crate::machine_employee::without_restricted_access(&self.home_dir, ctx.clone()));
        };
        // SQLite reads are blocking; keep them off the runtime workers.
        let cache = self.live_ctx_cache.clone();
        let jwt_ctx = ctx.clone();
        let resolved = tokio::task::spawn_blocking(move || resolve_live_context(&db, &cache, &jwt_ctx)).await;
        match resolved {
            Ok(Ok(mut live)) => {
                // Team grants are an Enterprise feature: a Personal home
                // (e.g. after a downgrade with reporting lines left in
                // users.db) gets none (`team_edition.rs`).
                if !live.team_access.is_empty() && self.resolve_edition_profile().await.is_personal() {
                    live.team_access.clear();
                }
                // Preset bindings do not bump users.db's generation. Narrow
                // every returned snapshot, after the raw cache lookup.
                Ok(crate::machine_employee::without_restricted_access(&self.home_dir, live))
            }
            Ok(Err(e)) => Err(WsFrame::error_response("", &e)),
            Err(join) => {
                // The blocking task panicked or was cancelled: same as a
                // failed read.
                tracing::warn!(error = %join, "live ACL task failed — refusing the call");
                record_read_failure();
                Err(WsFrame::error_response("", READ_FAILURE_ERROR))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(id: &str, generation: u64) -> UserContext {
        let mut c = UserContext::admin_fallback();
        c.user_id = id.to_string();
        c.acl_generation = generation;
        c
    }

    #[test]
    fn a_full_cache_drops_stale_generations_first_then_everything() {
        let cache = LiveContextCache::default();
        for i in 0..LIVE_CONTEXT_CACHE_CAP {
            cache.put(&ctx(&format!("u{i}"), 1));
        }
        assert_eq!(cache.len(), LIVE_CONTEXT_CACHE_CAP);
        // Re-putting a known user never evicts.
        cache.put(&ctx("u0", 1));
        assert_eq!(cache.len(), LIVE_CONTEXT_CACHE_CAP);

        // A newcomer at a newer generation: every generation-1 entry is
        // unreachable anyway and goes.
        cache.put(&ctx("fresh", 2));
        assert_eq!(cache.len(), 1);
        assert!(cache.get("fresh", 2).is_some());
        assert!(cache.get("u1", 1).is_none());

        // Full of current entries: a newcomer clears the map.
        for i in 0..LIVE_CONTEXT_CACHE_CAP - 1 {
            cache.put(&ctx(&format!("v{i}"), 2));
        }
        assert_eq!(cache.len(), LIVE_CONTEXT_CACHE_CAP);
        cache.put(&ctx("newcomer", 2));
        assert_eq!(cache.len(), 1);
        assert!(cache.get("newcomer", 2).is_some());
        // A hit needs the same generation.
        assert!(cache.get("newcomer", 3).is_none());
    }
}

#[cfg(test)]
mod mfa_socket_tests {
    use super::*;
    #[test]
    fn mfa_socket_cache_skips_message_reads_and_heartbeat_fails_closed() {
        let home=tempfile::tempdir().unwrap();
        let path=home.path().join("users.db");
        let db=UserDb::new(&path).unwrap();
        let user=db.create_user("cache@example.com","Cache","password-test",duduclaw_auth::UserRole::Admin).unwrap();
        let ctx=db.live_user_context(&user.id).unwrap().unwrap();
        let cache=LiveContextCache::default();
        resolve_socket_context(&db,&cache,&ctx,true).unwrap();
        // No SQLite read on a message cache hit, even if the database breaks.
        let raw=rusqlite::Connection::open(path).unwrap();
        raw.execute("DROP TABLE auth_meta",[]).unwrap();
        assert!(resolve_socket_context(&db,&cache,&ctx,false).is_ok());
        assert!(resolve_socket_context(&db,&cache,&ctx,true).is_err());
        assert!(!cache.lock().contains_key(&ctx.user_id));
        assert!(resolve_socket_context(&db,&cache,&ctx,false).is_err());
    }
}

#[cfg(test)]
mod mfa_recovery_tests {
    use super::*;

    #[test]
    fn mfa_unavailable_withholds_frames_recovers_and_resets_window() {
        let mut recovery = SocketRecovery::default();
        assert!(recovery.observe(SocketValidity::Valid, true));
        assert!(!recovery.observe(SocketValidity::Unavailable, true));
        assert!(!recovery.closed());
        for _ in 0..10 { assert!(!recovery.observe(SocketValidity::Unavailable, false)); }
        assert!(recovery.observe(SocketValidity::Valid, true));
        assert!(!recovery.observe(SocketValidity::Unavailable, true));
        assert!(!recovery.observe(SocketValidity::Unavailable, true));
        assert!(!recovery.closed());
        assert!(!recovery.observe(SocketValidity::Unavailable, true));
        assert!(recovery.closed());
        assert!(!recovery.observe(SocketValidity::Valid, false));
    }

    #[test]
    fn mfa_revoked_drops_every_queued_frame_immediately() {
        let mut recovery = SocketRecovery::default();
        assert!(!recovery.observe(SocketValidity::Revoked, false));
        assert!(recovery.closed());
        let queued = ["rpc response", "os event", "sensitive push"];
        let sent: Vec<_> = queued.into_iter().filter(|_| recovery.observe(SocketValidity::Valid, false)).collect();
        assert!(sent.is_empty());
    }

    #[test]
    fn mfa_identity_read_recovers_after_one_failure_and_detects_retirement() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("users.db");
        let db = UserDb::new(&path).unwrap();
        let user = db.create_user("recover@example.com", "Recovery", "password-test", duduclaw_auth::UserRole::Admin).unwrap();
        let ctx = db.live_user_context(&user.id).unwrap().unwrap();
        let cache = LiveContextCache::default();
        let raw = rusqlite::Connection::open(path).unwrap();
        resolve_socket_context(&db, &cache, &ctx, true).unwrap();
        raw.execute("ALTER TABLE auth_meta RENAME TO saved_auth_meta", []).unwrap();
        let error = resolve_socket_context(&db, &cache, &ctx, true).unwrap_err();
        assert_eq!(SocketValidity::from_error(&error), SocketValidity::Unavailable);
        raw.execute("ALTER TABLE saved_auth_meta RENAME TO auth_meta", []).unwrap();
        resolve_socket_context(&db, &cache, &ctx, false).unwrap();
        db.mfa_operator_reset(&user.email).unwrap();
        // The send gate uses the two-second cache, not a per-frame DB read.
        cache.lock().get_mut(&ctx.user_id).unwrap().2 -= std::time::Duration::from_secs(3);
        let error = resolve_socket_context(&db, &cache, &ctx, false).unwrap_err();
        assert_eq!(SocketValidity::from_error(&error), SocketValidity::Revoked);
    }
}
