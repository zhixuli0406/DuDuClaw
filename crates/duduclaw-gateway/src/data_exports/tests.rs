use super::writer::{Cell, LogicalRow};
use super::*;
use calamine::{Reader, Xlsx, open_workbook};
use duduclaw_auth::{
    UserDb,
    models::{AccessLevel, UserRole},
};
fn request(dataset: Dataset, format: ExportFormat) -> ExportRequest {
    ExportRequest {
        dataset,
        format,
        agent_id: Some("sales".into()),
        scope: None,
        from: None,
        until: None,
    }
}
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    initialize(dir.path()).unwrap();
    dir
}

#[test]
fn worker_lock_contention_preserves_owner_and_releases_on_drop() {
    let dir = fixture();
    let id = "0123456789abcdef0123456789abcdef";
    let lease = worker_lease::try_lease(dir.path(), id).unwrap().unwrap();
    assert!(worker_lease::try_lease(dir.path(), id).unwrap().is_none());
    assert!(
        worker_lease::try_existing_lease(dir.path(), id)
            .unwrap()
            .is_none()
    );
    drop(lease);
    assert!(
        worker_lease::try_existing_lease(dir.path(), id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn download_lock_contention_uses_both_slots_and_releases_on_drop() {
    let dir = fixture();
    let one = limits::download_permit(dir.path()).unwrap();
    let two = limits::download_permit(dir.path()).unwrap();
    assert_eq!(
        limits::download_permit(dir.path()).unwrap_err(),
        "EXPORT_BUSY"
    );
    drop(one);
    let replacement = limits::download_permit(dir.path()).unwrap();
    assert_eq!(
        limits::download_permit(dir.path()).unwrap_err(),
        "EXPORT_BUSY"
    );
    drop(two);
    drop(replacement);
    assert!(limits::download_permit(dir.path()).is_ok());
}

#[test]
fn lock_contention_requires_the_known_os_error_code() {
    assert!(worker_lease::is_lock_contention(
        &fs2::lock_contended_error()
    ));
    assert!(!worker_lease::is_lock_contention(&std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "no OS error code",
    )));
    assert!(!worker_lease::is_lock_contention(
        &std::io::Error::from_raw_os_error(i32::MAX)
    ));
}

#[test]
fn invalid_lock_paths_remain_io_errors_even_with_available_capacity() {
    let dir = fixture();
    let locks = dir.path().join("data_exports/.locks");
    std::fs::create_dir_all(&locks).unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    std::fs::create_dir(locks.join(id)).unwrap();
    assert_eq!(
        worker_lease::try_lease(dir.path(), id).unwrap_err(),
        "EXPORT_IO"
    );
    std::fs::create_dir(locks.join("download-0")).unwrap();
    assert_eq!(
        limits::download_permit(dir.path()).unwrap_err(),
        "EXPORT_IO"
    );
}

fn build_now(home: &Path, ctx: &UserContext, request: &ExportRequest) -> String {
    let (id, _lease) = reserve(home, ctx, request).unwrap();
    build(home, ctx, request, &id).unwrap();
    id
}
fn seed_conversations(home: &Path, count: usize) {
    let mut conn = rusqlite::Connection::open(home.join("sessions.db")).unwrap();
    conn.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,agent_id TEXT); CREATE TABLE session_messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,timestamp TEXT,hidden INTEGER DEFAULT 0,undone_at TEXT); INSERT INTO sessions VALUES ('webchat:test','sales');").unwrap();
    let tx = conn.transaction().unwrap();
    for n in 0..count {
        tx.execute("INSERT INTO session_messages(id,session_id,role,content,timestamp) VALUES (?1,'webchat:test','user',?2,'2026-10-10T02:00:00Z')",params![n as i64+1,format!("中文第{n}列\n\"引號\"")]).unwrap();
    }
    tx.commit().unwrap();
}
fn user(home: &Path, email: &str, role: UserRole) -> (UserDb, UserContext) {
    let db = UserDb::new(&home.join("users.db")).unwrap();
    let u = db
        .create_user(email, email, "isolated-test-password", role)
        .unwrap();
    db.bind_agent(&u.id, "sales", AccessLevel::Viewer).unwrap();
    let mut ctx = UserContext::admin_fallback();
    ctx.user_id = u.id;
    ctx.email = u.email;
    ctx.role = role;
    ctx.agent_access.insert("sales".into(), AccessLevel::Viewer);
    (db, ctx)
}
#[test]
fn native_csv_xlsx_same_ids_long_text_and_formula_safe() {
    let dir = fixture();
    let content = format!("=SUM(1,2)\n\"中文\"{}", "🐾".repeat(40_000));
    let rows = vec![LogicalRow {
        id: "test:1".into(),
        cells: vec![Cell::from(content.clone()), Cell::Number(123)],
    }];
    let csv = dir.path().join("same.csv");
    let xlsx = dir.path().join("same.xlsx");
    let c = writer::encode(
        &csv,
        ExportFormat::Csv,
        &["content", "number"],
        &rows,
        || Ok(()),
    )
    .unwrap();
    let x = writer::encode(
        &xlsx,
        ExportFormat::Xlsx,
        &["content", "number"],
        &rows,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(c, x);
    assert!(c > 2);
    let bytes = std::fs::read(csv).unwrap();
    assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
    let records: Vec<_> = csv::Reader::from_reader(&bytes[3..])
        .records()
        .map(Result::unwrap)
        .collect();
    let mut workbook: Xlsx<_> = open_workbook(xlsx).unwrap();
    let range = workbook.worksheet_range("Sheet1").unwrap();
    let mut assembled = String::new();
    let mut csv_assembled = String::new();
    for (i, record) in records.iter().enumerate() {
        let excel = range.rows().nth(i + 1).unwrap();
        assert_eq!(record[0], excel[0].to_string());
        assert_eq!(record[1], excel[1].to_string());
        assert_eq!(record[2], excel[2].to_string());
        let raw = excel[3].to_string();
        let escaped: Vec<String> = serde_json::from_str(&record[5]).unwrap();
        let csv_raw = if escaped.iter().any(|field| field == "content") {
            record[3].strip_prefix('\'').unwrap()
        } else {
            &record[3]
        };
        csv_assembled.push_str(csv_raw);
        assert_eq!(record[5], excel[5].to_string());
        assert_eq!(
            record[3],
            if i == 0 {
                format!("'{raw}")
            } else {
                raw.clone()
            }
        );
        assembled.push_str(&raw);
    }
    assert_eq!(assembled, content);
    assert_eq!(csv_assembled, content);
    let formulas = workbook.worksheet_formula("Sheet1").unwrap();
    assert!(formulas.rows().flatten().all(String::is_empty));
}
#[test]
fn conversations_over_old_limit_are_complete_in_both_formats_and_dates_half_open() {
    let dir = fixture();
    seed_conversations(dir.path(), 1_103);
    let ctx = UserContext::admin_fallback();
    let mut ids = Vec::new();
    for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
        let req = request(Dataset::Conversations, format);
        let id = build_now(dir.path(), &ctx, &req);
        assert_eq!(get(dir.path(), &ctx, &id).unwrap().logical_rows, 1_103);
        let file = download(dir.path(), &ctx, &id).unwrap();
        let path = dir.path().join(format!("test.{}", format.extension()));
        std::fs::write(&path, &file.bytes).unwrap();
        let records = if format == ExportFormat::Csv {
            csv::Reader::from_reader(&file.bytes[3..])
                .records()
                .map(|r| r.unwrap()[0].to_string())
                .collect::<Vec<_>>()
        } else {
            let mut book: Xlsx<_> = open_workbook(path).unwrap();
            book.worksheet_range("Sheet1")
                .unwrap()
                .rows()
                .skip(1)
                .map(|r| r[0].to_string())
                .collect()
        };
        ids.push(records);
    }
    assert_eq!(ids[0], ids[1]);
    assert_eq!(ids[0].len(), 1_103);
    let mut req = request(Dataset::Conversations, ExportFormat::Csv);
    req.until = Some("2026-10-10T10:00:00+08:00".parse().unwrap());
    assert!(
        source::collect(dir.path(), &ctx, &req, || Ok(()))
            .unwrap()
            .rows
            .is_empty()
    );
    req.until = None;
    req.from = Some("2026-10-10T10:00:00+08:00".parse().unwrap());
    assert_eq!(
        source::collect(dir.path(), &ctx, &req, || Ok(()))
            .unwrap()
            .rows
            .len(),
        1_103
    );
}
#[test]
fn hidden_undone_changed_and_cross_user_downloads_fail_closed() {
    let dir = fixture();
    seed_conversations(dir.path(), 3);
    let ctx = UserContext::admin_fallback();
    let req = request(Dataset::Conversations, ExportFormat::Csv);
    let id = build_now(dir.path(), &ctx, &req);
    let (_, other) = user(dir.path(), "other@test.invalid", UserRole::Manager);
    assert_eq!(
        download(dir.path(), &other, &id).err().unwrap(),
        "EXPORT_NOT_FOUND"
    );
    let conn = rusqlite::Connection::open(dir.path().join("sessions.db")).unwrap();
    conn.execute("UPDATE session_messages SET hidden=1 WHERE id=1", [])
        .unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
    let id2 = build_now(dir.path(), &ctx, &req);
    conn.execute(
        "UPDATE session_messages SET undone_at='2026-10-11T00:00:00Z' WHERE id=2",
        [],
    )
    .unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id2).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
    let id3 = build_now(dir.path(), &ctx, &req);
    conn.execute(
        "UPDATE session_messages SET content='changed' WHERE id=3",
        [],
    )
    .unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id3).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
}
#[test]
fn binding_and_admin_role_are_refreshed_and_unknown_usage_stays_unknown() {
    let dir = fixture();
    seed_conversations(dir.path(), 1);
    let (db, ctx) = user(dir.path(), "viewer@test.invalid", UserRole::Manager);
    let req = request(Dataset::Conversations, ExportFormat::Csv);
    let id = build_now(dir.path(), &ctx, &req);
    db.unbind_agent(&ctx.user_id, "sales").unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id).err().unwrap(),
        "EXPORT_PERMISSION_DENIED"
    );
    let conn = rusqlite::Connection::open(dir.path().join("cost_telemetry.db")).unwrap();
    conn.execute_batch("CREATE TABLE token_usage(id INTEGER PRIMARY KEY,agent_id TEXT,created_at TEXT,model TEXT,request_type TEXT,input_tokens INTEGER,cache_read_tokens INTEGER,cache_creation_tokens INTEGER,output_tokens INTEGER,cost_millicents INTEGER); INSERT INTO token_usage VALUES (1,'sales','2026-10-11T00:00:00Z','','unknown',0,0,0,0,0);").unwrap();
    let (db, admin) = user(dir.path(), "admin@test.invalid", UserRole::Admin);
    let req = request(Dataset::Usage, ExportFormat::Csv);
    let table = source::collect(dir.path(), &admin, &req, || Ok(())).unwrap();
    assert_eq!(table.rows[0].cells[3], Cell::from("unknown"));
    assert_eq!(table.rows[0].cells[6], Cell::from(""));
    assert_eq!(table.rows[0].cells[11], Cell::from(""));
    assert_eq!(table.rows[0].cells[13], Cell::from("unknown"));
    let id = build_now(dir.path(), &admin, &req);
    db.update_user(&admin.user_id, None, Some(UserRole::Manager), None)
        .unwrap();
    assert_eq!(
        download(dir.path(), &admin, &id).err().unwrap(),
        "EXPORT_PERMISSION_DENIED"
    );
}
#[test]
fn memory_forget_and_wiki_change_invalidate_existing_files() {
    let dir = fixture();
    let ctx = UserContext::admin_fallback();
    let conn = rusqlite::Connection::open(dir.path().join("memory.db")).unwrap();
    conn.execute_batch("CREATE TABLE memories(id TEXT,agent_id TEXT,layer TEXT,content TEXT,source_event TEXT,valid_until TEXT,timestamp TEXT,quarantined INTEGER,valid_from TEXT); CREATE TABLE key_facts(id TEXT,agent_id TEXT,fact TEXT,source_session TEXT,timestamp TEXT); INSERT INTO memories VALUES ('m1','sales','episodic','秘密','user',NULL,'2026-10-11T00:00:00Z',0,NULL); INSERT INTO key_facts VALUES ('k1','sales','關鍵事實','session','2026-10-11T00:00:00Z');").unwrap();
    let id = build_now(
        dir.path(),
        &ctx,
        &request(Dataset::Memories, ExportFormat::Csv),
    );
    assert_eq!(get(dir.path(), &ctx, &id).unwrap().logical_rows, 2);
    conn.execute("DELETE FROM memories WHERE id='m1'", [])
        .unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
    let root = dir.path().join("shared/wiki");
    std::fs::create_dir_all(&root).unwrap();
    let store = duduclaw_memory::WikiStore::new(root);
    store.write_page("test.md", "# 知識\n內容").unwrap();
    let mut req = request(Dataset::Wiki, ExportFormat::Xlsx);
    req.agent_id = None;
    req.scope = Some("shared".into());
    let id = build_now(dir.path(), &ctx, &req);
    assert_eq!(get(dir.path(), &ctx, &id).unwrap().logical_rows, 1);
    store.write_page("test.md", "# 知識\n已修改").unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
}
#[test]
fn task_audience_change_invalidates_and_initially_excludes_private_rows() {
    let dir = fixture();
    let (db, alice) = user(dir.path(), "alice@test.invalid", UserRole::Manager);
    let (_, bob) = user(dir.path(), "bob@test.invalid", UserRole::Manager);
    let conn = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    conn.execute_batch("CREATE TABLE tasks(id TEXT,title TEXT,status TEXT,assigned_to TEXT,created_at TEXT,completed_at TEXT); INSERT INTO tasks VALUES ('private-task','私密','done','sales','2026-10-11T00:00:00Z',NULL);").unwrap();
    let req = request(Dataset::Tasks, ExportFormat::Csv);
    let id = build_now(dir.path(), &bob, &req);
    let path = dir
        .path()
        .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
        .join("private-task/1");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("p.json"),serde_json::json!({"packet_id":"p","goal_id":"private-task","round":1,"from_role":"executor","to_role":"verifier","objective":"Review","output_format":"files","audience":[format!("user:{}",alice.user_id)]}).to_string()).unwrap();
    assert_eq!(
        download(dir.path(), &bob, &id).err().unwrap(),
        "EXPORT_SOURCE_CHANGED"
    );
    assert!(
        source::collect(dir.path(), &bob, &req, || Ok(()))
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        source::collect(dir.path(), &alice, &req, || Ok(()))
            .unwrap()
            .rows
            .len(),
        1
    );
    drop(db);
}
#[test]
fn cancellation_quota_restart_and_expiry_remove_files() {
    let dir = fixture();
    seed_conversations(dir.path(), 1);
    let ctx = UserContext::admin_fallback();
    let req = request(Dataset::Conversations, ExportFormat::Csv);
    let (queued, queued_lease) = reserve(dir.path(), &ctx, &req).unwrap();
    assert_eq!(reserve(dir.path(), &ctx, &req).unwrap_err(), "EXPORT_BUSY");
    let (_, other) = user(dir.path(), "other@test.invalid", UserRole::Manager);
    let (second, second_lease) = reserve(dir.path(), &other, &req).unwrap();
    let (_, third) = user(dir.path(), "third@test.invalid", UserRole::Manager);
    assert_eq!(
        reserve(dir.path(), &third, &req).unwrap_err(),
        "EXPORT_BUSY"
    );
    assert_eq!(
        cancel(dir.path(), &ctx, &queued).unwrap().state,
        "cancelled"
    );
    assert_eq!(
        build(dir.path(), &ctx, &req, &queued).unwrap_err(),
        "EXPORT_CANCELLED"
    );
    initialize(dir.path()).unwrap();
    assert_eq!(
        get(dir.path(), &other, &second).unwrap().state,
        "queued",
        "another gateway boot must skip a live worker"
    );
    assert_eq!(
        reserve(dir.path(), &third, &req).unwrap_err(),
        "EXPORT_BUSY",
        "a cancelled worker retains its real lifetime quota"
    );
    drop(queued_lease);
    drop(second_lease);
    initialize(dir.path()).unwrap();
    assert_eq!(
        get(dir.path(), &other, &second)
            .unwrap()
            .error_code
            .as_deref(),
        Some("EXPORT_INTERRUPTED")
    );
    let id = build_now(dir.path(), &ctx, &req);
    let path = job_dir(dir.path(), &id).unwrap().join("result.csv");
    assert!(path.exists());
    let conn = store::db(dir.path()).unwrap();
    conn.execute(
        "UPDATE exports SET expires_at=?2 WHERE id=?1",
        params![id, (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()],
    )
    .unwrap();
    assert_eq!(get(dir.path(), &ctx, &id).unwrap().state, "expired");
    assert!(!path.exists());
    assert!(download(dir.path(), &ctx, &id).is_err());
}
#[test]
fn cancellation_checked_during_encoding_and_cell_split_has_no_truncation() {
    let dir = fixture();
    let row = LogicalRow {
        id: "r".into(),
        cells: vec![Cell::from("中".repeat(70_000))],
    };
    let parts = writer::physical_rows(&row);
    let joined = parts
        .iter()
        .map(|p| {
            if let Cell::Text(t) = &p.cells[0] {
                t.clone()
            } else {
                String::new()
            }
        })
        .collect::<String>();
    assert_eq!(joined, "中".repeat(70_000));
    let result = writer::encode(
        &dir.path().join("partial.csv"),
        ExportFormat::Csv,
        &["text"],
        &[row],
        || Err("EXPORT_CANCELLED".into()),
    );
    assert_eq!(result.unwrap_err(), "EXPORT_CANCELLED");
}
#[test]
fn bounded_scan_refuses_over_limit_instead_of_silent_truncation() {
    let dir = fixture();
    seed_conversations(dir.path(), MAX_ROWS + 1);
    let err = source::collect(
        dir.path(),
        &UserContext::admin_fallback(),
        &request(Dataset::Conversations, ExportFormat::Csv),
        || Ok(()),
    )
    .err()
    .unwrap();
    assert_eq!(err, "EXPORT_TOO_LARGE");
}
#[cfg(unix)]
#[test]
fn private_storage_permissions_and_symlinks_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = fixture();
    assert_eq!(
        std::fs::metadata(dir.path().join("data_exports"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("data_exports/jobs.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let outside = tempfile::tempdir().unwrap();
    let path = dir.path().join("data_exports").join("f".repeat(32));
    symlink(outside.path(), &path).unwrap();
    assert!(job_dir(dir.path(), &"f".repeat(32)).is_err());
    assert!(job_dir(dir.path(), "../../escape").is_err());
}

#[test]
fn all_five_datasets_have_matching_native_csv_and_xlsx_record_sets() {
    let dir = fixture();
    seed_conversations(dir.path(), 503);
    let mut tasks = rusqlite::Connection::open(dir.path().join("tasks.db")).unwrap();
    tasks.execute_batch("CREATE TABLE tasks(id TEXT,title TEXT,status TEXT,assigned_to TEXT,created_at TEXT,completed_at TEXT);").unwrap();
    let mut usage = rusqlite::Connection::open(dir.path().join("cost_telemetry.db")).unwrap();
    usage.execute_batch("CREATE TABLE token_usage(id INTEGER PRIMARY KEY,agent_id TEXT,created_at TEXT,model TEXT,request_type TEXT,input_tokens INTEGER,cache_read_tokens INTEGER,cache_creation_tokens INTEGER,output_tokens INTEGER,cost_millicents INTEGER);").unwrap();
    let mut memory = rusqlite::Connection::open(dir.path().join("memory.db")).unwrap();
    memory.execute_batch("CREATE TABLE memories(id TEXT,agent_id TEXT,layer TEXT,content TEXT,source_event TEXT,valid_until TEXT,timestamp TEXT,quarantined INTEGER,valid_from TEXT); CREATE TABLE key_facts(id TEXT,agent_id TEXT,fact TEXT,source_session TEXT,timestamp TEXT);").unwrap();
    let tx = tasks.transaction().unwrap();
    let ux = usage.transaction().unwrap();
    let mx = memory.transaction().unwrap();
    for n in 1..=503 {
        tx.execute(
            "INSERT INTO tasks VALUES (?1,'中文任務','done','sales','2026-10-11T00:00:00Z',NULL)",
            params![format!("task-{n}")],
        )
        .unwrap();
        ux.execute("INSERT INTO token_usage VALUES (?1,'sales','2026-10-11T00:00:00Z','raw-model','reply',10,0,0,20,12345)",params![n]).unwrap();
        mx.execute("INSERT INTO memories VALUES (?1,'sales','episodic','中文記憶','user',NULL,'2026-10-11T00:00:00Z',0,NULL)",params![format!("memory-{n}")]).unwrap();
    }
    tx.commit().unwrap();
    ux.commit().unwrap();
    mx.commit().unwrap();
    let root = dir.path().join("agents/sales/wiki");
    std::fs::create_dir_all(&root).unwrap();
    for n in 1..=503 {
        std::fs::write(
            root.join(format!("page-{n}.md")),
            "---\ntitle: 中文知識\nupdated: 2026-10-11T00:00:00Z\n---\n# 知識\n正文",
        )
        .unwrap();
    }
    let ctx = UserContext::admin_fallback();
    for dataset in [
        Dataset::Tasks,
        Dataset::Conversations,
        Dataset::Usage,
        Dataset::Memories,
        Dataset::Wiki,
    ] {
        let mut formats = Vec::new();
        for format in [ExportFormat::Csv, ExportFormat::Xlsx] {
            let id = build_now(dir.path(), &ctx, &request(dataset, format));
            let status = get(dir.path(), &ctx, &id).unwrap();
            assert_eq!(status.logical_rows, 503, "{dataset:?}");
            let file = download(dir.path(), &ctx, &id).unwrap();
            let path = dir.path().join(format!(
                "matrix-{}-{}.{}",
                dataset.name(),
                id,
                format.extension()
            ));
            std::fs::write(&path, &file.bytes).unwrap();
            let mut ids = if format == ExportFormat::Csv {
                csv::Reader::from_reader(&file.bytes[3..])
                    .records()
                    .map(|row| row.unwrap()[0].to_string())
                    .collect::<Vec<_>>()
            } else {
                let mut book: Xlsx<_> = open_workbook(path).unwrap();
                book.worksheet_range("Sheet1")
                    .unwrap()
                    .rows()
                    .skip(1)
                    .map(|row| row[0].to_string())
                    .collect::<Vec<_>>()
            };
            ids.sort();
            formats.push(ids);
        }
        assert_eq!(formats[0], formats[1], "{dataset:?}");
    }
}
#[test]
fn memory_validity_and_unrelated_malformed_timestamps_are_not_misreported() {
    let dir = fixture();
    let conn = rusqlite::Connection::open(dir.path().join("memory.db")).unwrap();
    conn.execute_batch("CREATE TABLE memories(id TEXT,agent_id TEXT,layer TEXT,content TEXT,source_event TEXT,valid_until TEXT,timestamp TEXT,quarantined INTEGER,valid_from TEXT); CREATE TABLE key_facts(id TEXT,agent_id TEXT,fact TEXT,source_session TEXT,timestamp TEXT);
    INSERT INTO memories VALUES ('null','sales','episodic','null','user',NULL,'2026-10-11T00:00:00Z',0,NULL);
    INSERT INTO memories VALUES ('future','sales','episodic','future','user','2099-01-01T00:00:00Z','2026-10-11T00:00:00Z',0,NULL);
    INSERT INTO memories VALUES ('past','sales','episodic','past','user','2000-01-01T00:00:00Z','2026-10-11T00:00:00Z',0,NULL);
    INSERT INTO memories VALUES ('scheduled','sales','episodic','scheduled','user',NULL,'2026-10-11T00:00:00Z',0,'2099-01-01T00:00:00Z');
    INSERT INTO memories VALUES ('other','other','episodic','other','user',NULL,'malformed-other-time',0,NULL);
    INSERT INTO key_facts VALUES ('other','other','other','session','malformed-other-time');").unwrap();
    let rows = source::collect(
        dir.path(),
        &UserContext::admin_fallback(),
        &request(Dataset::Memories, ExportFormat::Csv),
        || Ok(()),
    )
    .unwrap()
    .rows;
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.id == format!("memory:{id}"))
            .unwrap()
            .cells[5]
            .clone()
    };
    assert_eq!(state("null"), Cell::from("current"));
    assert_eq!(state("future"), Cell::from("current"));
    assert_eq!(state("past"), Cell::from("expired"));
    assert_eq!(state("scheduled"), Cell::from("scheduled"));
    seed_conversations(dir.path(), 1);
    let conn = rusqlite::Connection::open(dir.path().join("sessions.db")).unwrap();
    conn.execute_batch("INSERT INTO sessions VALUES ('other-session','other'); INSERT INTO session_messages VALUES (999,'other-session','user','other','bad-other-time',0,NULL);").unwrap();
    assert_eq!(
        source::collect(
            dir.path(),
            &UserContext::admin_fallback(),
            &request(Dataset::Conversations, ExportFormat::Csv),
            || Ok(())
        )
        .unwrap()
        .rows
        .len(),
        1
    );
}
#[test]
fn retained_storage_and_download_lifetime_are_bounded() {
    let dir = fixture();
    seed_conversations(dir.path(), 1);
    let ctx = UserContext::admin_fallback();
    let req = request(Dataset::Conversations, ExportFormat::Csv);
    let id = build_now(dir.path(), &ctx, &req);
    let one = download(dir.path(), &ctx, &id).unwrap();
    let two = download(dir.path(), &ctx, &id).unwrap();
    assert_eq!(
        download(dir.path(), &ctx, &id).err().unwrap(),
        "EXPORT_BUSY"
    );
    drop(one);
    assert!(download(dir.path(), &ctx, &id).is_ok());
    drop(two);
    let conn = store::db(dir.path()).unwrap();
    conn.execute(
        "UPDATE exports SET size_bytes=?2 WHERE id=?1",
        params![id, limits::MAX_RETAINED_BYTES],
    )
    .unwrap();
    assert_eq!(
        limits::retained_capacity(&conn, &ctx.user_id, 1).unwrap_err(),
        "EXPORT_STORAGE_FULL"
    );
    conn.execute("UPDATE exports SET size_bytes=0 WHERE id=?1", params![id])
        .unwrap();
    for n in 0..19 {
        conn.execute(
            "INSERT INTO exports(id,owner,request,state,expires_at) VALUES (?1,?2,?3,'ready',?4)",
            params![
                format!("{n:032x}"),
                ctx.user_id,
                serde_json::to_string(&req).unwrap(),
                (Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
            ],
        )
        .unwrap();
    }
    assert_eq!(
        reserve(dir.path(), &ctx, &req).unwrap_err(),
        "EXPORT_STORAGE_FULL"
    );
}
#[test]
fn wiki_large_files_and_cancellation_are_checked_before_unbounded_loading() {
    let dir = fixture();
    let root = dir.path().join("agents/sales/wiki");
    std::fs::create_dir_all(&root).unwrap();
    let large = std::fs::File::create(root.join("large.md")).unwrap();
    large.set_len(MAX_BYTES + 1).unwrap();
    let req = request(Dataset::Wiki, ExportFormat::Csv);
    assert_eq!(
        source::collect(dir.path(), &UserContext::admin_fallback(), &req, || Ok(()))
            .err()
            .unwrap(),
        "EXPORT_TOO_LARGE"
    );
    assert_eq!(
        source::collect(dir.path(), &UserContext::admin_fallback(), &req, || Err(
            "EXPORT_CANCELLED".into()
        ))
        .err()
        .unwrap(),
        "EXPORT_CANCELLED"
    );
}
