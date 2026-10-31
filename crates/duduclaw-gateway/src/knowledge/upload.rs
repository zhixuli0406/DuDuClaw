//! `POST /api/knowledge/upload` and `GET /api/knowledge/sources/{id}/original`.
//!
//! Upload: multipart with a `collection_id` field **before** 1–20 `file`
//! parts. The caller must be an editor of the collection (checked before any
//! file byte is stored). Each file streams into a staging file while it is
//! hashed and counted; crossing `[knowledge] max_source_bytes` aborts that
//! file. Unsupported and legacy formats are refused by extension before
//! storage. Accepted files move into the content-addressed blob store and get
//! a queued source; the same bytes already in the collection answer
//! `duplicate`. The answer lists every file's outcome.
//!
//! Download: viewer of the source's collection; always
//! `Content-Disposition: attachment`, `application/octet-stream`,
//! `nosniff`, and an access-log row.

use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::Multipart;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use duduclaw_auth::UserContext;
use duduclaw_auth::db::UserDb;
use duduclaw_knowledge::acl::Role;
use duduclaw_knowledge::audit::{AccessEntry, log_access};
use duduclaw_knowledge::blob::BlobStore;
use duduclaw_knowledge::extract::Format;
use duduclaw_knowledge::limits::{KnowledgeConfig, MAX_FILES_PER_UPLOAD};
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use super::{blocking, dashboard_user};

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn file_result(name: &str, outcome: Result<(String, bool), (String, String)>) -> Value {
    match outcome {
        Ok((source_id, duplicate)) => {
            json!({"name": name, "ok": true, "source_id": source_id, "duplicate": duplicate})
        }
        Err((code, message)) => {
            json!({"name": name, "ok": false, "error_code": code, "message": message})
        }
    }
}

/// Stream one multipart field to staging. `Err((code, message))` on refusal.
async fn stage_field(
    field: &mut axum::extract::multipart::Field<'_>,
    store: &BlobStore,
    max: u64,
) -> Result<(std::path::PathBuf, String, u64), (String, String)> {
    let io = |_| {
        (
            "upload_failed".to_string(),
            "暫存上傳檔案失敗，請稍後再試。".to_string(),
        )
    };
    let staging = store.new_staging_path().map_err(|_| {
        (
            "upload_failed".to_string(),
            "暫存上傳檔案失敗，請稍後再試。".to_string(),
        )
    })?;
    let mut file = tokio::fs::File::create(&staging).await.map_err(io)?;
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                total += chunk.len() as u64;
                if total > max {
                    drop(file);
                    let _ = tokio::fs::remove_file(&staging).await;
                    return Err((
                        "too_large".into(),
                        format!("檔案超過 {} MB 上限。", max / (1024 * 1024)),
                    ));
                }
                hasher.update(&chunk);
                if file.write_all(&chunk).await.is_err() {
                    let _ = tokio::fs::remove_file(&staging).await;
                    return Err((
                        "upload_failed".into(),
                        "暫存上傳檔案失敗，請稍後再試。".into(),
                    ));
                }
            }
            Ok(None) => break,
            Err(_) => {
                let _ = tokio::fs::remove_file(&staging).await;
                return Err((
                    "upload_failed".into(),
                    "讀取上傳內容失敗（檔案過大或連線中斷）。".into(),
                ));
            }
        }
    }
    if file.flush().await.is_err() {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err((
            "upload_failed".into(),
            "暫存上傳檔案失敗，請稍後再試。".into(),
        ));
    }
    Ok((staging, hex::encode(hasher.finalize()), total))
}

async fn register_file(
    home: &Path,
    cfg: &KnowledgeConfig,
    user: &ResolvedUser,
    collection_id: &str,
    name: &str,
    format: Format,
    staged: (std::path::PathBuf, String, u64),
) -> Result<(String, bool), (String, String)> {
    let (staging, sha, size) = staged;
    if size == 0 {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(("empty_text".into(), "檔案是空的。".into()));
    }
    let (home, cfg, user, cid, name) = (
        home.to_path_buf(),
        cfg.clone(),
        user.clone(),
        collection_id.to_string(),
        name.to_string(),
    );
    blocking(move || {
        let store = BlobStore::for_home(&home);
        store.commit_staged(&staging, &sha)?;
        let mut conn = duduclaw_knowledge::schema::open_home(&home)?;
        let new = NewSource {
            collection_id: cid.clone(),
            kind: SourceKind::File,
            title: name.clone(),
            original_name: Some(name.clone()),
            url: None,
            format,
            blob_sha256: sha.clone(),
            bytes: size,
            origin: json!({"uploaded_by": user.user_id, "via": "dashboard_upload"}),
            created_by: user.user_id.clone(),
        };
        // A refused file stays as an unreferenced blob until the GC sweep
        // (removing it here could race an upload of the same bytes).
        let (source, duplicate) =
            match duduclaw_knowledge::sources::add_source(&mut conn, &new, &cfg)? {
                Added::New(s) => (s, false),
                Added::Duplicate(s) => (s, true),
            };
        let mut entry = AccessEntry {
            op: "add_source".into(),
            collection_id: Some(cid),
            requester_kind: "dashboard_user".into(),
            requester_id: Some(user.user_id.clone()),
            outcome: "ok".into(),
            ..Default::default()
        };
        entry.results = vec![json!({"source_id": source.id, "duplicate": duplicate})];
        log_access(&conn, &entry)?;
        Ok((source.id, duplicate))
    })
    .await
    .map_err(|e| (e.code().to_string(), e.public_message()))
}

/// Handle an authenticated upload.
pub async fn handle_upload(
    home: &Path,
    ctx: &UserContext,
    users: Option<Arc<UserDb>>,
    mut multipart: Multipart,
) -> Response {
    let cfg = KnowledgeConfig::from_home(home);
    if !cfg.enabled {
        return err(StatusCode::FORBIDDEN, "文件知識庫已由管理員關閉。");
    }
    let user = dashboard_user(ctx, users.as_ref());
    let store = BlobStore::for_home(home);
    let mut collection_id: Option<String> = None;
    let mut results = Vec::new();
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "上傳內容格式不正確或超過大小上限。",
                );
            }
        };
        match field.name().unwrap_or("") {
            "collection_id" => {
                let id = field.text().await.unwrap_or_default().trim().to_string();
                let (h, u, cid) = (home.to_path_buf(), user.clone(), id.clone());
                let allowed = blocking(move || {
                    let conn = duduclaw_knowledge::schema::open_home(&h)?;
                    super::principals::require_role(
                        &h,
                        &conn,
                        &cid,
                        &u.user_id,
                        u.department.as_deref(),
                        u.is_admin,
                        Role::Editor,
                    )
                })
                .await;
                if allowed.is_err() {
                    return err(StatusCode::FORBIDDEN, "permission denied");
                }
                collection_id = Some(id);
            }
            "file" => {
                let Some(cid) = collection_id.clone() else {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "collection_id 欄位必須放在檔案之前。",
                    );
                };
                if results.len() >= MAX_FILES_PER_UPLOAD {
                    return err(
                        StatusCode::BAD_REQUEST,
                        &format!("一次最多上傳 {MAX_FILES_PER_UPLOAD} 個檔案。"),
                    );
                }
                let name =
                    duduclaw_knowledge::sources::clean_title(field.file_name().unwrap_or("未命名"));
                let format = match Format::from_file_name(&name) {
                    Ok(f) => f,
                    Err(e) => {
                        // Drain nothing: the next `next_field` skips the rest.
                        results.push(file_result(&name, Err((e.code, e.message))));
                        continue;
                    }
                };
                let outcome = match stage_field(&mut field, &store, cfg.max_source_bytes).await {
                    Ok(staged) => {
                        register_file(home, &cfg, &user, &cid, &name, format, staged).await
                    }
                    Err(e) => Err(e),
                };
                results.push(file_result(&name, outcome));
            }
            _ => {}
        }
    }
    if collection_id.is_none() {
        return err(StatusCode::BAD_REQUEST, "缺少 collection_id 欄位。");
    }
    if results.is_empty() {
        return err(StatusCode::BAD_REQUEST, "沒有收到任何檔案。");
    }
    if results.iter().any(|r| r["ok"] == true) {
        super::worker::notify();
    }
    (StatusCode::OK, Json(json!({ "files": results }))).into_response()
}

fn ascii_file_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    duduclaw_core::truncate_bytes(&cleaned, 100).to_string()
}

/// RFC 5987 `filename*` value: unreserved bytes kept, the rest `%XX`.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Handle an authenticated original-file download.
pub async fn handle_original(
    home: &Path,
    ctx: &UserContext,
    users: Option<Arc<UserDb>>,
    source_id: &str,
) -> Response {
    if !super::enabled(home) {
        return err(StatusCode::FORBIDDEN, "文件知識庫已由管理員關閉。");
    }
    let user = dashboard_user(ctx, users.as_ref());
    let (h, sid) = (home.to_path_buf(), source_id.to_string());
    let found = blocking(move || {
        let conn = duduclaw_knowledge::schema::open_home(&h)?;
        let src = duduclaw_knowledge::sources::get_source(&conn, &sid)?
            .ok_or(duduclaw_knowledge::KbError::PermissionDenied)?;
        let allowed = super::principals::effective_collections(
            &h,
            &conn,
            &super::dashboard_human(&user),
            None,
            &duduclaw_knowledge::acl::Narrowing::default(),
        )?;
        let mut entry = AccessEntry {
            op: "blob_download".into(),
            collection_id: Some(src.collection_id.clone()),
            requester_kind: "dashboard_user".into(),
            requester_id: Some(user.user_id.clone()),
            results: vec![json!({"source_id": src.id})],
            outcome: "ok".into(),
            ..Default::default()
        };
        if !allowed.contains(&src.collection_id) {
            entry.outcome = "denied".into();
            log_access(&conn, &entry)?;
            return Err(duduclaw_knowledge::KbError::PermissionDenied);
        }
        log_access(&conn, &entry)?;
        Ok(src)
    })
    .await;
    let src = match found {
        Ok(s) => s,
        Err(_) => return err(StatusCode::FORBIDDEN, "permission denied"),
    };
    let Ok(path) = BlobStore::for_home(home).path_for(&src.blob_sha256) else {
        return err(StatusCode::NOT_FOUND, "找不到原始檔。");
    };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return err(StatusCode::NOT_FOUND, "找不到原始檔。"),
    };
    let base = src
        .original_name
        .clone()
        .unwrap_or_else(|| format!("{}.{}", src.title, src.format));
    let disposition = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii_file_name(&base),
        percent_encode(&base)
    );
    let mut resp = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(file)));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&disposition) {
        h.insert(header::CONTENT_DISPOSITION, v);
    } else {
        h.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    resp
}

#[cfg(test)]
#[path = "upload_tests.rs"]
mod tests;
