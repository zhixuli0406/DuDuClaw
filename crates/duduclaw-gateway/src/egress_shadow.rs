//! Content-free, cross-process shadow counts independent of rotating tool logs.
use std::path::{Path, PathBuf};
use rusqlite::{params, Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;

#[derive(Clone, Copy)]
pub enum Transport { Hook, Mcp }
impl Transport {
    fn name(self) -> &'static str { match self { Self::Hook => "hook", Self::Mcp => "mcp" } }
}

/// Only closed classes reach the database; tool/source arguments never become
/// free-form source text. Multiple classes count once as `mixed`.
pub fn source_class(sources: &[String]) -> &'static str {
    let classes: std::collections::BTreeSet<_> = sources.iter().map(|s| {
        match s.as_str() {
            "turn_state_unavailable" => "unknown",
            "pending_private_read" => "pending",
            "additional_private_sources" => "mixed",
            s if s.starts_with("memory_") || s.starts_with("wiki_") || s == "user_profile_get" => "memory",
            s if s.starts_with("gmail_") || s.starts_with("mail_") => "mail",
            s if s.starts_with("db_") => "database",
            s if s.starts_with("drive_") || s.starts_with("docs_") || s.starts_with("sheets_")
                || s.starts_with("slides_") || s.starts_with("calendar_") || s.starts_with("gtasks_")
                || s.starts_with("forms_") => "google",
            s if s.starts_with("computer_") => "computer",
            s if s.contains('.') => "external_tool",
            _ => "other",
        }
    }).collect();
    if classes.len() > 1 { "mixed" } else { classes.first().copied().unwrap_or("unknown") }
}

fn private_files(path: &Path, create: bool) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let path = PathBuf::from(name);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        if suffix.is_empty() && create { options.create(true).truncate(false); }
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !suffix.is_empty() => continue,
            Err(e) => return Err(e.to_string()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() { return Err("shadow store is not a regular file".into()); }
        #[cfg(unix)] {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if meta.nlink() != 1 { return Err("shadow store hard link refused".into()); }
            file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

struct Store(Connection);
impl Store {
    fn open(home: &Path) -> Result<Self, String> {
        let path = home.join("egress_shadow.db");
        private_files(&path, true)?;
        let conn = Connection::open_with_flags(&path, OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW)
            .map_err(|e| e.to_string())?;
        conn.busy_timeout(std::time::Duration::from_secs(1)).map_err(|e| e.to_string())?;
        conn.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS shadow_events (
                day TEXT NOT NULL, agent_id TEXT NOT NULL, tool TEXT NOT NULL,
                kind TEXT NOT NULL, source_class TEXT NOT NULL,
                call_id TEXT NOT NULL UNIQUE, at INTEGER NOT NULL,
                transport TEXT NOT NULL CHECK(transport IN ('hook','mcp')));
            CREATE INDEX IF NOT EXISTS shadow_events_at ON shadow_events(at);
            CREATE TABLE IF NOT EXISTS shadow_maintenance (id INTEGER PRIMARY KEY CHECK(id=1), pruned_at INTEGER NOT NULL);")
            .map_err(|e| e.to_string())?;
        private_files(&path, false)?;
        Ok(Self(conn))
    }

    fn write(&mut self, agent: &str, tool: &str, kind: &str, class: &str, call: &str, transport: Transport, at: i64) -> Result<bool, String> {
        if !(kind == "would_ask" || kind == "recipient_would_ask" || kind.starts_with("would_refuse_")) {
            return Err("invalid shadow event kind".into());
        }
        let tx = self.0.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let inserted = tx.execute("INSERT OR IGNORE INTO shadow_events(day,agent_id,tool,kind,source_class,call_id,at,transport)
            VALUES(date(?1,'unixepoch'),?2,?3,?4,?5,?6,?1,?7)", params![at,agent,tool,kind,class,call,transport.name()]).map_err(|e| e.to_string())?;
        let prune = tx.execute("INSERT INTO shadow_maintenance VALUES(1,?1)
            ON CONFLICT(id) DO UPDATE SET pruned_at=excluded.pruned_at WHERE pruned_at <= ?1-3600", [at]).map_err(|e| e.to_string())?;
        if prune != 0 {
            tx.execute("DELETE FROM shadow_events WHERE at < ?1-35*86400", [at]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(inserted != 0)
    }
}

/// Best effort only. A failed telemetry write cannot change a gate decision.
/// Returns true only for the first row, so preflight and dispatch also share
/// one tool-log row instead of counting the same would-be ticket twice.
pub fn record(home: &Path, agent: &str, tool: &str, kind: &str, sources: &[String], call_id: &str, transport: Transport) -> bool {
    match Store::open(home).and_then(|mut s| s.write(agent, tool, kind, source_class(sources), call_id, transport, chrono::Utc::now().timestamp())) {
        Ok(inserted) => inserted,
        Err(error) => { tracing::warn!(%error, "egress shadow count unavailable"); false }
    }
}

#[derive(Serialize, Debug)]
pub struct Count { pub key: String, pub count: u64 }
#[derive(Serialize, Debug)]
pub struct Report {
    pub days: u32, pub total: u64, pub ticket_total: u64,
    pub by_employee: Vec<Count>, pub by_tool: Vec<Count>, pub by_source_class: Vec<Count>,
    pub by_transport: Vec<Count>, pub daily: Vec<Count>,
}

pub fn report(home: &Path, days: u32) -> Result<Report, String> {
    report_at(home, days, chrono::Utc::now().timestamp())
}
fn report_at(home: &Path, days: u32, now: i64) -> Result<Report, String> {
    if !matches!(days, 7 | 30) { return Err("days must be 7 or 30".into()); }
    let mut result = Report { days, total: 0, ticket_total: 0, by_employee: vec![], by_tool: vec![],
        by_source_class: vec![], by_transport: vec![], daily: vec![] };
    // Include today and the preceding UTC calendar days, zero-filled below.
    let today = now.div_euclid(86400) * 86400;
    let start = today - i64::from(days - 1) * 86400;
    for offset in 0..days {
        let at = start + i64::from(offset)*86400;
        let date = chrono::DateTime::from_timestamp(at, 0).ok_or("invalid report time")?.format("%Y-%m-%d").to_string();
        result.daily.push(Count { key: date, count: 0 });
    }
    match std::fs::symlink_metadata(home.join("egress_shadow.db")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(result),
        Err(e) => return Err(e.to_string()),
        Ok(_) => (),
    }
    let mut store = Store::open(home)?;
    let tx = store.0.transaction().map_err(|e| e.to_string())?;
    (result.total, result.ticket_total) = tx.query_row("SELECT COUNT(*), COALESCE(SUM(kind IN ('would_ask','recipient_would_ask')),0)
        FROM shadow_events WHERE at>=?1 AND at<=?2", params![start,now], |r| Ok((r.get(0)?,r.get(1)?))).map_err(|e| e.to_string())?;
    for (column, output) in [("agent_id", &mut result.by_employee), ("tool", &mut result.by_tool),
        ("source_class", &mut result.by_source_class), ("transport", &mut result.by_transport)] {
        // Columns are a fixed host-owned list, never an RPC input.
        let mut stmt = tx.prepare(&format!("SELECT {column},COUNT(*) FROM shadow_events WHERE at>=?1 AND at<=?2 GROUP BY {column} ORDER BY COUNT(*) DESC,{column}")).map_err(|e| e.to_string())?;
        *output = stmt.query_map(params![start,now], |r| Ok(Count { key:r.get(0)?, count:r.get(1)? })).map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?;
    }
    let mut stmt = tx.prepare("SELECT day,COUNT(*) FROM shadow_events WHERE at>=?1 AND at<=?2 GROUP BY day").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![start,now], |r| Ok(Count { key:r.get(0)?, count:r.get(1)? })).map_err(|e| e.to_string())?;
    for row in rows {
        let row = row.map_err(|e| e.to_string())?;
        if let Some(day) = result.daily.iter_mut().find(|d| d.key == row.key) { day.count = row.count; }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000;

    #[test]
    fn aggregation_counts_calls_and_separates_tickets_from_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(home.path()).unwrap();
        for (id,agent,tool,kind,class,transport) in [
            ("a","bot","send_message","would_ask","memory",Transport::Mcp),
            ("b","bot","WebFetch","would_ask","google",Transport::Hook),
            ("c","other","Bash","would_refuse_ledger_unavailable","unknown",Transport::Hook),
            ("d","other","mail_send","recipient_would_ask","mail",Transport::Mcp),
        ] { assert!(store.write(agent,tool,kind,class,id,transport,NOW).unwrap()); }
        let report = report_at(home.path(),7,NOW).unwrap();
        assert_eq!((report.total,report.ticket_total),(4,3));
        assert_eq!(report.daily.len(),7);
        for series in [&report.by_employee,&report.by_tool,&report.by_source_class,&report.by_transport,&report.daily] {
            assert_eq!(series.iter().map(|v| v.count).sum::<u64>(),4);
        }
        assert_eq!(report.by_transport.iter().find(|v| v.key=="hook").unwrap().count,2);
        assert!(report_at(home.path(),8,NOW).is_err());
    }

    #[test]
    fn retention_prunes_at_most_hourly_across_handles() {
        let home = tempfile::tempdir().unwrap();
        let mut a = Store::open(home.path()).unwrap();
        let mut b = Store::open(home.path()).unwrap();
        a.write("bot","send","would_ask","memory","old",Transport::Mcp,NOW-36*86400).unwrap();
        b.write("bot","send","would_ask","memory","new",Transport::Mcp,NOW).unwrap();
        assert_eq!(a.0.query_row("SELECT COUNT(*) FROM shadow_events",[],|r|r.get::<_,u64>(0)).unwrap(),1);
        // A backdated event is retained until the next hourly maintenance pass.
        a.write("bot","send","would_ask","memory","late",Transport::Mcp,NOW-36*86400).unwrap();
        b.write("bot","send","would_ask","memory","same-hour",Transport::Mcp,NOW+3599).unwrap();
        assert_eq!(a.0.query_row("SELECT COUNT(*) FROM shadow_events WHERE call_id='late'",[],|r|r.get::<_,u64>(0)).unwrap(),1);
        b.write("bot","send","would_ask","memory","next-hour",Transport::Mcp,NOW+3600).unwrap();
        assert_eq!(a.0.query_row("SELECT COUNT(*) FROM shadow_events WHERE call_id='late'",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    }

    #[test]
    fn concurrent_handles_deduplicate_shared_call_ids() {
        let home = tempfile::tempdir().unwrap();
        Store::open(home.path()).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = (0..2).map(|_| {
            let path = home.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                store.write("bot","send","would_ask","memory","same",Transport::Mcp,NOW).unwrap()
            })
        }).collect();
        let inserted = threads.into_iter().map(|t| usize::from(t.join().unwrap())).sum::<usize>();
        assert_eq!(inserted,1);
        let store = Store::open(home.path()).unwrap();
        assert_eq!(store.0.query_row("SELECT COUNT(*) FROM shadow_events",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    }

    #[test]
    fn source_classes_never_persist_source_text_and_missing_store_is_empty() {
        assert_eq!(source_class(&["secret-server.private-document".into()]),"external_tool");
        assert_eq!(source_class(&["memory_search".into(),"gmail_read".into()]),"mixed");
        let home = tempfile::tempdir().unwrap();
        let report = report_at(home.path(),30,NOW).unwrap();
        assert_eq!(report.total,0);
        assert_eq!(report.daily.len(),30);
        assert!(!home.path().join("egress_shadow.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn shadow_store_refuses_links_and_keeps_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("target");
        std::fs::write(&target,"keep").unwrap();
        std::os::unix::fs::symlink(&target,home.path().join("egress_shadow.db")).unwrap();
        assert!(Store::open(home.path()).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(),"keep");
        let clean = tempfile::tempdir().unwrap();
        let _store = Store::open(clean.path()).unwrap();
        for suffix in ["","-wal","-shm"] {
            let path = clean.path().join(format!("egress_shadow.db{suffix}"));
            assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777,0o600);
        }
    }
}
