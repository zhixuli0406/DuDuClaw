//! Sources (one uploaded file, pasted text or web page) and their lifecycle.
//!
//! Status: `queued → extracting → indexing → ready` (`status_detail =
//! keyword_only` while there is no vector model), or `failed` with a
//! closed-set `error_code` and a zh-TW message. A source is deduplicated per
//! collection by the hash of its original bytes.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::extract::Format;
use crate::limits::KnowledgeConfig;
use crate::{KbError, KbResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    File,
    Paste,
    Url,
    Attachment,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::File => "file",
            SourceKind::Paste => "paste",
            SourceKind::Url => "url",
            SourceKind::Attachment => "attachment",
        }
    }
}

/// Progress of the newest job of a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub kind: String,
    pub state: String,
    pub attempts: i64,
    pub progress_done: i64,
    pub progress_total: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub collection_id: String,
    pub kind: String,
    pub title: String,
    pub original_name: Option<String>,
    pub url: Option<String>,
    pub format: String,
    pub blob_sha256: String,
    pub content_sha256: Option<String>,
    pub bytes: i64,
    pub chars: i64,
    pub pages: Option<i64>,
    pub chunk_count: i64,
    pub status: String,
    pub status_detail: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub warnings: Value,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub indexed_at: Option<String>,
    pub extractor_version: Option<String>,
    pub chunker_version: Option<String>,
    pub job: Option<JobProgress>,
}

/// Input for [`add_source`].
#[derive(Debug, Clone)]
pub struct NewSource {
    pub collection_id: String,
    pub kind: SourceKind,
    pub title: String,
    pub original_name: Option<String>,
    pub url: Option<String>,
    pub format: Format,
    pub blob_sha256: String,
    pub bytes: u64,
    pub origin: Value,
    pub created_by: String,
}

/// Result of [`add_source`].
#[derive(Debug, Clone, PartialEq)]
pub enum Added {
    New(Source),
    /// The same bytes are already in this collection.
    Duplicate(Source),
}

const COLS: &str = "id, collection_id, kind, title, original_name, url, format, blob_sha256, content_sha256, bytes, chars, pages, chunk_count, status, status_detail, error_code, error_message, warnings_json, created_by, created_at, updated_at, indexed_at, extractor_version, chunker_version";

fn row_to_source(r: &Row<'_>) -> rusqlite::Result<Source> {
    let warnings: String = r.get(17)?;
    Ok(Source {
        id: r.get(0)?,
        collection_id: r.get(1)?,
        kind: r.get(2)?,
        title: r.get(3)?,
        original_name: r.get(4)?,
        url: r.get(5)?,
        format: r.get(6)?,
        blob_sha256: r.get(7)?,
        content_sha256: r.get(8)?,
        bytes: r.get(9)?,
        chars: r.get(10)?,
        pages: r.get(11)?,
        chunk_count: r.get(12)?,
        status: r.get(13)?,
        status_detail: r.get(14)?,
        error_code: r.get(15)?,
        error_message: r.get(16)?,
        warnings: serde_json::from_str(&warnings).unwrap_or(Value::Array(Vec::new())),
        created_by: r.get(18)?,
        created_at: r.get(19)?,
        updated_at: r.get(20)?,
        indexed_at: r.get(21)?,
        extractor_version: r.get(22)?,
        chunker_version: r.get(23)?,
        job: None,
    })
}

fn latest_job(conn: &Connection, source_id: &str) -> KbResult<Option<JobProgress>> {
    Ok(conn
        .query_row(
            "SELECT kind, state, attempts, progress_done, progress_total FROM ingest_jobs
              WHERE source_id = ?1 ORDER BY id DESC LIMIT 1",
            params![source_id],
            |r| {
                Ok(JobProgress {
                    kind: r.get(0)?,
                    state: r.get(1)?,
                    attempts: r.get(2)?,
                    progress_done: r.get(3)?,
                    progress_total: r.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Display title: control characters removed, at most 120 characters.
pub fn clean_title(title: &str) -> String {
    let t: String = title.chars().filter(|c| !c.is_control()).collect();
    let t = duduclaw_core::truncate_chars(t.trim(), 120);
    if t.is_empty() {
        "未命名".to_string()
    } else {
        t
    }
}

pub fn get_source(conn: &Connection, id: &str) -> KbResult<Option<Source>> {
    let src = conn
        .query_row(
            &format!("SELECT {COLS} FROM sources WHERE id = ?1"),
            params![id],
            row_to_source,
        )
        .optional()?;
    match src {
        Some(mut s) => {
            s.job = latest_job(conn, &s.id)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

pub fn list_sources(conn: &Connection, collection_id: &str) -> KbResult<Vec<Source>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM sources WHERE collection_id = ?1 ORDER BY created_at DESC, id"
    ))?;
    let mut out: Vec<Source> = stmt
        .query_map(params![collection_id], row_to_source)?
        .collect::<Result<_, _>>()?;
    for s in &mut out {
        s.job = latest_job(conn, &s.id)?;
    }
    Ok(out)
}

/// Insert a queued source and its ingest job, or return the duplicate.
pub fn add_source(
    conn: &mut Connection,
    new: &NewSource,
    cfg: &KnowledgeConfig,
) -> KbResult<Added> {
    if new.bytes > cfg.max_source_bytes {
        return Err(KbError::Rejected {
            code: "too_large",
            message: format!(
                "檔案超過 {} MB 上限。",
                cfg.max_source_bytes / (1024 * 1024)
            ),
        });
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM sources WHERE collection_id = ?1 AND blob_sha256 = ?2",
            params![new.collection_id, new.blob_sha256],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        drop(tx);
        return Ok(Added::Duplicate(
            get_source(conn, &id)?.ok_or(KbError::PermissionDenied)?,
        ));
    }
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sources WHERE collection_id = ?1",
        params![new.collection_id],
        |r| r.get(0),
    )?;
    if count as usize >= cfg.max_sources_per_collection {
        return Err(KbError::Rejected {
            code: "quota_sources",
            message: format!(
                "這個集合的來源數量已達上限（{} 份）。",
                cfg.max_sources_per_collection
            ),
        });
    }
    let (coll_bytes, total_bytes): (i64, i64) = tx.query_row(
        "SELECT COALESCE(SUM(CASE WHEN collection_id = ?1 THEN bytes ELSE 0 END), 0), COALESCE(SUM(bytes), 0) FROM sources",
        params![new.collection_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    if coll_bytes as u64 + new.bytes > cfg.max_collection_bytes {
        return Err(KbError::Rejected {
            code: "quota_bytes",
            message: format!(
                "這個集合的原始檔總量已達上限（{:.1} GB）。",
                gib(cfg.max_collection_bytes)
            ),
        });
    }
    if total_bytes as u64 + new.bytes > cfg.max_total_bytes {
        return Err(KbError::Rejected {
            code: "quota_bytes",
            message: format!(
                "知識庫的原始檔總量已達上限（{:.1} GB）。",
                gib(cfg.max_total_bytes)
            ),
        });
    }
    let id = format!("src_{}", uuid::Uuid::new_v4().simple());
    let now = crate::now_rfc3339();
    tx.execute(
        "INSERT INTO sources(id, collection_id, kind, title, original_name, url, format, blob_sha256, bytes, status,
                             origin_json, created_by, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10, ?11, ?12, ?12)",
        params![
            id,
            new.collection_id,
            new.kind.as_str(),
            clean_title(&new.title),
            new.original_name.as_deref().map(clean_title),
            new.url,
            new.format.as_str(),
            new.blob_sha256,
            new.bytes as i64,
            new.origin.to_string(),
            new.created_by,
            now,
        ],
    )?;
    crate::jobs::enqueue_tx(&tx, &id, crate::jobs::JobKind::Ingest)?;
    tx.commit()?;
    Ok(Added::New(
        get_source(conn, &id)?.ok_or(KbError::PermissionDenied)?,
    ))
}

/// Move a source to `status` (no-op on a deleting / deleted source).
pub fn set_status(conn: &Connection, id: &str, status: &str) -> KbResult<bool> {
    let n = conn.execute(
        "UPDATE sources SET status = ?2, updated_at = ?3 WHERE id = ?1 AND status <> 'deleting'",
        params![id, status, crate::now_rfc3339()],
    )?;
    Ok(n > 0)
}

/// Record a failure. A source that already has an index (a failed reindex)
/// stays searchable: it returns to `ready` with the error kept for display.
pub fn record_failure(conn: &Connection, id: &str, code: &str, message: &str) -> KbResult<()> {
    conn.execute(
        "UPDATE sources SET status = CASE WHEN chunk_count > 0 THEN 'ready' ELSE 'failed' END,
                error_code = ?2, error_message = ?3, updated_at = ?4
          WHERE id = ?1 AND status <> 'deleting'",
        params![
            id,
            code,
            duduclaw_core::truncate_chars(message, 500),
            crate::now_rfc3339()
        ],
    )?;
    Ok(())
}

/// Delete a source with everything derived from it. Returns the blob hash
/// when no other source references it any more (caller removes the file).
pub fn delete_source(conn: &mut Connection, id: &str) -> KbResult<Option<String>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let sha: Option<String> = tx
        .query_row(
            "SELECT blob_sha256 FROM sources WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(sha) = sha else {
        return Err(KbError::PermissionDenied);
    };
    tx.execute(
        "UPDATE sources SET status = 'deleting' WHERE id = ?1",
        params![id],
    )?;
    tx.execute(
        "UPDATE ingest_jobs SET state = 'cancelled', finished_at = ?2 WHERE source_id = ?1 AND state IN ('queued','running')",
        params![id, crate::now_rfc3339()],
    )?;
    // Chunks first so the FTS delete trigger runs for every row.
    tx.execute("DELETE FROM chunks WHERE source_id = ?1", params![id])?;
    tx.execute("DELETE FROM sources WHERE id = ?1", params![id])?;
    crate::schema::bump_generation(&tx)?;
    let refs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sources WHERE blob_sha256 = ?1",
        params![sha],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok((refs == 0).then_some(sha))
}

/// Queue a reindex of a source that is not already being processed.
pub fn request_reindex(conn: &mut Connection, id: &str) -> KbResult<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM sources WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    match status.as_deref() {
        None | Some("deleting") => return Err(KbError::PermissionDenied),
        Some("queued" | "extracting" | "indexing") => {
            return Err(KbError::Invalid(
                "這份來源正在處理中，請等完成後再重建。".to_string(),
            ));
        }
        Some("awaiting_approval") => {
            return Err(KbError::Invalid("這份來源還在等待審批。".to_string()));
        }
        _ => {}
    }
    let kind = if status.as_deref() == Some("failed") {
        crate::jobs::JobKind::Ingest
    } else {
        crate::jobs::JobKind::Reindex
    };
    if kind == crate::jobs::JobKind::Ingest {
        tx.execute(
            "UPDATE sources SET status = 'queued', error_code = NULL, error_message = NULL, updated_at = ?2 WHERE id = ?1",
            params![id, crate::now_rfc3339()],
        )?;
    }
    crate::jobs::enqueue_tx(&tx, id, kind)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) fn new_source(collection_id: &str, sha: &str) -> NewSource {
        NewSource {
            collection_id: collection_id.to_string(),
            kind: SourceKind::File,
            title: "員工手冊.pdf".into(),
            original_name: Some("員工手冊.pdf".into()),
            url: None,
            format: Format::Pdf,
            blob_sha256: sha.to_string(),
            bytes: 10,
            origin: serde_json::json!({"uploaded_by": "u"}),
            created_by: "u".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::new_source;
    use super::*;
    use crate::store::{NewCollection, create_collection};

    #[test]
    fn dedupe_by_hash_within_collection_only() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let a = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let b = create_collection(
            &mut conn,
            &NewCollection {
                name: "B".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let sha = "a".repeat(64);
        let Added::New(first) = add_source(&mut conn, &new_source(&a.id, &sha), &cfg).unwrap()
        else {
            panic!()
        };
        assert_eq!(first.status, "queued");
        assert_eq!(first.job.as_ref().unwrap().state, "queued");
        let Added::Duplicate(dup) = add_source(&mut conn, &new_source(&a.id, &sha), &cfg).unwrap()
        else {
            panic!()
        };
        assert_eq!(dup.id, first.id);
        assert!(matches!(
            add_source(&mut conn, &new_source(&b.id, &sha), &cfg).unwrap(),
            Added::New(_)
        ));
        // Shared blob: deleting one reference keeps the file.
        assert_eq!(delete_source(&mut conn, &first.id).unwrap(), None);
    }

    #[test]
    fn quotas_and_size() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig {
            max_sources_per_collection: 1,
            ..Default::default()
        };
        let a = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        add_source(&mut conn, &new_source(&a.id, &"1".repeat(64)), &cfg).unwrap();
        let err = add_source(&mut conn, &new_source(&a.id, &"2".repeat(64)), &cfg).unwrap_err();
        assert_eq!(err.code(), "quota_sources");
        let mut big = new_source(&a.id, &"3".repeat(64));
        big.bytes = cfg.max_source_bytes + 1;
        assert_eq!(
            add_source(&mut conn, &big, &cfg).unwrap_err().code(),
            "too_large"
        );
    }

    #[test]
    fn byte_quotas() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig {
            max_collection_bytes: 100,
            max_total_bytes: 150,
            ..Default::default()
        };
        let a = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let b = create_collection(
            &mut conn,
            &NewCollection {
                name: "B".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let mut s1 = new_source(&a.id, &"a".repeat(64));
        s1.bytes = 90;
        add_source(&mut conn, &s1, &cfg).unwrap();
        let mut s2 = new_source(&a.id, &"b".repeat(64));
        s2.bytes = 20;
        assert_eq!(
            add_source(&mut conn, &s2, &cfg).unwrap_err().code(),
            "quota_bytes"
        );
        let mut s3 = new_source(&b.id, &"c".repeat(64));
        s3.bytes = 70;
        assert_eq!(
            add_source(&mut conn, &s3, &cfg).unwrap_err().code(),
            "quota_bytes",
            "home-wide"
        );
    }

    #[test]
    fn concurrent_writers_serialize_without_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knowledge.db");
        let cfg = KnowledgeConfig::default();
        let cid = {
            let mut conn = crate::schema::open(&path).unwrap();
            create_collection(
                &mut conn,
                &NewCollection {
                    name: "A".into(),
                    ..Default::default()
                },
                "u",
                &cfg,
            )
            .unwrap()
            .id
        };
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let (path, cid, cfg) = (path.clone(), cid.clone(), cfg.clone());
                std::thread::spawn(move || {
                    let mut conn = crate::schema::open(&path).unwrap();
                    let mut dup = 0;
                    for i in 0..25 {
                        // Half the hashes are shared by every thread.
                        let sha = if i % 2 == 0 {
                            format!("{i:064}")
                        } else {
                            format!("{t}{i:063}")
                        };
                        if matches!(
                            add_source(&mut conn, &new_source(&cid, &sha), &cfg).unwrap(),
                            Added::Duplicate(_)
                        ) {
                            dup += 1;
                        }
                    }
                    dup
                })
            })
            .collect();
        let dups: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
        let conn = crate::schema::open(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
            .unwrap();
        // 13 shared hashes once + 12 per-thread hashes × 4.
        assert_eq!(n, 13 + 48);
        assert_eq!(dups, 13 * 3);
    }

    #[test]
    fn failure_keeps_existing_index_searchable() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let a = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let Added::New(s) =
            add_source(&mut conn, &new_source(&a.id, &"4".repeat(64)), &cfg).unwrap()
        else {
            panic!()
        };
        record_failure(&conn, &s.id, "extract_timeout", "逾時").unwrap();
        assert_eq!(get_source(&conn, &s.id).unwrap().unwrap().status, "failed");
        conn.execute(
            "UPDATE sources SET chunk_count = 3 WHERE id = ?1",
            params![s.id],
        )
        .unwrap();
        record_failure(&conn, &s.id, "extract_timeout", "逾時").unwrap();
        let s2 = get_source(&conn, &s.id).unwrap().unwrap();
        assert_eq!(s2.status, "ready");
        assert_eq!(s2.error_code.as_deref(), Some("extract_timeout"));
    }
}
