//! One-time, re-runnable backfill of the origin columns on `sessions` rows
//! written before they existed (`origin_kind IS NULL`).
//!
//! Rules (best effort, and only where the id is reliable):
//! - `webchat:webchat:<owner_tag>:…` whose tag is `webchat_owner_tag` of
//!   exactly one account in `users.db` (active, suspended or offboarded) →
//!   `webchat_user` + that account. A tag shared by two accounts, a tag of no
//!   account (a website-widget visitor, a deleted account) and the
//!   pre-2026-08-03 connection-keyed ids (`webchat:webchat:<ip>:<conn>`) →
//!   `unknown`. The id alone cannot tell an employee from a widget visitor, so
//!   anything not matched to an account stays unknown (shown to nobody new).
//! - Messaging-app prefixes → `channel`; `odoolivechat:` → `public_visitor`;
//!   gateway-internal prefixes → `internal`; anything else → `unknown`
//!   ([`super::origin_of`] without a principal).
//!
//! Each row is updated only while its origin is still NULL, so a live turn
//! that stamped it first wins and a rerun changes nothing. Runs in the
//! gateway holding `<home>/locks/gateway.lock`, in batches.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{Connection, params};

use super::{OriginKind, SessionOrigin, origin_of};
use crate::session::SessionManager;

/// Rows updated per transaction.
pub const BACKFILL_BATCH: usize = 500;
/// Circuit breaker: at most this many batches per run (5 million rows).
const MAX_BATCHES: usize = 10_000;
/// Length of `webchat_owner_tag` (hex chars).
const OWNER_TAG_LEN: usize = 12;
const WEBCHAT_ACCOUNT_PREFIX: &str = "webchat:webchat:";
/// Audit event written when a run changed at least one row.
pub const BACKFILL_AUDIT_EVENT: &str = "session_origin_backfill";

/// `webchat_owner_tag(user_id) → user_id`, built from every account. A tag two
/// accounts share maps to nobody.
#[derive(Debug, Default)]
pub struct OwnerTagIndex {
    by_tag: HashMap<String, Option<String>>,
}

impl OwnerTagIndex {
    pub fn from_user_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> OwnerTagIndex {
        OwnerTagIndex::from_tagged(
            ids.into_iter()
                .map(|id| (crate::webchat::webchat_owner_tag(id), id)),
        )
    }

    /// From `(owner_tag, user_id)` pairs; a tag given for two different ids
    /// maps to nobody.
    fn from_tagged<'a>(pairs: impl IntoIterator<Item = (String, &'a str)>) -> OwnerTagIndex {
        let mut by_tag: HashMap<String, Option<String>> = HashMap::new();
        for (tag, id) in pairs {
            by_tag
                .entry(tag)
                .and_modify(|owner| {
                    if owner.as_deref() != Some(id) {
                        *owner = None;
                    }
                })
                .or_insert_with(|| Some(id.to_string()));
        }
        OwnerTagIndex { by_tag }
    }

    fn owner(&self, tag: &str) -> Option<&str> {
        self.by_tag.get(tag)?.as_deref()
    }

    /// Two accounts whose tags collide (a real 12-hex collision cannot be
    /// found in a test).
    #[cfg(test)]
    pub(crate) fn colliding_for_test(tag: &str, ids: &[&str]) -> OwnerTagIndex {
        OwnerTagIndex::from_tagged(ids.iter().map(|id| (tag.to_string(), *id)))
    }
}

/// The origin a legacy row gets from its id (see the module rules).
pub fn legacy_origin(session_id: &str, index: &OwnerTagIndex) -> SessionOrigin {
    if let Some(rest) = session_id.strip_prefix(WEBCHAT_ACCOUNT_PREFIX) {
        return match rest.split_once(':') {
            Some((tag, conn)) if is_owner_tag(tag) && !conn.is_empty() => index
                .owner(tag)
                .map(SessionOrigin::webchat_user)
                .unwrap_or_else(SessionOrigin::unknown),
            _ => SessionOrigin::unknown(),
        };
    }
    // Any other WebChat id has no principal here and maps to unknown.
    origin_of(session_id, None)
}

fn is_owner_tag(tag: &str) -> bool {
    tag.len() == OWNER_TAG_LEN
        && tag
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Rows changed per origin kind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackfillCounts {
    pub by_kind: BTreeMap<OriginKind, u64>,
}

impl BackfillCounts {
    pub fn total(&self) -> u64 {
        self.by_kind.values().sum()
    }

    fn add(&mut self, kind: OriginKind, n: u64) {
        if n > 0 {
            *self.by_kind.entry(kind).or_default() += n;
        }
    }

    fn merge(&mut self, other: &BackfillCounts) {
        for (k, n) in &other.by_kind {
            self.add(*k, *n);
        }
    }

    fn to_json(&self) -> serde_json::Value {
        let by_kind: serde_json::Map<String, serde_json::Value> = self
            .by_kind
            .iter()
            .map(|(k, n)| (k.as_str().to_string(), (*n).into()))
            .collect();
        serde_json::json!({ "updated": self.total(), "by_kind": by_kind })
    }
}

/// Stamp up to `limit` NULL-origin rows in one transaction. Returns the rows
/// changed and how many NULL rows were selected (0 ⇒ done).
pub(crate) fn backfill_batch(
    conn: &Connection,
    index: &OwnerTagIndex,
    limit: usize,
    now: &str,
) -> rusqlite::Result<(BackfillCounts, usize)> {
    let tx = conn.unchecked_transaction()?;
    let ids: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT id FROM sessions WHERE origin_kind IS NULL ORDER BY rowid LIMIT ?1")?;
        stmt.query_map(params![limit as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut counts = BackfillCounts::default();
    for id in &ids {
        let origin = legacy_origin(id, index);
        let n = tx.execute(
            "UPDATE sessions SET origin_kind = ?2, owner_user_id = ?3, origin_at = ?4
             WHERE id = ?1 AND origin_kind IS NULL",
            params![id, origin.kind().as_str(), origin.owner_user_id(), now],
        )?;
        counts.add(origin.kind(), n as u64);
    }
    tx.commit()?;
    Ok((counts, ids.len()))
}

impl SessionManager {
    /// Backfill every NULL-origin row, one pooled connection per batch so
    /// live turns are not starved.
    pub async fn backfill_origins(&self, index: &OwnerTagIndex) -> Result<BackfillCounts> {
        let mut total = BackfillCounts::default();
        for _ in 0..MAX_BATCHES {
            let now = chrono::Utc::now().to_rfc3339();
            let (counts, selected) = {
                let conn = self.acquire().await;
                backfill_batch(&conn, index, BACKFILL_BATCH, &now)
                    .map_err(|e| DuDuClawError::Gateway(format!("origin backfill: {e}")))?
            };
            total.merge(&counts);
            if selected < BACKFILL_BATCH {
                return Ok(total);
            }
            tokio::task::yield_now().await;
        }
        tracing::warn!("session origin backfill stopped after {MAX_BATCHES} batches");
        Ok(total)
    }
}

/// Build the tag index from `users.db` and backfill. Writes one
/// [`BACKFILL_AUDIT_EVENT`] (Info) with the per-kind counts when anything
/// changed. A failure leaves the remaining rows NULL (they read as unknown).
pub async fn run(
    home: &Path,
    sessions: &SessionManager,
    users: Arc<duduclaw_auth::UserDb>,
) -> std::result::Result<BackfillCounts, String> {
    let ids = tokio::task::spawn_blocking(move || users.list_users())
        .await
        .map_err(|e| format!("user list task failed: {e}"))??;
    let index = OwnerTagIndex::from_user_ids(ids.iter().map(|u| u.id.as_str()));
    let counts = sessions
        .backfill_origins(&index)
        .await
        .map_err(|e| e.to_string())?;
    if counts.total() > 0 {
        duduclaw_security::audit::append_audit_event(
            home,
            &duduclaw_security::audit::AuditEvent::new(
                BACKFILL_AUDIT_EVENT,
                "gateway",
                duduclaw_security::audit::Severity::Info,
                counts.to_json(),
            ),
        );
    }
    Ok(counts)
}

/// Run [`run`] once in the background (boot).
pub fn spawn(
    home: std::path::PathBuf,
    sessions: Arc<SessionManager>,
    users: Arc<duduclaw_auth::UserDb>,
) {
    tokio::spawn(async move {
        match run(&home, &sessions, users).await {
            Ok(c) if c.total() > 0 => {
                tracing::info!(updated = c.total(), "session origin backfill done")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("session origin backfill failed: {e}"),
        }
    });
}
