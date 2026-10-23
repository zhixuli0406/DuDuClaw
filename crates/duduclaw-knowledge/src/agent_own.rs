//! An AI employee's own collection: the 「資料來源」 tab of the employee
//! page uploads into it.
//!
//! - One row per employee in `agent_own_collections`, keyed by the employee
//!   **id** (never its display name), so renaming the employee keeps its
//!   files and two employees can never share one. Deleting the collection
//!   in the library deletes the row too (`ON DELETE CASCADE`); the next
//!   upload creates a new one.
//! - The collection is an ordinary collection: the same grants, the same
//!   ACL, the same search path. This module only creates it with its
//!   default grants and archives / restores it with the employee.
//! - Archiving (the employee is archived or deleted) removes every grant
//!   except the people who own the collection, so nothing (the employee
//!   included) reads it through a share any more; its owners (and Admins)
//!   can still download or delete the files. The removed grants are kept in
//!   the row, and restoring the employee puts exactly those back (plus the
//!   employee's own grant). While a row is archived the ACL query ignores
//!   the employee's grant on it even if someone adds it again by hand.
//! - Creating, archiving and restoring each run in one transaction.
//!
//! Callers check who may do this first (the gateway: owner access to the
//! employee, re-read from `users.db`).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::acl::{self, PrincipalKind, Role};
use crate::limits::KnowledgeConfig;
use crate::store::{self, Collection, NewCollection};
use crate::{KbError, KbResult};

pub const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS agent_own_collections (
  agent_id         TEXT PRIMARY KEY,
  collection_id    TEXT NOT NULL UNIQUE REFERENCES collections(id) ON DELETE CASCADE,
  share_chat_users INTEGER NOT NULL DEFAULT 1,
  created_by       TEXT NOT NULL,
  created_at       TEXT NOT NULL,
  archived_at      TEXT,
  archived_grants  TEXT
);
"#;

/// Longest display name kept in the collection name (the name is capped at
/// 80 characters).
const NAME_PREFIX_CHARS: usize = 60;

/// One employee's own collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnCollection {
    pub agent_id: String,
    pub collection_id: String,
    /// The people allowed to chat with the employee were made readers.
    pub share_chat_users: bool,
    pub created_by: String,
    pub created_at: String,
    pub archived_at: Option<String>,
}

/// A grant as this module hands it to [`acl::set_acl`].
pub type Grant = (PrincipalKind, String, Role);

/// What [`create`] needs.
#[derive(Debug, Clone)]
pub struct NewOwn<'a> {
    pub agent_id: &'a str,
    pub display_name: &'a str,
    pub sensitivity: Option<&'a str>,
    pub share_chat_users: bool,
    pub created_by: &'a str,
    /// Default readers besides the creator and the employee (the gateway
    /// derives them from the employee's account bindings).
    pub readers: Vec<Grant>,
}

/// [`create`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ensured {
    Created(Collection),
    Existing(Collection),
}

impl Ensured {
    pub fn collection(&self) -> &Collection {
        match self {
            Ensured::Created(c) | Ensured::Existing(c) => c,
        }
    }
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OwnCollection> {
    Ok(OwnCollection {
        agent_id: r.get(0)?,
        collection_id: r.get(1)?,
        share_chat_users: r.get::<_, i64>(2)? != 0,
        created_by: r.get(3)?,
        created_at: r.get(4)?,
        archived_at: r.get(5)?,
    })
}

const COLS: &str = "agent_id, collection_id, share_chat_users, created_by, created_at, archived_at";

/// The employee's own collection, if it has one.
pub fn get(conn: &Connection, agent_id: &str) -> KbResult<Option<OwnCollection>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM agent_own_collections WHERE agent_id = ?1"),
            params![agent_id],
            row,
        )
        .optional()?)
}

/// The employee a collection belongs to, if it is someone's own collection.
pub fn for_collection(conn: &Connection, collection_id: &str) -> KbResult<Option<OwnCollection>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM agent_own_collections WHERE collection_id = ?1"),
            params![collection_id],
            row,
        )
        .optional()?)
}

/// Every own-collection row (the gateway's start-up reconcile).
pub fn list(conn: &Connection) -> KbResult<Vec<OwnCollection>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM agent_own_collections ORDER BY agent_id"
    ))?;
    let rows = stmt.query_map([], row)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `<display name> 的資料`, within the 80-character name limit.
pub fn own_name(display_name: &str, agent_id: &str) -> String {
    let base: String = display_name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let base = if base.is_empty() { agent_id } else { &base };
    format!("{} 的資料", duduclaw_core::truncate_chars(base, NAME_PREFIX_CHARS))
}

/// `agent-<id>` as a valid slug (lower case, `_` and other characters as
/// `-`), at most 48 bytes.
fn base_slug(agent_id: &str) -> String {
    let mut s = String::from("agent-");
    for c in agent_id.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    duduclaw_core::truncate_bytes(&s, 48)
        .trim_end_matches('-')
        .to_string()
}

fn slug_taken(conn: &Connection, slug: &str) -> KbResult<bool> {
    let hit: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM collections WHERE slug = ?1",
            params![slug],
            |r| r.get(0),
        )
        .optional()?;
    Ok(hit.is_some())
}

/// A free slug for the employee: `agent-<id>`, or with a random suffix
/// when another collection already uses it.
fn unique_slug(conn: &Connection, agent_id: &str) -> KbResult<String> {
    let base = base_slug(agent_id);
    if store::valid_slug(&base) && !slug_taken(conn, &base)? {
        return Ok(base);
    }
    for _ in 0..8 {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let candidate = format!("{base}-{}", duduclaw_core::truncate_bytes(&suffix, 6));
        if store::valid_slug(&candidate) && !slug_taken(conn, &candidate)? {
            return Ok(candidate);
        }
    }
    Err(KbError::Invalid(
        "無法為這位 AI 員工產生集合代號，請稍後再試。".to_string(),
    ))
}

/// Merge `extra` into `base` (first grant of a principal wins).
fn merge(base: &mut Vec<Grant>, extra: &[Grant]) {
    for g in extra {
        if !base.iter().any(|(k, i, _)| *k == g.0 && *i == g.1) {
            base.push(g.clone());
        }
    }
}

/// Create the employee's own collection with its default grants, or return
/// the one it already has, in one transaction. Refused while the row is
/// archived (the employee is archived or deleted).
pub fn create(conn: &mut Connection, new: &NewOwn<'_>, cfg: &KnowledgeConfig) -> KbResult<Ensured> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if let Some(existing) = get(&tx, new.agent_id)? {
        let c = existing_answer(&tx, &existing)?;
        tx.commit()?;
        return Ok(Ensured::Existing(c));
    }
    let slug = unique_slug(&tx, new.agent_id)?;
    let id = store::create_collection_in(
        &tx,
        &NewCollection {
            name: own_name(new.display_name, new.agent_id),
            slug: Some(slug),
            description: String::new(),
            department: None,
            sensitivity: new.sensitivity.map(str::to_string),
        },
        new.created_by,
        cfg,
    )?;
    let now = crate::now_rfc3339();
    tx.execute(
        "INSERT INTO agent_own_collections(agent_id, collection_id, share_chat_users, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![new.agent_id, id, new.share_chat_users as i64, new.created_by, now],
    )?;
    let mut grants: Vec<Grant> = vec![(PrincipalKind::Agent, new.agent_id.to_string(), Role::Viewer)];
    merge(&mut grants, &new.readers);
    // The creator already holds the owner grant.
    grants.retain(|(k, i, _)| !(*k == PrincipalKind::User && i == new.created_by));
    insert_grants(&tx, &id, &grants, new.created_by)?;
    tx.commit()?;
    let c = store::get_collection(conn, &id)?.ok_or(KbError::PermissionDenied)?;
    Ok(Ensured::Created(c))
}

/// Add grants (validated like `acl::set_acl`) and bump the ACL revision.
fn insert_grants(tx: &Connection, collection_id: &str, grants: &[Grant], actor: &str) -> KbResult<()> {
    let now = crate::now_rfc3339();
    for (kind, id, role) in grants {
        acl::validate_entry(*kind, id, *role)?;
        tx.execute(
            "INSERT INTO collection_acl(collection_id, principal_kind, principal_id, role, granted_by, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![collection_id, kind.as_str(), id, role.as_str(), actor, now],
        )?;
    }
    tx.execute(
        "UPDATE collections SET acl_revision = acl_revision + 1, updated_at = ?2 WHERE id = ?1",
        params![collection_id, now],
    )?;
    Ok(())
}

fn grants_to_json(grants: &[Grant]) -> String {
    let v: Vec<serde_json::Value> = grants
        .iter()
        .map(|(k, i, r)| serde_json::json!({"kind": k.as_str(), "id": i, "role": r.as_str()}))
        .collect();
    serde_json::Value::Array(v).to_string()
}

/// The archived grants; anything unreadable is skipped (restores less).
fn grants_from_json(raw: Option<&str>) -> Vec<Grant> {
    let Some(v) = raw.and_then(|r| serde_json::from_str::<serde_json::Value>(r).ok()) else {
        return Vec::new();
    };
    v.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|g| {
                    let kind = g.get("kind")?.as_str().and_then(PrincipalKind::parse)?;
                    let id = g.get("id")?.as_str()?.to_string();
                    let role = g.get("role")?.as_str().and_then(Role::parse)?;
                    Some((kind, id, role))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn existing_answer(conn: &Connection, own: &OwnCollection) -> KbResult<Collection> {
    if own.archived_at.is_some() {
        return Err(KbError::Invalid(
            "這位 AI 員工已封存，資料來源暫停使用；請先恢復這位員工。".to_string(),
        ));
    }
    store::get_collection(conn, &own.collection_id)?.ok_or(KbError::PermissionDenied)
}

/// What [`archive`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archived {
    pub collection_id: String,
    pub removed: Vec<Grant>,
}

/// Archive the employee's own collection: keep only the people who own it
/// and remember the rest. `Ok(None)` when it has none or it is already
/// archived.
pub fn archive(conn: &mut Connection, agent_id: &str, _actor: &str) -> KbResult<Option<Archived>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(own) = get(&tx, agent_id)? else {
        return Ok(None);
    };
    if own.archived_at.is_some() {
        return Ok(None);
    }
    let (entries, _) = acl::get_acl(&tx, &own.collection_id)?;
    let removed: Vec<Grant> = entries
        .into_iter()
        .map(|e| (e.kind, e.id, e.role))
        .filter(|(k, _, r)| !(*k == PrincipalKind::User && *r == Role::Owner))
        .collect();
    for (k, i, _) in &removed {
        tx.execute(
            "DELETE FROM collection_acl WHERE collection_id = ?1 AND principal_kind = ?2 AND principal_id = ?3",
            params![own.collection_id, k.as_str(), i],
        )?;
    }
    let now = crate::now_rfc3339();
    tx.execute(
        "UPDATE collections SET acl_revision = acl_revision + 1, updated_at = ?2 WHERE id = ?1",
        params![own.collection_id, now],
    )?;
    tx.execute(
        "UPDATE agent_own_collections SET archived_at = ?2, archived_grants = ?3 WHERE agent_id = ?1",
        params![agent_id, now, grants_to_json(&removed)],
    )?;
    tx.commit()?;
    Ok(Some(Archived {
        collection_id: own.collection_id,
        removed,
    }))
}

/// Restore an archived own collection: the grants archiving removed come
/// back, plus the employee's own (principals already on the list keep
/// their role). Returns the grants added; `Ok(None)` when there is nothing
/// to restore.
pub fn restore(conn: &mut Connection, agent_id: &str, actor: &str) -> KbResult<Option<Vec<Grant>>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(own) = get(&tx, agent_id)? else {
        return Ok(None);
    };
    if own.archived_at.is_none() {
        return Ok(None);
    }
    let saved: Option<String> = tx.query_row(
        "SELECT archived_grants FROM agent_own_collections WHERE agent_id = ?1",
        params![agent_id],
        |r| r.get(0),
    )?;
    let (entries, _) = acl::get_acl(&tx, &own.collection_id)?;
    let current: Vec<Grant> = entries.into_iter().map(|e| (e.kind, e.id, e.role)).collect();
    let mut wanted = vec![(PrincipalKind::Agent, agent_id.to_string(), Role::Viewer)];
    merge(&mut wanted, &grants_from_json(saved.as_deref()));
    let added: Vec<Grant> = wanted
        .into_iter()
        .filter(|(k, i, _)| !current.iter().any(|(ck, ci, _)| ck == k && ci == i))
        .collect();
    insert_grants(&tx, &own.collection_id, &added, actor)?;
    tx.execute(
        "UPDATE agent_own_collections SET archived_at = NULL, archived_grants = NULL WHERE agent_id = ?1",
        params![agent_id],
    )?;
    tx.commit()?;
    Ok(Some(added))
}

#[cfg(test)]
#[path = "agent_own_tests.rs"]
mod tests;
