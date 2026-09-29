//! Opt-in agent-wiki sources with fail-closed file and trust-state rechecks.
//!
//! Wiki pages are outside SQLite. Every causal-store open compares active
//! imported page versions with their live files and current trust state before
//! returning any claims, evidence, models, or effect estimates.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::causal::{CausalStore, CausalStoreError, EvidenceScope, SourceArtifact, now};
use crate::trust_store::WikiTrustStore;
use crate::wiki::WikiStore;

pub const WIKI_ACL: &str = "agent-private";
pub const SHARED_WIKI_TENANT: &str = "workspace";
pub const SHARED_WIKI_ACL: &str = "shared-wiki";
const MAX_ACTIVE_WIKI_SOURCES: usize = 10_000;
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

struct LiveWikiPage {
    body: String,
    raw_sha256: String,
    body_sha256: String,
    updated_at: i64,
}

fn shared_policy_digest(home: &Path, page_path: &str) -> Option<String> {
    let path = home.join("shared/wiki/.scope.toml");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() || meta.len() > 64 * 1024 => return None,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return None,
    };
    if content.len() > 64 * 1024 {
        return None;
    }
    let table: toml::Table = if content.trim().is_empty() {
        toml::Table::new()
    } else {
        content.parse().ok()?
    };
    let namespace = page_path.split('/').next()?;
    let namespaces = match table.get("namespaces") {
        Some(value) => Some(value.as_table()?),
        None => None,
    };
    let declared = match namespaces.and_then(|namespaces| namespaces.get(namespace)) {
        Some(value) => Some(value.as_table()?),
        None => None,
    };
    let mode = match declared.and_then(|entry| entry.get("mode")) {
        Some(value) => value.as_str()?,
        None => "agent_writable",
    };
    if !matches!(mode, "agent_writable" | "read_only" | "operator_only") {
        return None;
    }
    let synced_from = match declared.and_then(|entry| entry.get("synced_from")) {
        Some(value) => value.as_str()?,
        None => "",
    };
    if mode == "read_only" && synced_from.trim().is_empty() {
        return None;
    }
    Some(format!(
        "{:x}",
        Sha256::digest(format!("{mode}\0{synced_from}").as_bytes())
    ))
}

fn live_shared_page(home: &Path, page_path: &str) -> Option<LiveWikiPage> {
    let wiki_dir = home.join("shared/wiki");
    let canonical_home = home.canonicalize().ok()?;
    if wiki_dir.canonicalize().ok()? != canonical_home.join("shared/wiki") {
        return None;
    }
    // Validate BEFORE touching the filesystem: an unvalidated `page_path`
    // would otherwise let the size probe below stat a path outside the wiki
    // tree (a file-existence/size oracle), even though the read itself is
    // refused.
    let store = WikiStore::new_shared(home);
    store.validate_page_path(page_path).ok()?;
    if std::fs::metadata(wiki_dir.join(page_path)).ok()?.len() > MAX_BODY_BYTES as u64 {
        return None;
    }
    let (page, raw) = store.read_page_with_raw(page_path).ok()?;
    if raw.len() > MAX_BODY_BYTES
        || page.body.is_empty()
        || page.body.len() > MAX_BODY_BYTES
        || page.do_not_inject
        || page.trust < 0.1
    {
        return None;
    }
    let policy = shared_policy_digest(home, page_path)?;
    let raw_sha = format!("{:x}", Sha256::digest(raw.as_bytes()));
    Some(LiveWikiPage {
        body_sha256: format!("{:x}", Sha256::digest(page.body.as_bytes())),
        raw_sha256: format!("{raw_sha}:{policy}"),
        body: page.body,
        updated_at: page.updated.timestamp(),
    })
}

pub(crate) fn home_for(db_path: &Path) -> Result<&Path, CausalStoreError> {
    db_path.parent().ok_or(CausalStoreError::InvalidInput)
}

/// Read-only liveness check for an imported Wiki source, usable outside a
/// write transaction.
///
/// `CausalCcrDeliveryLease::still_valid()` opens the database read-only on
/// purpose (it must never contend with, or be aborted by, the write path), so
/// it cannot run `sync_wiki_sources` — the only place a changed live page is
/// otherwise noticed. This compares the two digests pinned at import time
/// against the file on disk, which is all the check needs and costs no lock.
///
/// The trust store is consulted only when it is already open process-wide: a
/// `WikiTrustStore::open()` here would take the Wiki delivery fence in a hot
/// path. A trust-state change therefore still invalidates on the next
/// `CausalStore::open()`, not necessarily mid-delivery.
pub(crate) fn live_wiki_source_matches(
    db_path: &Path,
    kind: &str,
    tenant_id: &str,
    acl: &str,
    page_path: &str,
    version: &str,
    content_sha256: &str,
) -> bool {
    let Ok(home) = home_for(db_path) else {
        return false;
    };
    let trust = crate::trust_store::global_trust_store()
        .filter(|store| store.is_backed_by(&home.join("wiki_trust.db")));
    let live = match kind {
        "wiki_agent" if acl == WIKI_ACL => {
            live_page(home, tenant_id, page_path, trust.as_deref())
        }
        "wiki_shared" if tenant_id == SHARED_WIKI_TENANT && acl == SHARED_WIKI_ACL => {
            live_shared_page(home, page_path)
        }
        _ => return false,
    };
    live.is_some_and(|live| live.raw_sha256 == version && live.body_sha256 == content_sha256)
}

fn trust_store(home: &Path) -> Result<Option<WikiTrustStore>, CausalStoreError> {
    let path = home.join("wiki_trust.db");
    if !path.exists() {
        return Ok(None);
    }
    if let Some(global) = crate::trust_store::global_trust_store() {
        if global.is_backed_by(&path) {
            return Ok(Some((*global).clone()));
        }
    }
    // W2-B: fence contention is not malformed input. Mapping it to
    // `InvalidInput` made every `/api/causal/*` call answer 400 "invalid
    // causal evidence input" while a Wiki write held the fence.
    WikiTrustStore::open(&path).map(Some).map_err(|error| {
        if crate::wiki_fence::is_fence_busy(&error) {
            CausalStoreError::Busy
        } else {
            CausalStoreError::InvalidInput
        }
    })
}

fn live_page(
    home: &Path,
    agent_id: &str,
    page_path: &str,
    trust: Option<&WikiTrustStore>,
) -> Option<LiveWikiPage> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return None;
    }
    let wiki_dir = home.join("agents").join(agent_id).join("wiki");
    let canonical_home = home.canonicalize().ok()?;
    let canonical_wiki = wiki_dir.canonicalize().ok()?;
    // Do not follow a symlinked agent/wiki root outside the owner's home.
    if canonical_wiki != canonical_home.join("agents").join(agent_id).join("wiki") {
        return None;
    }
    // Validate BEFORE the size probe — see `live_shared_page`.
    let store = WikiStore::new(wiki_dir.clone());
    store.validate_page_path(page_path).ok()?;
    if std::fs::metadata(wiki_dir.join(page_path)).ok()?.len() > MAX_BODY_BYTES as u64 {
        return None;
    }
    let (page, raw) = store.read_page_with_raw(page_path).ok()?;
    if raw.len() > MAX_BODY_BYTES
        || page.do_not_inject
        || page.trust < 0.1
        || page.body.is_empty()
        || page.body.len() > MAX_BODY_BYTES
    {
        return None;
    }
    if let Some(trust) = trust {
        let state = trust.get(page_path, agent_id).ok()?;
        if state.is_some_and(|state| state.do_not_inject || state.trust < 0.1) {
            return None;
        }
    }
    Some(LiveWikiPage {
        raw_sha256: format!("{:x}", Sha256::digest(raw.as_bytes())),
        body_sha256: format!("{:x}", Sha256::digest(page.body.as_bytes())),
        body: page.body,
        updated_at: page.updated.timestamp(),
    })
}

/// Erase a changed/withdrawn Wiki source and everything copied out of it.
///
/// A source whose bytes are still being delivered under a CCR lease is left
/// alone: the `causal_ccr_prevent_leased_update` trigger would `RAISE(ABORT)`
/// this UPDATE, which — because `sync_wiki_sources` runs inside
/// `CausalStore::open()` — would fail EVERY causal read and write until the
/// lease is released. `sync_wiki_sources` recomputes staleness on each open,
/// so the skipped row is simply scrubbed on the next open after the lease
/// drops (the same "defer, never fail" rule the retention sweep uses).
fn scrub_wiki_artifact(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), CausalStoreError> {
    let changed = tx.execute(
        "UPDATE causal_artifacts SET content='',invalidated_at=?2
         WHERE id=?1 AND kind IN ('wiki_agent','wiki_shared') AND invalidated_at IS NULL
         AND NOT EXISTS (SELECT 1 FROM causal_ccr_delivery_leases l
          WHERE l.tenant_id=causal_artifacts.tenant_id
           AND l.acl=causal_artifacts.acl
           AND l.artifact_id=causal_artifacts.id
           AND l.version=causal_artifacts.version)",
        params![id, now()],
    )?;
    if changed == 0 {
        return Ok(());
    }
    tx.execute(
        "UPDATE causal_effect_estimates SET estimate=NULL,lower_bound=NULL,upper_bound=NULL,
         diagnostics_json='{}',identification_state='wiki_source_changed'
         WHERE data_snapshot_id=?1 OR model_id IN (
           SELECT me.model_id FROM causal_model_edges me JOIN causal_evidence e
           ON e.claim_id=me.claim_id WHERE e.artifact_id=?1)",
        [id],
    )?;
    tx.execute(
        "UPDATE causal_claims SET review_state='needs_review',reviewer=NULL,reviewed_at=NULL
         WHERE review_state='accepted' AND id IN (
           SELECT claim_id FROM causal_evidence WHERE artifact_id=?1)",
        [id],
    )?;
    tx.execute(
        "UPDATE causal_evidence SET excerpt='' WHERE artifact_id=?1",
        [id],
    )?;
    // Copied wording also lives in claim context and negative-control
    // rationales — same transaction, same rule as `erase_artifact`.
    crate::causal::scrub_copied_source_wording(tx, id)?;
    Ok(())
}

pub(crate) fn sync_wiki_sources(
    conn: &mut Connection,
    db_path: &Path,
) -> Result<(), CausalStoreError> {
    let home = home_for(db_path)?;
    let rows: Vec<(String, String, String, String, String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id,kind,tenant_id,acl,external_id,version,content_sha256
             FROM causal_artifacts WHERE kind IN ('wiki_agent','wiki_shared') AND invalidated_at IS NULL
             LIMIT ?1",
        )?;
        let iter = stmt.query_map([MAX_ACTIVE_WIKI_SOURCES as i64 + 1], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        iter.collect::<Result<Vec<_>, _>>()?
    };
    if rows.len() > MAX_ACTIVE_WIKI_SOURCES {
        return Err(CausalStoreError::InvalidInput);
    }
    if rows.is_empty() {
        return Ok(());
    }
    let trust = if rows.iter().any(|row| row.1 == "wiki_agent") {
        trust_store(home)?
    } else {
        None
    };
    let stale: Vec<String> = rows
        .into_iter()
        .filter_map(|(id, kind, tenant, acl, path, version, digest)| {
            let live = match kind.as_str() {
                "wiki_agent" if acl == WIKI_ACL => live_page(home, &tenant, &path, trust.as_ref()),
                "wiki_shared" if tenant == SHARED_WIKI_TENANT && acl == SHARED_WIKI_ACL => {
                    live_shared_page(home, &path)
                }
                _ => None,
            };
            if live.is_none_or(|live| live.raw_sha256 != version || live.body_sha256 != digest) {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for id in stale {
        scrub_wiki_artifact(&tx, &id)?;
    }
    tx.commit()?;
    Ok(())
}

impl CausalStore {
    /// Import a page already readable in the workspace-shared Wiki. The
    /// namespace policy is pinned into its version; any policy revision
    /// invalidates the old causal copy before a fresh import.
    pub fn import_shared_wiki_source(
        &self,
        page_path: &str,
    ) -> Result<SourceArtifact, CausalStoreError> {
        if page_path.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let home: PathBuf = home_for(self.path())?.into();
        let mut conn = self.open()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let live = live_shared_page(&home, page_path).ok_or(CausalStoreError::NotFound)?;
        let scope = EvidenceScope {
            tenant_id: SHARED_WIKI_TENANT.into(),
            acl: SHARED_WIKI_ACL.into(),
        };
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT id,ingested_at FROM causal_artifacts WHERE tenant_id=?1 AND acl=?2
             AND kind='wiki_shared' AND external_id=?3 AND version=?4",
                params![scope.tenant_id, scope.acl, page_path, live.raw_sha256],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (id, ingested_at) = if let Some((id, ingested_at)) = existing {
            let active: bool = tx.query_row(
                "SELECT invalidated_at IS NULL AND content_sha256=?2 AND content=?3
                 FROM causal_artifacts WHERE id=?1",
                params![id, live.body_sha256, live.body],
                |row| row.get(0),
            )?;
            if !active {
                return Err(CausalStoreError::Conflict);
            }
            (id, ingested_at)
        } else {
            let id = Uuid::new_v4().to_string();
            let ingested_at = now();
            tx.execute(
                "INSERT INTO causal_artifacts
                 (id,tenant_id,acl,kind,external_id,version,lineage_id,content_sha256,
                  content,occurred_at,ingested_at,retention_at)
                 VALUES (?1,?2,?3,'wiki_shared',?4,?5,?4,?6,?7,?8,?9,?10)",
                params![
                    id,
                    scope.tenant_id,
                    scope.acl,
                    page_path,
                    live.raw_sha256,
                    live.body_sha256,
                    live.body,
                    live.updated_at,
                    ingested_at,
                    i64::MAX
                ],
            )?;
            (id, ingested_at)
        };
        tx.commit()?;
        Ok(SourceArtifact {
            id,
            tenant_id: scope.tenant_id,
            acl: scope.acl,
            kind: "wiki_shared".into(),
            external_id: page_path.into(),
            version: live.raw_sha256,
            lineage_id: page_path.into(),
            content_sha256: live.body_sha256,
            occurred_at: live.updated_at,
            ingested_at,
            retention_at: i64::MAX,
        })
    }

    /// Copy one active agent page into the same agent-private causal scope.
    /// The raw-file digest is the version, so any frontmatter or body edit
    /// creates a new version after the old one is scrubbed.
    pub fn import_agent_wiki_source(
        &self,
        agent_id: &str,
        page_path: &str,
    ) -> Result<SourceArtifact, CausalStoreError> {
        if !duduclaw_core::is_valid_agent_id(agent_id) || page_path.trim().is_empty() {
            return Err(CausalStoreError::InvalidInput);
        }
        let home: PathBuf = home_for(self.path())?.into();
        let mut conn = self.open()?;
        let trust = trust_store(&home)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let live = live_page(&home, agent_id, page_path, trust.as_ref())
            .ok_or(CausalStoreError::NotFound)?;
        let scope = EvidenceScope {
            tenant_id: agent_id.into(),
            acl: WIKI_ACL.into(),
        };
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT id,ingested_at FROM causal_artifacts WHERE tenant_id=?1 AND acl=?2
             AND kind='wiki_agent' AND external_id=?3 AND version=?4",
                params![scope.tenant_id, scope.acl, page_path, live.raw_sha256],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (id, ingested_at) = if let Some((id, ingested_at)) = existing {
            let active: bool = tx.query_row(
                "SELECT invalidated_at IS NULL AND content_sha256=?2 AND content=?3
                 FROM causal_artifacts WHERE id=?1",
                params![id, live.body_sha256, live.body],
                |row| row.get(0),
            )?;
            if !active {
                return Err(CausalStoreError::Conflict);
            }
            (id, ingested_at)
        } else {
            let id = Uuid::new_v4().to_string();
            let ingested_at = now();
            tx.execute(
                "INSERT INTO causal_artifacts
                 (id,tenant_id,acl,kind,external_id,version,lineage_id,content_sha256,
                  content,occurred_at,ingested_at,retention_at)
                 VALUES (?1,?2,?3,'wiki_agent',?4,?5,?4,?6,?7,?8,?9,?10)",
                params![
                    id,
                    scope.tenant_id,
                    scope.acl,
                    page_path,
                    live.raw_sha256,
                    live.body_sha256,
                    live.body,
                    live.updated_at,
                    ingested_at,
                    i64::MAX
                ],
            )?;
            (id, ingested_at)
        };
        tx.commit()?;
        Ok(SourceArtifact {
            id,
            tenant_id: scope.tenant_id,
            acl: scope.acl,
            kind: "wiki_agent".into(),
            external_id: page_path.into(),
            version: live.raw_sha256,
            lineage_id: page_path.into(),
            content_sha256: live.body_sha256,
            occurred_at: live.updated_at,
            ingested_at,
            retention_at: i64::MAX,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::causal::{ClaimModality, EvidenceStance};

    fn ccr_acl_revision(scope: &EvidenceScope) -> String {
        format!(
            "immutable-acl-sha256:{:x}",
            Sha256::digest(format!("{}\0{}", scope.tenant_id, scope.acl))
        )
    }

    /// Regression (W2-B/b): fence contention on the trust home used to be
    /// reported as `InvalidInput`, which surfaced to operators as HTTP 400
    /// "invalid causal evidence input" — an answer with no relation to the
    /// real cause. It is now its own transient kind.
    #[test]
    fn trust_store_fence_contention_reports_busy_not_invalid_input() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        // Force the migration path: an existing but schema-incomplete DB
        // makes `WikiTrustStore::open` require the exclusive home fence.
        crate::trust_store::WikiTrustStore::open(home.join("wiki_trust.db")).unwrap();
        let conn = rusqlite::Connection::open(home.join("wiki_trust.db")).unwrap();
        conn.execute_batch("DROP TRIGGER wiki_trust_ccr_after_update")
            .unwrap();
        drop(conn);
        let _held = crate::wiki_fence::WikiDeliveryFence::for_trust_home(home)
            .try_shared()
            .unwrap();
        assert!(matches!(trust_store(home), Err(CausalStoreError::Busy)));
    }

    fn artifact_content(store: &CausalStore, id: &str) -> String {
        Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT content FROM causal_artifacts WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// F1 + F3 regression in one: with a CCR delivery lease held,
    /// (a) `CausalStore::open()` used to fail outright — `scrub_wiki_artifact`
    /// hit the `causal_ccr_prevent_leased_update` trigger's `RAISE(ABORT)`, so
    /// every `/api/causal/*` call returned an error until the lease dropped;
    /// and (b) `still_valid()` never noticed a live page change at all, because
    /// it opens read-only and therefore skips `sync_wiki_sources`.
    #[test]
    fn a_leased_wiki_source_defers_its_scrub_and_fails_still_valid_on_page_edit() {
        let (_dir, store, page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        let lease = store
            .acquire_ccr_delivery_lease(
                &scope,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&scope),
            )
            .unwrap();
        assert!(lease.still_valid());

        // The page is rewritten while the copied bytes are still in flight.
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-03\ntrust: 0.9\n---\nA changes C.\n",
        ).unwrap();

        // Production builds a store per request, so the next request's first
        // open always resyncs; one instance reused across an out-of-band page
        // edit has to drop its maintenance throttle to stand in for that.
        store.reset_maintenance_throttle();
        store
            .open()
            .expect("a held delivery lease must defer the scrub, not fail every causal read");
        assert!(
            !artifact_content(&store, &source.id).is_empty(),
            "the leased copy stays until the lease drops"
        );
        assert!(
            !lease.still_valid(),
            "a live page edit must invalidate the in-flight delivery lease"
        );

        drop(lease);
        store.reset_maintenance_throttle();
        store.open().unwrap();
        assert!(
            artifact_content(&store, &source.id).is_empty(),
            "the next open after the lease drops must complete the scrub"
        );
    }

    /// W3-2 regression: the maintenance pass in `CausalStore::open()` is now
    /// throttled to once per 30s per store instance, so it cannot notice a
    /// page edit inside that window. Egress must not depend on it —
    /// `still_valid()` compares the live file itself on every call, with no
    /// `open()` in between.
    #[test]
    fn page_edit_inside_the_throttle_window_still_invalidates_a_delivery_lease() {
        let (_dir, store, page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        let lease = store
            .acquire_ccr_delivery_lease(
                &scope,
                &source.id,
                &source.version,
                &source.content_sha256,
                &ccr_acl_revision(&scope),
            )
            .unwrap();
        assert!(lease.still_valid());
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-03\ntrust: 0.9\n---\nA changes C.\n",
        ).unwrap();
        // No `reset_maintenance_throttle()` and no `open()`: the resync has
        // provably not run, and the lease is invalid anyway.
        assert!(
            !lease.still_valid(),
            "the throttle must never delay revoking an in-flight delivery"
        );
    }

    /// The other half of the throttle contract, stated honestly: within the
    /// window a *reused* store instance still serves the pre-edit copy. The
    /// gateway builds a `CausalStore` per request, so a new request always
    /// resyncs first — only a long-lived instance (a 1,000-case evaluation
    /// run) reuses one, and that is the case the throttle exists for.
    #[test]
    fn a_reused_store_instance_resyncs_once_per_window_not_once_per_open() {
        let (_dir, store, page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-03\ntrust: 0.9\n---\nA changes C.\n",
        ).unwrap();
        store.open().unwrap();
        assert!(
            !artifact_content(&store, &source.id).is_empty(),
            "an open inside the window must not pay for the resync"
        );
        // What a new request does.
        assert!(matches!(
            CausalStore::new(store.path()).source_text(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(artifact_content(&store, &source.id).is_empty());
    }

    /// W3-2 regression: a Wiki page has no retention deadline, which the
    /// store records as `i64::MAX`. Serialised as a JSON number that value
    /// does not survive a browser round trip.
    #[test]
    fn an_imported_wiki_source_reports_no_retention_deadline_as_null() {
        let (_dir, store, _page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        assert_eq!(source.retention_at, i64::MAX);
        assert!(
            serde_json::to_value(&source).unwrap()["retention_at"].is_null(),
            "the import endpoint must report no deadline as null"
        );
    }

    /// F2 regression on the Wiki path: a changed page cleared the excerpt but
    /// left the wording copied into `causal_claims.context_json`.
    #[test]
    fn a_changed_wiki_page_also_scrubs_copied_claim_context() {
        let (_dir, store, page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({ "original_variable_names": ["A changes B"] }),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                0,
                1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-03\ntrust: 0.9\n---\nA changes C.\n",
        ).unwrap();
        // Stand in for the next request's fresh store — see
        // `a_leased_wiki_source_defers_its_scrub_and_fails_still_valid_on_page_edit`.
        store.reset_maintenance_throttle();
        store.open().unwrap();
        let context: String = Connection::open(store.path())
            .unwrap()
            .query_row(
                "SELECT context_json FROM causal_claims WHERE id=?1",
                [&claim.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            context, "{}",
            "wording copied into claim context must be scrubbed with the excerpt"
        );
    }

    #[test]
    fn traversal_page_path_is_refused_before_any_filesystem_probe() {
        let (dir, store, _page) = fixture();
        // The validator now runs before the size probe, so no `metadata()` call
        // ever reaches a path outside the wiki tree.
        assert!(
            live_page(dir.path(), "agent-a", "../../../../etc/passwd", None).is_none(),
            "traversal must be refused"
        );
        assert!(live_page(dir.path(), "agent-a", "sources/queue.md", None).is_some());
        assert!(matches!(
            store.import_agent_wiki_source("agent-a", "../queue.md"),
            Err(CausalStoreError::NotFound)
        ));
    }

    fn fixture() -> (tempfile::TempDir, CausalStore, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("agents/agent-a/wiki");
        std::fs::create_dir_all(wiki.join("sources")).unwrap();
        let page = wiki.join("sources/queue.md");
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-02\ntrust: 0.9\n---\nA changes B.\n",
        ).unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        (dir, store, page)
    }

    #[test]
    fn edit_quarantine_and_delete_revoke_exact_agent_page() {
        let (_dir, store, page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        assert_eq!(
            store
                .import_agent_wiki_source("agent-a", "sources/queue.md")
                .unwrap()
                .id,
            source.id
        );
        assert!(matches!(
            store.import_agent_wiki_source("agent-b", "sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.import_agent_wiki_source("agent-a", "../queue.md"),
            Err(CausalStoreError::NotFound)
        ));
        let body = store.source_text(&scope, &source.id).unwrap();
        let start = body.find('A').unwrap();
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &source.id,
                start,
                start + 1,
                "A",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope, &claim.id, "reviewer", true)
            .unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute(
            "INSERT INTO causal_models
            (id,tenant_id,acl,name,version,treatment_variable_id,outcome_variable_id,
             population,window_start,window_end,created_at)
             VALUES ('model','agent-a','agent-private','test','v1','a','b','all',0,1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO causal_model_edges (model_id,claim_id) VALUES ('model',?1)",
            [&claim.id],
        )
        .unwrap();
        conn.execute("INSERT INTO causal_effect_estimates
            (id,model_id,data_snapshot_id,method,code_sha256,estimate,lower_bound,upper_bound,
             diagnostics_json,identification_state,created_at)
             VALUES ('effect','model','snapshot','test','hash',1.0,0.5,1.5,'{\"private\":1}','identified',1)", []).unwrap();
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-03\ntrust: 0.9\n---\nA changes C.\n",
        ).unwrap();
        // Stand in for the next request's fresh store — see
        // `a_leased_wiki_source_defers_its_scrub_and_fails_still_valid_on_page_edit`.
        store.reset_maintenance_throttle();
        assert!(matches!(
            store.source_text(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert_eq!(
            store.claim_state(&scope, &claim.id).unwrap(),
            "needs_review"
        );
        assert_eq!(
            store.evidence_for_claim(&scope, &claim.id).unwrap()[0]
                .span
                .excerpt,
            ""
        );
        let (estimate, diagnostics, state): (Option<f64>, String, String) = conn.query_row(
            "SELECT estimate,diagnostics_json,identification_state FROM causal_effect_estimates WHERE id='effect'",
            [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!(estimate, None);
        assert_eq!(diagnostics, "{}");
        assert_eq!(state, "wiki_source_changed");
        let replacement = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        assert_ne!(replacement.id, source.id);
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-04\ntrust: 0.9\ndo_not_inject: true\n---\nA changes C.\n",
        ).unwrap();
        store.reset_maintenance_throttle();
        assert!(matches!(
            store.source_text(&scope, &replacement.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.import_agent_wiki_source("agent-a", "sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
        std::fs::remove_file(&page).unwrap();
        assert!(matches!(
            store.import_agent_wiki_source("agent-a", "sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
    }

    #[test]
    fn live_trust_quarantine_revokes_imported_source() {
        let (dir, store, _page) = fixture();
        let source = store
            .import_agent_wiki_source("agent-a", "sources/queue.md")
            .unwrap();
        let trust = WikiTrustStore::open(dir.path().join("wiki_trust.db")).unwrap();
        trust
            .manual_set("sources/queue.md", "agent-a", 0.05, false, Some(true), None)
            .unwrap();
        drop(trust);
        let scope = EvidenceScope {
            tenant_id: "agent-a".into(),
            acl: WIKI_ACL.into(),
        };
        // Stand in for the next request's fresh store — see
        // `a_leased_wiki_source_defers_its_scrub_and_fails_still_valid_on_page_edit`.
        store.reset_maintenance_throttle();
        assert!(matches!(
            store.source_text(&scope, &source.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.import_agent_wiki_source("agent-a", "sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
    }

    #[test]
    fn shared_namespace_policy_change_revokes_only_its_page_version() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("shared/wiki/sources");
        std::fs::create_dir_all(&wiki).unwrap();
        let page = wiki.join("queue.md");
        std::fs::write(&page,
            "---\ntitle: Queue\ncreated: 2026-01-01\nupdated: 2026-01-02\ntrust: 0.9\n---\nShared A changes B.\n",
        ).unwrap();
        let other_dir = dir.path().join("shared/wiki/concepts");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(other_dir.join("other.md"),
            "---\ntitle: Other\ncreated: 2026-01-01\nupdated: 2026-01-02\ntrust: 0.9\n---\nIndependent page.\n",
        ).unwrap();
        let policy = dir.path().join("shared/wiki/.scope.toml");
        std::fs::write(&policy, "[namespaces.sources]\nmode = 'agent_writable'\n").unwrap();
        let store = CausalStore::new(dir.path().join("memory.db"));
        let first = store.import_shared_wiki_source("sources/queue.md").unwrap();
        let other = store
            .import_shared_wiki_source("concepts/other.md")
            .unwrap();
        assert_eq!(
            store
                .import_shared_wiki_source("sources/queue.md")
                .unwrap()
                .id,
            first.id
        );
        let scope = EvidenceScope {
            tenant_id: SHARED_WIKI_TENANT.into(),
            acl: SHARED_WIKI_ACL.into(),
        };
        assert!(matches!(
            store.source_text(
                &EvidenceScope {
                    tenant_id: "agent-a".into(),
                    acl: WIKI_ACL.into()
                },
                &first.id
            ),
            Err(CausalStoreError::NotFound)
        ));
        let content = store.source_text(&scope, &first.id).unwrap();
        let start = content.find("Shared").unwrap();
        let claim = store
            .add_claim(
                &scope,
                "A",
                "B",
                0,
                1,
                &serde_json::json!({}),
                ClaimModality::Asserted,
            )
            .unwrap();
        store
            .add_evidence(
                &scope,
                &claim.id,
                &first.id,
                start,
                start + 6,
                "Shared",
                EvidenceStance::Supports,
                None,
                "test",
            )
            .unwrap();
        store
            .review_claim(&scope, &claim.id, "reviewer", true)
            .unwrap();
        std::fs::write(&policy, "[namespaces.sources]\nmode = 'operator_only'\n").unwrap();
        // Stand in for the next request's fresh store — see
        // `a_leased_wiki_source_defers_its_scrub_and_fails_still_valid_on_page_edit`.
        store.reset_maintenance_throttle();
        assert!(matches!(
            store.source_text(&scope, &first.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(store.source_text(&scope, &other.id).is_ok());
        assert_eq!(
            store.claim_state(&scope, &claim.id).unwrap(),
            "needs_review"
        );
        let second = store.import_shared_wiki_source("sources/queue.md").unwrap();
        assert_ne!(second.version, first.version);
        std::fs::write(&policy, "[namespaces.sources\nmode = 'operator_only'\n").unwrap();
        store.reset_maintenance_throttle();
        assert!(matches!(
            store.source_text(&scope, &second.id),
            Err(CausalStoreError::NotFound)
        ));
        assert!(matches!(
            store.import_shared_wiki_source("sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
        std::fs::write(&policy, "[namespaces]\nsources = 'unexpected'\n").unwrap();
        assert!(matches!(
            store.import_shared_wiki_source("sources/queue.md"),
            Err(CausalStoreError::NotFound)
        ));
    }
}
