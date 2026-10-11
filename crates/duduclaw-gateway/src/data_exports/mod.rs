//! Private customer data exports. Files never enter the shared attachment bucket.
//! JWT/agent/audience visibility is refreshed at create, build, and download.
mod lifecycle;
mod limits;
mod source;
pub use lifecycle::{initialize, sweep};
use lifecycle::{job_dir, private_directory, private_file};
pub use limits::DownloadPermit;
mod store;
#[cfg(test)]
mod tests;
mod wiki_source;
mod worker_lease;
mod writer;
use chrono::{DateTime, Utc};
use duduclaw_auth::{UserContext, models::AccessLevel};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub type ExportResult<T> = Result<T, String>;
pub const MAX_ROWS: usize = 100_000;
pub const MAX_BYTES: u64 = 50 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Dataset {
    Tasks,
    Conversations,
    Usage,
    Memories,
    Wiki,
}
impl Dataset {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Conversations => "conversations",
            Self::Usage => "usage",
            Self::Memories => "memories",
            Self::Wiki => "wiki",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Csv,
    Xlsx,
}
impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub dataset: Dataset,
    pub format: ExportFormat,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct ExportStatus {
    pub export_id: String,
    pub state: String,
    pub dataset: Dataset,
    pub format: ExportFormat,
    pub logical_rows: u64,
    pub physical_rows: u64,
    pub size_bytes: u64,
    pub expires_at: String,
    pub error_code: Option<String>,
}
pub struct DownloadedExport {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub content_type: &'static str,
    /// The HTTP body must retain this until end-of-stream or disconnect.
    pub permit: DownloadPermit,
}
#[derive(Serialize, Deserialize)]
struct ManifestItem {
    id: String,
    sha256: String,
}

fn is_expired(stamp: &str) -> ExportResult<bool> {
    Ok(DateTime::parse_from_rfc3339(stamp).map_err(|_| "EXPORT_IO".to_string())? <= Utc::now())
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn valid_agent(id: &str) -> bool {
    duduclaw_core::is_valid_new_agent_id(id)
}

pub(super) fn fresh(home: &Path, ctx: &UserContext) -> ExportResult<UserContext> {
    let live = crate::handlers::task_privacy::live_reader_context(home, ctx)
        .map_err(|_| "EXPORT_PERMISSION_DENIED".to_string())?;
    if live.requires_password_change() {
        return Err("EXPORT_PERMISSION_DENIED".into());
    }
    Ok(live)
}
pub(super) fn authorize(
    home: &Path,
    ctx: &UserContext,
    request: &ExportRequest,
) -> ExportResult<UserContext> {
    let live = fresh(home, ctx)?;
    if let Some(id) = &request.agent_id {
        if !valid_agent(id) {
            return Err("EXPORT_INVALID_REQUEST".into());
        }
        if !live.has_agent_access(id, AccessLevel::Viewer) {
            return Err("EXPORT_PERMISSION_DENIED".into());
        }
    }
    if request
        .from
        .zip(request.until)
        .is_some_and(|(from, until)| from >= until)
    {
        return Err("EXPORT_INVALID_REQUEST".into());
    }
    if request.dataset == Dataset::Usage && !live.is_admin() {
        return Err("EXPORT_PERMISSION_DENIED".into());
    }
    let shared = request.dataset == Dataset::Wiki && request.scope.as_deref() == Some("shared");
    if request
        .scope
        .as_deref()
        .is_some_and(|s| s != "agent" && s != "shared")
        || (request.scope.as_deref() == Some("shared") && !shared)
        || (shared && request.agent_id.is_some())
    {
        return Err("EXPORT_INVALID_REQUEST".into());
    }
    if !shared
        && request.agent_id.is_none()
        && (!live.is_admin() || matches!(request.dataset, Dataset::Memories | Dataset::Wiki))
    {
        return Err("EXPORT_INVALID_REQUEST".into());
    }
    Ok(live)
}
fn reserve(
    home: &Path,
    ctx: &UserContext,
    request: &ExportRequest,
) -> ExportResult<(String, worker_lease::WorkerLease)> {
    let live = authorize(home, ctx, request)?;
    sweep(home)?;
    let mut conn = store::db(home)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "EXPORT_IO".to_string())?;
    let total: u64 = tx
        .query_row(
            "SELECT COUNT(*) FROM exports WHERE worker_active=1 OR state IN ('queued','running')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "EXPORT_IO".to_string())?;
    let own: u64 = tx
        .query_row(
            "SELECT COUNT(*) FROM exports WHERE owner=?1 AND (worker_active=1 OR state IN ('queued','running'))",
            params![live.user_id],
            |r| r.get(0),
        )
        .map_err(|_| "EXPORT_IO".to_string())?;
    if own >= 1 || total >= 2 {
        return Err("EXPORT_BUSY".into());
    }
    limits::retained_capacity(&tx, &live.user_id, 0)?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    job_dir(home, &id)?;
    let mut lease = worker_lease::try_lease(home, &id)?.ok_or_else(|| "EXPORT_BUSY".to_string())?;
    let json = serde_json::to_string(request).map_err(|_| "EXPORT_INVALID_REQUEST".to_string())?;
    let expires = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    tx.execute(
        "INSERT INTO exports(id,owner,request,state,expires_at,worker_active) VALUES (?1,?2,?3,'queued',?4,1)",
        params![id, live.user_id, json, expires],
    )
    .map_err(|_| "EXPORT_IO".to_string())?;
    store::audit(&tx, &id, &live.user_id, "created", request, 0)?;
    tx.commit().map_err(|_| "EXPORT_IO".to_string())?;
    lease.activate();
    Ok((id, lease))
}
pub async fn create(
    home: &Path,
    ctx: &UserContext,
    request: ExportRequest,
) -> ExportResult<String> {
    let home = home.to_path_buf();
    let ctx = ctx.clone();
    let reserve_home = home.clone();
    let reserve_ctx = ctx.clone();
    let reserve_request = request.clone();
    let (id, lease) =
        tokio::task::spawn_blocking(move || reserve(&reserve_home, &reserve_ctx, &reserve_request))
            .await
            .map_err(|_| "EXPORT_IO".to_string())??;
    let job_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build(&home, &ctx, &request, &job_id)
        }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => store::fail(&home, &job_id, &error),
            Err(_) => store::fail(&home, &job_id, "EXPORT_INTERNAL"),
        }
    });
    Ok(id)
}
fn digest(row: &writer::LogicalRow) -> ExportResult<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(row).map_err(|_| "EXPORT_IO".to_string())?)
    ))
}
fn manifest(rows: &[writer::LogicalRow]) -> ExportResult<Vec<ManifestItem>> {
    rows.iter()
        .map(|row| {
            Ok(ManifestItem {
                id: row.id.clone(),
                sha256: digest(row)?,
            })
        })
        .collect()
}
fn verify(
    home: &Path,
    ctx: &UserContext,
    request: &ExportRequest,
    manifest: &[ManifestItem],
    checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<()> {
    let table = source::collect(home, ctx, request, checkpoint)?;
    let current: std::collections::HashMap<_, _> = manifest_fn(&table.rows)?
        .into_iter()
        .map(|item| (item.id, item.sha256))
        .collect();
    if manifest
        .iter()
        .any(|item| current.get(&item.id) != Some(&item.sha256))
    {
        return Err("EXPORT_SOURCE_CHANGED".into());
    }
    Ok(())
}
fn manifest_fn(rows: &[writer::LogicalRow]) -> ExportResult<Vec<ManifestItem>> {
    manifest(rows)
}
fn build(home: &Path, ctx: &UserContext, request: &ExportRequest, id: &str) -> ExportResult<()> {
    let conn = store::db(home)?;
    if conn
        .execute(
            "UPDATE exports SET state='running' WHERE id=?1 AND state='queued'",
            params![id],
        )
        .map_err(|_| "EXPORT_IO".to_string())?
        != 1
    {
        return Err("EXPORT_CANCELLED".into());
    }
    authorize(home, ctx, request)?;
    let table = source::collect(home, ctx, request, || store::checkpoint(home, id))?;
    let manifest = manifest(&table.rows)?;
    let directory = job_dir(home, id)?;
    let partial = directory.join("result.partial");
    let physical = writer::encode(
        &partial,
        request.format,
        &table.headers,
        &table.rows,
        || store::checkpoint(home, id),
    )?;
    verify(home, ctx, request, &manifest, || {
        store::checkpoint(home, id)
    })?;
    let size = std::fs::metadata(&partial)
        .map_err(|_| "EXPORT_IO".to_string())?
        .len();
    if size > MAX_BYTES {
        return Err("EXPORT_TOO_LARGE".into());
    }
    let mut conn = store::db(home)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "EXPORT_IO".to_string())?;
    let (_, _, status, _) = store::status(&tx, id)?;
    if status.state != "running" {
        return Err("EXPORT_CANCELLED".into());
    }
    authorize(home, ctx, request)?;
    limits::retained_capacity(&tx, &ctx.user_id, size)?;
    std::fs::rename(
        &partial,
        directory.join(format!("result.{}", request.format.extension())),
    )
    .map_err(|_| "EXPORT_IO".to_string())?;
    #[cfg(unix)]
    std::fs::File::open(&directory)
        .and_then(|f| f.sync_all())
        .map_err(|_| "EXPORT_IO".to_string())?;
    tx.execute("UPDATE exports SET state='ready',logical_rows=?2,physical_rows=?3,size_bytes=?4,expires_at=?5,manifest=?6 WHERE id=?1",params![id,table.rows.len() as u64,physical as u64,size,(Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),serde_json::to_string(&manifest).map_err(|_|"EXPORT_IO".to_string())?]).map_err(|_|"EXPORT_IO".to_string())?;
    store::audit(
        &tx,
        id,
        &ctx.user_id,
        "ready",
        request,
        table.rows.len() as u64,
    )?;
    tx.commit().map_err(|_| "EXPORT_IO".to_string())?;
    Ok(())
}
fn owned(
    home: &Path,
    ctx: &UserContext,
    id: &str,
) -> ExportResult<(rusqlite::Connection, ExportRequest, ExportStatus, String)> {
    if !valid_id(id) {
        return Err("EXPORT_NOT_FOUND".into());
    }
    let live = fresh(home, ctx)?;
    sweep(home)?;
    let conn = store::db(home)?;
    let (owner, request, status, manifest) = store::status(&conn, id)?;
    if owner != live.user_id {
        return Err("EXPORT_NOT_FOUND".into());
    }
    authorize(home, &live, &request)?;
    Ok((conn, request, status, manifest))
}
pub fn get(home: &Path, ctx: &UserContext, id: &str) -> ExportResult<ExportStatus> {
    let (_, _, status, _) = owned(home, ctx, id)?;
    Ok(status)
}
pub fn cancel(home: &Path, ctx: &UserContext, id: &str) -> ExportResult<ExportStatus> {
    let (conn, request, status, _) = owned(home, ctx, id)?;
    conn.execute("UPDATE exports SET state='cancelled',error_code='EXPORT_CANCELLED',manifest='[]' WHERE id=?1 AND state IN ('queued','running','ready')",params![id]).map_err(|_|"EXPORT_IO".to_string())?;
    store::audit(
        &conn,
        id,
        &ctx.user_id,
        "cancelled",
        &request,
        status.logical_rows,
    )?;
    store::remove_outputs(home, id)?;
    Ok(store::status(&conn, id)?.2)
}
pub fn download(home: &Path, ctx: &UserContext, id: &str) -> ExportResult<DownloadedExport> {
    let permit = limits::download_permit(home)?;
    let (conn, request, status, manifest) = owned(home, ctx, id)?;
    if status.state != "ready" {
        return Err("EXPORT_NOT_READY".into());
    }
    let manifest: Vec<ManifestItem> =
        serde_json::from_str(&manifest).map_err(|_| "EXPORT_IO".to_string())?;
    let result = (|| {
        verify(home, ctx, &request, &manifest, || Ok(()))?;
        let path = job_dir(home, id)?.join(format!("result.{}", request.format.extension()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path).map_err(|_| "EXPORT_IO".to_string())?;
        if file.metadata().map_err(|_| "EXPORT_IO".to_string())?.len() != status.size_bytes {
            return Err("EXPORT_IO".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "EXPORT_IO".to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("EXPORT_TOO_LARGE".into());
        }
        // Revalidate after file read, so revocation during file IO also denies delivery.
        verify(home, ctx, &request, &manifest, || Ok(()))?;
        let (_, _, final_status, _) = store::status(&conn, id)?;
        if final_status.state != "ready" || is_expired(&final_status.expires_at)? {
            return Err("EXPORT_NOT_READY".into());
        }
        Ok(DownloadedExport {
            bytes,
            permit,
            filename: format!(
                "duduclaw-{}-{}.{}",
                request.dataset.name(),
                id,
                request.format.extension()
            ),
            content_type: match request.format {
                ExportFormat::Csv => "text/csv; charset=utf-8",
                ExportFormat::Xlsx => {
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                }
            },
        })
    })();
    store::audit(
        &conn,
        id,
        &ctx.user_id,
        if result.is_ok() {
            "downloaded"
        } else {
            "download_denied"
        },
        &request,
        status.logical_rows,
    )?;
    result
}
