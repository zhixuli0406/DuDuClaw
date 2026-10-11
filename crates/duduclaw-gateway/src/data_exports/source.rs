//! Full store scans in stable rowid batches under a read transaction.
//! Existing UI history/recent limits are intentionally not used.
use super::writer::{Cell, LogicalRow};
use super::{Dataset, ExportRequest, ExportResult, MAX_BYTES, MAX_ROWS, fresh};
use duduclaw_auth::{UserContext, models::AccessLevel};
use rusqlite::{params, types::ValueRef};
use std::path::Path;

pub struct Table {
    pub headers: Vec<&'static str>,
    pub rows: Vec<LogicalRow>,
}
fn text(cell: &Cell) -> &str {
    if let Cell::Text(s) = cell { s } else { "" }
}
fn source_time(stamp: &str) -> ExportResult<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .map(|t| t.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(stamp, "%Y-%m-%d %H:%M:%S").map(|t| t.and_utc())
        })
        .map_err(|_| "EXPORT_INVALID_SOURCE_TIME".to_string())
}
pub(super) fn in_range(request: &ExportRequest, stamp: &str) -> ExportResult<bool> {
    let time = source_time(stamp)?;
    Ok(request.from.as_ref().is_none_or(|v| time >= *v)
        && request.until.as_ref().is_none_or(|v| time < *v))
}
fn normalize_time(cell: &mut Cell) -> ExportResult<()> {
    if let Cell::Text(stamp) = cell {
        if !stamp.is_empty() {
            *stamp = source_time(stamp)?.to_rfc3339();
        }
    }
    Ok(())
}
pub(super) fn push(table: &mut Table, row: LogicalRow, bytes: &mut usize) -> ExportResult<()> {
    *bytes += row.id.len()
        + row
            .cells
            .iter()
            .map(|c| match c {
                Cell::Text(s) => s.len(),
                Cell::Number(_) => 8,
            })
            .sum::<usize>();
    if table.rows.len() >= MAX_ROWS || *bytes as u64 > MAX_BYTES {
        return Err("EXPORT_TOO_LARGE".into());
    }
    table.rows.push(row);
    Ok(())
}
fn scan(
    path: &Path,
    query: &str,
    agent_filter: Option<&str>,
    mut visit: impl FnMut(i64, Vec<Cell>) -> ExportResult<()>,
    mut checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<()> {
    if !path.exists() {
        return Ok(());
    }
    let conn = crate::review_evidence::audience::readonly_db(path)
        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
    conn.execute_batch("BEGIN DEFERRED")
        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
    let mut after = 0;
    loop {
        checkpoint()?;
        let mut stmt = conn
            .prepare(query)
            .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
        let count = stmt.column_count();
        let mut rows = stmt
            .query(params![after, 512, agent_filter])
            .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
        let mut n = 0;
        while let Some(row) = rows
            .next()
            .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?
        {
            let id: i64 = row
                .get(0)
                .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
            after = id;
            n += 1;
            let mut cells = Vec::new();
            let mut row_bytes = 0u64;
            for i in 1..count {
                cells.push(
                    match row
                        .get_ref(i)
                        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?
                    {
                        ValueRef::Text(t) => {
                            row_bytes = row_bytes.saturating_add(t.len() as u64);
                            if row_bytes > MAX_BYTES {
                                return Err("EXPORT_TOO_LARGE".into());
                            }
                            Cell::Text(
                                String::from_utf8(t.to_vec())
                                    .map_err(|_| "EXPORT_INVALID_UTF8".to_string())?,
                            )
                        }
                        ValueRef::Integer(v) => Cell::Number(v),
                        ValueRef::Real(v) => Cell::Text(v.to_string()),
                        ValueRef::Null => Cell::Text(String::new()),
                        ValueRef::Blob(_) => return Err("EXPORT_SOURCE_UNAVAILABLE".into()),
                    },
                );
            }
            visit(id, cells)?;
        }
        if n < 512 {
            break;
        }
    }
    conn.execute_batch("COMMIT")
        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
    Ok(())
}
fn selected(request: &ExportRequest, agent: &str) -> bool {
    request.agent_id.as_ref().is_none_or(|id| id == agent)
}
fn agent_visible(home: &Path, original: &UserContext, agent: &str) -> ExportResult<bool> {
    let live = fresh(home, original)?;
    Ok(live.has_agent_access(agent, AccessLevel::Viewer))
}
pub fn collect(
    home: &Path,
    original: &UserContext,
    request: &ExportRequest,
    mut checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<Table> {
    let mut table = Table {
        headers: Vec::new(),
        rows: Vec::new(),
    };
    let mut bytes = 0;
    super::authorize(home, original, request)?;
    match request.dataset {
        Dataset::Tasks => {
            table.headers = vec![
                "task_id",
                "title",
                "status",
                "agent_id",
                "created_at_utc",
                "completed_at_utc",
            ];
            scan(
                &home.join("tasks.db"),
                "SELECT rowid,id,title,status,assigned_to,created_at,completed_at FROM tasks WHERE rowid>?1 AND (?3 IS NULL OR assigned_to=?3) ORDER BY rowid LIMIT ?2",
                request.agent_id.as_deref(),
                |_, mut cells| {
                    if !selected(request, text(&cells[3])) {
                        return Ok(());
                    }
                    let live = fresh(home, original)?;
                    if !crate::handlers::task_privacy::task_content_visible(
                        home,
                        &live,
                        text(&cells[0]),
                        text(&cells[3]),
                    ) {
                        return Ok(());
                    }
                    normalize_time(&mut cells[4])?;
                    normalize_time(&mut cells[5])?;
                    let id = text(&cells[0]);
                    let agent = text(&cells[3]);
                    if selected(request, agent) && in_range(request, text(&cells[4]))? {
                        let live = fresh(home, original)?;
                        if crate::handlers::task_privacy::task_content_visible(
                            home, &live, id, agent,
                        ) {
                            push(
                                &mut table,
                                LogicalRow {
                                    id: format!("task:{id}"),
                                    cells,
                                },
                                &mut bytes,
                            )?;
                        }
                    }
                    Ok(())
                },
                &mut checkpoint,
            )?;
        }
        Dataset::Conversations => {
            table.headers = vec![
                "message_id",
                "session_id",
                "agent_id",
                "channel",
                "role",
                "content",
                "timestamp_utc",
            ];
            scan(
                &home.join("sessions.db"),
                "SELECT m.id,m.session_id,s.agent_id,m.role,m.content,m.timestamp FROM session_messages m JOIN sessions s ON s.id=m.session_id WHERE m.id>?1 AND m.hidden=0 AND m.undone_at IS NULL AND (?3 IS NULL OR s.agent_id=?3) ORDER BY m.id LIMIT ?2",
                request.agent_id.as_deref(),
                |id, mut cells| {
                    if !selected(request, text(&cells[1]))
                        || !agent_visible(home, original, text(&cells[1]))?
                    {
                        return Ok(());
                    }
                    normalize_time(&mut cells[4])?;
                    let agent = text(&cells[1]);
                    if selected(request, agent)
                        && in_range(request, text(&cells[4]))?
                        && agent_visible(home, original, agent)?
                    {
                        let mut values = vec![
                            Cell::Number(id),
                            cells[0].clone(),
                            cells[1].clone(),
                            Cell::from("unknown"),
                        ];
                        values.extend_from_slice(&cells[2..]);
                        push(
                            &mut table,
                            LogicalRow {
                                id: format!("message:{id}"),
                                cells: values,
                            },
                            &mut bytes,
                        )?;
                    }
                    Ok(())
                },
                &mut checkpoint,
            )?;
        }
        Dataset::Usage => {
            table.headers = vec![
                "usage_id",
                "agent_id",
                "timestamp_utc",
                "provider",
                "model",
                "request_type",
                "input_tokens",
                "cache_read_tokens",
                "cache_creation_tokens",
                "output_tokens",
                "recorded_cost_millicents",
                "amount_usd",
                "currency",
                "measurement",
            ];
            scan(
                &home.join("cost_telemetry.db"),
                "SELECT id,agent_id,created_at,model,request_type,input_tokens,cache_read_tokens,cache_creation_tokens,output_tokens,cost_millicents FROM token_usage WHERE id>?1 AND (?3 IS NULL OR agent_id=?3) ORDER BY id LIMIT ?2",
                request.agent_id.as_deref(),
                |id, mut cells| {
                    if !selected(request, text(&cells[0])) {
                        return Ok(());
                    }
                    normalize_time(&mut cells[1])?;
                    // Fresh Admin role per batch/row; a downgraded creator cannot finish a cost export.
                    if !fresh(home, original)?.is_admin() {
                        return Err("EXPORT_PERMISSION_DENIED".into());
                    }
                    if selected(request, text(&cells[0])) && in_range(request, text(&cells[1]))? {
                        let has_tokens = cells[4..8]
                            .iter()
                            .any(|cell| matches!(cell, Cell::Number(n) if *n > 0));
                        let cost = if let Cell::Number(n) = cells[8] { n } else { 0 };
                        let mut values = vec![
                            Cell::Number(id),
                            cells[0].clone(),
                            cells[1].clone(),
                            Cell::from("unknown"),
                            if text(&cells[2]).is_empty() {
                                Cell::from("unknown")
                            } else {
                                cells[2].clone()
                            },
                            cells[3].clone(),
                        ];
                        values.extend(cells[4..8].iter().map(|c| {
                            if !has_tokens {
                                Cell::from("")
                            } else {
                                c.clone()
                            }
                        }));
                        values.push(cells[8].clone());
                        // Raw millicents remain truthful; zero/default costs cannot prove a measured free request.
                        values.push(if cost == 0 {
                            Cell::from("")
                        } else {
                            Cell::from(format!(
                                "{}.{:05}",
                                cost / 100_000,
                                cost.unsigned_abs() % 100_000
                            ))
                        });
                        values.extend([Cell::from("USD"), Cell::from("unknown")]);
                        push(
                            &mut table,
                            LogicalRow {
                                id: format!("usage:{id}"),
                                cells: values,
                            },
                            &mut bytes,
                        )?;
                    }
                    Ok(())
                },
                &mut checkpoint,
            )?;
        }
        Dataset::Memories => {
            table.headers = vec![
                "memory_id",
                "agent_id",
                "kind",
                "content",
                "source",
                "state",
                "timestamp_utc",
            ];
            let agent = request
                .agent_id
                .as_deref()
                .ok_or_else(|| "EXPORT_INVALID_REQUEST".to_string())?;
            let state = home.join("agents").join(agent).join("state/memory.db");
            let legacy = home.join("agents").join(agent).join("memory.db");
            let db = if state.exists() {
                state
            } else if legacy.exists() {
                legacy
            } else {
                home.join("memory.db")
            };
            scan(
                &db,
                "SELECT rowid,id,agent_id,layer,content,source_event,valid_until,timestamp,valid_from FROM memories WHERE rowid>?1 AND quarantined=0 AND (?3 IS NULL OR agent_id=?3) ORDER BY rowid LIMIT ?2",
                request.agent_id.as_deref(),
                |_, mut cells| {
                    if text(&cells[1]) != agent || !agent_visible(home, original, agent)? {
                        return Ok(());
                    }
                    normalize_time(&mut cells[6])?;
                    if text(&cells[1]) == agent
                        && in_range(request, text(&cells[6]))?
                        && agent_visible(home, original, agent)?
                    {
                        let id = format!("memory:{}", text(&cells[0]));
                        let now = chrono::Utc::now();
                        let state = if !text(&cells[5]).is_empty()
                            && source_time(text(&cells[5]))? <= now
                        {
                            "expired"
                        } else if !text(&cells[7]).is_empty() && source_time(text(&cells[7]))? > now
                        {
                            "scheduled"
                        } else {
                            "current"
                        };
                        cells[5] = Cell::from(state);
                        cells.pop();
                        push(&mut table, LogicalRow { id, cells }, &mut bytes)?;
                    }
                    Ok(())
                },
                &mut checkpoint,
            )?;
            scan(
                &db,
                "SELECT rowid,id,agent_id,fact,source_session,timestamp FROM key_facts WHERE rowid>?1 AND (?3 IS NULL OR agent_id=?3) ORDER BY rowid LIMIT ?2",
                request.agent_id.as_deref(),
                |_, mut cells| {
                    if text(&cells[1]) != agent || !agent_visible(home, original, agent)? {
                        return Ok(());
                    }
                    normalize_time(&mut cells[4])?;
                    if text(&cells[1]) == agent
                        && in_range(request, text(&cells[4]))?
                        && agent_visible(home, original, agent)?
                    {
                        let id = format!("key_fact:{}", text(&cells[0]));
                        let values = vec![
                            cells[0].clone(),
                            cells[1].clone(),
                            Cell::from("key_fact"),
                            cells[2].clone(),
                            cells[3].clone(),
                            Cell::from("current"),
                            cells[4].clone(),
                        ];
                        push(&mut table, LogicalRow { id, cells: values }, &mut bytes)?;
                    }
                    Ok(())
                },
                &mut checkpoint,
            )?;
        }
        Dataset::Wiki => super::wiki_source::collect(
            home,
            original,
            request,
            &mut table,
            &mut bytes,
            &mut checkpoint,
        )?,
    }
    Ok(table)
}
