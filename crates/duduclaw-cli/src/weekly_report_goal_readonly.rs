//! Strictly read-only, single-snapshot goal history reader.
//!
//! The source is never opened by SQLite. A bounded private physical copy is
//! accepted only when source identity, bytes and unfinished-journal checks
//! match before and after copying. SQLite reads that immutable private copy.
//! A nonempty WAL/journal is unavailable; this does not recover a live store.

use super::goal_survival::{self, GoalSurvival};
use chrono::{DateTime, Utc};
use duduclaw_gateway::task_store::{TaskIterationRow, TaskRow};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const REPORT_DEADLINE: Duration = Duration::from_secs(2);
const MAX_TASKS: i64 = 5_000;
const MAX_ITERATIONS: usize = 2_000;
const MAX_FIELD_BYTES: usize = 64 * 1024;
const MAX_RAW_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOGICAL_ROUND: i64 = 1_000;
const MAX_TABLE_ROWS: usize = 2_000;

struct SqliteDeadline {
    handle: *mut rusqlite::ffi::sqlite3,
    // SQLite borrows this allocation only until the handler is removed.
    _deadline: Box<Instant>,
}

impl SqliteDeadline {
    fn install(connection: &Connection, deadline: Instant) -> Self {
        extern "C" fn progress(context: *mut std::ffi::c_void) -> std::ffi::c_int {
            // SAFETY: install supplies a boxed Instant; Drop removes the
            // callback before the allocation or connection is released.
            let deadline = unsafe { &*context.cast::<Instant>() };
            i32::from(Instant::now() >= *deadline)
        }
        let mut deadline = Box::new(deadline);
        // SAFETY: this private guard remains on this thread, outlives the
        // read transaction and is dropped before its owning connection.
        let handle = unsafe { connection.handle() };
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                handle,
                1000,
                Some(progress),
                (&mut *deadline as *mut Instant).cast(),
            );
        }
        Self {
            handle,
            _deadline: deadline,
        }
    }
}

impl Drop for SqliteDeadline {
    fn drop(&mut self) {
        // SAFETY: collect drops its transaction, then this guard, then the
        // connection. No callback can refer to the box after removal.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(self.handle, 0, None, std::ptr::null_mut());
        }
    }
}

fn bounded_column(expression: &str) -> String {
    format!("CASE WHEN length(CAST({expression} AS BLOB)) <= {MAX_FIELD_BYTES} THEN {expression} ELSE NULL END")
}

fn raw_length(expression: &str) -> String {
    format!("COALESCE(length(CAST({expression} AS BLOB)),0)")
}

fn count_raw(total: &mut usize, sizes: &[i64]) -> Result<(), String> {
    for size in sizes {
        let size = usize::try_from(*size)
            .map_err(|_| "goal history unavailable: invalid field length".to_owned())?;
        if size > MAX_FIELD_BYTES {
            return Err("goal history unavailable: field byte limit exceeded".into());
        }
        *total = total
            .checked_add(size)
            .ok_or_else(|| "goal history unavailable: raw byte limit exceeded".to_owned())?;
    }
    if *total > MAX_RAW_BYTES {
        return Err("goal history unavailable: raw byte limit exceeded".into());
    }
    Ok(())
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        Err("goal history unavailable: total report deadline exceeded".into())
    } else {
        Ok(())
    }
}

fn same_identity(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.len() == b.len()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len()
            && a.modified().ok() == b.modified().ok()
            && a.created().ok() == b.created().ok()
    }
}

fn check_journals(home: &Path) -> Result<(), String> {
    for name in ["tasks.db-wal", "tasks.db-journal"] {
        match std::fs::symlink_metadata(home.join(name)) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err("goal history unavailable: unsafe SQLite journal".into()),
            Ok(meta) if meta.len()>0 => return Err("goal history unavailable: unfinished WAL/journal; checkpoint with the normal service before reporting".into()),
            Ok(_) => {},
            Err(error) if error.kind()==std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("goal history unavailable: {error}")),
        }
    }
    Ok(())
}

fn digest_copy(
    source: &mut File,
    mut target: Option<&mut File>,
    deadline: Instant,
) -> Result<([u8; 32], u64), String> {
    source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_deadline(deadline)?;
        let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > MAX_SOURCE_BYTES {
            return Err("goal history unavailable: source byte limit exceeded".into());
        }
        digest.update(&buffer[..count]);
        if let Some(writer) = target.as_mut() {
            writer
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok((digest.finalize().into(), bytes))
}

fn private_snapshot(
    home: &Path,
    deadline: Instant,
) -> Result<(tempfile::TempDir, std::path::PathBuf), String> {
    check_deadline(deadline)?;
    check_journals(home)?;
    let source_path = home.join("tasks.db");
    let baseline = std::fs::symlink_metadata(&source_path)
        .map_err(|e| format!("goal history unavailable: {e}"))?;
    if !baseline.is_file() || baseline.file_type().is_symlink() {
        return Err("goal history unavailable: database must be a regular file".into());
    }
    if baseline.len() > MAX_SOURCE_BYTES {
        return Err("goal history unavailable: source byte limit exceeded".into());
    }
    let canonical = std::fs::canonicalize(&source_path).map_err(|e| e.to_string())?;
    let mut source_options = OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        source_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut source = source_options.open(&canonical).map_err(|e| e.to_string())?;
    if !same_identity(&baseline, &source.metadata().map_err(|e| e.to_string())?) {
        return Err("goal history unavailable: source identity changed".into());
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("dudu-survival-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let directory = builder.tempdir().map_err(|e| e.to_string())?;
    let snapshot = directory.path().join("snapshot.db");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut target = options.open(&snapshot).map_err(|e| e.to_string())?;
    let before = digest_copy(&mut source, None, deadline)?;
    let copied = digest_copy(&mut source, Some(&mut target), deadline)?;
    let after = digest_copy(&mut source, None, deadline)?;
    check_journals(home)?;
    let final_path = std::fs::symlink_metadata(&source_path).map_err(|e| e.to_string())?;
    if !same_identity(&baseline, &final_path)
        || !same_identity(&baseline, &source.metadata().map_err(|e| e.to_string())?)
        || before != copied
        || before != after
        || before.1 != baseline.len()
    {
        return Err(
            "goal history unavailable: source changed while taking private snapshot".into(),
        );
    }
    check_deadline(deadline)?;
    target.flush().map_err(|e| e.to_string())?;
    drop(target);
    Ok((
        directory,
        std::fs::canonicalize(snapshot).map_err(|e| e.to_string())?,
    ))
}

fn columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>, String> {
    let mut query = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = query
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

fn optional(cols: &BTreeSet<String>, name: &str, fallback: &str) -> String {
    if cols.contains(name) {
        name.to_owned()
    } else {
        format!("{fallback} AS {name}")
    }
}

pub(crate) fn collect(
    home: &Path,
    start: &DateTime<Utc>,
    end: &DateTime<Utc>,
    agent: Option<&str>,
) -> Result<GoalSurvival, String> {
    let deadline = Instant::now() + REPORT_DEADLINE;
    let (_snapshot_guard, database) = private_snapshot(home, deadline)?;
    let mut uri = url::Url::from_file_path(&database)
        .map_err(|_| "goal history unavailable: unsupported snapshot path".to_owned())?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    let mut conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| format!("goal history unavailable: {e}"))?;
    let _sqlite_deadline = SqliteDeadline::install(&conn, deadline);
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA query_only=ON;")
        .map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let task_cols = columns(&tx, "tasks")?;
    for required in ["id", "status", "created_at", "goal_mode", "assigned_to"] {
        if !task_cols.contains(required) {
            return Err(format!(
                "goal history unavailable: tasks.{required} is missing"
            ));
        }
    }
    let iter_cols = columns(&tx, "task_iterations")?;
    for required in ["id", "task_id", "round", "verdict"] {
        if !iter_cols.contains(required) {
            return Err(format!(
                "goal history unavailable: task_iterations.{required} is missing"
            ));
        }
    }
    let task_sql = format!("SELECT {},{},{},{},{},{},{},{},{},{} FROM tasks WHERE goal_mode=1 AND (?1 IS NULL OR assigned_to=?1) AND (julianday(created_at) BETWEEN julianday(?2) AND julianday(?3) OR julianday(created_at) IS NULL){} ORDER BY id LIMIT {}",
        bounded_column("id"),bounded_column("status"),bounded_column("created_at"),bounded_column("assigned_to"),
        optional(&task_cols,"max_retries","-1"),if task_cols.contains("team_spec_json") {"CASE WHEN team_spec_json IS NOT NULL THEN 'frozen' ELSE NULL END"} else {"NULL"},
        raw_length("id"),raw_length("status"),raw_length("created_at"),raw_length("assigned_to"),
        if task_cols.contains("kind") { " AND kind IN ('task','goal')" } else { "" },MAX_TASKS+1);
    let mut task_query = tx.prepare(&task_sql).map_err(|e| e.to_string())?;
    let mut raw_bytes = 0;
    let mut task_rows = task_query
        .query(rusqlite::params![
            agent,
            start.to_rfc3339(),
            end.to_rfc3339()
        ])
        .map_err(|e| e.to_string())?;
    let mut tasks = Vec::new();
    while let Some(r) = task_rows.next().map_err(|e| e.to_string())? {
        check_deadline(deadline)?;
        if tasks.len() >= MAX_TASKS as usize {
            return Err("goal history unavailable: task limit exceeded".into());
        }
        count_raw(
            &mut raw_bytes,
            &[
                r.get::<_, i64>(6).map_err(|e| e.to_string())?,
                r.get(7).map_err(|e| e.to_string())?,
                r.get(8).map_err(|e| e.to_string())?,
                r.get(9).map_err(|e| e.to_string())?,
            ],
        )?;
        let id: String = r.get(0).map_err(|e| e.to_string())?;
        let mut task = TaskRow::new(
            id,
            String::new(),
            String::new(),
            String::new(),
            r.get(3).map_err(|e| e.to_string())?,
            String::new(),
        );
        task.status = r.get(1).map_err(|e| e.to_string())?;
        task.created_at = r.get(2).map_err(|e| e.to_string())?;
        task.max_retries = r.get(4).map_err(|e| e.to_string())?;
        task.team_spec_json = r.get(5).map_err(|e| e.to_string())?;
        tasks.push(task);
    }
    drop(task_rows);
    let field = |name: &str| {
        if iter_cols.contains(name) {
            name.to_owned()
        } else {
            "NULL".into()
        }
    };
    let iter_sql = format!(
        "SELECT id,round,{},{},{},{},{},{},{},{},{} FROM task_iterations WHERE task_id=?1 ORDER BY id LIMIT {}",
        bounded_column("verdict"),bounded_column(&field("team_mode")),bounded_column(&field("knobs_json")),bounded_column(&field("gate_inputs_json")),
        raw_length("verdict"),raw_length(&field("team_mode")),raw_length(&field("knobs_json")),raw_length(&field("gate_inputs_json")),optional(&iter_cols,"dispatch_count","NULL"),MAX_ITERATIONS+1
    );
    let mut iter_query = tx.prepare(&iter_sql).map_err(|e| e.to_string())?;
    let evidence_cols = columns(&tx, "task_survival_evidence")?;
    let has_evidence = [
        "task_id",
        "difficulty",
        "manual_retry",
        "human_approved",
        "evidence_version",
    ]
    .iter()
    .all(|c| evidence_cols.contains(*c));
    if !evidence_cols.is_empty() && !has_evidence {
        return Err("goal history unavailable: incomplete survival evidence schema".into());
    }
    let mut facts = Vec::new();
    let mut running = 0;
    let mut total_iterations = 0usize;
    for task in tasks {
        check_deadline(deadline)?;
        let created = DateTime::parse_from_rfc3339(&task.created_at)
            .map_err(|_| "goal history unavailable: invalid task timestamp".to_string())?
            .with_timezone(&Utc);
        if created < *start || created > *end {
            continue;
        }
        if !["done", "needs_human", "cancelled", "failed"].contains(&task.status.as_str()) {
            running += 1;
            continue;
        }
        let mut iteration_rows = iter_query.query([&task.id]).map_err(|e| e.to_string())?;
        let mut iterations = Vec::new();
        while let Some(r) = iteration_rows.next().map_err(|e| e.to_string())? {
            check_deadline(deadline)?;
            total_iterations += 1;
            if total_iterations > MAX_ITERATIONS {
                return Err("goal history unavailable: iteration limit exceeded".into());
            }
            count_raw(
                &mut raw_bytes,
                &[
                    r.get::<_, i64>(6).map_err(|e| e.to_string())?,
                    r.get(7).map_err(|e| e.to_string())?,
                    r.get(8).map_err(|e| e.to_string())?,
                    r.get(9).map_err(|e| e.to_string())?,
                ],
            )?;
            let round: i64 = r.get(1).map_err(|e| e.to_string())?;
            if !(1..=MAX_LOGICAL_ROUND).contains(&round) {
                return Err("goal history unavailable: logical round limit exceeded".into());
            }
            iterations.push(TaskIterationRow {
                id: r.get(0).map_err(|e| e.to_string())?,
                task_id: task.id.clone(),
                round,
                verdict: r.get(2).map_err(|e| e.to_string())?,
                team_mode: r.get(3).map_err(|e| e.to_string())?,
                knobs_json: r.get(4).map_err(|e| e.to_string())?,
                gate_inputs_json: r.get(5).map_err(|e| e.to_string())?,
                dispatched_at: String::new(),
                submitted_at: None,
                judged_at: None,
                judge_feedback: None,
                feedback_class: None,
                verdict_json: None,
                dispatch_count: r
                    .get::<_, Option<i64>>(10)
                    .map_err(|e| e.to_string())?
                    .unwrap_or(0),
                state_hash: None,
                repeat_streak: None,
                worker_excerpt: None,
                evaluator_verdict: None,
                iter_seq: None,
                state_block_json: None,
                pause_reason: None,
            });
        }
        drop(iteration_rows);
        let mut fact = goal_survival::facts_from(&task, &iterations);
        if has_evidence {
            use rusqlite::OptionalExtension;
            let evidence_sql=format!("SELECT {},manual_retry,human_approved,evidence_version,{},{},{} FROM task_survival_evidence WHERE task_id=?1",bounded_column("difficulty"),optional(&evidence_cols,"human_approved_iteration_id","NULL"),raw_length("difficulty"),optional(&evidence_cols,"difficulty_dispatches","NULL"));
            let evidence: Option<(
                Option<String>,
                Option<i64>,
                Option<i64>,
                i64,
                Option<i64>,
                i64,
                Option<i64>,
            )> = tx
                .query_row(&evidence_sql, [&task.id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                })
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some((
                difficulty,
                manual,
                approved,
                1,
                approved_id,
                field_bytes,
                difficulty_dispatches,
            )) = evidence
            {
                count_raw(&mut raw_bytes, &[field_bytes])?;
                let bool_value = |value| match value {
                    Some(0) => Ok(Some(false)),
                    Some(1) => Ok(Some(true)),
                    None => Ok(None),
                    _ => Err(
                        "goal history unavailable: invalid survival evidence boolean".to_string(),
                    ),
                };
                fact.manual_retry = bool_value(manual)?;
                fact.human_approved = bool_value(approved)?;
                if fact.human_approved == Some(true) {
                    if let Some(id) = approved_id {
                        let round: Option<i64> = tx
                            .query_row(
                                "SELECT round FROM task_iterations WHERE id=?1 AND task_id=?2",
                                rusqlite::params![id, &task.id],
                                |r| r.get(0),
                            )
                            .optional()
                            .map_err(|e| e.to_string())?;
                        fact.human_approved_round = round
                            .and_then(|round| u32::try_from(round).ok())
                            .filter(|round| *round > 0 && *round <= MAX_LOGICAL_ROUND as u32);
                        if fact.human_approved_round.is_none() {
                            return Err("goal history unavailable: approval receipt has an invalid iteration binding".into());
                        }
                    }
                }
                let recorded_dispatches = iterations.iter().try_fold(0i64, |sum, iteration| {
                    if iteration.dispatch_count < 1 {
                        None
                    } else {
                        sum.checked_add(iteration.dispatch_count)
                    }
                });
                let complete = difficulty_dispatches
                    .is_some_and(|count| count > 0 && Some(count) == recorded_dispatches);
                fact.difficulty = if complete {
                    match difficulty.as_deref() {
                        Some(value @ ("simple" | "complex" | "mixed")) => value.to_owned(),
                        None => "unknown".into(),
                        _ => {
                            return Err("goal history unavailable: invalid frozen difficulty".into())
                        }
                    }
                } else {
                    "unknown".into()
                };
            } else {
                fact.difficulty = "unknown".into();
            }
        } else {
            fact.difficulty = "unknown".into();
        }
        facts.push(fact);
    }
    drop(iter_query);
    drop(task_query);
    tx.commit().map_err(|e| e.to_string())?;
    check_deadline(deadline)?;
    // Bound the eventual table allocation before the pure renderer expands
    // one row per logical round, including the stronger approval subset.
    let mut table_sizes = std::collections::BTreeMap::new();
    for fact in &facts {
        let key = (
            fact.max_retries,
            fact.mode,
            fact.difficulty.clone(),
            fact.manual_retry,
        );
        let judged = fact.judged_rounds.iter().copied().max().unwrap_or(0);
        if fact.status != "done" || fact.accepted_round.is_some() {
            let max = judged.max(fact.accepted_round.unwrap_or(0));
            table_sizes
                .entry((false, key.clone()))
                .and_modify(|old: &mut u32| *old = (*old).max(max))
                .or_insert(max);
        }
        if fact.human_approved == Some(true) && fact.human_approved_round.is_some() {
            let max = judged.max(fact.human_approved_round.unwrap_or(0));
            table_sizes
                .entry((true, key))
                .and_modify(|old: &mut u32| *old = (*old).max(max))
                .or_insert(max);
        }
    }
    if table_sizes
        .values()
        .map(|value| *value as usize)
        .sum::<usize>()
        > MAX_TABLE_ROWS
    {
        return Err("goal history unavailable: table row limit exceeded".into());
    }
    check_deadline(deadline)?;
    let report = goal_survival::build(facts, running);
    check_deadline(deadline)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn survival_sql_deadline_interrupts_real_scan_and_drop_restores_connection() {
        let connection = Connection::open_in_memory().unwrap();
        {
            let _guard =
                SqliteDeadline::install(&connection, Instant::now() - Duration::from_secs(1));
            let error=connection.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000000) SELECT sum(x) FROM n",[],|row|row.get::<_,i64>(0)).unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::OperationInterrupted)
            );
        }
        assert_eq!(
            connection
                .query_row("SELECT 42", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            42
        );
    }
}
