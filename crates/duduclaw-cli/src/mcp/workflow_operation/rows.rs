use super::*;

pub(super) fn fresh(expiry: &str) -> Result<(), String> {
    if chrono::DateTime::parse_from_rfc3339(expiry).map_err(|_| "invalid workflow expiry")?
        <= Utc::now()
    {
        return Err("workflow authority expired".into());
    }
    Ok(())
}
/// Read-only opening refuses missing DB/symlink/unknown schema; prepare never creates state.
pub(super) fn read_row<T: DeserializeOwned>(
    home: &Path,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<T, String> {
    read_optional_row(home, sql, args)?.ok_or("workflow row unavailable".into())
}
pub(super) fn read_optional_row<T: DeserializeOwned>(
    home: &Path,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Option<T>, String> {
    let path = home.join("workflow.db");
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("workflow database symlink refused".into());
        }
    }
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|e| e.to_string())?;
    let version: i64 = conn
        .query_row(
            "SELECT version FROM workflow_schema_meta WHERE owner='foundation'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if version != 1 {
        return Err("unknown workflow database version".into());
    }
    let raw: Option<String> = conn
        .query_row(sql, args, |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    let Some(raw) = raw else { return Ok(None) };
    if raw.len() > MAX_FRAME_BYTES {
        return Err("oversize workflow row".into());
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| "invalid typed workflow row".into())
}
