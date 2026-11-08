//! H11: protected file_read / csv_read / xlsx_read over the brokered
//! `employee-work` root (root's sealed memfd snapshot), and the unchanged
//! non-protected readers.
use super::*;
#[cfg(target_os = "linux")]
use duduclaw_mcp_boundary::{factories::GatewayTableParser, parsing::{FileParser, TableKind, TableOptions},
    resources::{AuthorizedFile, BROKERED_FILE_MAX_BYTES, EMPLOYEE_WORK_ROOT, WorkBrokerError,
        test_broker::TestWorkBroker}};
#[cfg(target_os = "linux")]
use std::sync::Mutex;

#[cfg(target_os = "linux")]
const RUN: RunId = RunId([3; 16]);
#[cfg(target_os = "linux")]
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);

fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap_or("null")).unwrap_or(Value::Null)
}
fn message(result: &Value) -> String {
    result["content"][0]["text"].as_str().unwrap_or_default().to_owned()
}
fn audit_log(home: &std::path::Path) -> String {
    std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn brokered_call(broker: Arc<TestWorkBroker>, parser: Option<Arc<dyn FileParser>>) -> Arc<IsolatedCall> {
    let mut record = context().record().clone();
    record.tools = List::new(["file_read", "csv_read", "xlsx_read"].iter()
        .map(|tool| Text::new((*tool).to_owned()).unwrap()).collect()).unwrap();
    record.resources = List::new(vec![ResourceGrant::Workspace {
        handle: Text::new(EMPLOYEE_WORK_ROOT.to_owned()).unwrap() }]).unwrap();
    let mut roots = ResourceRoots::default();
    roots.insert_brokered(EMPLOYEE_WORK_ROOT.to_owned(), EMPLOYEE, broker).unwrap();
    Arc::new(IsolatedCall { forks: None, home: PathBuf::new(), execution: None, worker: None,
        record_resolver: None, context: live_context(record), agent: "main".into(),
        roots: Arc::new(roots), parser, services: None })
}

/// What the parse-worker stand-in received for one call.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct Seen { kind: TableKind, sheet: Option<String>, bytes: Vec<u8>, nlink: u64, declared: u64 }

/// The real boundary adapter (`GatewayTableParser`: call binding, request
/// recheck, invocation fence, blocking pool, response re-decode) over a
/// stand-in for the gateway's ReadTable worker call, so the test sees exactly
/// the descriptor E would snapshot. The stand-in reads it only through
/// `AuthorizedFile` (live authority on every read) and returns a fixed table.
#[cfg(target_os = "linux")]
fn worker_stand_in(seen: Arc<Mutex<Vec<Seen>>>) -> Arc<dyn FileParser> {
    Arc::new(GatewayTableParser::new(Arc::new(move |request: duduclaw_isolation::worker::ParseRequest,
        mut input: AuthorizedFile, options: TableOptions| -> Result<Value, VerificationError> {
        use std::{io::Read, os::unix::fs::MetadataExt};
        let metadata = input.metadata()?;
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).map_err(|_| VerificationError::Unavailable)?;
        seen.lock().unwrap().push(Seen { kind: options.kind, sheet: options.sheet.clone(), bytes,
            nlink: metadata.nlink(), declared: request.inputs.as_slice()[0].size_bytes });
        let mut table = json!({"columns": ["name"], "rows": [{"name": "Alice"}], "truncated": false});
        if options.kind == TableKind::Spreadsheet {
            table["sheet"] = json!("Sheet1");
            table["sheets"] = json!(["Sheet1", "備註"]);
        }
        Ok(table)
    })))
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn h11_file_read_uses_brokered_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let host = work.path().to_string_lossy().into_owned();
    std::fs::create_dir(work.path().join("notes")).unwrap();
    std::fs::write(work.path().join("notes/週報.md"), "第一週進度：完成").unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    ISOLATED_CALL.scope(brokered_call(broker.clone(), None), async {
        let args = json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "notes/週報.md", "max_bytes": 6});
        let result = crate::mcp_files::handle_isolated_file_read(&args, home.path(), "main").await;
        assert_ne!(result["isError"], true, "{result}");
        let read = body(&result);
        // Bounded by max_bytes on a char boundary; the size comes from the snapshot.
        assert_eq!(read["text"], "第一");
        assert_eq!(read["truncated"], true);
        assert_eq!(read["path"], "notes/週報.md");
        assert_eq!(read["table"], "週報.md");
        assert!(!result.to_string().contains(&host), "{result}");
        // Run and employee come from the capability; root saw the exact relative path.
        assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, "notes/週報.md".to_owned())]);
        // The synchronous entry point takes the same brokered route.
        let sync = crate::mcp_files::handle_file_read(&args, home.path(), "main");
        assert_eq!(body(&sync)["text"], "第一");
        assert_eq!(broker.calls().len(), 2);
        // Structured files are redirected before root is asked for anything.
        let csv = crate::mcp_files::handle_isolated_file_read(
            &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "notes/t.csv"}), home.path(), "main").await;
        assert_eq!(csv["isError"], true);
        assert_eq!(broker.calls().len(), 2);
    }).await;
    let audit = audit_log(home.path());
    assert!(audit.contains("notes/週報.md") && !audit.contains(&host), "{audit}");
    assert!(!audit.contains("第一週"), "content must never reach the audit: {audit}");

    // A broker handing back the live employee inode instead of root's sealed
    // snapshot is refused by the boundary; nothing of the file is returned.
    let unsealed = Arc::new(TestWorkBroker::unchecked(work.path().to_owned(), RUN, EMPLOYEE));
    ISOLATED_CALL.scope(brokered_call(unsealed.clone(), None), async {
        let result = crate::mcp_files::handle_isolated_file_read(
            &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "notes/週報.md"}), home.path(), "main").await;
        assert_eq!(result["isError"], true);
        assert_eq!(message(&result), "無法讀取這次執行授權範圍內的文字檔。");
        assert!(!result.to_string().contains("第一週"));
        assert_eq!(unsealed.calls().len(), 1);
    }).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn h11_csv_read_parses_via_worker() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let host = work.path().to_string_lossy().into_owned();
    std::fs::create_dir(work.path().join("客戶")).unwrap();
    std::fs::write(work.path().join("客戶/名單.csv"), "name\nAlice\n").unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    let seen = Arc::new(Mutex::new(Vec::new()));
    ISOLATED_CALL.scope(brokered_call(broker.clone(), Some(worker_stand_in(seen.clone()))), async {
        let result = crate::mcp_files::handle_isolated_table_read("csv_read",
            &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "客戶/名單.csv"}), home.path(), "main").await;
        assert_ne!(result["isError"], true, "{result}");
        let table = body(&result);
        assert_eq!(table["path"], "客戶/名單.csv");
        assert_eq!(table["table"], "名單.csv");
        assert_eq!(table["rows"][0]["name"], "Alice");
        assert_eq!(table["row_count"], 1);
        assert!(!result.to_string().contains(&host));
        assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, "客戶/名單.csv".to_owned())]);
        // The legacy in-process reader never runs inside a protected call.
        let legacy = crate::mcp_files::handle_csv_read(
            &json!({"path": work.path().join("客戶/名單.csv")}), home.path(), "main");
        assert_eq!(legacy["isError"], true);
    }).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].kind, TableKind::Csv);
    assert_eq!(seen[0].bytes, b"name\nAlice\n");
    // Root's sealed memfd snapshot (nlink 0), never the employee's linked inode.
    assert_eq!(seen[0].nlink, 0);
    assert_eq!(seen[0].declared, 11);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn h11_xlsx_read_parses_via_worker() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let workbook = b"PK\x03\x04 workbook bytes".to_vec();
    std::fs::create_dir(work.path().join("報表")).unwrap();
    std::fs::write(work.path().join("報表/2026.xlsx"), &workbook).unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    let seen = Arc::new(Mutex::new(Vec::new()));
    ISOLATED_CALL.scope(brokered_call(broker.clone(), Some(worker_stand_in(seen.clone()))), async {
        let result = crate::mcp_files::handle_isolated_table_read("xlsx_read",
            &json!({"workspace": EMPLOYEE_WORK_ROOT, "path": "報表/2026.xlsx", "sheet": "Sheet1"}),
            home.path(), "main").await;
        assert_ne!(result["isError"], true, "{result}");
        let table = body(&result);
        assert_eq!(table["table"], "2026.xlsx");
        assert_eq!(table["sheet"], "Sheet1");
        assert_eq!(table["sheets"], json!(["Sheet1", "備註"]));
        assert_eq!(table["rows"][0]["name"], "Alice");
        assert_eq!(broker.calls(), vec![(RUN, EMPLOYEE, "報表/2026.xlsx".to_owned())]);
        let legacy = crate::mcp_files::handle_xlsx_read(
            &json!({"path": work.path().join("報表/2026.xlsx")}), home.path(), "main");
        assert_eq!(legacy["isError"], true);
    }).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].kind, TableKind::Spreadsheet);
    assert_eq!(seen[0].sheet.as_deref(), Some("Sheet1"));
    assert_eq!(seen[0].bytes, workbook);
    assert_eq!(seen[0].nlink, 0);
    assert_eq!(seen[0].declared, workbook.len() as u64);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn h11_refusal_messages_no_path_leak() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let host = work.path().to_string_lossy().into_owned();
    std::fs::create_dir(work.path().join("機密")).unwrap();
    std::fs::write(work.path().join("target.txt"), b"TARGET_CONTENT").unwrap();
    std::os::unix::fs::symlink("../target.txt", work.path().join("機密/捷徑-2026.txt")).unwrap();
    std::fs::File::create(work.path().join("機密/巨大-2026.txt")).unwrap()
        .set_len(BROKERED_FILE_MAX_BYTES + 1).unwrap();
    let broker = Arc::new(TestWorkBroker::new(work.path().to_owned(), RUN, EMPLOYEE));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let file_read = |path: &str| json!({"workspace": EMPLOYEE_WORK_ROOT, "path": path});
    let assert_clean = |result: &Value, path: &str| {
        let whole = result.to_string();
        assert!(!whole.contains(&host) && !whole.contains(EMPLOYEE_WORK_ROOT) && !whole.contains("TARGET"), "{whole}");
        for fragment in path.split(['/', '-', '.']).filter(|part| !part.is_empty()) {
            assert!(!message(result).contains(fragment), "{result} leaks {fragment:?}");
        }
    };
    ISOLATED_CALL.scope(brokered_call(broker.clone(), Some(worker_stand_in(seen.clone()))), async {
        for (path, expected) in [("機密/薪資-2026.txt", WorkBrokerError::NotFound),
            ("機密/捷徑-2026.txt", WorkBrokerError::Refused), ("機密/巨大-2026.txt", WorkBrokerError::TooLarge)] {
            let result = crate::mcp_files::handle_isolated_file_read(&file_read(path), home.path(), "main").await;
            assert_eq!(result["isError"], true, "{path}");
            assert_eq!(message(&result), expected.user_message(), "{path}");
            assert_clean(&result, path);
        }
        for error in [WorkBrokerError::Unavailable, WorkBrokerError::NotLive] {
            broker.fail_with(Some(error));
            let result = crate::mcp_files::handle_isolated_file_read(
                &file_read("機密/薪資-2026.txt"), home.path(), "main").await;
            assert_eq!(message(&result), error.user_message());
            assert_clean(&result, "機密/薪資-2026.txt");
        }
        broker.fail_with(None);
        // The table tools show root's reason too, before any parser call.
        let missing = crate::mcp_files::handle_isolated_table_read("csv_read",
            &file_read("機密/薪資-2026.csv"), home.path(), "main").await;
        assert_eq!(message(&missing), WorkBrokerError::NotFound.user_message());
        assert_clean(&missing, "機密/薪資-2026.csv");
        // A boundary refusal keeps the generic text and never reaches root.
        let calls = broker.calls().len();
        let escape = crate::mcp_files::handle_isolated_file_read(
            &file_read("../機密/薪資-2026.txt"), home.path(), "main").await;
        assert_eq!(message(&escape), "無法讀取這次執行授權範圍內的文字檔。");
        let other = crate::mcp_files::handle_isolated_table_read("csv_read",
            &json!({"workspace": "work", "path": "機密/薪資-2026.csv"}), home.path(), "main").await;
        assert_eq!(message(&other), "無法解析這次執行授權範圍內的資料檔。");
        assert_eq!(broker.calls().len(), calls);
        // The run ended: root refuses, the employee sees the fixed reason.
        broker.end_run();
        let ended = crate::mcp_files::handle_isolated_file_read(
            &file_read("機密/薪資-2026.txt"), home.path(), "main").await;
        assert_eq!(message(&ended), WorkBrokerError::NotLive.user_message());
    }).await;
    assert!(seen.lock().unwrap().is_empty(), "no refused open may reach the worker");
    // Refusal rows carry the reason code only: no requested path, no host path.
    let audit = audit_log(home.path());
    for code in ["work_file_not_found", "work_file_refused", "work_file_too_large",
        "workspace_broker_unavailable", "work_run_not_live", "isolation_resource"] {
        assert!(audit.contains(code), "{code} missing: {audit}");
    }
    // (Not "2026": audit rows carry a timestamp.)
    assert!(!audit.contains(&host) && !audit.contains("機密") && !audit.contains("薪資")
        && !audit.contains("捷徑") && !audit.contains("巨大"), "{audit}");
}

#[tokio::test]
async fn h11_unprotected_behaviour_unchanged() {
    assert!(current().is_none());
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/main");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.md"), "# 標題\n內容\n").unwrap();
    std::fs::write(dir.join("customers.csv"), "id,name\n1,王小明\n").unwrap();
    std::fs::write(dir.join("not-a-workbook.xlsx"), b"this is not a zip").unwrap();
    let notes = dir.join("notes.md");
    let customers = dir.join("customers.csv");

    // file_read: legacy fence, canonical host path in the payload, as before.
    let read = crate::mcp_files::handle_file_read(&json!({"path": notes}), home.path(), "main");
    assert_ne!(read["isError"], true, "{read}");
    assert_eq!(body(&read)["path"], notes.canonicalize().unwrap().to_string_lossy().into_owned());
    assert_eq!(body(&read)["text"], "# 標題\n內容\n");
    assert_eq!(body(&read)["truncated"], false);
    // csv_read: in-process legacy reader, cells always strings.
    let table = crate::mcp_files::handle_csv_read(&json!({"path": customers}), home.path(), "main");
    assert_eq!(body(&table)["columns"], json!(["id", "name"]));
    assert_eq!(body(&table)["rows"][0]["id"], "1");
    assert_eq!(body(&table)["rows"][0]["name"], "王小明");
    assert_eq!(body(&table)["path"], customers.canonicalize().unwrap().to_string_lossy().into_owned());
    // xlsx_read: legacy calamine error text.
    let xlsx = crate::mcp_files::handle_xlsx_read(&json!({"path": dir.join("not-a-workbook.xlsx")}), home.path(), "main");
    assert!(message(&xlsx).contains("無法開啟"), "{xlsx}");
    // The legacy fence still names its roots and still records the request.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("payroll.csv"), "salary\n999\n").unwrap();
    let refused = crate::mcp_files::handle_csv_read(
        &json!({"path": outside.path().join("payroll.csv")}), home.path(), "main");
    assert!(message(&refused).contains("不在允許的目錄範圍內"), "{refused}");
    assert!(audit_log(home.path()).contains("payroll.csv"));
    let traversal = crate::mcp_files::handle_file_read(
        &json!({"path": format!("{}/../../x", dir.display())}), home.path(), "main");
    assert!(message(&traversal).contains(".."), "{traversal}");

    // The protected entry points never serve a legacy host-path read.
    let protected = crate::mcp_files::handle_isolated_file_read(&json!({"path": notes}), home.path(), "main").await;
    assert_eq!(protected["isError"], true);
    assert!(!protected.to_string().contains("標題"));
    for tool in ["csv_read", "xlsx_read"] {
        let protected = crate::mcp_files::handle_isolated_table_read(tool,
            &json!({"path": customers}), home.path(), "main").await;
        assert_eq!(protected["isError"], true);
        assert!(!protected.to_string().contains("王小明"));
    }
}
