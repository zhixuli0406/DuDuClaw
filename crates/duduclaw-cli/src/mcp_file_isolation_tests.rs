use super::*;
use duduclaw_mcp_boundary::parsing::{FileParser, TableOptions};
use duduclaw_isolation::worker::{ParseRequest, ParseResponse, ParseResult, ParsedItem, WORKER_PROTOCOL_VERSION};
use std::{fs::File, future::Future, pin::Pin, sync::atomic::{AtomicUsize, Ordering}};

struct Parser { calls: AtomicUsize, wrong_table: bool }
impl FileParser for Parser {
    fn parse<'a>(&'a self, call: duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall, request: ParseRequest,
        mut input: duduclaw_mcp_boundary::resources::AuthorizedFile,
        _options: TableOptions)
        -> Pin<Box<dyn Future<Output = Result<ParseResponse, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            use std::io::Read;
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(call.context().record().employee, EmployeeId([2; 16]));
            let mut bytes = String::new();
            input.read_to_string(&mut bytes).unwrap();
            assert_eq!(bytes, "name\nAlice\n");
            let mut value = json!({"columns":["name"],"rows":[{"name":"Alice"}],"truncated":false});
            if self.wrong_table { value["table"] = json!("unprotected.csv"); }
            Ok(ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
                result: ParseResult::Complete { items: List::new(vec![ParsedItem::Text {
                    input_slot: 0, page: None, text: Text::new(value.to_string()).unwrap(),
                }]).unwrap() } })
        })
    }
}

#[tokio::test]
async fn isolated_table_reader_never_calls_legacy_parser_or_falls_back_without_worker() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("secret.csv");
    std::fs::write(&path, "name\nDO_NOT_READ\n").unwrap();
    ISOLATED_CALL.scope(call(), async {
        for tool in ["csv_read", "xlsx_read"] {
            let result = crate::mcp_files::handle_isolated_table_read(tool,
                &json!({"workspace":"work","path":path}), home.path(), "main").await;
            assert_eq!(result["isError"], true);
            assert!(!result.to_string().contains("DO_NOT_READ"));
        }
        assert_eq!(crate::mcp_files::handle_csv_read(&json!({"path":path}), home.path(), "main")["isError"], true);
        assert_eq!(crate::mcp_files::handle_xlsx_read(&json!({"path":path}), home.path(), "main")["isError"], true);
    }).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn isolated_table_reader_passes_only_authorized_fd_and_rebuilds_redaction_table() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir_in("/dev/shm").unwrap();
    std::fs::write(work.path().join("customers.csv"), "name\nAlice\n").unwrap();
    for wrong_table in [false, true] {
        let parser = Arc::new(Parser { calls: AtomicUsize::new(0), wrong_table });
        let mut call = call();
        let mut roots = ResourceRoots::default();
        roots.insert("work".into(), EmployeeId([2; 16]), File::open(work.path()).unwrap()).unwrap();
        let inner = Arc::get_mut(&mut call).unwrap();
        inner.roots = Arc::new(roots);
        let mut record = inner.context.record().clone();
        record.tools = List::new(vec![Text::new("csv_read".into()).unwrap(), Text::new("xlsx_read".into()).unwrap()]).unwrap();
        inner.context = live_context(record);
        inner.parser = Some(parser.clone());
        ISOLATED_CALL.scope(call, async {
            for args in [json!({"workspace":"other","path":"customers.csv"}),
                json!({"workspace":"work","path":"../customers.csv"}),
                json!({"workspace":"work","path":"customers.csv","root":"/"})] {
                let denied = crate::mcp_files::handle_isolated_table_read("csv_read", &args, home.path(), "main").await;
                assert_eq!(denied["isError"], true);
            }
            assert_eq!(parser.calls.load(Ordering::SeqCst), 0);
            let result = crate::mcp_files::handle_isolated_table_read("csv_read",
                &json!({"workspace":"work","path":"customers.csv"}), home.path(), "main").await;
            assert_eq!(parser.calls.load(Ordering::SeqCst), 1);
            if wrong_table { assert_eq!(result["isError"], true); }
            else {
                let value: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
                assert_eq!(value["path"], "customers.csv");
                assert_eq!(value["table"], "customers.csv");
                assert_eq!(value["rows"][0]["name"], "Alice");
            }
        }).await;
    }
}
