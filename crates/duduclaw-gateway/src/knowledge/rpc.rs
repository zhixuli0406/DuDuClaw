//! `knowledge.*` dashboard RPCs.
//!
//! | method | who |
//! |---|---|
//! | `collections.list` | signed in (Admin: all; others: collections they hold a role on) |
//! | `collections.get` | viewer |
//! | `collections.create` | Manager or Admin (creator becomes owner) |
//! | `collections.update` / `.delete` | owner |
//! | `acl.get` / `acl.set` | owner |
//! | `sources.list` / `sources.get` | viewer |
//! | `sources.add` (paste / url) / `.delete` / `.reindex` | editor |
//! | `search` / `passage.get` | viewer (human side only, no employee side) |
//! | `access_log.list` | owner |
//! | `embedding.*` | see `embedding_rpc` (status: signed in; changes: Admin) |
//!
//! Admin counts as owner of every collection. A missing collection and one
//! the caller may not see give the same "permission denied".

use std::path::{Path, PathBuf};
use std::sync::Arc;

use duduclaw_auth::db::UserDb;
use duduclaw_auth::{UserContext, UserRole};
use duduclaw_knowledge::acl::{self, Narrowing, PrincipalKind, Role};
use duduclaw_knowledge::audit::{AccessEntry, log_access};
use duduclaw_knowledge::blob::BlobStore;
use duduclaw_knowledge::extract::Format;
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind};
use duduclaw_knowledge::store::{self, CollectionPatch, NewCollection};
use duduclaw_knowledge::{KbError, KbResult, schema};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::{audit_admin, blocking, dashboard_human, dashboard_user};

/// Methods this module answers.
pub fn is_knowledge_method(method: &str) -> bool {
    method.starts_with("knowledge.")
}

fn s<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn req<'a>(p: &'a Value, k: &str) -> KbResult<&'a str> {
    s(p, k).ok_or_else(|| KbError::Invalid(format!("缺少參數 {k}。")))
}

fn access(op: &str, user: &ResolvedUser, collection_id: Option<&str>) -> AccessEntry {
    AccessEntry {
        op: op.to_string(),
        collection_id: collection_id.map(str::to_string),
        requester_kind: "dashboard_user".into(),
        requester_id: Some(user.user_id.clone()),
        outcome: "ok".into(),
        ..Default::default()
    }
}

fn role_on(
    conn: &Connection,
    collection_id: &str,
    user: &ResolvedUser,
    min: Role,
) -> KbResult<Role> {
    acl::require_role(
        conn,
        collection_id,
        &user.user_id,
        user.department.as_deref(),
        user.is_admin,
        min,
    )
}

fn source_collection(
    conn: &Connection,
    source_id: &str,
) -> KbResult<duduclaw_knowledge::sources::Source> {
    duduclaw_knowledge::sources::get_source(conn, source_id)?.ok_or(KbError::PermissionDenied)
}

struct Call {
    home: PathBuf,
    method: String,
    params: Value,
    user: ResolvedUser,
    can_create: bool,
    cfg: KnowledgeConfig,
}

/// Answer `method`, or `None` when it is not a knowledge method.
pub async fn handle(
    home: &Path,
    method: &str,
    params: &Value,
    ctx: &UserContext,
    users: Option<Arc<UserDb>>,
) -> Option<Result<Value, String>> {
    if !is_knowledge_method(method) {
        return None;
    }
    let cfg = KnowledgeConfig::from_home(home);
    if !cfg.enabled {
        return Some(Err("文件知識庫已由管理員關閉。".to_string()));
    }
    let call = Call {
        home: home.to_path_buf(),
        method: method.to_string(),
        params: params.clone(),
        user: dashboard_user(ctx, users.as_ref()),
        can_create: duduclaw_auth::acl::require_role(ctx, UserRole::Manager).is_ok(),
        cfg,
    };
    if super::embedding_rpc::is_embedding_method(method) {
        return Some(super::embedding_rpc::handle(home, method, params, ctx).await);
    }
    if call.method == "knowledge.sources.add" {
        return Some(add_source(call).await.map_err(|e| message(&e)));
    }
    let wake = matches!(call.method.as_str(), "knowledge.sources.reindex");
    let embedder = if call.method == "knowledge.search" {
        Some(super::embedding::resolve(home).await)
    } else {
        None
    };
    let result = blocking(move || dispatch(call, embedder)).await;
    if wake && result.is_ok() {
        super::worker::notify();
    }
    Some(result.map_err(|e| message(&e)))
}

fn message(e: &KbError) -> String {
    if matches!(e, KbError::Db(_) | KbError::Io(_)) {
        tracing::error!(error = %e, "knowledge RPC failed");
    }
    e.public_message()
}

fn dispatch(call: Call, embedder: Option<Arc<dyn duduclaw_knowledge::embed::Embedder>>) -> KbResult<Value> {
    let mut conn = schema::open_home(&call.home)?;
    let p = &call.params;
    let u = &call.user;
    match call.method.as_str() {
        "knowledge.collections.list" => {
            let list =
                store::list_for_user(&conn, &u.user_id, u.department.as_deref(), u.is_admin)?;
            Ok(json!({ "collections": list, "can_create": call.can_create }))
        }
        "knowledge.collections.get" => {
            let id = req(p, "id")?;
            let role = role_on(&conn, id, u, Role::Viewer)?;
            let mut list = store::list_by_ids(&conn, &[id.to_string()])?;
            let mut c = list.pop().ok_or(KbError::PermissionDenied)?;
            c.my_role = Some(role);
            Ok(json!({ "collection": c }))
        }
        "knowledge.collections.create" => {
            if !call.can_create {
                return Err(KbError::PermissionDenied);
            }
            let new: NewCollection = serde_json::from_value(p.clone())
                .map_err(|_| KbError::Invalid("集合資料格式不正確。".to_string()))?;
            let c = store::create_collection(&mut conn, &new, &u.user_id, &call.cfg)?;
            log_access(&conn, &access("collection_change", u, Some(&c.id)))?;
            audit_admin(
                &call.home,
                "knowledge_collection_created",
                &u.user_id,
                json!({"collection_id": c.id, "slug": c.slug}),
            );
            Ok(json!({ "collection": c }))
        }
        "knowledge.collections.update" => {
            let id = req(p, "id")?;
            role_on(&conn, id, u, Role::Owner)?;
            let patch: CollectionPatch = serde_json::from_value(p.clone())
                .map_err(|_| KbError::Invalid("集合資料格式不正確。".to_string()))?;
            let c =
                store::update_collection(&mut conn, id, &patch, req(p, "expected_updated_at")?)?;
            log_access(&conn, &access("collection_change", u, Some(id)))?;
            audit_admin(
                &call.home,
                "knowledge_collection_updated",
                &u.user_id,
                json!({"collection_id": id}),
            );
            Ok(json!({ "collection": c }))
        }
        "knowledge.collections.delete" => {
            let id = req(p, "id")?.to_string();
            role_on(&conn, &id, u, Role::Owner)?;
            let orphans = store::delete_collection(
                &mut conn,
                &id,
                p.get("confirm_name").and_then(Value::as_str).unwrap_or(""),
            )?;
            remove_blobs(&call.home, &mut conn, &orphans);
            log_access(&conn, &access("collection_change", u, Some(&id)))?;
            audit_admin(
                &call.home,
                "knowledge_collection_deleted",
                &u.user_id,
                json!({"collection_id": id}),
            );
            Ok(json!({ "deleted": true }))
        }
        "knowledge.acl.get" => {
            let id = req(p, "collection_id")?;
            role_on(&conn, id, u, Role::Owner)?;
            let (entries, revision) = acl::get_acl(&conn, id)?;
            Ok(json!({ "entries": entries, "revision": revision }))
        }
        "knowledge.acl.set" => acl_set(&mut conn, &call),
        "knowledge.sources.list" => {
            let id = req(p, "collection_id")?;
            role_on(&conn, id, u, Role::Viewer)?;
            Ok(json!({ "sources": duduclaw_knowledge::sources::list_sources(&conn, id)? }))
        }
        "knowledge.sources.get" => {
            let src = source_collection(&conn, req(p, "source_id")?)?;
            role_on(&conn, &src.collection_id, u, Role::Viewer)?;
            Ok(json!({ "source": src }))
        }
        "knowledge.sources.delete" => {
            let src = source_collection(&conn, req(p, "source_id")?)?;
            role_on(&conn, &src.collection_id, u, Role::Editor)?;
            if let Some(sha) = duduclaw_knowledge::sources::delete_source(&mut conn, &src.id)? {
                remove_blobs(&call.home, &mut conn, &[sha]);
            }
            let mut entry = access("delete_source", u, Some(&src.collection_id));
            entry.results = vec![json!({"source_id": src.id})];
            log_access(&conn, &entry)?;
            audit_admin(
                &call.home,
                "knowledge_source_deleted",
                &u.user_id,
                json!({"collection_id": src.collection_id, "source_id": src.id, "title": src.title}),
            );
            Ok(json!({ "deleted": true }))
        }
        "knowledge.sources.reindex" => {
            let src = source_collection(&conn, req(p, "source_id")?)?;
            role_on(&conn, &src.collection_id, u, Role::Editor)?;
            duduclaw_knowledge::sources::request_reindex(&mut conn, &src.id)?;
            log_access(&conn, &access("reindex", u, Some(&src.collection_id)))?;
            Ok(json!({ "queued": true }))
        }
        "knowledge.search" => search(&conn, &call, embedder),
        "knowledge.passage.get" => passage(&conn, &call),
        "knowledge.access_log.list" => {
            let id = req(p, "collection_id")?;
            role_on(&conn, id, u, Role::Owner)?;
            let source_ids: Vec<String> = duduclaw_knowledge::sources::list_sources(&conn, id)?
                .into_iter()
                .map(|s| s.id)
                .collect();
            let rows = duduclaw_knowledge::audit::list_for_collection(
                &conn,
                duduclaw_knowledge::audit::Viewer {
                    user_id: &u.user_id,
                    is_admin: u.is_admin,
                },
                id,
                &source_ids,
                p.get("before_id").and_then(Value::as_i64),
                p.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize,
            )?;
            Ok(json!({ "entries": rows }))
        }
        _ => Err(KbError::Invalid("未知的知識庫操作。".to_string())),
    }
}

/// Remove original files nothing references any more. A file written
/// within the GC grace period stays until the worker's sweep (an upload of
/// the same bytes may be in flight; see `duduclaw_knowledge::blob`).
fn remove_blobs(home: &Path, conn: &mut Connection, shas: &[String]) {
    let store = BlobStore::for_home(home);
    for sha in shas {
        if let Err(e) = store.remove_if_unreferenced(conn, sha, duduclaw_knowledge::blob::GC_GRACE)
        {
            tracing::warn!(error = %e, "knowledge: could not remove blob file");
        }
    }
}

fn acl_set(conn: &mut Connection, call: &Call) -> KbResult<Value> {
    let p = &call.params;
    let id = req(p, "collection_id")?;
    role_on(conn, id, &call.user, Role::Owner)?;
    let expected = p
        .get("expected_revision")
        .and_then(Value::as_i64)
        .ok_or_else(|| KbError::Invalid("缺少參數 expected_revision。".to_string()))?;
    let raw = p
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| KbError::Invalid("缺少參數 entries。".to_string()))?;
    let mut entries = Vec::with_capacity(raw.len());
    for e in raw {
        let kind = e
            .get("kind")
            .and_then(Value::as_str)
            .and_then(PrincipalKind::parse);
        let role = e.get("role").and_then(Value::as_str).and_then(Role::parse);
        let pid = e.get("id").and_then(Value::as_str).map(str::trim);
        match (kind, pid, role) {
            (Some(k), Some(i), Some(r)) if !i.is_empty() => entries.push((k, i.to_string(), r)),
            _ => {
                return Err(KbError::Invalid(
                    "授權項目格式不正確（需要 kind、id、role）。".to_string(),
                ));
            }
        }
    }
    if entries.len() > 500 {
        return Err(KbError::Invalid("授權項目最多 500 筆。".to_string()));
    }
    let revision = acl::set_acl(conn, id, &entries, expected, &call.user.user_id)?;
    log_access(conn, &access("acl_change", &call.user, Some(id)))?;
    let summary: Vec<Value> = entries
        .iter()
        .map(|(k, i, r)| json!({"kind": k.as_str(), "id": i, "role": r.as_str()}))
        .collect();
    audit_admin(
        &call.home,
        "knowledge_acl_changed",
        &call.user.user_id,
        json!({"collection_id": id, "entries": summary, "revision": revision}),
    );
    Ok(json!({ "revision": revision }))
}

fn narrowing(p: &Value) -> Narrowing {
    Narrowing {
        only: p.get("collections").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        }),
    }
}

fn search(
    conn: &Connection,
    call: &Call,
    embedder: Option<Arc<dyn duduclaw_knowledge::embed::Embedder>>,
) -> KbResult<Value> {
    let p = &call.params;
    let query = req(p, "query")?;
    let human = dashboard_human(&call.user);
    let allowed = acl::effective_collections(conn, &human, None, &narrowing(p))?;
    let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(10) as usize;
    let embedder = embedder.unwrap_or_else(|| Arc::new(duduclaw_knowledge::embed::NoEmbedder));
    let cache = super::embedding::cache(&call.home);
    let r = duduclaw_knowledge::search::search(conn, &allowed, query, limit, embedder.as_ref(), &cache)?;
    if let Some(reason) = &r.vector_unavailable {
        super::embedding::warn_keyword_only(reason);
    }
    let mut entry = access("search", &call.user, None);
    entry.query = Some(query.to_string());
    entry.allowed = allowed.ids().to_vec();
    entry.results = r
        .hits
        .iter()
        .enumerate()
        .map(|(i, h)| json!({"chunk_id": h.chunk_id, "source_id": h.source_id, "rank": i + 1}))
        .collect();
    entry.mode = Some(r.mode.as_str().to_string());
    entry.outcome = if r.hits.is_empty() {
        "no_match".into()
    } else {
        "ok".into()
    };
    log_access(conn, &entry)?;
    Ok(serde_json::to_value(&r).unwrap_or(Value::Null))
}

fn passage(conn: &Connection, call: &Call) -> KbResult<Value> {
    let p = &call.params;
    let chunk_id = p
        .get("chunk_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| KbError::Invalid("缺少參數 chunk_id。".to_string()))?;
    let human = dashboard_human(&call.user);
    let allowed = acl::effective_collections(conn, &human, None, &Narrowing::default())?;
    let ctx = p
        .get("context_chars")
        .and_then(Value::as_u64)
        .unwrap_or(600) as usize;
    let mut entry = access("get_passage", &call.user, None);
    entry.allowed = allowed.ids().to_vec();
    let result = duduclaw_knowledge::passage::get_passage(conn, chunk_id, &allowed, ctx);
    match &result {
        Ok(pg) => {
            entry.collection_id = Some(pg.collection_id.clone());
            entry.results = vec![json!({"chunk_id": pg.chunk_id, "source_id": pg.source_id})];
        }
        Err(_) => entry.outcome = "denied".into(),
    }
    log_access(conn, &entry)?;
    Ok(serde_json::to_value(result?).unwrap_or(Value::Null))
}

/// Paste and URL sources (files come through the upload route).
async fn add_source(call: Call) -> KbResult<Value> {
    let p = call.params.clone();
    let collection_id = req(&p, "collection_id")?.to_string();
    let kind = req(&p, "kind")?.to_string();
    let (new_kind, format, title, url, bytes) = match kind.as_str() {
        "paste" => {
            let text = p.get("text").and_then(Value::as_str).unwrap_or("");
            if text.trim().is_empty() {
                return Err(KbError::Invalid("請貼上要加入的文字。".to_string()));
            }
            if text.chars().count() > call.cfg.max_source_chars {
                return Err(KbError::Rejected {
                    code: "too_many_chars",
                    message: format!("文字超過 {} 字的上限。", call.cfg.max_source_chars),
                });
            }
            let title = s(&p, "title").unwrap_or("貼上的文字").to_string();
            (
                SourceKind::Paste,
                Format::Txt,
                title,
                None,
                Some(text.as_bytes().to_vec()),
            )
        }
        "url" => {
            let url =
                super::url_ingest::validate(req(&p, "url")?).map_err(|e| KbError::Rejected {
                    code: "url_blocked",
                    message: e.message,
                })?;
            let title = s(&p, "title")
                .map(str::to_string)
                .unwrap_or_else(|| url.clone());
            (SourceKind::Url, Format::Html, title, Some(url), None)
        }
        _ => {
            return Err(KbError::Invalid(
                "kind 只能是 paste 或 url（檔案請用上傳）。".to_string(),
            ));
        }
    };
    let home = call.home.clone();
    let result = blocking(move || {
        let mut conn = schema::open_home(&home)?;
        role_on(&conn, &collection_id, &call.user, Role::Editor)?;
        let store = BlobStore::for_home(&home);
        let (sha, size) = match &bytes {
            Some(b) => (store.put(b)?, b.len() as u64),
            None => (
                duduclaw_knowledge::blob::url_identity(url.as_deref().unwrap_or("")),
                0,
            ),
        };
        let new = NewSource {
            collection_id: collection_id.clone(),
            kind: new_kind,
            title,
            original_name: None,
            url: url.clone(),
            format,
            blob_sha256: sha,
            bytes: size,
            origin: json!({"added_by": call.user.user_id, "via": "dashboard"}),
            created_by: call.user.user_id.clone(),
        };
        // A refused paste leaves an unreferenced blob for the GC sweep.
        let added = duduclaw_knowledge::sources::add_source(&mut conn, &new, &call.cfg)?;
        let mut entry = access("add_source", &call.user, Some(&collection_id));
        let (duplicate, source) = match added {
            Added::New(s) => (false, s),
            Added::Duplicate(s) => (true, s),
        };
        entry.results = vec![json!({"source_id": source.id, "duplicate": duplicate})];
        log_access(&conn, &entry)?;
        Ok(json!({ "source": source, "duplicate": duplicate }))
    })
    .await?;
    super::worker::notify();
    Ok(result)
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
