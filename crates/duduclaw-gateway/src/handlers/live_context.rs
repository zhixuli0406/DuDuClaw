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
//! - any read error → the caller continues as an Employee with no bindings
//!   and no team ([`UserContext::narrowed`]), keeping a forced-password-change
//!   flag it already had;
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

use duduclaw_auth::{UserContext, UserDb};

use super::MethodHandler;
use crate::protocol::WsFrame;

/// Same text `server.rs::jwt_account_gate` uses at handshake.
pub(crate) const INACTIVE_ACCOUNT_ERROR: &str = "account is suspended or offboarded";

/// Most cached identities; beyond it stale generations are dropped first,
/// then everything.
const LIVE_CONTEXT_CACHE_CAP: usize = 4096;

/// `user_id → (acl_generation, context)`.
#[derive(Default)]
pub(crate) struct LiveContextCache {
    entries: Mutex<HashMap<String, (u64, UserContext)>>,
}

impl LiveContextCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u64, UserContext)>> {
        // A panicked holder leaves plain data behind; keep serving.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn get(&self, user_id: &str, generation: u64) -> Option<UserContext> {
        self.lock()
            .get(user_id)
            .filter(|(g, _)| *g == generation)
            .map(|(_, ctx)| ctx.clone())
    }

    fn put(&self, ctx: &UserContext) {
        let mut entries = self.lock();
        if entries.len() >= LIVE_CONTEXT_CACHE_CAP && !entries.contains_key(&ctx.user_id) {
            let current = ctx.acl_generation;
            entries.retain(|_, (g, _)| *g == current);
            if entries.len() >= LIVE_CONTEXT_CACHE_CAP {
                entries.clear();
            }
        }
        entries.insert(ctx.user_id.clone(), (ctx.acl_generation, ctx.clone()));
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }
}

/// Rebuild `jwt_ctx` from the database. `Err` only for a missing or
/// non-Active account; read errors narrow instead (see the module docs).
pub(crate) fn resolve_live_context(
    db: &UserDb,
    cache: &LiveContextCache,
    jwt_ctx: &UserContext,
) -> Result<UserContext, String> {
    let generation = db.acl_generation();
    if let Ok(g) = generation {
        if let Some(hit) = cache.get(&jwt_ctx.user_id, g) {
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
            Ok(live)
        }
        Ok(None) => Err(INACTIVE_ACCOUNT_ERROR.to_string()),
        Err(e) => {
            tracing::warn!(
                user = %duduclaw_core::truncate_chars(&jwt_ctx.user_id, 12),
                error = %e,
                "live ACL read failed — continuing with no bindings"
            );
            Ok(jwt_ctx.narrowed())
        }
    }
}

impl MethodHandler {
    /// The context this RPC runs under. See the module docs.
    pub(crate) async fn live_context(&self, ctx: &UserContext) -> Result<UserContext, WsFrame> {
        if ctx.user_id == "system" {
            return Ok(ctx.clone());
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return Ok(ctx.clone());
        };
        resolve_live_context(&db, &self.live_ctx_cache, ctx)
            .map_err(|e| WsFrame::error_response("", &e))
    }
}
