//! Collections: create, read, update, delete.
//!
//! Callers check permissions first ([`crate::acl`]); these functions only
//! validate input and keep the rows consistent. Deleting a collection
//! cascades to its grants, sources, segments, chunks (and their FTS rows),
//! embeddings and jobs.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::acl::Role;
use crate::limits::KnowledgeConfig;
use crate::{KbError, KbResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub department: Option<String>,
    pub sensitivity: String,
    pub embedding_policy: String,
    pub acl_revision: i64,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
}

/// A collection with counts and the caller's role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSummary {
    #[serde(flatten)]
    pub collection: Collection,
    pub my_role: Option<Role>,
    pub source_count: i64,
    pub ready_count: i64,
    pub chunk_count: i64,
}

/// Input for [`create_collection`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewCollection {
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub sensitivity: Option<String>,
}

/// Fields [`update_collection`] may change; `None` keeps the value.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CollectionPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `Some("")` clears the department.
    #[serde(default)]
    pub department: Option<String>,
    #[serde(default)]
    pub sensitivity: Option<String>,
    #[serde(default)]
    pub embedding_policy: Option<String>,
}

const COLS: &str = "id, slug, name, description, department, sensitivity, embedding_policy, acl_revision, created_by, created_at, updated_at";

fn row_to_collection(r: &Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        slug: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        department: r.get(4)?,
        sensitivity: r.get(5)?,
        embedding_policy: r.get(6)?,
        acl_revision: r.get(7)?,
        created_by: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

/// `[a-z0-9][a-z0-9-]{0,63}`.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 64
        && slug
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn derive_slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_matches('-');
    let s = duduclaw_core::truncate_bytes(s, 48)
        .trim_end_matches('-')
        .to_string();
    if s.is_empty() {
        format!("kb-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    } else {
        s
    }
}

fn clean_name(name: &str) -> KbResult<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    if name.is_empty() {
        return Err(KbError::Invalid("請輸入集合名稱。".to_string()));
    }
    if name.chars().count() > 80 {
        return Err(KbError::Invalid("集合名稱最多 80 個字。".to_string()));
    }
    Ok(name)
}

fn clean_description(d: &str) -> KbResult<String> {
    if d.chars().count() > 2000 {
        return Err(KbError::Invalid("說明最多 2000 個字。".to_string()));
    }
    Ok(d.trim().to_string())
}

fn clean_department(d: Option<&str>) -> KbResult<Option<String>> {
    match d.map(str::trim).filter(|d| !d.is_empty()) {
        None => Ok(None),
        Some(d) if duduclaw_core::department::is_valid_department(d) => Ok(Some(d.to_string())),
        Some(_) => Err(KbError::Invalid("部門名稱格式不正確。".to_string())),
    }
}

fn clean_sensitivity(s: Option<&str>) -> KbResult<String> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok("internal".to_string()),
        Some(s) => duduclaw_core::sensitivity::Sensitivity::parse(s)
            .map(|v| v.as_str().to_string())
            .ok_or_else(|| {
                KbError::Invalid(
                    "敏感度只能是 public、internal、personal 或 restricted。".to_string(),
                )
            }),
    }
}

/// Create a collection; `created_by` becomes its owner.
pub fn create_collection(
    conn: &mut Connection,
    new: &NewCollection,
    created_by: &str,
    cfg: &KnowledgeConfig,
) -> KbResult<Collection> {
    let name = clean_name(&new.name)?;
    let description = clean_description(&new.description)?;
    let department = clean_department(new.department.as_deref())?;
    let sensitivity = clean_sensitivity(new.sensitivity.as_deref())?;
    let slug = match new.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if valid_slug(s) => s.to_string(),
        Some(_) => {
            return Err(KbError::Invalid(
                "代號只能用小寫英文字母、數字與連字號（-），最多 64 字，且開頭不能是連字號。"
                    .to_string(),
            ));
        }
        None => derive_slug(&name),
    };
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM collections", [], |r| r.get(0))?;
    if count as usize >= cfg.max_collections {
        return Err(KbError::Rejected {
            code: "quota_collections",
            message: format!("集合數量已達上限（{} 個）。", cfg.max_collections),
        });
    }
    let taken: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM collections WHERE slug = ?1",
            params![slug],
            |r| r.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Err(KbError::Invalid(format!(
            "代號「{slug}」已被使用，請換一個。"
        )));
    }
    let id = format!("col_{}", uuid::Uuid::new_v4().simple());
    let now = crate::now_rfc3339();
    tx.execute(
        "INSERT INTO collections(id, slug, name, description, department, sensitivity, created_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![id, slug, name, description, department, sensitivity, created_by, now],
    )?;
    tx.execute(
        "INSERT INTO collection_acl(collection_id, principal_kind, principal_id, role, granted_by, granted_at)
         VALUES (?1, 'user', ?2, 'owner', ?2, ?3)",
        params![id, created_by, now],
    )?;
    tx.commit()?;
    get_collection(conn, &id)?.ok_or(KbError::PermissionDenied)
}

pub fn get_collection(conn: &Connection, id: &str) -> KbResult<Option<Collection>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM collections WHERE id = ?1"),
            params![id],
            row_to_collection,
        )
        .optional()?)
}

fn counts(conn: &Connection, id: &str) -> KbResult<(i64, i64, i64)> {
    Ok(conn.query_row(
        "SELECT (SELECT COUNT(*) FROM sources WHERE collection_id = ?1),
                (SELECT COUNT(*) FROM sources WHERE collection_id = ?1 AND status IN ('ready','embedding')),
                (SELECT COUNT(*) FROM chunks WHERE collection_id = ?1)",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

fn summarize(
    conn: &Connection,
    c: Collection,
    my_role: Option<Role>,
) -> KbResult<CollectionSummary> {
    let (source_count, ready_count, chunk_count) = counts(conn, &c.id)?;
    Ok(CollectionSummary {
        collection: c,
        my_role,
        source_count,
        ready_count,
        chunk_count,
    })
}

/// Collections a dashboard user holds any role on (Admin: all), with counts.
pub fn list_for_user(
    conn: &Connection,
    user_id: &str,
    department: Option<&str>,
    is_admin: bool,
) -> KbResult<Vec<CollectionSummary>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM collections ORDER BY name, id"))?;
    let all: Vec<Collection> = stmt
        .query_map([], row_to_collection)?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for c in all {
        let role = crate::acl::user_role(conn, &c.id, user_id, department, is_admin)?;
        if role.is_some() {
            out.push(summarize(conn, c, role)?);
        }
    }
    Ok(out)
}

/// Summaries for an allowed set (the MCP listing). No role is reported.
pub fn list_by_ids(conn: &Connection, ids: &[String]) -> KbResult<Vec<CollectionSummary>> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(c) = get_collection(conn, id)? {
            out.push(summarize(conn, c, None)?);
        }
    }
    out.sort_by(|a, b| a.collection.name.cmp(&b.collection.name));
    Ok(out)
}

/// Apply `patch` if the row still carries `expected_updated_at`.
pub fn update_collection(
    conn: &mut Connection,
    id: &str,
    patch: &CollectionPatch,
    expected_updated_at: &str,
) -> KbResult<Collection> {
    let current = get_collection(conn, id)?.ok_or(KbError::PermissionDenied)?;
    if current.updated_at != expected_updated_at {
        return Err(KbError::Conflict(
            "集合已被其他人修改，請重新載入後再儲存。".to_string(),
        ));
    }
    let name = match patch.name.as_deref() {
        Some(n) => clean_name(n)?,
        None => current.name.clone(),
    };
    let description = match patch.description.as_deref() {
        Some(d) => clean_description(d)?,
        None => current.description.clone(),
    };
    let department = match patch.department.as_deref() {
        Some(d) => clean_department(Some(d))?,
        None => current.department.clone(),
    };
    let sensitivity = match patch.sensitivity.as_deref() {
        Some(s) => clean_sensitivity(Some(s))?,
        None => current.sensitivity.clone(),
    };
    let embedding_policy = match patch.embedding_policy.as_deref() {
        Some(p @ ("inherit" | "local_only")) => p.to_string(),
        Some(_) => {
            return Err(KbError::Invalid(
                "向量政策只能是 inherit 或 local_only。".to_string(),
            ));
        }
        None => current.embedding_policy.clone(),
    };
    let now = crate::now_rfc3339();
    let changed = conn.execute(
        "UPDATE collections SET name=?2, description=?3, department=?4, sensitivity=?5, embedding_policy=?6, updated_at=?7
          WHERE id=?1 AND updated_at=?8",
        params![id, name, description, department, sensitivity, embedding_policy, now, expected_updated_at],
    )?;
    if changed == 0 {
        return Err(KbError::Conflict(
            "集合已被其他人修改，請重新載入後再儲存。".to_string(),
        ));
    }
    get_collection(conn, id)?.ok_or(KbError::PermissionDenied)
}

/// Delete a collection after the caller typed its exact name. Returns the
/// blob hashes no other source references any more (the caller removes the
/// files after commit).
pub fn delete_collection(
    conn: &mut Connection,
    id: &str,
    confirm_name: &str,
) -> KbResult<Vec<String>> {
    let current = get_collection(conn, id)?.ok_or(KbError::PermissionDenied)?;
    if current.name != confirm_name {
        return Err(KbError::Invalid(
            "輸入的名稱與集合名稱不符，未刪除。".to_string(),
        ));
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let shas: Vec<String> = {
        let mut stmt =
            tx.prepare("SELECT DISTINCT blob_sha256 FROM sources WHERE collection_id = ?1")?;
        stmt.query_map(params![id], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    // Chunks first so the FTS delete trigger runs for every row.
    tx.execute("DELETE FROM chunks WHERE collection_id = ?1", params![id])?;
    tx.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
    crate::schema::bump_generation(&tx)?;
    let mut orphaned = Vec::new();
    for sha in shas {
        let refs: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sources WHERE blob_sha256 = ?1",
            params![sha],
            |r| r.get(0),
        )?;
        if refs == 0 {
            orphaned.push(sha);
        }
    }
    tx.commit()?;
    Ok(orphaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn new(name: &str) -> NewCollection {
        NewCollection {
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn create_derives_slug_and_owner() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let c = create_collection(&mut conn, &new("HR Handbook 2026"), "u1", &cfg).unwrap();
        assert_eq!(c.slug, "hr-handbook-2026");
        assert_eq!(c.sensitivity, "internal");
        let zh = create_collection(&mut conn, &new("人資手冊"), "u1", &cfg).unwrap();
        assert!(zh.slug.starts_with("kb-"));
        let (acl, rev) = crate::acl::get_acl(&conn, &c.id).unwrap();
        assert_eq!(rev, 0);
        assert_eq!(acl.len(), 1);
        assert_eq!(acl[0].role, Role::Owner);
        let dup = create_collection(
            &mut conn,
            &NewCollection {
                slug: Some("hr-handbook-2026".into()),
                ..new("x")
            },
            "u1",
            &cfg,
        );
        assert!(matches!(dup, Err(KbError::Invalid(_))));
        let bad = create_collection(
            &mut conn,
            &NewCollection {
                slug: Some("-bad".into()),
                ..new("x")
            },
            "u1",
            &cfg,
        );
        assert!(matches!(bad, Err(KbError::Invalid(_))));
    }

    #[test]
    fn quota_and_validation() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig {
            max_collections: 1,
            ..Default::default()
        };
        create_collection(&mut conn, &new("a"), "u", &cfg).unwrap();
        let err = create_collection(&mut conn, &new("b"), "u", &cfg).unwrap_err();
        assert_eq!(err.code(), "quota_collections");
        let cfg = KnowledgeConfig::default();
        assert!(create_collection(&mut conn, &new("  "), "u", &cfg).is_err());
        let bad = NewCollection {
            sensitivity: Some("secret".into()),
            ..new("c")
        };
        assert!(create_collection(&mut conn, &bad, "u", &cfg).is_err());
    }

    #[test]
    fn update_is_optimistic_and_delete_needs_name() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let c = create_collection(&mut conn, &new("產品FAQ"), "u", &KnowledgeConfig::default())
            .unwrap();
        let patch = CollectionPatch {
            description: Some("常見問題".into()),
            ..Default::default()
        };
        assert!(matches!(
            update_collection(&mut conn, &c.id, &patch, "stale"),
            Err(KbError::Conflict(_))
        ));
        std::thread::sleep(std::time::Duration::from_millis(2));
        let u = update_collection(&mut conn, &c.id, &patch, &c.updated_at).unwrap();
        assert_eq!(u.description, "常見問題");
        assert!(delete_collection(&mut conn, &c.id, "wrong").is_err());
        delete_collection(&mut conn, &c.id, "產品FAQ").unwrap();
        assert!(get_collection(&conn, &c.id).unwrap().is_none());
    }
}
