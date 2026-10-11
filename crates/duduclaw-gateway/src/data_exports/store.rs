//! Durable private jobs. Quotas and state transitions use SQLite transactions.
use super::*;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn db(home: &Path) -> ExportResult<Connection> {
    let root = private_directory(&home.join("data_exports"))?;
    let path = root.join("jobs.db");
    if !path.exists() {
        match private_file(&path) {
            Ok(_) => {}
            Err(_) if path.exists() => {}
            Err(e) => return Err(e),
        }
    }
    if std::fs::symlink_metadata(&path)
        .map_err(|_| "EXPORT_IO".to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("EXPORT_IO".into());
    }
    let conn = Connection::open(path).map_err(|_| "EXPORT_IO".to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "EXPORT_IO".to_string())?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
      CREATE TABLE IF NOT EXISTS exports (
        id TEXT PRIMARY KEY, owner TEXT NOT NULL, request TEXT NOT NULL,
        state TEXT NOT NULL, logical_rows INTEGER NOT NULL DEFAULT 0,
        physical_rows INTEGER NOT NULL DEFAULT 0, size_bytes INTEGER NOT NULL DEFAULT 0,
        expires_at TEXT NOT NULL, error_code TEXT, manifest TEXT NOT NULL DEFAULT '[]',worker_active INTEGER NOT NULL DEFAULT 0);
      CREATE INDEX IF NOT EXISTS idx_exports_owner_state ON exports(owner,state);
      CREATE TABLE IF NOT EXISTS export_audit (
        id INTEGER PRIMARY KEY, export_id TEXT NOT NULL, actor TEXT NOT NULL,
        event TEXT NOT NULL, dataset TEXT NOT NULL, filters TEXT NOT NULL,
        logical_rows INTEGER NOT NULL, timestamp TEXT NOT NULL);",
    )
    .map_err(|_| "EXPORT_IO".to_string())?;
    match conn.execute(
        "ALTER TABLE exports ADD COLUMN worker_active INTEGER NOT NULL DEFAULT 0",
        [],
    ) {
        Ok(_) => {}
        Err(error) if error.to_string().contains("duplicate column name") => {}
        Err(_) => return Err("EXPORT_IO".into()),
    }
    Ok(conn)
}
pub(super) fn status(
    conn: &Connection,
    id: &str,
) -> ExportResult<(String, ExportRequest, ExportStatus, String)> {
    conn.query_row("SELECT owner,request,state,logical_rows,physical_rows,size_bytes,expires_at,error_code,manifest FROM exports WHERE id=?1",params![id],|row| {
        Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,u64>(3)?,row.get::<_,u64>(4)?,row.get::<_,u64>(5)?,row.get::<_,String>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,String>(8)?))
    }).optional().map_err(|_| "EXPORT_IO".to_string())?.ok_or_else(|| "EXPORT_NOT_FOUND".to_string())
    .and_then(|(owner,request,state,logical_rows,physical_rows,size_bytes,expires_at,error_code,manifest)| {
        let request:ExportRequest=serde_json::from_str(&request).map_err(|_| "EXPORT_IO".to_string())?;
        let status=ExportStatus {export_id:id.into(),state,dataset:request.dataset,format:request.format,logical_rows,physical_rows,size_bytes,expires_at,error_code};
        Ok((owner,request,status,manifest))
    })
}
pub(super) fn audit(
    conn: &Connection,
    id: &str,
    actor: &str,
    event: &str,
    request: &ExportRequest,
    rows: u64,
) -> ExportResult<()> {
    let filters = serde_json::to_string(request).map_err(|_| "EXPORT_IO".to_string())?;
    conn.execute("INSERT INTO export_audit(export_id,actor,event,dataset,filters,logical_rows,timestamp) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![id,actor,event,request.dataset.name(),filters,rows,Utc::now().to_rfc3339()]).map_err(|_| "EXPORT_IO".to_string())?;
    Ok(())
}
pub(super) fn checkpoint(home: &Path, id: &str) -> ExportResult<()> {
    let conn = db(home)?;
    let (_, _, status, _) = status(&conn, id)?;
    if status.state != "running" {
        return Err("EXPORT_CANCELLED".into());
    }
    if is_expired(&status.expires_at)? {
        return Err("EXPORT_EXPIRED".into());
    }
    Ok(())
}
pub(super) fn fail(home: &Path, id: &str, error: &str) {
    if let Ok(conn) = db(home) {
        let state = if error == "EXPORT_CANCELLED" {
            "cancelled"
        } else {
            "failed"
        };
        let _=conn.execute("UPDATE exports SET state=?2,error_code=?3,manifest='[]' WHERE id=?1 AND state IN ('queued','running')",params![id,state,error]);
        if let Ok((owner, request, status, _)) = status(&conn, id) {
            let _ = audit(&conn, id, &owner, "failed", &request, status.logical_rows);
        }
    }
    let _ = remove_outputs(home, id);
}
pub(super) fn remove_outputs(home: &Path, id: &str) -> ExportResult<()> {
    if !valid_id(id) {
        return Err("EXPORT_NOT_FOUND".into());
    }
    let dir = private_directory(&home.join("data_exports"))?.join(id);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("EXPORT_IO".into()),
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err("EXPORT_IO".into());
        }
        Ok(_) => {}
    }
    for name in ["result.partial", "result.csv", "result.xlsx"] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("EXPORT_IO".into()),
        }
    }
    match std::fs::remove_dir(&dir) {
        Ok(()) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(_) => return Err("EXPORT_IO".into()),
    }
    Ok(())
}
